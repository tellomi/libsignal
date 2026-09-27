//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

//! Things the sender and the receiver must compute identically: the card level ladder, the
//! first-party display rules, and the few fixed strings a snapshot may carry.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The degradation ladder (ADR-0063 §5.1). Every link lands on exactly one rung; failure only
/// ever moves it down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// No preview; the link in the text stays tappable. When the message is only this link the
    /// client still draws a local no-image card (§5.1), which does not change the level.
    PlainLink,
    /// Signal's snapshot preview, no provider.
    Generic,
    /// Platform name + type text + domain (+ bundled icon).
    Brand,
    /// A card rendered by kind from validated attrs.
    Structured,
    /// tell.cc / official site: decided by the receiver from the URL and the local database.
    FirstParty,
}

/// How a `tellomi.user` card names the user, computed from the URL alone (§4.8): `@nickname`
/// (lower-case, `.01` hidden, any other discriminator shown in full), or the fixed generic text
/// when the link does not carry a readable username (`#eu/`, `#p/`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UserName {
    /// `@hk881qb`, `@ceshi.57`; `None` means "show the fixed 'Tellomi user' text".
    pub display: Option<String>,
    /// `hk881qb.01`: the key for a local-database lookup.
    pub username: Option<String>,
}

pub(crate) fn user_name(captures: &BTreeMap<String, String>) -> UserName {
    let (nick, disc) = if let Some(nick) = captures.get("nickname") {
        (nick.clone(), captures.get("discriminator").cloned())
    } else if let Some(full) = captures.get("username") {
        match full.rsplit_once('.') {
            Some((n, d)) if !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()) => {
                (n.to_owned(), Some(d.to_owned()))
            }
            _ => (full.clone(), None),
        }
    } else {
        return UserName {
            display: None,
            username: None,
        };
    };
    let nick = nick.to_ascii_lowercase();
    let disc = disc.unwrap_or_else(|| "01".to_owned());
    // ADR-0066: `.01` is hidden, every other discriminator is shown — `kaixin.57` must never
    // render as `kaixin`.
    let display = if disc == "01" {
        format!("@{nick}")
    } else {
        format!("@{nick}.{disc}")
    };
    UserName {
        display: Some(display),
        username: Some(format!("{nick}.{disc}")),
    }
}

enum Script {
    Hans,
    Hant,
    En,
}

fn script(locale: &str) -> Script {
    let l = locale.to_ascii_lowercase().replace('_', "-");
    if ["zh-hant", "zh-tw", "zh-hk", "zh-mo"]
        .iter()
        .any(|p| l.starts_with(p))
    {
        Script::Hant
    } else if l.is_empty() || l.starts_with("zh") {
        Script::Hans
    } else {
        Script::En
    }
}

/// Snapshot title for a user link that carries no readable username (for old clients only; new
/// receivers always compute the name themselves).
pub(crate) fn generic_user_title(locale: &str) -> &'static str {
    match script(locale) {
        Script::Hans => "Tellomi 用户",
        Script::Hant => "Tellomi 用戶",
        Script::En => "Tellomi user",
    }
}

/// Snapshot title for a call link whose room has no name.
pub(crate) fn generic_call_title(locale: &str) -> &'static str {
    match script(locale) {
        Script::Hans => "Tellomi 通话",
        Script::Hant => "Tellomi 通話",
        Script::En => "Tellomi call",
    }
}

/// Snapshot title for the official site when its page gave nothing (§5.2 last row).
pub(crate) const OFFICIAL_FALLBACK_TITLE: &str = "Tellomi";

/// The path an official card shows next to the badge: the parsed (so already percent-encoded,
/// ASCII) path, never decoded, at most 32 characters then `…` (§4.8).
pub(crate) fn official_path(url: &url::Url) -> String {
    let path = url.path();
    let count = path.chars().count();
    if count <= crate::limits::MAX_OFFICIAL_PATH_CHARS {
        path.to_owned()
    } else {
        let mut cut: String = path
            .chars()
            .take(crate::limits::MAX_OFFICIAL_PATH_CHARS)
            .collect();
        cut.push('…');
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn user_names_follow_adr_0066() {
        let n = user_name(&caps(&[("nickname", "HK881qb")]));
        assert_eq!(n.display.as_deref(), Some("@hk881qb"));
        assert_eq!(n.username.as_deref(), Some("hk881qb.01"));
        let n = user_name(&caps(&[("nickname", "ceshi"), ("discriminator", "57")]));
        assert_eq!(n.display.as_deref(), Some("@ceshi.57"));
        let n = user_name(&caps(&[("nickname", "ceshi"), ("discriminator", "01")]));
        assert_eq!(n.display.as_deref(), Some("@ceshi"));
        let n = user_name(&caps(&[("username", "hk881qb.01")]));
        assert_eq!(n.display.as_deref(), Some("@hk881qb"));
        let n = user_name(&caps(&[]));
        assert_eq!(n.display, None);
    }

    #[test]
    fn official_path_is_not_decoded_and_is_truncated() {
        let u = url::Url::parse("https://tellomi.app/账号异常请回复验证码").unwrap();
        let p = official_path(&u);
        assert!(p.is_ascii() || p.ends_with('…'));
        assert!(p.starts_with("/%E8%B4%A6"));
        assert_eq!(p.chars().count(), 33);
        let short = url::Url::parse("https://tellomi.app/download/").unwrap();
        assert_eq!(official_path(&short), "/download/");
    }
}
