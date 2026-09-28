//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The provider registry: loading it (with the §6.4 rules re-applied), and the Normalizer and
//! Matcher that turn a URL into `(provider, route, kind, captures, canonical_url)` (README §3).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;
use tellomi_policy::Region;
use tellomi_policy::envelope::{self, EnvelopeError};
use url::Url;

use crate::extract::{Converter, Seg, parse_json_path};
use crate::kinds::{self, WEB};
use crate::limits::*;
use crate::model::{LinksPayload, PlanFile, ProviderFile, RouteFile};
use crate::spoof::KnownDomains;
use crate::{pattern, urlx};

/// A provider's display name (`name` in the provider file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalizedName {
    #[serde(rename = "zh-Hans")]
    pub zh_hans: String,
    #[serde(rename = "zh-Hant", skip_serializing_if = "Option::is_none")]
    pub zh_hant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub en: Option<String>,
}

impl LocalizedName {
    /// `zh-Hant`, `zh-TW`, `zh-HK`, `zh-MO` → Traditional; other `zh*` and an empty locale →
    /// Simplified (the one name every provider must have); anything else → English, falling back
    /// to Simplified.
    pub fn for_locale(&self, locale: &str) -> &str {
        let l = locale.to_ascii_lowercase().replace('_', "-");
        if l.is_empty() {
            return &self.zh_hans;
        }
        let traditional = ["zh-hant", "zh-tw", "zh-hk", "zh-mo"]
            .iter()
            .any(|p| l.starts_with(p));
        if traditional {
            self.zh_hant.as_deref().unwrap_or(&self.zh_hans)
        } else if l.starts_with("zh") {
            &self.zh_hans
        } else {
            self.en.as_deref().unwrap_or(&self.zh_hans)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tier {
    FirstParty,
    Structured,
    Brand,
}

#[derive(Debug, Clone)]
pub(crate) enum Source {
    /// `url-only`: a `{capture}` template.
    Template(String),
    /// `og:<property>` on a page.
    Og(String),
    /// A JSON path into an API response or into the page's JSON-LD node.
    Json(Vec<Seg>),
}

#[derive(Debug, Clone)]
pub(crate) struct MapEntry {
    pub target: String,
    pub source: Source,
    pub converters: Vec<Converter>,
}

#[derive(Debug, Clone)]
pub(crate) enum Plan {
    UrlOnly(Vec<MapEntry>),
    PublicApi {
        url: String,
        map: Vec<MapEntry>,
    },
    Oembed {
        endpoint: String,
        map: Vec<MapEntry>,
    },
    OgJsonld {
        jsonld_type: Option<String>,
        map: Vec<MapEntry>,
    },
    FirstParty,
    None,
}

#[derive(Debug, Clone)]
pub(crate) struct Route {
    pub id: String,
    pub host: Option<String>,
    pub path: Regex,
    pub query: Vec<(String, Regex)>,
    pub fragment: Option<Regex>,
    /// The kind the registry wrote.
    pub declared_kind: String,
    /// The kind this build acts on: the declared one, or `web` when this build does not know it.
    pub kind: String,
    /// Set when a compatibility rule turned the route into a brand shell (§6.4).
    pub degraded: Option<String>,
    pub object: Option<String>,
    pub attrs: Vec<(String, String)>,
    pub plan: Vec<Plan>,
}

impl Route {
    /// Whether this route can ever yield a structured card: known, non-reserved kind, not
    /// degraded, and some plan step besides `none`.
    pub fn is_structured(&self) -> bool {
        self.degraded.is_none()
            && kinds::kind(&self.kind).is_some_and(|k| !k.reserved)
            && self.plan.iter().any(|p| !matches!(p, Plan::None))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Provider {
    pub id: String,
    pub name: LocalizedName,
    pub tier: Tier,
    /// Payment / ride-hailing (by category or by any route's kind): brand only, browser only (L10).
    pub locked: bool,
    pub domains: Vec<String>,
    pub short_domains: Vec<String>,
    pub official_domains: Vec<String>,
    pub reserved_paths: Vec<String>,
    pub host_rewrite: HashMap<String, String>,
    pub strip_params: Vec<String>,
    pub hash_route: bool,
    pub unreachable_in: Vec<Region>,
    pub placeholders: HashMap<String, Vec<String>>,
    pub title_strip: Vec<Regex>,
    pub api_hosts: Vec<String>,
    pub universal_verified: bool,
    pub scheme: Option<String>,
    /// No route recognised an object → generic OG preview instead of a brand shell.
    pub fallback_generic: bool,
    pub routes: Vec<Route>,
}

pub(crate) fn host_in(host: &str, hosts: &[String]) -> bool {
    hosts
        .iter()
        .any(|h| h == host || (h.starts_with("*.") && host.ends_with(&h[1..])))
}

/// `h` is `target` itself or a wildcard that covers it.
fn covers(h: &str, target: &str) -> bool {
    h == target || (h.starts_with("*.") && target.ends_with(&h[1..]))
}

/// Result of matching one URL against one provider.
#[derive(Debug, Clone, Default)]
pub(crate) struct RouteMatch {
    pub route: Option<usize>,
    pub captures: BTreeMap<String, String>,
    pub canonical: String,
    pub object: Option<String>,
}

pub(crate) fn fill_template(template: &str, captures: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if is_ident(&after[..close]) => {
                out.push_str(captures.get(&after[..close]).map_or("", String::as_str));
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `{name}` placeholders in a template.
pub(crate) fn placeholders(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if is_ident(&after[..close]) => {
                out.push(after[..close].to_owned());
                rest = &after[close + 1..];
            }
            _ => rest = after,
        }
    }
    out
}

impl Provider {
    pub fn is_tellomi(&self) -> bool {
        self.id == TELLOMI
    }

    pub fn owns(&self, host: &str) -> bool {
        host_in(host, &self.domains)
            || host_in(host, &self.short_domains)
            || host_in(host, &self.official_domains)
    }

    pub fn is_short(&self, host: &str) -> bool {
        host_in(host, &self.short_domains)
    }

    pub fn unreachable_from(&self, region: Region) -> bool {
        self.unreachable_in.contains(&region)
    }

    fn stripped(&self, key: &str) -> bool {
        self.strip_params.iter().any(|s| match s.strip_suffix('*') {
            Some(prefix) => key.starts_with(prefix),
            None => key == s,
        })
    }

    /// README §3 steps 1 and 3–6, for a host this provider owns and that is not a short domain.
    pub fn match_url(&self, url: &Url, host: &str) -> RouteMatch {
        let host = self
            .host_rewrite
            .get(host)
            .map_or(host, String::as_str)
            .to_owned();
        let mut path = url.path().to_owned();
        let mut query = url.query().unwrap_or("").to_owned();
        let mut fragment = url.fragment().unwrap_or("").to_owned();
        if path.is_empty() {
            path.push('/');
        }
        if self.hash_route && fragment.starts_with('/') {
            let (p, q) = fragment.split_once('?').unwrap_or((fragment.as_str(), ""));
            (path, query) = (p.to_owned(), q.to_owned());
            fragment.clear();
        }
        let kept: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .filter(|(k, _)| !self.stripped(k))
            .collect();
        let mut canonical = format!("https://{host}{path}");
        for (i, (k, v)) in kept.iter().enumerate() {
            canonical.push(if i == 0 { '?' } else { '&' });
            urlx::form_encode(k, &mut canonical);
            canonical.push('=');
            urlx::form_encode(v, &mut canonical);
        }
        // Later duplicates win, as in the registry's reference matcher.
        let params: HashMap<&str, &str> =
            kept.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let fragment = urlx::percent_decode(&fragment);

        for (index, route) in self.routes.iter().enumerate() {
            match &route.host {
                Some(h) if *h != host => continue,
                None if !host_in(&host, &self.domains) => continue,
                _ => {}
            }
            let Some(m) = route.path.captures(&path) else {
                continue;
            };
            let mut captures = BTreeMap::new();
            collect(&route.path, &m, &mut captures);
            let mut ok = true;
            for (name, rx) in &route.query {
                match params.get(name.as_str()).and_then(|v| rx.captures(v)) {
                    Some(qm) => collect(rx, &qm, &mut captures),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok && let Some(rx) = &route.fragment {
                match rx.captures(&fragment) {
                    Some(fm) => collect(rx, &fm, &mut captures),
                    None => ok = false,
                }
            }
            // A reserved first-level path is never a username (`tell.cc/app`); with a
            // discriminator it is (`tell.cc/app.57`), as ADR §4.8 keeps old shapes.
            if ok
                && !captures.contains_key("discriminator")
                && captures
                    .get("nickname")
                    .is_some_and(|n| self.reserved_paths.contains(&n.to_ascii_lowercase()))
            {
                ok = false;
            }
            if ok {
                let object = route.object.as_deref().map(|t| fill_template(t, &captures));
                return RouteMatch {
                    route: Some(index),
                    captures,
                    canonical,
                    object,
                };
            }
        }
        RouteMatch {
            canonical,
            ..Default::default()
        }
    }
}

fn collect(rx: &Regex, m: &regex::Captures<'_>, out: &mut BTreeMap<String, String>) {
    for name in rx.capture_names().flatten() {
        if let Some(v) = m.name(name) {
            out.insert(name.to_owned(), v.as_str().to_owned());
        }
    }
}

/// A rule a registry broke. `rule` is the lint number (`L3`, `L22`, …) so the same bad sample
/// reads the same in `lint.py` and here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Violation {
    pub rule: &'static str,
    pub provider: String,
    pub detail: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.rule, self.provider, self.detail)
    }
}

/// Something this build could not honour and turned into the default instead of rejecting the
/// registry (§6.4 compatibility rules): a route degraded to a brand shell, or (`route = "*"`) a
/// provider-level field such as `fallback` dropped back to its default. The rest applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DegradedRoute {
    pub provider: String,
    /// The route id, or `*` for a provider-level field.
    pub route: String,
    pub reason: String,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// `load_update`: the `.sig` does not verify with the given key over these exact bytes.
    #[error("registry signature does not verify")]
    BadSignature,
    #[error(transparent)]
    Envelope(#[from] EnvelopeError),
    /// `load_update`: a replay, a stale mirror or a rollback.
    #[error("registry version {found} is not newer than {current}")]
    NotNewer { found: u64, current: u64 },
    #[error("registry rejected: {}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    Rejected(Vec<Violation>),
}

impl LoadError {
    /// Every rule the registry broke (empty for envelope-level errors).
    pub fn violations(&self) -> &[Violation] {
        match self {
            LoadError::Rejected(v) => v,
            _ => &[],
        }
    }
}

/// What `identify` found: the Matcher's view of one URL, for tests, the build step and logs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Identified {
    pub provider: String,
    /// `None` when the provider owns the host but no route recognised an object.
    pub route: Option<String>,
    /// The kind the route declares.
    pub kind: Option<String>,
    /// Set when this build degraded the route to a brand shell.
    pub degraded: bool,
    pub object: Option<String>,
    pub captures: BTreeMap<String, String>,
    pub canonical_url: Option<String>,
    pub short: bool,
    /// Only when no route recognised an object on a third-party host: what that link gets —
    /// `"brand"` (a brand shell) or `"generic"` (the provider's `fallback`, README §3 rule 5).
    pub fallback: Option<String>,
}

/// The loaded, validated registry. Immutable; a hot update builds a new one.
#[derive(Debug)]
pub struct Registry {
    version: u64,
    pub(crate) providers: Vec<Provider>,
    exact: HashMap<String, usize>,
    wildcard: Vec<(String, usize)>,
    pub(crate) known: KnownDomains,
    pub(crate) tellomi: usize,
    degraded: Vec<DegradedRoute>,
}

impl Registry {
    /// Load the registry **shipped inside the app** (`links/dist/links-<version>.json`): it is
    /// covered by the app's own code signature, so no registry signature is checked. The content
    /// rules of §6.4 still run in full — the build step is not trusted either.
    ///
    /// Anything that arrives any other way (a hot update) goes through [`Registry::load_update`].
    pub fn load(bytes: &[u8]) -> Result<Registry, LoadError> {
        let env: envelope::Envelope<LinksPayload> = envelope::parse(
            bytes,
            REGISTRY_NAME,
            REGISTRY_SCHEMA_MIN..=REGISTRY_SCHEMA_MAX,
        )?;
        Self::from_payload(env.version, env.payload)
    }

    /// Load a hot update (links/README.md §8, ADR-0063 §5.4 / §7.3), checking in this order:
    /// 1. `signature_hex` (the `.sig` file: a 64-byte XEdDSA signature in hex) verifies over these
    ///    exact bytes with `public_key` — the Desktop update key, 33 bytes as libsignal serializes
    ///    it (`0x05` ‖ 32, `updatesPublicKey` in Desktop's config) or the raw 32 bytes;
    /// 2. `name == "links"`; 3. `schema` in the supported range;
    /// 4. `version` strictly newer than `current_version` (the registry in use);
    /// 5. every content rule of §6.4.
    ///
    /// On any error: keep using the current registry, and say nothing to the user.
    pub fn load_update(
        bytes: &[u8],
        signature_hex: &str,
        public_key: &[u8],
        current_version: Option<u64>,
    ) -> Result<Registry, LoadError> {
        if !verify_registry_signature(bytes, signature_hex, public_key) {
            return Err(LoadError::BadSignature);
        }
        let env: envelope::Envelope<LinksPayload> = envelope::parse(
            bytes,
            REGISTRY_NAME,
            REGISTRY_SCHEMA_MIN..=REGISTRY_SCHEMA_MAX,
        )?;
        if !envelope::is_newer(env.version, current_version) {
            return Err(LoadError::NotNewer {
                found: env.version,
                current: current_version.unwrap_or_default(),
            });
        }
        Self::from_payload(env.version, env.payload)
    }

    pub fn from_payload(version: u64, payload: LinksPayload) -> Result<Registry, LoadError> {
        let mut loader = Loader::default();
        let mut files = Vec::new();
        for (i, value) in payload.providers.into_iter().enumerate() {
            let hint = value
                .get("id")
                .and_then(|v| v.as_str())
                .map_or_else(|| format!("providers[{i}]"), str::to_owned);
            match serde_json::from_value::<ProviderFile>(value) {
                Ok(file) => files.push(file),
                Err(e) => loader.violate("L1", &hint, e.to_string()),
            }
        }
        let providers = loader.check_all(&files);
        if !loader.violations.is_empty() {
            return Err(LoadError::Rejected(loader.violations));
        }
        let mut exact = HashMap::new();
        let mut wildcard = Vec::new();
        let mut known = KnownDomains::default();
        for (i, p) in providers.iter().enumerate() {
            for h in p
                .domains
                .iter()
                .chain(&p.short_domains)
                .chain(&p.official_domains)
            {
                match h.strip_prefix("*.") {
                    Some(suffix) => wildcard.push((suffix.to_owned(), i)),
                    None => {
                        exact.insert(h.clone(), i);
                    }
                }
                known.insert(h);
            }
        }
        for h in compiled_first_party_hosts() {
            known.insert(h);
        }
        for h in &payload.popular_domains {
            if HOST.is_match(h) {
                known.insert(h);
            }
        }
        let tellomi = providers
            .iter()
            .position(Provider::is_tellomi)
            .expect("checked: tellomi is present");
        Ok(Registry {
            version,
            providers,
            exact,
            wildcard,
            known,
            tellomi,
            degraded: loader.degraded,
        })
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    pub fn route_count(&self) -> usize {
        self.providers.iter().map(|p| p.routes.len()).sum()
    }

    /// Routes this build degraded to brand shells while loading (§6.4 compatibility rules).
    pub fn degraded_routes(&self) -> &[DegradedRoute] {
        &self.degraded
    }

    pub(crate) fn provider_for_host(&self, host: &str) -> Option<usize> {
        if let Some(i) = self.exact.get(host) {
            return Some(*i);
        }
        self.wildcard
            .iter()
            .find(|(suffix, _)| {
                host.len() > suffix.len() + 1
                    && host.ends_with(suffix.as_str())
                    && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
            })
            .map(|(_, i)| *i)
    }

    pub(crate) fn tellomi(&self) -> &Provider {
        &self.providers[self.tellomi]
    }

    /// First-party by the URL's own host, exactly (https, `tell.cc` or an enabled official host):
    /// never by `canonical_url`, never through `host_rewrite` (§4.8, B16).
    pub(crate) fn is_first_party_url(&self, url: &Url) -> bool {
        url.scheme() == "https"
            && urlx::host_str(url).is_some_and(|h| {
                let t = self.tellomi();
                (is_first_party_host(&h) && t.domains.contains(&h))
                    || (is_official_host_bound(&h) && t.official_domains.contains(&h))
            })
    }

    /// The Matcher. `location` allows an `http://` URL, for the first `Location` of a short link,
    /// which is only ever used for identification (README §3 step 2).
    pub(crate) fn match_parsed(
        &self,
        url: &Url,
        location: bool,
    ) -> Option<(usize, bool, RouteMatch)> {
        if !(url.scheme() == "https" || (location && url.scheme() == "http")) {
            return None;
        }
        let host = urlx::host_str(url)?;
        let pid = self.provider_for_host(&host)?;
        let provider = &self.providers[pid];
        if provider.is_short(&host) {
            return Some((pid, true, RouteMatch::default()));
        }
        Some((pid, false, provider.match_url(url, &host)))
    }

    /// Identify one URL: which provider, which route, what it captured, and its canonical form.
    /// `None` when no provider claims the host (a generic link).
    pub fn identify(&self, url: &str, location: bool) -> Option<Identified> {
        let parsed = urlx::parse_web_url(url)?;
        let (pid, short, m) = self.match_parsed(&parsed, location)?;
        let provider = &self.providers[pid];
        let route = m.route.map(|r| &provider.routes[r]);
        Some(Identified {
            provider: provider.id.clone(),
            route: route.map(|r| r.id.clone()),
            kind: route.map(|r| r.declared_kind.clone()),
            degraded: route.is_some_and(|r| r.degraded.is_some()),
            object: m.object,
            captures: m.captures,
            canonical_url: (!short).then_some(m.canonical),
            short,
            fallback: (route.is_none() && !short && !provider.is_tellomi()).then(|| {
                if provider.fallback_generic {
                    "generic"
                } else {
                    "brand"
                }
                .to_owned()
            }),
        })
    }
}

/// XEdDSA over the exact bytes, the way `scripts/links/xeddsa.py` signs and Desktop's updater
/// checks its `.sig` files: libsignal's own `PublicKey::verify_signature`, nothing re-implemented.
pub fn verify_registry_signature(bytes: &[u8], signature_hex: &str, public_key: &[u8]) -> bool {
    let hex = signature_hex.trim();
    if hex.len() != 128 || !hex.is_ascii() {
        return false;
    }
    let Ok(signature) = (0..64)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
    else {
        return false;
    };
    let key = match public_key.len() {
        33 => libsignal_core::curve::PublicKey::deserialize(public_key),
        32 => libsignal_core::curve::PublicKey::from_djb_public_key_bytes(public_key),
        _ => return false,
    };
    key.is_ok_and(|k| k.verify_signature(bytes, &signature))
}

// ---------------------------------------------------------------------------------- loading

static HOST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\*\.)?[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$")
        .expect("valid")
});
static PROVIDER_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]{1,31}$").expect("valid"));
static ROUTE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]{0,31}$").expect("valid"));
static STRIP_PARAM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.-]+\*?$").expect("valid"));
static SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9+.-]*://").expect("valid"));
static DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^20[0-9]{2}-[01][0-9]-[0-3][0-9]$").expect("valid"));
static JSONLD_TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z][A-Za-z]+$").expect("valid"));
static RESERVED_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z]{1,8}$").expect("valid"));
static ICON: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9-]+\.png$").expect("valid"));

