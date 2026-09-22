//
// Copyright (C) 2026 Tellomi.
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Stage 2 of the policy lexicon build (ADR-0062 §5.2).
//!
//! Stage 1 (`scripts/policy/build.py`) collects rules from the lexicon sources and writes them
//! with their raw terms. This binary fills in `normalized` and `skeleton` **using the engine's own
//! normalization**, validates the result by loading it exactly as a client would, and then runs the
//! corpus: a list of ordinary names that must stay allowed, and a list that must be refused.
//!
//! Normalization therefore exists once, in `normalize.rs`. A second implementation in the build
//! script would drift, and the drift would only show up as a name that the build accepts and a
//! phone rejects.
//!
//!     cargo run -p tellomi-policy --bin policy-build -- \
//!         policy/dist/policy-source.json policy/dist/policy-2026092201.json \
//!         --must-allow policy/tests/corpus/must-allow.txt \
//!         --must-deny  policy/tests/corpus/must-deny.txt

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::Value;
use tellomi_policy::{Field, PolicyEngine, Region, normalize};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        eprintln!(
            "usage: policy-build <source.json> <out.json> \
             [--must-allow <file>] [--must-deny <file>]"
        );
        return ExitCode::FAILURE;
    };

    let mut must_allow: Option<PathBuf> = None;
    let mut must_deny: Option<PathBuf> = None;
    while let Some(flag) = args.next() {
        match (flag.as_str(), args.next()) {
            ("--must-allow", Some(p)) => must_allow = Some(p.into()),
            ("--must-deny", Some(p)) => must_deny = Some(p.into()),
            (other, _) => {
                eprintln!("unknown argument {other}");
                return ExitCode::FAILURE;
            }
        }
    }

    match run(&input, &output, must_allow, must_deny) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("policy-build failed: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(
    input: &str,
    output: &str,
    must_allow: Option<PathBuf>,
    must_deny: Option<PathBuf>,
) -> Result<(), String> {
    let raw = std::fs::read(input).map_err(|e| format!("reading {input}: {e}"))?;
    let mut doc: Value =
        serde_json::from_slice(&raw).map_err(|e| format!("parsing {input}: {e}"))?;

    let rules = doc
        .pointer_mut("/payload/rules")
        .and_then(Value::as_array_mut)
        .ok_or("source has no payload.rules array")?;

    let mut normalized_collisions: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for rule in rules.iter_mut() {
        let term = rule["term"]
            .as_str()
            .ok_or("a rule has no term")?
            .to_string();
        let id = rule["id"].as_str().unwrap_or("?").to_string();

        if normalize::has_disallowed_control(&term) {
            return Err(format!("{id}: term contains a control or format character"));
        }
        let normalized = normalize::normalize(&term);
        if normalized.is_empty() {
            return Err(format!("{id}: term normalizes to nothing"));
        }

        let wants_confusable = rule["matches"]
            .as_array()
            .is_some_and(|m| m.iter().any(|v| v.as_str() == Some("CONFUSABLE")));
        let skeleton = if wants_confusable {
            normalize::skeleton(&normalized)
        } else {
            String::new()
        };

        normalized_collisions
            .entry(normalized.clone())
            .or_default()
            .push(id);
        rule["normalized"] = Value::String(normalized);
        rule["skeleton"] = Value::String(skeleton);
    }

    // Two rules that normalize to the same string are not an error (one may be a username rule and
    // the other a display-name rule), but they are worth seeing: it usually means a term was
    // written twice in different spellings.
    let dupes: Vec<_> = normalized_collisions
        .iter()
        .filter(|(_, ids)| ids.len() > 1)
        .collect();
    for (normalized, ids) in &dupes {
        println!(
            "note: {} rules normalize to {normalized:?}: {}",
            ids.len(),
            ids.join(", ")
        );
    }

    if let Some(allow) = doc
        .pointer_mut("/payload/allow")
        .and_then(Value::as_array_mut)
    {
        for entry in allow.iter_mut() {
            let term = entry["term"]
                .as_str()
                .ok_or("an allow entry has no term")?
                .to_string();
            entry["normalized"] = Value::String(normalize::normalize(&term));
        }
    }

    doc["generated_by"] = Value::String(format!(
        "{} + policy-build (stage 2: normalization, validation, corpus)",
        doc["generated_by"].as_str().unwrap_or("stage 1")
    ));

    let serialized = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;

    // The real guarantee: load the file the way a client will, then run the corpus through it.
    let engine =
        PolicyEngine::load(&serialized).map_err(|e| format!("built lexicon is invalid: {e}"))?;
    println!(
        "loaded: {} rules, version {}",
        engine.rule_count(),
        engine.version()
    );

    let mut failures = 0usize;
    if let Some(path) = must_allow {
        failures += check_corpus(&engine, &path, true)?;
    }
    if let Some(path) = must_deny {
        failures += check_corpus(&engine, &path, false)?;
    }
    if failures > 0 {
        return Err(format!(
            "{failures} corpus expectations failed; lexicon not written"
        ));
    }

    std::fs::write(output, &serialized).map_err(|e| format!("writing {output}: {e}"))?;
    println!("wrote {output} ({} bytes)", serialized.len());
    Ok(())
}

/// Corpus line format: `<field> <regions> <input>`, e.g. `username global admin`.
/// Regions are comma separated. Blank lines and `#` comments are skipped.
fn check_corpus(
    engine: &PolicyEngine,
    path: &PathBuf,
    expect_allowed: bool,
) -> Result<usize, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let (mut checked, mut failed) = (0usize, 0usize);

    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(3, char::is_whitespace);
        let (Some(field), Some(regions), Some(input)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(format!(
                "{}:{}: expected `<field> <regions> <input>`",
                path.display(),
                lineno + 1
            ));
        };
        let field = parse_field(field)
            .ok_or_else(|| format!("{}:{}: unknown field {field}", path.display(), lineno + 1))?;
        let regions: Vec<Region> = regions
            .split(',')
            .map(|r| parse_region(r).ok_or_else(|| format!("unknown region {r}")))
            .collect::<Result<_, _>>()?;

        let verdict = engine.check(input, field, &regions);
        checked += 1;
        if verdict.is_allowed() != expect_allowed {
            failed += 1;
            let what = if expect_allowed {
                "should have been allowed"
            } else {
                "should have been refused"
            };
            println!(
                "  FAIL {}:{} {input:?} ({field:?}) {what}, got {:?}{}",
                path.display(),
                lineno + 1,
                verdict.outcome,
                verdict
                    .hit
                    .map(|h| format!(" via {}", h.rule_id))
                    .unwrap_or_default()
            );
        }
    }

    let label = if expect_allowed {
        "must-allow"
    } else {
        "must-deny"
    };
    println!("{label}: {checked} checked, {failed} failed");
    Ok(failed)
}

fn parse_field(s: &str) -> Option<Field> {
    serde_json::from_value(Value::String(s.to_string())).ok()
}

fn parse_region(s: &str) -> Option<Region> {
    serde_json::from_value(Value::String(s.to_string())).ok()
}
