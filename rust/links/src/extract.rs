//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The field-mapping language of the registry (README §4) and the JSON / JSON-LD side of the
//! extractor (ADR-0063 §4.4).
//!
//! `map` is a closed little language on purpose: a source expression, then zero or more
//! converters from a fixed set. No expressions, no arithmetic — a hot update can combine what this
//! build already knows, never make it run something new (§6.4, README §9).

use serde_json::Value;

use crate::limits::MAX_JSON_DEPTH;
use crate::time;

/// README §4 converters. A converter this build does not know degrades the route (§6.4 compat).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Converter {
    UrlDecode,
    Trim,
    Int,
    Iso8601Ms,
    SecondsMs,
    Rfc3339,
    UnixS,
}

impl Converter {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "urldecode" => Converter::UrlDecode,
            "trim" => Converter::Trim,
            "int" => Converter::Int,
            "iso8601_ms" => Converter::Iso8601Ms,
            "seconds_ms" => Converter::SecondsMs,
            "rfc3339" => Converter::Rfc3339,
            "unix_s" => Converter::UnixS,
            _ => return None,
        })
    }

    pub fn apply(self, value: &str) -> Option<String> {
        match self {
            Converter::UrlDecode => Some(crate::urlx::percent_decode(value)),
            Converter::Trim => Some(value.trim().to_owned()),
            Converter::Int => {
                let v = value.trim();
                let digits = v.strip_prefix('-').unwrap_or(v);
                (!digits.is_empty()
                    && digits.len() <= 19
                    && digits.bytes().all(|b| b.is_ascii_digit()))
                .then(|| v.parse::<i64>().ok().map(|n| n.to_string()))
                .flatten()
            }
            Converter::Iso8601Ms => time::iso8601_duration_ms(value).map(|ms| ms.to_string()),
            Converter::SecondsMs => {
                let secs: f64 = value.trim().parse().ok()?;
                if !secs.is_finite() || !(0.0..=1e12).contains(&secs) {
                    return None;
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                Some(((secs * 1000.0).round() as u64).to_string()) // bounded above
            }
            Converter::Rfc3339 => time::parse_rfc3339(value).map(|dt| dt.text),
            Converter::UnixS => {
                let secs: i64 = value.trim().parse().ok()?;
                time::unix_seconds_to_rfc3339(secs)
            }
        }
    }
}

/// One step of a JSON path: `results[0].trackName` is `Key(results) Index(0) Key(trackName)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Seg {
    Key(String),
    Index(usize),
}

/// Dot-separated keys with `[n]` subscripts; no wildcards, no filters (README §4).
pub(crate) fn parse_json_path(expr: &str) -> Option<Vec<Seg>> {
    let mut segs = Vec::new();
    for part in expr.split('.') {
        let (key, mut rest) = match part.find('[') {
            Some(at) => (&part[..at], &part[at..]),
            None => (part, ""),
        };
        if key.is_empty() && segs.is_empty() && rest.is_empty() {
            return None;
        }
        if !key.is_empty() {
            if key.contains(']') {
                return None;
            }
            segs.push(Seg::Key(key.to_owned()));
        } else if rest.is_empty() {
            return None; // empty segment, e.g. `a..b`
        }
        while !rest.is_empty() {
            let inner = rest.strip_prefix('[')?;
            let close = inner.find(']')?;
            let index: usize = inner[..close].parse().ok()?;
            segs.push(Seg::Index(index));
            rest = &inner[close + 1..];
        }
    }
    (!segs.is_empty()).then_some(segs)
}

