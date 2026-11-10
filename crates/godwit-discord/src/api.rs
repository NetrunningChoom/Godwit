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
use crate::mfa;

pub const API: &str = "https://discord.com/api/v9";

fn create_group_body(users: &[String]) -> Value {
    json!({"recipients": users})
}
/// The name requests carry in the network layer's log.
const NET: &str = "discord";
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

fn thread_membership_path(thread: &str, join: bool) -> (Method, String) {
    (
        if join { Method::PUT } else { Method::DELETE },
        format!("/channels/{thread}/thread-members/@me"),
    )
}

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

/// The longest note Discord keeps (docs.discord.food, Modify User Note).
pub const NOTE_LIMIT: usize = 256;

/// The text of a Get User Note answer.
pub fn parse_note(answer: &Value) -> String {
    answer["note"].as_str().unwrap_or_default().to_owned()
}

/// The body of Modify User Note: the note cut to Discord's limit, which the
/// web client's field enforces as well.
pub fn note_body(note: &str) -> Value {
    json!({"note": note.chars().take(NOTE_LIMIT).collect::<String>()})
}

/// A friend nickname change: empty clears it (null), as Yapfast sends.
pub fn friend_nickname_body(nickname: &str) -> Value {
    let nickname = nickname.trim();
    json!({"nickname": (!nickname.is_empty()).then_some(nickname)})
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

/// Discord's upload API carries spoiler state in the multipart filename.
pub fn upload_filename(name: &str, spoiler: bool) -> String {
    if spoiler {
        format!("SPOILER_{name}")
    } else {
        name.to_owned()
    }
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
    base: String,
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
    /// Discord's default call sounds (Soundboard resource).
    pub async fn default_soundboard_sounds(&self) -> Result<Vec<Value>> {
        let response = self
            .json(self.request(Method::GET, "/soundboard-default-sounds"))
            .await?;
        Ok(response.as_array().cloned().unwrap_or_default())
    }

    /// Guild soundboard entries, as described by the Soundboard resource.
    pub async fn guild_soundboard_sounds(&self, guild: &str) -> Result<Vec<Value>> {
        let response = self
            .json(self.request(Method::GET, &format!("/guilds/{guild}/soundboard-sounds")))
            .await?;
        Ok(response["soundboard_sounds"].as_array().cloned().unwrap_or_default())
    }

    /// Play a server sound in a voice channel. Other-server sounds are omitted
    /// because Discord reserves them for Nitro.
    pub async fn send_soundboard_sound(&self, channel: &str, sound: &Value) -> Result<()> {
        self.empty(with_json(
            self.request(Method::POST, &format!("/channels/{channel}/send-soundboard-sound")),
            sound,
        ))
        .await
    }

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
            base: API.to_owned(),
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
            let request = self
                .http
                .get(client::update_url())
                .header(header::ACCEPT, "application/json");
            let answer: Value = godwit_core::net::stream(NET, godwit_core::net::Class::Read, request)
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
            let request = self.http.get(WEB_APP).header(header::ACCEPT, "text/html");
            let mut response = godwit_core::net::stream(NET, godwit_core::net::Class::Read, request)
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
            .request(method, format!("{}{path}", self.base))
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
        // The one request that goes again after a confirmation carries its
        // token (docs.discord.food, "Multi-Factor Authentication").
        if let Some(confirmed) = mfa::token()
            && let Ok(mut value) = header::HeaderValue::from_str(&confirmed)
        {
            value.set_sensitive(true);
            request = request.header("x-discord-mfa-authorization", value);
        }
        let token = self.token();
        if token.is_empty() {
            request
        } else {
            request.header(header::AUTHORIZATION, token)
        }
    }

    /// Turns a confirmation into the token a request held back for
    /// two-factor authentication goes again with: `data` is the code, the
    /// password or the backup code, by `method` (docs.discord.food,
    /// "Multi-Factor Authentication": `POST /mfa/finish`).
    pub(crate) async fn finish_mfa(&self, ticket: &str, method: mfa::Method, data: &str) -> Result<String> {
        let answer = self
            .json(with_json(
                self.request(Method::POST, "/mfa/finish"),
                &mfa_finish_body(ticket, method, data),
            ))
            .await?;
        answer["token"]
            .as_str()
            .map(str::to_owned)
            .context("Discord gave no token for the confirmation")
    }

    /// Changes the password; the answer carries the account's new token
    /// (docs.discord.food, "Modify Current User").
    pub(crate) async fn change_password(&self, current: &str, new: &str) -> Result<Value> {
        self.json(with_json(
            self.request(Method::PATCH, "/users/@me"),
            &crate::security::password_change_body(current, new),
        ))
        .await
    }

    /// Checks the password before authenticator setup: Discord answers a
    /// right one with code 60005 (docs.discord.food, "Enable User MFA").
    pub(crate) async fn check_password_for_totp(&self, password: &str) -> Result<()> {
        let body = crate::security::totp_check_body(password);
        match self
            .json(with_json(
                self.request(Method::POST, "/users/@me/mfa/totp/enable"),
                &body,
            ))
            .await
        {
            Err(e)
                if e.downcast_ref::<DiscordError>()
                    .is_some_and(|d| d.code == Some(crate::security::TOTP_SETUP_CONTINUES)) =>
            {
                Ok(())
            }
            Ok(_) => bail!("Discord didn't ask for the authenticator setup to go on. Try again."),
            Err(e) => Err(e),
        }
    }

    /// Turns the authenticator on; the answer has the new token and the
    /// backup codes.
    pub(crate) async fn enable_totp(&self, password: &str, secret: &str, code: &str) -> Result<Value> {
        self.json(with_json(
            self.request(Method::POST, "/users/@me/mfa/totp/enable"),
            &crate::security::totp_enable_body(password, secret, code),
        ))
        .await
    }

    /// Turns the authenticator off (Discord asks to confirm first); the
    /// answer has the new token.
    pub(crate) async fn disable_totp(&self) -> Result<Value> {
        self.json(
            self.request(Method::POST, "/users/@me/mfa/totp/disable")
                .header(header::CONTENT_TYPE, "application/json")
                .body("{}"),
        )
        .await
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
        let class = godwit_core::net::Class::of(request.method());
        let response = match godwit_core::net::stream_built(NET, class, &client, request).await {
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

    /// Signs in with email/phone and password, optionally carrying the
    /// captcha solution Discord requested (Authentication page).
    pub async fn password_login_solved(
        &self,
        login: &str,
        password: &str,
        solved: Option<(&crate::captcha::Captcha, &str)>,
    ) -> Result<Value> {
        let mut request = with_json(
            self.request(Method::POST, "/auth/login"),
            &json!({"login": login, "password": password, "undelete": false, "login_source": null, "gift_code_sku_id": null}),
        );
        if let Some((captcha, solution)) = solved {
            for (name, value) in captcha.headers(solution) {
                request = request.header(name, value);
            }
        }
        self.json(request).await
    }

    /// Completes one of Discord's login MFA methods.
    pub async fn login_mfa(&self, method: &str, code: &str, ticket: &str) -> Result<Value> {
        self.json(with_json(
            self.request(Method::POST, &format!("/auth/mfa/{method}")),
            &json!({"code": code, "ticket": ticket, "login_source": null, "gift_code_sku_id": null}),
        ))
        .await
    }

    pub async fn login_sms(&self, ticket: &str) -> Result<Value> {
        self.json(with_json(
            self.request(Method::POST, "/auth/mfa/sms/send"),
            &json!({"ticket": ticket}),
        ))
        .await
    }

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
        let mut body = message_body(content, nonce);
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

    /// Forwards message `message` of `channel` (in server `guild`, if any) to
    /// `target`, as the web client does: a message with no content of its own
    /// whose reference has the forward type; Discord copies the original into
    /// it as a snapshot (docs.discord.food, Message resource, Forwarded
    /// Messages; Yapfast's `Api::forward`).
    pub async fn forward(
        &self,
        guild: Option<&str>,
        channel: &str,
        message: &str,
        target: &str,
        nonce: &str,
    ) -> Result<Value> {
        let body = forward_body(guild, channel, message, nonce);
        let path = format!("/channels/{target}/messages");
        let request =
            with_json(self.request(Method::POST, &path), &body).header("X-Context-Properties", context("forwarding"));
        self.json(request).await
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
        let response = godwit_core::net::stream(NET, godwit_core::net::Class::Live, self.http.put(url).body(data))
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

    /// Marks a channel read up to `message` by hand ("Mark Unread" acks the
    /// message before the chosen one), with the mentions after it that stay
    /// unread. docs.discord.food, Read States, "Acknowledge Message": `manual`
    /// and `mention_count` make the ack move the marker back.
    pub async fn ack_manual(&self, channel: &str, message: &str, mention_count: u32) -> Result<()> {
        let path = format!("/channels/{channel}/messages/{message}/ack");
        let body = json!({"manual": true, "mention_count": mention_count});
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

    /// Who reacted with `emoji`: the first page (100, the documented maximum)
    /// of normal reactors, then of burst ("super") reactors, which the endpoint
    /// lists separately (`type=0` and `type=1`; docs.discord.food, Get
    /// Reactions: https://docs.discord.food/resources/message#get-reactions).
    /// Someone can react both ways, so they're listed once.
    pub async fn reaction_users(&self, channel: &str, message: &str, emoji: &str) -> Result<Value> {
        let page = |kind: u8| {
            format!(
                "/channels/{channel}/messages/{message}/reactions/{}?limit=100&type={kind}",
                encode(emoji)
            )
        };
        let mut users = match self.json(self.request(Method::GET, &page(0))).await? {
            Value::Array(users) => users,
            other => return Ok(other),
        };
        if let Value::Array(burst) = self.json(self.request(Method::GET, &page(1))).await? {
            for user in burst {
                if !users.iter().any(|u| u["id"] == user["id"]) {
                    users.push(user);
                }
            }
        }
        Ok(Value::Array(users))
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

    /// Creates a group DM from recipients (Create Private Channel).
    /// Discord's member limit includes the account that creates the group.
    pub async fn create_group(&self, users: &[String]) -> Result<Value> {
        self.json(with_json(
            self.request(Method::POST, "/users/@me/channels"),
            &create_group_body(users),
        ))
        .await
    }

    /// Leaves a group DM (Close Channel); `silent` is a query parameter
    /// (docs.discord.food), and the web client leaves non-silently.
    pub async fn leave_group(&self, channel: &str) -> Result<()> {
        self.json(self.request(Method::DELETE, &format!("/channels/{channel}?silent=false")))
            .await
            .map(drop)
    }

    /// Closes a one-to-one DM without deleting its history (Close Channel,
    /// docs.discord.food Channels resource).
    pub async fn close_dm(&self, channel: &str) -> Result<()> {
        self.json(self.request(Method::DELETE, &format!("/channels/{channel}")))
            .await
            .map(drop)
    }

    /// Adds a recipient to a group DM.
    pub async fn add_group_recipient(&self, channel: &str, user: &str) -> Result<()> {
        self.empty(self.request(Method::PUT, &format!("/channels/{channel}/recipients/{user}")))
            .await
    }

    /// Removes a recipient from a group DM.
    pub async fn remove_group_recipient(&self, channel: &str, user: &str) -> Result<()> {
        self.empty(self.request(Method::DELETE, &format!("/channels/{channel}/recipients/{user}")))
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

    /// Discord's web client reads and edits the current member profile in a guild;
    /// endpoint and fields follow docs.discord.food's User Profiles routes.
    pub async fn guild_member_profile(&self, user: &str, guild: &str) -> Result<Value> {
        self.json(self.request(Method::GET, &format!("/users/{user}/profile?guild_id={guild}")))
            .await
    }

    pub async fn edit_guild_member(&self, guild: &str, body: &Value) -> Result<Value> {
        self.json(with_json(
            self.request(Method::PATCH, &format!("/guilds/{guild}/members/@me")),
            body,
        ))
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

    /// Our private note on `user` (docs.discord.food, Get User Note; Yapfast's
    /// `api::note`): empty when there is none, which Discord answers as a 404.
    pub async fn note(&self, user: &str) -> Result<String> {
        let path = format!("/users/@me/notes/{user}");
        match self.json(self.request(Method::GET, &path)).await {
            Ok(answer) => Ok(parse_note(&answer)),
            Err(e) if status(&e) == Some(StatusCode::NOT_FOUND) => Ok(String::new()),
            Err(e) => Err(e),
        }
    }

    /// Sets our note on `user` (Modify User Note: 204; the gateway then sends
    /// USER_NOTE_UPDATE). Empty removes it.
    pub async fn set_note(&self, user: &str, note: &str) -> Result<()> {
        let path = format!("/users/@me/notes/{user}");
        self.empty(with_json(self.request(Method::PUT, &path), &note_body(note)))
            .await
    }

    /// Sends a friend request, or accepts theirs. `place` is where in the
    /// client it happened, which the web client tells Discord.
    pub async fn add_friend(&self, user: &str, place: &str) -> Result<()> {
        let path = format!("/users/@me/relationships/{user}");
        let request = with_json(self.request(Method::PUT, &path), &json!({}));
        self.empty(request.header("X-Context-Properties", context(place))).await
    }

    /// Blocks `user`: a relationship of type 2 (docs.discord.food,
    /// resources/relationships, "Create Relationship"; Yapfast does the same).
    pub async fn block_user(&self, user: &str, place: &str) -> Result<()> {
        let path = format!("/users/@me/relationships/{user}");
        let request = with_json(self.request(Method::PUT, &path), &json!({"type": 2}));
        self.empty(request.header("X-Context-Properties", context(place))).await
    }

    /// Sets the nickname this account gives a friend; empty clears it
    /// (Modify Relationship, docs.discord.food resources/relationships;
    /// Yapfast's `set_friend_nickname` sends the same body).
    pub async fn set_friend_nickname(&self, user: &str, nickname: &str) -> Result<()> {
        let path = format!("/users/@me/relationships/{user}");
        self.empty(with_json(
            self.request(Method::PATCH, &path),
            &friend_nickname_body(nickname),
        ))
        .await
    }

    /// Uses Discord's relationship `/ignore` endpoint (docs.discord.food, User Relationships).
    pub async fn ignore_user(&self, user: &str, ignore: bool) -> Result<()> {
        let (method, path) = Self::ignore_user_request(user, ignore);
        self.empty(self.request(method, &path)).await
    }

    fn ignore_user_request(user: &str, ignore: bool) -> (Method, String) {
        (
            if ignore { Method::PUT } else { Method::DELETE },
            format!("/users/@me/relationships/{user}/ignore"),
        )
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

    /// Saves the complete guild-folder setting through Discord's web-client
    /// settings-proto PATCH, preserving unrelated fields from the last read.
    pub async fn set_folders(&self, proto: &str, folders: &[crate::settings::Folder]) -> Result<()> {
        let body = json!({"settings": crate::settings::write_folder_list(proto, folders)});
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

    /// The look-and-feel choices that are `Some`, in the settings.
    pub async fn set_preferences(&self, prefs: &godwit_core::preferences::SyncedPreferences) -> Result<()> {
        let body = json!({"settings": crate::settings::write_preferences(prefs)});
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

    /// Read a guild audit-log page. Field meanings follow Discord's Audit Log Entry object.
    pub async fn audit_log(&self, guild: &str, before: Option<&str>) -> Result<Value> {
        let mut path = format!("/guilds/{guild}/audit-logs?limit=50");
        if let Some(before) = before {
            path.push_str("&before=");
            path.push_str(&encode(before));
        }
        self.json(self.request(Method::GET, &path)).await
    }

    /// List Scheduled Events with interested-user counts (`with_user_count=true`).
    /// Source: Discord Developer Documentation, Guild Scheduled Event resource.
    pub async fn scheduled_events(&self, guild: &str) -> Result<Value> {
        self.json(self.request(
            Method::GET,
            &format!("/guilds/{guild}/scheduled-events?with_user_count=true"),
        ))
        .await
    }

    /// Set the current user's Scheduled Event RSVP.
    /// Source: Discord Developer Documentation, Guild Scheduled Event resource, Modify Current User's RSVP.
    pub async fn scheduled_event_interest(&self, guild: &str, event: &str, interested: bool) -> Result<()> {
        let method = if interested { Method::PUT } else { Method::DELETE };
        self.empty(self.request(method, &format!("/guilds/{guild}/scheduled-events/{event}/users/@me")))
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

    /// `delete_message_seconds` is how much of their history goes (0 to
    /// 604800), docs.discord.food/resources/guild, Create Guild Ban.
    pub async fn ban(&self, guild: &str, user: &str, reason: &str, delete_seconds: u32) -> Result<()> {
        let path = format!("/guilds/{guild}/bans/{user}");
        let body = json!({"delete_message_seconds": delete_seconds.min(604_800)});
        let request = with_json(self.request(Method::PUT, &path), &body);
        self.empty(audit_reason(request, reason)).await
    }

    /// Adds or removes one role of a member (the member's gateway update
    /// follows).
    pub async fn set_member_role(&self, guild: &str, user: &str, role: &str, on: bool) -> Result<()> {
        let path = format!("/guilds/{guild}/members/{user}/roles/{role}");
        let method = if on { Method::PUT } else { Method::DELETE };
        self.empty(self.request(method, &path)).await
    }

    /// Timeout (`communication_disabled_until`) and nickname changes; `user`
    /// is `@me` for our own nickname (docs.discord.food, Modify Guild Member).
    pub async fn modify_member(&self, guild: &str, user: &str, body: &Value) -> Result<()> {
        let path = format!("/guilds/{guild}/members/{user}");
        self.empty(with_json(self.request(Method::PATCH, &path), body)).await
    }

    /// Lists a server's emoji, each with its uploader (`user`) when we may
    /// manage expressions (docs.discord.food, List Guild Emojis).
    pub async fn guild_emojis(&self, guild: &str) -> Result<Value> {
        self.json(self.request(Method::GET, &format!("/guilds/{guild}/emojis")))
            .await
    }

    /// Create Guild Emoji: `image` is a data URI.
    pub async fn create_emoji(&self, guild: &str, name: &str, image: &str) -> Result<Value> {
        self.json(with_json(
            self.request(Method::POST, &format!("/guilds/{guild}/emojis")),
            &emoji_create_body(name, image),
        ))
        .await
    }

    pub async fn rename_emoji(&self, guild: &str, emoji: &str, name: &str) -> Result<Value> {
        self.json(with_json(
            self.request(Method::PATCH, &format!("/guilds/{guild}/emojis/{emoji}")),
            &json!({"name": name}),
        ))
        .await
    }

    pub async fn delete_emoji(&self, guild: &str, emoji: &str) -> Result<()> {
        self.empty(self.request(Method::DELETE, &format!("/guilds/{guild}/emojis/{emoji}")))
            .await
    }

    /// A server's stickers or soundboard sounds
    /// (docs.discord.food, List Guild Stickers / List Guild Soundboard Sounds).
    pub async fn guild_expressions(&self, guild: &str, kind: ExpressionKind) -> Result<Value> {
        self.json(self.request(Method::GET, &expression_path(guild, kind, None)))
            .await
    }

    /// Create Guild Sticker: a form (`name`, `tags`, `description`, `file`).
    pub async fn create_sticker(
        &self,
        guild: &str,
        fields: &[(&str, String)],
        file: (&str, &str, &[u8]),
    ) -> Result<Value> {
        let (content_type, body) = crate::expressions::form(fields, file);
        self.json(
            self.request(Method::POST, &expression_path(guild, ExpressionKind::Sticker, None))
                .header(header::CONTENT_TYPE, content_type)
                .body(body),
        )
        .await
    }

    /// Create Guild Soundboard Sound: JSON with the sound as a data URI.
    pub async fn create_sound(&self, guild: &str, body: &Value) -> Result<Value> {
        self.json(with_json(
            self.request(Method::POST, &expression_path(guild, ExpressionKind::Sound, None)),
            body,
        ))
        .await
    }

    /// Modify Guild Sticker / Modify Guild Soundboard Sound.
    pub async fn edit_expression(&self, guild: &str, kind: ExpressionKind, id: &str, body: &Value) -> Result<Value> {
        self.json(with_json(
            self.request(Method::PATCH, &expression_path(guild, kind, Some(id))),
            body,
        ))
        .await
    }

    pub async fn delete_expression(&self, guild: &str, kind: ExpressionKind, id: &str) -> Result<()> {
        self.empty(self.request(Method::DELETE, &expression_path(guild, kind, Some(id))))
            .await
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

    /// A channel's webhooks (Get Channel Webhooks, docs.discord.food/resources/webhook).
    pub async fn channel_webhooks(&self, channel: &str) -> Result<Value> {
        self.json(self.request(Method::GET, &format!("/channels/{channel}/webhooks")))
            .await
    }

    pub async fn create_webhook(&self, channel: &str, body: &Value) -> Result<Value> {
        self.json(with_json(
            self.request(Method::POST, &format!("/channels/{channel}/webhooks")),
            body,
        ))
        .await
    }

    pub async fn modify_webhook(&self, id: &str, body: &Value) -> Result<Value> {
        self.json(with_json(self.request(Method::PATCH, &format!("/webhooks/{id}")), body))
            .await
    }

    pub async fn delete_webhook(&self, id: &str) -> Result<()> {
        self.empty(self.request(Method::DELETE, &format!("/webhooks/{id}")))
            .await
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

    /// Changes a channel's or category's fields that `body` names (Modify
    /// Channel, docs.discord.food/resources/channel); the channel object
    /// back. Discord sends CHANNEL_UPDATE. Shape ported from Yapfast's `edit_channel`.
    pub async fn modify_channel(&self, channel: &str, body: &Value) -> Result<Value> {
        self.json(with_json(
            self.request(Method::PATCH, &format!("/channels/{channel}")),
            body,
        ))
        .await
    }

    /// Deletes a server's channel or category (Delete Channel); Discord
    /// sends CHANNEL_DELETE and moves a category's channels out of it.
    pub async fn delete_channel(&self, channel: &str) -> Result<()> {
        self.json(self.request(Method::DELETE, &format!("/channels/{channel}")))
            .await
            .map(drop)
    }

    /// Sets a role's or member's permission overwrite in a channel (Modify
    /// Channel Permissions, 204; docs.discord.food/resources/channel):
    /// `allow` and `deny` are strings of digits, `type` 0 role or 1 member.
    pub async fn put_overwrite(&self, channel: &str, overwrite: &crate::permissions::Overwrite) -> Result<()> {
        self.empty(with_json(
            self.request(
                Method::PUT,
                &format!("/channels/{channel}/permissions/{}", overwrite.id),
            ),
            &overwrite.body(),
        ))
        .await
    }

    /// Removes one (Delete Channel Permission, 204).
    pub async fn delete_overwrite(&self, channel: &str, id: &str) -> Result<()> {
        self.empty(self.request(Method::DELETE, &format!("/channels/{channel}/permissions/{id}")))
            .await
    }

    /// Moves channels and categories, `[{"id", "position", "parent_id",
    /// "lock_permissions"}, ...]` (Modify Guild Channel Positions, 204);
    /// Discord sends CHANNEL_UPDATE for each that changed.
    pub async fn move_channels(&self, guild: &str, moves: &Value) -> Result<()> {
        self.empty(with_json(
            self.request(Method::PATCH, &format!("/guilds/{guild}/channels")),
            moves,
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

    /// Modify Guild MFA Level (owner only, needs their own 2FA):
    /// https://docs.discord.food/resources/guild
    pub async fn modify_guild_mfa(&self, guild: &str, level: u8) -> Result<Value> {
        self.json(with_json(
            self.request(Method::POST, &format!("/guilds/{guild}/mfa")),
            &mfa_body(level),
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
    /// A forum's archived posts, as the web client's list asks (Yapfast's
    /// `search_threads`): in `sort` order, [`crate::forum::THREAD_PAGE`] from
    /// the `offset`th, narrowed to `tag` (`tag` and `tag_setting` are in
    /// docs.discord.food, Channel, "Search Threads").
    pub async fn search_threads(
        &self,
        channel: &str,
        sort: godwit_core::forum::ForumSort,
        tag: Option<&str>,
        offset: usize,
    ) -> Result<Value> {
        let mut path = format!(
            "/channels/{channel}/threads/search?archived=true&sort_by={}&sort_order=desc&limit={}&tag_setting=match_some",
            crate::forum::sort_by(sort),
            crate::forum::THREAD_PAGE
        );
        if let Some(tag) = tag {
            path.push_str(&format!("&tag={}", encode(tag)));
        }
        path.push_str(&format!("&offset={offset}"));
        self.json(self.request(Method::GET, &path)).await
    }

    /// Starts a thread from `message`, or (no message) a forum post whose
    /// first message is `text` and tags `tags` (by id), as the web client
    /// does; the thread channel.
    pub async fn create_thread(
        &self,
        channel: &str,
        message: Option<&str>,
        name: &str,
        text: &str,
        tags: &[String],
    ) -> Result<Value> {
        let (path, body) = match message {
            Some(m) => (
                format!("/channels/{channel}/messages/{m}/threads"),
                serde_json::json!({ "name": name, "auto_archive_duration": 4320 }),
            ),
            None => (
                format!("/channels/{channel}/threads"),
                serde_json::json!({ "name": name, "auto_archive_duration": 4320, "applied_tags": tags, "message": { "content": text } }),
            ),
        };
        self.json(with_json(self.request(Method::POST, &path), &body)).await
    }

    /// Join or leave a thread (`PUT`/`DELETE /channels/{thread}/thread-members/@me`;
    /// docs.discord.food, Channel, Thread Members).
    pub async fn thread_membership(&self, thread: &str, join: bool) -> Result<()> {
        let (method, path) = thread_membership_path(thread, join);
        let context_name = if join { "Join Thread" } else { "Leave Thread" };
        self.empty(
            self.request(method, &path)
                .header("X-Context-Properties", context(context_name)),
        )
        .await
    }

    /// Set our thread notification options ([published endpoint](https://docs.discord.food/resources/channel#modify-thread-settings)).
    pub async fn thread_notify(&self, thread: &str, body: &Value) -> Result<()> {
        let path = format!("/channels/{thread}/thread-members/@me/settings");
        self.empty(with_json(self.request(Method::PATCH, &path), body)).await
    }

    /// Our notification level for a server, as the client's server menu sets
    /// it (`PATCH /users/@me/guilds/{g}/settings`).
    pub async fn guild_notify(&self, guild: &str, level: u8) -> Result<()> {
        let path = format!("/users/@me/guilds/{guild}/settings");
        let body = serde_json::json!({ "message_notifications": level });
        self.empty(with_json(self.request(Method::PATCH, &path), &body)).await
    }

    /// One channel's override in our settings for its server (`@me` for
    /// DMs), as the client's channel menu sends it: `PATCH
    /// /users/@me/guilds/{g}/settings` with `channel_overrides`
    /// (docs.discord.food, Guild Settings / Channel Override).
    pub async fn channel_notify(&self, guild: Option<&str>, channel: &str, entry: &Value) -> Result<()> {
        let path = format!("/users/@me/guilds/{}/settings", guild.unwrap_or("@me"));
        let body = serde_json::json!({ "channel_overrides": { channel: entry } });
        self.empty(with_json(self.request(Method::PATCH, &path), &body)).await
    }

    /// Messages matching `params`: a server's search (narrowed by its
    /// `channel_id`), or a DM's own (Yapfast's `search`). Discord answers
    /// groups of messages; a 202 body (`retry_after`) means it is indexing.
    pub async fn search(
        &self,
        guild: Option<&str>,
        channel: &str,
        params: &crate::search::SearchParams,
    ) -> Result<Value> {
        let path = params.path(guild, channel);
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

    /// Invokes a message button using the browser client's interaction body
    /// (Yapfast `components.rs`; Discord Message Components API).
    pub async fn message_component(&self, body: &Value) -> Result<()> {
        self.json(with_json(self.request(Method::POST, "/interactions"), body))
            .await
            .map(drop)
    }

    /// Posts a poll as a message of its own (docs.discord.food, Poll Create
    /// Structure); it comes back through the gateway like any message.
    pub async fn create_poll(&self, channel: &str, body: &Value) -> Result<()> {
        let path = format!("/channels/{channel}/messages");
        self.json(with_json(self.request(Method::POST, &path), body))
            .await
            .map(drop)
    }

    /// Sends a sticker, as a reply when `reply` names a message.
    pub async fn send_sticker(&self, channel: &str, body: &Value) -> Result<()> {
        let path = format!("/channels/{channel}/messages");
        self.json(with_json(self.request(Method::POST, &path), body))
            .await
            .map(drop)
    }

    /// Ends our poll now (End Poll, docs.discord.food Message resource);
    /// Discord sends MESSAGE_UPDATE with the final results.
    pub async fn end_poll(&self, channel: &str, message: &str) -> Result<()> {
        let path = format!("/channels/{channel}/polls/{message}/expire");
        self.json(self.request(Method::POST, &path)).await.map(drop)
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

/// Inputs the browser client includes in a type 3 component interaction.
pub struct MessageComponentInteraction<'a> {
    pub application_id: &'a str,
    pub guild_id: Option<&'a str>,
    pub channel_id: &'a str,
    pub message_id: &'a str,
    pub message_flags: u64,
    pub session_id: &'a str,
    pub nonce: &'a str,
    pub custom_id: &'a str,
}

/// The browser client sends component clicks as type 3 interactions
/// (docs/discord-parity.md, Yapfast `components.rs`).
pub fn message_component_body(interaction: MessageComponentInteraction<'_>) -> Value {
    let MessageComponentInteraction {
        application_id,
        guild_id,
        channel_id,
        message_id,
        message_flags,
        session_id,
        nonce,
        custom_id,
    } = interaction;
    json!({
        "type": 3,
        "application_id": application_id,
        "guild_id": guild_id,
        "channel_id": channel_id,
        "message_id": message_id,
        "message_flags": message_flags,
        "session_id": session_id,
        "nonce": nonce,
        "data": {"component_type": 2, "custom_id": custom_id},
    })
}

/// An answer's JSON body, or null.
async fn body(response: Response) -> Value {
    match response.bytes().await {
        Ok(body) => serde_json::from_slice(&body).unwrap_or_default(),
        Err(_) => Value::Null,
    }
}

/// A refusal as an error a person can act on.
pub(crate) fn refusal(status: StatusCode, body: &Value) -> anyhow::Error {
    // An hCaptcha (what Discord uses) is shown; any other kind can't be.
    if let Some(captcha) = crate::captcha::Captcha::from_body(body) {
        return anyhow!(captcha);
    }
    if body.get("captcha_key").is_some() {
        return anyhow!(CaptchaRequired);
    }
    // A request held back for two-factor authentication carries its ticket
    // (docs.discord.food, "Multi-Factor Authentication").
    if status == StatusCode::UNAUTHORIZED
        && body["code"] == mfa::MFA_REQUIRED
        && let Some(required) = mfa::Required::from_body(body)
    {
        return anyhow!(required);
    }
    if body.to_string().contains("ACCOUNT_LOGIN_VERIFICATION_EMAIL") {
        return anyhow!("Discord sent you an email to confirm this sign-in. Confirm it, then try again.");
    }
    if body
        .get("errors")
        .is_some_and(|e| e.get("login").is_some() || e.get("password").is_some())
    {
        return anyhow!("Wrong email or password.");
    }
    anyhow!(DiscordError {
        status,
        code: body["code"].as_u64(),
        message: body["message"].as_str().unwrap_or_default().to_owned(),
    })
}

use godwit_core::server_settings::ExpressionKind;

/// `/guilds/{id}/stickers[/{id}]` or `/guilds/{id}/soundboard-sounds[/{id}]`.
pub(crate) fn expression_path(guild: &str, kind: ExpressionKind, id: Option<&str>) -> String {
    let base = match kind {
        ExpressionKind::Sticker => "stickers",
        ExpressionKind::Sound => "soundboard-sounds",
    };
    match id {
        Some(id) => format!("/guilds/{guild}/{base}/{id}"),
        None => format!("/guilds/{guild}/{base}"),
    }
}

/// Create Guild Emoji's body: usable by everyone (`roles` empty), as the
/// official client sends it (docs.discord.food, Create Guild Emoji).
pub(crate) fn emoji_create_body(name: &str, image: &str) -> Value {
    json!({"name": name, "image": image, "roles": []})
}

/// The `POST /mfa/finish` body (docs.discord.food, "Multi-Factor Authentication").
pub(crate) fn mfa_finish_body(ticket: &str, method: mfa::Method, data: &str) -> Value {
    json!({"ticket": ticket, "mfa_type": method.name(), "data": data})
}

/// The Modify Guild MFA Level body (docs.discord.food, Guild resource).
pub(crate) fn mfa_body(level: u8) -> Value {
    json!({"level": level})
}

fn with_json(request: RequestBuilder, body: &Value) -> RequestBuilder {
    request
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
}

/// The web client's new message body.
fn message_body(content: &str, nonce: &str) -> Value {
    json!({
        "mobile_network_type": "unknown",
        "content": content,
        "nonce": nonce,
        "tts": false,
        "flags": 0,
    })
}

/// A sticker's message: no text, the sticker's id (`sticker_ids`, up to 3;
/// docs.discord.food Message resource, Create Message).
pub fn sticker_body(sticker: &str, nonce: &str, reply: Option<(Option<&str>, &str, &str)>) -> Value {
    let mut body = message_body("", nonce);
    body["sticker_ids"] = json!([sticker]);
    if let Some((guild, channel, message)) = reply {
        body["message_reference"] = json!({
            "guild_id": guild,
            "channel_id": channel,
            "message_id": message,
            "type": 0,
        });
    }
    body
}

/// A new poll's message as the web client posts it (docs.discord.food, Poll
/// Create Structure): text-only answers, layout 1 (default).
pub fn poll_body(question: &str, answers: &[String], hours: u32, multi: bool, nonce: &str) -> Value {
    let mut body = message_body("", nonce);
    body["poll"] = json!({
        "question": {"text": question},
        "answers": answers.iter().map(|a| json!({"poll_media": {"text": a}})).collect::<Vec<_>>(),
        "duration": hours,
        "allow_multiselect": multi,
        "layout_type": 1,
    });
    body
}

/// The type of a message reference that forwards the message instead of
/// replying to it (docs.discord.food, Message Reference Types).
const FORWARD: u64 = 1;

/// A forward's body: no content, only the reference.
fn forward_body(guild: Option<&str>, channel: &str, message: &str, nonce: &str) -> Value {
    let mut body = message_body("", nonce);
    body["message_reference"] = json!({
        "guild_id": guild,
        "channel_id": channel,
        "message_id": message,
        "type": FORWARD,
    });
    body
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

    #[test]
    fn the_finish_body_names_the_ticket_method_and_code() {
        assert_eq!(
            mfa_finish_body("t", mfa::Method::Backup, "abcd1234"),
            json!({"ticket": "t", "mfa_type": "backup", "data": "abcd1234"})
        );
        assert_eq!(mfa_finish_body("t", mfa::Method::Totp, "123456")["mfa_type"], "totp");
    }

    #[tokio::test]
    async fn only_the_confirmed_request_carries_the_mfa_header() {
        let Ok(api) = Api::new("tok".into()) else {
            panic!("client")
        };
        let plain = api.request(Method::POST, "/guilds/1/mfa").build().unwrap();
        assert!(plain.headers().get("x-discord-mfa-authorization").is_none());
        let carried = mfa::with_token_for_tests("mfa-token".into(), async {
            api.request(Method::POST, "/guilds/1/mfa").build().unwrap()
        })
        .await;
        let value = carried.headers().get("x-discord-mfa-authorization").unwrap();
        assert_eq!(value, "mfa-token");
        assert!(value.is_sensitive());
    }

    #[test]
    fn spoiler_upload_filename_prefix_is_preserved_in_message_body() {
        let upload = Upload {
            filename: "SPOILER_image.png".into(),
            uploaded_filename: "uploaded.png".into(),
        };
        let mut body = message_body("", "nonce");
        body["attachments"] = vec![json!({
            "id": "0",
            "filename": upload.filename,
            "uploaded_filename": upload.uploaded_filename,
        })]
        .into();
        assert_eq!(body["attachments"][0]["filename"], "SPOILER_image.png");
    }

    #[test]
    fn upload_filename_marks_only_selected_spoilers() {
        assert_eq!(upload_filename("image.png", true), "SPOILER_image.png");
        assert_eq!(upload_filename("image.png", false), "image.png");
    }

    #[test]
    fn ignore_uses_discords_ignore_relationship_route() {
        assert_eq!(
            Api::ignore_user_request("123", true),
            (Method::PUT, "/users/@me/relationships/123/ignore".into())
        );
        assert_eq!(
            Api::ignore_user_request("123", false),
            (Method::DELETE, "/users/@me/relationships/123/ignore".into())
        );
    }

    #[test]
    fn create_group_request_contains_recipients_without_a_name() {
        assert_eq!(
            create_group_body(&["12".into(), "34".into()]),
            json!({"recipients": ["12", "34"]})
        );
    }

    #[test]
    fn thread_membership_uses_discord_join_and_leave_routes() {
        assert_eq!(
            thread_membership_path("123", true),
            (Method::PUT, "/channels/123/thread-members/@me".into())
        );
        assert_eq!(
            thread_membership_path("123", false),
            (Method::DELETE, "/channels/123/thread-members/@me".into())
        );
    }

    #[test]
    fn reactor_emoji_path_segment_is_encoded() {
        assert_eq!(encode("name:123"), "name%3A123");
        assert_eq!(encode("😀"), "%F0%9F%98%80");
    }

    #[test]
    fn component_interaction_payload_matches_browser_shape() {
        assert_eq!(
            message_component_body(MessageComponentInteraction {
                application_id: "app",
                guild_id: Some("guild"),
                channel_id: "channel",
                message_id: "message",
                message_flags: 64,
                session_id: "session",
                nonce: "nonce",
                custom_id: "custom"
            }),
            json!({
                "type": 3, "application_id": "app", "guild_id": "guild", "channel_id": "channel",
                "message_id": "message", "message_flags": 64, "session_id": "session", "nonce": "nonce",
                "data": {"component_type": 2, "custom_id": "custom"}
            })
        );
    }

    #[test]
    fn a_note_is_read_from_the_answer_and_sent_cut_to_the_limit() {
        assert_eq!(parse_note(&json!({"note": "hi", "note_user_id": "1"})), "hi");
        assert_eq!(parse_note(&json!({})), "", "no note reads as empty");
        assert_eq!(note_body("hi"), json!({"note": "hi"}));
        let long = "é".repeat(NOTE_LIMIT + 10);
        assert_eq!(
            note_body(&long)["note"].as_str().map(|n| n.chars().count()),
            Some(NOTE_LIMIT)
        );
        assert_eq!(note_body("")["note"], "", "empty removes it");
    }

    #[test]
    fn a_sticker_is_sent_as_sticker_ids_with_no_text() {
        let body = sticker_body("42", "n1", None);
        assert_eq!(body["sticker_ids"], json!(["42"]));
        assert_eq!(body["content"], "");
        assert!(body.get("message_reference").is_none());
        let reply = sticker_body("42", "n1", Some((Some("g"), "c", "m")));
        assert_eq!(reply["message_reference"]["message_id"], "m");
        assert_eq!(reply["message_reference"]["guild_id"], "g");
    }

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
    async fn reaction_user_request_uses_documented_query_and_parses_json() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            // Normal reactors first, then burst ones; Ada reacted both ways.
            let pages: [(&str, &[u8]); 2] = [
                ("0", b"[{\"id\":\"7\",\"username\":\"Ada\"}]"),
                (
                    "1",
                    b"[{\"id\":\"7\",\"username\":\"Ada\"},{\"id\":\"8\",\"username\":\"Bo\"}]",
                ),
            ];
            for (kind, body) in pages {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = [0; 4096];
                let n = socket.read(&mut bytes).await.unwrap();
                let request = String::from_utf8_lossy(&bytes[..n]);
                let want = format!("GET /channels/1/messages/2/reactions/%F0%9F%91%8D?limit=100&type={kind} ");
                assert!(request.starts_with(&want), "{request}");
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                socket.write_all(head.as_bytes()).await.unwrap();
                socket.write_all(body).await.unwrap();
            }
        });
        let mut api = Api::anonymous().unwrap();
        api.base = base;
        let result = api.reaction_users("1", "2", "👍").await.unwrap();
        let names: Vec<_> = result
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["username"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Ada", "Bo"]);
        server.await.unwrap();
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
    fn a_forward_is_a_reference_of_the_forward_type_with_no_content() {
        let body = forward_body(Some("10"), "13", "500", "9");
        assert_eq!(body["content"], "");
        assert_eq!(
            body["message_reference"],
            json!({"guild_id": "10", "channel_id": "13", "message_id": "500", "type": 1})
        );
        // In a direct message there is no server: the field is null, as the web client sends it.
        assert!(forward_body(None, "20", "600", "9")["message_reference"]["guild_id"].is_null());
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
    fn login_refusals_are_actionable_and_do_not_echo_credentials() {
        let wrong = refusal(
            StatusCode::BAD_REQUEST,
            &json!({"errors":{"login":{"_errors":[{"message":"bad"}]},"password":{"_errors":[{"message":"bad"}]}}}),
        );
        assert_eq!(wrong.to_string(), "Wrong email or password.");
        let email = refusal(
            StatusCode::BAD_REQUEST,
            &json!({"code": "ACCOUNT_LOGIN_VERIFICATION_EMAIL"}),
        );
        assert_eq!(
            email.to_string(),
            "Discord sent you an email to confirm this sign-in. Confirm it, then try again."
        );
        let captcha = refusal(
            StatusCode::BAD_REQUEST,
            &json!({"captcha_key":["captcha-required"],"captcha_service":"hcaptcha","captcha_sitekey":"site"}),
        );
        assert!(captcha.downcast_ref::<crate::captcha::Captcha>().is_some());
    }

    #[test]
    fn path_segments_are_encoded() {
        assert_eq!(encode("👍"), "%F0%9F%91%8D");
        assert_eq!(encode("name:123"), "name%3A123");
        assert_eq!(encode("abc-_.~"), "abc-_.~");
    }
}
