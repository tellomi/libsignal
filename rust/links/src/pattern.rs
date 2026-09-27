//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Registry regular expressions (README §3, L5, L11).
//!
//! Patterns are written in the common subset of Rust `regex` and Python `re` so the draft lint and
//! this crate read them the same way; this crate is the one that runs them, so the rules are
//! re-checked here at load time rather than trusted from the builder.

use regex::{Regex, RegexBuilder};

/// A compiled pattern is capped well below `regex`'s default so a hostile update cannot make the
/// client build a huge automaton. Real registry patterns compile to a few KiB.
const PATTERN_SIZE_LIMIT: usize = 256 * 1024;

/// `^…$`, and the final `$` is not an escaped `\$`.
pub(crate) fn anchored_both_ends(rx: &str) -> bool {
    if !(rx.starts_with('^') && rx.ends_with('$')) || rx.len() < 2 {
        return false;
    }
    let head = &rx[..rx.len() - 1];
    let backslashes = head.len() - head.trim_end_matches('\\').len();
    backslashes.is_multiple_of(2)
}

/// `(?` may only open `(?:` or `(?P<`: inline flags such as `(?i)` would let `[a-z]` match U+212A
/// under case folding and make an apparently narrow class wide (L5 / L11).
fn unsupported_group(rx: &str) -> Option<String> {
    let b = rx.as_bytes();
    let (mut i, mut in_class) = (0, false);
    while i < b.len() {
        match b[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b']' if in_class => in_class = false,
            b'[' if !in_class => in_class = true,
            b'(' if !in_class
                && rx[i..].starts_with("(?")
                && !(rx[i..].starts_with("(?:") || rx[i..].starts_with("(?P<")) =>
            {
                return Some(rx[i..].chars().take(4).collect());
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Rust reads `^a|b$` as `(^a)|(b$)`; a full match reads it as `^(?:a|b)$`. Not allowed.
fn top_level_alternation(rx: &str) -> bool {
    let b = rx.as_bytes();
    let (mut i, mut depth, mut in_class) = (0usize, 0i32, false);
    while i < b.len() {
        match b[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b']' if in_class => in_class = false,
            b'[' if !in_class => in_class = true,
            b'(' if !in_class => depth += 1,
            b')' if !in_class => depth -= 1,
            b'|' if !in_class && depth == 0 => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// `\d \w \s \b \p{…}` and their negations: Python reads them as Unicode classes, and the client
/// builds `regex` without Unicode tables (they would cost ~800 KB on Android), so they are not in
/// the shared subset. `[0-9]`, `[A-Za-z0-9_]` say the same thing in both engines.
fn perl_class(rx: &str) -> Option<char> {
    let b = rx.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'\\' {
            let c = char::from(b[i + 1]);
            if "dDwWsSbBpP".contains(c) {
                return Some(c);
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    None
}

fn has_posix_class(rx: &str) -> bool {
    // `[:alpha:]` / `[:^alpha:]` anywhere: Python `re` does not have them.
    let b = rx.as_bytes();
    (0..b.len()).any(|i| {
        b[i] == b'[' && b.get(i + 1) == Some(&b':') && {
            let rest = &rx[i + 2..];
            let rest = rest.strip_prefix('^').unwrap_or(rest);
            let letters = rest.bytes().take_while(u8::is_ascii_lowercase).count();
            letters > 0 && rest[letters..].starts_with(":]")
        }
    })
}

/// L5, then compile. `anchored` patterns (`path` / `query` / `fragment`) must be `^…$` and are
/// compiled as a full match of their body; `title_strip` only needs one anchor (L18) and is
/// compiled as written.
pub(crate) fn compile(rx: &str, anchored: bool) -> Result<Regex, String> {
    if let Some(bad) = unsupported_group(rx) {
        return Err(format!("`(?` may only open `(?:` or `(?P<` (found {bad}…)"));
    }
    if let Some(c) = perl_class(rx) {
        return Err(format!(
            "\\{c} is a Perl / Unicode class, not in the shared subset: write the ASCII class"
        ));
    }
    if has_posix_class(rx) {
        return Err("POSIX character classes are not in the shared subset".into());
    }
    if top_level_alternation(rx) {
        return Err("top-level `|` means different things in the two engines".into());
    }
    let source = if anchored {
        if !anchored_both_ends(rx) {
            return Err("must be anchored ^…$".into());
        }
        format!("^(?:{})$", &rx[1..rx.len() - 1])
    } else {
        rx.to_owned()
    };
    RegexBuilder::new(&source)
        .size_limit(PATTERN_SIZE_LIMIT)
        .build()
        .map_err(|e| e.to_string())
}

/// `(?P<name>…)` → the text inside the group (only unnested groups need to be exact, which is what
/// L11 inspects).
pub(crate) fn group_patterns(rx: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut search = 0;
    while let Some(rel) = rx[search..].find("(?P<") {
        let name_start = search + rel + 4;
        let Some(name_len) = rx[name_start..].find('>') else {
            break;
        };
        let name = rx[name_start..name_start + name_len].to_owned();
        let body_start = name_start + name_len + 1;
        let b = rx.as_bytes();
        let (mut depth, mut i) = (1i32, body_start);
        while i < b.len() && depth > 0 {
            match b[i] {
                b'\\' => {
                    i += 2;
                    continue;
                }
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        // Unbalanced input never compiles; do not panic on it either.
        let body_end = if depth == 0 { i - 1 } else { rx.len() };
        out.push((name, rx[body_start..body_end].to_owned()));
        search = body_start;
    }
    out
}

/// L11: a capture that is substituted into an app scheme must be "an optional literal prefix, one
/// character class, a quantifier", and the class may only hold `A-Z`, `a-z`, `0-9`, `_` and `-`
/// (`[0-z]` would let in `: ? = @ [ \ ]`).
pub(crate) fn is_narrow_capture(pattern: &str) -> bool {
    let prefix_len = pattern
        .bytes()
        .take_while(u8::is_ascii_alphanumeric)
        .count();
    let rest = &pattern[prefix_len..];
    let Some(inner) = rest.strip_prefix('[') else {
        return false;
    };
    let Some(close) = inner.find(']') else {
        return false;
    };
    let (class, quantifier) = (&inner[..close], &inner[close + 1..]);
    let quantifier_ok = quantifier == "+"
        || quantifier == "*"
        || quantifier
            .strip_prefix('{')
            .and_then(|q| q.strip_suffix('}'))
            .is_some_and(|q| {
                let (a, b) = q.split_once(',').unwrap_or((q, ""));
                !a.is_empty()
                    && a.bytes().all(|c| c.is_ascii_digit())
                    && b.bytes().all(|c| c.is_ascii_digit())
            });
    quantifier_ok && !class.is_empty() && safe_class(class)
}

fn safe_class(body: &str) -> bool {
    const TOKENS: [&str; 5] = ["A-Z", "a-z", "0-9", "_", "\\-"];
    let mut i = 0;
    'outer: while i < body.len() {
        for tok in TOKENS {
            if body[i..].starts_with(tok) {
                i += tok.len();
                continue 'outer;
            }
        }
        if &body[i..] == "-" {
            return true; // a trailing `-` is literal
        }
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l5_shapes() {
        assert!(compile(r"^/video/(?P<bv>BV[0-9A-Za-z]{10})/?$", true).is_ok());
        assert!(compile(r"^/a|/b$", true).is_err());
        assert!(compile(r"^(?:/a|/b)$", true).is_ok());
        assert!(compile(r"^(?i)/a$", true).is_err());
        assert!(compile(r"^/a(?=b)$", true).is_err());
        assert!(compile(r"^/a\$", true).is_err());
        assert!(compile(r"/a$", true).is_err());
        assert!(compile(r"^[[:digit:]]$", true).is_err());
        assert!(compile(r"^/(a)\1$", true).is_err());
        // Perl / Unicode classes: Python reads them as Unicode classes, and the client's engine is
        // built without Unicode tables; write [0-9] / [A-Za-z0-9_] instead.
        for rx in [
            r"^/\d+$",
            r"^/\w+$",
            r"^/\s$",
            r"^/a\b$",
            r"^/\p{Han}$",
            r"^/[\d]$",
        ] {
            let err = compile(rx, true).expect_err(rx);
            assert!(err.contains("Perl / Unicode class"), "{rx}: {err}");
        }
        assert!(
            compile(r"^/\\d$", true).is_ok(),
            "an escaped backslash followed by d is a literal"
        );
        // Non-ASCII literals and negated ASCII classes still match any character.
        let han = compile(r"^/(?P<name>[^,]{1,64})$", true).unwrap();
        assert!(han.is_match("/同仁堂(五一路店)"));
        assert!(compile(r"_哔哩哔哩bilibili(_[^_]*)?$", false).is_ok());
        // The full-match wrapping matters: without it `^/a$` would still be fine, but a pattern
        // body is always matched against the whole path.
        let r = compile(r"^/marker$", true).unwrap();
        assert!(r.is_match("/marker") && !r.is_match("/marker/x"));
    }

    #[test]
    fn captures_and_l11() {
        let groups = group_patterns(r"^/(?P<cc>[a-z]{2})/app/[^/]+/id(?P<id>[0-9]{6,12})$");
        assert_eq!(
            groups,
            vec![
                ("cc".to_owned(), "[a-z]{2}".to_owned()),
                ("id".to_owned(), "[0-9]{6,12}".to_owned())
            ]
        );
        assert!(is_narrow_capture("[0-9]{6,12}"));
        assert!(is_narrow_capture("BV[0-9A-Za-z]{10}"));
        assert!(is_narrow_capture("[A-Za-z0-9_-]+"));
        assert!(!is_narrow_capture("[0-z]+"));
        assert!(!is_narrow_capture("[A-z]+"));
        assert!(!is_narrow_capture("[^/]+"));
        assert!(!is_narrow_capture(".+"));
        assert!(!is_narrow_capture("[a-z]+|x"));
    }
}