const SCHEME_DENY: &[&str] = &[
    "http",
    "https",
    "intent",
    "javascript",
    "data",
    "file",
    "content",
    "about",
    "blob",
    "vbscript",
];

fn is_https(s: &str) -> bool {
    s.starts_with("https://") && s.len() > 8 && !s.chars().any(char::is_whitespace)
}

fn url_host(s: &str) -> Option<String> {
    Url::parse(s).ok()?.host_str().map(str::to_ascii_lowercase)
}

#[derive(Default)]
struct Loader {
    violations: Vec<Violation>,
    degraded: Vec<DegradedRoute>,
}

/// Captures of one route: name → the pattern text inside its group, and which came from the
/// fragment.
struct RouteCaptures {
    all: HashMap<String, String>,
    fragment: HashSet<String>,
}

impl Loader {
    fn violate(&mut self, rule: &'static str, provider: &str, detail: impl Into<String>) {
        self.violations.push(Violation {
            rule,
            provider: provider.to_owned(),
            detail: detail.into(),
        });
    }

    fn degrade(&mut self, provider: &str, route: &str, reason: impl Into<String>) {
        self.degraded.push(DegradedRoute {
            provider: provider.to_owned(),
            route: route.to_owned(),
            reason: reason.into(),
        });
    }

    fn check_all(&mut self, files: &[ProviderFile]) -> Vec<Provider> {
        let mut seen = HashSet::new();
        for f in files {
            if !seen.insert(f.id.as_str()) {
                self.violate("L2", &f.id, "duplicate provider id");
            }
        }
        if !files.iter().any(|f| f.id == TELLOMI) {
            // Without it tell.cc would fall back to generic previews (fetching the landing page
            // with the username in the path) and the official host to sender-written cards (§4.8).
            self.violate("L13", TELLOMI, "the tellomi provider is missing");
        }
        self.check_host_ownership(files);
        files
            .iter()
            .filter_map(|f| self.check_provider(f))
            .collect()
    }

