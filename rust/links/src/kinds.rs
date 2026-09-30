//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The kind vocabulary (ADR-0063 §4.6; `kinds.toml` schema 1), compiled in.
//!
//! This table is the contract between the extractor and the renderers, so it ships with the client
//! and is never taken from a hot update: an old client judges a registry by the kinds *it* can
//! render, not by the ones the registry says exist (§7.2). A test keeps it identical to the
//! registry's `kinds.toml`.

use serde::Serialize;

use crate::limits::MAX_ATTR_VALUE_CHARS;

/// `[attr_types]` in `kinds.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttrType {
    /// Plain text, 1–256 characters, never rendered as a link.
    String,
    /// Decimal non-negative integer.
    Int,
    /// Non-negative integer milliseconds.
    DurationMs,
    /// RFC 3339 date-time.
    Datetime,
    /// Decimal latitude in [-90, 90].
    Lat,
    /// Decimal longitude in [-180, 180].
    Lng,
    /// `wgs84` | `gcj02` | `bd09`.
    CoordSys,
    /// `ios` | `android`.
    Platform,
}

impl AttrType {
    pub fn name(self) -> &'static str {
        match self {
            AttrType::String => "string",
            AttrType::Int => "int",
            AttrType::DurationMs => "duration_ms",
            AttrType::Datetime => "datetime",
            AttrType::Lat => "lat",
            AttrType::Lng => "lng",
            AttrType::CoordSys => "coord_sys",
            AttrType::Platform => "platform",
        }
    }

    /// Check (and normalize) one attr value. `None` means "drop this attr" — never "drop the card"
    /// unless the kind requires it (§4.5, §5.2).
    pub fn validate(self, value: &str) -> Option<String> {
        match self {
            AttrType::String => {
                // The size limit is about what travelled; the rest is about what would be shown
                // (§6.1: zero-width and bidi control characters handled first, so that neither can
                // hide a link or a control character from the checks below).
                let travelled = value.chars().count();
                let text = crate::text::display_text(value);
                let ok = (1..=MAX_ATTR_VALUE_CHARS).contains(&travelled)
                    && !text.is_empty()
                    && !text.chars().any(char::is_control)
                    && !looks_like_link(&text);
                ok.then_some(text)
            }
            AttrType::Int | AttrType::DurationMs => {
                let ok = !value.is_empty()
                    && value.len() <= 19
                    && value.bytes().all(|b| b.is_ascii_digit());
                if !ok {
                    return None;
                }
                value.parse::<u64>().ok().map(|n| n.to_string())
            }
            AttrType::Datetime => crate::time::parse_rfc3339(value).map(|dt| dt.text),
            AttrType::Lat => decimal_in(value, 90.0),
            AttrType::Lng => decimal_in(value, 180.0),
            AttrType::CoordSys => {
                matches!(value, "wgs84" | "gcj02" | "bd09").then(|| value.to_owned())
            }
            AttrType::Platform => matches!(value, "ios" | "android").then(|| value.to_owned()),
        }
    }
}

/// Attr values are text, never something a renderer could turn into a tap target (§4.5).
fn looks_like_link(value: &str) -> bool {
    let lower = value.trim_start().to_ascii_lowercase();
    lower.contains("://")
        || [
            "javascript:",
            "data:",
            "intent:",
            "file:",
            "vbscript:",
            "content:",
            "about:",
            "blob:",
            "tel:",
            "sms:",
            "mailto:",
        ]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

fn decimal_in(value: &str, bound: f64) -> Option<String> {
    let body = value.strip_prefix('-').unwrap_or(value);
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (body, None),
    };
    let ok = (1..=3).contains(&int.len())
        && int.bytes().all(|b| b.is_ascii_digit())
        && frac
            .is_none_or(|f| (1..=15).contains(&f.len()) && f.bytes().all(|b| b.is_ascii_digit()));
    if !ok {
        return None;
    }
    let n: f64 = value.parse().ok()?;
    (n.abs() <= bound).then(|| value.to_owned())
}

/// One `[[kind]]` entry.
#[derive(Debug)]
pub struct KindDef {
    pub id: &'static str,
    pub first_party: bool,
    /// Only type text on a brand shell; structured plans may not produce it (L7).
    pub reserved: bool,
    /// `title` / `description` / `image` name the snapshot; anything else is an attr key.
    pub required: &'static [&'static str],
    /// Alternatives: satisfying any one group is enough.
    pub required_any: &'static [&'static [&'static str]],
    pub attrs: &'static [(&'static str, AttrType)],
}

impl KindDef {
    pub fn attr_type(&self, key: &str) -> Option<AttrType> {
        self.attrs.iter().find(|(k, _)| *k == key).map(|(_, t)| *t)
    }

    /// The one "are the required fields there" function, used by the sender's assembler and by
    /// the receiver's `classify` alike (§4.4 last bullet, §4.5).
    pub fn meets_required(&self, present: impl Fn(&str) -> bool) -> bool {
        self.required.iter().all(|f| present(f))
            && (self.required_any.is_empty()
                || self
                    .required_any
                    .iter()
                    .any(|group| group.iter().all(|f| present(f))))
    }
}

