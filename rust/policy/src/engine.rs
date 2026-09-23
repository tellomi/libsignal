//
// Copyright (C) 2026 Tellomi.
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The engine: load a lexicon once, then answer `check(input, field, regions)` in microseconds.
//!
//! Matching strategy (ADR-0062 §4.3):
//! * `Exact` / `NormalizedExact` / `Confusable` — hash lookups.
//! * `Prefix` / `Suffix` — hash lookups over the candidate prefixes/suffixes of the input, which is
//!   bounded by the input's length rather than by the size of the lexicon.
//! * `Contains` — a single Aho-Corasick pass over the normalized input, so a hundred thousand
//!   terms cost one scan rather than a hundred thousand `contains` calls.

use std::collections::HashMap;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

use crate::lexicon::*;
use crate::normalize;

/// What the engine concluded. `hit` stays on this side of the language bridge: the outcome is all
/// the UI is ever told (ADR-0062 §12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub outcome: Outcome,
    pub hit: Option<Hit>,
}

impl Verdict {
    pub fn allowed() -> Self {
        Self {
            outcome: Outcome::Allowed,
            hit: None,
        }
    }

    pub fn is_allowed(&self) -> bool {
        self.outcome.is_allowed()
    }
}

/// Which rule fired, for audit logs and for the lexicon authors' own tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub rule_id: String,
    pub term: String,
    pub matched: Match,
    pub source: String,
}

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum LoadError {
    /// not a policy lexicon: name was {0:?}
    WrongPayload(String),
    /// lexicon schema {found} is outside the supported range {min}..={max}
    UnsupportedSchema { found: u32, min: u32, max: u32 },
    /// lexicon is malformed: {0}
    Invalid(String),
    /// lexicon could not be parsed: {0}
    Parse(#[from] serde_json::Error),
}

/// One immutable, shareable lexicon. Cheap to clone behind an `Arc`; a hot update swaps the whole
/// value rather than mutating it, so readers never see a half-applied lexicon.
pub struct PolicyEngine {
    version: u64,
    rules: Vec<Rule>,
    /// normalized term -> rule indices that use an equality-shaped match
    by_normalized: HashMap<String, Vec<usize>>,
    /// raw term -> rule indices using `Exact`
    by_raw: HashMap<String, Vec<usize>>,
    /// skeleton -> rule indices using `Confusable`
    by_skeleton: HashMap<String, Vec<usize>>,
    /// rule indices using `Prefix` / `Suffix`, keyed by normalized term
    by_affix: HashMap<String, Vec<usize>>,
    /// Aho-Corasick over the `Contains` terms, plus the rule index for each pattern
    contains: Option<AhoCorasick>,
    contains_rules: Vec<usize>,
    /// (field, region, normalized) triples that are always allowed
    allow: Vec<AllowEntry>,
}

impl std::fmt::Debug for PolicyEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyEngine")
            .field("version", &self.version)
            .field("rules", &self.rules.len())
            .field("allow", &self.allow.len())
            .finish_non_exhaustive()
    }
}

impl PolicyEngine {
    /// Parse and index a lexicon. Returns an error rather than a partial engine: the caller falls
    /// back to the copy that shipped with the app.
    pub fn load(bytes: &[u8]) -> Result<Self, LoadError> {
        let envelope: Envelope<LexiconPayload> = serde_json::from_slice(bytes)?;
        if envelope.name != "policy" {
            return Err(LoadError::WrongPayload(envelope.name));
        }
        if envelope.schema < SCHEMA_MIN || envelope.schema > SCHEMA_MAX {
            return Err(LoadError::UnsupportedSchema {
                found: envelope.schema,
                min: SCHEMA_MIN,
                max: SCHEMA_MAX,
            });
        }
        validate(&envelope.payload).map_err(LoadError::Invalid)?;
        Ok(Self::index(envelope.version, envelope.payload))
    }

