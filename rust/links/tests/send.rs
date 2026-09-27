//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The sender pipeline against scripted responses (ADR-0063 §4.2–§4.4, the sender table of §5.2).
//! No network: a fake fetcher answers from a table and panics on any request it was not given,
//! which is how "sends no request" is asserted.

mod common;

use std::collections::HashMap;

use common::*;
use serde_json::json;
use tellomi_links::*;
use tellomi_policy::PolicyEngine;

#[derive(Clone)]
enum Reply {
    Http {
        status: u16,
        final_url: String,
        content_type: &'static str,
        location: Option<String>,
        body: Vec<u8>,
    },
    NetworkError,
    Failure,
}

fn page(final_url: &str, html: &str) -> Reply {
    Reply::Http {
        status: 200,
        final_url: final_url.into(),
        content_type: "text/html; charset=utf-8",
        location: None,
        body: html.as_bytes().to_vec(),
    }
}

fn json_reply(final_url: &str, content_type: &'static str, body: serde_json::Value) -> Reply {
    Reply::Http {
        status: 200,
        final_url: final_url.into(),
        content_type,
        location: None,
        body: serde_json::to_vec(&body).expect("json"),
    }
}

fn redirect(location: &str) -> Reply {
    Reply::Http {
        status: 302,
        final_url: String::new(),
        content_type: "",
        location: Some(location.into()),
        body: Vec::new(),
    }
}

#[derive(Default)]
struct Script {
    replies: HashMap<String, Reply>,
    first_party: Option<FirstPartyResult>,
    image_ok: bool,
}

struct Run {
    out: SendOutcome,
    requests: Vec<Request>,
}

impl Run {
    fn urls(&self) -> Vec<String> {
        self.requests
            .iter()
            .map(|r| match &r.kind {
                RequestKind::ExpandShortLink { url, .. } | RequestKind::Fetch { url, .. } => {
                    url.clone()
                }
                RequestKind::Image { url, .. } => format!("image {url}"),
                RequestKind::FirstParty { kind } => format!("first-party {kind}"),
            })
            .collect()
    }

    fn rich(&self) -> RichContent {
        let bytes = self
            .out
            .preview
            .as_ref()
            .and_then(PreviewDraft::rich_bytes)
            .expect("rich");
        RichContent::decode_checked(&bytes).expect("round-trips")
    }

    fn attr(&self, key: &str) -> Option<String> {
        self.rich()
            .attrs
            .into_iter()
            .find(|a| a.key.as_deref() == Some(key))
            .and_then(|a| a.value)
    }
}

fn run_with(
    registry: &Registry,
    url: &str,
    ctx: &SendContext,
    script: &Script,
    policy: Option<&PolicyEngine>,
) -> Run {
    let mut job = registry.begin(url, ctx);
    let mut requests = Vec::new();
    while let Some(req) = job.next_request() {
        requests.push(req.clone());
        assert!(
            requests.len() <= MAX_METADATA_REQUESTS + 1,
            "too many requests: {requests:?}"
        );
        match &req.kind {
            RequestKind::ExpandShortLink { url, .. } | RequestKind::Fetch { url, .. } => {
                match script
                    .replies
                    .get(url)
                    .unwrap_or_else(|| panic!("unexpected request {url}"))
                {
                    Reply::Http {
                        status,
                        final_url,
                        content_type,
                        location,
                        body,
                    } => {
                        let final_url = if final_url.is_empty() {
                            url.as_str()
                        } else {
                            final_url.as_str()
                        };
                        job.on_response(
                            req.id,
                            *status,
                            final_url,
                            content_type,
                            location.as_deref(),
                            body,
                        )
                    }
                    Reply::NetworkError => job.on_network_error(req.id),
                    Reply::Failure => job.on_failure(req.id),
                }
            }
            RequestKind::FirstParty { .. } => job.on_first_party(
                req.id,
                script
                    .first_party
                    .clone()
                    .expect("first-party result scripted"),
            ),
            RequestKind::Image { .. } => job.on_image(req.id, script.image_ok),
        }
    }
    Run {
        out: job.finish(policy),
        requests,
    }
}

fn run(url: &str, script: &Script) -> Run {
    run_with(&base_registry(), url, &SendContext::default(), script, None)
}

fn bilibili_page() -> String {
    String::from_utf8(read("html/bilibili-video.html")).expect("utf-8")
}

const BV: &str = "https://www.bilibili.com/video/BV1YDhJ6ZEL6";

#[test]
fn not_previewable_and_not_objects_send_nothing() {
    for url in [
        "http://www.bilibili.com/video/BV1YDhJ6ZEL6",
        "javascript:alert(1)",
        "https://localhost/x",
        "https://tell.cc/app",
        "https://tell.cc/call",
        "https://tell.cc/.well-known/assetlinks.json",
    ] {
        let r = run(url, &Script::default());
        assert_eq!(r.out.level, Level::PlainLink, "{url}");
        assert!(r.out.preview.is_none() && r.requests.is_empty(), "{url}");
    }
}

