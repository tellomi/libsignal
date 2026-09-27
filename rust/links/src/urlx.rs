//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! URL plumbing shared by the sender and the receiver: parsing, Signal's "may this URL have a
//! preview" rule, encodings, registrable domains, and "does this URL appear in the message body".

use std::net::IpAddr;

use unicode_security::MixedScript;
use url::{Host, Url};

/// Parse an absolute `http(s)` URL. Anything else — `intent:`, `javascript:`, relative
/// references — is `None`.
pub(crate) fn parse_web_url(raw: &str) -> Option<Url> {
    let url = Url::parse(raw.trim()).ok()?;
    (matches!(url.scheme(), "http" | "https") && url.host_str().is_some()).then_some(url)
}

pub(crate) fn host_str(url: &Url) -> Option<String> {
    match url.host()? {
        Host::Domain(d) => Some(d.to_ascii_lowercase()),
        Host::Ipv4(ip) => Some(ip.to_string()),
        Host::Ipv6(ip) => Some(format!("[{ip}]")),
    }
}

const INVALID_DOMAIN_SUFFIXES: &[&str] = &[
    "example",
    "example.com",
    "example.net",
    "example.org",
    "i2p",
    "invalid",
    "localhost",
    "onion",
    "test",
];

fn is_invalid_domain(domain: &str) -> bool {
    let d = domain.strip_suffix('.').unwrap_or(domain);
    INVALID_DOMAIN_SUFFIXES
        .iter()
        .any(|s| d == *s || d.ends_with(&format!(".{s}")))
}

fn is_private_or_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_unspecified()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_private()
                || v4.is_multicast()
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_or_local(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || (first & 0xffc0) == 0xfe80 // link-local
                || (first & 0xfe00) == 0xfc00 // unique local fc00::/7
        }
    }
}

/// Signal's preview-URL rule (Android `LinkUtil.isValidPreviewUrl`, Desktop `shouldPreviewHref` /
/// `isLinkSneaky`), applied on both sides so they agree: https only; no bidi overrides or box
/// drawing characters; no `..` / `…` in the authority; the authority is all-ASCII or all-non-ASCII
/// (dots aside); not a reserved or special-use domain; not a private or local IP literal.
pub fn is_valid_preview_url(raw: &str) -> bool {
    let raw = raw.trim();
    if raw.chars().any(|c| {
        matches!(c, '\u{202C}' | '\u{202D}' | '\u{202E}') || ('\u{2500}'..='\u{25FF}').contains(&c)
    }) {
        return false;
    }
    let after_scheme = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"))
        .unwrap_or(raw);
    let authority = after_scheme.split('/').next().unwrap_or("");
    if authority.is_empty() || authority.contains("..") || authority.contains('…') {
        return false;
    }
    let cleaned: String = authority.chars().filter(|c| *c != '.').collect();
    if !(cleaned.is_ascii() || cleaned.chars().all(|c| !c.is_ascii())) {
        return false;
    }
    let Some(url) = parse_web_url(raw) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    match url.host() {
        Some(Host::Domain(d)) => !is_invalid_domain(&d.to_ascii_lowercase()),
        Some(Host::Ipv4(ip)) => !is_private_or_local(IpAddr::V4(ip)),
        Some(Host::Ipv6(ip)) => !is_private_or_local(IpAddr::V6(ip)),
        None => false,
    }
}

/// `application/x-www-form-urlencoded` the way the registry's corpus spells canonical URLs
/// (Python `urlencode(…, safe=",()")`): unreserved characters and `,()` stay, space is `+`.
pub(crate) fn form_encode(s: &str, out: &mut String) {
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'_'
            | b'.'
            | b'-'
            | b'~'
            | b','
            | b'('
            | b')' => out.push(char::from(b)),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
}

