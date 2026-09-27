//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The public suffix list (ADR-0063 §4.10): the registrable domain (eTLD+1) behind the card's
//! domain row and the lookalike check.
//!
//! The list ships as data (`data/public_suffix_list.dat`, MPL-2.0, about 145 KB) and is matched
//! here with the algorithm from publicsuffix.org. The `psl` crate compiles the same list into
//! code, which costs about 800 KB in the Android library; this costs the list itself.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::LazyLock;

const LIST: &str = include_str!("../data/public_suffix_list.dat");

/// Every rule, in ASCII (IDN rules converted to punycode once, at first use), with its `*.` or
/// `!` prefix kept. ASCII rules borrow from the embedded list (the list is all lower case).
static RULES: LazyLock<HashSet<Cow<'static, str>>> = LazyLock::new(|| {
    LIST.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .filter_map(|rule| {
            if rule.is_ascii() {
                return Some(Cow::Borrowed(rule));
            }
            let (prefix, body) = if let Some(b) = rule.strip_prefix("*.") {
                ("*.", b)
            } else if let Some(b) = rule.strip_prefix('!') {
                ("!", b)
            } else {
                ("", rule)
            };
            let ascii = url::quirks::domain_to_ascii(body);
            (!ascii.is_empty()).then(|| Cow::Owned(format!("{prefix}{ascii}")))
        })
        .collect()
});

/// How many trailing labels of `host` form its public suffix. Unlisted TLDs follow the implicit
/// `*` rule (one label).
fn suffix_labels(labels: &[&str]) -> usize {
    let rules = &*RULES;
    let n = labels.len();
    let mut best = 1; // the default rule "*"
    for start in 0..n {
        let candidate = labels[start..].join(".");
        if rules.contains(format!("!{candidate}").as_str()) {
            // An exception rule wins outright; the suffix is the candidate minus its first label.
            return n - start - 1;
        }
        let exact = rules.contains(candidate.as_str());
        let wildcard = start + 1 < n
            && rules.contains(format!("*.{}", labels[start + 1..].join(".")).as_str());
        if exact || wildcard {
            best = best.max(n - start);
        }
    }
    best
}

/// eTLD+1 of an ASCII host, e.g. `login.apple.com.evil.cn` → `evil.cn`. `None` when the host is
/// itself a public suffix (or has an empty label).
pub(crate) fn registrable(host: &str) -> Option<String> {
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    if labels.iter().any(|l| l.is_empty()) {
        return None;
    }
    let suffix = suffix_labels(&labels);
    (labels.len() > suffix).then(|| labels[labels.len() - suffix - 1..].join("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publicsuffix_org_rules() {
        // Examples from the list's own test set (publicsuffix.org, checkPublicSuffix).
        let cases = [
            ("com", None),
            ("example.com", Some("example.com")),
            ("www.example.com", Some("example.com")),
            ("uk.com", None),
            ("example.uk.com", Some("example.uk.com")),
            ("b.example.uk.com", Some("example.uk.com")),
            ("c.kobe.jp", None), // *.kobe.jp
            ("b.c.kobe.jp", Some("b.c.kobe.jp")),
            ("city.kobe.jp", Some("city.kobe.jp")), // !city.kobe.jp
            ("www.city.kobe.jp", Some("city.kobe.jp")),
            ("ck", None),
            ("test.ck", None),
            ("b.test.ck", Some("b.test.ck")),
            ("www.ck", Some("www.ck")),
            ("www.www.ck", Some("www.ck")),
            ("test.ac", Some("test.ac")),
            ("xn--85x722f.com.cn", Some("xn--85x722f.com.cn")), // 食狮.com.cn
            ("xn--55qx5d.cn", None),                            // 公司.cn
            (
                "xn--85x722f.xn--55qx5d.cn",
                Some("xn--85x722f.xn--55qx5d.cn"),
            ),
            (
                "www.xn--85x722f.xn--55qx5d.cn",
                Some("xn--85x722f.xn--55qx5d.cn"),
            ),
            ("shishi.xn--55qx5d.cn", Some("shishi.xn--55qx5d.cn")),
            ("user.github.io", Some("user.github.io")), // private section
            ("github.io", None),
            ("login.apple.com.evil.cn", Some("evil.cn")),
            ("surl.amap.com", Some("amap.com")),
            ("tell.cc", Some("tell.cc")),
            ("a.b.unlisted-tld", Some("b.unlisted-tld")),
            ("a..b.com", None),
        ];
        for (host, want) in cases {
            assert_eq!(registrable(host).as_deref(), want, "{host}");
        }
    }

    /// One-off equivalence check against the `psl` crate this replaces (same list snapshot is not
    /// guaranteed, so this only runs while both exist): for every rule R, `x.R`, `y.x.R` and R.
    #[test]
    #[ignore = "needs the psl crate as a dev-dependency"]
    fn same_answers_as_the_psl_crate() {
        let mut checked = 0;
        let mut differ = Vec::new();
        for rule in LIST
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with("//"))
        {
            let base = rule.trim_start_matches('!').replace('*', "x");
            let ascii = url::quirks::domain_to_ascii(&base);
            for host in [ascii.clone(), format!("x.{ascii}"), format!("y.x.{ascii}")] {
                checked += 1;
                let ours = registrable(&host);
                let theirs = psl_crate_domain(&host);
                if ours != theirs {
                    differ.push(format!("{host}: ours {ours:?}, psl {theirs:?}"));
                }
            }
        }
        assert!(
            differ.is_empty(),
            "{checked} checked, {} differ:\n{}",
            differ.len(),
            differ[..differ.len().min(40)].join("\n")
        );
        eprintln!("{checked} hosts, identical");
    }

    fn psl_crate_domain(host: &str) -> Option<String> {
        psl::domain_str(host)
            .map(str::to_owned)
            .filter(|d| d != host.rsplit('.').next().unwrap_or(host))
    }
}
