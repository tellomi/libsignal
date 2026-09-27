//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Numbers and names the three clients must agree on. They live here, compiled into the crate,
//! rather than in the registry: a hot update can tighten what the registry says but can never move
//! any of these (ADR-0063 §4.4, §4.5, §6.4).

/// The one User-Agent every metadata request carries (ADR-0063 §3.4.1). The registry's `fetch.ua`
/// is reserved and rejected at load (L10).
pub const USER_AGENT: &str = "WhatsApp/2";

/// At most this many metadata requests per link, short-link expansion included (§4.4).
pub const MAX_METADATA_REQUESTS: usize = 3;
/// Redirect hops per request; every hop is re-validated by the client's fetcher (§4.4 / §6.2).
pub const MAX_REDIRECTS: u32 = 5;
pub const CONNECT_TIMEOUT_MS: u32 = 5_000;
pub const REQUEST_TIMEOUT_MS: u32 = 10_000;
/// Total wall-clock budget for one link. The client stops asking for requests once it is spent
/// and calls `finish` with what it has (§5.2).
pub const LINK_BUDGET_MS: u32 = 10_000;

/// Size limits counted on **decompressed** bytes (§4.4).
pub const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_JSON_BYTES: usize = 256 * 1024;
pub const MAX_JSON_DEPTH: usize = 32;

/// `RichContent` bounds (§4.5, §6.1). Exceeding the first four drops the whole `rich` on receipt;
/// an individual attr that is malformed is dropped on its own.
pub const MAX_KIND_CHARS: usize = 32;
pub const MAX_PROVIDER_CHARS: usize = 32;
pub const MAX_CANONICAL_URL_CHARS: usize = 2048;
pub const MAX_ATTRS: usize = 16;
pub const MAX_ATTR_KEY_CHARS: usize = 32;
pub const MAX_ATTR_VALUE_CHARS: usize = 256;
/// Encoded `RichContent` larger than this is not even parsed. The largest legal value is about
/// 26 KiB (16 attrs of 256 four-byte characters plus a 2048-character URL).
pub const MAX_RICH_BYTES: usize = 32 * 1024;

/// `RichContent.schema` values this build understands; 0 or absent means 1 (§5.3, §7.1).
pub const RICH_SCHEMA: u32 = 1;
pub const RICH_SCHEMA_MAX: u32 = 1;

/// The registry envelope this build accepts (§7.3).
pub const REGISTRY_NAME: &str = "links";
pub const REGISTRY_SCHEMA_MIN: u32 = 1;
pub const REGISTRY_SCHEMA_MAX: u32 = 1;

/// The only provider that may be first-party. Every first-party hard rule is keyed on this id and
/// never on `tier`, because `tier` is data a hot update can change (§6.4, Pro's B12).
pub const TELLOMI: &str = "tellomi";
/// Upper bound for `tellomi.domains` (L22). A hot update may only remove hosts, never add.
pub const FIRST_PARTY_HOSTS_MAX: &[&str] = &["tell.cc"];
/// Upper bound for `tellomi.official_domains` (L22). `tellomi.cn` is in the bound ahead of time
/// and takes effect only once `tellomi.toml` lists it (card-visual §5.1).
pub const OFFICIAL_HOSTS_MAX: &[&str] = &[
    "tellomi.app",
    "www.tellomi.app",
    "tellomi.cn",
    "www.tellomi.cn",
];
/// First-party kinds are recognised by prefix, not by the registry's own vocabulary (L7).
pub const FIRST_PARTY_KIND_PREFIX: &str = "tellomi.";

/// An `og:image` whose declared short side is below this is treated as absent, and the one image
/// request goes to the site icon instead (§4.4 icon fallback).
pub const SMALL_OG_IMAGE_PX: u32 = 150;
/// Icons are only picked from `<link>` tags that declare at least this size.
pub const MIN_ICON_PX: u32 = 64;

/// The official card shows the parsed path, never decoded, truncated to this many characters
/// followed by `…` (§4.8).
pub const MAX_OFFICIAL_PATH_CHARS: usize = 32;

/// Snapshot text caps, so a hostile page cannot put a megabyte into a message.
pub const MAX_TITLE_CHARS: usize = 300;
pub const MAX_DESCRIPTION_CHARS: usize = 1000;

pub(crate) fn is_first_party_host(host: &str) -> bool {
    FIRST_PARTY_HOSTS_MAX.contains(&host)
}

pub(crate) fn is_official_host_bound(host: &str) -> bool {
    OFFICIAL_HOSTS_MAX.contains(&host)
}

/// Every host that only `tellomi` may name, in any field, directly or under a wildcard (L23).
pub(crate) fn compiled_first_party_hosts() -> impl Iterator<Item = &'static str> {
    FIRST_PARTY_HOSTS_MAX
        .iter()
        .chain(OFFICIAL_HOSTS_MAX.iter())
        .copied()
}
