//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The Rust half of the link registry build (links/README.md §8; ADR-0063 appendix A item 3).
//!
//! `scripts/links/build.py` stage 1 lints the sources in Python; before stage 2 writes
//! `links/dist/links-<version>.json`, this binary checks the same sources **with the client's own
//! code**: the registry is loaded exactly as a phone loads it (every §6.4 rule, regexes compiled by
//! the runtime `regex` engine), every corpus sample runs through the real Matcher, and every bad
//! sample must be refused or downgraded. A registry that passes the Python lint but that this
//! crate reads differently fails here — "the build passed, the phone does not match" is the drift
//! the two-stage build exists to stop.
//!
//!     cargo run -p tellomi-links --features build-tools --bin links-build -- links
//!     cargo run -p tellomi-links --features build-tools --bin links-build -- links \
//!         --dist links/dist/links-2026092702.json
//!
//! `--dist` additionally loads the written envelope and requires its providers to be exactly the
//! ones built from the sources. Exit status 0 = the build may proceed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Value, json};
use tellomi_links::{LinksPayload, LoadError, Registry, is_valid_preview_url};

/// `relative path → TOML text` for every `*.toml` under `dir`, relative to `root`.
fn collect(dir: &Path, root: &Path, out: &mut BTreeMap<String, String>) -> Result<(), String> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(&path, root, out)?;
        } else if path.extension().is_some_and(|e| e == "toml") {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .into_owned();
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            out.insert(rel, text);
        }
    }
    Ok(())
}

fn toml_value(where_: &str, text: &str) -> Result<Value, String> {
    toml::from_str::<Value>(text).map_err(|e| format!("{where_}: {e}"))
}

/// Provider files as JSON values, keyed by their path under the registry root.
fn providers(files: &BTreeMap<String, String>) -> Result<Vec<Value>, String> {
    files
        .iter()
        .filter(|(rel, _)| rel.starts_with("providers/"))
        .map(|(rel, text)| toml_value(rel, text))
        .collect()
}

fn load(providers: Vec<Value>, version: u64) -> Result<Registry, LoadError> {
    Registry::from_payload(
        version,
        LinksPayload {
            providers,
            popular_domains: Vec::new(),
        },
    )
}

#[derive(Default)]
struct Report {
    errors: Vec<String>,
    providers: usize,
    routes: usize,
    samples: usize,
    bad_refused: usize,
    bad_downgraded: usize,
    bad_build_only: usize,
}

impl Report {
    fn error(&mut self, e: impl Into<String>) {
        self.errors.push(e.into());
    }
}

/// Every sample in `tests/corpus/*.toml` must land exactly where its `expect` says.
fn run_corpus(registry: &Registry, root: &Path, report: &mut Report) -> Result<(), String> {
    let mut files = BTreeMap::new();
    let dir = root.join("tests/corpus");
    collect(&dir, &dir, &mut files)?;
    for (rel, text) in &files {
        let doc = toml_value(rel, text)?;
        let provider = doc["provider"].as_str().unwrap_or_default();
        for (i, case) in doc["case"].as_array().into_iter().flatten().enumerate() {
            report.samples += 1;
            let Some(url) = case["url"].as_str() else {
                report.error(format!("corpus {rel}[{i}]: no url"));
                continue;
            };
            let at = format!("corpus {provider}[{i}] {url}");
            let location = case["location"].as_bool().unwrap_or(false);
            let expect = &case["expect"];
            let Some(got) = registry.identify(url, location) else {
                report.error(format!("{at}: no provider"));
                continue;
            };
            let mut check = |what: &str, have: Option<&str>, want: Option<&str>| {
                if have != want {
                    report.error(format!("{at}: {what} = {have:?}, expected {want:?}"));
                }
            };
            check("provider", Some(&got.provider), Some(provider));
            let short = expect["short"].as_bool().unwrap_or(false).to_string();
            check("short", Some(&got.short.to_string()), Some(&short));
            if let Some(route) = expect.get("route").and_then(Value::as_str) {
                let want = (!route.is_empty()).then_some(route);
                check("route", got.route.as_deref(), want);
            }
            for key in ["kind", "object", "canonical_url", "fallback"] {
                if let Some(want) = expect.get(key).and_then(Value::as_str) {
                    let have = match key {
                        "kind" => got.kind.as_deref(),
                        "object" => got.object.as_deref(),
                        "canonical_url" => got.canonical_url.as_deref(),
                        _ => got.fallback.as_deref(),
                    };
                    check(key, have, Some(want));
                }
            }
            for (k, v) in expect["captures"].as_object().into_iter().flatten() {
                check(
                    &format!("capture {k}"),
                    got.captures.get(k).map(String::as_str),
                    v.as_str(),
                );
            }
            if !location
                && !case["synthetic"].as_bool().unwrap_or(false)
                && !is_valid_preview_url(url)
            {
                report.error(format!("{at}: not a URL Signal would preview"));
            }
        }
    }
    Ok(())
}

