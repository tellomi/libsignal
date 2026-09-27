//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! `RichContent`, the one protocol addition (ADR-0063 §4.5), as a Rust model with the exact field
//! numbers of `proto/rich_content.proto`:
//!
//! ```proto
//! message Preview { /* upstream 1–5 unchanged */ optional RichContent rich = 1000; }
//! message RichContent {
//!   optional string kind = 1;  optional string provider = 2;  optional uint32 schema = 3;
//!   optional string canonical_url = 4;  repeated Attr attrs = 5;  optional uint32 level = 6;
//! }
//! message Attr { optional string key = 1; optional string value = 2; }
//! ```
//!
//! Clients store the bytes they received untouched (§7.4) and hand them back to `classify` at
//! render time; nothing here re-encodes a received value.

use prost::Message;
use serde::Serialize;

use crate::limits::*;

/// `Preview.rich` (field 1000).
#[derive(Clone, PartialEq, Message, Serialize)]
pub struct RichContent {
    #[prost(string, optional, tag = "1")]
    pub kind: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub provider: Option<String>,
    #[prost(uint32, optional, tag = "3")]
    pub schema: Option<u32>,
    #[prost(string, optional, tag = "4")]
    pub canonical_url: Option<String>,
    #[prost(message, repeated, tag = "5")]
    pub attrs: Vec<Attr>,
    /// 1 = brand shell, 2 = structured card; absent or unknown = brand shell (§4.5). Only ever
    /// lowers what the receiver computes by itself; ignored for first-party cards.
    #[prost(uint32, optional, tag = "6")]
    pub level: Option<u32>,
}

#[derive(Clone, PartialEq, Message, Serialize)]
pub struct Attr {
    #[prost(string, optional, tag = "1")]
    pub key: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub value: Option<String>,
}

/// `RichContent.level` values.
pub const LEVEL_BRAND: u32 = 1;
pub const LEVEL_STRUCTURED: u32 = 2;

impl RichContent {
    pub fn encode_to_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }

    /// Parse received bytes and apply the whole-value bounds of §6.1 / §7.1: `None` means "drop
    /// `rich`, keep the snapshot". Per-attr problems are not judged here — `classify` drops those
    /// one by one against the kind's attr types.
    pub fn decode_checked(bytes: &[u8]) -> Option<RichContent> {
        if bytes.len() > MAX_RICH_BYTES {
            return None;
        }
        let rich = RichContent::decode(bytes).ok()?;
        let too_long =
            |s: &Option<String>, max: usize| s.as_ref().is_some_and(|s| s.chars().count() > max);
        if too_long(&rich.kind, MAX_KIND_CHARS)
            || too_long(&rich.provider, MAX_PROVIDER_CHARS)
            || too_long(&rich.canonical_url, MAX_CANONICAL_URL_CHARS)
            || rich.attrs.len() > MAX_ATTRS
        {
            return None;
        }
        Some(rich)
    }

    /// `schema` 0 or absent means 1 (§5.3).
    pub fn effective_schema(&self) -> u32 {
        match self.schema {
            None | Some(0) => RICH_SCHEMA,
            Some(n) => n,
        }
    }
}

/// An attr key the protocol allows: `[a-z_]{1,32}` (§4.5).
pub(crate) fn is_valid_attr_key(key: &str) -> bool {
    (1..=MAX_ATTR_KEY_CHARS).contains(&key.len())
        && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_numbers_match_the_adr() {
        let rich = RichContent {
            kind: Some("video".into()),
            provider: Some("bilibili".into()),
            schema: Some(1),
            canonical_url: Some("u".into()),
            attrs: vec![Attr {
                key: Some("k".into()),
                value: Some("v".into()),
            }],
            level: Some(2),
        };
        let bytes = rich.encode_to_bytes();
        // tag = field << 3 | wire type (2 = length-delimited, 0 = varint)
        let expected: Vec<u8> = [
            &[0x0A, 5][..],
            b"video",
            &[0x12, 8],
            b"bilibili",
            &[0x18, 1],
            &[0x22, 1],
            b"u",
            &[0x2A, 6, 0x0A, 1],
            b"k",
            &[0x12, 1],
            b"v",
            &[0x30, 2],
        ]
        .concat();
        assert_eq!(bytes, expected);
        assert_eq!(RichContent::decode_checked(&bytes), Some(rich));

        // `Preview.rich = 1000`: key = 1000 << 3 | 2 = 8002 = varint C2 3E.
        #[derive(Clone, PartialEq, Message)]
        struct PreviewRichOnly {
            #[prost(message, optional, tag = "1000")]
            rich: Option<RichContent>,
        }
        let wrapped = PreviewRichOnly {
            rich: Some(RichContent::default()),
        }
        .encode_to_vec();
        assert_eq!(wrapped, vec![0xC2, 0x3E, 0x00]);
    }

    #[test]
    fn bounds_drop_the_whole_value() {
        let ok = RichContent {
            kind: Some("k".repeat(MAX_KIND_CHARS)),
            ..Default::default()
        };
        assert!(RichContent::decode_checked(&ok.encode_to_bytes()).is_some());
        for bad in [
            RichContent {
                kind: Some("k".repeat(MAX_KIND_CHARS + 1)),
                ..Default::default()
            },
            RichContent {
                provider: Some("p".repeat(MAX_PROVIDER_CHARS + 1)),
                ..Default::default()
            },
            RichContent {
                canonical_url: Some("u".repeat(MAX_CANONICAL_URL_CHARS + 1)),
                ..Default::default()
            },
            RichContent {
                attrs: vec![Attr::default(); MAX_ATTRS + 1],
                ..Default::default()
            },
        ] {
            assert_eq!(RichContent::decode_checked(&bad.encode_to_bytes()), None);
        }
        assert_eq!(RichContent::decode_checked(&[0xFF, 0xFF]), None);
        assert_eq!(
            RichContent::decode_checked(&vec![0; MAX_RICH_BYTES + 1]),
            None
        );
    }
}
