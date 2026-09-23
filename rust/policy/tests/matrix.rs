//
// Copyright (C) 2026 Tellomi.
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The per-field test matrix from ADR-0062 §6.
//!
//! The matrix is split by field on purpose. A `username` reaches this engine only after
//! libsignal's `validate_nickname` has already rejected every non-`[_a-z0-9]` character, so
//! Unicode spoofing cases asserted against `Field::Username` would pass without testing anything.
//! They are asserted against `Field::DisplayName`, which really does receive arbitrary text.

use tellomi_policy::*;

/// A small lexicon in the shape the builder emits, so these tests exercise the real load path
/// (parse → validate → index) rather than a hand-built engine.
fn fixture() -> PolicyEngine {
    let json = serde_json::json!({
        "name": "policy",
        "version": 2026092201u64,
        "schema": 1,
        "generated_by": "tests/matrix.rs",
        "payload": {
            "rules": [
                rule("reserved:admin", "admin", ["NORMALIZED_EXACT", "PREFIX", "SUFFIX"],
                     ["username", "slug", "display_name", "group_name"], ["global"], "reserved"),
                rule("reserved:support", "support", ["NORMALIZED_EXACT"],
                     ["username", "slug", "display_name"], ["global"], "reserved"),
                rule("reserved:staff", "staff", ["NORMALIZED_EXACT"],
                     ["username", "slug"], ["global"], "reserved"),
                rule("impersonation:moderator", "moderator", ["NORMALIZED_EXACT", "PREFIX"],
                     ["username", "slug", "display_name"], ["global"], "impersonation"),
                rule("brand:tellomi", "tellomi", ["NORMALIZED_EXACT", "PREFIX", "SUFFIX", "CONFUSABLE"],
                     ["username", "slug", "display_name", "group_name"], ["global"], "brand_protected"),
                rule("impersonation:tellomi_support", "tellomi_support", ["NORMALIZED_EXACT", "CONFUSABLE"],
                     ["username", "slug", "display_name"], ["global"], "impersonation"),
                rule("security:verify", "verify_account", ["NORMALIZED_EXACT", "CONTAINS"],
                     ["display_name", "group_name"], ["global"], "security_sensitive"),
                // Regional: only applies when the caller passes Region::Cn.
                rule("cn:restricted_phrase", "敏感词示例", ["CONTAINS"],
                     ["display_name", "group_name"], ["cn"], "regional_restricted"),
            ],
            "allow": [
                {
                    "normalized": "admin",
                    "fields": ["group_name"],
                    "regions": ["global"],
                    "note": "a group may legitimately be called Admin; the username may not"
                }
            ]
        }
    });
    PolicyEngine::load(json.to_string().as_bytes()).expect("fixture lexicon loads")
}

fn rule(
    id: &str,
    term: &str,
    matches: impl IntoIterator<Item = &'static str>,
    fields: impl IntoIterator<Item = &'static str>,
    regions: impl IntoIterator<Item = &'static str>,
    outcome: &str,
) -> serde_json::Value {
    let normalized = normalize::normalize(term);
    let matches: Vec<&str> = matches.into_iter().collect();
    let skeleton = if matches.contains(&"CONFUSABLE") {
        normalize::skeleton(&normalized)
    } else {
        String::new()
    };
    serde_json::json!({
        "id": id,
        "term": term,
        "normalized": normalized,
        "skeleton": skeleton,
        "matches": matches,
        "fields": fields.into_iter().collect::<Vec<_>>(),
        "regions": regions.into_iter().collect::<Vec<_>>(),
        "outcome": outcome,
        "source": "tests/matrix.rs",
    })
}

const GLOBAL: &[Region] = &[Region::Global];
const CN: &[Region] = &[Region::Global, Region::Cn];

fn outcome(engine: &PolicyEngine, input: &str, field: Field, regions: &[Region]) -> Outcome {
    engine.check(input, field, regions).outcome
}

// ---------------------------------------------------------------------------------------------
// 1. Case and ASCII — username / slug
// ---------------------------------------------------------------------------------------------

