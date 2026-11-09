// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Fluxer's HTTP API (docs.fluxer.com, HTTP API): `endpoints.api_public` +
//! `/v1`, a user session's token sent as it is in `Authorization`.
//!
//! Careful with it: each user has a global budget of 50 requests a second,
//! and a global denial **revokes the token** (signs the user out). Godwit
//! spaces requests by 250 ms (four a second at most), never replays one
//! after a refusal, and on a 429 waits what the body's `retry_after` says
//! (everything waits when `global` is true). Successful answers to user
//! sessions carry no rate-limit headers, so 429s are the only signal.

use std::sync::RwLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::header::{self, HeaderValue};
use reqwest::{Client, Method, RequestBuilder, StatusCode};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::discovery::Instance;

/// The name requests carry in the network layer's log.
const NET: &str = "fluxer";

/// Between any two requests, across accounts.
const SPACING: Duration = Duration::from_millis(250);
/// The pace and the pause a 429 sets (shared: the global bucket is per user,
/// and pausing everyone a little is the safe side).
static PACE: Mutex<Option<Instant>> = Mutex::const_new(None);
/// Signing in again after the CAPTCHA: at most this many tries.
const CAPTCHA_TRIES: usize = 3;

/// What signing in gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Login {
    Token {
        token: String,
        user: String,
    },
    /// A second factor is needed: the ticket for the code (5 minutes).
    Mfa {
        ticket: String,
    },
}

pub struct Api {
    http: Client,
    instance: Instance,
    token: RwLock<String>,
}

// Ten files per message and the `IS_SPOILER` attachment flag (bit 3):
// https://docs.fluxer.com/http-api/messages/
const MAX_FILES_PER_MESSAGE: usize = 10;
const ATTACHMENT_FLAG_IS_SPOILER: u64 = 1 << 3;

fn message_body(content: &str, nonce: &str, files: &[godwit_core::application::Outgoing]) -> Value {
    let mut body = json!({"content": content, "nonce": nonce});
    if !files.is_empty() {
        body["attachments"] = Value::Array(
            files
                .iter()
                .enumerate()
                .map(|(id, file)| {
                    let mut attachment = json!({"id": id, "filename": file.name.as_ref()});
                    if file.spoiler {
                        attachment["flags"] = json!(ATTACHMENT_FLAG_IS_SPOILER);
                    }
                    attachment
                })
                .collect(),
        );
    }
    body
}

/// A refusal: its status and Fluxer's error body (`{code, message}`, and
/// for a CAPTCHA its challenge), shown as Fluxer words it.
#[derive(Debug)]
pub struct Refused {
    pub status: StatusCode,
    pub body: Value,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = self.body["message"].as_str().unwrap_or_default();
        match self.body["code"].as_str() {
            Some(code) if !message.is_empty() => write!(f, "{message} ({code})"),
            Some(code) => write!(f, "Fluxer refused: {code}"),
            None => write!(f, "Fluxer answered {}.", self.status),
        }
    }
}

impl std::error::Error for Refused {}

/// The OS family only (`X-Fluxer-Client-Properties`, as its apps send it):
/// nothing more about this machine.
fn client_properties() -> String {
    use base64::Engine;
    let os = if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    };
    base64::engine::general_purpose::STANDARD.encode(json!({"os": os}).to_string())
}

impl Api {
    /// Finds the instance's addresses (`GET /.well-known/fluxer`, no token).
    pub async fn discover(origin: &str) -> Result<Instance> {
        let http = client()?;
        let request = http.get(format!("{}/.well-known/fluxer", origin.trim_end_matches('/')));
        let answer: Value = godwit_core::net::send(NET, request)
            .await
            .context("Can't reach Fluxer.")?
            .error_for_status()
            .context("Fluxer didn't answer as expected.")?
            .json()
            .await
            .context("Fluxer's answer couldn't be read.")?;
        Instance::from(&answer).context("Fluxer's addresses are missing from its answer.")
    }

