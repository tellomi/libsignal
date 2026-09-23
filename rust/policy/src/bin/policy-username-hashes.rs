//
// Copyright (C) 2026 Tellomi.
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Stage 3 of the policy build: the server-side username denylist (ADR-0062 §5.4).
//!
//! The server never sees a username in the clear — the client sends `Username::hash()` and the
//! server stores that. So the only way for the server to refuse `admin.01` is to know the hash of
//! `admin.01` in advance. This binary enumerates `<reserved nickname>.<discriminator>` for the
//! discriminator ranges the Signal clients actually pick from, hashes each one with libsignal's
//! own function, and writes a Bloom filter.
//!
//! Why a Bloom filter rather than the hashes themselves: ~3.8 million entries × 32 bytes is about
//! 121 MB, while a filter at a 1e-6 false-positive rate is about 13 MB. A false positive costs the
//! user nothing — the client is already looping over a list of candidate usernames and simply
//! tries the next one — while a false negative is impossible, which is the direction that matters.
//!
//! The limits of this defence are written down in the ADR: a modified client can pick a
//! discriminator outside the enumerated ranges, and the engine on the client is what catches that.
//!
//!     cargo run -p tellomi-policy --features build-tools --bin policy-username-hashes -- \
//!         policy/dist/policy-2026092201.json policy/dist/username-denylist-2026092201.bloom

use std::collections::BTreeSet;
use std::process::ExitCode;

use sha2::{Digest, Sha256};
use tellomi_policy::{Envelope, Field, LexiconPayload, Match, Outcome, normalize};
use usernames::{NicknameLimits, Username};

/// How many discriminators to enumerate per nickname, by how much the term matters.
///
/// Signal's clients propose candidates from the low ranges first (`DISCRIMINATOR_RANGES` in
/// `libsignal/rust/usernames/src/constants.rs` starts at 1..100), so covering 1..=9999 catches
/// every username a stock client would offer for a brand term.
const CORE_DISCRIMINATOR_MAX: u32 = 9_999;
const OTHER_DISCRIMINATOR_MAX: u32 = 999;

/// Outcomes worth spending denylist space on. `reserved` covers the routing words, but the
/// expensive high range is kept for the terms someone would actually impersonate.
fn is_core(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::BrandProtected | Outcome::Impersonation)
}

