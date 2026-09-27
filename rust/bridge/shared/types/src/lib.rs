//
// Copyright 2020-2021 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

#![allow(clippy::missing_safety_doc)]
#![deny(clippy::unwrap_used)]

#[cfg(feature = "metadata")]
pub mod metadata;

#[cfg(feature = "ffi")]
#[macro_use]
pub mod ffi;

#[cfg(feature = "jni")]
#[macro_use]
pub mod jni;

#[cfg(feature = "node")]
#[macro_use]
pub mod node;

#[macro_use]
pub mod support;

pub use support::{AsyncRuntime, ResultReporter, describe_panic};

pub mod cds2;
pub mod crypto;
pub mod hsm_enclave;
pub mod net;
pub mod protocol;
pub mod sgx_session;
pub mod zkgroup;

mod pin {
    use ::libsignal_account_keys::PinHash;

    use crate::*;

    bridge_as_handle!(PinHash);
}

/// Tellomi: the policy engine is held across the bridge as an opaque handle — clients load the
/// lexicon once at startup and keep the engine for the life of the process (ADR-0062).
mod policy {
    use ::tellomi_policy::PolicyEngine;

    use crate::*;

    bridge_as_handle!(PolicyEngine);
}

/// Tellomi: link cards (ADR-0063). The registry is loaded once and shared; a job is one link being
/// previewed by the sender, driven request by request from the client's own fetcher.
pub mod links {
    use crate::*;

    pub struct LinkRegistry(pub ::tellomi_links::Registry);
    bridge_as_handle!(LinkRegistry);

    pub struct LinkJob(pub ::tellomi_links::Job);
    bridge_as_handle!(LinkJob, mut = true);
}

pub mod incremental_mac;
pub mod message_backup;

pub mod io;

#[cfg(feature = "signal-media")]
pub mod media {
    use signal_media::sanitize::mp4::SanitizedMetadata;

    use crate::*;

    bridge_as_handle!(SanitizedMetadata);
}
