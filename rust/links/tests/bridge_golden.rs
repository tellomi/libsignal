//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! `tests/data/bridge-golden.json`: inputs and the **exact** JSON the bridges must return for them.
//!
//! The file is produced by the same `tellomi_links::json` functions `rust/bridge/shared/src/links.rs`
//! calls, from the shared `classify-golden.json` samples plus a few calls to every other
//! function. The Java, Swift and TypeScript tests replay it through their bridge and compare
//! byte for byte, so the three platforms provably return the same thing.
//!
//! This test fails when the committed file is stale. Regenerate with
//! `UPDATE_BRIDGE_GOLDEN=1 cargo test -p tellomi-links --test bridge_golden`.

mod common;

use common::*;
use serde_json::{Value, json};
use tellomi_links::{Attr, Registry, RichContent, json as api};

const DIST: &str = "registry/dist/links-2026092702.json";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn rich_hex(spec: &Value) -> String {
    let s = |k: &str| spec.get(k).and_then(Value::as_str).map(str::to_owned);
    let n = |k: &str| {
        spec.get(k)
            .and_then(Value::as_u64)
            .map(|v| u32::try_from(v).expect("u32"))
    };
    hex(&RichContent {
        kind: s("kind"),
        provider: s("provider"),
        schema: n("schema"),
        canonical_url: s("canonical_url"),
        attrs: spec
            .get("attrs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|p| Attr {
                key: p[0].as_str().map(str::to_owned),
                value: p[1].as_str().map(str::to_owned),
            })
            .collect(),
        level: n("level"),
    }
    .encode_to_bytes())
}

/// Scripted network for a send case: the bridge tests answer each request from this table.
fn send_cases() -> Vec<Value> {
    let lookup = json!({"resultCount": 1, "results": [{"trackName": "微信", "artistName": "WeChat",
        "artworkUrl512": "https://is1-ssl.mzstatic.com/image/thumb/512x512bb.jpg"}]});
    vec![
        json!({"name": "brand shell, no request", "url": "https://item.taobao.com/item.htm?id=100032608854",
               "context": "{\"locale\":\"zh-Hans\"}", "script": {}}),
        json!({"name": "tell.cc user, no request", "url": "https://tell.cc/ceshi.57", "context": "{}", "script": {}}),
        json!({"name": "App Store public API + image", "url": "https://apps.apple.com/cn/app/wechat/id414478124",
               "context": "{}",
               "script": {"responses": {"https://itunes.apple.com/cn/lookup?id=414478124": {
                   "status": 200, "final_url": "https://itunes.apple.com/cn/lookup?id=414478124",
                   "content_type": "text/javascript; charset=utf-8", "body": lookup.to_string()}},
                   "image_ok": true}}),
        json!({"name": "group via the client's own lookup", "url": "https://tell.cc/g#AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
               "context": "{}",
               "script": {"first_party": "{\"ok\":true,\"title\":\"周末爬山群\",\"member_count\":12}"}}),
        json!({"name": "unreachable network, remembered", "url": "https://www.bilibili.com/video/BV1YDhJ6ZEL6",
               "context": "{}", "script": {"network_error": ["https://www.bilibili.com/video/BV1YDhJ6ZEL6"]}}),
    ]
}

/// Drive one send case exactly as the bridge tests do, recording every request JSON.
fn run_send(registry: &Registry, case: &Value) -> (Vec<String>, String) {
    let script = &case["script"];
    let mut job = api::begin(
        registry,
        case["url"].as_str().unwrap(),
        case["context"].as_str().unwrap(),
    )
    .expect("context parses");
    let mut requests = Vec::new();
    while let Some(req) = api::next_request(&mut job) {
        let parsed: Value = serde_json::from_str(&req).unwrap();
        let id = u32::try_from(parsed["id"].as_u64().unwrap()).unwrap();
        requests.push(req);
        match parsed["type"].as_str().unwrap() {
            "first_party" => {
                api::on_first_party(&mut job, id, script["first_party"].as_str().unwrap()).unwrap()
            }
            "image" => job.on_image(id, script["image_ok"].as_bool().unwrap_or(false)),
            _ => {
                let url = parsed["url"].as_str().unwrap();
                if let Some(r) = script["responses"].get(url) {
                    api::on_response(
                        &mut job,
                        id,
                        u32::try_from(r["status"].as_u64().unwrap()).unwrap(),
                        r["final_url"].as_str().unwrap(),
                        r["content_type"].as_str().unwrap(),
                        r["location"].as_str(),
                        r["body"].as_str().unwrap_or("").as_bytes(),
                    );
                } else if script["network_error"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|u| u == url))
                {
                    job.on_network_error(id);
                } else {
                    job.on_failure(id);
                }
            }
        }
    }
    (requests, api::finish(&mut job, None))
}