/// Percent-encode everything but RFC 3986 unreserved characters: what goes into a URL template or
/// an app scheme when a capture is substituted (README §2 `open.scheme`, L11).
pub(crate) fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(char::from(b))
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Percent-decode (not `+`), lossy on invalid UTF-8.
pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// eTLD+1 by the public suffix list, e.g. `login.apple.com.evil.cn` → `evil.cn`. `None` for IP
/// literals and for hosts that are themselves a public suffix.
pub fn registrable_domain(host: &str) -> Option<String> {
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.starts_with('[') || host.parse::<IpAddr>().is_ok() {
        return None;
    }
    let domain = psl::domain_str(host)?;
    (domain != host.rsplit('.').next().unwrap_or(host)).then(|| domain.to_owned())
}

/// Unicode form of an ASCII (punycode) host, when showing it is safe: every label is written in
/// one script and does not mix ASCII with non-ASCII (card-visual §3.4). Otherwise the ASCII form.
pub(crate) fn display_host(ascii_host: &str) -> String {
    if !ascii_host.split('.').any(|l| l.starts_with("xn--")) {
        return ascii_host.to_owned();
    }
    let unicode = url::quirks::domain_to_unicode(ascii_host);
    let safe = !unicode.is_empty()
        && unicode.split('.').all(|label| {
            label.is_ascii()
                || (label.chars().all(|c| !c.is_ascii() || c == '-') && label.is_single_script())
        });
    if safe { unicode } else { ascii_host.to_owned() }
}

/// The domain row of a card: the registrable domain, computed by the receiver from the URL in the
/// message (§4.1 item 3, card-visual §3.4).
pub fn display_domain(url: &str) -> Option<String> {
    let url = parse_web_url(url)?;
    let host = host_str(&url)?;
    let domain = registrable_domain(&host).unwrap_or(host);
    Some(display_host(&domain))
}

/// Does `url` appear in `body` as a link of its own (§4.1 item 3, §5.3)?
///
/// Signal's clients disagree on this today (Desktop: exact entry of the linkified list; iOS: raw
/// substring; Android: ignoring a trailing `/`). The single rule here is the substring match made
/// safe: the occurrence must start at a boundary and must not be the prefix of a longer URL —
/// otherwise `https://tellomi.app` would "appear" inside `https://tellomi.app.evil.cn/login`.
/// Trailing sentence punctuation is allowed, and so is Android's trailing-slash leniency.
pub fn url_appears_in_body(url: &str, body: &str) -> bool {
    let url = url.trim();
    if url.is_empty() {
        return false;
    }
    let bare = url
        .strip_suffix('/')
        .filter(|b| !b.ends_with('/') && !b.ends_with(':'));
    let needle = bare.unwrap_or(url);
    body.match_indices(needle).any(|(at, _)| {
        let before_ok = body[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || "-._~:/?#@$&*+=%".contains(c)));
        let rest = &body[at + needle.len()..];
        // One trailing slash may be on either side (Android compares without it).
        before_ok && (ends_link(rest) || rest.strip_prefix('/').is_some_and(ends_link))
    })
}

