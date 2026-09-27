//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The registry as data: one provider file (`providers/<tier>/<id>.toml`, README §2) after the
//! builder has turned it into JSON, and the envelope payload that carries them.
//!
//! Field names follow `provider.schema.json` exactly. Unknown fields are ignored rather than
//! rejected — the payload only ever grows (§7.3), and an old client must keep loading a newer
//! registry. The one field that is looked for precisely so it can be refused is `fetch` (L10).

use std::collections::BTreeMap;

use serde::Deserialize;

/// The envelope `payload` for `name = "links"`.
#[derive(Debug, Clone, Deserialize)]
pub struct LinksPayload {
    /// Every provider file, in any order.
    pub providers: Vec<serde_json::Value>,
    /// The "well-known domains" list that travels with the registry and feeds the lookalike check
    /// together with every provider's own hosts (§6.1). Optional.
    #[serde(default)]
    pub popular_domains: Vec<String>,
    // `kinds` may be present (the builder copies `kinds.toml` in for reference). It is ignored:
    // this build renders the kinds compiled into it (§7.2).
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderFile {
    pub schema: u32,
    pub id: String,
    pub name: Names,
    pub tier: String,
    pub category: String,
    #[serde(default)]
    pub icon: Option<String>,
    pub domains: Vec<String>,
    #[serde(default)]
    pub short_domains: Vec<String>,
    #[serde(default)]
    pub host_rewrite: BTreeMap<String, String>,
    #[serde(default)]
    pub strip_params: Vec<String>,
    #[serde(default)]
    pub hash_route: bool,
    #[serde(default)]
    pub unreachable_in: Vec<String>,
    #[serde(default)]
    pub placeholders: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub title_strip: Vec<String>,
    #[serde(default)]
    pub api_hosts: Vec<String>,
    #[serde(default)]
    pub attribution: Option<String>,
    #[serde(default)]
    pub terms: Option<Terms>,
    #[serde(default)]
    pub open: Option<OpenFile>,
    pub evidence: String,
    /// Presence matters, not only content: only `tellomi` may carry the key at all (L13).
    #[serde(default)]
    pub official_domains: Option<Vec<String>>,
    #[serde(default)]
    pub reserved_paths: Option<Vec<String>>,
    #[serde(default)]
    pub route: Vec<RouteFile>,
    /// What a link on this provider's hosts gets when no route recognises an object: `"brand"`
    /// (default; README §3 rule 5) or `"generic"` (fall through to the generic OG path, e.g.
    /// GitHub issues and PRs, which have no kind but a good page preview).
    #[serde(default)]
    pub fallback: Option<String>,
    /// Reserved (`fetch.ua`) and forbidden: its mere presence rejects the registry (L10).
    #[serde(default)]
    pub fetch: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Names {
    #[serde(rename = "zh-Hans")]
    pub zh_hans: String,
    #[serde(rename = "zh-Hant", default)]
    pub zh_hant: Option<String>,
    #[serde(default)]
    pub en: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Terms {
    pub url: String,
    #[serde(default)]
    pub checked: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenFile {
    #[serde(default)]
    pub universal: Option<Universal>,
    #[serde(default)]
    pub scheme: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Universal {
    pub verified: String,
    pub evidence: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteFile {
    pub id: String,
    #[serde(default)]
    pub host: Option<String>,
    pub path: String,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub fragment: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub attrs: BTreeMap<String, String>,
    #[serde(default)]
    pub plan: Vec<PlanFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlanFile {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub jsonld_type: Option<String>,
    #[serde(default)]
    pub map: BTreeMap<String, String>,
}
