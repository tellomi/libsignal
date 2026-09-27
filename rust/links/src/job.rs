//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The sender side of the pipeline (ADR-0063 §4.2): Planner → (client's fetcher) → Extractor →
//! Assembler → Policy.
//!
//! Sans-IO: the crate never touches the network. It hands the client one request at a time, the
//! client's own safe fetcher performs it under the §4.4 contract, and feeds the outcome back:
//!
//! ```text
//! let mut job = registry.begin(url, &ctx);
//! while let Some(req) = job.next_request() {      // ≤ 3 metadata requests + 1 image
//!     …perform it, then exactly one of:
//!     job.on_response(req.id, status, final_url, content_type, location, body);
//!     job.on_network_error(req.id);               // DNS / TCP / TLS: remembered as unreachable
//!     job.on_failure(req.id);                     // anything else (timeout, too big, bad hop…)
//!     job.on_first_party(req.id, result);         // for `first_party` requests
//!     job.on_image(req.id, ok);                   // for `image` requests
//! }
//! let out = job.finish(Some(&policy));            // snapshot + RichContent + level
//! ```
//!
//! Nothing here ever fails loudly: every problem moves the link down one rung of the ladder (§5).

use std::collections::{BTreeMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use tellomi_policy::{Field as PolicyField, PolicyEngine, Region};
use url::Url;

use crate::card::{self, Level};
use crate::extract::{self, Converter};
use crate::html;
use crate::kinds::{self, KindDef, WEB};
use crate::limits::*;
use crate::registry::{MapEntry, Plan, Provider, Registry, Source, Tier, fill_template, host_in};
use crate::rich::{Attr, LEVEL_BRAND, LEVEL_STRUCTURED, RichContent};
use crate::urlx;

fn global() -> Region {
    Region::Global
}

fn yes() -> bool {
    true
}

/// What the sender's client tells `begin`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendContext {
    /// `RegionProfile.id` (ADR-0065). P1 passes `global` until RegionProfile lands on mobile.
    #[serde(default = "global")]
    pub region: Region,
    /// The in-memory reachability memo (§4.3): hosts that failed at the network layer on the
    /// current network in the last 30 minutes.
    #[serde(default)]
    pub unreachable_hosts: Vec<String>,
    /// The "expand short links" setting (default on, local only; §九.4).
    #[serde(default = "yes")]
    pub expand_short_links: bool,
    /// The sender's UI locale, only for the few fixed snapshot strings (platform names, "Tellomi
    /// user"). Levels never depend on it.
    #[serde(default)]
    pub locale: String,
}

impl Default for SendContext {
    fn default() -> Self {
        SendContext {
            region: Region::Global,
            unreachable_hosts: Vec::new(),
            expand_short_links: true,
            locale: String::new(),
        }
    }
}

const HTML_TYPES: &[&str] = &["text/html", "application/xhtml+xml"];
/// JSON steps accept these and parse strictly as JSON. `text/javascript` is what the iTunes lookup
/// API actually sends (verified 2026-09-27); it is never executed.
const JSON_TYPES: &[&str] = &[
    "application/json",
    "text/json",
    "text/javascript",
    "application/javascript",
];

/// One request for the client to perform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Request {
    pub id: u32,
    #[serde(flatten)]
    pub kind: RequestKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RequestKind {
    /// GET the short link **without following redirects** and report the status and the first
    /// `Location` header. The body is never read (§4.4 last bullet).
    ExpandShortLink {
        url: String,
        user_agent: &'static str,
        connect_timeout_ms: u32,
        timeout_ms: u32,
    },
    /// A metadata request. Headers are exactly `User-Agent`, `Accept`, `Accept-Encoding`; no
    /// cookies; every redirect hop re-checked (https, domain, private address); body read up to
    /// `max_bytes` of decompressed data; `Content-Type` must be one of `content_types` (§4.4).
    Fetch {
        url: String,
        user_agent: &'static str,
        accept: &'static str,
        content_types: &'static [&'static str],
        max_bytes: usize,
        max_redirects: u32,
        connect_timeout_ms: u32,
        timeout_ms: u32,
    },
    /// A tell.cc object, fetched with Signal's existing call for it (group join info, call link
    /// room, sticker manifest), then reported with `on_first_party`.
    FirstParty { kind: String },
    /// The one preview image (§4.4 "+1"). Validate and re-encode it as today; report `on_image`.
    Image {
        url: String,
        user_agent: &'static str,
        max_redirects: u32,
        connect_timeout_ms: u32,
        timeout_ms: u32,
    },
}

/// What the client found for a `first_party` request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FirstPartyResult {
    pub ok: bool,
    /// The group link is definitely not active (reset / group gone): the one sender-side hint of
    /// §5.1 rule 2. Network failures are `ok = false, invalid = false`.
    #[serde(default)]
    pub invalid: bool,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub member_count: Option<u64>,
    #[serde(default)]
    pub sticker_count: Option<u64>,
}

/// Failure classes for the local debug log: never a URL (§6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    NotPreviewable,
    Lookalike,
    NotAnObject,
    ShortLinkDisabled,
    ShortLinkUnresolved,
    RegionUnreachable,
    HostUnreachable,
    HttpOnlyLocation,
    Budget,
    Network,
    Failed,
    HttpStatus,
    ContentType,
    TooLarge,
    Charset,
    Parse,
    WrongHost,
    JsonLdIdentity,
    RequiredMissing,
    ImageFailed,
    FirstPartyUnavailable,
    PolicyHit,
}

/// The finished preview to put in `DataMessage.preview`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PreviewDraft {
    /// Always the URL the user typed: what gets opened (§4.9).
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    /// The `image` request that succeeded: attach the bytes the client downloaded for it.
    /// First-party avatars and covers come from the client itself and are not listed here.
    pub image_url: Option<String>,
    pub date: Option<u64>,
    /// `Preview.rich`; encode with [`PreviewDraft::rich_bytes`].
    pub rich: Option<RichContent>,
}