#[test]
fn lookalike_domain_is_a_plain_link_and_is_not_fetched() {
    let r = run(
        "https://www.bi1ibili.com/video/BV1YDhJ6ZEL6",
        &Script::default(),
    );
    assert_eq!(r.out.level, Level::PlainLink);
    assert_eq!(r.out.lookalike.as_deref(), Some("bilibili.com"));
    assert!(r.requests.is_empty());
}

#[test]
fn structured_video_from_verified_json_ld() {
    let script = Script {
        replies: HashMap::from([(BV.to_owned(), page(&format!("{BV}/"), &bilibili_page()))]),
        image_ok: true,
        ..Default::default()
    };
    let r = run(BV, &script);
    assert_eq!(r.out.level, Level::Structured, "{:?}", r.out.failures);
    let p = r.out.preview.as_ref().expect("preview");
    // JSON-LD's clean name wins over og:title with the site suffix (§4.4).
    assert_eq!(p.title.as_deref(), Some("《柯洁围棋入门课》"));
    assert_eq!(p.url, BV, "the target is always the typed URL");
    assert_eq!(
        p.image_url.as_deref(),
        Some(
            "https://i2.hdslb.com/bfs/archive/35baeae957483b53d75c1dd12eecb7e6d2f498c4.jpg@1280w_720h"
        )
    );
    assert_eq!(p.date, Some(1_790_069_133_000));
    let rich = r.rich();
    assert_eq!(
        (
            rich.kind.as_deref(),
            rich.provider.as_deref(),
            rich.level,
            rich.schema
        ),
        (
            Some("video"),
            Some("bilibili"),
            Some(LEVEL_STRUCTURED),
            Some(1)
        )
    );
    assert_eq!(rich.canonical_url.as_deref(), Some(BV));
    assert_eq!(r.attr("author").as_deref(), Some("柯洁"));
    assert_eq!(r.attr("duration_ms").as_deref(), Some("499000"));
    assert_eq!(
        r.attr("published_at").as_deref(),
        Some("2026-09-22T09:25:33.000Z")
    );
    // The placeholder "-" never reaches the card: OG's description is used instead.
    assert_ne!(p.description.as_deref(), Some("-"));
    assert_eq!(
        r.urls(),
        vec![
            BV.to_owned(),
            format!("image {}", p.image_url.clone().unwrap())
        ]
    );
    for req in &r.requests {
        if let RequestKind::Fetch {
            user_agent,
            accept,
            max_bytes,
            max_redirects,
            content_types,
            ..
        } = &req.kind
        {
            assert_eq!(
                (*user_agent, *accept, *max_bytes, *max_redirects),
                ("WhatsApp/2", "text/html", MAX_HTML_BYTES, 5)
            );
            assert!(content_types.contains(&"text/html"));
        }
    }
}