    pub fn new(instance: Instance, token: String) -> Result<Self> {
        Ok(Self {
            http: client()?,
            instance,
            token: RwLock::new(token),
        })
    }

    pub fn instance(&self) -> &Instance {
        &self.instance
    }

    fn token(&self) -> String {
        self.token
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let r = self
            .http
            .request(method, self.instance.url(path))
            .header("X-Fluxer-Client-Properties", client_properties());
        let token = self.token();
        if token.is_empty() {
            r
        } else {
            r.header(header::AUTHORIZATION, token)
        }
    }

    /// Sends once, paced; a 429 sets the pause for everyone and fails.
    async fn send(&self, request: RequestBuilder) -> Result<Value> {
        {
            let mut next = PACE.lock().await;
            if let Some(due) = *next {
                tokio::time::sleep_until(due).await;
            }
            *next = Some(Instant::now() + SPACING);
        }
        let response = godwit_core::net::send(NET, request)
            .await
            .context("Can't reach Fluxer.")?;
        let status = response.status();
        let header_wait = godwit_core::backend::retry_after(
            response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Duration::from_secs(1),
        );
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status == StatusCode::TOO_MANY_REQUESTS {
            let wait = body["retry_after"]
                .as_f64()
                .and_then(|s| Duration::try_from_secs_f64(s).ok())
                .unwrap_or(Duration::from_secs(5))
                .max(header_wait);
            let wait = if body["global"] == true {
                wait.max(Duration::from_secs(60))
            } else {
                wait
            };
            *PACE.lock().await = Some(godwit_core::backend::checked_deadline(wait));
            bail!("Fluxer asks Godwit to slow down. Try again in {} s.", wait.as_secs());
        }
        if !status.is_success() {
            return Err(Refused { status, body }.into());
        }
        Ok(body)
    }

    /// `POST /auth/login` (email or username, and password), solving the
    /// CAPTCHA when Fluxer asks for one.
    pub async fn login(&self, login: &str, password: &str) -> Result<Login> {
        let body = json!({"login": login, "password": password});
        let mut captcha: Option<String> = None;
        for _ in 0..CAPTCHA_TRIES {
            let mut request = self.request(Method::POST, "/auth/login").json(&body);
            if let Some(token) = &captcha {
                request = request.header("X-Captcha-Token", token);
            }
            match self.send(request).await {
                Ok(answer) => return logged_in(&answer),
                Err(error) => {
                    let Some(challenge) = challenge_of(&error) else {
                        return Err(error);
                    };
                    let solved = tokio::task::spawn_blocking(move || crate::captcha::solve(&challenge))
                        .await
                        .ok()
                        .flatten();
                    captcha = Some(solved.context("Fluxer's CAPTCHA couldn't be solved here.")?);
                }
            }
        }
        bail!("Fluxer's CAPTCHA wasn't accepted. Try again in a minute.")
    }

    /// `POST /auth/login/mfa/totp`: the authenticator (or a backup) code.
    pub async fn totp(&self, ticket: &str, code: &str) -> Result<Login> {
        let body = json!({"code": code.trim(), "ticket": ticket});
        let answer = self
            .send(self.request(Method::POST, "/auth/login/mfa/totp").json(&body))
            .await?;
        logged_in(&answer)
    }

    /// Ends this session at Fluxer (`POST /auth/logout`).
    pub async fn logout(&self) -> Result<()> {
        self.send(self.request(Method::POST, "/auth/logout")).await.map(drop)
    }

    /// A channel's messages, newest first, 50 at a time.
    pub async fn history(&self, channel: &str, before: Option<&str>) -> Result<Value> {
        let mut path = format!("/channels/{channel}/messages?limit=50");
        if let Some(before) = before {
            path.push_str(&format!("&before={before}"));
        }
        self.send(self.request(Method::GET, &path)).await
    }

