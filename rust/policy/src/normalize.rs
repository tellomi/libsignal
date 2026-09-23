//
// Copyright (C) 2026 Tellomi.
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Normalization pipeline (ADR-0062 §4.2).
//!
//! Fixed order, identical on every client and on the server, and identical at build time
//! (the lexicon builder normalizes terms with these same functions):
//!
//! 1. reject / strip format & control characters (Cc, Cf) — zero width, BOM, soft hyphen, bidi marks
//! 2. NFKC (UAX #15) — folds full width, compatibility forms, composes combining marks
//! 3. case fold — not `to_lowercase`: ß→ss, İ→i̇
//! 4. collapse whitespace — every Unicode space becomes U+0020, runs collapse, ends trimmed
//! 5. (only for `CONFUSABLE`) skeleton (UTS #39) — maps confusable characters to a prototype
//!
//! Note the asymmetry deliberately documented in the ADR: a `username` never reaches step 2+
//! carrying anything but ASCII, because libsignal's `validate_nickname` rejects everything else
//! long before the engine is called. The full pipeline exists for display names, group names,
//! descriptions and bios.

use unicode_normalization::UnicodeNormalization;
use unicode_security::{
    GeneralSecurityProfile, MixedScript, RestrictionLevel, RestrictionLevelDetection,
};

/// Characters that must never appear in a public identity field, whatever the field.
///
/// `Cf` (format) covers zero width space/joiner, BOM, soft hyphen and the bidi overrides used to
/// visually reverse text; `Cc` (control) covers the C0/C1 ranges. We reject rather than strip so
/// that a name cannot silently become a different name than the one the user typed.
pub fn has_disallowed_control(input: &str) -> bool {
    input.chars().any(|c| {
        matches!(c, '\u{0}'..='\u{1F}' | '\u{7F}'..='\u{9F}')  // Cc
            || matches!(c, '\u{AD}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
            || ('\u{E0000}'..='\u{E007F}').contains(&c) // tag characters
    })
}

/// True when the string mixes scripts in a way UTS #39 considers suspicious
/// (e.g. Latin + Cyrillic in one label). Pure single-script strings and the
/// common Latin+Han / Latin+Hiragana combinations are fine.
pub fn is_mixed_script(input: &str) -> bool {
    // Per word, not per string: `detect_restriction_level` degrades to `Unrestricted` as soon as
    // the text contains a space (a space is not an identifier character), so "Bob 李" would look
    // as suspicious as "tеllomi" if we asked about the whole string. Splitting first gives the
    // answer people expect — "Bob" and "李" are each fine, while "tеllomi" is not.
    input.split(' ').any(|word| {
        !word.is_empty()
            && !word.is_single_script()
            && !word.check_restriction_level(RestrictionLevel::ModeratelyRestrictive)
    })
}

/// Steps 2–4: NFKC → case fold → whitespace collapse.
pub fn normalize(input: &str) -> String {
    let folded: String = input
        .nfkc()
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
        // `to_lowercase` handles İ→i̇ but not ß→ss; do the one special case that matters for
        // brand protection explicitly rather than pulling in a full case-folding table.
        .replace('\u{00DF}', "ss");

    let mut out = String::with_capacity(folded.len());
    let mut pending_space = false;
    for c in folded.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    out
}

/// Step 5: UTS #39 skeleton over the already-normalized string.
///
/// Two strings whose skeletons are equal look the same to a reader: `tеllomi` (Cyrillic е) and
/// `tellomi` share a skeleton, and so do `paypal` and `paypaI`.
pub fn skeleton(normalized: &str) -> String {
    let uts39: String = unicode_security::skeleton(normalized).collect();
    // UTS #39 covers cross-script confusables but leaves some within-ASCII lookalikes alone,
    // and those are exactly the ones available inside a Signal username's `[_a-z0-9]` alphabet.
    // Fold them here so `tel1omi` and `tellomi` collide for CONFUSABLE rules.
    let mut out = String::with_capacity(uts39.len());
    let mut chars = uts39.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '1' | 'l' | '|' | 'i' | '!' => out.push('l'),
            '0' => out.push('o'),
            '5' => out.push('s'),
            '$' => out.push('s'),
            '3' => out.push('e'),
            '4' => out.push('a'),
            '@' => out.push('a'),
            '7' => out.push('t'),
            'r' if chars.peek() == Some(&'n') => {
                chars.next();
                out.push('m');
            }
            '_' | '-' | '.' | ' ' => {} // separators carry no visual identity
            other => out.push(other),
        }
    }
    out
}