    /// L3: one host, one provider — exact hosts, wildcards, and wildcards over others' hosts.
    fn check_host_ownership(&mut self, files: &[ProviderFile]) {
        let mut exact: HashMap<&str, &str> = HashMap::new();
        let mut wild: Vec<(&str, &str)> = Vec::new();
        for f in files {
            let hosts = f
                .domains
                .iter()
                .chain(&f.short_domains)
                .chain(f.official_domains.iter().flatten());
            for h in hosts {
                if let Some(suffix) = h.strip_prefix("*.") {
                    if let Some((_, other)) = wild.iter().find(|(s, o)| *s == suffix && *o != f.id)
                    {
                        self.violate("L3", &f.id, format!("{h} also belongs to {other}"));
                    }
                    wild.push((suffix, &f.id));
                } else {
                    match exact.get(h.as_str()) {
                        Some(other) if *other != f.id => {
                            self.violate("L3", &f.id, format!("{h} also belongs to {other}"));
                        }
                        _ => {
                            exact.insert(h, &f.id);
                        }
                    }
                }
            }
        }
        for (suffix, pid) in &wild {
            for (h, other) in &exact {
                if other != pid && h.ends_with(&format!(".{suffix}")) {
                    self.violate("L3", pid, format!("*.{suffix} covers {other}'s {h}"));
                }
            }
            for (s2, other) in &wild {
                if other != pid && suffix.ends_with(&format!(".{s2}")) {
                    self.violate("L3", pid, format!("*.{suffix} overlaps {other}'s *.{s2}"));
                }
            }
        }
    }