/// Every `tests/bad/<case>/` is a tampered update. Whatever lint says about it, this client must
/// not take it as written: refused as a whole, or (a compatibility rule) with an overlaid
/// provider downgraded. `icon-*` cases only touch the icon register, which clients never load.
fn run_bad_samples(
    root: &Path,
    base: &BTreeMap<String, String>,
    report: &mut Report,
) -> Result<(), String> {
    let bad = root.join("tests/bad");
    if !bad.is_dir() {
        return Ok(());
    }
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&bad)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    cases.sort();
    for dir in cases {
        let name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let spec = toml_value(
            &format!("{name}/case.toml"),
            &std::fs::read_to_string(dir.join("case.toml")).map_err(|e| e.to_string())?,
        )?;
        let mut tree = base.clone();
        for rel in spec["remove"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            tree.remove(rel);
        }
        let mut overlay = BTreeMap::new();
        if dir.join("providers").is_dir() {
            collect(&dir.join("providers"), &dir, &mut overlay)?;
        }
        if overlay.is_empty()
            && spec["remove"].as_array().is_none_or(|r| {
                r.iter()
                    .filter_map(Value::as_str)
                    .all(|p| !p.starts_with("providers/"))
            })
        {
            report.bad_build_only += 1; // e.g. icon-*: nothing a client loads changes
            continue;
        }
        let overlaid: Vec<String> = overlay
            .values()
            .filter_map(|t| toml_value(&name, t).ok())
            .filter_map(|v| v["id"].as_str().map(str::to_owned))
            .collect();
        tree.extend(overlay);
        let providers = match providers(&tree) {
            Ok(p) => p,
            Err(_) => {
                report.bad_refused += 1; // not even TOML a builder would emit
                continue;
            }
        };
        match load(providers, 0) {
            Err(_) => report.bad_refused += 1,
            Ok(registry)
                if registry
                    .degraded_routes()
                    .iter()
                    .any(|d| overlaid.contains(&d.provider)) =>
            {
                report.bad_downgraded += 1
            }
            Ok(_) => report.error(format!(
                "bad sample {name}: this client would accept it unchanged"
            )),
        }
    }
    Ok(())
}