#[test]
fn json_ld_about_another_object_is_discarded() {
    // Xigua-style: the page's JSON-LD describes some other video (§3.4.5).
    let html = bilibili_page()
        .replace("BV1YDhJ6ZEL6/#video", "BV1xx411c7mD/#video")
        .replace(r#""url":"https://www.bilibili.com/video/BV1YDhJ6ZEL6/","mainEntityOfPage":{"@id":"https://www.bilibili.com/video/BV1YDhJ6ZEL6/#webpage"}"#,
                 r#""url":"https://www.bilibili.com/video/BV1xx411c7mD/""#);
    let script = Script {
        replies: HashMap::from([(BV.to_owned(), page(&format!("{BV}/"), &html))]),
        image_ok: true,
        ..Default::default()
    };
    let r = run(BV, &script);
    assert!(
        r.out.failures.contains(&Failure::JsonLdIdentity),
        "{:?}",
        r.out.failures
    );
    // Still structured from OG, with title_strip removing the site suffix; nothing from the
    // foreign node (no author, no duration).
    assert_eq!(r.out.level, Level::Structured);
    assert_eq!(
        r.out.preview.as_ref().unwrap().title.as_deref(),
        Some("《柯洁围棋入门课》")
    );
    assert_eq!(r.attr("author"), None);
}

#[test]
fn only_title_meta_is_not_enough_for_a_structured_card() {
    let html =
        r#"<html><head><title>《柯洁围棋入门课》_哔哩哔哩bilibili_教学</title></head></html>"#;
    let script = Script {
        replies: HashMap::from([(BV.to_owned(), page(BV, html))]),
        ..Default::default()
    };
    let r = run(BV, &script);
    assert_eq!(r.out.level, Level::Brand);
    assert!(r.out.failures.contains(&Failure::RequiredMissing));
    // The best title (title-meta, suffix stripped) still goes into the snapshot for old clients.
    assert_eq!(
        r.out.preview.as_ref().unwrap().title.as_deref(),
        Some("《柯洁围棋入门课》")
    );
    assert_eq!(r.rich().level, Some(LEVEL_BRAND));
    assert!(r.rich().attrs.is_empty());
    assert!(
        r.urls().iter().all(|u| !u.starts_with("image")),
        "brand shells carry no image"
    );
}

#[test]
fn image_failure_makes_a_video_a_brand_shell() {
    let script = Script {
        replies: HashMap::from([(BV.to_owned(), page(&format!("{BV}/"), &bilibili_page()))]),
        image_ok: false,
        ..Default::default()
    };
    let r = run(BV, &script);
    assert_eq!(r.out.level, Level::Brand);
    assert!(r.out.failures.contains(&Failure::ImageFailed));
}

#[test]
fn step_failures_fall_through_to_a_brand_shell() {
    for (why, reply) in [
        (
            "http status",
            Reply::Http {
                status: 404,
                final_url: BV.into(),
                content_type: "text/html",
                location: None,
                body: vec![],
            },
        ),
        (
            "content type",
            Reply::Http {
                status: 200,
                final_url: BV.into(),
                content_type: "text/plain",
                location: None,
                body: bilibili_page().into_bytes(),
            },
        ),
        (
            "too large",
            Reply::Http {
                status: 200,
                final_url: BV.into(),
                content_type: "text/html",
                location: None,
                body: vec![b' '; MAX_HTML_BYTES + 1],
            },
        ),
        // An open redirect on the provider's domain must not put another site's page in the card.
        (
            "redirected off the provider",
            page("https://evil.cn/video", &bilibili_page()),
        ),
        ("failure", Reply::Failure),
    ] {
        let script = Script {
            replies: HashMap::from([(BV.to_owned(), reply)]),
            ..Default::default()
        };
        let r = run(BV, &script);
        assert_eq!(r.out.level, Level::Brand, "{why}");
        assert_eq!(
            r.out.preview.as_ref().unwrap().title.as_deref(),
            Some("哔哩哔哩"),
            "{why}"
        );
        assert!(r.out.newly_unreachable_hosts.is_empty(), "{why}");
    }
}

#[test]
fn network_failure_is_remembered_and_the_host_skipped() {
    let script = Script {
        replies: HashMap::from([(BV.to_owned(), Reply::NetworkError)]),
        ..Default::default()
    };
    let r = run(BV, &script);
    assert_eq!(r.out.level, Level::Brand);
    assert_eq!(
        r.out.newly_unreachable_hosts,
        vec!["www.bilibili.com".to_owned()]
    );
    // Next time the memo is passed in and no request is made at all.
    let ctx = SendContext {
        unreachable_hosts: r.out.newly_unreachable_hosts.clone(),
        ..Default::default()
    };
    let again = run_with(&base_registry(), BV, &ctx, &Script::default(), None);
    assert_eq!(again.out.level, Level::Brand);
    assert!(again.requests.is_empty());
}

#[test]
fn region_gate_skips_unreachable_providers() {
    let track = "https://open.spotify.com/track/7qiZfU4dY1lWllzX7mPBI3";
    let cn = SendContext {
        region: Region::Cn,
        ..Default::default()
    };
    let r = run_with(&base_registry(), track, &cn, &Script::default(), None);
    assert_eq!(r.out.level, Level::Brand);
    assert!(
        r.requests.is_empty(),
        "cn never asks a provider marked unreachable_in = cn"
    );
    assert!(r.out.failures.contains(&Failure::RegionUnreachable));

    let oembed = "https://open.spotify.com/oembed?url=https%3A%2F%2Fopen.spotify.com%2Ftrack%2F7qiZfU4dY1lWllzX7mPBI3&format=json";
    let script = Script {
        replies: HashMap::from([(
            oembed.to_owned(),
            json_reply(
                oembed,
                "application/json",
                json!({"title": "Blinding Lights", "thumbnail_url": "https://image-cdn-ak.spotifycdn.com/image/ab67616d00001e02"}),
            ),
        )]),
        image_ok: true,
        ..Default::default()
    };
    let r = run_with(
        &base_registry(),
        track,
        &SendContext::default(),
        &script,
        None,
    );
    assert_eq!(r.out.level, Level::Structured, "{:?}", r.out.failures);
    assert_eq!(r.rich().kind.as_deref(), Some("music.track"));
    assert_eq!(
        r.out.preview.as_ref().unwrap().title.as_deref(),
        Some("Blinding Lights")
    );
}

#[test]
fn json_api_is_checked_for_host_and_type() {
    let url = "https://apps.apple.com/cn/app/wechat/id414478124";
    let api = "https://itunes.apple.com/cn/lookup?id=414478124";
    let body = json!({"resultCount": 1, "results": [{"trackName": "微信", "artistName": "WeChat",
        "artworkUrl512": "https://is1-ssl.mzstatic.com/image/thumb/512x512bb.jpg"}]});
    // iTunes really answers `text/javascript`; it is parsed as JSON, never run.
    let script = Script {
        replies: HashMap::from([(
            api.to_owned(),
            json_reply(api, "text/javascript; charset=utf-8", body.clone()),
        )]),
        image_ok: true,
        ..Default::default()
    };
    let r = run(url, &script);
    assert_eq!(r.out.level, Level::Structured, "{:?}", r.out.failures);
    assert_eq!(r.attr("developer").as_deref(), Some("WeChat"));
    assert_eq!(
        r.attr("platform").as_deref(),
        Some("ios"),
        "constant attrs from the route"
    );

    // Redirected to a host outside api_hosts: the step is void.
    let script = Script {
        replies: HashMap::from([(
            api.to_owned(),
            json_reply(
                "https://collector.example.org/x",
                "application/json",
                body.clone(),
            ),
        )]),
        image_ok: true,
        ..Default::default()
    };
    assert_eq!(run(url, &script).out.level, Level::Brand);
    // Nested deeper than 32 levels: not parsed.
    let deep = serde_json::from_str::<serde_json::Value>(&format!(
        "{}1{}",
        "[".repeat(40),
        "]".repeat(40)
    ))
    .unwrap();
    let script = Script {
        replies: HashMap::from([(api.to_owned(), json_reply(api, "application/json", deep))]),
        ..Default::default()
    };
    let r = run(url, &script);
    assert_eq!(r.out.level, Level::Brand);
    assert!(r.out.failures.contains(&Failure::Parse));
}

#[test]
fn brand_tier_sends_nothing_and_says_only_the_platform_name() {
    let ctx = SendContext {
        locale: "zh-Hans".into(),
        ..Default::default()
    };
    let r = run_with(
        &base_registry(),
        "https://item.taobao.com/item.htm?id=100032608854",
        &ctx,
        &Script::default(),
        None,
    );
    assert!(r.requests.is_empty());
    assert_eq!(r.out.level, Level::Brand);
    let p = r.out.preview.as_ref().unwrap();
    assert_eq!(
        (
            p.title.as_deref(),
            p.description.as_deref(),
            p.image_url.as_deref()
        ),
        (Some("淘宝"), None, None)
    );
    assert_eq!(
        (r.rich().kind.as_deref(), r.rich().level),
        (Some("product"), Some(LEVEL_BRAND))
    );
    let en = SendContext {
        locale: "en-US".into(),
        ..Default::default()
    };
    let r = run_with(
        &base_registry(),
        "https://render.alipay.com/p/f/fd-j5rqp49m/index.html",
        &en,
        &Script::default(),
        None,
    );
    assert_eq!(
        r.out.preview.as_ref().unwrap().title.as_deref(),
        Some("Alipay")
    );
    assert_eq!(
        r.rich().kind.as_deref(),
        Some("web"),
        "no route → type text 'web'"
    );
}

#[test]
fn provider_without_a_recognised_object_is_a_brand_shell() {
    let r = run("https://www.bilibili.com/", &Script::default());
    assert_eq!(
        (r.out.level, r.out.kind.as_deref()),
        (Level::Brand, Some("web"))
    );
    assert!(r.requests.is_empty());
}

#[test]
fn short_links_expand_once_and_never_switch_provider() {
    let short = "https://b23.tv/BV1YDhJ6ZEL6";
    let script = Script {
        replies: HashMap::from([
            (short.to_owned(), redirect(BV)),
            (BV.to_owned(), page(&format!("{BV}/"), &bilibili_page())),
        ]),
        image_ok: true,
        ..Default::default()
    };
    let r = run(short, &script);
    assert_eq!(r.out.level, Level::Structured, "{:?}", r.out.failures);
    assert_eq!(
        r.out.preview.as_ref().unwrap().url,
        short,
        "the typed short link is what opens"
    );
    assert_eq!(r.rich().canonical_url.as_deref(), Some(BV));
    assert!(matches!(
        r.requests[0].kind,
        RequestKind::ExpandShortLink { .. }
    ));

    for (why, reply) in [
        (
            "no Location",
            Reply::Http {
                status: 200,
                final_url: short.into(),
                content_type: "text/html",
                location: None,
                body: vec![],
            },
        ),
        (
            "Location on another provider",
            redirect("https://open.spotify.com/track/7qiZfU4dY1lWllzX7mPBI3"),
        ),
        (
            "Location not in the registry",
            redirect("https://evil.cn/video/BV1YDhJ6ZEL6"),
        ),
        (
            "Location is another short link",
            redirect("https://b23.tv/again"),
        ),
        ("network", Reply::NetworkError),
    ] {
        let script = Script {
            replies: HashMap::from([(short.to_owned(), reply)]),
            ..Default::default()
        };
        let r = run(short, &script);
        assert_eq!(
            (r.out.level, r.out.provider.as_deref()),
            (Level::Brand, Some("bilibili")),
            "{why}"
        );
        assert_eq!(r.requests.len(), 1, "{why}: exactly one expansion attempt");
    }

    // The setting is off: brand shell of the short domain's provider, no request.
    let off = SendContext {
        expand_short_links: false,
        ..Default::default()
    };
    let r = run_with(&base_registry(), short, &off, &Script::default(), None);
    assert_eq!(r.out.level, Level::Brand);
    assert!(r.requests.is_empty());
}

#[test]
fn http_location_identifies_but_never_fetches() {
    let short = "https://surl.amap.com/483oczN13g2s";
    let location = "http://wb.amap.com/?p=B015F0IUBL%2C37.877682%2C112.573974%2C%E5%90%8C%E4%BB%81%E5%A0%82%28%E4%BA%94%E4%B8%80%E8%B7%AF%E5%BA%97%29%2C%E5%A4%AA%E5%8E%9F%E5%B8%82%E6%9D%8F%E8%8A%B1%E5%B2%AD%E5%8C%BA%E4%BA%94%E4%B8%80%E8%B7%AF227%E5%8F%B7%2C140100&src=pc_sms_poi";
    let script = Script {
        replies: HashMap::from([(short.to_owned(), redirect(location))]),
        ..Default::default()
    };
    let r = run(short, &script);
    assert_eq!(r.requests.len(), 1, "only the expansion");
    assert_eq!(r.out.level, Level::Structured, "{:?}", r.out.failures);
    assert_eq!(r.rich().kind.as_deref(), Some("place"));
    assert_eq!(r.attr("lat").as_deref(), Some("37.877682"));
    assert_eq!(r.attr("lng").as_deref(), Some("112.573974"));
    assert_eq!(r.attr("coord_sys").as_deref(), Some("gcj02"));
    assert_eq!(r.attr("name").as_deref(), Some("同仁堂(五一路店)"));
    assert_eq!(
        r.out.preview.as_ref().unwrap().title.as_deref(),
        Some("同仁堂(五一路店)")
    );
}

#[test]
fn metadata_requests_never_exceed_three() {
    let mut providers = base_providers();
    let step = json!({"type": "og+jsonld", "jsonld_type": "VideoObject", "map": {"title": "name", "image": "thumbnailUrl[0]"}});
    providers.get_mut("bilibili").unwrap()["route"][0]["plan"] =
        json!([step, step, step, step, step]);
    let registry = load_providers(&providers).expect("loads");
    let short = "https://b23.tv/BV1YDhJ6ZEL6";
    let script = Script {
        replies: HashMap::from([
            (short.to_owned(), redirect(BV)),
            (BV.to_owned(), Reply::Failure),
        ]),
        ..Default::default()
    };
    let r = run_with(&registry, short, &SendContext::default(), &script, None);
    assert_eq!(r.requests.len(), MAX_METADATA_REQUESTS, "{:?}", r.urls());
    assert!(r.out.failures.contains(&Failure::Budget));
    assert_eq!(r.out.level, Level::Brand);
}

#[test]
fn generic_links_use_og_then_title_meta() {
    let url = "https://www.163.com/news/article/K1234.html";
    let og = r#"<meta property="og:title" content="网易新闻标题"><meta property="og:description" content="摘要">
        <meta property="og:image" content="/cover.jpg"><script type="application/ld+json">{"@type":"NewsArticle","name":"ignored"}</script>"#;
    let script = Script {
        replies: HashMap::from([(url.to_owned(), page(url, og))]),
        image_ok: true,
        ..Default::default()
    };
    let r = run(url, &script);
    assert_eq!(r.out.level, Level::Generic);
    let p = r.out.preview.as_ref().unwrap();
    assert_eq!(
        (p.title.as_deref(), p.description.as_deref()),
        (Some("网易新闻标题"), Some("摘要"))
    );
    assert_eq!(
        p.image_url.as_deref(),
        Some("https://www.163.com/cover.jpg")
    );
    assert!(
        p.rich.is_none(),
        "generic carries no rich: the wire format is Signal's"
    );

    // Only <title>: still a generic preview (title-meta may fill the snapshot).
    let script = Script {
        replies: HashMap::from([(url.to_owned(), page(url, "<title>大麦 · 演出</title>"))]),
        ..Default::default()
    };
    let r = run(url, &script);
    assert_eq!(r.out.level, Level::Generic);
    // The icon fallback asked for /apple-touch-icon.png as the one image request.
    assert_eq!(
        r.urls().last().map(String::as_str),
        Some("image https://www.163.com/apple-touch-icon.png")
    );

    // Nothing at all: plain link.
    let script = Script {
        replies: HashMap::from([(url.to_owned(), page(url, "<p>hi</p>"))]),
        ..Default::default()
    };
    assert_eq!(run(url, &script).out.level, Level::PlainLink);

    // Legacy Chinese charsets are decoded (GBK / GB2312 / GB18030 / Big5), whether the header or
    // the page's own <meta> declares them; anything else keeps its image but none of its text.
    let legacy = |content_type: &'static str, body: &[u8]| {
        let reply = Reply::Http {
            status: 200,
            final_url: url.into(),
            content_type,
            location: None,
            body: body.to_vec(),
        };
        run(
            url,
            &Script {
                replies: HashMap::from([(url.to_owned(), reply)]),
                ..Default::default()
            },
        )
    };
    let title = |r: &Run| r.out.preview.as_ref().and_then(|p| p.title.clone());
    let r = legacy("text/html; charset=gbk", b"<title>\xcd\xf8\xd2\xd7</title>");
    assert_eq!(
        (r.out.level, title(&r).as_deref()),
        (Level::Generic, Some("网易"))
    );
    let r = legacy(
        "text/html",
        b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=gb2312\"><meta property=\"og:title\" content=\"\xcd\xf8\xd2\xd7 &amp; \x952\x826\">",
    );
    assert_eq!(
        title(&r).as_deref(),
        Some("网易 & 𠀀"),
        "GB18030 four-byte sequences too"
    );
    let r = legacy(
        "text/html; charset=Big5",
        b"<title>\xba\xf4\xa9\xf6\xb7s\xbbD</title>",
    );
    assert_eq!(title(&r).as_deref(), Some("網易新聞"));
    let r = legacy(
        "text/html; charset=shift_jis",
        b"<title>\x93\xfa\x96{\x8c\xea</title>",
    );
    assert_eq!(r.out.level, Level::PlainLink);
    assert!(r.out.failures.contains(&Failure::Charset));
}