    fn check_shape(&mut self, f: &ProviderFile) {
        let pid = f.id.as_str();
        let bad = |this: &mut Self, what: String| this.violate("L1", pid, what);
        if f.schema != 1 {
            bad(self, format!("schema {} (this build reads 1)", f.schema));
        }
        if !PROVIDER_ID.is_match(pid) {
            bad(self, format!("id {pid:?}"));
        }
        for (lang, name) in [
            ("zh-Hans", Some(&f.name.zh_hans)),
            ("zh-Hant", f.name.zh_hant.as_ref()),
            ("en", f.name.en.as_ref()),
        ] {
            if name.is_some_and(|n| !(1..=32).contains(&n.chars().count())) {
                bad(self, format!("name.{lang} length"));
            }
        }
        if !matches!(f.tier.as_str(), "first-party" | "structured" | "brand") {
            bad(self, format!("tier {:?}", f.tier));
        }
        if f.domains.is_empty() {
            bad(self, "domains is empty".into());
        }
        let host_lists: Vec<(&str, Vec<&String>)> = vec![
            ("domains", f.domains.iter().collect()),
            ("short_domains", f.short_domains.iter().collect()),
            ("api_hosts", f.api_hosts.iter().collect()),
            (
                "official_domains",
                f.official_domains.iter().flatten().collect(),
            ),
            (
                "host_rewrite",
                f.host_rewrite.iter().flat_map(|(a, b)| [a, b]).collect(),
            ),
            (
                "route.host",
                f.route.iter().filter_map(|r| r.host.as_ref()).collect(),
            ),
        ];
        for (field, hosts) in host_lists {
            let mut unique = HashSet::new();
            for h in hosts {
                if !HOST.is_match(h) {
                    bad(self, format!("{field}: {h:?} is not a host"));
                } else if let Some(suffix) = h.strip_prefix("*.")
                    && urlx::registrable_domain(suffix).is_none()
                {
                    bad(
                        self,
                        format!("{field}: {h} is a wildcard over a public suffix"),
                    );
                }
                if field != "host_rewrite" && field != "route.host" && !unique.insert(h) {
                    bad(self, format!("{field}: duplicate {h}"));
                }
            }
        }
        for s in &f.strip_params {
            if !STRIP_PARAM.is_match(s) {
                bad(self, format!("strip_params: {s:?}"));
            }
        }
        if f.icon.as_ref().is_some_and(|i| !ICON.is_match(i)) {
            bad(self, "icon".into());
        }
        if f.attribution
            .as_ref()
            .is_some_and(|a| a.chars().count() > 80)
        {
            bad(self, "attribution longer than 80".into());
        }
        if let Some(t) = &f.terms {
            if !is_https(&t.url) {
                bad(self, format!("terms.url {:?} is not https", t.url));
            }
            if t.checked.as_ref().is_some_and(|d| !DATE.is_match(d)) {
                bad(self, "terms.checked".into());
            }
        }
        if let Some(open) = &f.open {
            if let Some(u) = &open.universal
                && (!DATE.is_match(&u.verified) || u.evidence.is_empty())
            {
                bad(self, "open.universal needs a date and evidence".into());
            }
            if open.scheme.as_ref().is_some_and(|s| !SCHEME.is_match(s)) {
                bad(self, "open.scheme".into());
            }
        }
        if f.evidence.trim().is_empty() {
            bad(self, "evidence is empty".into());
        }
        if let Some(paths) = &f.reserved_paths {
            let mut unique = HashSet::new();
            for p in paths {
                if !RESERVED_PATH.is_match(p) || !unique.insert(p) {
                    bad(self, format!("reserved_paths: {p:?}"));
                }
            }
        }
        for r in &f.route {
            if !ROUTE_ID.is_match(&r.id) {
                bad(self, format!("route id {:?}", r.id));
            }
            if !(r.path.starts_with('^') && r.path.ends_with('$')) {
                bad(self, format!("route {}: path must be ^…$", r.id));
            }
            for pl in &r.plan {
                if pl.url.as_ref().is_some_and(|u| !u.starts_with("https://")) {
                    bad(self, format!("route {}: plan url is not https", r.id));
                }
                for (what, v) in [("endpoint", &pl.endpoint), ("doc", &pl.doc)] {
                    if v.as_ref().is_some_and(|u| !is_https(u)) {
                        bad(self, format!("route {}: plan {what} is not https", r.id));
                    }
                }
                if pl
                    .jsonld_type
                    .as_ref()
                    .is_some_and(|t| !JSONLD_TYPE.is_match(t))
                {
                    bad(self, format!("route {}: jsonld_type", r.id));
                }
            }
        }
    }