fn ends_link(rest: &str) -> bool {
    const PUNCTUATION: &str = ".,;:!?)]}'\"";
    let boundary = |c: char| c.is_whitespace() || !c.is_ascii() || "<>".contains(c);
    match rest.chars().next() {
        None => true,
        Some(c) if boundary(c) || c == '"' => true,
        // Sentence punctuation is not part of the link, as long as nothing URL-like follows it.
        Some(c) if PUNCTUATION.contains(c) => rest
            .trim_start_matches(|c: char| PUNCTUATION.contains(c))
            .chars()
            .next()
            .is_none_or(boundary),
        Some(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_url_rule_matches_signal() {
        assert!(is_valid_preview_url(
            "https://www.bilibili.com/video/BV1YDhJ6ZEL6"
        ));
        assert!(is_valid_preview_url("https://tell.cc/u#u/hk881qb.01"));
        assert!(!is_valid_preview_url("http://www.bilibili.com/"));
        assert!(!is_valid_preview_url("https://evil.example.com/"));
        assert!(!is_valid_preview_url("https://a.test/"));
        assert!(!is_valid_preview_url("https://localhost/"));
        assert!(!is_valid_preview_url("https://192.168.1.1/"));
        assert!(!is_valid_preview_url("https://[::1]/"));
        assert!(is_valid_preview_url("https://1.1.1.1/"));
        assert!(!is_valid_preview_url("https://a..b.com/"));
        // Mixed ASCII and non-ASCII in the authority: Signal's rule.
        assert!(!is_valid_preview_url("https://аррӏе.com/"));
        // All-ASCII punycode passes this rule; the lookalike check is what catches it.
        assert!(is_valid_preview_url("https://xn--80ak6aa92e.com/"));
        assert!(!is_valid_preview_url("https://evil.cn/\u{202E}gpj.exe"));
        assert!(!is_valid_preview_url("javascript:alert(1)"));
        assert!(!is_valid_preview_url(
            "intent://scan/#Intent;scheme=zxing;end"
        ));
    }

    #[test]
    fn registrable_domains() {
        assert_eq!(
            registrable_domain("login.apple.com.evil.cn").as_deref(),
            Some("evil.cn")
        );
        assert_eq!(
            registrable_domain("surl.amap.com").as_deref(),
            Some("amap.com")
        );
        assert_eq!(registrable_domain("tell.cc").as_deref(), Some("tell.cc"));
        assert_eq!(
            registrable_domain("www.bbc.co.uk").as_deref(),
            Some("bbc.co.uk")
        );
        assert_eq!(registrable_domain("co.uk"), None);
        assert_eq!(registrable_domain("1.1.1.1"), None);
        assert_eq!(
            display_domain("https://surl.amap.com/x").as_deref(),
            Some("amap.com")
        );
        assert_eq!(
            display_domain("https://login.apple.com.evil.cn/").as_deref(),
            Some("evil.cn")
        );
    }

    #[test]
    fn appears_in_body() {
        let u = "https://tellomi.app";
        assert!(url_appears_in_body(u, "https://tellomi.app"));
        assert!(url_appears_in_body(u, "看这个 https://tellomi.app 吧"));
        assert!(url_appears_in_body(u, "看这个https://tellomi.app吧"));
        assert!(url_appears_in_body(u, "Visit https://tellomi.app."));
        assert!(url_appears_in_body(u, "(https://tellomi.app)"));
        assert!(!url_appears_in_body(u, "https://tellomi.app.evil.cn/login"));
        assert!(!url_appears_in_body(u, "https://tellomi.app@evil.cn/"));
        assert!(!url_appears_in_body(
            u,
            "https://x.cn/?to=https://tellomi.app"
        ));
        assert!(!url_appears_in_body(u, "https://tellomi.apps/"));
        let v = "https://tell.cc/kefu";
        assert!(!url_appears_in_body(v, "https://tell.cc/kefu.99"));
        assert!(url_appears_in_body(v, "https://tell.cc/kefu/"));
        assert!(url_appears_in_body(
            "https://tell.cc/kefu/",
            "https://tell.cc/kefu"
        ));
        assert!(url_appears_in_body(
            "https://www.bilibili.com/video/BV1YDhJ6ZEL6",
            "【视频】https://www.bilibili.com/video/BV1YDhJ6ZEL6，快看"
        ));
        assert!(!url_appears_in_body(u, "nothing here"));
    }

    #[test]
    fn encodings() {
        let mut s = String::new();
        form_encode("116.47,39.99 (x)*~", &mut s);
        assert_eq!(s, "116.47,39.99+(x)%2A~");
        assert_eq!(encode_component("a b/c"), "a%20b%2Fc");
        assert_eq!(percent_decode("%E5%90%8C%2C%zz"), "同,%zz");
    }
}
