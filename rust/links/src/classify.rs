//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! The receiver side (ADR-0063 §4.2 Validator, §5.1 rule 4, §5.3, §6.1): purely local.
//!
//! Everything the sender wrote is a claim. Provider, first-party status, the official badge and
//! the open target are recomputed from the URL in the message with this device's registry, and
//! the sender's `level` can only lower the result. `classify` is the one function all three
//! clients call at render time, so one message + one registry + one crate version gives one level
//! everywhere.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::card::{self, Level, UserName};
use crate::kinds::{self, WEB};
use crate::limits::*;
use crate::registry::{LocalizedName, Registry, Tier};
use crate::rich::{LEVEL_STRUCTURED, RichContent, is_valid_attr_key};
use crate::urlx;

/// A received `Preview`, as plain data: the upstream fields 1–5 plus the raw bytes of field 1000.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PreviewInput {
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// `Preview.image` is present (whether or not the attachment has been downloaded — the level
    /// never depends on the download, §5.3).
    #[serde(default)]
    pub has_image: bool,
    #[serde(default)]
    pub date: Option<u64>,
    /// `Preview.rich`, exactly as received and stored (§7.4).
    #[serde(default, with = "hex_bytes")]
    pub rich: Option<Vec<u8>>,
}

/// The message around the preview.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageContext {
    #[serde(default)]
    pub is_story: bool,
    /// Content types of the message's attachments. A single long-text attachment
    /// (`text/x-signal-plain`) is the only kind that keeps the preview (§5.1 rule 4).
    #[serde(default)]
    pub attachment_content_types: Vec<String>,
}

const LONG_TEXT: &str = "text/x-signal-plain";

/// A first-party card: what to show comes from the URL and, on the client, the local database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FirstPartyCard {
    /// Name from the URL (`@nickname`), never from the snapshot; the client may replace it with
    /// the local contact's name and avatar.
    User(UserName),
    /// Group name from the snapshot (the only place it can come from, as in Signal).
    Group {
        title: String,
        member_count: Option<u64>,
    },
    Call {
        title: Option<String>,
    },
    Sticker {
        title: String,
        sticker_count: Option<u64>,
    },
    /// Fixed text "Tellomi 官网" + this path + the official badge. Nothing the sender wrote.
    Official {
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CardAttr {
    pub key: String,
    pub value: String,
}

/// What to draw. Serialized as JSON across the bridges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Card {
    pub level: Level,
    pub provider: Option<String>,
    pub provider_name: Option<LocalizedName>,
    pub kind: Option<String>,
    pub route: Option<String>,
    /// Snapshot title to show (generic / structured). Brand shells show the provider name,
    /// first-party cards their own fields.
    pub title: Option<String>,
    pub description: Option<String>,
    /// Validated attrs of a structured card, by key.
    pub attrs: Vec<CardAttr>,
    /// Registrable domain of the URL in the message (card-visual §3.4).
    pub domain: Option<String>,
    /// Only a `tellomi.official` card on an official host (§4.8).
    pub official_badge: bool,
    pub first_party: Option<FirstPartyCard>,
    /// The domain imitates this well-known one: flag it and warn once before opening (§6.1).
    pub lookalike: Option<String>,
    /// Download and show `Preview.image` (§7.4: not for plain links, brand shells, user and
    /// official cards).
    pub show_image: bool,
    /// Brand shell only: the bundled icon's file name (`links/icons/`), drawn by the client from
    /// its own package and never fetched (ADR-0063 §5.1, §九.6). `None` = name + domain only.
    pub icon: Option<String>,
    /// Colour from the image (card-visual §3.3): third-party cards only; never first-party,
    /// payment, or anything in a message request (the client knows the last one).
    pub tintable: bool,
    /// Payment / ride-hailing: brand shell that says where it leads, browser-only (L10, §6.6).
    pub payment: bool,
    /// Why it landed here, for the local debug log (never a URL).
    pub reason: Option<&'static str>,
}

impl Card {
    fn plain(reason: &'static str) -> Card {
        Card {
            level: Level::PlainLink,
            provider: None,
            provider_name: None,
            kind: None,
            route: None,
            title: None,
            description: None,
            attrs: Vec::new(),
            domain: None,
            official_badge: false,
            first_party: None,
            lookalike: None,
            show_image: false,
            icon: None,
            tintable: false,
            payment: false,
            reason: Some(reason),
        }
    }
}

/// Receive-time checks (§7.4): whether to keep the preview at all, and whether to keep `rich`.
/// Everything else is decided at render time by `classify`, so that a later hot update also
/// applies to messages already stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReceiveCheck {
    pub keep_preview: bool,
    pub keep_rich: bool,
}

/// Sender-written text as it may be shown (ADR-0063 §6.1: zero-width and bidi control characters
/// handled, see `text`); `None` when nothing visible is left.
fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(crate::text::display_text)
        .filter(|t| !t.is_empty())
}

