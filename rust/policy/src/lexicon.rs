//
// Copyright (C) 2026 Tellomi.
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The lexicon: what the engine loads, and the vocabulary it is expressed in (ADR-0062 §4.1, §5.1).
//!
//! The wire format is the shared registry envelope (`name`/`version`/`schema`/`inputs`/`payload`)
//! also used by the links and sticker registries, so one loader and one update path serve all three.
//! Data is never compiled into this crate: the lexicon carries CC-BY-4.0 material and must stay a
//! separate aggregate from AGPL code (ADR-0062 §5.3).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Where a term may apply. Adding a field is a data change plus one variant — never engine logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Username,
    Slug,
    DisplayName,
    ProfileName,
    GroupName,
    GroupDescription,
    ChannelName,
    BotUsername,
    MiniappName,
    OrgName,
    Bio,
    StickerPackTitle,
    LinkPreview,
    Search,
}

impl std::str::FromStr for Field {
    type Err = UnknownName;

    /// Field names are exactly the strings the lexicon files use, so a caller spells them the way
    /// an author does: `username`, `display_name`, `group_name`, `slug`, …
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_value(serde_json::Value::String(s.to_owned())).map_err(|_| UnknownName {
            kind: "field",
            name: s.to_owned(),
        })
    }
}

impl std::str::FromStr for Region {
    type Err = UnknownName;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_value(serde_json::Value::String(s.to_owned())).map_err(|_| UnknownName {
            kind: "region",
            name: s.to_owned(),
        })
    }
}

/// A field or region name that is not in the vocabulary. Callers hear about it rather than getting
/// a silent `allowed`: a misspelled field would otherwise mean "no rules apply".
#[derive(Debug, thiserror::Error, displaydoc::Display)]
#[displaydoc("unknown policy {kind} {name:?}")]
pub struct UnknownName {
    pub kind: &'static str,
    pub name: String,
}

/// Parse a comma-separated region list, e.g. `global` or `global,cn`. Empty means `global`.
pub fn parse_regions(regions: &str) -> Result<Vec<Region>, UnknownName> {
    let trimmed = regions.trim();
    if trimmed.is_empty() {
        return Ok(vec![Region::Global]);
    }
    trimmed.split(',').map(|code| code.trim().parse()).collect()
}

impl Field {
    /// Fields whose input is guaranteed ASCII by a layer above us (libsignal's `validate_nickname`
    /// for usernames, our own slug grammar for tell.cc). Unicode-only rules are pointless there —
    /// see the per-field test matrix in ADR-0062 §6.
    pub fn is_ascii_only(self) -> bool {
        matches!(self, Field::Username | Field::Slug)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    Global,
    Cn,
}

/// How a term is compared against the (normalized) input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Match {
    /// Raw input equals the term, byte for byte.
    Exact,
    /// Normalized input equals the normalized term.
    NormalizedExact,
    /// Normalized input starts with the term **and** ends at a boundary
    /// (`admin_x` matches `admin`, `administer` does not).
    Prefix,
    /// Mirror of `Prefix` at the end of the input.
    Suffix,
    /// Normalized input contains the term anywhere. Restricted by the builder to terms long
    /// enough not to shred ordinary names (>= 2 CJK chars or >= 5 Latin chars).
    Contains,
    /// Skeletons (UTS #39 + ASCII lookalike folding) are equal: it *looks* like the term.
    Confusable,
}

/// Why an input was refused. The business layer maps this to user-facing copy; the copy never
/// names the rule, the lexicon or the category (ADR-0062 §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Allowed,
    Reserved,
    BrandProtected,
    Impersonation,
    SecuritySensitive,
    RegionalRestricted,
    ContentRestricted,
    InvalidUnicode,
    Confusable,
}

impl Outcome {
    pub fn is_allowed(self) -> bool {
        self == Outcome::Allowed
    }
}

