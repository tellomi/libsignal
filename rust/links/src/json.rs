//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The crate's API in the shape the ffi / jni / node bridges carry: plain numbers, strings, bytes
//! and JSON text. `rust/bridge/shared/src/links.rs` calls these one-to-one, so every platform gets
//! byte-identical JSON for the same input — the output of these functions *is* what crosses the
//! bridge, and `tests/data/bridge-golden.json` pins it.
//!
//! JSON is compact (`serde_json::to_string`), fields in declaration order, maps sorted. Inputs are
//! the `Deserialize` shapes of [`SendContext`], [`PreviewInput`], [`MessageContext`] and
//! [`FirstPartyResult`]; unknown input fields are ignored.

use serde::Serialize;
use tellomi_policy::PolicyEngine;

use crate::{Job, Layout, Level, MessageContext, PreviewInput, Registry, SendContext};

/// A JSON argument that does not parse, or an enum name this build does not know.
#[derive(Debug, thiserror::Error)]
#[error("invalid {what}: {detail}")]
pub struct InputError {
    pub what: &'static str,
    pub detail: String,
}

fn parse<T: serde::de::DeserializeOwned>(what: &'static str, json: &str) -> Result<T, InputError> {
    serde_json::from_str(json).map_err(|e| InputError {
        what,
        detail: e.to_string(),
    })
}

fn to_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("these types always serialize")
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `Registry::degraded_routes` as a JSON array.
pub fn degraded(registry: &Registry) -> String {
    to_json(&registry.degraded_routes())
}

/// `Registry::identify` as JSON, or `None` for a generic link.
pub fn identify(registry: &Registry, url: &str, location: bool) -> Option<String> {
    registry.identify(url, location).map(|i| to_json(&i))
}

/// `Registry::classify`: `preview` is a [`PreviewInput`] (`rich` as hex), `message` a
/// [`MessageContext`]; returns the [`crate::Card`].
pub fn classify(
    registry: &Registry,
    preview: &str,
    body: &str,
    message: &str,
) -> Result<String, InputError> {
    let preview: PreviewInput = parse("preview", preview)?;
    let message: MessageContext = parse("message context", message)?;
    Ok(to_json(&registry.classify(&preview, body, &message)))
}

/// `Registry::receive_check`, same inputs as [`classify`].
pub fn receive_check(
    registry: &Registry,
    preview: &str,
    body: &str,
    message: &str,
) -> Result<String, InputError> {
    let preview: PreviewInput = parse("preview", preview)?;
    let message: MessageContext = parse("message context", message)?;
    Ok(to_json(&registry.receive_check(&preview, body, &message)))
}

/// `Registry::open_plan`.
pub fn open_plan(registry: &Registry, url: &str) -> String {
    to_json(&registry.open_plan(url))
}

/// `Registry::begin` with a [`SendContext`] as JSON (`{}` = all defaults).
pub fn begin(registry: &Registry, url: &str, context: &str) -> Result<Job, InputError> {
    let context: SendContext = parse("send context", context)?;
    Ok(registry.begin(url, &context))
}

/// `Job::next_request` as JSON.
pub fn next_request(job: &mut Job) -> Option<String> {
    job.next_request().map(|r| to_json(&r))
}

/// `Job::on_first_party` with a [`FirstPartyResult`] as JSON.
pub fn on_first_party(job: &mut Job, id: u32, result: &str) -> Result<(), InputError> {
    job.on_first_party(id, parse("first-party result", result)?);
    Ok(())
}

/// `Job::on_response` with the HTTP status as the bridges carry it (`u32`); anything that is not
/// a valid status is treated as a failed request.
pub fn on_response(
    job: &mut Job,
    id: u32,
    status: u32,
    final_url: &str,
    content_type: &str,
    location: Option<&str>,
    body: &[u8],
) {
    match u16::try_from(status) {
        Ok(status) => job.on_response(id, status, final_url, content_type, location, body),
        Err(_) => job.on_failure(id),
    }
}

/// `Job::finish` as JSON (`preview.rich_hex` holds the bytes for `Preview` field 1000).
pub fn finish(job: &mut Job, policy: Option<&PolicyEngine>) -> String {
    to_json(&job.finish(policy))
}

fn enum_from_name<T: serde::de::DeserializeOwned>(
    what: &'static str,
    name: &str,
) -> Result<T, InputError> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).map_err(|_| InputError {
        what,
        detail: format!("{name:?}"),
    })
}

fn enum_name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => unreachable!("unit enums serialize as strings"),
    }
}

/// [`crate::layout`] with the level as its JSON name (`"structured"`, `"brand"`, …); returns the
/// layout's name (`"large_image"`, `"icon"`, …).
pub fn layout(image_w: u32, image_h: u32, kind: &str, level: &str) -> Result<String, InputError> {
    let level: Level = enum_from_name("level", level)?;
    Ok(enum_name(&crate::layout(image_w, image_h, kind, level)))
}

/// [`crate::tint`] with the layout as its name; returns the [`crate::Tint`] as JSON.
pub fn tint(layout: &str, width: u32, height: u32, rgba: &[u8]) -> Result<String, InputError> {
    let layout: Layout = enum_from_name("layout", layout)?;
    Ok(to_json(&crate::tint(layout, width, height, rgba)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_names_round_trip() {
        assert_eq!(
            layout(1200, 630, "video", "structured").unwrap(),
            "large_image"
        );
        assert_eq!(layout(0, 0, "", "plain_link").unwrap(), "no_image");
        assert!(layout(0, 0, "", "Structured").is_err());
        let red = [255u8, 0, 0, 255].repeat(4);
        let t = tint("icon", 2, 2, &red).unwrap();
        assert!(
            t.starts_with(r##"{"tinted":true,"source":"#FF0000""##),
            "{t}"
        );
        assert!(tint("big", 2, 2, &red).is_err());
    }
}