/// Bounded cross product of brand-protected × impersonation affix terms, both orders, joined by
/// `_` — the only character the username grammar (`[a-z][a-z0-9_]{1,31}`) allows that also forms
/// a PREFIX/SUFFIX match boundary. `match_affix` (`engine.rs`) requires the character immediately
/// outside the term to be non-alphanumeric or the string to end there; letters and digits never
/// qualify, so a bare concatenation like `tellomisupport` has no boundary anywhere in the middle
/// and the client engine would never flag it as a PREFIX/SUFFIX hit. Generating it here would only
/// bloat the filter with entries nothing can ever be denied against, so `_` is the only join.
///
/// Every input already passed `normalize::is_username_safe` (the caller's collection loop),
/// and `_` is itself in that alphabet, so every combination produced here is safe too.
fn derive_affix_combinations(
    brand: &BTreeSet<String>,
    role: &BTreeSet<String>,
) -> BTreeSet<String> {
    brand
        .iter()
        .flat_map(|b| {
            role.iter()
                .flat_map(move |r| [format!("{b}_{r}"), format!("{r}_{b}")])
        })
        .collect()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(input), Some(output)) = (args.first(), args.get(1)) else {
        eprintln!(
            "usage: policy-username-hashes <policy-<version>.json> <out.bloom> \
             [--fp-rate 1e-6] [--verify <nickname.discriminator>]"
        );
        return ExitCode::FAILURE;
    };
    let mut fp_rate = 1e-6f64;
    let mut verify: Vec<String> = Vec::new();
    let mut rest = args[2..].iter();
    while let Some(flag) = rest.next() {
        match (flag.as_str(), rest.next()) {
            ("--fp-rate", Some(v)) => match v.parse() {
                Ok(v) => fp_rate = v,
                Err(e) => {
                    eprintln!("bad --fp-rate: {e}");
                    return ExitCode::FAILURE;
                }
            },
            ("--verify", Some(v)) => verify.push(v.clone()),
            (other, _) => {
                eprintln!("unknown argument {other}");
                return ExitCode::FAILURE;
            }
        }
    }

    match run(input, output, fp_rate, &verify) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("policy-username-hashes failed: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(input: &str, output: &str, fp_rate: f64, verify: &[String]) -> Result<(), String> {
    let raw = std::fs::read(input).map_err(|e| format!("reading {input}: {e}"))?;
    let envelope: Envelope<LexiconPayload> =
        serde_json::from_slice(&raw).map_err(|e| format!("parsing {input}: {e}"))?;

    // Which nicknames to enumerate: every rule that applies to usernames by an equality- or
    // affix-shaped match. CONTAINS and CONFUSABLE are deliberately excluded — enumerating every
    // string that *contains* a term, or every visual variant of it, is unbounded. The client-side
    // engine covers those; see the ADR on what this filter does and does not promise.
    //
    // PREFIX and SUFFIX are a narrower case of the same unboundedness: `tellomi*` matches any
    // string that *starts* with `tellomi`, which is just as unenumerable as CONTAINS in general.
    // Inserting only the bare term (as the loop below does) gives it zero actual PREFIX/SUFFIX
    // coverage — it is fully subsumed by the NORMALIZED_EXACT entry for the same term, so an
    // affix-tagged term contributed *nothing* beyond what an exact-only term would have.
    //
    // The one case worth closing: a PREFIX/SUFFIX brand term combined with a PREFIX/SUFFIX
    // impersonation term — `tellomi_staff`, `support_tellomi` — is exactly the shape
    // `tellomi/impersonation.toml`'s own comment calls "真正的冒充" (the real impersonation
    // shape), and it *is* bounded: brand terms × role terms is a small cross product. See
    // `derive_affix_combinations` below for why only `_` is a valid separator and other/no
    // combination is generated.
    let mut core: BTreeSet<String> = BTreeSet::new();
    let mut other: BTreeSet<String> = BTreeSet::new();
    let mut brand_affix: BTreeSet<String> = BTreeSet::new();
    let mut role_affix: BTreeSet<String> = BTreeSet::new();
    for rule in &envelope.payload.rules {
        if !rule.fields.contains(&Field::Username) {
            continue;
        }
        let has_affix = rule
            .matches
            .iter()
            .any(|m| matches!(m, Match::Prefix | Match::Suffix));
        let enumerable = has_affix
            || rule
                .matches
                .iter()
                .any(|m| matches!(m, Match::Exact | Match::NormalizedExact));
        if !enumerable {
            continue;
        }
        if !normalize::is_username_safe(&rule.normalized) {
            continue;
        }
        if is_core(rule.outcome) {
            &mut core
        } else {
            &mut other
        }
        .insert(rule.normalized.clone());
        if has_affix {
            match rule.outcome {
                Outcome::BrandProtected => {
                    brand_affix.insert(rule.normalized.clone());
                }
                Outcome::Impersonation => {
                    role_affix.insert(rule.normalized.clone());
                }
                _ => {}
            }
        }
    }
    let derived = derive_affix_combinations(&brand_affix, &role_affix);
    println!(
        "affix combinations: {} brand × {} role terms → {} bounded `brand_role`/`role_brand` \
         pairs (only `_` is a valid boundary inside a username; see comment)",
        brand_affix.len(),
        role_affix.len(),
        derived.len()
    );
    core.extend(derived);
    other.retain(|t| !core.contains(t));

    let total = core.len() as u64 * u64::from(CORE_DISCRIMINATOR_MAX)
        + other.len() as u64 * u64::from(OTHER_DISCRIMINATOR_MAX);
    println!(
        "nicknames: {} core (×{CORE_DISCRIMINATOR_MAX}) + {} other (×{OTHER_DISCRIMINATOR_MAX}) \
         = {total} usernames to hash",
        core.len(),
        other.len()
    );

    let mut filter = BloomFilter::new(total, fp_rate);
    println!(
        "bloom: {} bits ({:.1} MB), {} hash functions, target fp rate {fp_rate:e}",
        filter.bits,
        filter.bytes_len() as f64 / 1_048_576.0,
        filter.hashes
    );

    let started = std::time::Instant::now();
    let mut inserted = 0u64;
    let mut skipped = 0u64;
    for (terms, max) in [
        (&core, CORE_DISCRIMINATOR_MAX),
        (&other, OTHER_DISCRIMINATOR_MAX),
    ] {
        for nickname in terms {
            for d in 1..=max {
                // Zero-pad to two digits, exactly as `Username::format_parts` does
                // (`libsignal/rust/usernames/src/username.rs:171`). A bare "1" is not a legal
                // discriminator at all, so enumerating `d.to_string()` would have produced a
                // filter full of nothing for the whole 1..=9 range.
                match Username::from_parts(nickname, &format!("{d:0>2}"), NicknameLimits::default())
                {
                    Ok(username) => {
                        filter.insert(&username.hash());
                        inserted += 1;
                    }
                    // A nickname shorter than the 3-character minimum, or any other rejection:
                    // the username could not exist, so it needs no entry.
                    Err(_) => skipped += 1,
                }
            }
        }
    }
    println!(
        "hashed {inserted} usernames in {:.1}s ({skipped} impossible ones skipped)",
        started.elapsed().as_secs_f64()
    );

    for candidate in verify {
        let username = Username::new(candidate).map_err(|e| format!("{candidate}: {e}"))?;
        let present = filter.contains(&username.hash());
        println!(
            "verify {candidate}: {}",
            if present { "DENIED" } else { "allowed" }
        );
    }

    let encoded = filter.encode(envelope.version);
    std::fs::write(output, &encoded).map_err(|e| format!("writing {output}: {e}"))?;
    println!(
        "wrote {output} ({:.1} MB)",
        encoded.len() as f64 / 1_048_576.0
    );
    Ok(())
}

