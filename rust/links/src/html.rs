//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The lightweight HTML reading the extractor needs (ADR-0063 §4.4, §4.10: no html5ever): `<meta>`
//! (OG, `description`, charset), `<title>`, `<link rel=…icon…>`, and the text of
//! `<script type="application/ld+json">`. One forward pass, no backtracking, never panics on
//! malformed input; everything else in the document is skipped.

/// Declared icon size, from `sizes="WxH"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Icon {
    pub href: String,
    pub touch: bool,
    pub size: Option<(u32, u32)>,
}

#[derive(Debug, Default)]
pub(crate) struct Page {
    /// `(property-or-name, content)`, keys lower-cased, in document order.
    pub meta: Vec<(String, String)>,
    pub title: Option<String>,
    pub json_ld: Vec<String>,
    pub icons: Vec<Icon>,
    /// From `<meta charset>` or `<meta http-equiv="content-type">`.
    pub charset: Option<String>,
}

impl Page {
    /// First value for a meta key (`og:title`, `description`, …).
    pub fn meta(&self, key: &str) -> Option<&str> {
        self.meta
            .iter()
            .find(|(k, v)| k == key && !v.trim().is_empty())
            .map(|(_, v)| v.as_str())
    }
}

const MAX_JSON_LD_BLOCKS: usize = 16;

fn find_ci(haystack: &str, from: usize, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || from >= h.len() {
        return None;
    }
    let mut i = from;
    while i + n.len() <= h.len() {
        let rel = haystack[i..].find('<')?;
        i += rel;
        if i + n.len() <= h.len() && h[i..i + n.len()].eq_ignore_ascii_case(n) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Tag name at `pos` (just after `<`), lower-cased, and the index after it.
fn tag_name(s: &str, pos: usize) -> (String, usize) {
    let bytes = s.as_bytes();
    let mut end = pos;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'-') {
        end += 1;
    }
    (s[pos..end].to_ascii_lowercase(), end)
}

/// Attributes up to the closing `>`; returns them and the index after `>`.
fn attributes(s: &str, mut i: usize) -> (Vec<(String, String)>, usize) {
    let bytes = s.as_bytes();
    let mut attrs = Vec::new();
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= bytes.len() {
            return (attrs, i);
        }
        if bytes[i] == b'>' {
            return (attrs, i + 1);
        }
        let start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && !matches!(bytes[i], b'=' | b'>' | b'/')
        {
            i += 1;
        }
        let name = s[start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && matches!(bytes[i], b'"' | b'\'') {
                let quote = bytes[i];
                let vstart = i + 1;
                let vend = s[vstart..]
                    .bytes()
                    .position(|b| b == quote)
                    .map_or(bytes.len(), |p| vstart + p);
                value = decode_entities(&s[vstart..vend]);
                i = (vend + 1).min(bytes.len());
            } else {
                let vstart = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                value = decode_entities(&s[vstart..i]);
            }
        }
        if !name.is_empty() {
            attrs.push((name, value));
        }
        if i == start {
            i += 1; // stray byte: make progress
        }
    }
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn charset_from_content_type(value: &str) -> Option<String> {
    value.split(';').find_map(|part| {
        let (k, v) = part.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| v.trim().trim_matches(['"', '\'']).to_ascii_lowercase())
    })
}

pub(crate) fn content_type_charset(content_type: &str) -> Option<String> {
    charset_from_content_type(content_type)
}

