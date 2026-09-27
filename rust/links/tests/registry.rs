//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Registry loading (ADR-0063 §6.4, appendix A): the draft registry loads, every corpus sample
//! lands on the provider and kind it declares, every bad hot update is refused for the rule it
//! breaks, and compatibility problems degrade one route instead of the whole update.

mod common;

use std::collections::BTreeSet;

use common::*;
use serde_json::{Value, json};
use tellomi_links::{KINDS, LoadError, Registry, is_valid_preview_url};

#[test]
fn draft_registry_loads_cleanly() {
    let registry = base_registry();
    assert_eq!(registry.version(), 2026092701);
    // build.py's own count for this registry: providers 30 · routes 74.
    assert_eq!(
        (registry.provider_count(), registry.route_count()),
        (30, 74)
    );
    assert!(
        registry.degraded_routes().is_empty(),
        "{:?}",
        registry.degraded_routes()
    );
}

/// The build output itself (`links/dist/links-2026092701.json`, payload = {kinds, providers}).
#[test]
fn the_built_dist_envelope_loads() {
    let registry =
        Registry::load(&read("registry/dist/links-2026092701.json")).expect("dist loads");
    assert_eq!(registry.version(), 2026092701);
    assert_eq!(
        (registry.provider_count(), registry.route_count()),
        (30, 74)
    );
    assert!(registry.degraded_routes().is_empty());
}