    pub async fn send_message(
        &self,
        channel: &str,
        content: &str,
        nonce: &str,
        reply: Option<&str>,
        files: &[godwit_core::application::Outgoing],
    ) -> Result<Value> {
        let mut body = message_body(content, nonce, files);
        if let Some(reply) = reply {
            body["message_reference"] = json!({"channel_id": channel, "message_id": reply, "type": 0});
        }
        let request = self.request(Method::POST, &format!("/channels/{channel}/messages"));
        if files.is_empty() {
            return self.send(request.json(&body)).await;
        }
        if files.len() > MAX_FILES_PER_MESSAGE {
            bail!("Attach at most ten files at once.");
        }
        let mut form = reqwest::multipart::Form::new().text("payload_json", body.to_string());
        let mut total = 0u64;
        for (id, file) in files.iter().enumerate() {
            let bytes = godwit_core::media::read_upload(file)
                .await
                .map_err(anyhow::Error::msg)?;
            total += bytes.len() as u64;
            if total > godwit_core::media::MAX_UPLOAD {
                bail!("Attachments in one message must total at most 25 MiB.");
            }
            let part = reqwest::multipart::Part::bytes(bytes)
                .file_name(file.name.to_string())
                .mime_str(godwit_core::media::mime(&file.name))
                .context("unusable attachment type")?;
            form = form.part(format!("files[{id}]"), part);
        }
        self.send(request.multipart(form)).await
    }

    pub async fn edit_message(&self, channel: &str, message: &str, content: &str) -> Result<Value> {
        let path = format!("/channels/{channel}/messages/{message}");
        self.send(self.request(Method::PATCH, &path).json(&json!({"content": content})))
            .await
    }

    pub async fn delete_message(&self, channel: &str, message: &str) -> Result<()> {
        let path = format!("/channels/{channel}/messages/{message}");
        self.send(self.request(Method::DELETE, &path)).await.map(drop)
    }

    /// Our reaction on or off; `emoji` is the character, or `name:id`.
    pub async fn react(&self, channel: &str, message: &str, emoji: &str, on: bool) -> Result<()> {
        let path = format!("/channels/{channel}/messages/{message}/reactions/{}/@me", encode(emoji));
        let method = if on { Method::PUT } else { Method::DELETE };
        self.send(self.request(method, &path)).await.map(drop)
    }

    pub async fn typing(&self, channel: &str) -> Result<()> {
        self.send(self.request(Method::POST, &format!("/channels/{channel}/typing")))
            .await
            .map(drop)
    }

    /// Read up to `message` (`POST …/messages/{id}/ack`).
    pub async fn ack(&self, channel: &str, message: &str) -> Result<()> {
        let path = format!("/channels/{channel}/messages/{message}/ack");
        self.send(self.request(Method::POST, &path).json(&json!({"mention_count": 0})))
            .await
            .map(drop)
    }

    /// Fluxer's documented indexed-message search shape: https://docs.fluxer.com/http-api/search/.
    pub async fn search(
        &self,
        channel: &str,
        guild: Option<&str>,
        query: &godwit_core::search::SearchQuery,
        offset: usize,
    ) -> Result<Value> {
        use godwit_core::search::{SearchHas, SearchScope};
        let page = (offset / 25 + 1).clamp(1, 400);
        let mut body = json!({"scope":"current", "hits_per_page":25, "page":page});
        if let Some(guild) = guild {
            body["context_guild_id"] = json!(guild);
        } else {
            body["context_channel_id"] = json!(channel);
        }
        if !query.text.is_empty() {
            body["content"] = json!(query.text);
        }
        if query.scope == SearchScope::Channel {
            body["context_channel_id"] = json!(channel);
        }
        if let Some(user) = &query.from {
            body["author_id"] = json!([user.raw.as_ref()]);
        }
        if let Some(user) = &query.mentions {
            body["mentions"] = json!([user.raw.as_ref()]);
        }
        if query.pinned {
            body["pinned"] = json!(true);
        }
        if let Some(room) = &query.in_room {
            body["channel_ids"] = json!([room.raw.as_ref()]);
        }
        let mut has = Vec::new();
        for item in &query.has {
            has.push(match item {
                SearchHas::Link => "link",
                SearchHas::Embed => "embed",
                SearchHas::File => "file",
                SearchHas::Video => "video",
                SearchHas::Image => "image",
                SearchHas::Sound => "sound",
                SearchHas::Sticker => "sticker",
                SearchHas::Poll => "poll",
                SearchHas::Forward => "snapshot",
            });
        }
        if !has.is_empty() {
            body["has"] = json!(has);
        }
        // Fluxer accepts snowflake bounds. Map local-day boundaries to millisecond snowflakes.
        let after = query
            .after
            .map(|d| (d, true))
            .into_iter()
            .chain(query.during.map(|d| (d, false)));
        let before = query
            .before
            .map(|d| (d, false))
            .into_iter()
            .chain(query.during.map(|d| (d, true)));
        let min = after.filter_map(|(d, end)| Self::day_snowflake(d, end)).max();
        let max = before.filter_map(|(d, end)| Self::day_snowflake(d, end)).min();
        if let Some(id) = min {
            body["min_id"] = json!(id);
        }
        if let Some(id) = max {
            body["max_id"] = json!(id);
        }
        self.send(self.request(Method::POST, "/search/messages").json(&body))
            .await
    }

