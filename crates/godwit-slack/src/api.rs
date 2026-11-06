// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Slack's Web API (<https://api.slack.com/methods>) as the web client
//! calls it: `POST https://{workspace}.slack.com/api/{method}`, the client
//! token in the form, the `d` cookie as a cookie. Answers are
//! `{"ok": true, …}` or `{"ok": false, "error": "…"}`.
//!
//! Requests go one at a time per workspace, at least [`SPACING`] apart; a
//! 429 pauses for its `Retry-After` and isn't replayed (Slack's tiers allow
//! 20–100 calls a minute per method).

use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::header::{self, HeaderValue};
use reqwest::{Client, StatusCode};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::session::{Team, valid_team_url};

const SPACING: Duration = Duration::from_millis(300);
/// History pages are this long.
pub const PAGE: usize = 50;
/// Most channel list pages followed (200 a page).
const MAX_PAGES: usize = 20;

/// A refusal in Slack's words (`not_authed`, `channel_not_found`, …).
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let words = match self.0.as_str() {
            "not_authed" | "invalid_auth" | "token_revoked" | "account_inactive" => {
                "Slack no longer accepts this sign-in"
            }
            "ratelimited" => "Slack asked Godwit to slow down; try again in a moment",
            "channel_not_found" => "Slack can't find that channel",
            "not_in_channel" => "You aren't in that channel",
            "msg_too_long" => "That message is too long for Slack",
            "cant_update_message" | "cant_delete_message" => "Slack doesn't let you change that message",
            _ => "",
        };
        if words.is_empty() {
            write!(f, "Slack refused: {}", self.0)
        } else {
            f.write_str(words)
        }
    }
}

impl std::error::Error for Refused {}

impl Refused {
    /// The sign-in is dead: sign in again.
    pub fn signed_out(&self) -> bool {
        matches!(
            self.0.as_str(),
            "not_authed" | "invalid_auth" | "token_revoked" | "account_inactive"
        )
    }
}

/// HTTPS with ring, no retries, no cookie jar (the one cookie is set by hand).
pub fn client() -> Result<Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut headers = header::HeaderMap::new();
    headers.insert(header::USER_AGENT, HeaderValue::from_static("Godwit"));
    Client::builder()
        .default_headers(headers)
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(60))
        .build()
        .context("could not set up HTTPS")
}

/// One workspace's API.
pub struct Api {
    http: Client,
    team: Team,
    d: String,
    pace: Mutex<Instant>,
    /// Whose presence the websocket follows (`rtm::run`).
    presence: tokio::sync::watch::Sender<Vec<String>>,
    active: tokio::sync::watch::Sender<bool>,
}

/// `application/x-www-form-urlencoded`, by hand.
use godwit_core::encoding::form;

impl Api {
    pub fn new(http: Client, team: Team, d: String) -> Result<Self> {
        if !valid_team_url(&team.url) {
            bail!("that isn't a Slack workspace's address");
        }
        Ok(Self {
            http,
            team,
            d,
            pace: Mutex::new(Instant::now()),
            presence: tokio::sync::watch::Sender::new(Vec::new()),
            active: tokio::sync::watch::Sender::new(false),
        })
    }

    /// Follow these people's presence (replacing the list before).
    pub fn follow_presence(&self, ids: Vec<String>) {
        self.presence.send_if_modified(|now| {
            let changed = *now != ids;
            *now = ids;
            changed
        });
    }

    pub fn active_list(&self) -> tokio::sync::watch::Receiver<bool> {
        self.active.subscribe()
    }
    pub fn keep_active(&self, enabled: bool) {
        self.active.send_if_modified(|current| {
            if *current == enabled {
                return false;
            }
            *current = enabled;
            true
        });
    }

    /// Official users.getPresence and users.profile.get, on connect or a user
    /// action only. An online transport is not proof of Available presence.
    pub async fn own_state(&self) -> Result<Value> {
        let mut presence = self.call("users.getPresence", &[]).await?;
        self.keep_active(presence["manual_away"] != true);
        if let Ok(profile) = self.call("users.profile.get", &[]).await {
            presence["profile"] = profile["profile"].clone();
        }
        Ok(presence)
    }

    /// The followed list, for the websocket.
    pub fn presence_list(&self) -> tokio::sync::watch::Receiver<Vec<String>> {
        self.presence.subscribe()
    }

    pub fn team(&self) -> &Team {
        &self.team
    }

