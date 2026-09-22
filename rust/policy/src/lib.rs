//
// Copyright (C) 2026 Tellomi.
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Tellomi Policy Engine (ADR-0062).
//!
//! One implementation of "may this public identity string be used", shared by every client and by
//! the server through libsignal's existing bridges. Usernames are only the first caller: display
//! names, group names, bios, tell.cc slugs, and later bots and mini apps all ask the same question
//! of the same lexicon.
//!
//! Business code never spells a word out. It asks:
//!
//! ```no_run
//! # use tellomi_policy::{PolicyEngine, Field, Region};
//! # let bytes: Vec<u8> = vec![];
//! let engine = PolicyEngine::load(&bytes).expect("shipped lexicon is valid");
//! let verdict = engine.check("tellomi_support", Field::Username, &[Region::Global, Region::Cn]);
//! if !verdict.is_allowed() {
//!     // The UI says "this name is unavailable" and nothing more: the reason, the rule and the
//!     // lexicon it came from stay on this side (ADR-0062 §12).
//! }
//! ```

mod engine;
mod lexicon;
pub mod normalize;

pub use engine::{Hit, LoadError, PolicyEngine, Verdict};
pub use lexicon::{
    AllowEntry, Envelope, EnvelopeInput, Field, LexiconPayload, Match, Outcome, Region, Rule,
    SCHEMA_MAX, SCHEMA_MIN, UnknownName, is_long_enough_for_contains, parse_regions,
};
