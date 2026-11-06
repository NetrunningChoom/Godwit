// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Discord's HTTP API, asked as its web client asks: the same headers and
//! client description on every request, within the rate limits Discord
//! names (see [`crate::limits`]), and never what's known to be refused.
//! Request shapes follow Yapfast's `api.rs` and <https://docs.discord.food>.

use std::sync::RwLock;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::header;
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::Instant;

use crate::client;
use crate::limits::{self, Limits, Refusals, Stopped};

pub const API: &str = "https://discord.com/api/v9";
const WEB_APP: &str = "https://discord.com/app";
const UNAUTHORIZED: &str = "Discord no longer accepts this sign-in. Sign in again.";
/// Messages per history page, as the web client asks.
pub const PAGE: usize = 50;
/// Discord keeps up to 512 characters of an audit log's reason.
const MAX_REASON: usize = 512;
/// Deliberately conservative local policy, in addition to server buckets.
/// Shared across accounts, including anonymous sign-in requests.
const REQUEST_SPACING: Duration = Duration::from_millis(500);
static HTTP_PACE: Mutex<Option<Instant>> = Mutex::const_new(None);
const REQUEST_QUEUE: usize = 16;

/// Discord's own code and words for a refusal.
#[derive(Debug)]
pub struct DiscordError {
    pub status: StatusCode,
    pub code: Option<u64>,
    pub message: String,
}

impl std::fmt::Display for DiscordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.status, self.code) {
            (StatusCode::FORBIDDEN, _) => write!(f, "Discord says you're not allowed to do that ({}).", self.message),
            (StatusCode::NOT_FOUND, _) => write!(f, "Discord doesn't know that any more ({}).", self.message),
            (_, Some(code)) => write!(f, "Discord refused: {} (code {code}).", self.message),
            _ => write!(f, "Discord answered {}.", self.status),
        }
    }
}

impl std::error::Error for DiscordError {}

/// Discord wants a captcha solved before it goes on (a 400 naming
/// `captcha_key`). Godwit can't show hCaptcha yet.
#[derive(Debug)]
pub struct CaptchaRequired;

impl std::fmt::Display for CaptchaRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "Discord wants a captcha solved first, which Godwit can't show yet. Sign in once in Discord's own app or website from this network, then try again.",
        )
    }
}

impl std::error::Error for CaptchaRequired {}

/// Puts an API request's headers in the order the desktop app sends them
/// (a measured session, neucord issue #92); headers it doesn't list, such as
/// `X-Context-Properties` or captcha answers, go after `x-discord-locale`, as
/// the client's own `x-` headers do. reqwest's cookie jar adds `cookie` last.
pub(crate) fn chromium_order(headers: &mut header::HeaderMap) {
    const BEFORE: &[&str] = &[
        "sec-ch-ua-platform",
        "authorization",
        "x-installation-id",
        "x-debug-options",
        "accept-language",
        "sec-ch-ua",
        "sec-ch-ua-mobile",
        "x-discord-timezone",
        "x-super-properties",
        "x-discord-locale",
    ];
    const AFTER: &[&str] = &[
        "user-agent",
        "x-fingerprint",
        "content-type",
        "accept",
        "origin",
        "sec-fetch-site",
        "sec-fetch-mode",
        "sec-fetch-dest",
        "referer",
        "accept-encoding",
        "priority",
    ];
    let mut old = std::mem::take(headers);
    let move_named = |name: &str, into: &mut header::HeaderMap, from: &mut header::HeaderMap| {
        if let Ok(name) = header::HeaderName::from_bytes(name.as_bytes()) {
            for value in from.get_all(&name).iter() {
                into.append(name.clone(), value.clone());
            }
            from.remove(&name);
        }
    };
    for name in BEFORE {
        move_named(name, headers, &mut old);
    }
    let rest: Vec<header::HeaderName> = old.keys().filter(|k| !AFTER.contains(&k.as_str())).cloned().collect();
    for name in rest {
        move_named(name.as_str(), headers, &mut old);
    }
    for name in AFTER {
        move_named(name, headers, &mut old);
    }
}

/// The status Discord refused a request with, if it did.
pub fn status(error: &anyhow::Error) -> Option<StatusCode> {
    error.downcast_ref::<DiscordError>().map(|e| e.status)
}

/// A file uploaded to Discord's storage, for a message to name.
#[derive(Clone, Debug)]
pub struct Upload {
    pub filename: String,
    pub uploaded_filename: String,
}