impl PreviewDraft {
    pub fn rich_bytes(&self) -> Option<Vec<u8>> {
        self.rich.as_ref().map(RichContent::encode_to_bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SendOutcome {
    pub level: Level,
    pub provider: Option<String>,
    pub route: Option<String>,
    pub kind: Option<String>,
    /// `None` = send the message without a preview.
    pub preview: Option<PreviewDraft>,
    /// Show "This group link is not active" in the composer (§5.1 rule 2's only exception).
    pub group_link_invalid: bool,
    /// The domain imitates this well-known one (§6.1): no preview; the composer may flag it.
    pub lookalike: Option<String>,
    /// Add these to the reachability memo.
    pub newly_unreachable_hosts: Vec<String>,
    pub failures: Vec<Failure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Conf {
    /// `<title>` / `meta description` / site icon: snapshot only, never counts as required.
    Low,
    /// URL captures, API, oEmbed, OG, verified JSON-LD.
    Object,
}

#[derive(Debug, Clone)]
struct Value {
    text: String,
    conf: Conf,
}

#[derive(Debug, Default)]
struct Fields {
    title: Option<Value>,
    description: Option<Value>,
    image: Option<Value>,
    date: Option<u64>,
    attrs: BTreeMap<String, String>,
}

impl Fields {
    fn slot(&mut self, target: &str) -> Option<&mut Option<Value>> {
        match target {
            "title" => Some(&mut self.title),
            "description" => Some(&mut self.description),
            "image" => Some(&mut self.image),
            _ => None,
        }
    }
}

#[derive(Debug)]
enum Target {
    PlainLink,
    /// Signal's snapshot preview of this page (no provider, or a provider whose `fallback` is
    /// `generic` and no route recognised an object).
    Generic {
        fetch: Url,
    },
    FirstParty {
        route: usize,
        captures: BTreeMap<String, String>,
        canonical: String,
    },
    Provider {
        provider: Box<Provider>,
        route: Option<usize>,
        captures: BTreeMap<String, String>,
        canonical: Option<String>,
        object: Option<String>,
        /// Identified from an `http://` Location: identification only, no requests (README §3).
        via_http: bool,
    },
}

#[derive(Debug)]
enum Step {
    Expand,
    Plan(usize),
    GenericPage,
    FirstParty,
}

#[derive(Debug, Clone)]
enum Pending {
    Expand {
        url: Url,
        host: String,
    },
    Page {
        plan: Option<usize>,
        host: String,
        url: Url,
    },
    Json {
        plan: usize,
        host: String,
    },
    FirstParty,
    Image,
}

/// One link being previewed by the sender.
#[derive(Debug)]
pub struct Job {
    input: String,
    parsed: Option<Url>,
    ctx: SendContext,
    tellomi: Box<Provider>,
    target: Target,
    queue: VecDeque<Step>,
    pending: Option<(u32, Pending)>,
    next_id: u32,
    metadata_requests: usize,
    unreachable: HashSet<String>,
    new_unreachable: Vec<String>,
    fields: Fields,
    image_decided: bool,
    image_ok: bool,
    first_party_failed: bool,
    group_invalid: bool,
    lookalike: Option<String>,
    failures: Vec<Failure>,
    finished: bool,
}

impl Registry {
    /// Start previewing `url` as typed by the user (Planner, §4.2).
    pub fn begin(&self, url: &str, ctx: &SendContext) -> Job {
        let mut job = Job {
            input: url.trim().to_owned(),
            parsed: None,
            ctx: ctx.clone(),
            tellomi: Box::new(self.tellomi().clone()),
            target: Target::PlainLink,
            queue: VecDeque::new(),
            pending: None,
            next_id: 1,
            metadata_requests: 0,
            unreachable: ctx
                .unreachable_hosts
                .iter()
                .map(|h| h.to_ascii_lowercase())
                .collect(),
            new_unreachable: Vec::new(),
            fields: Fields::default(),
            image_decided: false,
            image_ok: false,
            first_party_failed: false,
            group_invalid: false,
            lookalike: None,
            failures: Vec::new(),
            finished: false,
        };
        if !urlx::is_valid_preview_url(&job.input) {
            job.failures.push(Failure::NotPreviewable);
            return job;
        }
        let Some(parsed) = urlx::parse_web_url(&job.input) else {
            job.failures.push(Failure::NotPreviewable);
            return job;
        };
        let Some(host) = urlx::host_str(&parsed) else {
            return job;
        };
        job.parsed = Some(parsed.clone());
        if let Some(like) = self.known.lookalike_of(&host) {
            // Both sides drop to a plain link; the sender does not even fetch the page (§6.2).
            job.lookalike = Some(like.to_owned());
            job.failures.push(Failure::Lookalike);
            return job;
        }

        if self.is_first_party_url(&parsed) {
            let m = job.tellomi.match_url(&parsed, &host);
            let Some(route_index) = m.route else {
                job.failures.push(Failure::NotAnObject);
                return job;
            };
            let route = &job.tellomi.routes[route_index];
            if route.degraded.is_some() {
                job.failures.push(Failure::NotAnObject);
                return job;
            }
            let kind = route.kind.clone();
            let plan = route.plan.clone();
            job.target = Target::FirstParty {
                route: route_index,
                captures: m.captures.clone(),
                canonical: m.canonical,
            };
            match kind.as_str() {
                "tellomi.user" => {
                    let name = card::user_name(&m.captures);
                    let title = name
                        .display
                        .unwrap_or_else(|| card::generic_user_title(&ctx.locale).to_owned());
                    job.fields.title = Some(Value {
                        text: title,
                        conf: Conf::Object,
                    });
                }
                "tellomi.official" => {
                    job.queue.extend((0..plan.len()).map(Step::Plan));
                }
                _ if plan.iter().any(|p| matches!(p, Plan::FirstParty)) => {
                    job.queue.push_back(Step::FirstParty);
                }
                _ => {
                    job.target = Target::PlainLink;
                    job.failures.push(Failure::NotAnObject);
                }
            }
            return job;
        }

        let Some(pid) = self.provider_for_host(&host) else {
            job.switch_to_generic(parsed);
            return job;
        };
        let provider = self.providers[pid].clone();
        if provider.is_short(&host) {
            let expand = ctx.expand_short_links;
            job.target = Target::Provider {
                provider: Box::new(provider),
                route: None,
                captures: BTreeMap::new(),
                canonical: None,
                object: None,
                via_http: false,
            };
            if expand {
                job.queue.push_back(Step::Expand);
            } else {
                job.failures.push(Failure::ShortLinkDisabled);
            }
            return job;
        }
        let m = provider.match_url(&parsed, &host);
        if m.route.is_none() && provider.fallback_generic {
            if provider.unreachable_from(ctx.region) {
                job.failures.push(Failure::RegionUnreachable);
            } else {
                // e.g. a GitHub issue: no kind for it, but the page's own OG is a good preview.
                job.switch_to_generic(parsed);
                return job;
            }
        }
        job.set_provider_target(
            provider,
            m.route,
            m.captures,
            Some(m.canonical),
            m.object,
            false,
        );
        job
    }
}

fn is_disallowed_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1F}' | '\u{7F}'..='\u{9F}')
        || matches!(c, '\u{AD}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
        || ('\u{E0000}'..='\u{E007F}').contains(&c)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        s.chars().take(max).collect()
    }
}

impl Job {
    fn switch_to_generic(&mut self, mut fetch: Url) {
        fetch.set_fragment(None);
        self.target = Target::Generic { fetch };
        self.queue.clear();
        self.queue.push_back(Step::GenericPage);
    }

    fn set_provider_target(
        &mut self,
        provider: Provider,
        route: Option<usize>,
        captures: BTreeMap<String, String>,
        canonical: Option<String>,
        object: Option<String>,
        via_http: bool,
    ) {
        if let Some(r) = route.map(|r| &provider.routes[r]) {
            let kind = kinds::kind(&r.kind);
            for (k, v) in &r.attrs {
                if let Some(v) = kind
                    .and_then(|kd| kd.attr_type(k))
                    .and_then(|t| t.validate(v))
                {
                    self.fields.attrs.entry(k.clone()).or_insert(v);
                }
            }
            if provider.tier != Tier::Brand && r.is_structured() {
                self.queue.extend((0..r.plan.len()).map(Step::Plan));
            }
        } else {
            self.failures.push(Failure::NotAnObject);
        }
        self.target = Target::Provider {
            provider: Box::new(provider),
            route,
            captures,
            canonical,
            object,
            via_http,
        };
    }

    fn provider_route(&self) -> Option<(&Provider, &crate::registry::Route)> {
        match &self.target {
            Target::Provider {
                provider,
                route: Some(r),
                ..
            } => Some((provider, &provider.routes[*r])),
            Target::FirstParty { route, .. } => Some((&self.tellomi, &self.tellomi.routes[*route])),
            _ => None,
        }
    }

    fn kind_def(&self) -> Option<&'static KindDef> {
        self.provider_route()
            .and_then(|(_, r)| kinds::kind(&r.kind))
    }

    /// Required fields present at object level. `image_downloaded` distinguishes "we have an
    /// object-level image URL" (enough to stop fetching metadata) from "the image arrived" (what
    /// the assembler needs).
    fn meets_required(&self, image_downloaded: bool) -> bool {
        let Some(kind) = self.kind_def() else {
            return false;
        };
        let f = &self.fields;
        kind.meets_required(|name| match name {
            "title" => f.title.as_ref().is_some_and(|v| v.conf == Conf::Object),
            "description" => f
                .description
                .as_ref()
                .is_some_and(|v| v.conf == Conf::Object),
            "image" => {
                f.image.as_ref().is_some_and(|v| v.conf == Conf::Object)
                    && (!image_downloaded || self.image_ok)
            }
            attr => f.attrs.contains_key(attr),
        })
    }

    fn id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn fetch_request(&mut self, url: &Url, json: bool, pending: Pending) -> Request {
        self.metadata_requests += 1;
        let id = self.id();
        self.pending = Some((id, pending));
        Request {
            id,
            kind: RequestKind::Fetch {
                url: url.as_str().to_owned(),
                user_agent: USER_AGENT,
                accept: if json {
                    "application/json"
                } else {
                    "text/html"
                },
                content_types: if json { JSON_TYPES } else { HTML_TYPES },
                max_bytes: if json { MAX_JSON_BYTES } else { MAX_HTML_BYTES },
                max_redirects: MAX_REDIRECTS,
                connect_timeout_ms: CONNECT_TIMEOUT_MS,
                timeout_ms: REQUEST_TIMEOUT_MS,
            },
        }
    }

    /// `None` when there is nothing (more) to fetch — call `finish` — or while a request is still
    /// unanswered.
    pub fn next_request(&mut self) -> Option<Request> {
        if self.finished || self.pending.is_some() {
            return None;
        }
        while let Some(step) = self.queue.pop_front() {
            if let Some(request) = self.issue(step) {
                return Some(request);
            }
        }
        self.image_request()
    }

    fn can_fetch(&mut self, host: &str) -> bool {
        if self.unreachable.contains(host) {
            self.failures.push(Failure::HostUnreachable);
            return false;
        }
        if self.metadata_requests >= MAX_METADATA_REQUESTS {
            self.failures.push(Failure::Budget);
            return false;
        }
        true
    }

    fn issue(&mut self, step: Step) -> Option<Request> {
        match step {
            Step::Expand => {
                let url = self.parsed.clone()?;
                let host = urlx::host_str(&url)?;
                if let Target::Provider { provider, .. } = &self.target
                    && provider.unreachable_from(self.ctx.region)
                {
                    self.failures.push(Failure::RegionUnreachable);
                    return None;
                }
                if !self.can_fetch(&host) {
                    return None;
                }
                self.metadata_requests += 1;
                let id = self.id();
                self.pending = Some((
                    id,
                    Pending::Expand {
                        url: url.clone(),
                        host,
                    },
                ));
                Some(Request {
                    id,
                    kind: RequestKind::ExpandShortLink {
                        url: url.as_str().to_owned(),
                        user_agent: USER_AGENT,
                        connect_timeout_ms: CONNECT_TIMEOUT_MS,
                        timeout_ms: REQUEST_TIMEOUT_MS,
                    },
                })
            }
            Step::GenericPage => {
                let Target::Generic { fetch } = &self.target else {
                    return None;
                };
                let url = fetch.clone();
                let host = urlx::host_str(&url)?;
                if !self.can_fetch(&host) {
                    return None;
                }
                Some(self.fetch_request(
                    &url.clone(),
                    false,
                    Pending::Page {
                        plan: None,
                        host,
                        url,
                    },
                ))
            }
            Step::FirstParty => {
                let kind = self.provider_route()?.1.kind.clone();
                let id = self.id();
                self.pending = Some((id, Pending::FirstParty));
                Some(Request {
                    id,
                    kind: RequestKind::FirstParty { kind },
                })
            }
            Step::Plan(i) => self.issue_plan(i),
        }
    }

    fn issue_plan(&mut self, i: usize) -> Option<Request> {
        let (provider, route) = self.provider_route()?;
        let plan = route.plan.get(i)?.clone();
        let unreachable_region = provider.unreachable_from(self.ctx.region);
        let (via_http, captures, canonical) = match &self.target {
            Target::Provider {
                via_http,
                captures,
                canonical,
                ..
            } => (*via_http, captures.clone(), canonical.clone()),
            Target::FirstParty {
                captures,
                canonical,
                ..
            } => (false, captures.clone(), Some(canonical.clone())),
            _ => return None,
        };
        match &plan {
            Plan::None => {
                self.queue.clear();
                return None;
            }
            Plan::UrlOnly(map) => {
                self.apply_template_map(map, &captures);
                return None;
            }
            Plan::FirstParty => {
                return self.issue(Step::FirstParty);
            }
            _ => {}
        }
        // "Stop as soon as a step has all the required fields" (§4.4).
        if self.meets_required(false) && !matches!(self.target, Target::FirstParty { .. }) {
            return None;
        }
        if via_http {
            self.failures.push(Failure::HttpOnlyLocation);
            return None;
        }
        if unreachable_region {
            self.failures.push(Failure::RegionUnreachable);
            return None;
        }
        let canonical = canonical?;
        let (url, json, pending) = match &plan {
            Plan::OgJsonld { .. } => {
                let url = Url::parse(&canonical).ok()?;
                let host = urlx::host_str(&url)?;
                (
                    url.clone(),
                    false,
                    Pending::Page {
                        plan: Some(i),
                        host,
                        url,
                    },
                )
            }
            Plan::PublicApi { url, .. } => {
                let encoded: BTreeMap<String, String> = captures
                    .iter()
                    .map(|(k, v)| (k.clone(), urlx::encode_component(v)))
                    .collect();
                let url = Url::parse(&fill_template(url, &encoded)).ok()?;
                let host = urlx::host_str(&url)?;
                (url, true, Pending::Json { plan: i, host })
            }
            Plan::Oembed { endpoint, .. } => {
                let mut url = Url::parse(endpoint).ok()?;
                url.query_pairs_mut()
                    .append_pair("url", &canonical)
                    .append_pair("format", "json");
                let host = urlx::host_str(&url)?;
                (url, true, Pending::Json { plan: i, host })
            }
            _ => return None,
        };
        let host = match &pending {
            Pending::Page { host, .. } | Pending::Json { host, .. } => host.clone(),
            _ => return None,
        };
        if !self.can_fetch(&host) {
            return None;
        }
        Some(self.fetch_request(&url, json, pending))
    }

    fn image_request(&mut self) -> Option<Request> {
        if self.image_decided {
            return None;
        }
        self.image_decided = true;
        let wanted = match &self.target {
            Target::Generic { .. } => true,
            Target::FirstParty { .. } => self
                .provider_route()
                .is_some_and(|(_, r)| r.kind == "tellomi.official"),
            Target::Provider {
                provider,
                route: Some(r),
                ..
            } => {
                // Brand shells carry no image (§4.5); a structured card fetches one only once
                // everything else it needs is there.
                provider.tier != Tier::Brand
                    && provider.routes[*r].is_structured()
                    && self.kind_def().is_some_and(|k| {
                        k.meets_required(|name| match name {
                            "image" => self
                                .fields
                                .image
                                .as_ref()
                                .is_some_and(|v| v.conf == Conf::Object),
                            "title" => self
                                .fields
                                .title
                                .as_ref()
                                .is_some_and(|v| v.conf == Conf::Object),
                            "description" => self
                                .fields
                                .description
                                .as_ref()
                                .is_some_and(|v| v.conf == Conf::Object),
                            attr => self.fields.attrs.contains_key(attr),
                        })
                    })
            }
            _ => false,
        };
        let url = self
            .fields
            .image
            .as_ref()
            .map(|v| v.text.clone())
            .filter(|_| wanted)?;
        let id = self.id();
        self.pending = Some((id, Pending::Image));
        Some(Request {
            id,
            kind: RequestKind::Image {
                url,
                user_agent: USER_AGENT,
                max_redirects: MAX_REDIRECTS,
                connect_timeout_ms: CONNECT_TIMEOUT_MS,
                timeout_ms: REQUEST_TIMEOUT_MS,
            },
        })
    }

    fn take_pending(&mut self, id: u32) -> Option<Pending> {
        match &self.pending {
            Some((pid, _)) if *pid == id => self.pending.take().map(|(_, p)| p),
            _ => None,
        }
    }

    /// A DNS / TCP connect / TLS handshake failure (not an HTTP status): the host is remembered as
    /// unreachable on this network and skipped from now on (§4.3).
    pub fn on_network_error(&mut self, id: u32) {
        let Some(pending) = self.take_pending(id) else {
            return;
        };
        self.failures.push(Failure::Network);
        let host = match pending {
            Pending::Expand { host, .. }
            | Pending::Page { host, .. }
            | Pending::Json { host, .. } => host,
            Pending::FirstParty => {
                self.first_party_failed = true;
                return;
            }
            Pending::Image => return,
        };
        if self.unreachable.insert(host.clone()) {
            self.new_unreachable.push(host);
        }
    }

    /// Any other failure: timeout after connecting, over the size limit, a hop that failed the
    /// https / private-address check, too many redirects…
    pub fn on_failure(&mut self, id: u32) {
        match self.take_pending(id) {
            Some(Pending::FirstParty) => self.first_party_failed = true,
            Some(_) => self.failures.push(Failure::Failed),
            None => {}
        }
    }

    pub fn on_first_party(&mut self, id: u32, result: FirstPartyResult) {
        if !matches!(self.take_pending(id), Some(Pending::FirstParty)) {
            return;
        }
        if !result.ok {
            self.first_party_failed = true;
            self.group_invalid = result.invalid
                && self
                    .provider_route()
                    .is_some_and(|(_, r)| r.kind == "tellomi.group");
            self.failures.push(Failure::FirstPartyUnavailable);
            return;
        }
        let kind = self.kind_def();
        if let Some(title) = result.title.filter(|t| !t.trim().is_empty()) {
            self.offer("title", title, Conf::Object, kind, false);
        }
        if let Some(n) = result.member_count {
            self.offer("member_count", n.to_string(), Conf::Object, kind, false);
        }
        if let Some(n) = result.sticker_count {
            self.offer("sticker_count", n.to_string(), Conf::Object, kind, false);
        }
    }

    pub fn on_image(&mut self, id: u32, ok: bool) {
        if matches!(self.take_pending(id), Some(Pending::Image)) {
            self.image_ok = ok;
            if !ok {
                self.failures.push(Failure::ImageFailed);
            }
        }
    }

    /// An HTTP response. `final_url` is where the redirects ended; `location` is only read for a
    /// short-link expansion; `body` is the decompressed body (empty for the expansion).
    pub fn on_response(
        &mut self,
        id: u32,
        status: u16,
        final_url: &str,
        content_type: &str,
        location: Option<&str>,
        body: &[u8],
    ) {
        match self.take_pending(id) {
            Some(Pending::Expand { url, .. }) => self.on_expanded(&url, status, location),
            Some(Pending::Page { plan, url, .. }) => {
                self.on_page(plan, &url, status, final_url, content_type, body)
            }
            Some(Pending::Json { plan, .. }) => {
                self.on_json(plan, status, final_url, content_type, body)
            }
            Some(Pending::FirstParty) => self.first_party_failed = true,
            Some(Pending::Image) => self.image_ok = (200..300).contains(&status),
            None => {}
        }
    }

    fn on_expanded(&mut self, request_url: &Url, status: u16, location: Option<&str>) {
        let Target::Provider { provider, .. } = &self.target else {
            return;
        };
        let provider = (**provider).clone();
        let resolved = location
            .filter(|_| (300..400).contains(&status))
            .and_then(|l| request_url.join(l.trim()).ok())
            .filter(|u| matches!(u.scheme(), "http" | "https"));
        let Some(next) = resolved else {
            self.failures.push(Failure::ShortLinkUnresolved);
            return;
        };
        let host = urlx::host_str(&next).unwrap_or_default();
        // The Location must stay with the short link's own provider: expanding is the sender's
        // claim, and a receiver will refuse a `rich` that switches provider (§5.3). Only one
        // expansion, ever.
        if !provider.owns(&host) || provider.is_short(&host) {
            self.failures.push(Failure::ShortLinkUnresolved);
            return;
        }
        let m = provider.match_url(&next, &host);
        let via_http = next.scheme() == "http";
        if m.route.is_none()
            && provider.fallback_generic
            && !via_http
            && !provider.unreachable_from(self.ctx.region)
        {
            self.switch_to_generic(next);
            return;
        }
        self.set_provider_target(
            provider,
            m.route,
            m.captures,
            Some(m.canonical),
            m.object,
            via_http,
        );
    }

    fn allowed_final_host(&self, plan: Option<usize>, host: &str) -> bool {
        match (&self.target, plan) {
            (Target::Generic { .. }, None) => true,
            (Target::FirstParty { .. }, Some(_)) => {
                self.tellomi.official_domains.iter().any(|h| h == host)
            }
            (Target::Provider { provider, .. }, Some(_)) => host_in(host, &provider.domains),
            _ => false,
        }
    }

    fn on_page(
        &mut self,
        plan: Option<usize>,
        request_url: &Url,
        status: u16,
        final_url: &str,
        content_type: &str,
        body: &[u8],
    ) {
        if !(200..300).contains(&status) {
            self.failures.push(Failure::HttpStatus);
            return;
        }
        let final_url = Url::parse(final_url).unwrap_or_else(|_| request_url.clone());
        let final_host = urlx::host_str(&final_url).unwrap_or_default();
        // A structured step must still be on its provider after redirects (§4.4): an open redirect
        // on the provider's domain must not put someone else's page into this card.
        if final_url.scheme() != "https" || !self.allowed_final_host(plan, &final_host) {
            self.failures.push(Failure::WrongHost);
            return;
        }
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !HTML_TYPES.contains(&mime.as_str()) {
            self.failures.push(Failure::ContentType);
            return;
        }
        if body.len() > MAX_HTML_BYTES {
            self.failures.push(Failure::TooLarge);
            return;
        }
        let text = String::from_utf8_lossy(body);
        let page = html::scan(&text);
        let charset = html::content_type_charset(content_type).or(page.charset.clone());
        // Only UTF-8 is decoded. A page that declares another charset keeps its image and icon
        // (URLs are ASCII) but none of its text.
        let text_ok = charset
            .as_deref()
            .is_none_or(|c| matches!(c, "utf-8" | "utf8" | "us-ascii" | "ascii"));
        if !text_ok {
            self.failures.push(Failure::Charset);
        }
        let kind = self.kind_def();
        let (map, jsonld_type) = match plan.and_then(|i| {
            self.provider_route()
                .and_then(|(_, r)| r.plan.get(i).cloned())
        }) {
            Some(Plan::OgJsonld { map, jsonld_type }) => (map, jsonld_type),
            _ => (Vec::new(), None),
        };
        let provider_page = !matches!(self.target, Target::Generic { .. });

        // JSON-LD only for providers that ask for it, and only a node that proves it is about
        // this very object (§4.4; Xigua's JSON-LD is another video altogether).
        let node = match (&jsonld_type, text_ok && provider_page) {
            (Some(t), true) => {
                let nodes = extract::json_ld_nodes(&page.json_ld, t);
                let verified = nodes
                    .into_iter()
                    .find(|n| self.json_ld_is_this_object(n, &final_url));
                if verified.is_none() && !page.json_ld.is_empty() {
                    self.failures.push(Failure::JsonLdIdentity);
                }
                verified
            }
            _ => None,
        };

        for entry in &map {
            let raw = match &entry.source {
                Source::Og(prop) => page.meta(prop).map(str::to_owned),
                Source::Json(path) => node.as_ref().and_then(|n| extract::eval_path(n, path)),
                Source::Template(_) => None,
            };
            let is_og_title = matches!(&entry.source, Source::Og(p) if p == "og:title");
            if let Some(v) = raw.and_then(|v| convert(v, &entry.converters)) {
                if !text_ok && entry.target != "image" {
                    continue;
                }
                let v = if entry.target == "image" {
                    self.resolve_image(&final_url, &v)
                } else {
                    Some(v)
                };
                if let Some(v) = v {
                    self.offer(&entry.target, v, Conf::Object, kind, is_og_title);
                }
            }
        }

        // OG defaults, then the low-confidence fallbacks (§4.4).
        if text_ok {
            if let Some(t) = page.meta("og:title") {
                self.offer("title", t.to_owned(), Conf::Object, kind, true);
            }
            if let Some(d) = page.meta("og:description") {
                self.offer("description", d.to_owned(), Conf::Object, kind, false);
            }
            if let Some(t) = &page.title {
                self.offer("title", t.clone(), Conf::Low, kind, true);
            }
            if let Some(d) = page.meta("description") {
                self.offer("description", d.to_owned(), Conf::Low, kind, false);
            }
            if self.fields.date.is_none()
                && let Some(dt) = page
                    .meta("article:published_time")
                    .or_else(|| page.meta("og:published_time"))
                    .and_then(crate::time::parse_rfc3339)
            {
                self.fields.date = u64::try_from(dt.unix_ms).ok();
            }
        }
        let small = |k: &str| page.meta(k).and_then(|v| v.trim().parse::<u32>().ok());
        let og_small = matches!((small("og:image:width"), small("og:image:height")), (Some(w), Some(h)) if w.min(h) < SMALL_OG_IMAGE_PX);
        let og_image = page
            .meta("og:image")
            .filter(|_| !og_small)
            .and_then(|u| self.resolve_image(&final_url, u));
        if let Some(img) = og_image {
            self.offer("image", img, Conf::Object, kind, false);
        } else if self.fields.image.is_none() {
            // Icon fallback: the largest declared icon ≥ 64 px, else /apple-touch-icon.png.
            let icon = page
                .icons
                .iter()
                .filter_map(|i| i.size.map(|(w, h)| (w.min(h), i)))
                .filter(|(side, _)| *side >= MIN_ICON_PX)
                .max_by_key(|(side, _)| *side)
                .and_then(|(_, i)| self.resolve_image(&final_url, &i.href))
                .or_else(|| self.resolve_image(&final_url, "/apple-touch-icon.png"));
            if let Some(icon) = icon {
                self.offer("image", icon, Conf::Low, kind, false);
            }
        }
    }

    fn json_ld_is_this_object(&self, node: &serde_json::Value, page_url: &Url) -> bool {
        let Target::Provider {
            provider,
            canonical: Some(canonical),
            object,
            ..
        } = &self.target
        else {
            return false;
        };
        extract::json_ld_identities(node).iter().any(|id| {
            if let Some(obj) = object.as_deref().filter(|o| !o.is_empty())
                && id.contains(obj)
            {
                return true;
            }
            let Ok(u) = page_url.join(id) else {
                return false;
            };
            let host = urlx::host_str(&u).unwrap_or_default();
            provider.owns(&host)
                && !provider.is_short(&host)
                && provider.match_url(&u, &host).canonical == *canonical
        })
    }

    fn resolve_image(&self, base: &Url, candidate: &str) -> Option<String> {
        let u = base.join(candidate.trim()).ok()?;
        (u.scheme() == "https" && u.host_str().is_some()).then(|| u.to_string())
    }

    fn on_json(
        &mut self,
        plan: usize,
        status: u16,
        final_url: &str,
        content_type: &str,
        body: &[u8],
    ) {
        if !(200..300).contains(&status) {
            self.failures.push(Failure::HttpStatus);
            return;
        }
        let Some((provider, route)) = self.provider_route() else {
            return;
        };
        let final_host = Url::parse(final_url)
            .ok()
            .filter(|u| u.scheme() == "https")
            .and_then(|u| urlx::host_str(&u));
        if !final_host.is_some_and(|h| provider.api_hosts.contains(&h)) {
            self.failures.push(Failure::WrongHost);
            return;
        }
        let map = match route.plan.get(plan) {
            Some(Plan::PublicApi { map, .. } | Plan::Oembed { map, .. }) => map.clone(),
            _ => return,
        };
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !(JSON_TYPES.contains(&mime.as_str()) || mime.ends_with("+json")) {
            self.failures.push(Failure::ContentType);
            return;
        }
        if body.len() > MAX_JSON_BYTES {
            self.failures.push(Failure::TooLarge);
            return;
        }
        let Some(value) = extract::parse_json(body) else {
            self.failures.push(Failure::Parse);
            return;
        };
        let kind = self.kind_def();
        for entry in &map {
            if let Source::Json(path) = &entry.source
                && let Some(v) =
                    extract::eval_path(&value, path).and_then(|v| convert(v, &entry.converters))
            {
                let v = if entry.target == "image" {
                    Url::parse(&v)
                        .ok()
                        .filter(|u| u.scheme() == "https")
                        .map(|u| u.to_string())
                } else {
                    Some(v)
                };
                if let Some(v) = v {
                    self.offer(&entry.target, v, Conf::Object, kind, false);
                }
            }
        }
    }

    fn apply_template_map(&mut self, map: &[MapEntry], captures: &BTreeMap<String, String>) {
        let kind = self.kind_def();
        for entry in map {
            if let Source::Template(t) = &entry.source
                && let Some(v) = convert(fill_template(t, captures), &entry.converters)
            {
                self.offer(&entry.target, v, Conf::Object, kind, false);
            }
        }
    }

    /// Put one extracted value in its slot: placeholders count as missing, titles lose their site
    /// suffix, attrs must pass their type, and a better source may replace a worse one.
    fn offer(
        &mut self,
        target: &str,
        value: String,
        conf: Conf,
        kind: Option<&KindDef>,
        strip_title: bool,
    ) {
        let mut value = value.trim().to_owned();
        let placeholders = match &self.target {
            Target::Provider { provider, .. } => provider.placeholders.get(target).cloned(),
            _ => None,
        };
        if strip_title
            && target == "title"
            && let Target::Provider { provider, .. } = &self.target
        {
            for rx in &provider.title_strip {
                value = rx.replace(&value, "").trim().to_owned();
            }
        }
        if value.is_empty() || placeholders.is_some_and(|p| p.contains(&value)) {
            return;
        }
        match target {
            "title" | "description" => {
                let max = if target == "title" {
                    MAX_TITLE_CHARS
                } else {
                    MAX_DESCRIPTION_CHARS
                };
                let text = truncate_chars(&html::collapse_ws(&value), max);
                let slot = self.fields.slot(target).expect("snapshot field");
                if slot.as_ref().is_none_or(|v| v.conf < conf) {
                    *slot = Some(Value { text, conf });
                }
            }
            "image" => {
                if self.fields.image.as_ref().is_none_or(|v| v.conf < conf) {
                    self.fields.image = Some(Value { text: value, conf });
                }
            }
            attr => {
                if let Some(v) = kind
                    .and_then(|k| k.attr_type(attr))
                    .and_then(|t| t.validate(&value))
                {
                    self.fields.attrs.entry(attr.to_owned()).or_insert(v);
                }
            }
        }
    }

    /// Assemble the preview (§4.2 Assembler + Policy). Call once, after `next_request` returned
    /// `None` or when the 10 s budget ran out.
    pub fn finish(&mut self, policy: Option<&PolicyEngine>) -> SendOutcome {
        self.finished = true;
        self.pending = None;
        let mut out = SendOutcome {
            level: Level::PlainLink,
            provider: None,
            route: None,
            kind: None,
            preview: None,
            group_link_invalid: self.group_invalid,
            lookalike: self.lookalike.clone(),
            newly_unreachable_hosts: self.new_unreachable.clone(),
            failures: Vec::new(),
        };
        let draft = |title: Option<String>| PreviewDraft {
            url: self.input.clone(),
            title,
            description: None,
            image_url: None,
            date: None,
            rich: None,
        };
        let best = |v: &Option<Value>| v.as_ref().map(|v| v.text.clone());
        let image = self
            .fields
            .image
            .as_ref()
            .filter(|_| self.image_ok)
            .map(|v| v.text.clone());

        match &self.target {
            Target::PlainLink => {}
            Target::Generic { .. } => {
                if let Some(title) = best(&self.fields.title) {
                    out.level = Level::Generic;
                    out.preview = Some(PreviewDraft {
                        description: best(&self.fields.description),
                        image_url: image,
                        date: self.fields.date,
                        ..draft(Some(title))
                    });
                }
            }
            Target::FirstParty {
                route, canonical, ..
            } => {
                let r = &self.tellomi.routes[*route];
                out.provider = Some(TELLOMI.to_owned());
                out.route = Some(r.id.clone());
                out.kind = Some(r.kind.clone());
                let official = r.kind == "tellomi.official";
                let title = if official {
                    best(&self.fields.title)
                        .or_else(|| Some(card::OFFICIAL_FALLBACK_TITLE.to_owned()))
                } else if r.kind == "tellomi.call" && !self.first_party_failed {
                    best(&self.fields.title)
                        .or_else(|| Some(card::generic_call_title(&self.ctx.locale).to_owned()))
                } else {
                    best(&self.fields.title)
                };
                let kind_ok = kinds::kind(&r.kind).is_some_and(|k| {
                    k.meets_required(|f| match f {
                        "title" => title.is_some(),
                        attr => self.fields.attrs.contains_key(attr),
                    })
                });
                // tell.cc objects that could not be fetched are plain links, not a generic
                // "Tellomi group" card that cannot be opened (§5.2).
                if (!official && self.first_party_failed) || !kind_ok || title.is_none() {
                    self.failures.push(Failure::FirstPartyUnavailable);
                } else {
                    out.level = Level::FirstParty;
                    out.preview = Some(PreviewDraft {
                        description: if official {
                            best(&self.fields.description)
                        } else {
                            None
                        },
                        image_url: if official { image } else { None },
                        rich: Some(RichContent {
                            kind: Some(r.kind.clone()),
                            provider: Some(TELLOMI.to_owned()),
                            schema: Some(RICH_SCHEMA),
                            canonical_url: Some(canonical.clone())
                                .filter(|c| c.chars().count() <= MAX_CANONICAL_URL_CHARS),
                            attrs: attrs_of(&self.fields.attrs),
                            level: Some(LEVEL_STRUCTURED),
                        }),
                        ..draft(title)
                    });
                }
            }
            Target::Provider {
                provider,
                route,
                canonical,
                ..
            } => {
                let r = route.map(|r| &provider.routes[r]);
                out.provider = Some(provider.id.clone());
                out.route = r.map(|r| r.id.clone());
                let kind_id = r.map_or(WEB, |r| r.kind.as_str()).to_owned();
                out.kind = Some(kind_id.clone());
                let structured = provider.tier != Tier::Brand
                    && r.is_some_and(|r| r.is_structured())
                    && self.meets_required(true);
                if !structured
                    && r.is_some_and(|r| r.is_structured())
                    && provider.tier != Tier::Brand
                {
                    self.failures.push(Failure::RequiredMissing);
                }
                let platform = provider.name.for_locale(&self.ctx.locale).to_owned();
                let canonical = canonical
                    .clone()
                    .filter(|c| c.chars().count() <= MAX_CANONICAL_URL_CHARS);
                if structured {
                    // Signal shows "· date" after the domain; for structured kinds it comes from
                    // the object's own publication time when the page had no OG date.
                    let date = self.fields.date.or_else(|| {
                        self.fields
                            .attrs
                            .get("published_at")
                            .and_then(|t| crate::time::parse_rfc3339(t))
                            .and_then(|dt| u64::try_from(dt.unix_ms).ok())
                    });
                    let title = best(&self.fields.title)
                        .or_else(|| self.fields.attrs.get("name").cloned())
                        .unwrap_or(platform);
                    out.level = Level::Structured;
                    out.preview = Some(PreviewDraft {
                        description: best(&self.fields.description),
                        image_url: image,
                        date,
                        rich: Some(RichContent {
                            kind: Some(kind_id),
                            provider: Some(provider.id.clone()),
                            schema: Some(RICH_SCHEMA),
                            canonical_url: canonical,
                            attrs: attrs_of(&self.fields.attrs),
                            level: Some(LEVEL_STRUCTURED),
                        }),
                        ..draft(Some(title))
                    });
                } else {
                    // A brand-tier platform says only its name; a structured one that came up
                    // short keeps the best title it found, title-meta included (§4.5).
                    let title = if provider.tier == Tier::Brand {
                        platform
                    } else {
                        best(&self.fields.title).unwrap_or(platform)
                    };
                    out.level = Level::Brand;
                    out.preview = Some(PreviewDraft {
                        rich: Some(RichContent {
                            kind: Some(kind_id),
                            provider: Some(provider.id.clone()),
                            schema: Some(RICH_SCHEMA),
                            canonical_url: canonical,
                            attrs: Vec::new(),
                            level: Some(LEVEL_BRAND),
                        }),
                        ..draft(Some(title))
                    });
                }
            }
        }

        // Policy (§4.2, §6.6): any text field that hits the lexicon voids the whole card; the
        // message is still sent. Only lexicon hits count here — the structural checks meant for
        // identity fields (invisible characters, mixed scripts) would void ordinary titles.
        if let (Some(policy), Some(preview)) = (policy, &out.preview) {
            let regions: &[Region] = match self.ctx.region {
                Region::Global => &[Region::Global],
                Region::Cn => &[Region::Global, Region::Cn],
            };
            let mut texts: Vec<&str> = preview
                .title
                .iter()
                .chain(&preview.description)
                .map(String::as_str)
                .collect();
            if let Some(rich) = &preview.rich {
                texts.extend(rich.attrs.iter().filter_map(|a| a.value.as_deref()));
            }
            let hit = texts.iter().any(|t| {
                let cleaned: String = t.chars().filter(|c| !is_disallowed_control(*c)).collect();
                let verdict = policy.check(&cleaned, PolicyField::LinkPreview, regions);
                !verdict.is_allowed() && verdict.hit.is_some()
            });
            if hit {
                self.failures.push(Failure::PolicyHit);
                out.level = Level::PlainLink;
                out.preview = None;
            }
        }
        out.failures = self.failures.clone();
        out
    }
}

fn convert(value: String, converters: &[Converter]) -> Option<String> {
    converters.iter().try_fold(value, |v, c| c.apply(&v))
}

fn attrs_of(attrs: &BTreeMap<String, String>) -> Vec<Attr> {
    attrs
        .iter()
        .take(MAX_ATTRS)
        .map(|(k, v)| Attr {
            key: Some(k.clone()),
            value: Some(v.clone()),
        })
        .collect()
}