    /// Converts a day boundary to Fluxer's documented snowflake epoch.
    fn day_snowflake(d: godwit_core::search::SearchDate, next_day: bool) -> Option<String> {
        let date = chrono::NaiveDate::from_ymd_opt(d.year, u32::from(d.month), u32::from(d.day))?;
        let date = if next_day { date.succ_opt()? } else { date };
        let millis = date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis();
        let delta = u64::try_from(millis.checked_sub(1_420_070_400_000)?).ok()?;
        Some((delta.checked_shl(22)?).to_string())
    }

    /// Lists pins: https://docs.fluxer.com/http-api/messages/.
    pub async fn pins(&self, channel: &str) -> Result<Value> {
        self.send(self.request(Method::GET, &format!("/channels/{channel}/messages/pins?limit=50")))
            .await
    }

    pub async fn set_pinned(&self, channel: &str, message: &str, pinned: bool) -> Result<()> {
        let method = if pinned { Method::PUT } else { Method::DELETE };
        self.send(self.request(method, &format!("/channels/{channel}/pins/{message}")))
            .await
            .map(drop)
    }

    /// Fluxer instance GIF provider endpoints: https://docs.fluxer.com/http-api/gifs/.
    pub async fn gif_featured(&self) -> Result<Value> {
        self.send(self.request(Method::GET, "/gifs/featured")).await
    }
    pub async fn gif_trending(&self) -> Result<Value> {
        self.send(self.request(Method::GET, "/gifs/trending")).await
    }
    pub async fn gif_search(&self, query: &str) -> Result<Value> {
        self.send(self.request(
            Method::GET,
            &format!("/gifs/search?q={}", godwit_core::encoding::encode_uri_component(query)),
        ))
        .await
    }
}

#[cfg(test)]
mod attachment_tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn outgoing(name: &str, spoiler: bool) -> godwit_core::application::Outgoing {
        godwit_core::application::Outgoing {
            path: PathBuf::from(name),
            name: Arc::from(name),
            size: 1,
            spoiler,
        }
    }

    #[test]
    fn message_body_maps_text_file_order_and_spoiler_flag() {
        let body = message_body("hello", "nonce", &[outgoing("a.png", true), outgoing("b.txt", false)]);
        assert_eq!(body["content"], "hello");
        assert_eq!(body["nonce"], "nonce");
        assert_eq!(
            body["attachments"][0],
            json!({"id": 0, "filename": "a.png", "flags": 8})
        );
        assert_eq!(body["attachments"][1], json!({"id": 1, "filename": "b.txt"}));
    }

    #[test]
    fn message_body_omits_attachments_without_files() {
        assert_eq!(message_body("hello", "n", &[]), json!({"content":"hello", "nonce":"n"}));
    }
}