    fn check_provider(&mut self, f: &ProviderFile) -> Option<Provider> {
        let before = self.violations.len();
        self.check_shape(f);
        let pid = f.id.as_str();
        let is_tellomi = pid == TELLOMI;
        let first_party_tier = f.tier == "first-party";

        // L13 — by id, never by tier (B12).
        if first_party_tier && !is_tellomi {
            self.violate("L13", pid, "only tellomi can be first-party");
        }
        if is_tellomi && !first_party_tier {
            self.violate(
                "L13",
                pid,
                format!("tellomi must be first-party (tier = {:?})", f.tier),
            );
        }
        let non_empty = |v: &Option<Vec<String>>| v.as_ref().is_some_and(|v| !v.is_empty());
        if is_tellomi && !(non_empty(&f.reserved_paths) && non_empty(&f.official_domains)) {
            self.violate(
                "L13",
                pid,
                "tellomi needs non-empty reserved_paths and official_domains",
            );
        }
        if !is_tellomi && (f.reserved_paths.is_some() || f.official_domains.is_some()) {
            self.violate(
                "L13",
                pid,
                "reserved_paths / official_domains are tellomi's only",
            );
        }

        // L23 — the compiled first-party and official hosts belong to tellomi alone.
        if !is_tellomi {
            let named = f
                .domains
                .iter()
                .map(|h| ("domains", h))
                .chain(f.short_domains.iter().map(|h| ("short_domains", h)))
                .chain(
                    f.official_domains
                        .iter()
                        .flatten()
                        .map(|h| ("official_domains", h)),
                )
                .chain(f.api_hosts.iter().map(|h| ("api_hosts", h)))
                .chain(
                    f.host_rewrite
                        .iter()
                        .flat_map(|(a, b)| [("host_rewrite", a), ("host_rewrite", b)]),
                )
                .chain(
                    f.route
                        .iter()
                        .filter_map(|r| r.host.as_ref().map(|h| ("route.host", h))),
                );
            for (field, h) in named {
                let hit: Vec<&str> = compiled_first_party_hosts()
                    .filter(|c| covers(h, c))
                    .collect();
                if !hit.is_empty() {
                    let verb = if hit.contains(&h.as_str()) {
                        "is"
                    } else {
                        "covers"
                    };
                    let detail = format!("{field}: {h} {verb} first-party / official host {hit:?}");
                    self.violate("L23", pid, detail);
                }
            }
        }

        // L4
        for (a, b) in &f.host_rewrite {
            if !f.domains.contains(a) || !f.domains.contains(b) {
                self.violate(
                    "L4",
                    pid,
                    format!("host_rewrite {a} → {b}: both sides must be in domains"),
                );
            }
        }
        let route_hosts: Vec<String> = f
            .domains
            .iter()
            .chain(f.official_domains.iter().flatten())
            .cloned()
            .collect();

        // L10
        let locked_kinds: Vec<&str> = f
            .route
            .iter()
            .map(|r| r.kind.as_str())
            .filter(|k| kinds::is_locked_kind(k))
            .collect();
        let locked =
            matches!(f.category.as_str(), "payment" | "ride-hailing") || !locked_kinds.is_empty();
        if f.fetch.is_some() {
            self.violate(
                "L10",
                pid,
                "fetch is not allowed (the UA is always WhatsApp/2)",
            );
        }
        if locked && f.tier != "brand" {
            self.violate("L10", pid, "payment / ride-hailing must be brand");
        }
        if locked && f.open.is_some() {
            self.violate("L10", pid, "payment / ride-hailing may not declare open");
        }
        let scheme = f.open.as_ref().and_then(|o| o.scheme.clone());
        if let Some(s) = &scheme {
            let name = s.split(':').next().unwrap_or("");
            if SCHEME_DENY.contains(&name) {
                self.violate("L11", pid, format!("scheme {name} is on the deny list"));
            }
        }

        // L22 / L21 (provider level)
        if is_tellomi {
            let extra: Vec<&String> = f
                .domains
                .iter()
                .filter(|h| !is_first_party_host(h))
                .collect();
            if !extra.is_empty() {
                self.violate(
                    "L22",
                    pid,
                    format!("first-party hosts beyond the compiled bound: {extra:?}"),
                );
            }
            if !f.short_domains.is_empty() {
                self.violate(
                    "L22",
                    pid,
                    "short_domains must be empty (tell.cc is not a short link)",
                );
            }
            let extra: Vec<&String> = f
                .official_domains
                .iter()
                .flatten()
                .filter(|h| !is_official_host_bound(h))
                .collect();
            if !extra.is_empty() {
                self.violate(
                    "L22",
                    pid,
                    format!("official hosts beyond the compiled bound: {extra:?}"),
                );
            }
            if scheme.is_some() {
                self.violate("L21", pid, "first-party may not declare open.scheme");
            }
        }

        let tier = match f.tier.as_str() {
            "first-party" => Tier::FirstParty,
            "brand" => Tier::Brand,
            _ => Tier::Structured,
        };
        let mut all_caps: HashMap<String, Vec<String>> = HashMap::new();
        let mut fragment_caps: HashSet<String> = HashSet::new();
        let mut routes = Vec::new();
        for r in &f.route {
            let caps = self.check_route(f, r, &route_hosts, is_tellomi);
            if let Some(caps) = caps {
                for (name, rx) in &caps.all {
                    all_caps.entry(name.clone()).or_default().push(rx.clone());
                }
                fragment_caps.extend(caps.fragment.iter().cloned());
            }
            if let Some(route) = self.compile_route(f, r) {
                routes.push(route);
            }
        }

        // Scheme placeholders: L20 (never a fragment capture), L6 (exists), L11 (narrow, in every
        // route that spells a capture of that name).
        if let Some(s) = &scheme {
            for name in placeholders(s) {
                if fragment_caps.contains(&name) {
                    self.violate(
                        "L20",
                        pid,
                        format!("open.scheme uses fragment capture {{{name}}}"),
                    );
                }
                match all_caps.get(&name) {
                    None => self.violate(
                        "L6",
                        pid,
                        format!("open.scheme uses {{{name}}}, which is no capture"),
                    ),
                    Some(patterns) => {
                        for rx in patterns {
                            if !pattern::is_narrow_capture(rx) {
                                self.violate(
                                    "L11",
                                    pid,
                                    format!(
                                        "capture {name} used in open.scheme is not narrow: {rx}"
                                    ),
                                );
                            }
                        }
                    }
                }
            }
        }

        // `fallback` is a compatibility field: whatever this build cannot honour falls back to the
        // default brand shell for that provider instead of refusing the update.
        let fallback_generic = match f.fallback.as_deref() {
            None | Some("brand") => false,
            Some("generic") if locked => {
                self.degrade(
                    pid,
                    "*",
                    "fallback = generic is not allowed on a payment / ride-hailing provider (L10)",
                );
                false
            }
            Some("generic") if is_tellomi => {
                self.degrade(
                    pid,
                    "*",
                    "fallback does not apply to tellomi: unknown tell.cc paths stay plain links",
                );
                false
            }
            // L24: only a structured provider may fall through to generic; a brand-tier platform
            // is one whose pages are known to say nothing true about the object.
            Some("generic") if tier != Tier::Structured => {
                self.degrade(
                    pid,
                    "*",
                    "fallback = generic is only for structured providers (L24)",
                );
                false
            }
            Some("generic") => true,
            Some(other) => {
                self.degrade(
                    pid,
                    "*",
                    format!("fallback {other:?} is not known to this build"),
                );
                false
            }
        };

        let mut title_strip = Vec::new();
        for rx in &f.title_strip {
            // L18 is about display quality only: a bad entry is skipped, not fatal.
            if !(rx.starts_with('^') || rx.ends_with('$')) {
                continue;
            }
            match pattern::compile(rx, false) {
                Ok(re) => title_strip.push(re),
                Err(e) => self.violate("L5", pid, format!("title_strip {rx:?}: {e}")),
            }
        }

        if self.violations.len() > before {
            return None;
        }
        let unreachable_in = f
            .unreachable_in
            .iter()
            .filter_map(|r| r.parse::<Region>().ok())
            .collect();
        Some(Provider {
            id: f.id.clone(),
            name: LocalizedName {
                zh_hans: f.name.zh_hans.clone(),
                zh_hant: f.name.zh_hant.clone(),
                en: f.name.en.clone(),
            },
            tier,
            locked,
            domains: f.domains.clone(),
            short_domains: f.short_domains.clone(),
            official_domains: f.official_domains.clone().unwrap_or_default(),
            reserved_paths: f.reserved_paths.clone().unwrap_or_default(),
            host_rewrite: f.host_rewrite.clone().into_iter().collect(),
            strip_params: f.strip_params.clone(),
            hash_route: f.hash_route,
            unreachable_in,
            placeholders: f.placeholders.clone().into_iter().collect(),
            title_strip,
            api_hosts: f.api_hosts.clone(),
            universal_verified: f.open.as_ref().is_some_and(|o| o.universal.is_some()),
            scheme,
            fallback_generic,
            routes,
        })
    }