/// True when every character is allowed in a Signal username nickname
/// (`libsignal/rust/usernames`: `[_a-z0-9]` after ASCII lowercasing).
///
/// Used by the builder to decide whether a term can ever match the `username` field at all,
/// so the hash denylist does not waste entries on terms a username can never contain.
pub fn is_username_safe(term: &str) -> bool {
    !term.is_empty()
        && term
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// True when the string contains at least one character that is not allowed in identifiers
/// per UTS #39's identifier status (deprecated, obsolete or otherwise not recommended).
pub fn has_non_identifier_char(input: &str) -> bool {
    input
        .chars()
        .any(|c| !c.identifier_allowed() && !c.is_whitespace() && !c.is_ascii_punctuation())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfkc_folds_fullwidth_and_case() {
        assert_eq!(normalize("ＡＤＭＩＮ"), "admin");
        assert_eq!(normalize("Admin"), "admin");
        assert_eq!(normalize("ADMIN"), "admin");
        // combining acute composes, then lowercases
        assert_eq!(normalize("adm\u{0301}in"), "adḿin");
    }

    #[test]
    fn eszett_and_dotted_i() {
        assert_eq!(normalize("straße"), "strasse");
        assert_eq!(normalize("İstanbul"), "i\u{307}stanbul");
    }

    #[test]
    fn whitespace_collapses() {
        assert_eq!(
            normalize("  Tellomi\u{00A0}\u{3000}Support  "),
            "tellomi support"
        );
    }

    #[test]
    fn control_and_format_rejected() {
        assert!(has_disallowed_control("ad\u{200B}min"));
        assert!(has_disallowed_control("\u{FEFF}admin"));
        assert!(has_disallowed_control("ad\u{202E}min"));
        assert!(!has_disallowed_control("admin"));
        assert!(!has_disallowed_control("小明"));
    }

    #[test]
    fn skeleton_collides_lookalikes() {
        assert_eq!(
            skeleton(&normalize("tel1omi")),
            skeleton(&normalize("tellomi"))
        );
        assert_eq!(
            skeleton(&normalize("te11omi")),
            skeleton(&normalize("tellomi"))
        );
        assert_eq!(
            skeleton(&normalize("tellorni")),
            skeleton(&normalize("tellomi"))
        );
        assert_eq!(
            skeleton(&normalize("t3llomi")),
            skeleton(&normalize("tellomi"))
        );
        assert_eq!(
            skeleton(&normalize("tellomi_")),
            skeleton(&normalize("tellomi"))
        );
        // Cyrillic е
        assert_eq!(
            skeleton(&normalize("t\u{0435}llomi")),
            skeleton(&normalize("tellomi"))
        );
    }

    #[test]
    fn skeleton_keeps_different_words_apart() {
        assert_ne!(
            skeleton(&normalize("telegram")),
            skeleton(&normalize("tellomi"))
        );
        assert_ne!(
            skeleton(&normalize("hello")),
            skeleton(&normalize("tellomi"))
        );
    }

    #[test]
    fn mixed_script_detected() {
        assert!(is_mixed_script("t\u{0435}llomi")); // Latin + Cyrillic inside one word
        assert!(!is_mixed_script("tellomi"));
        assert!(!is_mixed_script("小明"));
        // Names that mix scripts the way real people write them stay allowed:
        assert!(!is_mixed_script("bob 李"));
        assert!(!is_mixed_script("bob李"));
        assert!(!is_mixed_script("alice chen"));
        assert!(!is_mixed_script("咖啡爱好者"));
    }

    #[test]
    fn username_safety() {
        assert!(is_username_safe("admin"));
        assert!(is_username_safe("tellomi_support"));
        assert!(!is_username_safe("Admin"));
        assert!(!is_username_safe("小明"));
        assert!(!is_username_safe("tellomi-support"));
    }
}
