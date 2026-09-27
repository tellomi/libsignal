//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Lookalike domains (ADR-0063 §6.1, card-visual §7.3 layer 1).
//!
//! Signal's "ASCII and non-ASCII may not mix" rule stops `аррӏе.com` written in Unicode, but not
//! its punycode spelling, nor ASCII lookalikes such as `bi1ibili.com`. Here the registrable domain
//! is reduced to its UTS #39 skeleton and compared with the skeletons of well-known domains (every
//! provider host in the registry plus the registry's `popular_domains`). Same skeleton, different
//! domain → the card drops to a plain link, the domain is flagged, and the client warns once before
//! opening. The check is local on both sides and never sends the URL anywhere.

use std::collections::HashMap;

use crate::urlx::registrable_domain;

/// `fold(lower(skeleton(lower(unicode form))))`.
///
/// Case is folded on both sides of the skeleton because UTS #39 maps some lookalikes to capitals
/// (`0` → `O`) and host names are case-insensitive anyway. UTS #39 alone does not catch the
/// ADR's own example: it maps the Cyrillic palochka in `аррӏе` to `i`, not `l`. So the vertical
/// strokes `i l 1 | !` fold together and `0` folds to `o` — the same within-ASCII folding the
/// policy engine applies to names, minus its separator stripping (a `-` or `.` changes a domain).
pub(crate) fn domain_skeleton(ascii_domain: &str) -> String {
    let unicode = url::quirks::domain_to_unicode(ascii_domain);
    let base = if unicode.is_empty() {
        ascii_domain.to_owned()
    } else {
        unicode
    };
    let lowered = base.to_lowercase();
    unicode_security::skeleton(&lowered)
        .collect::<String>()
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'i' | '1' | '|' | '!' => 'l',
            '0' => 'o',
            other => other,
        })
        .collect()
}

/// Skeleton → the well-known registrable domain it belongs to.
#[derive(Debug, Default)]
pub(crate) struct KnownDomains {
    by_skeleton: HashMap<String, String>,
    exact: std::collections::HashSet<String>,
}

impl KnownDomains {
    pub fn insert(&mut self, host: &str) {
        let host = host.trim_start_matches("*.").to_ascii_lowercase();
        let domain = registrable_domain(&host).unwrap_or(host);
        self.by_skeleton
            .entry(domain_skeleton(&domain))
            .or_insert_with(|| domain.clone());
        self.exact.insert(domain);
    }

    /// The well-known domain `host` imitates, if any.
    pub fn lookalike_of(&self, host: &str) -> Option<&str> {
        let domain = registrable_domain(host)?;
        if self.exact.contains(&domain) {
            return None;
        }
        self.by_skeleton
            .get(&domain_skeleton(&domain))
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> KnownDomains {
        let mut k = KnownDomains::default();
        for h in [
            "www.bilibili.com",
            "apple.com",
            "tell.cc",
            "tellomi.app",
            "*.amap.com",
        ] {
            k.insert(h);
        }
        k
    }

    #[test]
    fn catches_lookalikes() {
        let k = known();
        // Cyrillic аррӏе.com, in the punycode form that passes Signal's mixed-script rule.
        let cyrillic = url::Url::parse("https://аррӏе.com/").unwrap();
        let host = cyrillic.host_str().unwrap().to_owned();
        assert!(host.starts_with("xn--"), "{host}");
        assert_eq!(k.lookalike_of(&host), Some("apple.com"));
        assert_eq!(k.lookalike_of("bi1ibili.com"), Some("bilibili.com"));
        assert_eq!(k.lookalike_of("www.bi1ibili.com"), Some("bilibili.com"));
        assert_eq!(k.lookalike_of("te11.cc"), Some("tell.cc"));
        assert_eq!(k.lookalike_of("tellorni.app"), Some("tellomi.app"));
        assert_eq!(k.lookalike_of("app1e.com"), Some("apple.com"));
        assert_eq!(k.lookalike_of("b1l1b1l1.com"), Some("bilibili.com"));
    }

    #[test]
    fn leaves_real_and_unrelated_domains_alone() {
        let k = known();
        assert_eq!(k.lookalike_of("www.bilibili.com"), None);
        assert_eq!(k.lookalike_of("m.bilibili.com"), None);
        assert_eq!(k.lookalike_of("uri.amap.com"), None);
        assert_eq!(k.lookalike_of("github.com"), None);
        // Word changes and hyphens are out of reach of a skeleton (§6.1 residual risk).
        assert_eq!(k.lookalike_of("apple-support.com"), None);
    }
}