    /// Security rules for one route (L4, L5, L6, L7 first-party half, L9, L10, L13, L20, L21).
    fn check_route(
        &mut self,
        f: &ProviderFile,
        r: &RouteFile,
        route_hosts: &[String],
        is_tellomi: bool,
    ) -> Option<RouteCaptures> {
        let pid = f.id.as_str();
        let at = format!("{pid}.{}", r.id);
        let kind = kinds::kind(&r.kind);
        if !is_tellomi
            && (r.kind.starts_with(FIRST_PARTY_KIND_PREFIX) || kind.is_some_and(|k| k.first_party))
        {
            self.violate("L7", pid, format!("{at}: {} is a first-party kind", r.kind));
        }
        if let Some(h) = &r.host
            && !host_in(h, route_hosts)
        {
            self.violate(
                "L4",
                pid,
                format!("{at}: host {h} is not in domains / official_domains"),
            );
        }
        if r.fragment.is_some() && !is_tellomi {
            self.violate(
                "L13",
                pid,
                format!("{at}: only tellomi may match on the fragment"),
            );
        }

        let mut caps = RouteCaptures {
            all: HashMap::new(),
            fragment: HashSet::new(),
        };
        let mut patterns: Vec<(&str, &String)> = vec![("path", &r.path)];
        patterns.extend(r.query.values().map(|rx| ("query", rx)));
        if let Some(fr) = &r.fragment {
            patterns.push(("fragment", fr));
        }
        let mut compiled = true;
        for (what, rx) in &patterns {
            if let Err(e) = pattern::compile(rx, true) {
                self.violate("L5", pid, format!("{at}.{what} {rx:?}: {e}"));
                compiled = false;
                continue;
            }
            for (name, body) in pattern::group_patterns(rx) {
                if *what == "fragment" {
                    caps.fragment.insert(name.clone());
                }
                caps.all.insert(name, body);
            }
        }

        let tier = f.tier.as_str();
        if matches!(tier, "structured" | "first-party") && r.plan.is_empty() {
            self.violate("L9", pid, format!("{at}: a {tier} route needs a plan"));
        }
        for (i, pl) in r.plan.iter().enumerate() {
            let pw = format!("{at}.plan[{i}]");
            let t = pl.kind.as_str();
            if tier == "brand" && t != "none" {
                self.violate("L10", pid, format!("{pw}: brand allows only plan none"));
            }
            if t == "first-party" && !is_tellomi {
                self.violate(
                    "L9",
                    pid,
                    format!("{pw}: first-party plan is tellomi's only"),
                );
            }
            if is_tellomi && matches!(t, "public-api" | "oembed") {
                self.violate(
                    "L21",
                    pid,
                    format!("{pw}: first-party may not have a {t} plan"),
                );
            }
            if is_tellomi
                && t == "og+jsonld"
                && !r.host.as_deref().is_some_and(is_official_host_bound)
            {
                self.violate(
                    "L21",
                    pid,
                    format!("{pw}: first-party og+jsonld only on official-host routes"),
                );
            }
            self.check_plan_hosts(f, pl, &pw);
            for (field, template) in [("url", &pl.url), ("endpoint", &pl.endpoint)] {
                for name in template.iter().flat_map(|t| placeholders(t)) {
                    if caps.fragment.contains(&name) {
                        self.violate(
                            "L20",
                            pid,
                            format!("{pw}: {field} template uses fragment capture {{{name}}}"),
                        );
                    }
                    if !caps.all.contains_key(&name) {
                        self.violate(
                            "L6",
                            pid,
                            format!("{pw}: {field} uses {{{name}}}, which is no capture"),
                        );
                    }
                }
            }
            for (target, src) in &pl.map {
                for name in placeholders(src) {
                    if !caps.all.contains_key(&name) {
                        self.violate(
                            "L6",
                            pid,
                            format!("{pw}: map {target} uses {{{name}}}, which is no capture"),
                        );
                    }
                }
            }
        }
        for name in r.object.iter().flat_map(|o| placeholders(o)) {
            if !caps.all.contains_key(&name) {
                self.violate(
                    "L6",
                    pid,
                    format!("{at}: object uses {{{name}}}, which is no capture"),
                );
            }
        }
        compiled.then_some(caps)
    }

