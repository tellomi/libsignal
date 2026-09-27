//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Test support: read the registry fixtures in their source TOML form and build the envelope the
//! way the builder will (`links-<version>.json`, name = "links", schema 1), all offline.

#![allow(dead_code)] // each test binary uses a different subset

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tellomi_links::Registry;

pub fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

pub fn registry_dir() -> PathBuf {
    data_dir().join("registry")
}

/// `providers/**/*.toml` of a registry tree: relative path → TOML text.
pub type Tree = BTreeMap<String, String>;

fn walk(dir: &Path, root: &Path, out: &mut Tree) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.expect("entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk(&path, root, out);
        } else if path.extension().is_some_and(|e| e == "toml") {
            let rel = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .into_owned();
            out.insert(rel, std::fs::read_to_string(&path).expect("utf-8"));
        }
    }
}

/// The base registry's provider files.
pub fn base_tree() -> Tree {
    let root = registry_dir();
    let mut tree = Tree::new();
    walk(&root.join("providers"), &root, &mut tree);
    tree
}

pub fn toml_value(text: &str) -> Value {
    toml::from_str::<Value>(text).expect("fixture TOML parses")
}

/// Envelope bytes for a provider tree, as `links-build` will write them.
pub fn envelope(tree: &Tree, version: u64) -> Vec<u8> {
    envelope_with(
        tree.values().map(|t| toml_value(t)).collect(),
        version,
        json!([]),
    )
}

pub fn envelope_with(providers: Vec<Value>, version: u64, popular: Value) -> Vec<u8> {
    let kinds = toml_value(
        &std::fs::read_to_string(registry_dir().join("kinds.toml")).expect("kinds.toml"),
    );
    serde_json::to_vec(&json!({
        "name": "links",
        "version": version,
        "schema": 1,
        "generated_by": "tellomi-links tests",
        "inputs": [],
        "payload": { "providers": providers, "kinds": kinds, "popular_domains": popular }
    }))
    .expect("serializes")
}

pub fn base_registry() -> Registry {
    Registry::load(&envelope(&base_tree(), 2026092701)).expect("the draft registry loads cleanly")
}

/// Provider values of the base tree, by id, for mutation tests.
pub fn base_providers() -> BTreeMap<String, Value> {
    base_tree()
        .values()
        .map(|t| {
            let v = toml_value(t);
            (v["id"].as_str().expect("id").to_owned(), v)
        })
        .collect()
}

pub fn load_providers(
    providers: &BTreeMap<String, Value>,
) -> Result<Registry, tellomi_links::LoadError> {
    Registry::load(&envelope_with(
        providers.values().cloned().collect(),
        2026092702,
        json!([]),
    ))
}

pub fn read(path: &str) -> Vec<u8> {
    std::fs::read(data_dir().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}
