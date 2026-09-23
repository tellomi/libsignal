//
// Copyright 2026 Tellomi
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Tellomi policy engine bridge (ADR-0062).
//!
//! Three clients and the server ask the same question of the same lexicon, so the engine lives in
//! Rust and every platform reaches it through this bridge. The surface is deliberately tiny:
//!
//! * `PolicyEngine_Load` — parse and index a lexicon once; the caller keeps the handle.
//! * `PolicyEngine_Check` — ask about one string, get one number back.
//!
//! **Only the outcome crosses the bridge.** `Verdict::hit` names the rule, the term and the
//! lexicon file it came from; that is for our own logs, not for a UI and not for a client that
//! could be used to enumerate the lexicon one guess at a time (ADR-0062 §12). Keeping it on this
//! side is why the return value here is a `u8` rather than the `Verdict` itself.

use libsignal_bridge_macros::*;
use tellomi_policy::{Field, Outcome, PolicyEngine};

#[allow(unused_imports)]
use crate::support::*;
use crate::*;

bridge_handle_fns!(PolicyEngine, clone = false);

/// Outcome values as they cross the bridge. Stable: the three clients switch on these numbers, so
/// a value's meaning never changes and new outcomes only ever get new numbers.
///
/// A client that does not recognise a number must treat it as "not allowed, no detail", which is
/// what the generic copy says anyway.
const OUTCOME_ALLOWED: u8 = 0;
const OUTCOME_RESERVED: u8 = 1;
const OUTCOME_BRAND_PROTECTED: u8 = 2;
const OUTCOME_IMPERSONATION: u8 = 3;
const OUTCOME_SECURITY_SENSITIVE: u8 = 4;
const OUTCOME_REGIONAL_RESTRICTED: u8 = 5;
const OUTCOME_CONTENT_RESTRICTED: u8 = 6;
const OUTCOME_INVALID_UNICODE: u8 = 7;
const OUTCOME_CONFUSABLE: u8 = 8;

fn outcome_code(outcome: Outcome) -> u8 {
    match outcome {
        Outcome::Allowed => OUTCOME_ALLOWED,
        Outcome::Reserved => OUTCOME_RESERVED,
        Outcome::BrandProtected => OUTCOME_BRAND_PROTECTED,
        Outcome::Impersonation => OUTCOME_IMPERSONATION,
        Outcome::SecuritySensitive => OUTCOME_SECURITY_SENSITIVE,
        Outcome::RegionalRestricted => OUTCOME_REGIONAL_RESTRICTED,
        Outcome::ContentRestricted => OUTCOME_CONTENT_RESTRICTED,
        Outcome::InvalidUnicode => OUTCOME_INVALID_UNICODE,
        Outcome::Confusable => OUTCOME_CONFUSABLE,
    }
}

#[bridge_fn]
pub fn PolicyEngine_Load(lexicon: &[u8]) -> Result<PolicyEngine, IllegalArgumentError> {
    PolicyEngine::load(lexicon).map_err(|e| IllegalArgumentError::new(e.to_string()))
}

/// The lexicon version this engine was built from, so a client can tell whether a hot update
/// actually took effect.
#[bridge_fn]
pub fn PolicyEngine_Version(engine: &PolicyEngine) -> u64 {
    engine.version()
}

#[bridge_fn]
pub fn PolicyEngine_RuleCount(engine: &PolicyEngine) -> u32 {
    u32::try_from(engine.rule_count()).unwrap_or(u32::MAX)
}

/// Check one string. Returns one of the `OUTCOME_*` codes above; `0` means allowed.
#[bridge_fn]
pub fn PolicyEngine_Check(
    engine: &PolicyEngine,
    input: String,
    field: String,
    regions: String,
) -> Result<u8, IllegalArgumentError> {
    // Field and region names are the same strings the lexicon files use (`username`,
    // `display_name`, `global,cn`), so callers spell them the way an author does. A name we do not
    // know is an error rather than a silent `allowed`: a misspelled field would otherwise read as
    // "no rules apply to this".
    let field: Field = field
        .parse()
        .map_err(|e: tellomi_policy::UnknownName| IllegalArgumentError::new(e.to_string()))?;
    let regions = tellomi_policy::parse_regions(&regions)
        .map_err(|e| IllegalArgumentError::new(e.to_string()))?;
    Ok(outcome_code(engine.check(&input, field, &regions).outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-rule lexicon, written out literally rather than built with `serde_json::json!`: the
    /// bridge crate has no JSON dependency and this test is not a reason to give it one.
    const FIXTURE: &str = r#"{
        "name": "policy",
        "version": 2026092201,
        "schema": 1,
        "payload": {
            "rules": [{
                "id": "brand:tellomi",
                "term": "tellomi",
                "normalized": "tellomi",
                "skeleton": "",
                "matches": ["NORMALIZED_EXACT"],
                "fields": ["username", "display_name"],
                "regions": ["global"],
                "outcome": "brand_protected",
                "source": "tests"
            }],
            "allow": []
        }
    }"#;

    fn fixture() -> PolicyEngine {
        PolicyEngine_Load(FIXTURE.as_bytes()).expect("fixture loads")
    }

    #[test]
    fn outcomes_cross_the_bridge_as_stable_numbers() {
        let engine = fixture();
        assert_eq!(
            PolicyEngine_Check(
                &engine,
                "tellomi".into(),
                "username".into(),
                "global".into()
            )
            .unwrap(),
            OUTCOME_BRAND_PROTECTED
        );
        assert_eq!(
            PolicyEngine_Check(
                &engine,
                "xiaoming".into(),
                "username".into(),
                "global".into()
            )
            .unwrap(),
            OUTCOME_ALLOWED
        );
    }

    #[test]
    fn regions_parse_the_way_callers_write_them() {
        use tellomi_policy::{Region, parse_regions};
        assert_eq!(parse_regions("").unwrap(), vec![Region::Global]);
        assert_eq!(parse_regions("global").unwrap(), vec![Region::Global]);
        assert_eq!(
            parse_regions(" global , cn ").unwrap(),
            vec![Region::Global, Region::Cn]
        );
        assert!(parse_regions("global,mars").is_err());
    }

    #[test]
    fn an_unknown_field_is_an_error_rather_than_a_silent_pass() {
        let engine = fixture();
        // The dangerous failure mode would be treating an unrecognised field as "no rules apply"
        // and answering `allowed`; a caller that misspells a field must hear about it.
        assert!(
            PolicyEngine_Check(
                &engine,
                "tellomi".into(),
                "user_name".into(),
                "global".into()
            )
            .is_err()
        );
    }

    #[test]
    fn a_malformed_lexicon_does_not_produce_a_half_loaded_engine() {
        assert!(PolicyEngine_Load(b"not json").is_err());
    }
}
