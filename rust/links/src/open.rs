//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! What happens when a card, or a link in the text, is tapped (ADR-0063 §4.9, §5.5).
//!
//! The target is always the URL in the message — never `canonical_url`, which only serves
//! identification: some platforms carry an access token in a parameter the Normalizer strips.
//! The plan is the same on every platform; Desktop has no "installed app only" call and no app
//! schemes, so it skips straight to `browser` (§4.9 step 3).

use serde::Serialize;

use crate::registry::{LocalizedName, Registry, fill_template};
use crate::urlx;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenStep {
    /// tell.cc: the app's own router (Android `TellomiLinks`, iOS, Desktop `signalRoutes`).
    InApp { url: String },
    /// Hand the https URL to the system, accepting only an installed app (iOS
    /// `.universalLinksOnly`, Android `FLAG_ACTIVITY_REQUIRE_NON_BROWSER`). Not on Desktop.
    InstalledAppOnly { url: String },
    /// The registry's app scheme, filled from what *this* device matched on the URL, every value
    /// percent-encoded. Not on Desktop.
    Scheme { url: String },
    /// An explicit browser: `SFSafariViewController`; Android's `CATEGORY_APP_BROWSER` selector
    /// or Custom Tabs pinned with `setPackage`; Desktop `shell.openExternal`.
    Browser { url: String },
    /// Last resort when not even a browser opened it: copy, and say "Link copied" (§5.5).
    CopyLink { url: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenLabel {
    /// "Open link".
    OpenLink,
    /// "Open in {app}": only when the registry records a verified universal link or a scheme.
    OpenInApp,
    /// "Opening will go to {app}" — payment and ride-hailing: browser only, nothing pre-filled
    /// (§4.9, §6.6).
    LeavesTo,
    /// A tell.cc object, opened inside Tellomi.
    InApp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpenPlan {
    /// Try in order; move on when a step fails. Empty: do nothing at all (not an http(s) URL —
    /// `intent:`, `javascript:`, `data:`, `file:` are never a target).
    pub steps: Vec<OpenStep>,
    pub label: OpenLabel,
    pub app_name: Option<LocalizedName>,
    /// Warn once before opening: the domain looks like this well-known one (§6.1).
    pub lookalike: Option<String>,
}

impl Registry {
    pub fn open_plan(&self, url: &str) -> OpenPlan {
        let mut plan = OpenPlan {
            steps: Vec::new(),
            label: OpenLabel::OpenLink,
            app_name: None,
            lookalike: None,
        };
        let raw = url.trim().to_owned();
        let Some(parsed) = urlx::parse_web_url(&raw) else {
            return plan;
        };
        let browser = |plan: &mut OpenPlan| {
            plan.steps.push(OpenStep::Browser { url: raw.clone() });
            plan.steps.push(OpenStep::CopyLink { url: raw.clone() });
        };
        let host = urlx::host_str(&parsed).unwrap_or_default();
        plan.lookalike = self.known.lookalike_of(&host).map(str::to_owned);
        if parsed.scheme() != "https" {
            // No universal links over http; the browser decides.
            browser(&mut plan);
            return plan;
        }

        if self.is_first_party_url(&parsed) {
            let tellomi = self.tellomi();
            let official = tellomi.official_domains.contains(&host);
            let object = tellomi.match_url(&parsed, &host).route.is_some();
            if !official && object {
                plan.label = OpenLabel::InApp;
                plan.steps.push(OpenStep::InApp { url: raw.clone() });
                plan.steps.push(OpenStep::CopyLink { url: raw });
            } else {
                browser(&mut plan);
            }
            return plan;
        }

        let Some((pid, short, m)) = self.match_parsed(&parsed, false) else {
            plan.steps
                .push(OpenStep::InstalledAppOnly { url: raw.clone() });
            browser(&mut plan);
            return plan;
        };
        let provider = &self.providers[pid];
        plan.app_name = Some(provider.name.clone());
        if provider.locked {
            // Skip the app hand-off entirely: with Alipay installed, the universal link would take
            // the user straight into it.
            plan.label = OpenLabel::LeavesTo;
            browser(&mut plan);
            return plan;
        }
        plan.steps
            .push(OpenStep::InstalledAppOnly { url: raw.clone() });
        let scheme = provider
            .scheme
            .as_ref()
            .filter(|_| !short)
            .and_then(|template| {
                let names = crate::registry::placeholders(template);
                names.iter().all(|n| m.captures.contains_key(n)).then(|| {
                    let encoded = m
                        .captures
                        .iter()
                        .map(|(k, v)| (k.clone(), urlx::encode_component(v)))
                        .collect();
                    fill_template(template, &encoded)
                })
            });
        if provider.universal_verified || scheme.is_some() {
            plan.label = OpenLabel::OpenInApp;
        }
        if let Some(url) = scheme {
            plan.steps.push(OpenStep::Scheme { url });
        }
        browser(&mut plan);
        plan
    }
}
