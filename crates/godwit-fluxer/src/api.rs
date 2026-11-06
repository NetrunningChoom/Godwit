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
        let answer: Value = http
            .get(format!("{}/.well-known/fluxer", origin.trim_end_matches('/')))
            .send()
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
        let response = request.send().await.context("Can't reach Fluxer.")?;
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
        let mut body = json!({"content": content, "nonce": nonce});
        if let Some(reply) = reply {
            body["message_reference"] = json!({"channel_id": channel, "message_id": reply, "type": 0});
        }
        let request = self.request(Method::POST, &format!("/channels/{channel}/messages"));
        if files.is_empty() {
            return self.send(request.json(&body)).await;
        }
        if files.len() > 10 {
            bail!("Attach at most ten files at once.");
        }
        body["attachments"] = Value::Array(
            files
                .iter()
                .enumerate()
                .map(|(id, file)| json!({"id":id,"filename":file.name.as_ref()}))
                .collect(),
        );
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
    fn emoji_are_encoded_for_the_path() {
        assert_eq!(encode("👍"), "%F0%9F%91%8D");
        assert_eq!(encode("wave:123"), "wave:123");
    }
}
