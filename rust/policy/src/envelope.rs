//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The shared registry envelope (ADR-0064 `registry-envelope.schema.json`).
//!
//! `policy`, `links` and `stickers` ship as the same outer shape —
//! `{name, version, schema, generated_at, generated_by, inputs, payload}` — so one loader and one
//! update path serve all three (ADR-0062 §5, ADR-0063 §4.7 / §4.10). This module is that loader:
//! it checks the name and the schema range before the payload is even looked at, and it owns the
//! "is this update newer than what I have" rule. Each registry keeps its own payload type and its
//! own payload validation.

use std::ops::RangeInclusive;

use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};

/// The shared registry envelope. `payload` is generic: `LexiconPayload` for policy, the provider
/// list for links, the sticker manifest for stickers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub name: String,
    pub version: u64,
    pub schema: u32,
    #[serde(default)]
    pub generated_at: String,
    #[serde(default)]
    pub generated_by: String,
    #[serde(default)]
    pub inputs: Vec<EnvelopeInput>,
    pub payload: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvelopeInput {
    pub path: String,
    pub sha256: String,
}

/// Just the header, so a wrong name or an unsupported schema is reported as such instead of as
/// whatever the payload parser trips over first.
#[derive(Deserialize)]
struct Header {
    name: String,
    #[allow(dead_code)] // required to be present; read again with the payload
    version: u64,
    schema: u32,
    #[allow(dead_code)]
    payload: IgnoredAny,
}

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum EnvelopeError {
    /// not a {expected:?} registry: name was {found:?}
    WrongName { expected: String, found: String },
    /// registry schema {found} is outside the supported range {min}..={max}
    UnsupportedSchema { found: u32, min: u32, max: u32 },
    /// registry could not be parsed: {0}
    Parse(#[from] serde_json::Error),
}

/// Parse an envelope named `name` whose schema is in `schemas`.
///
/// Anything outside the range is an error rather than a best effort: the caller keeps the copy it
/// already has (the one that shipped with the app, or the last update that verified).
pub fn parse<T: DeserializeOwned>(
    bytes: &[u8],
    name: &str,
    schemas: RangeInclusive<u32>,
) -> Result<Envelope<T>, EnvelopeError> {
    let header: Header = serde_json::from_slice(bytes)?;
    if header.name != name {
        return Err(EnvelopeError::WrongName {
            expected: name.to_owned(),
            found: header.name,
        });
    }
    if !schemas.contains(&header.schema) {
        return Err(EnvelopeError::UnsupportedSchema {
            found: header.schema,
            min: *schemas.start(),
            max: *schemas.end(),
        });
    }
    Ok(serde_json::from_slice(bytes)?)
}

/// Whether a candidate update replaces the registry currently in use: only a strictly larger
/// `version` does (ADR-0063 §5.4 / §7.3). An equal or older version — a replay, a stale mirror, a
/// rollback — is ignored without an error.
pub fn is_newer(candidate_version: u64, current_version: Option<u64>) -> bool {
    current_version.is_none_or(|current| candidate_version > current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Payload {
        n: u32,
    }

    #[test]
    fn checks_name_then_schema_then_payload() {
        let ok = br#"{"name":"links","version":7,"schema":1,"payload":{"n":3}}"#;
        let env: Envelope<Payload> = parse(ok, "links", 1..=1).expect("valid");
        assert_eq!((env.version, env.payload.n), (7, 3));

        // A wrong name is reported as such even though the payload would not parse either.
        let wrong = br#"{"name":"policy","version":7,"schema":1,"payload":{"rules":[]}}"#;
        assert!(matches!(
            parse::<Payload>(wrong, "links", 1..=1),
            Err(EnvelopeError::WrongName { .. })
        ));

        let future = br#"{"name":"links","version":7,"schema":2,"payload":{"new":true}}"#;
        assert!(matches!(
            parse::<Payload>(future, "links", 1..=1),
            Err(EnvelopeError::UnsupportedSchema {
                found: 2,
                min: 1,
                max: 1
            })
        ));

        let bad_payload = br#"{"name":"links","version":7,"schema":1,"payload":{}}"#;
        assert!(matches!(
            parse::<Payload>(bad_payload, "links", 1..=1),
            Err(EnvelopeError::Parse(_))
        ));
    }

    #[test]
    fn only_strictly_newer_versions_replace() {
        assert!(is_newer(2026092701, None));
        assert!(is_newer(2026092702, Some(2026092701)));
        assert!(!is_newer(2026092701, Some(2026092701)));
        assert!(!is_newer(2026092601, Some(2026092701)));
    }
}