    fn index(version: u64, payload: LexiconPayload) -> Self {
        let mut by_normalized: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_raw: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_skeleton: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_affix: HashMap<String, Vec<usize>> = HashMap::new();
        let mut contains_patterns: Vec<String> = Vec::new();
        let mut contains_rules: Vec<usize> = Vec::new();

        for (i, rule) in payload.rules.iter().enumerate() {
            for m in &rule.matches {
                match m {
                    Match::Exact => by_raw.entry(rule.term.clone()).or_default().push(i),
                    Match::NormalizedExact => by_normalized
                        .entry(rule.normalized.clone())
                        .or_default()
                        .push(i),
                    Match::Confusable => by_skeleton
                        .entry(rule.skeleton.clone())
                        .or_default()
                        .push(i),
                    Match::Prefix | Match::Suffix => {
                        let entry = by_affix.entry(rule.normalized.clone()).or_default();
                        if !entry.contains(&i) {
                            entry.push(i);
                        }
                    }
                    Match::Contains => {
                        contains_patterns.push(rule.normalized.clone());
                        contains_rules.push(i);
                    }
                }
            }
        }

        let contains = if contains_patterns.is_empty() {
            None
        } else {
            Some(
                AhoCorasickBuilder::new()
                    .match_kind(MatchKind::LeftmostFirst)
                    .build(&contains_patterns)
                    .expect("patterns were validated at load"),
            )
        };

        Self {
            version,
            rules: payload.rules,
            by_normalized,
            by_raw,
            by_skeleton,
            by_affix,
            contains,
            contains_rules,
            allow: payload.allow,
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// The whole public surface. Everything else in this crate exists to serve it.
    pub fn check(&self, input: &str, field: Field, regions: &[Region]) -> Verdict {
        if input.is_empty() {
            return Verdict::allowed();
        }

        // 1. Structural rejections. These do not consult the lexicon at all, so a name made of
        //    invisible characters is refused even if no rule mentions it.
        if normalize::has_disallowed_control(input) {
            return Verdict {
                outcome: Outcome::InvalidUnicode,
                hit: None,
            };
        }
        let normalized = normalize::normalize(input);
        if normalized.is_empty() {
            return Verdict {
                outcome: Outcome::InvalidUnicode,
                hit: None,
            };
        }
        if !field.is_ascii_only() && normalize::is_mixed_script(&normalized) {
            return Verdict {
                outcome: Outcome::Confusable,
                hit: None,
            };
        }

        // 2. The allowlist wins over every rule, including upstream ones we did not write.
        if self.is_allowed(&normalized, field, regions) {
            return Verdict::allowed();
        }

        // 3. Rules. Several may fire on one input — "Tellomi Support" matches both the brand rule
        //    `tellomi` (as a prefix) and the impersonation rule `tellomi_support` (as a
        //    confusable). The most specific rule wins, i.e. the one with the longest term, so the
        //    answer does not depend on the order the strategies happen to run in.
        let mut best: Option<(usize, Match)> = None;
        let mut consider = |candidate: Option<(usize, Match)>| {
            if let Some((idx, matched)) = candidate {
                let better = match best {
                    None => true,
                    Some((current, _)) => {
                        self.rules[idx].normalized.chars().count()
                            > self.rules[current].normalized.chars().count()
                    }
                };
                if better {
                    best = Some((idx, matched));
                }
            }
        };
        consider(self.match_equality(input, &normalized, field, regions));
        consider(self.match_affix(&normalized, field, regions));
        consider(self.match_confusable(&normalized, field, regions));
        consider(self.match_contains(&normalized, field, regions));

        match best {
            Some((idx, matched)) => self.verdict_for(idx, matched),
            None => Verdict::allowed(),
        }
    }

    fn is_allowed(&self, normalized: &str, field: Field, regions: &[Region]) -> bool {
        self.allow.iter().any(|a| {
            a.normalized == normalized
                && a.fields.contains(&field)
                && a.regions.iter().any(|r| regions.contains(r))
        })
    }

    fn verdict_for(&self, idx: usize, matched: Match) -> Verdict {
        let rule = &self.rules[idx];
        Verdict {
            outcome: rule.outcome,
            hit: Some(Hit {
                rule_id: rule.id.clone(),
                term: rule.term.clone(),
                matched,
                source: rule.source.clone(),
            }),
        }
    }

    fn first_applicable(
        &self,
        candidates: Option<&Vec<usize>>,
        field: Field,
        regions: &[Region],
        wanted: Match,
    ) -> Option<(usize, Match)> {
        candidates?
            .iter()
            .find(|&&i| {
                self.rules[i].applies_to(field, regions) && self.rules[i].matches.contains(&wanted)
            })
            .map(|&i| (i, wanted))
    }

    fn match_equality(
        &self,
        raw: &str,
        normalized: &str,
        field: Field,
        regions: &[Region],
    ) -> Option<(usize, Match)> {
        self.first_applicable(self.by_raw.get(raw), field, regions, Match::Exact)
            .or_else(|| {
                self.first_applicable(
                    self.by_normalized.get(normalized),
                    field,
                    regions,
                    Match::NormalizedExact,
                )
            })
    }

    /// Prefix/suffix need a boundary so that `administer` does not match the rule `admin`.
    /// The boundary is "not a letter or digit", or the end of the string.
    fn match_affix(
        &self,
        normalized: &str,
        field: Field,
        regions: &[Region],
    ) -> Option<(usize, Match)> {
        let chars: Vec<char> = normalized.chars().collect();

        for end in 1..chars.len() {
            if chars[end].is_alphanumeric() {
                continue; // no boundary here
            }
            let candidate: String = chars[..end].iter().collect();
            if let Some(found) =
                self.first_applicable(self.by_affix.get(&candidate), field, regions, Match::Prefix)
            {
                return Some(found);
            }
        }
        for start in 1..chars.len() {
            if chars[start - 1].is_alphanumeric() {
                continue;
            }
            let candidate: String = chars[start..].iter().collect();
            if let Some(found) =
                self.first_applicable(self.by_affix.get(&candidate), field, regions, Match::Suffix)
            {
                return Some(found);
            }
        }
        None
    }

    fn match_confusable(
        &self,
        normalized: &str,
        field: Field,
        regions: &[Region],
    ) -> Option<(usize, Match)> {
        let skeleton = normalize::skeleton(normalized);
        self.first_applicable(
            self.by_skeleton.get(&skeleton),
            field,
            regions,
            Match::Confusable,
        )
    }

    fn match_contains(
        &self,
        normalized: &str,
        field: Field,
        regions: &[Region],
    ) -> Option<(usize, Match)> {
        let ac = self.contains.as_ref()?;
        for m in ac.find_iter(normalized) {
            let idx = self.contains_rules[m.pattern().as_usize()];
            if self.rules[idx].applies_to(field, regions) {
                return Some((idx, Match::Contains));
            }
        }
        None
    }
}