/// One entry of the compiled lexicon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Stable id for audit logs: `<source>:<line>`, e.g. `tellomi/brand:12`.
    pub id: String,
    /// The term as written by the lexicon author (kept raw for `Match::Exact`).
    pub term: String,
    /// The same term after `normalize::normalize`, precomputed by the builder.
    pub normalized: String,
    /// `normalize::skeleton(normalized)`, precomputed; empty when the rule has no `Confusable`.
    #[serde(default)]
    pub skeleton: String,
    pub matches: Vec<Match>,
    pub fields: Vec<Field>,
    pub regions: Vec<Region>,
    pub outcome: Outcome,
    /// Which lexicon file it came from, for audit only. Never crosses the language bridge.
    pub source: String,
}

impl Rule {
    pub fn applies_to(&self, field: Field, regions: &[Region]) -> bool {
        self.fields.contains(&field) && self.regions.iter().any(|r| regions.contains(r))
    }
}

/// The payload of a `stickers`-style envelope, for `name = "policy"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LexiconPayload {
    pub rules: Vec<Rule>,
    /// Allowlist entries: normalized strings that are always `Allowed` for the listed fields and
    /// regions, whatever any rule says. `allowlist > tellomi > upstream` (ADR-0062 §5.1).
    #[serde(default)]
    pub allow: Vec<AllowEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowEntry {
    pub normalized: String,
    pub fields: Vec<Field>,
    pub regions: Vec<Region>,
    #[serde(default)]
    pub note: String,
}

/// The shared registry envelope. `payload` is generic so links/stickers can reuse it later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub name: String,
    pub version: u64,
    pub schema: u32,
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub generated_by: String,
    #[serde(default)]
    pub inputs: Vec<EnvelopeInput>,
    pub payload: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvelopeInput {
    pub path: String,
    pub sha256: String,
}

/// Schema versions this build understands. Anything outside the range means "keep the copy that
/// shipped with the app" rather than "fail".
pub const SCHEMA_MIN: u32 = 1;
pub const SCHEMA_MAX: u32 = 1;

/// Sanity checks the builder is also expected to enforce; re-checked at load time because a
/// hot-updated file is not necessarily one we built.
pub(crate) fn validate(payload: &LexiconPayload) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for rule in &payload.rules {
        if !ids.insert(&rule.id) {
            return Err(format!("duplicate rule id {}", rule.id));
        }
        if rule.term.is_empty() || rule.normalized.is_empty() {
            return Err(format!("rule {} has an empty term", rule.id));
        }
        if rule.matches.is_empty() || rule.fields.is_empty() || rule.regions.is_empty() {
            return Err(format!("rule {} matches nothing", rule.id));
        }
        if rule.matches.contains(&Match::Confusable) && rule.skeleton.is_empty() {
            return Err(format!("rule {} is CONFUSABLE without a skeleton", rule.id));
        }
        if rule.matches.contains(&Match::Contains) && !is_long_enough_for_contains(&rule.normalized)
        {
            return Err(format!(
                "rule {} uses CONTAINS with a short term ({:?}): that shreds ordinary names",
                rule.id, rule.normalized
            ));
        }
        if rule.outcome == Outcome::Allowed {
            return Err(format!(
                "rule {} has outcome=allowed; use the allowlist",
                rule.id
            ));
        }
    }
    Ok(())
}

/// A `CONTAINS` term must be long enough that ordinary names do not collide with it:
/// at least 2 CJK characters, or 5 characters otherwise.
pub fn is_long_enough_for_contains(normalized: &str) -> bool {
    let cjk = normalized
        .chars()
        .filter(|c| {
            matches!(*c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F)
        })
        .count();
    if cjk > 0 {
        cjk >= 2
    } else {
        normalized.chars().count() >= 5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_length_rule() {
        assert!(is_long_enough_for_contains("admin")); // exactly 5 latin chars
        assert!(is_long_enough_for_contains("tellomi"));
        assert!(!is_long_enough_for_contains("abcd")); // 4 is too short to scan for
        assert!(!is_long_enough_for_contains("adm"));
        assert!(is_long_enough_for_contains("支持"));
        assert!(!is_long_enough_for_contains("支"));
    }

    #[test]
    fn ascii_only_fields() {
        assert!(Field::Username.is_ascii_only());
        assert!(Field::Slug.is_ascii_only());
        assert!(!Field::DisplayName.is_ascii_only());
    }
}