fn attachments_allow_preview(msg: &MessageContext) -> bool {
    match msg.attachment_content_types.as_slice() {
        [] => true,
        [only] => only
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case(LONG_TEXT),
        _ => false,
    }
}

impl Registry {
    pub fn receive_check(
        &self,
        preview: &PreviewInput,
        body: &str,
        msg: &MessageContext,
    ) -> ReceiveCheck {
        let keep_preview = urlx::is_valid_preview_url(&preview.url)
            && (msg.is_story || urlx::url_appears_in_body(&preview.url, body));
        let keep_rich = keep_preview
            && preview
                .rich
                .as_deref()
                .is_some_and(|b| RichContent::decode_checked(b).is_some());
        ReceiveCheck {
            keep_preview,
            keep_rich,
        }
    }

    /// Decide the card for a stored preview (§5.1 rule 4). Pure and local.
    pub fn classify(&self, preview: &PreviewInput, body: &str, msg: &MessageContext) -> Card {
        if !attachments_allow_preview(msg) {
            return Card::plain("attachments");
        }
        if !urlx::is_valid_preview_url(&preview.url) {
            return Card::plain("url");
        }
        if !msg.is_story && !urlx::url_appears_in_body(&preview.url, body) {
            return Card::plain("not_in_body");
        }
        let Some(url) = urlx::parse_web_url(&preview.url) else {
            return Card::plain("url");
        };
        let Some(host) = urlx::host_str(&url) else {
            return Card::plain("url");
        };
        let domain = urlx::display_domain(&preview.url);
        if let Some(like) = self.known.lookalike_of(&host) {
            return Card {
                lookalike: Some(like.to_owned()),
                domain,
                ..Card::plain("lookalike")
            };
        }
        let rich = preview
            .rich
            .as_deref()
            .and_then(RichContent::decode_checked);
        let title = non_empty(&preview.title);

        if self.is_first_party_url(&url) {
            return self.first_party(&url, &host, preview, rich.as_ref(), title, domain);
        }

        let generic = |reason: &'static str| {
            if title.is_some() || msg.is_story {
                Card {
                    level: Level::Generic,
                    title: title.clone(),
                    description: non_empty(&preview.description),
                    domain: domain.clone(),
                    show_image: preview.has_image,
                    tintable: true,
                    reason: Some(reason),
                    ..Card::plain(reason)
                }
            } else {
                Card {
                    domain: domain.clone(),
                    ..Card::plain("no_title")
                }
            }
        };

        let Some((pid, short, m)) = self.match_parsed(&url, false) else {
            return generic("no_provider");
        };
        let provider = &self.providers[pid];
        let Some(rich) = rich else {
            return generic("no_rich");
        };
        // A rich whose schema this build does not know, or whose kind it cannot render, is
        // ignored: the snapshot is always meaningful on its own (§5.3, §7.1).
        if rich.effective_schema() > RICH_SCHEMA_MAX {
            return generic("rich_schema");
        }
        let Some(rich_kind) = rich.kind.as_deref().filter(|k| kinds::kind(k).is_some()) else {
            return generic("unknown_kind");
        };
        if rich.provider.as_deref() != Some(provider.id.as_str()) {
            return generic("provider_mismatch");
        }
        // Recompute the kind locally. Both the message URL and the sender's canonical_url must
        // land on this same provider; for a short link (which this side never expands) the kind
        // comes from canonical_url, so expansion can refine the kind but never switch provider.
        let canonical_match = match rich.canonical_url.as_deref().and_then(urlx::parse_web_url) {
            Some(c) => match self.match_parsed(&c, false) {
                Some((cpid, false, cm)) if cpid == pid => Some(cm),
                _ => return generic("canonical_mismatch"),
            },
            None if rich.canonical_url.is_some() => return generic("canonical_mismatch"),
            None => None,
        };
        // A short link the sender could not (or was not allowed to) expand has no canonical_url:
        // it can only be the provider's brand shell with type text `web`.
        let local = if short {
            canonical_match.as_ref().and_then(|cm| cm.route)
        } else {
            m.route
        };
        let kind_of =
            |route: Option<usize>| route.map_or(WEB, |r| provider.routes[r].kind.as_str());
        if !short
            && let Some(cm) = &canonical_match
            && kind_of(cm.route) != kind_of(m.route)
        {
            return generic("canonical_kind_mismatch");
        }
        let local_kind = kind_of(local);
        if rich_kind != local_kind {
            return generic("kind_mismatch");
        }
        // Signal's "snapshot must stand on its own": a rich preview without a title is a plain
        // link (Android's "needs a title", folded in here).
        let Some(title) = title else {
            return Card {
                domain,
                ..Card::plain("no_title")
            };
        };