#[test]
fn compiled_kinds_equal_the_registry_vocabulary() {
    let doc = toml_value(&String::from_utf8(read("registry/kinds.toml")).expect("utf-8"));
    assert_eq!(doc["schema"], json!(tellomi_links::KINDS_SCHEMA));
    let file_kinds = doc["kind"].as_array().expect("[[kind]]");
    assert_eq!(file_kinds.len(), KINDS.len());
    for (file, compiled) in file_kinds.iter().zip(KINDS) {
        assert_eq!(file["id"], json!(compiled.id));
        assert_eq!(
            file["first_party"].as_bool().unwrap_or(false),
            compiled.first_party,
            "{}",
            compiled.id
        );
        assert_eq!(
            file["reserved"].as_bool().unwrap_or(false),
            compiled.reserved,
            "{}",
            compiled.id
        );
        let required: Vec<&str> = file["required"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_str().expect("str")).collect())
            .unwrap_or_default();
        assert_eq!(required, compiled.required, "{}", compiled.id);
        let any: Vec<Vec<&str>> = file["required_any"]
            .as_array()
            .map(|groups| {
                groups
                    .iter()
                    .map(|g| {
                        g.as_array()
                            .expect("group")
                            .iter()
                            .map(|v| v.as_str().expect("str"))
                            .collect()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let compiled_any: Vec<Vec<&str>> =
            compiled.required_any.iter().map(|g| g.to_vec()).collect();
        assert_eq!(any, compiled_any, "{}", compiled.id);
        let attrs: BTreeSet<(String, String)> = file["attrs"]
            .as_object()
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().expect("type").to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let compiled_attrs: BTreeSet<(String, String)> = compiled
            .attrs
            .iter()
            .map(|(k, t)| ((*k).to_owned(), t.name().to_owned()))
            .collect();
        assert_eq!(attrs, compiled_attrs, "{}", compiled.id);
    }
    let types: BTreeSet<&str> = doc["attr_types"]
        .as_object()
        .expect("types")
        .keys()
        .map(String::as_str)
        .collect();
    for kind in KINDS {
        for (_, t) in kind.attrs {
            assert!(types.contains(t.name()), "{}", t.name());
        }
    }
}

/// Appendix A: every sample in `tests/corpus` must match exactly what it declares.
#[test]
fn every_corpus_sample_matches_its_declared_provider_and_kind() {
    let registry = base_registry();
    let dir = registry_dir().join("tests/corpus");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("corpus")
        .map(|e| e.expect("entry").path())
        .collect();
    files.sort();
    let mut checked = 0;
    let mut routes_seen = BTreeSet::new();
    for path in files {
        let doc = toml_value(&std::fs::read_to_string(&path).expect("corpus file"));
        let provider = doc["provider"].as_str().expect("provider");
        for case in doc["case"].as_array().expect("[[case]]") {
            let url = case["url"].as_str().expect("url");
            let location = case["location"].as_bool().unwrap_or(false);
            let expect = &case["expect"];
            let got = registry
                .identify(url, location)
                .unwrap_or_else(|| panic!("{url}: no provider"));
            assert_eq!(got.provider, provider, "{url}");
            assert_eq!(
                got.short,
                expect["short"].as_bool().unwrap_or(false),
                "{url}"
            );
            if let Some(route) = expect.get("route").and_then(Value::as_str) {
                assert_eq!(got.route.as_deref().unwrap_or(""), route, "{url}");
            }
            if let Some(kind) = expect.get("kind").and_then(Value::as_str) {
                assert_eq!(got.kind.as_deref(), Some(kind), "{url}");
            }
            if let Some(object) = expect.get("object").and_then(Value::as_str) {
                assert_eq!(got.object.as_deref(), Some(object), "{url}");
            }
            if let Some(caps) = expect.get("captures").and_then(Value::as_object) {
                for (k, v) in caps {
                    assert_eq!(
                        got.captures.get(k).map(String::as_str),
                        v.as_str(),
                        "{url} capture {k}"
                    );
                }
            }
            if let Some(canonical) = expect.get("canonical_url").and_then(Value::as_str) {
                assert_eq!(got.canonical_url.as_deref(), Some(canonical), "{url}");
            }
            if let Some(route) = &got.route {
                routes_seen.insert(format!("{provider}.{route}"));
            }
            // Third-party samples are real links, so they must also be previewable ones —
            // except the first Location of a short link, which is identification only.
            if !location && !case["synthetic"].as_bool().unwrap_or(false) {
                assert!(is_valid_preview_url(url), "{url}");
            }
            checked += 1;
        }
    }
    // build.py: corpus 227 条, and every route has at least one sample (L14).
    assert_eq!(checked, 227);
    assert_eq!(routes_seen.len(), 74, "{routes_seen:?}");
}

fn rules(err: &LoadError) -> BTreeSet<(&'static str, String)> {
    err.violations()
        .iter()
        .map(|v| (v.rule, v.provider.clone()))
        .collect()
}

/// `tests/bad/<case>/`: remove what `case.toml` says, overlay the rest, load. Each one is a
/// tampered hot update and must be refused as a whole, for (at least) the rules lint reports.
#[test]
fn every_bad_sample_is_refused_for_the_rule_it_breaks() {
    let bad_root = registry_dir().join("tests/bad");
    let mut cases: Vec<_> = std::fs::read_dir(&bad_root)
        .expect("bad")
        .map(|e| e.expect("entry").path())
        .collect();
    cases.sort();
    assert_eq!(cases.len(), 19);
    for dir in cases {
        let name = dir
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        if name.starts_with("icon-") {
            // L15 / L16 judge the bundled icon files and their licence register: build time only.
            // The provider data a client loads is unchanged by these overlays, so it still loads.
            Registry::load(&envelope(&base_tree(), 2026092799)).expect(&name);
            continue;
        }
        let spec = toml_value(&std::fs::read_to_string(dir.join("case.toml")).expect("case.toml"));
        let mut tree = base_tree();
        for rel in spec["remove"].as_array().into_iter().flatten() {
            assert!(
                tree.remove(rel.as_str().expect("path")).is_some(),
                "{name}: remove {rel}"
            );
        }
        let mut overlay = Tree::new();
        let providers = dir.join("providers");
        if providers.is_dir() {
            fn collect(d: &std::path::Path, root: &std::path::Path, out: &mut Tree) {
                for e in std::fs::read_dir(d).expect("dir") {
                    let p = e.expect("entry").path();
                    if p.is_dir() {
                        collect(&p, root, out);
                    } else {
                        let rel = p
                            .strip_prefix(root)
                            .expect("rel")
                            .to_string_lossy()
                            .into_owned();
                        out.insert(rel, std::fs::read_to_string(&p).expect("utf-8"));
                    }
                }
            }
            collect(&providers, &dir, &mut overlay);
        }
        tree.extend(overlay);
        let err = Registry::load(&envelope(&tree, 2026092799)).expect_err(&name);
        assert!(matches!(err, LoadError::Rejected(_)), "{name}: {err}");
        let got = rules(&err);
        if std::env::var("DUMP_BAD").is_ok() {
            eprintln!("== {name}");
            for v in err.violations() {
                eprintln!("   {v}");
            }
        }
        for expected in spec["expect"].as_array().expect("expect") {
            let expected = expected.as_str().expect("str");
            let mut words = expected.split_whitespace();
            let first = words.next().unwrap_or("");
            let rule = first
                .strip_prefix('L')
                .filter(|n| n.bytes().all(|b| b.is_ascii_digit()));
            let Some(_) = rule else {
                // Shape errors (a missing key) and corpus expectations: lint-only wording. Here
                // they surface as L1 shape errors or as the host-ownership rules; the load must
                // simply be refused, which it was.
                continue;
            };
            let who = words.next().unwrap_or("").trim_end_matches(':');
            if matches!(first, "L14" | "L19") {
                continue; // corpus and cross-repo checks run at build time only
            }
            if first == "L3" {
                // L3's message names a host, not a provider.
                assert!(
                    got.iter().any(|(r, _)| *r == "L3"),
                    "{name}: expected L3 in {got:?}"
                );
                continue;
            }
            let provider = who.split('.').next().unwrap_or(who);
            assert!(
                got.contains(&(leak(first), provider.to_owned())),
                "{name}: expected {first} for {provider} (lint: {expected}); got {got:?}"
            );
        }
    }
}

fn leak(s: &str) -> &'static str {
    // rule codes are a closed set; compare by value
    match s {
        "L1" => "L1",
        "L2" => "L2",
        "L3" => "L3",
        "L4" => "L4",
        "L5" => "L5",
        "L6" => "L6",
        "L7" => "L7",
        "L9" => "L9",
        "L10" => "L10",
        "L11" => "L11",
        "L13" => "L13",
        "L20" => "L20",
        "L21" => "L21",
        "L22" => "L22",
        "L23" => "L23",
        other => panic!("unexpected rule {other}"),
    }
}

type Mutation = fn(&mut std::collections::BTreeMap<String, Value>);

/// §6.4 security rules not already covered by a bad sample: one tampered update each, and each
/// is refused as a whole with the matching rule.
#[test]
fn each_security_rule_refuses_the_whole_update() {
    let cases: Vec<(&str, &str, Mutation)> = vec![
        ("L1", "https only: terms", |p| {
            p.get_mut("spotify").unwrap()["terms"]["url"] =
                json!("http://developer.spotify.com/terms");
        }),
        ("L1", "https only: oEmbed endpoint", |p| {
            p.get_mut("spotify").unwrap()["route"][0]["plan"][0]["endpoint"] =
                json!("http://open.spotify.com/oembed");
        }),
        ("L1", "a wildcard over a public suffix", |p| {
            p.get_mut("taobao").unwrap()["domains"] = json!(["item.taobao.com", "*.com.cn"]);
        }),
        ("L9", "public-api host outside api_hosts", |p| {
            p.get_mut("app-store").unwrap()["route"][0]["plan"][0]["url"] =
                json!("https://collector.example.net/lookup?id={id}");
        }),
        ("L9", "oEmbed endpoint outside api_hosts", |p| {
            p.get_mut("spotify").unwrap()["api_hosts"] = json!(["api.spotify.com"]);
        }),
        ("L9", "public-api without terms", |p| {
            p.get_mut("app-store")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove("terms");
        }),
        ("L3", "one host, two providers", |p| {
            p.get_mut("taobao").unwrap()["domains"] =
                json!(["item.taobao.com", "www.bilibili.com"]);
        }),
        ("L3", "wildcard over another provider's host", |p| {
            p.get_mut("taobao").unwrap()["domains"] = json!(["item.taobao.com", "*.bilibili.com"]);
        }),
        ("L4", "route.host outside the provider", |p| {
            p.get_mut("bilibili").unwrap()["route"][0]["host"] = json!("live.bilibili.com");
        }),
        ("L11", "scheme on the deny list", |p| {
            p.get_mut("bilibili").unwrap()["open"] = json!({"scheme": "intent://video/{bv}"});
        }),
        ("L11", "scheme capture wider than [A-Za-z0-9_-]", |p| {
            p.get_mut("app-store").unwrap()["open"] = json!({"scheme": "itms-apps://app/{cc}{id}"});
            p.get_mut("app-store").unwrap()["route"][0]["path"] =
                json!("^/(?P<cc>[a-z]{2})/app/[^/]+/id(?P<id>[0-z]{6,12})$");
        }),
        ("L10", "payment provider made structured", |p| {
            p.get_mut("alipay").unwrap()["tier"] = json!("structured");
        }),
        ("L10", "payment provider with an app scheme", |p| {
            p.get_mut("alipay").unwrap()["open"] =
                json!({"scheme": "alipays://platformapi/startapp"});
        }),
        ("L10", "payment kind under another category", |p| {
            let t = p.get_mut("taobao").unwrap();
            t["route"][0]["kind"] = json!("payment");
            t["tier"] = json!("structured");
            t["route"][0]["plan"] = json!([{"type": "og+jsonld"}]);
        }),
        ("L10", "brand with a fetching plan", |p| {
            p.get_mut("taobao").unwrap()["route"][0]["plan"] = json!([{"type": "og+jsonld"}]);
        }),
        ("L10", "fetch.ua present", |p| {
            p.get_mut("bilibili").unwrap()["fetch"] = json!({"ua": "Mozilla/5.0"});
        }),
        ("L13", "tellomi removed altogether", |p| {
            p.remove("tellomi");
        }),
        ("L13", "last official host removed", |p| {
            p.get_mut("tellomi").unwrap()["official_domains"] = json!([]);
        }),
        ("L13", "reserved paths removed", |p| {
            p.get_mut("tellomi")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove("reserved_paths");
        }),
        ("L20", "fragment capture sent in an oEmbed endpoint", |p| {
            let t = p.get_mut("tellomi").unwrap();
            t["route"][3]["fragment"] = json!("^(?P<invite>[A-Za-z0-9_-]{16,})$");
            t["route"][3]["plan"] = json!([{"type": "og+jsonld"}]);
            t["api_hosts"] = json!(["collector.example.net"]);
            t["route"][3]["plan"] = json!([{"type": "oembed", "endpoint": "https://collector.example.net/{invite}",
                "doc": "https://collector.example.net/doc"}]);
            t["terms"] = json!({"url": "https://collector.example.net/terms"});
        }),
        ("L21", "first-party oEmbed", |p| {
            let t = p.get_mut("tellomi").unwrap();
            t["api_hosts"] = json!(["collector.example.net"]);
            t["terms"] = json!({"url": "https://collector.example.net/terms"});
            t["route"][6]["plan"] = json!([{"type": "oembed", "endpoint": "https://collector.example.net/o",
                "doc": "https://collector.example.net/doc"}]);
        }),
        (
            "L22",
            "tellomi.cn beyond the bound? no — a foreign official host",
            |p| {
                p.get_mut("tellomi").unwrap()["official_domains"] =
                    json!(["tellomi.app", "tellomi.net"]);
            },
        ),
        ("L23", "tell.cc as another provider's short domain", |p| {
            p.get_mut("bilibili").unwrap()["short_domains"] = json!(["b23.tv", "tell.cc"]);
        }),
        (
            "L23",
            "official host in another provider's api_hosts",
            |p| {
                p.get_mut("app-store").unwrap()["api_hosts"] =
                    json!(["itunes.apple.com", "www.tellomi.app"]);
            },
        ),
        (
            "L23",
            "official host as another provider's route.host",
            |p| {
                let b = p.get_mut("bilibili").unwrap();
                b["domains"] = json!(["www.bilibili.com", "www.tellomi.cn"]);
                b["route"][0]["host"] = json!("www.tellomi.cn");
            },
        ),
        ("L7", "first-party kind from a third party", |p| {
            p.get_mut("bilibili").unwrap()["route"][0]["kind"] = json!("tellomi.group");
        }),
        ("L9", "first-party plan from a third party", |p| {
            p.get_mut("bilibili").unwrap()["route"][0]["plan"] = json!([{"type": "first-party"}]);
        }),
        ("L5", "an unanchored path", |p| {
            p.get_mut("bilibili").unwrap()["route"][0]["path"] = json!("^/video/.*");
        }),
        ("L5", "an inline flag that widens a class", |p| {
            p.get_mut("bilibili").unwrap()["route"][0]["path"] =
                json!("^(?i)/video/(?P<bv>BV[0-9a-z]{10})$");
        }),
        ("L6", "a template placeholder that is no capture", |p| {
            p.get_mut("app-store").unwrap()["route"][0]["plan"][0]["url"] =
                json!("https://itunes.apple.com/{country}/lookup?id={id}");
        }),
        ("L2", "duplicate id", |p| {
            let mut dup = p["taobao"].clone();
            dup["domains"] = json!(["world.taobao.com"]);
            p.insert("taobao-2".into(), dup);
        }),
    ];
    for (rule, about, mutate) in cases {
        let mut providers = base_providers();
        mutate(&mut providers);
        let err = load_providers(&providers).expect_err(about);
        assert!(
            err.violations().iter().any(|v| v.rule == rule),
            "{about}: expected {rule}, got {:?}",
            err.violations()
        );
    }
}

/// §6.4 / §7.2 compatibility rules: the update still applies, only the route degrades — and it
/// is still security-checked (the bad sample `hot-unknown-kind-still-checked` covers that half).
#[test]
fn compatibility_problems_degrade_one_route_only() {
    let cases: Vec<(&str, &str, Mutation, &str)> = vec![
        (
            "bilibili",
            "video",
            |p| {
                p.get_mut("bilibili").unwrap()["route"][0]["kind"] = json!("video.short");
            },
            "not known",
        ),
        (
            "bilibili",
            "video",
            |p| {
                p.get_mut("bilibili").unwrap()["route"][0]["plan"][0]["type"] = json!("graphql");
            },
            "plan type",
        ),
        (
            "bilibili",
            "video",
            |p| {
                p.get_mut("bilibili").unwrap()["route"][0]["plan"][0]["map"]["duration_ms"] =
                    json!("duration|iso8601_us");
            },
            "converter",
        ),
        (
            "spotify",
            "track",
            |p| {
                p.get_mut("spotify").unwrap()["route"][0]["kind"] = json!("article");
            },
            "reserved",
        ),
    ];
    for (provider, route, mutate, why) in cases {
        let mut providers = base_providers();
        mutate(&mut providers);
        let registry = load_providers(&providers).unwrap_or_else(|e| panic!("{why}: {e}"));
        let degraded = registry.degraded_routes();
        assert_eq!(degraded.len(), 1, "{why}: {degraded:?}");
        assert_eq!(
            (degraded[0].provider.as_str(), degraded[0].route.as_str()),
            (provider, route)
        );
        assert!(
            degraded[0].reason.contains(why),
            "{why}: {}",
            degraded[0].reason
        );
        // Everything else still works.
        let amap = registry
            .identify("https://uri.amap.com/marker?position=116.47,39.99", false)
            .expect("amap");
        assert_eq!(amap.route.as_deref(), Some("marker"));
        let tellomi = registry
            .identify("https://tell.cc/hk881qb", false)
            .expect("tellomi");
        assert_eq!(tellomi.kind.as_deref(), Some("tellomi.user"));
    }

    // A map target this build cannot render is dropped, not fatal and not a degradation.
    let mut providers = base_providers();
    providers.get_mut("bilibili").unwrap()["route"][0]["plan"][0]["map"]["view_count"] =
        json!("interactionStatistic[0].userInteractionCount");
    let registry = load_providers(&providers).expect("loads");
    assert!(registry.degraded_routes().is_empty());
}

#[test]
fn envelope_gate() {
    let tree = base_tree();
    let mut env: Value = serde_json::from_slice(&envelope(&tree, 1)).expect("json");
    env["schema"] = json!(2);
    let err = Registry::load(&serde_json::to_vec(&env).expect("json")).expect_err("schema 2");
    assert!(matches!(err, LoadError::Envelope(_)), "{err}");
    env["schema"] = json!(1);
    env["name"] = json!("policy");
    assert!(matches!(
        Registry::load(&serde_json::to_vec(&env).expect("json")),
        Err(LoadError::Envelope(_))
    ));
    // Unknown fields anywhere are tolerated: the payload only grows (§7.3).
    env["name"] = json!("links");
    env["payload"]["providers"][0]["future_field"] = json!({"x": 1});
    env["payload"]["future_list"] = json!([1, 2]);
    Registry::load(&serde_json::to_vec(&env).expect("json")).expect("forward compatible");
    // Only a strictly newer version replaces the current one.
    assert!(tellomi_policy::envelope::is_newer(
        2026092702,
        Some(2026092701)
    ));
    assert!(!tellomi_policy::envelope::is_newer(
        2026092701,
        Some(2026092701)
    ));
}