#[test]
fn icon_fallback_prefers_declared_icons_and_small_og_images() {
    let url = "https://www.zol.com.cn/";
    let html = r#"<meta property="og:title" content="美团"><meta property="og:image" content="https://p0.meituan.net/og.png">
        <meta property="og:image:width" content="100"><meta property="og:image:height" content="100">
        <link rel="icon" sizes="32x32" href="/f32.png"><link rel="apple-touch-icon" sizes="180x180" href="/t180.png">"#;
    let script = Script {
        replies: HashMap::from([(url.to_owned(), page(url, html))]),
        image_ok: true,
        ..Default::default()
    };
    let r = run(url, &script);
    assert_eq!(
        r.urls().last().map(String::as_str),
        Some("image https://www.zol.com.cn/t180.png")
    );
}

#[test]
fn first_party_objects() {
    // Usernames: no request, name computed from the URL.
    let r = run("https://tell.cc/ceshi.57", &Script::default());
    assert_eq!(r.out.level, Level::FirstParty);
    assert!(r.requests.is_empty());
    assert_eq!(
        r.out.preview.as_ref().unwrap().title.as_deref(),
        Some("@ceshi.57")
    );
    assert_eq!(r.rich().kind.as_deref(), Some("tellomi.user"));
    let r = run_with(
        &base_registry(),
        "https://tell.cc/u#p/+8613800000006",
        &SendContext {
            locale: "zh-CN".into(),
            ..Default::default()
        },
        &Script::default(),
        None,
    );
    assert_eq!(
        r.out.preview.as_ref().unwrap().title.as_deref(),
        Some("Tellomi 用户")
    );

    // Group: Signal's own lookup.
    let group = "https://tell.cc/g#AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let ok = Script {
        first_party: Some(FirstPartyResult {
            ok: true,
            title: Some("周末爬山群".into()),
            member_count: Some(12),
            ..Default::default()
        }),
        ..Default::default()
    };
    let r = run(group, &ok);
    assert_eq!(r.urls(), vec!["first-party tellomi.group".to_owned()]);
    assert_eq!(r.out.level, Level::FirstParty);
    assert_eq!(r.attr("member_count").as_deref(), Some("12"));
    // The fragment (the invite secret) never leaves the device: not even in canonical_url.
    assert_eq!(r.rich().canonical_url.as_deref(), Some("https://tell.cc/g"));

    let invalid = Script {
        first_party: Some(FirstPartyResult {
            ok: false,
            invalid: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let r = run(group, &invalid);
    assert_eq!(
        (r.out.level, r.out.group_link_invalid),
        (Level::PlainLink, true)
    );
    let offline = Script {
        first_party: Some(FirstPartyResult {
            ok: false,
            ..Default::default()
        }),
        ..Default::default()
    };
    let r = run(group, &offline);
    assert_eq!(
        (r.out.level, r.out.group_link_invalid),
        (Level::PlainLink, false)
    );

    // Call with an unnamed room: still a card.
    let call = Script {
        first_party: Some(FirstPartyResult {
            ok: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let r = run(
        "https://tell.cc/call/#key=bcdf-ghkm-npqr-stxz-bcdf-ghkm-npqr-stxz",
        &call,
    );
    assert_eq!(r.out.level, Level::FirstParty);
}

#[test]
fn official_site_page_goes_to_the_snapshot_only() {
    let url = "https://tellomi.app/download/";
    let script = Script {
        replies: HashMap::from([(
            url.to_owned(),
            page(url, r#"<meta property="og:title" content="下载 Tellomi">"#),
        )]),
        ..Default::default()
    };
    let r = run(url, &script);
    assert_eq!(r.out.level, Level::FirstParty);
    assert_eq!(
        r.out.preview.as_ref().unwrap().title.as_deref(),
        Some("下载 Tellomi")
    );
    assert_eq!(r.rich().kind.as_deref(), Some("tellomi.official"));
    // No OG at all: the card is still the official card (§5.2 last row).
    let script = Script {
        replies: HashMap::from([(url.to_owned(), page(url, ""))]),
        ..Default::default()
    };
    assert_eq!(
        run(url, &script).out.preview.unwrap().title.as_deref(),
        Some("Tellomi")
    );
    // A host that is in the compiled bound but not enabled by tellomi.toml is just a web page.
    let other = "https://www.tellomi.app/download/";
    let script = Script {
        replies: HashMap::from([(other.to_owned(), page(other, "<title>x</title>"))]),
        ..Default::default()
    };
    assert_eq!(run(other, &script).out.level, Level::Generic);
}

#[test]
fn policy_hit_voids_the_card_but_not_the_message() {
    let lexicon = json!({"name": "policy", "version": 1, "schema": 1, "payload": {"rules": [{
        "id": "test:1", "term": "赌博网站", "normalized": "赌博网站", "skeleton": "",
        "matches": ["CONTAINS"], "fields": ["link_preview"], "regions": ["global"],
        "outcome": "content_restricted", "source": "test"}]}});
    let policy = PolicyEngine::load(&serde_json::to_vec(&lexicon).unwrap()).expect("lexicon");
    let url = "https://www.163.com/news/article/K1234.html";
    let hit = r#"<meta property="og:title" content="最新赌博网站推荐">"#;
    let script = Script {
        replies: HashMap::from([(url.to_owned(), page(url, hit))]),
        ..Default::default()
    };
    let r = run_with(
        &base_registry(),
        url,
        &SendContext::default(),
        &script,
        Some(&policy),
    );
    assert_eq!(r.out.level, Level::PlainLink);
    assert!(r.out.preview.is_none());
    assert!(r.out.failures.contains(&Failure::PolicyHit));
    // Invisible characters or bidi marks in an ordinary title are not a policy hit.
    let clean = "<meta property=\"og:title\" content=\"\u{200F}سلام 👨\u{200D}👩\u{200D}👧\">";
    let script = Script {
        replies: HashMap::from([(url.to_owned(), page(url, clean))]),
        ..Default::default()
    };
    let r = run_with(
        &base_registry(),
        url,
        &SendContext::default(),
        &script,
        Some(&policy),
    );
    assert_eq!(r.out.level, Level::Generic);
}

#[test]
fn invalid_attrs_are_dropped_one_by_one() {
    let html = bilibili_page().replace("PT00H08M19S", "eight minutes");
    let script = Script {
        replies: HashMap::from([(BV.to_owned(), page(&format!("{BV}/"), &html))]),
        image_ok: true,
        ..Default::default()
    };
    let r = run(BV, &script);
    assert_eq!(r.out.level, Level::Structured);
    assert_eq!(r.attr("duration_ms"), None);
    assert_eq!(r.attr("author").as_deref(), Some("柯洁"));
}

#[test]
fn outcomes_serialize_for_the_bridges() {
    let r = run(
        "https://item.taobao.com/item.htm?id=100032608854",
        &Script::default(),
    );
    let v = serde_json::to_value(&r.out).expect("serializes");
    assert_eq!(v["level"], "brand");
    assert_eq!(v["preview"]["rich"]["kind"], "product");
    let req = base_registry()
        .begin(BV, &SendContext::default())
        .next_request()
        .expect("request");
    let v = serde_json::to_value(&req).expect("serializes");
    assert_eq!(
        (v["type"].as_str(), v["user_agent"].as_str()),
        (Some("fetch"), Some("WhatsApp/2"))
    );
}

// ------------------------------------------------ links/ #1426 semantics (coordinator 2026-09-27)

#[test]
fn plus_in_query_values_is_a_space() {
    // Amap's newer share links write spaces in `p` as `+` (links/README.md §10, corpus amap #6).
    let url = "https://wb.amap.com/?p=B00156NZVG%2C31.247461353927147%2C121.4993718266487%2CNorth+Bund+Green+Land%2CDongdaming+Road+558-678&src=app_share";
    let got = base_registry().identify(url, false).expect("amap");
    assert_eq!(
        got.captures.get("name").map(String::as_str),
        Some("North Bund Green Land")
    );
    let r = run(url, &Script::default());
    assert!(r.requests.is_empty(), "url-only");
    assert_eq!(r.out.level, Level::Structured);
    assert_eq!(r.attr("name").as_deref(), Some("North Bund Green Land"));
    assert_eq!(
        r.attr("address").as_deref(),
        Some("Dongdaming Road 558-678")
    );
}

#[test]
fn hash_routes_fetch_the_rewritten_canonical_url() {
    // The fragment never reaches the server: fetching the typed URL would return the home page.
    let typed = "https://music.163.com/#/song?id=428350227";
    let canonical = "https://music.163.com/song?id=428350227";
    let html = r#"<meta property="og:title" content="海阔天空 - 单曲 - 网易云音乐">
        <meta property="og:image" content="https://p1.music.126.net/cover.jpg">
        <meta property="og:music:artist" content="Beyond">"#;
    let script = Script {
        replies: HashMap::from([(canonical.to_owned(), page(canonical, html))]),
        image_ok: true,
        ..Default::default()
    };
    let r = run(typed, &script);
    assert_eq!(r.urls().first().map(String::as_str), Some(canonical));
    assert_eq!(r.out.level, Level::Structured, "{:?}", r.out.failures);
    assert_eq!(
        r.out.preview.as_ref().unwrap().url,
        typed,
        "what opens is still the typed URL"
    );
    assert_eq!(
        r.out.preview.as_ref().unwrap().title.as_deref(),
        Some("海阔天空")
    );
    assert_eq!(r.attr("artist").as_deref(), Some("Beyond"));
    assert_eq!(r.rich().canonical_url.as_deref(), Some(canonical));
}

#[test]
fn js_redirect_short_domains_are_brand_shells_without_a_request() {
    // m.tb.cn / u.jd.com answer 200 with a JS page and no Location: they sit in `domains`.
    for (url, provider) in [
        ("https://m.tb.cn/h.TBxLfpbarZpWrRH", "taobao"),
        ("https://u.jd.com/EDB3Atu", "jd"),
    ] {
        let r = run(url, &Script::default());
        assert!(r.requests.is_empty(), "{url}");
        assert_eq!(
            (r.out.level, r.out.provider.as_deref()),
            (Level::Brand, Some(provider)),
            "{url}"
        );
    }
}

fn with_fallback(id: &str, value: &str) -> Registry {
    let mut providers = base_providers();
    providers.get_mut(id).unwrap()["fallback"] = json!(value);
    load_providers(&providers).expect("loads")
}

#[test]
fn fallback_generic_uses_the_page_preview_when_no_route_matches() {
    let issue = "https://github.com/signalapp/Signal-Android/issues/10000";
    // `fallback = "brand"` (the default when the field is absent): a brand shell, no request.
    let r = run_with(
        &with_fallback("github", "brand"),
        issue,
        &SendContext::default(),
        &Script::default(),
        None,
    );
    assert_eq!(
        (r.out.level, r.out.kind.as_deref()),
        (Level::Brand, Some("web"))
    );
    assert!(r.requests.is_empty());

    // `fallback = "generic"` (what links/providers/structured/github.toml says since #1427): the
    // generic OG path, same fetcher contract, no rich.
    let registry = base_registry();
    let html = r#"<meta property="og:title" content="Crash on startup · Issue #10000 · signalapp/Signal-Android">
        <meta property="og:image" content="https://opengraph.githubassets.com/x/signalapp/Signal-Android/issues/10000">"#;
    let script = Script {
        replies: HashMap::from([(issue.to_owned(), page(issue, html))]),
        image_ok: true,
        ..Default::default()
    };
    let r = run_with(&registry, issue, &SendContext::default(), &script, None);
    assert_eq!(r.out.level, Level::Generic, "{:?}", r.out.failures);
    let draft = r.out.preview.as_ref().unwrap();
    assert!(draft.rich.is_none());
    assert_eq!(
        draft.title.as_deref(),
        Some("Crash on startup · Issue #10000 · signalapp/Signal-Android")
    );
    // A receiver lands on the same level.
    let card = registry.classify(
        &PreviewInput {
            url: issue.into(),
            title: draft.title.clone(),
            has_image: true,
            ..Default::default()
        },
        issue,
        &MessageContext::default(),
    );
    assert_eq!(card.level, Level::Generic);

    // Routes still win: a repo is structured (REST), a site page is still a brand shell.
    let r = run_with(
        &registry,
        "https://github.com/features/copilot",
        &SendContext::default(),
        &Script::default(),
        None,
    );
    assert_eq!(
        (r.out.level, r.out.kind.as_deref()),
        (Level::Brand, Some("web"))
    );
    assert!(r.requests.is_empty());
}

#[test]
fn fallback_generic_respects_the_region_gate() {
    let registry = with_fallback("youtube", "generic");
    let cn = SendContext {
        region: Region::Cn,
        ..Default::default()
    };
    let r = run_with(
        &registry,
        "https://www.youtube.com/feed/trending",
        &cn,
        &Script::default(),
        None,
    );
    assert!(
        r.requests.is_empty(),
        "never asks a host that is unreachable from here"
    );
    assert_eq!(r.out.level, Level::Brand);
}

#[test]
fn fallback_generic_is_dropped_where_it_may_not_apply() {
    // Payment / ride-hailing (L10) and tellomi: a compatibility drop back to brand, not a reject.
    for (id, url, level) in [
        (
            "alipay",
            "https://render.alipay.com/p/f/fd-j5rqp49m/index.html",
            Level::Brand,
        ),
        ("tellomi", "https://tell.cc/app", Level::PlainLink),
        // L24: a brand-tier platform (not payment) may not fall through to generic either.
        ("taobao", "https://item.taobao.com/", Level::Brand),
    ] {
        let registry = with_fallback(id, "generic");
        let dropped = registry.degraded_routes();
        assert_eq!(dropped.len(), 1, "{id}: {dropped:?}");
        assert_eq!(
            (dropped[0].provider.as_str(), dropped[0].route.as_str()),
            (id, "*")
        );
        let r = run_with(
            &registry,
            url,
            &SendContext::default(),
            &Script::default(),
            None,
        );
        assert!(r.requests.is_empty(), "{id}");
        assert_eq!(r.out.level, level, "{id}");
    }
    // A value this build does not know is ignored the same way.
    let registry = with_fallback("github", "maybe");
    assert_eq!(registry.degraded_routes()[0].route, "*");
    let r = run_with(
        &registry,
        "https://github.com/signalapp/Signal-Android/issues/10000",
        &SendContext::default(),
        &Script::default(),
        None,
    );
    assert_eq!(r.out.level, Level::Brand);
}
