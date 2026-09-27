//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Hot updates (links/README.md §8, ADR-0063 §5.4 / §6.4 / §7.3): signature → name → schema range
//! → version only increases → content rules, in that order.

mod common;

use common::*;
use libsignal_core::curve::KeyPair;
use rand::TryRngCore as _;
use rand::rngs::OsRng;
use serde_json::{Value, json};
use tellomi_links::{LoadError, Registry, verify_registry_signature};

/// The libsignal (node) known answer from `scripts/links/test_build.py`: a throwaway key, not a
/// release key. It proves this crate verifies exactly what the build's `xeddsa.py` and Desktop's
/// updater produce.
const VECTOR_PUB: &str = "05b6c7b57b26ffe0d0740d48bb92c1f2ba2b5d4901528e5f7adf89eefc3c12d00e";
const VECTOR_MSG: &str = r#"{"name":"links","version":2026092701}"#;
const VECTOR_SIG: &str = "5938f55fcf332f4408f851fcdcca380d406d9320cb963c2b59066b5c4d586096\
                          cd18061ba8eb19597778b83ffa453ce3084c99ae7be08bdaedf494324d75b08d";

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

struct Signer(KeyPair);

impl Signer {
    fn new() -> Self {
        Signer(KeyPair::generate(&mut OsRng.unwrap_err()))
    }

    fn public(&self) -> Vec<u8> {
        self.0.public_key.serialize().to_vec()
    }

    fn sign(&self, bytes: &[u8]) -> String {
        // A `.sig` file: hex and a trailing newline, as Desktop writes them.
        let sig = self
            .0
            .private_key
            .calculate_signature(bytes, &mut OsRng.unwrap_err())
            .expect("signs");
        format!("{}\n", hex(&sig))
    }
}

fn dist() -> Vec<u8> {
    read("registry/dist/links-2026092702.json")
}

fn with(bytes: &[u8], f: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut v: Value = serde_json::from_slice(bytes).expect("json");
    f(&mut v);
    serde_json::to_vec_pretty(&v).expect("json")
}

#[test]
fn libsignal_known_answer() {
    let public = unhex(VECTOR_PUB);
    assert!(verify_registry_signature(
        VECTOR_MSG.as_bytes(),
        VECTOR_SIG,
        &public
    ));
    assert!(verify_registry_signature(
        VECTOR_MSG.as_bytes(),
        &format!("{VECTOR_SIG}\n"),
        &public
    ));
    // The raw 32-byte form of the same key is accepted too.
    assert!(verify_registry_signature(
        VECTOR_MSG.as_bytes(),
        VECTOR_SIG,
        &public[1..]
    ));
    assert!(!verify_registry_signature(
        format!("{VECTOR_MSG} ").as_bytes(),
        VECTOR_SIG,
        &public
    ));
    assert!(!verify_registry_signature(
        VECTOR_MSG.as_bytes(),
        &VECTOR_SIG[2..],
        &public
    ));
    assert!(!verify_registry_signature(
        VECTOR_MSG.as_bytes(),
        &VECTOR_SIG.replace('5', "g"),
        &public
    ));
    assert!(!verify_registry_signature(
        VECTOR_MSG.as_bytes(),
        VECTOR_SIG,
        &public[..20]
    ));
}

#[test]
fn a_signed_newer_update_loads() {
    let signer = Signer::new();
    let bytes = dist();
    let sig = signer.sign(&bytes);
    let registry =
        Registry::load_update(&bytes, &sig, &signer.public(), Some(2026092612)).expect("loads");
    assert_eq!(registry.version(), 2026092702);
    Registry::load_update(&bytes, &sig, &signer.public(), None).expect("no current version yet");
}

#[test]
fn verification_order_and_every_refusal() {
    let signer = Signer::new();
    let bytes = dist();
    let sig = signer.sign(&bytes);
    let key = signer.public();

    // 1. Signature over the exact bytes: one flipped byte, another key, a malformed .sig.
    let mut tampered = bytes.clone();
    let at = tampered.len() / 2;
    tampered[at] ^= 1;
    assert!(matches!(
        Registry::load_update(&tampered, &sig, &key, None),
        Err(LoadError::BadSignature)
    ));
    let other = Signer::new();
    assert!(matches!(
        Registry::load_update(&bytes, &sig, &other.public(), None),
        Err(LoadError::BadSignature)
    ));
    assert!(matches!(
        Registry::load_update(&bytes, "zz", &key, None),
        Err(LoadError::BadSignature)
    ));
    // Even a re-serialisation of the same JSON is a different file: bytes are signed, not values.
    let reformatted = with(&bytes, |_| {});
    assert!(matches!(
        Registry::load_update(&reformatted, &sig, &key, None),
        Err(LoadError::BadSignature)
    ));

    // 2. / 3. Name and schema — checked after the signature.
    let wrong_name = with(&bytes, |v| v["name"] = json!("policy"));
    assert!(matches!(
        Registry::load_update(&wrong_name, &signer.sign(&wrong_name), &key, None),
        Err(LoadError::Envelope(_))
    ));
    let future = with(&bytes, |v| v["schema"] = json!(2));
    assert!(matches!(
        Registry::load_update(&future, &signer.sign(&future), &key, None),
        Err(LoadError::Envelope(_))
    ));

    // 4. Version only increases: equal (a replay) and older (a rollback) are both refused.
    for current in [2026092702, 2026092703] {
        assert!(matches!(
            Registry::load_update(&bytes, &sig, &key, Some(current)),
            Err(LoadError::NotNewer {
                found: 2026092702,
                ..
            })
        ));
    }

    // 5. A correctly signed file is still held to the content rules (§6.4: the builder is not
    //    trusted either — a leaked key must not hand out first-party hosts).
    let grab = with(&bytes, |v| {
        let providers = v["payload"]["providers"].as_array_mut().expect("providers");
        let bilibili = providers
            .iter_mut()
            .find(|p| p["id"] == "bilibili")
            .expect("bilibili");
        bilibili["short_domains"] = json!(["b23.tv", "tell.cc"]);
    });
    let err =
        Registry::load_update(&grab, &signer.sign(&grab), &key, None).expect_err("tell.cc grab");
    assert!(err.violations().iter().any(|v| v.rule == "L23"), "{err}");
}