/// Follow a path to a scalar. Objects, arrays and nulls at the end are "missing".
pub(crate) fn eval_path(value: &Value, path: &[Seg]) -> Option<String> {
    let mut cur = value;
    for seg in path {
        cur = match seg {
            Seg::Key(k) => cur.as_object()?.get(k)?,
            Seg::Index(i) => cur.as_array()?.get(*i)?,
        };
    }
    match cur {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Nesting depth of a JSON text, counted without parsing it (§4.4: JSON ≤ 32 levels). `None`
/// when it is deeper than allowed.
pub(crate) fn json_depth_ok(text: &[u8]) -> bool {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for &b in text {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_JSON_DEPTH {
                    return false;
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    true
}

pub(crate) fn parse_json(text: &[u8]) -> Option<Value> {
    if !json_depth_ok(text) {
        return None;
    }
    serde_json::from_slice(text).ok()
}

fn has_type(node: &Value, wanted: &str) -> bool {
    match node.get("@type") {
        Some(Value::String(t)) => t == wanted,
        Some(Value::Array(ts)) => ts.iter().any(|t| t.as_str() == Some(wanted)),
        _ => false,
    }
}

/// Top-level JSON-LD nodes of the wanted `@type`: the block itself, the elements of a top-level
/// array, and the members of `@graph`. Nothing deeper — a nested node is not "the page's object".
pub(crate) fn json_ld_nodes(blocks: &[String], wanted: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for block in blocks {
        let Some(value) = parse_json(block.trim().as_bytes()) else {
            continue;
        };
        let mut candidates = Vec::new();
        match value {
            Value::Array(items) => candidates.extend(items),
            Value::Object(ref obj) => {
                if let Some(Value::Array(graph)) = obj.get("@graph") {
                    candidates.extend(graph.iter().cloned());
                }
                candidates.push(value);
            }
            _ => {}
        }
        out.extend(candidates.into_iter().filter(|n| has_type(n, wanted)));
    }
    out
}

/// The identity URLs a JSON-LD node claims: `url`, `@id`, and `mainEntityOfPage` (a string, or an
/// object with `@id` / `url`).
pub(crate) fn json_ld_identities(node: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    for key in ["url", "@id"] {
        if let Some(s) = node.get(key).and_then(Value::as_str) {
            ids.push(s.to_owned());
        }
    }
    match node.get("mainEntityOfPage") {
        Some(Value::String(s)) => ids.push(s.clone()),
        Some(Value::Object(o)) => {
            for key in ["@id", "url"] {
                if let Some(s) = o.get(key).and_then(Value::as_str) {
                    ids.push(s.to_owned());
                }
            }
        }
        _ => {}
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_paths() {
        let v: Value = serde_json::json!({
            "results": [{"trackName": "WeChat", "artworkUrl512": "https://a/b.png", "n": 5}],
            "author": {"name": "柯洁"},
            "thumbnailUrl": ["https://i2.hdslb.com/x.jpg"]
        });
        let get = |p: &str| eval_path(&v, &parse_json_path(p).unwrap());
        assert_eq!(get("results[0].trackName").as_deref(), Some("WeChat"));
        assert_eq!(get("results[0].n").as_deref(), Some("5"));
        assert_eq!(
            get("thumbnailUrl[0]").as_deref(),
            Some("https://i2.hdslb.com/x.jpg")
        );
        assert_eq!(get("author.name").as_deref(), Some("柯洁"));
        assert_eq!(get("author"), None, "objects are not scalars");
        assert_eq!(get("results[1].trackName"), None);
        assert!(parse_json_path("a..b").is_none());
        assert!(parse_json_path("a[x]").is_none());
        assert!(parse_json_path("").is_none());
    }

    #[test]
    fn converters() {
        let c = |name: &str, v: &str| Converter::parse(name).unwrap().apply(v);
        assert_eq!(c("iso8601_ms", "PT00H08M19S").as_deref(), Some("499000"));
        assert_eq!(c("seconds_ms", "12.5").as_deref(), Some("12500"));
        assert_eq!(c("int", " 042 ").as_deref(), Some("42"));
        assert_eq!(c("int", "4.2"), None);
        assert_eq!(c("urldecode", "%E5%90%8C").as_deref(), Some("同"));
        assert_eq!(c("unix_s", "0").as_deref(), Some("1970-01-01T00:00:00Z"));
        assert!(Converter::parse("eval").is_none());
    }

    #[test]
    fn depth_limit() {
        let deep = format!("{}{}", "[".repeat(33), "]".repeat(33));
        assert!(!json_depth_ok(deep.as_bytes()));
        let ok = format!("{}{}", "[".repeat(32), "]".repeat(32));
        assert!(json_depth_ok(ok.as_bytes()));
        assert!(json_depth_ok(
            br#"{"a":"[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[["}"#
        ));
    }

    #[test]
    fn json_ld_nodes_and_identities() {
        let blocks = vec![
            r#"{"@type":"WebPage","url":"https://x/1"}"#.to_owned(),
            r#"{"@graph":[{"@type":"VideoObject","@id":"https://x/v#video","mainEntityOfPage":{"@id":"https://x/v"}}]}"#.to_owned(),
            r#"[{"@type":["Thing","VideoObject"],"url":"https://x/2"}]"#.to_owned(),
            "not json".to_owned(),
        ];
        let nodes = json_ld_nodes(&blocks, "VideoObject");
        assert_eq!(nodes.len(), 2);
        assert_eq!(
            json_ld_identities(&nodes[0]),
            vec!["https://x/v#video".to_owned(), "https://x/v".to_owned()]
        );
    }
}
