//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Tellomi link cards bridge (ADR-0063 §4.10).
//!
//! One function per `tellomi_links` call, over plain values, bytes and JSON text; every function
//! delegates to `tellomi_links::json`, so the JSON that comes out is the same on all three
//! platforms, byte for byte (`rust/links/tests/data/bridge-golden.json`).
//!
//! * `LinkRegistry_Load` / `LinkRegistry_LoadUpdate` — the shipped registry, or a hot update
//!   (signature → name → schema → newer version → §6.4 rules). `current_version = 0` means "no
//!   registry yet".
//! * `LinkRegistry_Begin` → `LinkJob`, then `LinkJob_NextRequest` / `LinkJob_On*` /
//!   `LinkJob_Finish`: the sender. The crate never touches the network; the client's fetcher does.
//! * `LinkRegistry_Classify` / `LinkRegistry_ReceiveCheck` / `LinkRegistry_OpenPlan`: the receiver.
//! * `Links_Layout` / `Links_Tint`: the card's shape and colours.
//!
//! Malformed JSON arguments are an `IllegalArgumentError`; nothing else here can fail.

use libsignal_bridge_macros::*;
use libsignal_bridge_types::links::{LinkJob, LinkRegistry};
use tellomi_links::json;
use tellomi_policy::PolicyEngine;

#[allow(unused_imports)]
use crate::support::*;
use crate::*;

bridge_handle_fns!(LinkRegistry, clone = false);
bridge_handle_fns!(LinkJob, clone = false);

fn illegal(e: impl std::fmt::Display) -> IllegalArgumentError {
    IllegalArgumentError::new(e.to_string())
}

/// The registry that ships inside the app (`links/dist/links-<version>.json`).
#[bridge_fn]
pub fn LinkRegistry_Load(envelope: &[u8]) -> Result<LinkRegistry, IllegalArgumentError> {
    tellomi_links::Registry::load(envelope)
        .map(LinkRegistry)
        .map_err(illegal)
}

/// A hot update and its `.sig` (hex), checked with `public_key` (33 bytes with the `0x05` prefix,
/// or 32). `current_version` is the version in use, `0` when there is none.
#[bridge_fn]
pub fn LinkRegistry_LoadUpdate(
    envelope: &[u8],
    signature_hex: String,
    public_key: &[u8],
    current_version: u64,
) -> Result<LinkRegistry, IllegalArgumentError> {
    let current = (current_version != 0).then_some(current_version);
    tellomi_links::Registry::load_update(envelope, &signature_hex, public_key, current)
        .map(LinkRegistry)
        .map_err(illegal)
}

#[bridge_fn]
pub fn LinkRegistry_Version(registry: &LinkRegistry) -> u64 {
    registry.0.version()
}

/// What this build could not honour and turned into the default (JSON array); for debug logs.
#[bridge_fn]
pub fn LinkRegistry_Degraded(registry: &LinkRegistry) -> String {
    json::degraded(&registry.0)
}

/// The Matcher's view of one URL (JSON), or null for a link no provider claims.
#[bridge_fn]
pub fn LinkRegistry_Identify(
    registry: &LinkRegistry,
    url: String,
    location: bool,
) -> Option<String> {
    json::identify(&registry.0, &url, location)
}

/// The card for a stored preview (JSON). `preview` and `message` are JSON (`rich` as hex).
#[bridge_fn]
pub fn LinkRegistry_Classify(
    registry: &LinkRegistry,
    preview: String,
    body: String,
    message: String,
) -> Result<String, IllegalArgumentError> {
    json::classify(&registry.0, &preview, &body, &message).map_err(illegal)
}

/// Whether to keep the preview and its `rich` when a message arrives (JSON).
#[bridge_fn]
pub fn LinkRegistry_ReceiveCheck(
    registry: &LinkRegistry,
    preview: String,
    body: String,
    message: String,
) -> Result<String, IllegalArgumentError> {
    json::receive_check(&registry.0, &preview, &body, &message).map_err(illegal)
}

/// What tapping this URL does (JSON).
#[bridge_fn]
pub fn LinkRegistry_OpenPlan(registry: &LinkRegistry, url: String) -> String {
    json::open_plan(&registry.0, &url)
}