fn parse_sizes(sizes: &str) -> Option<(u32, u32)> {
    sizes
        .split_ascii_whitespace()
        .filter_map(|one| {
            let (w, h) = one.split_once(['x', 'X'])?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .max_by_key(|(w, h): &(u32, u32)| (*w).min(*h))
}

/// Minimal character-reference decoding: the named references that show up in titles, and
/// numeric ones.
pub(crate) fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest[1..].find(';').map(|p| p + 1).filter(|p| *p <= 12);
        let decoded = end.and_then(|end| {
            let name = &rest[1..end];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{A0}'),
                _ => {
                    let n = if let Some(hex) = name.strip_prefix("#x").or(name.strip_prefix("#X")) {
                        u32::from_str_radix(hex, 16).ok()
                    } else if let Some(dec) = name.strip_prefix('#') {
                        dec.parse().ok()
                    } else {
                        None
                    };
                    n.and_then(char::from_u32).filter(|c| *c != '\0')
                }
            };
            c.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Collapse runs of whitespace and trim.
pub(crate) fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn scan(html: &str) -> Page {
    let mut page = Page::default();
    let bytes = html.as_bytes();
    let mut i = 0;
    while let Some(rel) = html.get(i..).and_then(|s| s.find('<')) {
        i += rel + 1;
        if html[i..].starts_with("!--") {
            i = html[i..].find("-->").map_or(bytes.len(), |p| i + p + 3);
            continue;
        }
        let (name, after) = tag_name(html, i);
        if name.is_empty() {
            continue;
        }
        let (attrs, after_tag) = attributes(html, after);
        i = after_tag;
        match name.as_str() {
            "meta" => {
                if let Some(cs) = attr(&attrs, "charset") {
                    page.charset.get_or_insert(cs.trim().to_ascii_lowercase());
                }
                if attr(&attrs, "http-equiv")
                    .is_some_and(|v| v.eq_ignore_ascii_case("content-type"))
                    && let Some(cs) = attr(&attrs, "content").and_then(charset_from_content_type)
                {
                    page.charset.get_or_insert(cs);
                }
                let key = attr(&attrs, "property").or_else(|| attr(&attrs, "name"));
                if let (Some(key), Some(content)) = (key, attr(&attrs, "content")) {
                    page.meta
                        .push((key.trim().to_ascii_lowercase(), content.to_owned()));
                }
            }
            "link" => {
                let rel = attr(&attrs, "rel").unwrap_or("").to_ascii_lowercase();
                let tokens: Vec<&str> = rel.split_ascii_whitespace().collect();
                let touch = tokens
                    .iter()
                    .any(|t| matches!(*t, "apple-touch-icon" | "apple-touch-icon-precomposed"));
                if (touch || tokens.contains(&"icon"))
                    && let Some(href) = attr(&attrs, "href").filter(|h| !h.trim().is_empty())
                {
                    page.icons.push(Icon {
                        href: href.trim().to_owned(),
                        touch,
                        size: attr(&attrs, "sizes").and_then(parse_sizes),
                    });
                }
            }
            "title" => {
                let end = find_ci(html, i, "</title").unwrap_or(bytes.len());
                if page.title.is_none() {
                    let text = collapse_ws(&decode_entities(&html[i..end]));
                    if !text.is_empty() {
                        page.title = Some(text);
                    }
                }
                i = end;
            }
            "script" | "style" | "template" | "noscript" | "textarea" => {
                let close = format!("</{name}");
                let end = find_ci(html, i, &close).unwrap_or(bytes.len());
                let is_ld = name == "script"
                    && attr(&attrs, "type")
                        .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/ld+json"));
                if is_ld && page.json_ld.len() < MAX_JSON_LD_BLOCKS {
                    page.json_ld.push(html[i..end].to_owned());
                }
                i = end;
            }
            _ => {}
        }
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_meta_title_icons_and_json_ld() {
        let html = r#"<!doctype html><html><head>
            <meta charset="utf-8">
            <!-- <meta property="og:title" content="commented out"> -->
            <meta data-x property="og:title" content="《柯洁围棋入门课》_哔哩哔哩bilibili_教学">
            <meta content='A &amp; B &#x4E2D;' name=description>
            <META PROPERTY="og:image" CONTENT="https://i2.hdslb.com/a.jpg@1200w_630h">
            <title> Hello
              &quot;world&quot; </title>
            <link rel="shortcut icon" href="/favicon.ico">
            <link rel="apple-touch-icon" sizes="57x57 180x180" href="/touch.png">
            <script>var x = "<meta property='og:title' content='in script'>";</script>
            <script type="application/ld+json">{"@type":"VideoObject","name":"x"}</script>
            </head><body></body></html>"#;
        let page = scan(html);
        assert_eq!(page.charset.as_deref(), Some("utf-8"));
        assert_eq!(
            page.meta("og:title"),
            Some("《柯洁围棋入门课》_哔哩哔哩bilibili_教学")
        );
        assert_eq!(page.meta("description"), Some("A & B 中"));
        assert_eq!(
            page.meta("og:image"),
            Some("https://i2.hdslb.com/a.jpg@1200w_630h")
        );
        assert_eq!(page.title.as_deref(), Some("Hello \"world\""));
        assert_eq!(page.icons.len(), 2);
        assert_eq!(page.icons[1].size, Some((180, 180)));
        assert!(page.icons[1].touch);
        assert_eq!(page.json_ld, vec![r#"{"@type":"VideoObject","name":"x"}"#]);
        assert!(!page.meta.iter().any(|(_, v)| v == "in script"));
    }

    #[test]
    fn survives_garbage() {
        for junk in [
            "<",
            "<meta",
            "<meta content=\"",
            "<title>",
            "<script>",
            "<!--",
            "&#xFFFFFFFF;",
            "<a =>",
        ] {
            let _ = scan(junk);
        }
        assert_eq!(decode_entities("&#0;&bogus;&#x41;"), "&#0;&bogus;A");
        let page = scan(r#"<meta http-equiv="Content-Type" content="text/html; charset=GBK">"#);
        assert_eq!(page.charset.as_deref(), Some("gbk"));
    }
}