/// Which page of a channel's history.
#[derive(Clone, Copy, Debug)]
pub enum Span<'a> {
    Latest,
    Before(&'a str),
    Around(&'a str),
}

pub struct Api {
    http: Client,
    /// Empty without a sign-in; Discord may refresh it in READY.
    token: RwLock<String>,
    limits: Limits,
    refusals: Refusals,
    /// Serialize bucket learning so concurrent responses cannot refill a
    /// depleted bucket with an older remaining count.
    requests: Mutex<()>,
    queued: Semaphore,
}

impl Api {
    pub fn new(token: String) -> Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        // Headers are set per request (see `request` and `chromium_order`),
        // so their order is Chromium's rather than reqwest's merge order.
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .cookie_store(true)
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .http2_keep_alive_interval(Duration::from_secs(10))
            .http2_keep_alive_timeout(Duration::from_secs(10))
            .build()
            .context("could not set up HTTPS")?;
        Ok(Self {
            http,
            token: RwLock::new(token),
            limits: Limits::default(),
            refusals: Refusals::default(),
            requests: Mutex::new(()),
            queued: Semaphore::new(REQUEST_QUEUE),
        })
    }

    /// Without a sign-in, for linking one.
    pub fn anonymous() -> Result<Self> {
        Self::new(String::new())
    }

    /// Asks with `token` from now on: Discord refreshed the sign-in to it.
    pub fn relogin(&self, token: &str) {
        *self.token.write().unwrap_or_else(std::sync::PoisonError::into_inner) = token.to_owned();
    }

    fn token(&self) -> String {
        self.token
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The desktop app's version from Discord's update server (as the app
    /// checks for updates at start), then the client's current build number
    /// from its page (the desktop app loads the same client). Once a run.
    pub async fn learn_build_number(&self) {
        const READ: usize = 256 << 10;
        let asked = async {
            let answer: Value = self
                .http
                .get(client::update_url())
                .header(header::ACCEPT, "application/json")
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            anyhow::Ok(client::learn_client_version(&answer))
        };
        match tokio::time::timeout(Duration::from_secs(10), asked).await {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => godwit_core::verbose!("discord: the update server gave no app version"),
            Ok(Err(e)) => godwit_core::verbose!("discord: could not ask the update server: {e}"),
            Err(_) => godwit_core::verbose!("discord: the update server didn't answer"),
        }
        if client::knows_build_number() {
            return;
        }
        let read = async {
            let mut response = self
                .http
                .get(WEB_APP)
                .header(header::ACCEPT, "text/html")
                .send()
                .await?
                .error_for_status()?;
            let mut page = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                page.extend_from_slice(&chunk);
                if client::learn_build_number(&page) || page.len() >= READ {
                    break;
                }
            }
            anyhow::Ok(())
        };
        match tokio::time::timeout(Duration::from_secs(10), read).await {
            Ok(Ok(())) if client::knows_build_number() => {}
            Ok(Ok(())) => godwit_core::verbose!("discord: its web page gave no build number"),
            Ok(Err(e)) => godwit_core::verbose!("discord: could not read its web page: {e}"),
            Err(_) => godwit_core::verbose!("discord: its web page didn't load"),
        }
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        // What the desktop app (Chromium inside Electron) sends with an API
        // fetch; `execute` puts them in Chromium's order. The description
        // changes through a run (its heartbeat session), so each request
        // makes it again, as the client does.
        let cross_origin = !matches!(method, Method::GET | Method::HEAD);
        let mut request = self
            .http
            .request(method, format!("{API}{path}"))
            .header("sec-ch-ua-platform", client::sec_ch_ua_platform())
            .header("x-debug-options", "bugReporterEnabled")
            .header(header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .header("sec-ch-ua", client::sec_ch_ua())
            .header("sec-ch-ua-mobile", "?0")
            .header("x-discord-timezone", client::timezone())
            .header("x-super-properties", client::super_properties())
            .header("x-discord-locale", "en-US")
            .header(header::USER_AGENT, client::user_agent())
            .header(header::ACCEPT, "*/*")
            .header("sec-fetch-site", "same-origin")
            .header("sec-fetch-mode", "cors")
            .header("sec-fetch-dest", "empty")
            .header(header::REFERER, "https://discord.com/channels/@me")
            .header(header::ACCEPT_ENCODING, client::ACCEPT_ENCODING)
            .header("priority", "u=1, i");
        // Chromium sends Origin with fetches other than GET and HEAD.
        if cross_origin {
            request = request.header(header::ORIGIN, "https://discord.com");
        }
        let token = self.token();
        if token.is_empty() {
            request
        } else {
            request.header(header::AUTHORIZATION, token)
        }
    }

    async fn json(&self, request: RequestBuilder) -> Result<Value> {
        let response = self.execute(request).await?;
        let body = response.bytes().await.context("could not read Discord's answer")?;
        if body.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&body).context("could not understand Discord's answer")
    }

    async fn empty(&self, request: RequestBuilder) -> Result<()> {
        self.execute(request).await.map(drop)
    }

    /// Sends once, within server buckets and a process-wide pace limit.
    /// Failed requests are never automatically replayed.
    async fn execute(&self, request: RequestBuilder) -> Result<Response> {
        let (client, request) = request.build_split();
        let mut request = request.context("could not build the request")?;
        chromium_order(request.headers_mut());
        let route = limits::route(request.method(), request.url().path());
        let _queued = self
            .queued
            .try_acquire()
            .context("Too many pending Discord requests. Try again shortly.")?;
        let _serial = tokio::time::timeout(limits::MAX_WAIT, self.requests.lock())
            .await
            .context("Discord requests are still pending. Try again shortly.")?;
        match self.refusals.check() {
            Ok(()) => {}
            Err(Stopped::Unauthorized) => bail!(UNAUTHORIZED),
            Err(Stopped::Refused(wait)) => bail!(
                "Discord refused too many requests lately, so Godwit stopped asking. Try again in {} min.",
                wait.as_secs().div_ceil(60)
            ),
        }
        if let Err(wait) = self.limits.acquire(&route).await {
            bail!("Discord asks Godwit to slow down. Try again in {} s.", wait.as_secs());
        }
        {
            let mut next = HTTP_PACE.lock().await;
            if let Some(due) = *next {
                tokio::time::sleep_until(due).await;
            }
            *next = Some(Instant::now() + REQUEST_SPACING);
        }
        let response = match client.execute(request).await {
            Ok(response) => response,
            Err(error) => {
                self.limits.failed(&route, StatusCode::SERVICE_UNAVAILABLE);
                return Err(error).context("Can't reach Discord.");
            }
        };
        let status = response.status();
        let signed_in = !self.token().is_empty();
        if signed_in {
            self.refusals.answered(status);
        }
        if status == StatusCode::UNAUTHORIZED && signed_in {
            self.refusals.unauthorized();
            bail!(UNAUTHORIZED);
        }
        if status != StatusCode::TOO_MANY_REQUESTS {
            self.limits.update(&route, response.headers());
            if status.is_success() {
                return Ok(response);
            }
            self.limits.failed(&route, status);
            return Err(refusal(status, &body(response).await));
        }
        let headers = response.headers().clone();
        let body = body(response).await;
        self.limits.limited(&route, &headers, &body);
        Err(refusal(status, &body))
    }

    // Signing in and out.

    /// Trades a remote login's ticket for the sign-in, encrypted to the key
    /// that started it, in base64 (see [`crate::remote_auth`]).
    pub async fn remote_auth_login(
        &self,
        ticket: &str,
        solved: Option<(&crate::captcha::Captcha, &str)>,
    ) -> Result<String> {
        let mut request = with_json(
            self.request(Method::POST, "/users/@me/remote-auth/login"),
            &json!({"ticket": ticket}),
        );
        if let Some((captcha, solution)) = solved {
            for (name, value) in captcha.headers(solution) {
                request = request.header(name, value);
            }
        }
        let answer = self.json(request).await?;
        answer["encrypted_token"]
            .as_str()
            .map(str::to_owned)
            .context("Discord didn't hand over the sign-in.")
    }

    /// Ends this sign-in's session at Discord, as signing out of the web
    /// client does. The token stops working.
    pub async fn logout(&self) -> Result<()> {
        let body = json!({"provider": null, "voip_provider": null});
        self.empty(with_json(self.request(Method::POST, "/auth/logout"), &body))
            .await
    }

    // Messages.

    pub async fn messages(&self, channel: &str, span: Span<'_>) -> Result<Vec<Value>> {
        let mut path = format!("/channels/{channel}/messages?limit={PAGE}");
        match span {
            Span::Latest => {}
            Span::Before(id) => path.push_str(&format!("&before={id}")),
            Span::Around(id) => path.push_str(&format!("&around={id}")),
        }
        match self.json(self.request(Method::GET, &path)).await? {
            Value::Array(messages) => Ok(messages),
            _ => bail!("could not understand Discord's answer"),
        }
    }

    /// Sends a message, as a reply when `reply` names one, with uploaded files.
    pub async fn send(
        &self,
        guild: Option<&str>,
        channel: &str,
        content: &str,
        nonce: &str,
        reply: Option<&str>,
        uploads: &[Upload],
    ) -> Result<Value> {
        // The web client's new message body.
        let mut body = json!({
            "mobile_network_type": "unknown",
            "content": content,
            "nonce": nonce,
            "tts": false,
            "flags": 0,
        });
        if !uploads.is_empty() {
            body["attachments"] = uploads
                .iter()
                .enumerate()
                .map(|(i, u)| json!({"id": i.to_string(), "filename": u.filename, "uploaded_filename": u.uploaded_filename}))
                .collect();
        }
        if let Some(reply) = reply {
            body["message_reference"] = json!({
                "guild_id": guild,
                "channel_id": channel,
                "message_id": reply,
                "type": 0,
            });
        }
        let path = format!("/channels/{channel}/messages");
        self.json(with_json(self.request(Method::POST, &path), &body)).await
    }

    /// Uploads a file to Discord's storage as the web client does: asks for
    /// a signed address, then puts the file there.
    pub async fn upload(&self, channel: &str, filename: &str, data: Vec<u8>) -> Result<Upload> {
        let path = format!("/channels/{channel}/attachments");
        let body = json!({"files": [{
            "id": "0",
            "filename": filename,
            "file_size": data.len(),
            "is_clip": false,
        }]});
        let answer = self.json(with_json(self.request(Method::POST, &path), &body)).await?;
        let target = &answer["attachments"][0];
        let (Some(url), Some(uploaded)) = (target["upload_url"].as_str(), target["upload_filename"].as_str()) else {
            bail!("could not understand Discord's answer");
        };
        // The address is signed: no sign-in goes with it.
        let response = self
            .http
            .put(url)
            .body(data)
            .send()
            .await
            .context("Can't reach Discord's file storage.")?;
        if !response.status().is_success() {
            bail!("Discord's file storage answered {}.", response.status());
        }
        Ok(Upload {
            filename: filename.to_owned(),
            uploaded_filename: uploaded.to_owned(),
        })
    }

    pub async fn edit(&self, channel: &str, message: &str, content: &str) -> Result<Value> {
        let path = format!("/channels/{channel}/messages/{message}");
        self.json(with_json(
            self.request(Method::PATCH, &path),
            &json!({"content": content}),
        ))
        .await
    }

    /// Deletes a message; one Discord no longer has is as good as deleted.
    pub async fn delete(&self, channel: &str, message: &str) -> Result<()> {
        let path = format!("/channels/{channel}/messages/{message}");
        match self.empty(self.request(Method::DELETE, &path)).await {
            Err(e) if status(&e) == Some(StatusCode::NOT_FOUND) => Ok(()),
            done => done,
        }
    }

    /// Marks a channel read up to `message`, as the web client does when a
    /// channel is on screen.
    pub async fn ack(&self, channel: &str, message: &str) -> Result<()> {
        let path = format!("/channels/{channel}/messages/{message}/ack");
        // Days since Discord's epoch, as `last_viewed`.
        let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
        let days = now_ms.saturating_sub(DISCORD_EPOCH_MS) / 86_400_000;
        let body = json!({"token": null, "last_viewed": days});
        self.json(with_json(self.request(Method::POST, &path), &body))
            .await
            .map(drop)
    }

    /// Marks channels read, each up to its message, in one request, as the
    /// web client does for a whole server (at most 100 at once).
    pub async fn ack_bulk(&self, reads: &[(String, String)]) -> Result<()> {
        for chunk in reads.chunks(100) {
            let read_states: Vec<Value> = chunk
                .iter()
                .map(|(channel, message)| json!({"channel_id": channel, "message_id": message}))
                .collect();
            let body = json!({"read_states": read_states});
            self.empty(with_json(self.request(Method::POST, "/read-states/ack-bulk"), &body))
                .await?;
        }
        Ok(())
    }

    /// Adds or removes our reaction; `emoji` is the emoji itself or
    /// `name:id` for a custom one.
    pub async fn react(&self, channel: &str, message: &str, emoji: &str, add: bool) -> Result<()> {
        let path = format!(
            "/channels/{channel}/messages/{message}/reactions/{}/@me?location=Message&type=0",
            encode(emoji)
        );
        let method = if add { Method::PUT } else { Method::DELETE };
        self.empty(self.request(method, &path)).await
    }

    pub async fn typing(&self, channel: &str) -> Result<()> {
        let path = format!("/channels/{channel}/typing");
        self.empty(self.request(Method::POST, &path)).await
    }

    // Calls in DMs and group DMs (docs.discord.food: Calls; as Yapfast).

    /// Whether we may ring the others in a DM, beyond calling them.
    pub async fn ringable(&self, channel: &str) -> Result<bool> {
        let path = format!("/channels/{channel}/call");
        let answer = self.json(self.request(Method::GET, &path)).await?;
        Ok(answer["ringable"].as_bool().unwrap_or(false))
    }

    /// Rings everyone in a DM or group DM call who isn't in it yet.
    pub async fn ring(&self, channel: &str) -> Result<()> {
        let path = format!("/channels/{channel}/call/ring");
        let request = with_json(self.request(Method::POST, &path), &json!({"recipients": null}));
        self.empty(request).await
    }

    /// Stops ringing `me` for a call, which declines it.
    pub async fn stop_ringing(&self, channel: &str, me: &str) -> Result<()> {
        let path = format!("/channels/{channel}/call/stop-ringing");
        let request = with_json(self.request(Method::POST, &path), &json!({"recipients": [me]}));
        self.empty(request).await
    }

    // People.

    /// Opens (or finds) the DM with `user`; returns its channel.
    pub async fn open_dm(&self, user: &str) -> Result<Value> {
        let body = json!({"recipients": [user]});
        self.json(with_json(self.request(Method::POST, "/users/@me/channels"), &body))
            .await
    }

    /// Our account's own fields (`global_name`, `avatar` as a data URI), as
    /// the official client's Profiles page saves them (docs.discord.food,
    /// Modify Current User).
    pub async fn edit_me(&self, body: &Value) -> Result<Value> {
        self.json(with_json(self.request(Method::PATCH, "/users/@me"), body))
            .await
    }

    /// Our profile's `bio` and `pronouns` (Modify User Profile).
    pub async fn edit_my_profile(&self, body: &Value) -> Result<Value> {
        self.json(with_json(self.request(Method::PATCH, "/users/@me/profile"), body))
            .await
    }

    pub async fn profile(&self, user: &str, guild: Option<&str>) -> Result<Value> {
        let mut path = format!(
            "/users/{user}/profile?with_mutual_guilds=true&with_mutual_friends=true&with_mutual_friends_count=false"
        );
        if let Some(guild) = guild {
            path.push_str(&format!("&guild_id={guild}"));
        }
        self.json(self.request(Method::GET, &path)).await
    }

    /// Sends a friend request, or accepts theirs. `place` is where in the
    /// client it happened, which the web client tells Discord.
    pub async fn add_friend(&self, user: &str, place: &str) -> Result<()> {
        let path = format!("/users/@me/relationships/{user}");
        let request = with_json(self.request(Method::PUT, &path), &json!({}));
        self.empty(request.header("X-Context-Properties", context(place))).await
    }

    /// Sends a friend request to whoever has `username`.
    pub async fn add_friend_by_username(&self, username: &str) -> Result<()> {
        let body = json!({"username": username, "discriminator": null});
        let request = with_json(self.request(Method::POST, "/users/@me/relationships"), &body);
        self.empty(request.header("X-Context-Properties", context("Add Friend")))
            .await
    }

    /// Unfriends, cancels or declines.
    pub async fn remove_relationship(&self, user: &str, place: &str) -> Result<()> {
        let path = format!("/users/@me/relationships/{user}");
        let request = self.request(Method::DELETE, &path);
        self.empty(request.header("X-Context-Properties", context(place))).await
    }

    /// The custom status, kept in the account's settings as the web client
    /// keeps it (the gateway is told too; see the account).
    pub async fn set_custom_status(&self, text: Option<&str>) -> Result<()> {
        let body = json!({"settings": crate::settings::write_custom(text)});
        self.json(with_json(
            self.request(Method::PATCH, "/users/@me/settings-proto/1"),
            &body,
        ))
        .await
        .map(drop)
    }

    /// Revokes data-use consents (docs.discord.food, User Settings: Modify
    /// User Consents): `usage_statistics` ("use data to improve Discord")
    /// and `personalization` ("use data to personalize my experience").
    pub async fn revoke_consents(&self) -> Result<()> {
        let body = json!({"grant": [], "revoke": ["usage_statistics", "personalization"]});
        self.json(with_json(self.request(Method::POST, "/users/@me/consent"), &body))
            .await
            .map(drop)
    }

    /// The account's signed-in devices, as the official Devices page lists
    /// them (docs.discord.food, Get User Sessions): `user_sessions`, each with
    /// `id_hash`, `approx_last_used_time` and `client_info` (os, platform, location).
    pub async fn sessions(&self) -> Result<Value> {
        self.json(self.request(Method::GET, "/auth/sessions")).await
    }

    /// Signs one device out (Log Out User Sessions), by its `id_hash`.
    pub async fn log_out_session(&self, id_hash: &str) -> Result<()> {
        let body = json!({"session_id_hashes": [id_hash]});
        self.json(with_json(self.request(Method::POST, "/auth/sessions/logout"), &body))
            .await
            .map(drop)
    }

    /// The data-use consents (`usage_statistics`, `personalization`, …),
    /// each `{"consented": bool}` (docs.discord.food, Get User Consents).
    pub async fn consents(&self) -> Result<Value> {
        self.json(self.request(Method::GET, "/users/@me/consent")).await
    }

    /// Grants or revokes one consent, as the Privacy page's switch does.
    pub async fn set_consent(&self, key: &str, grant: bool) -> Result<()> {
        let body = if grant {
            json!({"grant": [key], "revoke": []})
        } else {
            json!({"grant": [], "revoke": [key]})
        };
        self.json(with_json(self.request(Method::POST, "/users/@me/consent"), &body))
            .await
            .map(drop)
    }

    /// Shares activities (games) in the presence, or stops, in the settings.
    pub async fn set_show_activity(&self, shown: bool) -> Result<()> {
        let body = json!({"settings": crate::settings::write_show_game(shown)});
        self.json(with_json(
            self.request(Method::PATCH, "/users/@me/settings-proto/1"),
            &body,
        ))
        .await
        .map(drop)
    }

    /// Stops sharing activities (games) in the presence, in the settings.
    pub async fn hide_activity(&self) -> Result<()> {
        let body = json!({"settings": crate::settings::write_show_game(false)});
        self.json(with_json(
            self.request(Method::PATCH, "/users/@me/settings-proto/1"),
            &body,
        ))
        .await
        .map(drop)
    }

    /// The status (`online`, `idle`, `dnd`, `invisible`), in the settings.
    pub async fn set_status(&self, status: &str) -> Result<()> {
        let body = json!({"settings": crate::settings::write_status(status)});
        self.json(with_json(
            self.request(Method::PATCH, "/users/@me/settings-proto/1"),
            &body,
        ))
        .await
        .map(drop)
    }

    // Servers.

    /// Leaves a server, as its menu's Leave Server does.
    pub async fn leave_guild(&self, guild: &str) -> Result<()> {
        let path = format!("/users/@me/guilds/{guild}");
        self.empty(with_json(
            self.request(Method::DELETE, &path),
            &json!({"lurking": false}),
        ))
        .await
    }

    pub async fn delete_guild(&self, guild: &str) -> Result<()> {
        let path = format!("/guilds/{guild}/delete");
        self.empty(self.request(Method::POST, &path)).await
    }

    pub async fn guild_invites(&self, guild: &str) -> Result<Value> {
        self.json(self.request(Method::GET, &format!("/guilds/{guild}/invites")))
            .await
    }

    pub async fn revoke_invite(&self, code: &str) -> Result<()> {
        self.empty(self.request(Method::DELETE, &format!("/invites/{}", encode(code))))
            .await
    }

    /// Makes an invite as the server menu's Invite People does: Discord's
    /// default of seven days and any number of uses.
    pub async fn create_invite(&self, channel: &str) -> Result<String> {
        let path = format!("/channels/{channel}/invites");
        let body = json!({"max_age": 604800, "max_uses": 0, "temporary": false});
        let response = self
            .execute(with_json(self.request(Method::POST, &path), &body))
            .await?;
        // Discord answers 204, with nothing, when the server paused invites.
        if response.status() == StatusCode::NO_CONTENT {
            bail!("This server has paused invites.");
        }
        let answer: Value =
            serde_json::from_slice(&response.bytes().await?).context("could not understand Discord's answer")?;
        answer["code"]
            .as_str()
            .map(str::to_owned)
            .context("Discord made no invite.")
    }

    pub async fn bans(&self, guild: &str) -> Result<Value> {
        self.json(self.request(Method::GET, &format!("/guilds/{guild}/bans?limit=1000")))
            .await
    }

    pub async fn unban(&self, guild: &str, user: &str) -> Result<()> {
        self.empty(self.request(Method::DELETE, &format!("/guilds/{guild}/bans/{user}")))
            .await
    }

    pub async fn kick(&self, guild: &str, user: &str, reason: &str) -> Result<()> {
        let path = format!("/guilds/{guild}/members/{user}");
        self.empty(audit_reason(self.request(Method::DELETE, &path), reason))
            .await
    }

    pub async fn ban(&self, guild: &str, user: &str, reason: &str) -> Result<()> {
        let path = format!("/guilds/{guild}/bans/{user}");
        let request = with_json(self.request(Method::PUT, &path), &json!({"delete_message_seconds": 0}));
        self.empty(audit_reason(request, reason)).await
    }

    pub async fn create_role(&self, guild: &str, body: &Value) -> Result<Value> {
        self.json(with_json(
            self.request(Method::POST, &format!("/guilds/{guild}/roles")),
            body,
        ))
        .await
    }

    pub async fn modify_role(&self, guild: &str, role: &str, body: &Value) -> Result<Value> {
        let path = format!("/guilds/{guild}/roles/{role}");
        self.json(with_json(self.request(Method::PATCH, &path), body)).await
    }

    pub async fn delete_role(&self, guild: &str, role: &str) -> Result<()> {
        self.empty(self.request(Method::DELETE, &format!("/guilds/{guild}/roles/{role}")))
            .await
    }

    pub async fn create_channel(&self, guild: &str, body: &Value) -> Result<Value> {
        self.json(with_json(
            self.request(Method::POST, &format!("/guilds/{guild}/channels")),
            body,
        ))
        .await
    }

    pub async fn modify_guild(&self, guild: &str, body: &Value) -> Result<Value> {
        self.json(with_json(
            self.request(Method::PATCH, &format!("/guilds/{guild}")),
            body,
        ))
        .await
    }

    /// Joins a server by invite code, as accepting an invite does.
    pub async fn join_invite(&self, code: &str) -> Result<Value> {
        let path = format!("/invites/{}", encode(code));
        let request = with_json(self.request(Method::POST, &path), &json!({"session_id": null}));
        self.json(request.header("X-Context-Properties", context("Join Guild")))
            .await
    }

    /// An invite's preview, as an invite card shows it.
    /// Starts a thread from `message`, or (no message) a forum post whose
    /// first message is `text`, as the web client does; the thread channel.
    pub async fn create_thread(&self, channel: &str, message: Option<&str>, name: &str, text: &str) -> Result<Value> {
        let (path, body) = match message {
            Some(m) => (
                format!("/channels/{channel}/messages/{m}/threads"),
                serde_json::json!({ "name": name, "auto_archive_duration": 4320 }),
            ),
            None => (
                format!("/channels/{channel}/threads"),
                serde_json::json!({ "name": name, "auto_archive_duration": 4320, "message": { "content": text } }),
            ),
        };
        self.json(with_json(self.request(Method::POST, &path), &body)).await
    }

    /// Our notification level for a server, as the client's server menu sets
    /// it (`PATCH /users/@me/guilds/{g}/settings`).
    pub async fn guild_notify(&self, guild: &str, level: u8) -> Result<()> {
        let path = format!("/users/@me/guilds/{guild}/settings");
        let body = serde_json::json!({ "message_notifications": level });
        self.empty(with_json(self.request(Method::PATCH, &path), &body)).await
    }

    /// Messages matching `content`: a server's search narrowed to `channel`,
    /// or a DM's own (Yapfast's `search`). Discord answers groups of messages
    /// with the matches first in each.
    pub async fn search(&self, guild: Option<&str>, channel: &str, content: &str) -> Result<Value> {
        let path = match guild {
            Some(guild) => format!(
                "/guilds/{guild}/messages/search?content={}&channel_id={channel}",
                encode(content)
            ),
            None => format!("/channels/{channel}/messages/search?content={}", encode(content)),
        };
        self.json(self.request(Method::GET, &path)).await
    }

    /// Accepts a message request (`consent_status` 2), or ignores it: it
    /// leaves our DMs and the sender isn't told (Yapfast's `accept_request`,
    /// `ignore_request`).
    pub async fn answer_request(&self, channel: &str, accept: bool) -> Result<()> {
        let path = format!("/channels/{channel}/recipients/@me");
        if accept {
            let body = serde_json::json!({ "consent_status": 2 });
            self.empty(with_json(self.request(Method::PUT, &path), &body)).await
        } else {
            self.empty(self.request(Method::DELETE, &path)).await
        }
    }

    /// Our votes in a poll, all of them (empty takes them back), as the web
    /// client sends them: answer ids as strings.
    pub async fn vote(&self, channel: &str, message: &str, answers: &[u64]) -> Result<()> {
        let path = format!("/channels/{channel}/polls/{message}/answers/@me");
        let ids: Vec<String> = answers.iter().map(u64::to_string).collect();
        let body = serde_json::json!({ "answer_ids": ids });
        self.empty(with_json(self.request(Method::PUT, &path), &body)).await
    }

    /// Pins or unpins a message (Yapfast's `pin`).
    pub async fn pin(&self, channel: &str, message: &str, pinned: bool) -> Result<()> {
        let path = format!("/channels/{channel}/messages/pins/{message}");
        let method = if pinned { Method::PUT } else { Method::DELETE };
        self.empty(self.request(method, &path)).await
    }

    /// A channel's pinned messages, the latest pinned first (up to 50), as
    /// the web client's paged pins endpoint answers (Yapfast's `pins`).
    pub async fn pins(&self, channel: &str) -> Result<Vec<Value>> {
        let path = format!("/channels/{channel}/messages/pins?limit=50");
        let mut answer = self.json(self.request(Method::GET, &path)).await?;
        match answer["items"].take() {
            Value::Array(items) => Ok(items.into_iter().map(|mut item| item["message"].take()).collect()),
            _ => anyhow::bail!("could not understand Discord's answer"),
        }
    }

    /// The GIF picker's categories (and a few trending GIFs), as Discord's
    /// client asks (Yapfast's `gifs_trending`).
    pub async fn gifs_trending(&self) -> Result<Value> {
        let path = format!("/gifs/trending?media_format={GIF_FORMAT}&locale={GIF_LOCALE}");
        self.json(self.request(Method::GET, &path)).await
    }

    /// The GIFs trending now (the picker's "Trending GIFs").
    pub async fn gifs_trending_gifs(&self) -> Result<Value> {
        let path = format!("/gifs/trending-gifs?media_format={GIF_FORMAT}&locale={GIF_LOCALE}");
        self.json(self.request(Method::GET, &path)).await
    }

    /// GIFs for `query`, as many as Discord gives at once (no pages).
    pub async fn gifs_search(&self, query: &str) -> Result<Value> {
        let path = format!(
            "/gifs/search?q={}&media_format={GIF_FORMAT}&locale={GIF_LOCALE}",
            encode(query)
        );
        self.json(self.request(Method::GET, &path)).await
    }

    pub async fn invite(&self, code: &str) -> Result<Value> {
        let path = format!("/invites/{}?with_counts=true&with_expiration=true", encode(code));
        self.json(self.request(Method::GET, &path)).await
    }
}

/// The GIF previews' format and locale, as Discord's web client asks for them.
const GIF_FORMAT: &str = "tinywebp";
const GIF_LOCALE: &str = "en-US";

/// Discord's epoch (2015-01-01), in Unix milliseconds.
pub const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;

/// A snowflake's time, in Unix milliseconds.
pub fn snowflake_ms(id: &str) -> i64 {
    id.parse::<u64>()
        .map_or(0, |id| i64::try_from((id >> 22) + DISCORD_EPOCH_MS).unwrap_or(0))
}

/// A nonce as the web client makes one: a snowflake for now.
pub fn nonce() -> String {
    let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
    (now_ms.saturating_sub(DISCORD_EPOCH_MS) << 22).to_string()
}

/// An answer's JSON body, or null.
async fn body(response: Response) -> Value {
    match response.bytes().await {
        Ok(body) => serde_json::from_slice(&body).unwrap_or_default(),
        Err(_) => Value::Null,
    }
}

/// A refusal as an error a person can act on.
fn refusal(status: StatusCode, body: &Value) -> anyhow::Error {
    // An hCaptcha (what Discord uses) is shown; any other kind can't be.
    if let Some(captcha) = crate::captcha::Captcha::from_body(body) {
        return anyhow!(captcha);
    }
    if body.get("captcha_key").is_some() {
        return anyhow!(CaptchaRequired);
    }
    anyhow!(DiscordError {
        status,
        code: body["code"].as_u64(),
        message: body["message"].as_str().unwrap_or_default().to_owned(),
    })
}

fn with_json(request: RequestBuilder, body: &Value) -> RequestBuilder {
    request
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
}

/// The `X-Context-Properties` header: where in the client something happened.
fn context(place: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(json!({"location": place}).to_string())
}

/// A moderation request's audit log reason, URL-encoded as Discord reads it.
fn audit_reason(request: RequestBuilder, reason: &str) -> RequestBuilder {
    let reason = reason.trim();
    if reason.is_empty() {
        return request;
    }
    let end = reason
        .char_indices()
        .nth(MAX_REASON)
        .map_or(reason.len(), |(end, _)| end);
    request.header("X-Audit-Log-Reason", encode(&reason[..end]))
}

/// Percent-encodes a path segment.
pub fn encode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len() * 3);
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn reply(socket: &mut tokio::net::TcpStream, headers: &str) {
        let mut bytes = [0; 4096];
        let mut received = Vec::new();
        while !received.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = socket.read(&mut bytes).await.unwrap();
            assert!(n > 0);
            received.extend_from_slice(&bytes[..n]);
        }
        socket
            .write_all(format!("HTTP/1.1 {headers}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").as_bytes())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_429_is_not_retried() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/channels/1/messages", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            reply(&mut socket, "429 Too Many Requests\r\nRetry-After: 0").await;
            assert!(
                tokio::time::timeout(Duration::from_millis(200), listener.accept())
                    .await
                    .is_err(),
                "no automatic retry, even for a zero wait"
            );
        });
        let api = Api::anonymous().unwrap();
        let error = api.execute(api.http.get(url)).await.unwrap_err();
        assert_eq!(status(&error), Some(StatusCode::TOO_MANY_REQUESTS));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_requests_learn_an_empty_bucket_before_sending_more() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/channels/1/messages", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            reply(&mut socket, "200 OK\r\nX-RateLimit-Bucket: messages\r\nX-RateLimit-Limit: 1\r\nX-RateLimit-Remaining: 0\r\nX-RateLimit-Reset-After: 120").await;
            assert!(
                tokio::time::timeout(Duration::from_millis(200), listener.accept())
                    .await
                    .is_err(),
                "no parallel request escapes the bucket"
            );
        });
        let api = Api::anonymous().unwrap();
        let (first, second) = tokio::join!(api.execute(api.http.get(&url)), api.execute(api.http.get(&url)));
        assert!(first.is_ok());
        assert!(second.unwrap_err().to_string().contains("slow down"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn request_pacing_is_shared_across_accounts_and_unknown_routes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut times = Vec::new();
            for _ in 0..3 {
                let (mut socket, _) = listener.accept().await.unwrap();
                times.push(Instant::now());
                reply(&mut socket, "200 OK").await;
            }
            for pair in times.windows(2) {
                assert!(pair[1].duration_since(pair[0]) >= REQUEST_SPACING - Duration::from_millis(20));
            }
        });
        let first = Api::anonymous().unwrap();
        let second = Api::anonymous().unwrap();
        let (a, b, c) = tokio::join!(
            first.execute(first.http.get(format!("{base}/first"))),
            second.execute(second.http.get(format!("{base}/second"))),
            first.execute(first.http.get(format!("{base}/third")))
        );
        assert!(a.is_ok() && b.is_ok() && c.is_ok());
        server.await.unwrap();
    }

    #[test]
    fn snowflakes_carry_their_time() {
        assert_eq!(snowflake_ms("175928847299117063"), 1_462_015_105_796);
        let n = nonce();
        assert!(snowflake_ms(&n) > 1_700_000_000_000);
        assert_eq!(snowflake_ms("nope"), 0);
    }

    #[test]
    fn refusals_say_what_discord_said() {
        let e = refusal(
            StatusCode::FORBIDDEN,
            &json!({"code": 50013, "message": "Missing Permissions"}),
        );
        assert_eq!(status(&e), Some(StatusCode::FORBIDDEN));
        assert!(e.to_string().contains("Missing Permissions"));
        let captcha = refusal(StatusCode::BAD_REQUEST, &json!({"captcha_key": ["captcha-required"]}));
        assert!(captcha.downcast_ref::<CaptchaRequired>().is_some());
    }

    #[test]
    fn path_segments_are_encoded() {
        assert_eq!(encode("👍"), "%F0%9F%91%8D");
        assert_eq!(encode("name:123"), "name%3A123");
        assert_eq!(encode("abc-_.~"), "abc-_.~");
    }
}