#[test]
fn case_variants_are_reserved_for_usernames() {
    let e = fixture();
    for input in ["admin", "Admin", "ADMIN", "AdMiN"] {
        assert_eq!(
            outcome(&e, input, Field::Username, GLOBAL),
            Outcome::Reserved,
            "{input} should be reserved"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// 2. Normalization — display name (full width, zero width, combining, case folding)
// ---------------------------------------------------------------------------------------------

#[test]
fn fullwidth_is_reserved() {
    let e = fixture();
    assert_eq!(
        outcome(&e, "ＡＤＭＩＮ", Field::DisplayName, GLOBAL),
        Outcome::Reserved
    );
}

#[test]
fn zero_width_and_bidi_are_invalid_unicode() {
    let e = fixture();
    // The point of rejecting rather than stripping: "ad<ZWSP>min" is refused outright, so nobody
    // ends up with a name that renders as "admin" but is stored as something else.
    assert_eq!(
        outcome(&e, "ad\u{200B}min", Field::DisplayName, GLOBAL),
        Outcome::InvalidUnicode
    );
    assert_eq!(
        outcome(&e, "\u{202E}nimda", Field::DisplayName, GLOBAL),
        Outcome::InvalidUnicode
    );
    assert_eq!(
        outcome(&e, "\u{FEFF}hello", Field::DisplayName, GLOBAL),
        Outcome::InvalidUnicode
    );
}

#[test]
fn nbsp_and_ideographic_space_collapse_before_matching() {
    let e = fixture();
    // "tellomi␣support" normalizes to a single ASCII space, which the skeleton then drops,
    // so this is the impersonation rule and not an ordinary two-word name.
    assert_eq!(
        outcome(&e, "Tellomi\u{00A0}Support", Field::DisplayName, GLOBAL),
        Outcome::Impersonation
    );
}

// ---------------------------------------------------------------------------------------------
// 3. Confusables — ASCII lookalikes (username) vs UTS #39 skeleton (display name)
// ---------------------------------------------------------------------------------------------

#[test]
fn ascii_lookalikes_are_caught_for_usernames() {
    let e = fixture();
    for input in ["tel1omi", "te11omi", "tellorni", "t3llomi"] {
        assert_eq!(
            outcome(&e, input, Field::Username, GLOBAL),
            Outcome::BrandProtected,
            "{input} looks like the brand"
        );
    }
}

#[test]
fn cross_script_lookalikes_are_caught_for_display_names() {
    let e = fixture();
    // Cyrillic е in place of the Latin one. In a username this never reaches us: libsignal
    // rejects it as BadNicknameCharacter (see `libsignal_prefilter_would_reject`).
    let cyrillic = "t\u{0435}llomi";
    assert!(!cyrillic.is_ascii());
    assert_ne!(
        outcome(&e, cyrillic, Field::DisplayName, GLOBAL),
        Outcome::Allowed,
        "a cross-script lookalike of the brand must not be allowed"
    );
}

#[test]
fn libsignal_prefilter_would_reject_before_the_engine_is_consulted() {
    // Not an engine assertion: this documents *why* the cross-script cases are tested against
    // display names only. The character set below is what `validate_nickname` accepts.
    let cyrillic = "t\u{0435}llomi";
    assert!(!normalize::is_username_safe(cyrillic));
    assert!(!normalize::is_username_safe("Tellomi")); // upper case is lowercased earlier still
    assert!(normalize::is_username_safe("tellomi_support"));
}

// ---------------------------------------------------------------------------------------------
// 4. Semantics — exact vs prefix vs suffix vs contains (the false-positive guard)
// ---------------------------------------------------------------------------------------------

#[test]
fn prefix_matches_only_at_a_boundary() {
    let e = fixture();
    assert_eq!(
        outcome(&e, "admin", Field::Username, GLOBAL),
        Outcome::Reserved
    );
    assert_eq!(
        outcome(&e, "admin_x", Field::Username, GLOBAL),
        Outcome::Reserved
    );
    assert_eq!(
        outcome(&e, "admin_team", Field::Username, GLOBAL),
        Outcome::Reserved
    );
    // …and not in the middle of a longer word:
    assert_eq!(
        outcome(&e, "administer", Field::Username, GLOBAL),
        Outcome::Allowed
    );
    assert_eq!(
        outcome(&e, "administrator2", Field::Username, GLOBAL),
        Outcome::Allowed
    );
    assert_eq!(
        outcome(&e, "badminton", Field::Username, GLOBAL),
        Outcome::Allowed
    );
}

#[test]
fn suffix_matches_only_at_a_boundary() {
    let e = fixture();
    assert_eq!(
        outcome(&e, "team_admin", Field::Username, GLOBAL),
        Outcome::Reserved
    );
    assert_eq!(
        outcome(&e, "sysadmin", Field::Username, GLOBAL),
        Outcome::Allowed
    );
}

#[test]
fn contains_is_reserved_for_long_terms() {
    let e = fixture();
    assert_eq!(
        outcome(&e, "please verify_account now", Field::DisplayName, GLOBAL),
        Outcome::SecuritySensitive
    );
    // "admin" has no CONTAINS rule, so an ordinary sentence containing it is fine.
    assert_eq!(
        outcome(&e, "the badminton club", Field::DisplayName, GLOBAL),
        Outcome::Allowed
    );
}

// ---------------------------------------------------------------------------------------------
// 5. Brand and official-identity impersonation
// ---------------------------------------------------------------------------------------------

#[test]
fn brand_and_official_identity() {
    let e = fixture();
    assert_eq!(
        outcome(&e, "tellomi", Field::Username, GLOBAL),
        Outcome::BrandProtected
    );
    assert_eq!(
        outcome(&e, "tellomi_official", Field::Username, GLOBAL),
        Outcome::BrandProtected
    );
    assert_eq!(
        outcome(&e, "my_tellomi", Field::Username, GLOBAL),
        Outcome::BrandProtected
    );
    assert_eq!(
        outcome(&e, "tellomi_support", Field::Username, GLOBAL),
        Outcome::Impersonation
    );
    assert_eq!(
        outcome(&e, "support", Field::Username, GLOBAL),
        Outcome::Reserved
    );
    assert_eq!(
        outcome(&e, "staff", Field::Username, GLOBAL),
        Outcome::Reserved
    );
    assert_eq!(
        outcome(&e, "moderator_zh", Field::Username, GLOBAL),
        Outcome::Impersonation
    );
}

// ---------------------------------------------------------------------------------------------
// 6. Fields and regions are independent axes
// ---------------------------------------------------------------------------------------------

#[test]
fn a_rule_only_applies_to_its_own_fields() {
    let e = fixture();
    // "staff" is reserved as a username but says nothing about display names.
    assert_eq!(
        outcome(&e, "staff", Field::Username, GLOBAL),
        Outcome::Reserved
    );
    assert_eq!(
        outcome(&e, "staff", Field::DisplayName, GLOBAL),
        Outcome::Allowed
    );
}

#[test]
fn regional_rules_need_the_region() {
    let e = fixture();
    let input = "这是敏感词示例的一句话";
    assert_eq!(
        outcome(&e, input, Field::DisplayName, GLOBAL),
        Outcome::Allowed
    );
    assert_eq!(
        outcome(&e, input, Field::DisplayName, CN),
        Outcome::RegionalRestricted
    );
}

// ---------------------------------------------------------------------------------------------
// 7. Allowlist beats rules
// ---------------------------------------------------------------------------------------------

#[test]
fn allowlist_overrides_a_rule_for_the_listed_field_only() {
    let e = fixture();
    assert_eq!(
        outcome(&e, "Admin", Field::GroupName, GLOBAL),
        Outcome::Allowed
    );
    assert_eq!(
        outcome(&e, "Admin", Field::Username, GLOBAL),
        Outcome::Reserved
    );
}

// ---------------------------------------------------------------------------------------------
// 8. Internals stay internal
// ---------------------------------------------------------------------------------------------

#[test]
fn a_hit_carries_audit_detail_for_logs_only() {
    let e = fixture();
    let verdict = e.check("tellomi_support", Field::Username, GLOBAL);
    let hit = verdict.hit.expect("a refusal names its rule internally");
    assert_eq!(hit.rule_id, "impersonation:tellomi_support");
    assert_eq!(hit.source, "tests/matrix.rs");
    // The bridge layer exports `outcome` and nothing else; this test exists so that a future
    // change which starts leaking `hit` across the bridge has to delete an explicit assertion.
    assert_eq!(verdict.outcome, Outcome::Impersonation);
}

#[test]
fn an_allowed_verdict_has_no_hit() {
    let e = fixture();
    let verdict = e.check("xiaoming", Field::Username, GLOBAL);
    assert!(verdict.is_allowed());
    assert!(verdict.hit.is_none());
}

// ---------------------------------------------------------------------------------------------
// 9. Loading: schema range, wrong payload, malformed lexicon
// ---------------------------------------------------------------------------------------------

#[test]
fn refuses_a_future_schema_instead_of_guessing() {
    let json = serde_json::json!({
        "name": "policy", "version": 1u64, "schema": SCHEMA_MAX + 1,
        "payload": { "rules": [], "allow": [] }
    });
    assert!(matches!(
        PolicyEngine::load(json.to_string().as_bytes()),
        Err(LoadError::UnsupportedSchema { .. })
    ));
}

#[test]
fn refuses_another_registry() {
    let json = serde_json::json!({
        "name": "stickers", "version": 1u64, "schema": 1,
        "payload": { "rules": [], "allow": [] }
    });
    assert!(matches!(
        PolicyEngine::load(json.to_string().as_bytes()),
        Err(LoadError::WrongPayload(_))
    ));
}

#[test]
fn refuses_a_short_contains_term() {
    // The guard that keeps a big lexicon from shredding ordinary names: a 3-character CONTAINS
    // term would match inside thousands of legitimate usernames.
    let json = serde_json::json!({
        "name": "policy", "version": 1u64, "schema": 1,
        "payload": {
            "rules": [ rule("bad:short", "adm", ["CONTAINS"], ["username"], ["global"], "reserved") ],
            "allow": []
        }
    });
    let err = PolicyEngine::load(json.to_string().as_bytes()).unwrap_err();
    assert!(
        matches!(err, LoadError::Invalid(ref m) if m.contains("CONTAINS")),
        "{err}"
    );
}

#[test]
fn refuses_a_rule_that_claims_to_allow() {
    let json = serde_json::json!({
        "name": "policy", "version": 1u64, "schema": 1,
        "payload": {
            "rules": [ rule("bad:allow", "hello", ["NORMALIZED_EXACT"], ["username"], ["global"], "allowed") ],
            "allow": []
        }
    });
    assert!(matches!(
        PolicyEngine::load(json.to_string().as_bytes()),
        Err(LoadError::Invalid(_))
    ));
}

// ---------------------------------------------------------------------------------------------
// 10. The guard that matters most: ordinary names must survive
// ---------------------------------------------------------------------------------------------

/// Names drawn from the shapes real users pick: English given names, pinyin, common words,
/// handles with digits and underscores. If a lexicon change starts refusing any of these, this
/// test fails before the change reaches anyone.
const ORDINARY_USERNAMES: &[&str] = &[
    "alice",
    "bob",
    "carol",
    "dave",
    "erin",
    "frank",
    "grace",
    "heidi",
    "ivan",
    "judy",
    "xiaoming",
    "lihua",
    "wangwei",
    "zhangsan",
    "lisi",
    "chenchen",
    "liuyang",
    "zhaolei",
    "sunny_day",
    "night_owl",
    "coffee_lover",
    "bookworm",
    "traveler",
    "photographer",
    "dev_ops",
    "frontend",
    "backend",
    "designer",
    "musician",
    "runner42",
    "chess_master",
    "hello_world",
    "just_me",
    "the_real_bob",
    "mountain_cat",
    "blue_sky",
    "river_stone",
    "badminton_club",
    "administrative_law",
    "administrator_of_nothing_2",
    "support_group_for_cats",
    "supporter",
    "supportive",
    "staffordshire",
    "moderate",
    "telegram_fan",
    "telephone",
    "television",
    "telling_stories",
    "tell_me_more",
    "a_b_c",
    "x1",
    "zz9",
    "user_2026",
    "player_one",
    "green_tea",
    "red_panda",
];

#[test]
fn ordinary_usernames_are_not_caught() {
    let e = fixture();
    let refused: Vec<_> = ORDINARY_USERNAMES
        .iter()
        .filter(|n| !e.check(n, Field::Username, CN).is_allowed())
        .collect();
    assert!(
        refused.is_empty(),
        "false positives on ordinary usernames: {refused:?}"
    );
}

#[test]
fn ordinary_display_names_are_not_caught() {
    let e = fixture();
    let names = [
        "小明",
        "张三",
        "李华",
        "王伟",
        "陈晨",
        "刘洋",
        "赵磊",
        "Alice Chen",
        "Bob 李",
        "咖啡爱好者",
        "夜猫子",
        "摄影师小王",
        "北京天气",
        "周末爬山群",
        "羽毛球俱乐部",
        "读书会",
        "前端开发交流",
    ];
    let refused: Vec<_> = names
        .iter()
        .filter(|n| !e.check(n, Field::DisplayName, CN).is_allowed())
        .collect();
    assert!(
        refused.is_empty(),
        "false positives on ordinary display names: {refused:?}"
    );
}

/// Names that the fixture *should* refuse, so that "nothing is refused" can never pass as success.
#[test]
fn the_guard_above_is_not_vacuous() {
    let e = fixture();
    let must_refuse = [
        "admin",
        // A PREFIX rule fires at any boundary, so `admin_…` is refused however long the rest is.
        // That is the intended reading of "admin_x is impersonation" in ADR-0062 §5 — recorded
        // here because it is the one place where the rule is deliberately broad.
        "admin_is_not_me_but_i_am_fine",
        "tellomi",
        "tel1omi",
        "support",
        "tellomi_support",
    ];
    for name in must_refuse {
        assert!(
            !e.check(name, Field::Username, CN).is_allowed(),
            "{name} must be refused, otherwise the false-positive test proves nothing"
        );
    }
}
