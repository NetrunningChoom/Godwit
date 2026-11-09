// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A channel's webhooks as Discord lists them (Webhook resource,
//! <https://docs.discord.food/resources/webhook>).
//!
//! The token in a webhook object lets anyone post as it, so it lives only in
//! [`HookToken`] (which never prints) and leaves this crate only as the copied
//! URL; the core's list has no token field.

use serde_json::{Value, json};

/// Webhook type 1: an incoming webhook, the only kind with a token.
const INCOMING: u64 = 1;

/// A webhook token: kept out of `Debug` and logs.
#[derive(Clone, PartialEq, Eq)]
pub struct HookToken(String);

impl HookToken {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }
}

impl std::fmt::Debug for HookToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HookToken(..)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hook {
    pub id: String,
    pub name: String,
    pub avatar_url: Option<String>,
    /// None for channel followers and application webhooks.
    pub token: Option<HookToken>,
}

impl Hook {
    /// The URL a service posts to, built only when it is copied.
    pub fn url(&self) -> Option<String> {
        let token = self.token.as_ref()?;
        Some(format!("https://discord.com/api/webhooks/{}/{}", self.id, token.0))
    }
}

/// Reads a `GET /channels/{id}/webhooks` answer; objects without an id are skipped.
pub fn parse_list(list: &Value) -> Vec<Hook> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| {
            let id = w["id"].as_str()?;
            let incoming = w["type"].as_u64().is_none_or(|t| t == INCOMING);
            Some(Hook {
                id: id.to_owned(),
                name: w["name"].as_str().unwrap_or("Webhook").to_owned(),
                avatar_url: w["avatar"]
                    .as_str()
                    .map(|hash| crate::store::avatar_url(id, Some(hash))),
                token: w["token"]
                    .as_str()
                    .filter(|_| incoming)
                    .map(|t| HookToken(t.to_owned())),
            })
        })
        .collect()
}

/// `POST /channels/{id}/webhooks` takes the name.
pub fn create_body(name: &str) -> Value {
    json!({ "name": name.trim() })
}

/// `PATCH /webhooks/{id}`: a rename, and optionally a move to another channel.
pub fn modify_body(name: Option<&str>, channel: Option<&str>) -> Value {
    let mut body = serde_json::Map::new();
    if let Some(name) = name {
        body.insert("name".into(), json!(name.trim()));
    }
    if let Some(channel) = channel {
        body.insert("channel_id".into(), json!(channel));
    }
    Value::Object(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!([
            {"id": "1", "type": 1, "name": "Deploys", "avatar": "abc", "token": "s3cret", "channel_id": "9"},
            {"id": "2", "type": 2, "name": "Follow", "avatar": null},
            {"name": "no id"}
        ])
    }

    #[test]
    fn the_list_keeps_incoming_tokens_and_skips_broken_objects() {
        let hooks = parse_list(&sample());
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].name, "Deploys");
        assert!(
            hooks[0]
                .avatar_url
                .as_deref()
                .is_some_and(|u| u.contains("/avatars/1/abc"))
        );
        assert_eq!(
            hooks[0].url().as_deref(),
            Some("https://discord.com/api/webhooks/1/s3cret")
        );
        assert!(hooks[1].url().is_none() && hooks[1].avatar_url.is_none());
    }

    #[test]
    fn a_token_on_a_follower_is_ignored_and_debug_hides_tokens() {
        let hooks = parse_list(&json!([{"id": "3", "type": 2, "name": "x", "token": "nope"}]));
        assert!(hooks[0].token.is_none());
        let text = format!("{:?}", parse_list(&sample()));
        assert!(!text.contains("s3cret"));
    }

    #[test]
    fn request_bodies_carry_only_what_changed() {
        assert_eq!(create_body("  Hook ").to_string(), r#"{"name":"Hook"}"#);
        assert_eq!(modify_body(Some("A"), None).to_string(), r#"{"name":"A"}"#);
        assert_eq!(modify_body(None, Some("7")).to_string(), r#"{"channel_id":"7"}"#);
    }
}