/// `kinds.toml` schema 1, in file order.
pub static KINDS: &[KindDef] = &[
    KindDef {
        id: "tellomi.user",
        first_party: true,
        reserved: false,
        required: &[],
        required_any: &[],
        attrs: &[],
    },
    KindDef {
        id: "tellomi.group",
        first_party: true,
        reserved: false,
        required: &["title"],
        required_any: &[],
        attrs: &[("member_count", AttrType::Int)],
    },
    KindDef {
        id: "tellomi.call",
        first_party: true,
        reserved: false,
        required: &[],
        required_any: &[],
        attrs: &[],
    },
    KindDef {
        id: "tellomi.sticker",
        first_party: true,
        reserved: false,
        required: &["title"],
        required_any: &[],
        attrs: &[("sticker_count", AttrType::Int)],
    },
    KindDef {
        id: "tellomi.official",
        first_party: true,
        reserved: false,
        required: &[],
        required_any: &[],
        attrs: &[],
    },
    KindDef {
        id: "video",
        first_party: false,
        reserved: false,
        required: &["title", "image"],
        required_any: &[],
        attrs: &[
            ("author", AttrType::String),
            ("duration_ms", AttrType::DurationMs),
            ("published_at", AttrType::Datetime),
        ],
    },
    KindDef {
        id: "channel",
        first_party: false,
        reserved: false,
        required: &["title"],
        required_any: &[],
        attrs: &[("author", AttrType::String)],
    },
    KindDef {
        id: "music.track",
        first_party: false,
        reserved: false,
        required: &["title"],
        required_any: &[],
        attrs: &[
            ("artist", AttrType::String),
            ("album", AttrType::String),
            ("duration_ms", AttrType::DurationMs),
        ],
    },
    KindDef {
        id: "music.album",
        first_party: false,
        reserved: false,
        required: &["title"],
        required_any: &[],
        attrs: &[("artist", AttrType::String), ("track_count", AttrType::Int)],
    },
    KindDef {
        id: "music.playlist",
        first_party: false,
        reserved: false,
        required: &["title"],
        required_any: &[],
        attrs: &[("author", AttrType::String), ("track_count", AttrType::Int)],
    },
    KindDef {
        id: "place",
        first_party: false,
        reserved: false,
        required: &[],
        required_any: &[&["lat", "lng", "coord_sys"], &["name"]],
        attrs: &[
            ("lat", AttrType::Lat),
            ("lng", AttrType::Lng),
            ("coord_sys", AttrType::CoordSys),
            ("name", AttrType::String),
            ("address", AttrType::String),
        ],
    },
    KindDef {
        id: "app",
        first_party: false,
        reserved: false,
        required: &["title", "image"],
        required_any: &[],
        attrs: &[
            ("developer", AttrType::String),
            ("platform", AttrType::Platform),
        ],
    },
    KindDef {
        id: "repo",
        first_party: false,
        reserved: false,
        required: &["title"],
        required_any: &[],
        attrs: &[("owner", AttrType::String)],
    },
    reserved("article"),
    reserved("product"),
    reserved("package"),
    reserved("question"),
    reserved("deal"),
    reserved("ride"),
    reserved("payment"),
    reserved("web"),
];

const fn reserved(id: &'static str) -> KindDef {
    KindDef {
        id,
        first_party: false,
        reserved: true,
        required: &[],
        required_any: &[],
        attrs: &[],
    }
}

/// `kinds.toml`'s own `schema`.
pub const KINDS_SCHEMA: u32 = 1;

/// The type text a brand shell uses when the route recognised no object (README §3.5).
pub const WEB: &str = "web";

pub fn kind(id: &str) -> Option<&'static KindDef> {
    KINDS.iter().find(|k| k.id == id)
}

/// Payment pages and one-off ride shares are locked to brand shells that only open in a browser
/// (L10), whatever category the provider claims.
pub(crate) fn is_locked_kind(id: &str) -> bool {
    matches!(id, "payment" | "ride")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attr_types() {
        assert_eq!(AttrType::Int.validate("0042").as_deref(), Some("42"));
        assert_eq!(AttrType::Int.validate("-1"), None);
        assert_eq!(AttrType::Int.validate("1.5"), None);
        assert_eq!(
            AttrType::Lat.validate("37.877682").as_deref(),
            Some("37.877682")
        );
        assert_eq!(AttrType::Lat.validate("91.0"), None);
        assert_eq!(AttrType::Lng.validate("-112.5").as_deref(), Some("-112.5"));
        assert_eq!(AttrType::Lng.validate("1e2"), None);
        assert_eq!(
            AttrType::CoordSys.validate("gcj02").as_deref(),
            Some("gcj02")
        );
        assert_eq!(AttrType::CoordSys.validate("GCJ02"), None);
        assert_eq!(AttrType::String.validate("柯洁").as_deref(), Some("柯洁"));
        assert_eq!(AttrType::String.validate(""), None);
        assert_eq!(AttrType::String.validate("https://evil.example/"), None);
        assert_eq!(AttrType::String.validate("javascript:alert(1)"), None);
        assert_eq!(
            AttrType::String
                .validate(&"长".repeat(256))
                .map(|s| s.len()),
            Some(768)
        );
        assert_eq!(AttrType::String.validate(&"长".repeat(257)), None);
        assert!(
            AttrType::Datetime
                .validate("2026-09-22T09:25:33.000Z")
                .is_some()
        );
        assert_eq!(AttrType::Datetime.validate("2026-09-22"), None);
    }

    #[test]
    fn required_fields() {
        let video = kind("video").unwrap();
        assert!(video.meets_required(|f| matches!(f, "title" | "image")));
        assert!(!video.meets_required(|f| f == "title"));
        let place = kind("place").unwrap();
        assert!(place.meets_required(|f| f == "name"));
        assert!(place.meets_required(|f| matches!(f, "lat" | "lng" | "coord_sys")));
        assert!(!place.meets_required(|f| matches!(f, "lat" | "lng")));
        assert!(kind("tellomi.user").unwrap().meets_required(|_| false));
    }
}
