//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Link cards (ADR-0063): one implementation of "what is this link and what card does it get",
//! shared by all three clients through libsignal's bridges.
//!
//! **Sans-IO.** The crate never touches the network or the file system. It takes bytes and returns
//! decisions; each client's existing safe fetcher performs the requests it asks for (§4.4).
//!
//! The whole surface, meant to cross the ffi / jni / node bridges one-to-one (plain values in,
//! JSON-serializable values and `RichContent` protobuf bytes out):
//!
//! | call | side | what |
//! | --- | --- | --- |
//! | [`Registry::load`] | both | the registry shipped in the app → registry, §6.4 rules re-applied |
//! | [`Registry::load_update`] | both | a hot update: signature → name → schema → newer version → rules |
//! | [`Registry::begin`] → [`Job`] | sender | Planner; then `next_request` / `on_*` / `finish` |
//! | [`Registry::receive_check`] | receiver | keep the preview / keep `rich` when a message arrives |
//! | [`Registry::classify`] | receiver | the card level and contents, at render time |
//! | [`Registry::open_plan`] | both | what a tap does |
//! | [`Registry::identify`] | tools | the Matcher's view of one URL |
//! | [`layout`], [`tint`] | both | card shape and colours |
//!
//! ```no_run
//! # use tellomi_links::*;
//! # let bytes: Vec<u8> = vec![];
//! let registry = Registry::load(&bytes).expect("the shipped registry is valid");
//! let mut job = registry.begin("https://www.bilibili.com/video/BV1YDhJ6ZEL6", &SendContext::default());
//! while let Some(request) = job.next_request() {
//!     // the client performs `request` and reports back, e.g.:
//!     job.on_network_error(request.id);
//! }
//! let outcome = job.finish(None);
//! let rich_bytes = outcome.preview.as_ref().and_then(PreviewDraft::rich_bytes);
//! ```

mod card;
mod classify;
mod extract;
mod html;
mod job;
mod kinds;
mod limits;
mod model;
mod open;
mod pattern;
mod registry;
mod rich;
mod spoof;
mod time;
mod urlx;
mod visual;

pub use card::{Level, UserName};
pub use classify::{Card, CardAttr, FirstPartyCard, MessageContext, PreviewInput, ReceiveCheck};
pub use job::{
    Failure, FirstPartyResult, Job, PreviewDraft, Request, RequestKind, SendContext, SendOutcome,
};
pub use kinds::{AttrType, KINDS, KINDS_SCHEMA, KindDef, kind};
pub use limits::*;
pub use model::LinksPayload;
pub use open::{OpenLabel, OpenPlan, OpenStep};
pub use registry::{
    DegradedRoute, Identified, LoadError, LocalizedName, Registry, Tier, Violation,
    verify_registry_signature,
};
pub use rich::{Attr, LEVEL_BRAND, LEVEL_STRUCTURED, RichContent};
pub use tellomi_policy::Region;
/// Registry update rule shared with policy: only a strictly larger `version` replaces the current one.
pub use tellomi_policy::envelope::is_newer;
pub use urlx::{display_domain, is_valid_preview_url, registrable_domain, url_appears_in_body};
pub use visual::{Colors, Layout, Rgb, Tint, layout, tint};
