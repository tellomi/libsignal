//
// Copyright (C) 2026 Tellomi.
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Stage 3 of the policy build: the server-side username denylist (ADR-0062 §5.4, as revised by
//! ADR-0066).
//!
//! The server never sees a username in the clear — the client sends `Username::hash()` and the
//! server stores that. So the only way for the server to refuse `admin.01` is to know the hash of
//! `admin.01` in advance. This binary enumerates `<reserved nickname>.<discriminator>` for every
//! reserved nickname and every discriminator from `01` to `99`, hashes each one with libsignal's
//! own function, and writes the hashes, sorted.
//!
//! Why `01`–`99`: ADR-0066 fixes the discriminator at `01` and hides it in the UI, so `01` is the
//! one every stock client produces. `02`–`99` are covered as well because `admin.12` is exactly
//! the shape upstream's default candidates had, and it still reads like an official account.
//! Three or more digits are left alone: official clients show any discriminator other than `01`
//! in full, so `tellomi.123` does not pass for `tellomi`.
//!
//! Why an exact list rather than the Bloom filter this binary used to write: with the range down
//! from 1–9999 to 01–99 the list is a few tens of thousands of hashes (about 1.4 MB for the first
//! lexicon), small enough for the server to hold as a plain set. No false positives, nothing to
//! tune.
//!
//! Layout of the output file, integers little endian:
//!
//! ```text
//! "TLPH" | u32 format version (1) | u64 lexicon version | u32 highest discriminator (99)
//!        | u32 count | [u8; 32] SHA-256 of the body | body: count × [u8; 32], sorted, unique
//! ```
//!
//! The server recomputes the SHA-256 when it loads the file and refuses to start on a mismatch.
//!
//!     cargo run -p tellomi-policy --features build-tools --bin policy-username-hashes -- \
//!         policy/dist/policy-2026092201.json policy/dist/username-hash-denylist-2026092201.bin

use std::collections::BTreeSet;
use std::process::ExitCode;

use sha2::{Digest, Sha256};
use tellomi_policy::{Envelope, Field, LexiconPayload, Match, Outcome, normalize};
use usernames::{NicknameLimits, Username};

/// Discriminators `01..=HIGHEST_DISCRIMINATOR` are enumerated for every reserved nickname.
const HIGHEST_DISCRIMINATOR: u32 = 99;

const MAGIC: &[u8; 4] = b"TLPH";
const FORMAT_VERSION: u32 = 1;
/// magic + format version + lexicon version + highest discriminator + count + body digest
const HEADER_LEN: usize = 4 + 4 + 8 + 4 + 4 + 32;

/// Bounded cross product of brand-protected × impersonation affix terms, both orders, joined by
/// `_` — the only character the username grammar (`[a-z][a-z0-9_]{1,31}`) allows that also forms
/// a PREFIX/SUFFIX match boundary. `match_affix` (`engine.rs`) requires the character immediately
/// outside the term to be non-alphanumeric or the string to end there; letters and digits never
/// qualify, so a bare concatenation like `tellomisupport` has no boundary anywhere in the middle
/// and the client engine would never flag it as a PREFIX/SUFFIX hit. Generating it here would only
/// add entries nothing can ever be denied against, so `_` is the only join.
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
            "usage: policy-username-hashes <policy-<version>.json> <out.bin> \
             [--verify <nickname.discriminator>]..."
        );
        return ExitCode::FAILURE;
    };
    let mut verify: Vec<String> = Vec::new();
    let mut rest = args[2..].iter();
    while let Some(flag) = rest.next() {
        match (flag.as_str(), rest.next()) {
            ("--verify", Some(v)) => verify.push(v.clone()),
            ("--fp-rate", _) => {
                eprintln!("--fp-rate is gone: the output is an exact list now, not a Bloom filter");
                return ExitCode::FAILURE;
            }
            (other, _) => {
                eprintln!("unknown argument {other}");
                return ExitCode::FAILURE;
            }
        }
    }

    match run(input, output, &verify) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("policy-username-hashes failed: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(input: &str, output: &str, verify: &[String]) -> Result<(), String> {
    let raw = std::fs::read(input).map_err(|e| format!("reading {input}: {e}"))?;
    let envelope: Envelope<LexiconPayload> =
        serde_json::from_slice(&raw).map_err(|e| format!("parsing {input}: {e}"))?;

    // Which nicknames to enumerate: every rule that applies to usernames by an equality- or
    // affix-shaped match. CONTAINS and CONFUSABLE are deliberately excluded — enumerating every
    // string that *contains* a term, or every visual variant of it, is unbounded. The client-side
    // engine covers those; see the ADR on what this list does and does not promise.
    //
    // PREFIX and SUFFIX are a narrower case of the same unboundedness: `tellomi*` matches any
    // string that *starts* with `tellomi`, which is just as unenumerable as CONTAINS in general.
    // Inserting only the bare term (as the loop below does) gives it zero actual PREFIX/SUFFIX
    // coverage — it is fully subsumed by the NORMALIZED_EXACT entry for the same term, so an
    // affix-tagged term contributes nothing beyond what an exact-only term would have.
    //
    // The one case worth closing: a PREFIX/SUFFIX brand term combined with a PREFIX/SUFFIX
    // impersonation term — `tellomi_staff`, `support_tellomi` — is exactly the shape
    // `tellomi/impersonation.toml`'s own comment calls "真正的冒充" (the real impersonation
    // shape), and it *is* bounded: brand terms × role terms is a small cross product. See
    // `derive_affix_combinations` for why only `_` is a valid separator.
    let mut nicknames: BTreeSet<String> = BTreeSet::new();
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
        if !enumerable || !normalize::is_username_safe(&rule.normalized) {
            continue;
        }
        nicknames.insert(rule.normalized.clone());
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
    nicknames.extend(derived);

    let started = std::time::Instant::now();
    let mut hashes: Vec<[u8; 32]> = Vec::new();
    let mut skipped = 0u64;
    for nickname in &nicknames {
        for d in 1..=HIGHEST_DISCRIMINATOR {
            // Zero-padded to two digits, exactly as `Username::format_parts` does
            // (`libsignal/rust/usernames/src/username.rs`): a bare "1" is not a legal
            // discriminator at all, while "01" is — and "01" is the one ADR-0066 fixes.
            match Username::from_parts(nickname, &format!("{d:0>2}"), NicknameLimits::default()) {
                Ok(username) => hashes.push(username.hash()),
                // A nickname shorter than the 3-character minimum, or any other rejection: the
                // username could not exist, so it needs no entry.
                Err(_) => skipped += 1,
            }
        }
    }
    hashes.sort_unstable();
    hashes.dedup();
    println!(
        "{} nicknames × 01–{HIGHEST_DISCRIMINATOR} → {} hashes in {:.1}s \
         ({skipped} impossible usernames skipped)",
        nicknames.len(),
        hashes.len(),
        started.elapsed().as_secs_f64()
    );

    for candidate in verify {
        let username = Username::new(candidate).map_err(|e| format!("{candidate}: {e}"))?;
        let denied = hashes.binary_search(&username.hash()).is_ok();
        println!(
            "verify {candidate}: {}",
            if denied { "DENIED" } else { "allowed" }
        );
    }

    let encoded = encode(envelope.version, &hashes)?;
    // Read our own output back through the decoder the server's loader mirrors, so the layout
    // documented above and the bytes on disk cannot drift apart.
    let decoded = decode(&encoded)?;
    if decoded != hashes {
        return Err("round trip through decode() changed the list".to_owned());
    }
    std::fs::write(output, &encoded).map_err(|e| format!("writing {output}: {e}"))?;
    println!(
        "wrote {output} ({:.2} MB, body sha256 {})",
        encoded.len() as f64 / 1_048_576.0,
        hex(&encoded[HEADER_LEN - 32..HEADER_LEN])
    );
    Ok(())
}