fn generate() -> Value {
    let dist = read(DIST);
    let registry = Registry::load(&dist).expect("dist loads");
    let doc: Value = serde_json::from_slice(&read("classify-golden.json")).expect("golden");
    let mut classify = Vec::new();
    for case in doc["cases"].as_array().unwrap() {
        let p = &case["preview"];
        let mut preview = serde_json::Map::new();
        for key in ["url", "title", "description", "has_image"] {
            if let Some(v) = p.get(key) {
                preview.insert(key.into(), v.clone());
            }
        }
        if let Some(spec) = p.get("rich") {
            preview.insert("rich".into(), json!(rich_hex(spec)));
        } else if let Some(h) = p.get("rich_hex") {
            preview.insert("rich".into(), h.clone());
        }
        let preview = Value::Object(preview).to_string();
        let message = case.get("msg").cloned().unwrap_or(json!({})).to_string();
        let body = case["body"].as_str().unwrap();
        classify.push(json!({
            "name": case["name"],
            "preview": preview,
            "body": body,
            "message": message,
            "card": api::classify(&registry, &preview, body, &message).unwrap(),
            "receive_check": api::receive_check(&registry, &preview, body, &message).unwrap(),
        }));
    }
    let open_urls = [
        "https://www.bilibili.com/video/BV1YDhJ6ZEL6/?spm_id_from=333.1007",
        "https://render.alipay.com/p/f/fd-j5rqp49m/index.html",
        "https://tell.cc/g#AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "https://tellomi.app/download/",
        "https://www.bi1ibili.com/video/BV1YDhJ6ZEL6",
        "intent://scan/#Intent;scheme=zxing;end",
        "http://www.163.com/",
    ];
    let identify_urls = [
        ("https://b23.tv/BV1YDhJ6ZEL6", false),
        ("https://music.163.com/#/song?id=428350227", false),
        (
            "https://github.com/signalapp/Signal-Android/issues/10000",
            false,
        ),
        (
            "https://wb.amap.com/?p=B00156NZVG%2C31.247461353927147%2C121.4993718266487%2CNorth+Bund+Green+Land%2CDongdaming+Road+558-678&src=app_share",
            false,
        ),
        (
            "http://wb.amap.com/?p=B015F0IUBL%2C37.877682%2C112.573974%2C%E5%90%8C%E4%BB%81%E5%A0%82%28%E4%BA%94%E4%B8%80%E8%B7%AF%E5%BA%97%29%2C%E5%A4%AA%E5%8E%9F%E5%B8%82%E6%9D%8F%E8%8A%B1%E5%B2%AD%E5%8C%BA%E4%BA%94%E4%B8%80%E8%B7%AF227%E5%8F%B7%2C140100&src=pc_sms_poi",
            true,
        ),
        ("https://www.163.com/", false),
    ];
    let layouts = [
        (1200, 630, "video", "structured"),
        (100, 100, "", "generic"),
        (0, 0, "", "generic"),
        (1200, 630, "product", "brand"),
        // A brand shell asks with its bundled icon's size (card-visual §3.9): an icon card; without
        // an icon it asks with 0 × 0: no image.
        (114, 114, "product", "brand"),
        (0, 0, "product", "brand"),
        (1200, 630, "tellomi.group", "first_party"),
    ];
    // A 4×4 image: three quarters orange, one quarter transparent.
    let mut rgba = Vec::new();
    for i in 0..16 {
        rgba.extend_from_slice(if i % 4 == 3 {
            &[0, 0, 0, 0]
        } else {
            &[0xFE, 0x75, 0x00, 0xFF]
        });
    }
    json!({
        "about": "Generated by rust/links/tests/bridge_golden.rs from classify-golden.json and the registry below; every *_json value is the exact string the bridges must return. Do not edit by hand.",
        "registry": DIST,
        "registry_version": registry.version(),
        "degraded": api::degraded(&registry),
        "classify": classify,
        "open_plan": open_urls.iter().map(|u| json!({"url": u, "plan": api::open_plan(&registry, u)})).collect::<Vec<_>>(),
        "identify": identify_urls.iter().map(|(u, loc)| json!({"url": u, "location": loc, "result": api::identify(&registry, u, *loc)})).collect::<Vec<_>>(),
        "layout": layouts.iter().map(|(w, h, k, l)| json!({"width": w, "height": h, "kind": k, "level": l, "layout": api::layout(*w, *h, k, l).unwrap()})).collect::<Vec<_>>(),
        "tint": [{"layout": "icon", "width": 4, "height": 4, "rgba_hex": hex(&rgba), "tint": api::tint("icon", 4, 4, &rgba).unwrap()}],
        "send": send_cases().iter().map(|c| {
            let (requests, outcome) = run_send(&registry, c);
            let mut c = c.clone();
            c["requests"] = json!(requests);
            c["outcome"] = json!(outcome);
            c
        }).collect::<Vec<_>>(),
    })
}

#[test]
fn bridge_golden_is_current() {
    let generated = serde_json::to_string_pretty(&generate()).unwrap() + "\n";
    let path = data_dir().join("bridge-golden.json");
    if std::env::var_os("UPDATE_BRIDGE_GOLDEN").is_some() {
        std::fs::write(&path, &generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == generated,
        "tests/data/bridge-golden.json is stale; regenerate with \
         UPDATE_BRIDGE_GOLDEN=1 cargo test -p tellomi-links --test bridge_golden"
    );
}

#[test]
fn golden_cards_agree_with_the_classify_samples() {
    // The golden file is made from the dist envelope, the classify samples were written against
    // the TOML sources: the levels must agree, so the dist really is those sources.
    let golden = generate();
    let doc: Value = serde_json::from_slice(&read("classify-golden.json")).unwrap();
    for (g, case) in golden["classify"]
        .as_array()
        .unwrap()
        .iter()
        .zip(doc["cases"].as_array().unwrap())
    {
        let card: Value = serde_json::from_str(g["card"].as_str().unwrap()).unwrap();
        if let Some(level) = case["expect"].get("level") {
            assert_eq!(&card["level"], level, "{}", case["name"]);
        }
    }
    assert_eq!(golden["degraded"], json!("[]"));
}