        let kind_def = kinds::kind(local_kind).expect("checked above");
        let mut attrs = BTreeMap::new();
        for a in &rich.attrs {
            let (Some(k), Some(v)) = (a.key.as_deref(), a.value.as_deref()) else {
                continue;
            };
            if !is_valid_attr_key(k) || attrs.contains_key(k) {
                continue;
            }
            if let Some(v) = kind_def.attr_type(k).and_then(|t| t.validate(v)) {
                attrs.insert(k.to_owned(), v);
            }
        }
        let local_structured = provider.tier != Tier::Brand
            && local.is_some_and(|r| provider.routes[r].is_structured())
            && kind_def.meets_required(|f| match f {
                "title" => true,
                "image" => preview.has_image,
                "description" => non_empty(&preview.description).is_some(),
                attr => attrs.contains_key(attr),
            });
        // min(sender's claim, our own judgment): a missing or unknown level counts as brand (§4.5).
        let sender_structured = rich.level == Some(LEVEL_STRUCTURED);
        let level = if local_structured && sender_structured {
            Level::Structured
        } else {
            Level::Brand
        };
        let structured = level == Level::Structured;
        Card {
            level,
            provider: Some(provider.id.clone()),
            provider_name: Some(provider.name.clone()),
            kind: Some(local_kind.to_owned()),
            route: local.map(|r| provider.routes[r].id.clone()),
            title: structured.then_some(title),
            description: if structured {
                non_empty(&preview.description)
            } else {
                None
            },
            attrs: if structured {
                attrs
                    .into_iter()
                    .map(|(key, value)| CardAttr { key, value })
                    .collect()
            } else {
                Vec::new()
            },
            domain,
            official_badge: false,
            first_party: None,
            lookalike: None,
            show_image: structured && preview.has_image,
            icon: if structured {
                None
            } else {
                provider.icon.clone()
            },
            tintable: !provider.locked,
            payment: provider.locked,
            reason: None,
        }
    }

    fn first_party(
        &self,
        url: &url::Url,
        host: &str,
        preview: &PreviewInput,
        rich: Option<&RichContent>,
        title: Option<String>,
        domain: Option<String>,
    ) -> Card {
        let tellomi = self.tellomi();
        let m = tellomi.match_url(url, host);
        // tell.cc paths that are not objects, and official hosts with no official route (how a
        // hot update switches the official card off), are plain links (§4.8, §6.4).
        let Some(route) = m
            .route
            .map(|r| &tellomi.routes[r])
            .filter(|r| r.degraded.is_none())
        else {
            return Card {
                domain,
                ..Card::plain("not_an_object")
            };
        };
        // Attrs only from a rich that agrees about what this is; its level is irrelevant here.
        let rich_attrs: BTreeMap<String, String> = rich
            .filter(|r| {
                r.provider.as_deref() == Some(TELLOMI)
                    && r.kind.as_deref() == Some(route.kind.as_str())
            })
            .map(|r| {
                let kind_def = kinds::kind(&route.kind);
                r.attrs
                    .iter()
                    .filter_map(|a| {
                        let (k, v) = (a.key.as_deref()?, a.value.as_deref()?);
                        let v = kind_def?.attr_type(k)?.validate(v)?;
                        Some((k.to_owned(), v))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let count = |k: &str| rich_attrs.get(k).and_then(|v| v.parse::<u64>().ok());
        let (first_party, show_image, badge) = match route.kind.as_str() {
            "tellomi.user" => (
                FirstPartyCard::User(card::user_name(&m.captures)),
                false,
                false,
            ),
            "tellomi.group" => match title {
                Some(t) => (
                    FirstPartyCard::Group {
                        title: t,
                        member_count: count("member_count"),
                    },
                    preview.has_image,
                    false,
                ),
                None => {
                    return Card {
                        domain,
                        ..Card::plain("no_title")
                    };
                }
            },
            "tellomi.call" => (FirstPartyCard::Call { title }, false, false),
            "tellomi.sticker" => match title {
                Some(t) => (
                    FirstPartyCard::Sticker {
                        title: t,
                        sticker_count: count("sticker_count"),
                    },
                    preview.has_image,
                    false,
                ),
                None => {
                    return Card {
                        domain,
                        ..Card::plain("no_title")
                    };
                }
            },
            "tellomi.official" => (
                FirstPartyCard::Official {
                    path: card::official_path(url),
                },
                false,
                true,
            ),
            _ => {
                return Card {
                    domain,
                    ..Card::plain("not_an_object")
                };
            }
        };
        Card {
            level: Level::FirstParty,
            provider: Some(TELLOMI.to_owned()),
            provider_name: Some(tellomi.name.clone()),
            kind: Some(route.kind.clone()),
            route: Some(route.id.clone()),
            first_party: Some(first_party),
            official_badge: badge,
            domain,
            show_image,
            reason: None,
            ..Card::plain("first_party")
        }
    }
}

/// `rich` travels through JSON as lower-case hex.
mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(b) => s.serialize_str(&b.iter().map(|x| format!("{x:02x}")).collect::<String>()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let Some(text) = Option::<String>::deserialize(d)? else {
            return Ok(None);
        };
        if text.len() % 2 != 0 || !text.is_ascii() {
            return Err(serde::de::Error::custom("not hex"));
        }
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(serde::de::Error::custom))
            .collect::<Result<Vec<u8>, _>>()
            .map(Some)
    }
}