    /// The `d` cookie as a header value, marked sensitive (the websocket
    /// wants it too, as the web client sends it).
    pub fn cookie(&self) -> Result<HeaderValue> {
        let mut cookie = HeaderValue::from_str(&format!("d={}", self.d)).context("the cookie isn't valid")?;
        cookie.set_sensitive(true);
        Ok(cookie)
    }

    /// Calls `method` with `args`; `Ok` is the whole answer.
    pub async fn call(&self, method: &str, args: &[(&str, &str)]) -> Result<Value> {
        {
            let mut next = self.pace.lock().await;
            tokio::time::sleep_until(*next).await;
            *next = Instant::now() + SPACING;
        }
        let mut pairs: Vec<(&str, &str)> = vec![("token", &self.team.token)];
        pairs.extend_from_slice(args);
        let mut cookie = HeaderValue::from_str(&format!("d={}", self.d)).context("the cookie isn't valid")?;
        cookie.set_sensitive(true);
        let url = format!("{}/api/{method}", self.team.url.trim_end_matches('/'));
        let answer = self
            .http
            .post(url)
            .header(header::COOKIE, cookie)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form(&pairs))
            .send()
            .await
            .context("Can't reach Slack.")?;
        if answer.status() == StatusCode::TOO_MANY_REQUESTS {
            let wait = godwit_core::backend::retry_after(
                answer.headers().get(header::RETRY_AFTER).and_then(|v| v.to_str().ok()),
                Duration::from_secs(60),
            );
            *self.pace.lock().await = godwit_core::backend::checked_deadline(wait);
            bail!(Refused("ratelimited".into()));
        }
        let status = answer.status();
        let body: Value = answer
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("Slack's answer couldn't be read ({status})."))?;
        if body["ok"] != true {
            bail!(Refused(body["error"].as_str().unwrap_or("unknown_error").to_owned()));
        }
        Ok(body)
    }

    /// Every conversation we're in: channels, private channels, DMs, group DMs.
    pub async fn conversations(&self) -> Result<Vec<Value>> {
        let mut all = Vec::new();
        let mut cursor = String::new();
        for _ in 0..MAX_PAGES {
            let mut args = vec![
                ("types", "public_channel,private_channel,mpim,im"),
                ("exclude_archived", "true"),
                ("limit", "200"),
            ];
            if !cursor.is_empty() {
                args.push(("cursor", &cursor));
            }
            let v = self.call("users.conversations", &args).await?;
            all.extend(v["channels"].as_array().cloned().unwrap_or_default());
            cursor = v["response_metadata"]["next_cursor"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if cursor.is_empty() {
                break;
            }
        }
        Ok(all)
    }

    /// The workspace's people (`users.list`, as the web client's member
    /// directory): id, name and picture; deactivated accounts and bots left out.
    pub async fn directory(&self) -> Result<Vec<(String, String, Option<String>)>> {
        let mut all = Vec::new();
        let mut cursor = String::new();
        for _ in 0..MAX_PAGES {
            let mut args = vec![("limit", "200")];
            if !cursor.is_empty() {
                args.push(("cursor", &cursor));
            }
            let v = self.call("users.list", &args).await?;
            for u in v["members"].as_array().into_iter().flatten() {
                let skip = u["deleted"].as_bool() == Some(true)
                    || u["is_bot"].as_bool() == Some(true)
                    || u["id"].as_str() == Some("USLACKBOT");
                if skip {
                    continue;
                }
                if let (Some(id), Some(name)) = (u["id"].as_str(), user_name(u)) {
                    all.push((id.to_owned(), name, user_picture(u)));
                }
            }
            cursor = v["response_metadata"]["next_cursor"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if cursor.is_empty() {
                break;
            }
        }
        Ok(all)
    }

    /// Unread state for every conversation (the web client's `client.counts`).
    pub async fn counts(&self) -> Result<Value> {
        self.call("client.counts", &[("thread_counts_by_channel", "false")])
            .await
    }

    /// A page of history, newest first; `before` (a `ts`) pages back.
    pub async fn history(&self, channel: &str, before: Option<&str>) -> Result<Value> {
        let limit = PAGE.to_string();
        let mut args = vec![("channel", channel), ("limit", limit.as_str())];
        if let Some(b) = before {
            args.push(("latest", b));
            args.push(("inclusive", "false"));
        }
        self.call("conversations.history", &args).await
    }

    /// Posts `text`, as a reply in the thread `thread` (its root's `ts`) if given.
    pub async fn post(&self, channel: &str, thread: Option<&str>, text: &str) -> Result<Value> {
        let mut args = vec![("channel", channel), ("text", text)];
        if let Some(t) = thread {
            args.push(("thread_ts", t));
        }
        self.call("chat.postMessage", &args).await
    }

    /// Slack's current upload flow (files.upload was retired): obtain a
    /// signed upload URL, send bytes, then share all files in one message.
    pub async fn post_files(
        &self,
        channel: &str,
        thread: Option<&str>,
        text: &str,
        files: &[godwit_core::application::Outgoing],
    ) -> Result<Value> {
        if files.is_empty() {
            return self.post(channel, thread, text).await;
        }
        if files.len() > 10 {
            bail!("Attach at most ten files at once.");
        }
        let upload_http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(120))
            .build()
            .context("could not prepare upload")?;
        let mut uploaded = Vec::new();
        for file in files {
            let bytes = godwit_core::media::read_upload(file)
                .await
                .map_err(anyhow::Error::msg)?;
            let length = bytes.len().to_string();
            let plan = self
                .call(
                    "files.getUploadURLExternal",
                    &[("filename", file.name.as_ref()), ("length", &length)],
                )
                .await?;
            let url = plan["upload_url"]
                .as_str()
                .and_then(godwit_core::media::https)
                .context("Slack gave no upload URL")?;
            if !url
                .host_str()
                .is_some_and(|h| h == "slack.com" || h.ends_with(".slack.com"))
            {
                bail!("Slack gave an unexpected upload host.");
            }
            let id = plan["file_id"].as_str().context("Slack gave no file id")?.to_owned();
            let answer = upload_http
                .post(url)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(bytes)
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("Could not upload the attachment."))?;
            if !answer.status().is_success() {
                bail!("Slack refused the attachment ({}).", answer.status());
            }
            uploaded.push(serde_json::json!({"id":id,"title":file.name.as_ref()}));
        }
        let encoded = Value::Array(uploaded.clone()).to_string();
        let mut args = vec![
            ("files", encoded.as_str()),
            ("channel_id", channel),
            ("initial_comment", text),
        ];
        if let Some(thread) = thread {
            args.push(("thread_ts", thread));
        }
        self.call("files.completeUploadExternal", &args).await?;
        // files.info supplies private download URLs and the message's ts in
        // shares; no polling or a second comment/message is needed.
        let mut files = Vec::new();
        let mut timestamp = None;
        for file in uploaded {
            let id = file["id"].as_str().context("missing uploaded file id")?;
            let info = self.call("files.info", &[("file", id)]).await?;
            let file = info["file"].clone();
            if timestamp.is_none() {
                timestamp = crate::media::shared_timestamp(&file, channel);
            }
            files.push(file);
        }
        let ts = timestamp
            .context("The files were uploaded, but Slack gave no shared message. Reopen the chat to see them.")?;
        Ok(serde_json::json!({"message":{"ts":ts,"user":self.team.user,"text":text,"files":files}}))
    }

    /// A thread: its root first, then the replies, oldest first. Pages by
    /// `response_metadata.next_cursor` (as the web client paginates).
    pub async fn replies(&self, channel: &str, ts: &str, cursor: Option<&str>) -> Result<Value> {
        let mut args = vec![("channel", channel), ("ts", ts), ("limit", "200")];
        if let Some(c) = cursor {
            args.push(("cursor", c));
        }
        self.call("conversations.replies", &args).await
    }

    /// One page of the web client's Threads view (newest activity first),
    /// including roots in unopened channels, and the cursor for the next
    /// older page if there is one. Protocol reference: gammons/slk
    /// internal/slack/client.go, ListThreadSubscriptions.
    pub async fn threads(&self, cursor: Option<&str>) -> Result<(Vec<Value>, Option<String>)> {
        let mut args = vec![
            ("limit", "100"),
            ("fetch_threads_state", "true"),
            ("priority_mode", "all"),
        ];
        if let Some(cursor) = cursor {
            args.push(("current_ts", cursor));
        }
        let page = self.call("subscriptions.thread.getView", &args).await?;
        let items: Vec<Value> = page["threads"]
            .as_array()
            .context("Slack returned no thread list")?
            .iter()
            .take(100)
            .cloned()
            .collect();
        let next = items
            .last()
            .and_then(|item| {
                let root = &item["root_msg"];
                root["latest_reply"].as_str().or_else(|| root["thread_ts"].as_str())
            })
            .filter(|next| !next.is_empty() && cursor.is_none_or(|c| *next < c))
            .map(str::to_owned);
        // A cursor that doesn't move back would repeat the same page.
        let next = if page["has_more"] == true { next } else { None };
        Ok((items, next))
    }

    pub async fn mark_thread(&self, channel: &str, root: &str, ts: &str) -> Result<()> {
        self.call(
            "subscriptions.thread.mark",
            &[("channel", channel), ("thread_ts", root), ("ts", ts), ("read", "1")],
        )
        .await
        .map(drop)
    }

    pub async fn update(&self, channel: &str, ts: &str, text: &str) -> Result<Value> {
        self.call("chat.update", &[("channel", channel), ("ts", ts), ("text", text)])
            .await
    }

    pub async fn delete(&self, channel: &str, ts: &str) -> Result<()> {
        self.call("chat.delete", &[("channel", channel), ("ts", ts)])
            .await
            .map(drop)
    }

    pub async fn react(&self, channel: &str, ts: &str, name: &str, add: bool) -> Result<()> {
        let method = if add { "reactions.add" } else { "reactions.remove" };
        self.call(method, &[("channel", channel), ("timestamp", ts), ("name", name)])
            .await
            .map(drop)
    }

    pub async fn mark(&self, channel: &str, ts: &str) -> Result<()> {
        self.call("conversations.mark", &[("channel", channel), ("ts", ts)])
            .await
            .map(drop)
    }

    pub async fn user(&self, id: &str) -> Result<Value> {
        self.call("users.info", &[("user", id)]).await
    }

    /// An app's bot: its name and icons (`bots.info`).
    pub async fn bot(&self, id: &str) -> Result<Value> {
        self.call("bots.info", &[("bot", id)]).await
    }

    /// The websocket's address, for this connection only.
    pub async fn rtm_url(&self) -> Result<String> {
        let v = self.call("rtm.connect", &[]).await?;
        let url = v["url"].as_str().unwrap_or_default();
        if !url.starts_with("wss://") {
            bail!("Slack's websocket address isn't secure");
        }
        let host = url
            .trim_start_matches("wss://")
            .split(['/', '?', ':'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !host.ends_with(".slack.com") && !host.ends_with(".slack-msgs.com") {
            bail!("Slack named a websocket host that isn't Slack's");
        }
        Ok(url.to_owned())
    }
}

/// A picture Slack serves (its CDN, or Gravatar for some profiles), over
/// HTTPS: only those are loaded.
pub fn picture(url: &str) -> Option<String> {
    let host = url
        .strip_prefix("https://")?
        .split(['/', '?', '#', ':'])
        .next()?
        .to_ascii_lowercase();
    (host.ends_with(".slack-edge.com") || host == "secure.gravatar.com" || host.ends_with(".slack.com"))
        .then(|| url.to_owned())
}

/// A user's picture (72 px, as the web client's lists use).
pub fn user_picture(u: &Value) -> Option<String> {
    let p = &u["profile"];
    [p["image_72"].as_str(), p["image_48"].as_str()]
        .into_iter()
        .flatten()
        .find_map(picture)
}

/// A user's shown name: their display name, else their real name, else the id.
pub fn user_name(u: &Value) -> Option<String> {
    let p = &u["profile"];
    [
        p["display_name"].as_str(),
        p["real_name"].as_str(),
        u["real_name"].as_str(),
        u["name"].as_str(),
    ]
    .into_iter()
    .flatten()
    .find(|s| !s.is_empty())
    .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn refusals_read_plainly_and_signed_out_is_known() {
        assert!(Refused("invalid_auth".into()).signed_out());
        assert!(!Refused("channel_not_found".into()).signed_out());
        assert_eq!(Refused("weird".into()).to_string(), "Slack refused: weird");
    }

    #[test]
    fn names_prefer_the_display_name() {
        let u = json!({"name": "ada", "profile": {"display_name": "", "real_name": "Ada Lovelace"}});
        assert_eq!(user_name(&u).as_deref(), Some("Ada Lovelace"));
        let u = json!({"name": "ada", "profile": {"display_name": "ada.l"}});
        assert_eq!(user_name(&u).as_deref(), Some("ada.l"));
    }

    #[test]
    fn only_slacks_pictures_load() {
        assert!(picture("https://avatars.slack-edge.com/2024/a_72.png").is_some());
        assert!(picture("https://secure.gravatar.com/avatar/x.jpg").is_some());
        assert!(picture("http://avatars.slack-edge.com/a.png").is_none());
        assert!(picture("https://evil.example/a.png").is_none());
        assert!(picture("https://slack-edge.com.evil.example/a.png").is_none());
    }

    #[test]
    fn foreign_workspaces_are_refused() {
        let team = Team {
            id: "T1".into(),
            name: String::new(),
            url: "https://evil.example/".into(),
            token: "xoxc-x".into(),
            user: String::new(),
        };
        assert!(Api::new(client().unwrap(), team, "d".into()).is_err());
    }
}