    /// L9: API plans name their official doc and terms, and only talk to `api_hosts`.
    fn check_plan_hosts(&mut self, f: &ProviderFile, pl: &PlanFile, pw: &str) {
        let pid = f.id.as_str();
        let (target, what) = match pl.kind.as_str() {
            "public-api" => (pl.url.as_deref(), "url"),
            "oembed" => (pl.endpoint.as_deref(), "endpoint"),
            _ => {
                if pl.url.is_some() || pl.endpoint.is_some() {
                    self.violate(
                        "L9",
                        pid,
                        format!("{pw}: {} may not carry url / endpoint", pl.kind),
                    );
                }
                return;
            }
        };
        if target.is_none() || pl.doc.is_none() || f.terms.is_none() {
            self.violate(
                "L9",
                pid,
                format!(
                    "{pw}: {} needs {what}, doc and the provider's terms",
                    pl.kind
                ),
            );
        }
        let host = target.and_then(url_host);
        if !host.as_ref().is_some_and(|h| f.api_hosts.contains(h)) {
            self.violate(
                "L9",
                pid,
                format!("{pw}: {what} host {host:?} is not in api_hosts"),
            );
        }
    }

    /// Build the runtime route, applying the compatibility rules: an unknown or reserved kind, an
    /// unknown plan type or converter turns this one route into a brand shell (§6.4, §7.2).
    fn compile_route(&mut self, f: &ProviderFile, r: &RouteFile) -> Option<Route> {
        let path = pattern::compile(&r.path, true).ok()?;
        let mut query = Vec::new();
        for (name, rx) in &r.query {
            query.push((name.clone(), pattern::compile(rx, true).ok()?));
        }
        let fragment = match &r.fragment {
            Some(rx) => Some(pattern::compile(rx, true).ok()?),
            None => None,
        };
        let kind_def = kinds::kind(&r.kind);
        let mut degraded = None;
        let mut plans = Vec::new();
        for pl in &r.plan {
            let map = || -> Result<Vec<MapEntry>, String> { parse_map(pl, kind_def) };
            let plan = match pl.kind.as_str() {
                "url-only" => map().map(Plan::UrlOnly),
                "public-api" => map().map(|map| Plan::PublicApi {
                    url: pl.url.clone().unwrap_or_default(),
                    map,
                }),
                "oembed" => map().map(|map| Plan::Oembed {
                    endpoint: pl.endpoint.clone().unwrap_or_default(),
                    map,
                }),
                "og+jsonld" => map().map(|map| Plan::OgJsonld {
                    jsonld_type: pl.jsonld_type.clone(),
                    map,
                }),
                "first-party" => Ok(Plan::FirstParty),
                "none" => Ok(Plan::None),
                other => Err(format!("plan type {other:?} is not known to this build")),
            };
            match plan {
                Ok(p) => plans.push(p),
                Err(reason) => {
                    degraded.get_or_insert(reason);
                }
            }
        }
        match kind_def {
            None => {
                degraded = Some(format!("kind {:?} is not known to this build", r.kind));
            }
            Some(k) if k.reserved && plans.iter().any(|p| !matches!(p, Plan::None)) => {
                degraded = Some(format!("kind {} is reserved", r.kind));
            }
            _ => {}
        }
        if let Some(reason) = &degraded {
            self.degrade(&f.id, &r.id, reason.clone());
            plans = vec![Plan::None];
        }
        let attrs = r
            .attrs
            .iter()
            .filter(|(k, _)| kind_def.is_some_and(|kd| kd.attr_type(k).is_some()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Some(Route {
            id: r.id.clone(),
            host: r.host.clone(),
            path,
            query,
            fragment,
            declared_kind: r.kind.clone(),
            kind: if kind_def.is_some() {
                r.kind.clone()
            } else {
                WEB.to_owned()
            },
            degraded,
            object: r.object.clone(),
            attrs,
            plan: plans,
        })
    }
}

/// Parse a plan's `map`. Entries whose target the kind does not have are dropped (a newer
/// registry may use an attr this build cannot render); an unknown converter or source syntax
/// degrades the whole route.
fn parse_map(pl: &PlanFile, kind: Option<&kinds::KindDef>) -> Result<Vec<MapEntry>, String> {
    let mut out = Vec::new();
    for (target, expr) in &pl.map {
        let mut parts = expr.split('|');
        let source = parts.next().unwrap_or("").trim();
        let mut converters = Vec::new();
        for c in parts {
            converters.push(
                Converter::parse(c.trim())
                    .ok_or_else(|| format!("converter {c:?} is not known to this build"))?,
            );
        }
        let source = match pl.kind.as_str() {
            "url-only" => Source::Template(source.to_owned()),
            "og+jsonld" if source.starts_with("og:") => Source::Og(source.to_ascii_lowercase()),
            _ => Source::Json(
                parse_json_path(source)
                    .ok_or_else(|| format!("source {source:?} is not a JSON path"))?,
            ),
        };
        let known_target = matches!(target.as_str(), "title" | "description" | "image")
            || kind.is_some_and(|k| k.attr_type(target).is_some());
        if known_target {
            out.push(MapEntry {
                target: target.clone(),
                source,
                converters,
            });
        }
    }
    Ok(out)
}
