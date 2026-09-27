//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The receiver (ADR-0063 §5.1 rule 4, §5.3, §6.1, §7.4): `classify` over the shared golden
//! samples, plus the receive-time checks and a sender → receiver round trip.

mod common;

use common::*;
use serde_json::{Value, json};
use tellomi_links::*;

fn rich_from(spec: &Value) -> Vec<u8> {
    let s = |k: &str| spec.get(k).and_then(Value::as_str).map(str::to_owned);
    let n = |k: &str| {
        spec.get(k)
            .and_then(Value::as_u64)
            .map(|v| u32::try_from(v).expect("u32"))
    };
    RichContent {
        kind: s("kind"),
        provider: s("provider"),
        schema: n("schema"),
        canonical_url: s("canonical_url"),
        attrs: spec
            .get("attrs")
            .and_then(Value::as_array)
            .map(|pairs| {
                pairs
                    .iter()
                    .map(|p| Attr {
                        key: p[0].as_str().map(str::to_owned),
                        value: p[1].as_str().map(str::to_owned),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        level: n("level"),
    }
    .encode_to_bytes()
}

fn preview_from(case: &Value) -> PreviewInput {
    let p = &case["preview"];
    let mut preview: PreviewInput = serde_json::from_value(json!({
        "url": p["url"],
        "title": p.get("title").cloned().unwrap_or(Value::Null),
        "description": p.get("description").cloned().unwrap_or(Value::Null),
        "has_image": p.get("has_image").cloned().unwrap_or(json!(false)),
        "rich": p.get("rich_hex").cloned().unwrap_or(Value::Null),
    }))
    .expect("preview input");
    if let Some(spec) = p.get("rich") {
        preview.rich = Some(rich_from(spec));
    }
    preview
}

#[test]
fn golden_preview_to_card_samples() {
    let registry = base_registry();
    let doc: Value = serde_json::from_slice(&read("classify-golden.json")).expect("golden json");
    let cases = doc["cases"].as_array().expect("cases");
    assert!(cases.len() >= 40);
    let mut failures = Vec::new();
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let preview = preview_from(case);
        let msg: MessageContext =
            serde_json::from_value(case.get("msg").cloned().unwrap_or(json!({}))).expect("msg");
        let body = case["body"].as_str().expect("body");
        let card = registry.classify(&preview, body, &msg);
        let got = serde_json::to_value(&card).expect("card serializes");
        for (key, want) in case["expect"].as_object().expect("expect") {
            let have = &got[key];
            let matches = match (key.as_str(), want) {
                // Expect a subset of the first-party object.
                ("first_party", Value::Object(w)) => w.iter().all(|(k, v)| &have[k] == v),
                _ => have == want,
            };
            if !matches {
                failures.push(format!(
                    "{name}\n    {key}: want {want}, got {have}\n    card: {got}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} golden cases failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn receive_check_keeps_or_drops_before_storing() {
    let registry = base_registry();
    let msg = MessageContext::default();
    let url = "https://www.bilibili.com/video/BV1YDhJ6ZEL6";
    let ok_rich = RichContent {
        kind: Some("video".into()),
        provider: Some("bilibili".into()),
        level: Some(2),
        ..Default::default()
    };
    let mut preview = PreviewInput {
        url: url.into(),
        title: Some("t".into()),
        rich: Some(ok_rich.encode_to_bytes()),
        ..Default::default()
    };
    assert_eq!(
        registry.receive_check(&preview, url, &msg),
        ReceiveCheck {
            keep_preview: true,
            keep_rich: true
        }
    );
    // Oversized rich: dropped before it is stored, the snapshot stays (§6.1 / §7.4).
    preview.rich = Some(
        RichContent {
            canonical_url: Some("x".repeat(MAX_CANONICAL_URL_CHARS + 1)),
            ..ok_rich.clone()
        }
        .encode_to_bytes(),
    );
    assert_eq!(
        registry.receive_check(&preview, url, &msg),
        ReceiveCheck {
            keep_preview: true,
            keep_rich: false
        }
    );
    // A URL that is not in the body: the whole preview goes.
    assert_eq!(
        registry.receive_check(&preview, "something else", &msg),
        ReceiveCheck {
            keep_preview: false,
            keep_rich: false
        }
    );
    // Not https / not a legal preview domain.
    preview.url = "http://www.bilibili.com/".into();
    assert!(
        !registry
            .receive_check(&preview, "http://www.bilibili.com/", &msg)
            .keep_preview
    );
}

/// Whatever a sender produces, a receiver on the same registry lands on the same level (§5.1
/// rule 4), and the bytes go through `rich_bytes` → `classify` unchanged.
#[test]
fn sender_output_classifies_to_the_same_level() {
    let registry = base_registry();
    for url in [
        "https://item.taobao.com/item.htm?id=100032608854",
        "https://render.alipay.com/p/f/fd-j5rqp49m/index.html",
        "https://www.bilibili.com/",
        "https://tell.cc/hk881qb",
        "https://tell.cc/u#p/+8613800000006",
        "https://uri.amap.com/marker?position=116.47,39.99",
        "https://b23.tv/BV1YDhJ6ZEL6",
    ] {
        let ctx = SendContext {
            expand_short_links: false,
            ..Default::default()
        };
        let mut job = registry.begin(url, &ctx);
        assert!(job.next_request().is_none(), "{url}: these need no network");
        let out = job.finish(None);
        let draft = out.preview.as_ref().expect(url);
        let preview = PreviewInput {
            url: draft.url.clone(),
            title: draft.title.clone(),
            description: draft.description.clone(),
            has_image: draft.image_url.is_some(),
            date: draft.date,
            rich: draft.rich_bytes(),
        };
        let card = registry.classify(&preview, url, &MessageContext::default());
        assert_eq!(card.level, out.level, "{url}: {card:?}");
        assert_eq!(card.kind, out.kind, "{url}");
    }
}

#[test]
fn a_hot_update_demoting_a_provider_applies_to_stored_messages() {
    let url = "https://www.bilibili.com/video/BV1YDhJ6ZEL6";
    let preview = PreviewInput {
        url: url.into(),
        title: Some("t".into()),
        has_image: true,
        rich: Some(
            RichContent {
                kind: Some("video".into()),
                provider: Some("bilibili".into()),
                level: Some(2),
                ..Default::default()
            }
            .encode_to_bytes(),
        ),
        ..Default::default()
    };
    let before = base_registry().classify(&preview, url, &MessageContext::default());
    assert_eq!(before.level, Level::Structured);
    // §5.4: the provider is demoted to brand by a hot update (plan none) → the same stored bytes
    // now render as a brand shell. Removing it altogether → generic.
    let mut providers = base_providers();
    let b = providers.get_mut("bilibili").unwrap();
    b["tier"] = json!("brand");
    for route in b["route"].as_array_mut().expect("routes") {
        route["plan"] = json!([{"type": "none"}]);
    }
    let demoted = load_providers(&providers).expect("loads");
    assert_eq!(
        demoted
            .classify(&preview, url, &MessageContext::default())
            .level,
        Level::Brand
    );
    providers.remove("bilibili");
    let removed = load_providers(&providers).expect("loads");
    assert_eq!(
        removed
            .classify(&preview, url, &MessageContext::default())
            .level,
        Level::Generic
    );
}

#[test]
fn preview_input_round_trips_through_json_with_hex_rich() {
    let input = PreviewInput {
        url: "https://tell.cc/hk881qb".into(),
        rich: Some(vec![0x0a, 0x01, 0x78]),
        ..Default::default()
    };
    let v = serde_json::to_value(&input).expect("serializes");
    assert_eq!(v["rich"], "0a0178");
    let back: PreviewInput = serde_json::from_value(v).expect("parses");
    assert_eq!(back.rich, input.rich);
    assert!(serde_json::from_value::<PreviewInput>(json!({"url": "x", "rich": "中文"})).is_err());
}