/// Start previewing `url` as typed; `context` is the send context as JSON (`{}` = defaults).
#[bridge_fn]
pub fn LinkRegistry_Begin(
    registry: &LinkRegistry,
    url: String,
    context: String,
) -> Result<LinkJob, IllegalArgumentError> {
    json::begin(&registry.0, &url, &context)
        .map(LinkJob)
        .map_err(illegal)
}

/// The next request to perform (JSON), or null: then call `LinkJob_Finish`.
#[bridge_fn]
pub fn LinkJob_NextRequest(job: &mut LinkJob) -> Option<String> {
    json::next_request(&mut job.0)
}

/// An HTTP response: where the redirects ended, the content type, the first `Location` (short-link
/// expansion only) and the decompressed body.
#[bridge_fn]
pub fn LinkJob_OnResponse(
    job: &mut LinkJob,
    id: u32,
    status: u32,
    final_url: String,
    content_type: String,
    location: Option<String>,
    body: &[u8],
) {
    json::on_response(
        &mut job.0,
        id,
        status,
        &final_url,
        &content_type,
        location.as_deref(),
        body,
    )
}

/// DNS / TCP / TLS failure: the host is remembered as unreachable.
#[bridge_fn]
pub fn LinkJob_OnNetworkError(job: &mut LinkJob, id: u32) {
    job.0.on_network_error(id)
}

/// Any other failure (timeout after connecting, too large, a rejected redirect hop…).
#[bridge_fn]
pub fn LinkJob_OnFailure(job: &mut LinkJob, id: u32) {
    job.0.on_failure(id)
}

/// The result of a `first_party` request, as JSON (`{"ok":…,"invalid":…,"title":…,…}`).
#[bridge_fn]
pub fn LinkJob_OnFirstParty(
    job: &mut LinkJob,
    id: u32,
    result: String,
) -> Result<(), IllegalArgumentError> {
    json::on_first_party(&mut job.0, id, &result).map_err(illegal)
}

#[bridge_fn]
pub fn LinkJob_OnImage(job: &mut LinkJob, id: u32, ok: bool) {
    job.0.on_image(id, ok)
}

/// Assemble the preview (JSON). `preview.rich_hex` is `Preview` field 1000; a policy engine, when
/// given, voids a card whose text hits the lexicon.
#[bridge_fn]
pub fn LinkJob_Finish(job: &mut LinkJob, policy: Option<&PolicyEngine>) -> String {
    json::finish(&mut job.0, policy)
}

/// The card shape (`"first_party"`, `"large_image"`, `"icon"`, `"no_image"`) for an image of this
/// size (0 × 0 = none), a kind, and a level name as `LinkRegistry_Classify` returns it.
#[bridge_fn]
pub fn Links_Layout(
    image_width: u32,
    image_height: u32,
    kind: String,
    level: String,
) -> Result<String, IllegalArgumentError> {
    json::layout(image_width, image_height, &kind, &level).map_err(illegal)
}

/// Card colours from the card's own image, decoded to RGBA (JSON).
#[bridge_fn]
pub fn Links_Tint(
    layout: String,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<String, IllegalArgumentError> {
    json::tint(&layout, width, height, rgba).map_err(illegal)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIST: &[u8] =
        include_bytes!("../../../links/tests/data/registry/dist/links-2026092702.json");

    #[test]
    fn a_whole_send_through_the_bridge_functions() {
        let registry = LinkRegistry_Load(DIST).expect("dist loads");
        assert_eq!(LinkRegistry_Version(&registry), 2026092702);
        let mut job = LinkRegistry_Begin(&registry, "https://tell.cc/ceshi.57".into(), "{}".into())
            .expect("begins");
        assert_eq!(LinkJob_NextRequest(&mut job), None);
        let out = LinkJob_Finish(&mut job, None);
        assert!(out.contains(r#""title":"@ceshi.57""#), "{out}");
        assert!(LinkRegistry_Begin(&registry, "https://x.cn".into(), "not json".into()).is_err());
    }

    #[test]
    fn bad_input_is_an_illegal_argument_not_a_panic() {
        let registry = LinkRegistry_Load(DIST).expect("dist loads");
        assert!(LinkRegistry_Classify(&registry, "{".into(), "".into(), "{}".into()).is_err());
        assert!(Links_Layout(1, 1, "".into(), "nope".into()).is_err());
        assert!(LinkRegistry_Load(b"{}").is_err());
        assert!(LinkRegistry_LoadUpdate(DIST, "00".into(), &[5; 33], 0).is_err());
    }
}