fn encode(lexicon_version: u64, hashes: &[[u8; 32]]) -> Result<Vec<u8>, String> {
    let count = u32::try_from(hashes.len()).map_err(|_| "more than u32::MAX hashes".to_owned())?;
    let body: Vec<u8> = hashes.concat();
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&lexicon_version.to_le_bytes());
    out.extend_from_slice(&HIGHEST_DISCRIMINATOR.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&Sha256::digest(&body));
    out.extend_from_slice(&body);
    Ok(out)
}

/// The same checks the server's loader makes, in the same order.
fn decode(bytes: &[u8]) -> Result<Vec<[u8; 32]>, String> {
    let header = bytes.get(..HEADER_LEN).ok_or("shorter than the header")?;
    if &header[0..4] != MAGIC {
        return Err("bad magic".to_owned());
    }
    let format = u32::from_le_bytes(header[4..8].try_into().expect("4 bytes"));
    if format != FORMAT_VERSION {
        return Err(format!("unknown format version {format}"));
    }
    let count = u32::from_le_bytes(header[20..24].try_into().expect("4 bytes"));
    let body = &bytes[HEADER_LEN..];
    let expected_len = usize::try_from(count)
        .ok()
        .and_then(|c| c.checked_mul(32))
        .ok_or("count × 32 overflows usize")?;
    if body.len() != expected_len {
        return Err(format!(
            "body is {} bytes, header says {count} × 32",
            body.len()
        ));
    }
    if Sha256::digest(body).as_slice() != &header[24..56] {
        return Err("body does not match the SHA-256 in the header".to_owned());
    }
    let hashes: Vec<[u8; 32]> = body
        .chunks_exact(32)
        .map(|c| c.try_into().expect("32-byte chunk"))
        .collect();
    if hashes.windows(2).any(|w| w[0] >= w[1]) {
        return Err("hashes are not strictly ascending".to_owned());
    }
    Ok(hashes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