/// The challenge in a 400 `CAPTCHA_REQUIRED` (or `INVALID_CAPTCHA`, which
/// brings a new one).
fn challenge_of(error: &anyhow::Error) -> Option<Value> {
    let refused = error.downcast_ref::<Refused>()?;
    let code = refused.body["code"].as_str()?;
    matches!(code, "CAPTCHA_REQUIRED" | "INVALID_CAPTCHA")
        .then(|| refused.body["altcha_challenge"].clone())
        .filter(Value::is_object)
}

fn logged_in(answer: &Value) -> Result<Login> {
    if answer["mfa"].as_bool() == Some(true) {
        let ticket = answer["ticket"]
            .as_str()
            .context("Fluxer asked for a code without a ticket.")?;
        if answer["totp"].as_bool() == Some(false) && answer["backup_codes"].as_bool() == Some(false) {
            bail!("This account's second factor is a security key, which Godwit can't use yet.");
        }
        return Ok(Login::Mfa {
            ticket: ticket.to_owned(),
        });
    }
    let token = answer["token"]
        .as_str()
        .context("Fluxer didn't hand over the sign-in.")?;
    Ok(Login::Token {
        token: token.to_owned(),
        user: answer["user_id"].as_str().unwrap_or_default().to_owned(),
    })
}

fn client() -> Result<Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut headers = header::HeaderMap::new();
    // Shown in the account's sessions list: say what it is.
    headers.insert(header::USER_AGENT, HeaderValue::from_static("Godwit"));
    Client::builder()
        .default_headers(headers)
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(60))
        .build()
        .context("could not set up HTTPS")
}

/// Percent-encodes a path segment (an emoji).
fn encode(s: &str) -> String {
    godwit_core::encoding::percent_encode_with(s, b":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_in_answers_read_as_token_or_code() {
        assert_eq!(
            logged_in(&json!({"token": "t", "user_id": "1"})).unwrap(),
            Login::Token {
                token: "t".into(),
                user: "1".into()
            }
        );
        assert_eq!(
            logged_in(&json!({"mfa": true, "ticket": "k", "totp": true})).unwrap(),
            Login::Mfa { ticket: "k".into() }
        );
        assert!(logged_in(&json!({"mfa": true, "ticket": "k", "totp": false, "backup_codes": false})).is_err());
    }

    #[test]
    fn captcha_challenges_are_found_in_refusals() {
        let challenge = json!({"parameters": {"algorithm": "PBKDF2/SHA-256"}, "signature": "s"});
        let refused = |body| {
            anyhow::Error::from(Refused {
                status: StatusCode::BAD_REQUEST,
                body,
            })
        };
        let body = json!({"code": "CAPTCHA_REQUIRED", "message": "m", "altcha_challenge": challenge});
        assert_eq!(challenge_of(&refused(body)), Some(challenge));
        let error = refused(json!({"code": "INVALID_FORM_BODY", "message": "Too short"}));
        assert!(challenge_of(&error).is_none());
        assert_eq!(error.to_string(), "Too short (INVALID_FORM_BODY)");
    }

    #[test]
    fn fluxer_search_days_become_snowflake_bounds() {
        let day = godwit_core::search::SearchDate {
            year: 2025,
            month: 1,
            day: 2,
        };
        let start = Api::day_snowflake(day, false)
            .expect("day start")
            .parse::<u64>()
            .expect("snowflake");
        let end = Api::day_snowflake(day, true)
            .expect("next day")
            .parse::<u64>()
            .expect("snowflake");
        assert!(end > start);
        assert_eq!(start & ((1 << 22) - 1), 0);
    }

    #[test]
    fn gif_search_query_is_url_encoded() {
        assert_eq!(
            godwit_core::encoding::encode_uri_component("cats & dogs"),
            "cats%20%26%20dogs"
        );
    }

    #[test]
    fn emoji_are_encoded_for_the_path() {
        assert_eq!(encode("👍"), "%F0%9F%91%8D");
        assert_eq!(encode("wave:123"), "wave:123");
    }
}
