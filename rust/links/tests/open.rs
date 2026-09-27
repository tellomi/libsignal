//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Tapping a card or a link (ADR-0063 §4.9, §5.5, §6.1).

mod common;

use common::*;
use serde_json::json;
use tellomi_links::*;

fn kinds(plan: &OpenPlan) -> Vec<&'static str> {
    plan.steps
        .iter()
        .map(|s| match s {
            OpenStep::InApp { .. } => "in_app",
            OpenStep::InstalledAppOnly { .. } => "installed_app_only",
            OpenStep::Scheme { .. } => "scheme",
            OpenStep::Browser { .. } => "browser",
            OpenStep::CopyLink { .. } => "copy_link",
        })
        .collect()
}

fn targets(plan: &OpenPlan) -> Vec<&str> {
    plan.steps
        .iter()
        .map(|s| match s {
            OpenStep::InApp { url }
            | OpenStep::InstalledAppOnly { url }
            | OpenStep::Scheme { url }
            | OpenStep::Browser { url }
            | OpenStep::CopyLink { url } => url.as_str(),
        })
        .collect()
}

#[test]
fn third_party_order_and_target() {
    let registry = base_registry();
    // Tracking parameters stay: the target is the URL in the message, never canonical_url.
    let url = "https://www.bilibili.com/video/BV1YDhJ6ZEL6/?spm_id_from=333.1007&vd_source=0123";
    let plan = registry.open_plan(url);
    assert_eq!(kinds(&plan), ["installed_app_only", "browser", "copy_link"]);
    assert!(targets(&plan).iter().all(|t| *t == url));
    assert_eq!(plan.label, OpenLabel::OpenLink);
    assert_eq!(
        plan.app_name.as_ref().map(|n| n.zh_hans.as_str()),
        Some("哔哩哔哩")
    );

    // Unknown site: the same order without a name.
    let plan = registry.open_plan("https://www.163.com/news/article/K1234.html");
    assert_eq!(kinds(&plan), ["installed_app_only", "browser", "copy_link"]);
    assert_eq!(plan.app_name, None);
}

#[test]
fn registry_scheme_is_filled_from_this_devices_match_and_encoded() {
    let mut providers = base_providers();
    providers.get_mut("bilibili").unwrap()["open"] = json!({"scheme": "bilibili://video/{bv}"});
    providers.get_mut("app-store").unwrap()["open"] =
        json!({"universal": {"verified": "2026-09-24", "evidence": "tests"}});
    let registry = load_providers(&providers).expect("loads");
    let plan = registry.open_plan("https://www.bilibili.com/video/BV1YDhJ6ZEL6");
    assert_eq!(
        kinds(&plan),
        ["installed_app_only", "scheme", "browser", "copy_link"]
    );
    assert_eq!(targets(&plan)[1], "bilibili://video/BV1YDhJ6ZEL6");
    assert_eq!(plan.label, OpenLabel::OpenInApp);
    // No object recognised (or a short link this side never expands): no scheme step.
    assert_eq!(
        kinds(&registry.open_plan("https://b23.tv/BV1YDhJ6ZEL6")),
        ["installed_app_only", "browser", "copy_link"]
    );
    assert_eq!(
        kinds(&registry.open_plan("https://www.bilibili.com/")),
        ["installed_app_only", "browser", "copy_link"]
    );
    // A verified universal link alone earns "Open in App Store".
    assert_eq!(
        registry
            .open_plan("https://apps.apple.com/cn/app/wechat/id414478124")
            .label,
        OpenLabel::OpenInApp
    );
}

#[test]
fn payment_goes_straight_to_the_browser() {
    let plan = base_registry().open_plan("https://render.alipay.com/p/f/fd-j5rqp49m/index.html");
    assert_eq!(kinds(&plan), ["browser", "copy_link"]);
    assert_eq!(plan.label, OpenLabel::LeavesTo);
    assert_eq!(
        plan.app_name.as_ref().and_then(|n| n.en.as_deref()),
        Some("Alipay")
    );
}

#[test]
fn first_party() {
    let registry = base_registry();
    let plan = registry.open_plan("https://tell.cc/g#AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
    assert_eq!(kinds(&plan), ["in_app", "copy_link"]);
    assert_eq!(plan.label, OpenLabel::InApp);
    assert_eq!(
        kinds(&registry.open_plan("https://tell.cc/hk881qb")),
        ["in_app", "copy_link"]
    );
    // Not an object on tell.cc, and the official site: a browser.
    assert_eq!(
        kinds(&registry.open_plan("https://tell.cc/app")),
        ["browser", "copy_link"]
    );
    assert_eq!(
        kinds(&registry.open_plan("https://tellomi.app/download/")),
        ["browser", "copy_link"]
    );
}

#[test]
fn never_a_target() {
    let registry = base_registry();
    for url in [
        "intent://scan/#Intent;scheme=zxing;package=com.google.zxing.client.android;end",
        "javascript:alert(1)",
        "data:text/html,<script>alert(1)</script>",
        "file:///etc/passwd",
        "not a url",
    ] {
        assert!(registry.open_plan(url).steps.is_empty(), "{url}");
    }
    // Plain http: no universal links, the browser decides.
    assert_eq!(
        kinds(&registry.open_plan("http://www.bilibili.com/")),
        ["browser", "copy_link"]
    );
}

#[test]
fn lookalikes_are_warned_about_before_opening() {
    let plan = base_registry().open_plan("https://www.bi1ibili.com/video/BV1YDhJ6ZEL6");
    assert_eq!(plan.lookalike.as_deref(), Some("bilibili.com"));
    assert!(!plan.steps.is_empty(), "a warning, not a block (§6.1)");
    let v = serde_json::to_value(&plan).expect("serializes");
    assert_eq!(v["steps"][0]["type"], "installed_app_only");
}