/// 2 GiB of bits: far above anything this lexicon will need, and small enough to allocate.
const MAX_FILTER_BITS: u64 = 8 * 2 * 1024 * 1024 * 1024;

/// A plain Bloom filter with the classic double-hashing construction, so the server side can be a
/// few dozen lines of Java rather than a dependency.
///
/// Layout of the encoded file, all little endian:
/// `"TLPB"` | u32 format version | u64 lexicon version | u64 bit count | u32 hash count | bits
struct BloomFilter {
    bits: u64,
    hashes: u32,
    data: Vec<u8>,
}

impl BloomFilter {
    fn new(expected: u64, fp_rate: f64) -> Self {
        // m = -n ln p / (ln 2)^2 and k = (m/n) ln 2, the standard sizing. Both results are clamped
        // into ranges that fit their target type before being converted, so the conversions below
        // cannot truncate; a nonsensical --fp-rate gives a small or large filter, never a panic.
        let n = expected.max(1) as f64;
        let bits_f = (-n * fp_rate.ln() / (std::f64::consts::LN_2 * std::f64::consts::LN_2))
            .ceil()
            .clamp(8.0, MAX_FILTER_BITS as f64);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "clamped to 8..=MAX_FILTER_BITS above"
        )]
        let bits = bits_f as u64;
        let hashes_f = ((bits_f / n) * std::f64::consts::LN_2)
            .round()
            .clamp(1.0, 64.0);
        #[allow(clippy::cast_possible_truncation, reason = "clamped to 1..=64 above")]
        let hashes = hashes_f as u32;
        let bytes = usize::try_from(bits.div_ceil(8)).expect("filter fits in memory");
        Self {
            bits,
            hashes,
            data: vec![0u8; bytes],
        }
    }

    fn bytes_len(&self) -> usize {
        self.data.len()
    }

    /// Derive all k positions from one SHA-256 of the username hash (Kirsch-Mitzenmacher).
    fn positions(&self, key: &[u8; 32]) -> impl Iterator<Item = u64> + '_ {
        let digest = Sha256::digest(key);
        let h1 = u64::from_le_bytes(digest[0..8].try_into().expect("32-byte digest"));
        let h2 = u64::from_le_bytes(digest[8..16].try_into().expect("32-byte digest")) | 1;
        (0..self.hashes).map(move |i| h1.wrapping_add(h2.wrapping_mul(u64::from(i))) % self.bits)
    }

    fn insert(&mut self, key: &[u8; 32]) {
        for pos in self.positions(key).collect::<Vec<_>>() {
            let (byte, bit) = Self::locate(pos);
            self.data[byte] |= bit;
        }
    }

    fn contains(&self, key: &[u8; 32]) -> bool {
        self.positions(key).all(|pos| {
            let (byte, bit) = Self::locate(pos);
            self.data[byte] & bit != 0
        })
    }

    fn locate(pos: u64) -> (usize, u8) {
        let byte = usize::try_from(pos / 8).expect("position is inside the allocated filter");
        (byte, 1u8 << (pos % 8))
    }

    fn encode(&self, lexicon_version: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() + 32);
        out.extend_from_slice(b"TLPB");
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&lexicon_version.to_le_bytes());
        out.extend_from_slice(&self.bits.to_le_bytes());
        out.extend_from_slice(&self.hashes.to_le_bytes());
        out.extend_from_slice(&self.data);
        out
    }
}