fn build(root: &Path, dist: Option<&Path>) -> Result<Report, String> {
    let mut report = Report::default();
    let mut tree = BTreeMap::new();
    collect(&root.join("providers"), root, &mut tree)?;
    let kinds = toml_value(
        "kinds.toml",
        &std::fs::read_to_string(root.join("kinds.toml")).map_err(|e| e.to_string())?,
    )?;
    if kinds["schema"] != json!(tellomi_links::KINDS_SCHEMA) {
        report.error(format!(
            "kinds.toml schema {} is not the {} this crate renders",
            kinds["schema"],
            tellomi_links::KINDS_SCHEMA
        ));
    }
    let providers = providers(&tree)?;
    let registry = match load(providers.clone(), 0) {
        Ok(r) => r,
        Err(e) => {
            for v in e.violations() {
                report.error(format!("load: {v}"));
            }
            if e.violations().is_empty() {
                report.error(format!("load: {e}"));
            }
            return Ok(report);
        }
    };
    report.providers = registry.provider_count();
    report.routes = registry.route_count();
    // The newest client degrading anything means the registry uses something no client renders.
    for d in registry.degraded_routes() {
        report.error(format!(
            "degraded by this client: {}.{}: {}",
            d.provider, d.route, d.reason
        ));
    }
    run_corpus(&registry, root, &mut report)?;
    run_bad_samples(root, &tree, &mut report)?;

    if let Some(dist) = dist {
        let bytes = std::fs::read(dist).map_err(|e| format!("{}: {e}", dist.display()))?;
        match Registry::load(&bytes) {
            Err(e) => report.error(format!("{}: {e}", dist.display())),
            Ok(_) => {
                let env: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                let mut built = providers;
                built.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
                if env["payload"]["providers"] != Value::Array(built) {
                    report.error(format!(
                        "{}: payload.providers differ from the sources",
                        dist.display()
                    ));
                }
                if env["payload"]["kinds"] != kinds {
                    report.error(format!(
                        "{}: payload.kinds differs from kinds.toml",
                        dist.display()
                    ));
                }
            }
        }
    }
    Ok(report)
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next().map(PathBuf::from) else {
        eprintln!("usage: links-build <links registry root> [--dist <links-<version>.json>]");
        return ExitCode::FAILURE;
    };
    let mut dist = None;
    while let Some(flag) = args.next() {
        match (flag.as_str(), args.next()) {
            ("--dist", Some(p)) => dist = Some(PathBuf::from(p)),
            _ => {
                eprintln!("unknown or incomplete argument: {flag}");
                return ExitCode::FAILURE;
            }
        }
    }
    let report = match build(&root, dist.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("links-build: {e}");
            return ExitCode::FAILURE;
        }
    };
    for e in &report.errors {
        println!("ERROR {e}");
    }
    println!(
        "links-build: providers {} · routes {} · corpus {} · bad samples refused {} / downgraded {} / build-only {} · errors {}",
        report.providers,
        report.routes,
        report.samples,
        report.bad_refused,
        report.bad_downgraded,
        report.bad_build_only,
        report.errors.len()
    );
    if report.errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/registry")
    }

    #[test]
    fn the_fixture_registry_builds() {
        let dist = fixture().join("dist/links-2026092702.json");
        let report = build(&fixture(), Some(&dist)).expect("runs");
        assert!(report.errors.is_empty(), "{:#?}", report.errors);
        assert_eq!(
            (report.providers, report.routes, report.samples),
            (30, 74, 227)
        );
        assert_eq!(
            report.bad_refused + report.bad_downgraded + report.bad_build_only,
            20
        );
        assert_eq!(report.bad_downgraded, 1, "hot-l24 is a compatibility drop");
        assert_eq!(report.bad_build_only, 3, "the icon-* cases");
    }

    #[test]
    fn a_sample_the_real_matcher_disagrees_with_fails_the_build() {
        let tmp = std::env::temp_dir().join(format!("links-build-test-{}", std::process::id()));
        let copy = |from: &Path, to: &Path| {
            for entry in walk(from) {
                let rel = entry.strip_prefix(from).unwrap();
                std::fs::create_dir_all(to.join(rel).parent().unwrap()).unwrap();
                std::fs::copy(&entry, to.join(rel)).unwrap();
            }
        };
        fn walk(dir: &Path) -> Vec<PathBuf> {
            let mut out = Vec::new();
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    out.extend(walk(&p));
                } else {
                    out.push(p);
                }
            }
            out
        }
        copy(&fixture(), &tmp);
        // The regex a Python `re` reads one way and `regex` another would show up exactly like this:
        // a sample expecting a route this engine does not give.
        let corpus = tmp.join("tests/corpus/bilibili.toml");
        let text = std::fs::read_to_string(&corpus).unwrap();
        std::fs::write(
            &corpus,
            text.replacen("route = \"video\"", "route = \"video-av\"", 1),
        )
        .unwrap();
        let report = build(&tmp, None).expect("runs");
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.contains("route = Some(\"video\"), expected Some(\"video-av\")")),
            "{:#?}",
            report.errors
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
