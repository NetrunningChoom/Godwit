// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Signing in by QR code, through Discord's remote login gateway, as the
//! login page of its web client does (ported from Yapfast's `remote_auth.rs`):
//!
//! 1. Make a 2048-bit RSA key and send its public half.
//! 2. Discord sends a nonce encrypted to it; answer with its SHA-256, which
//!    proves we hold the key.
//! 3. Discord sends a fingerprint: the code is `https://discord.com/ra/{fingerprint}`.
//! 4. Scanning it sends who scanned (encrypted); the phone asks them to confirm.
//! 5. Confirming sends a ticket, which `POST /users/@me/remote-auth/login`
//!    trades for the sign-in, encrypted to the key again.
//!
//! The gateway closes the connection about two minutes after the code
//! shows; a new code needs a new connection. The key lives only in memory
//! for one attempt.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use rsa::pkcs8::EncodePublicKey;
use rsa::rand_core::OsRng;
use rsa::{Oaep, RsaPrivateKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, header};

use crate::api::Api;
use crate::client;

const URL: &str = "wss://remote-auth-gateway.discord.gg/?v=2";
/// The key size Discord's web client uses.
const KEY_BITS: usize = 2048;

/// How far signing in got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// The code to show, as the address the phone app scans.
    Code(String),
    /// Someone scanned it; their app asks them to confirm.
    Scanned { name: String },
    /// They confirmed, and Discord wants a captcha solved first: the page
    /// with it, to open in the browser.
    Captcha(String),
}

/// Why signing in stopped without a sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// The gateway closed, as it does a while after the code shows.
    Expired,
    /// They said no on their phone.
    Cancelled,
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Expired => "The code expired.",
            Self::Cancelled => "Signing in was cancelled on the phone.",
        })
    }
}

impl std::error::Error for Stopped {}

/// Signs in: tells `step` what to show as it goes, and returns the token
/// once the person confirms on their phone.
pub async fn link(mut step: impl FnMut(Step)) -> Result<String> {
    let api = Api::anonymous()?;
    // A browser loads the page before its login gateway connects.
    api.learn_build_number().await;
    let key = tokio::task::spawn_blocking(|| RsaPrivateKey::new(&mut OsRng, KEY_BITS))
        .await?
        .context("could not make a key to sign in with")?;
    let public = key
        .to_public_key()
        .to_public_key_der()
        .context("could not encode the key to sign in with")?;
    let mut request = URL.into_client_request()?;
    let headers = request.headers_mut();
    if let Ok(agent) = HeaderValue::from_str(&client::user_agent()) {
        headers.insert(header::USER_AGENT, agent);
    }
    headers.insert(header::ORIGIN, HeaderValue::from_static("https://discord.com"));
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(15), connect_async(request))
        .await
        .context("Discord's login service didn't answer.")?
        .context("Can't reach Discord's login service.")?;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(40));
    heartbeat.tick().await;
    loop {
        let message = tokio::select! {
            _ = heartbeat.tick() => {
                socket.send(text(json!({"op": "heartbeat"}))).await?;
                continue;
            }
            message = socket.next() => message,
        };
        let message = match message {
            Some(Ok(Message::Text(message))) => message,
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return Err(Stopped::Expired.into()),
            Some(Ok(_)) => continue,
        };
        let message: Value = serde_json::from_str(&message).context("could not understand Discord's login service")?;
        match message["op"].as_str().unwrap_or_default() {
            "hello" => {
                if let Some(ms) = message["heartbeat_interval"].as_u64() {
                    let interval = Duration::from_millis(ms.max(1_000));
                    heartbeat = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
                }
                let init = json!({"op": "init", "encoded_public_key": STANDARD.encode(public.as_bytes())});
                socket.send(text(init)).await?;
            }
            "nonce_proof" => {
                let nonce = decrypt(&key, &message["encrypted_nonce"])?;
                let proof = URL_SAFE_NO_PAD.encode(Sha256::digest(&nonce));
                socket.send(text(json!({"op": "nonce_proof", "proof": proof}))).await?;
            }
            "pending_remote_init" => {
                let fingerprint = message["fingerprint"]
                    .as_str()
                    .context("Discord's login service sent no code")?;
                step(Step::Code(format!("https://discord.com/ra/{fingerprint}")));
            }
            "pending_ticket" => {
                let payload = decrypt(&key, &message["encrypted_user_payload"])?;
                step(scanned(&String::from_utf8_lossy(&payload)));
            }
            "pending_login" => {
                let ticket = message["ticket"]
                    .as_str()
                    .context("Discord's login service sent no ticket")?;
                let encrypted = trade(&api, ticket, &mut socket, &mut heartbeat, &mut step).await;
                let _ = socket.close(None).await;
                return login(&key, &Value::String(encrypted?));
            }
            // The first version of the protocol sent the sign-in itself.
            "pending_finish" => {
                let _ = socket.close(None).await;
                return login(&key, &message["encrypted_token"]);
            }
            "cancel" => return Err(Stopped::Cancelled.into()),
            _ => {}
        }
    }
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Trades a confirmed sign-in's ticket, showing Discord's captcha when it
/// asks for one (up to three times). The login service keeps hearing
/// heartbeats while someone solves it, as it would while the code shows.
async fn trade(
    api: &Api,
    ticket: &str,
    socket: &mut Socket,
    heartbeat: &mut tokio::time::Interval,
    step: &mut impl FnMut(Step),
) -> Result<String> {
    use crate::captcha::{self, Captcha};
    let mut solved: Option<(Captcha, String)> = None;
    for shown in 0..=captcha::TRIES {
        let answer = api
            .remote_auth_login(ticket, solved.as_ref().map(|(c, s)| (c, s.as_str())))
            .await;
        let error = match answer {
            Ok(encrypted) => return Ok(encrypted),
            Err(e) => e,
        };
        let Some(captcha) = error.downcast_ref::<Captcha>().cloned() else {
            return Err(error);
        };
        if shown == captcha::TRIES {
            break;
        }
        let page = captcha::Page::serve(&captcha).await?;
        let solution = page.solve(|url| step(Step::Captcha(url.to_owned())));
        tokio::pin!(solution);
        let mut open = true;
        let solution = loop {
            tokio::select! {
                solution = &mut solution => break solution?,
                _ = heartbeat.tick(), if open => {
                    open = socket.send(text(json!({"op": "heartbeat"}))).await.is_ok();
                }
                message = socket.next(), if open => open = matches!(message, Some(Ok(_))),
            }
        };
        solved = Some((captcha, solution));
    }
    bail!("Discord didn't take the captcha. Try again.")
}

fn text(value: Value) -> Message {
    Message::Text(value.to_string().into())
}

/// Something Discord encrypted to the key, from base64.
fn decrypt(key: &RsaPrivateKey, encrypted: &Value) -> Result<Vec<u8>> {
    let encrypted = STANDARD
        .decode(
            encrypted
                .as_str()
                .context("Discord's login service sent nothing to read")?,
        )
        .context("could not read Discord's login service")?;
    key.decrypt(Oaep::new::<Sha256>(), &encrypted)
        .map_err(|_| anyhow!("could not read what Discord's login service sent"))
}

fn login(key: &RsaPrivateKey, encrypted: &Value) -> Result<String> {
    let token = String::from_utf8(decrypt(key, encrypted)?).context("Discord sent a sign-in Godwit can't read")?;
    if token.is_empty() || token.contains(char::is_whitespace) {
        bail!("Discord sent a sign-in Godwit can't read");
    }
    Ok(token)
}

/// Who scanned, from `id:discriminator:avatar:username`.
fn scanned(payload: &str) -> Step {
    let name = payload.splitn(4, ':').nth(3).unwrap_or_default().to_owned();
    Step::Scanned { name }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_who_scanned() {
        assert_eq!(
            scanned("1400000000000000001:0:abc1:quorble"),
            Step::Scanned { name: "quorble".into() }
        );
        assert_eq!(scanned("garbage"), Step::Scanned { name: String::new() });
    }

    #[test]
    fn decrypts_what_discord_encrypted_to_the_key() {
        let key = RsaPrivateKey::new(&mut OsRng, 1024).unwrap();
        let sealed = key
            .to_public_key()
            .encrypt(&mut OsRng, Oaep::new::<Sha256>(), b"fake-test-token")
            .unwrap();
        let sealed = Value::String(STANDARD.encode(sealed));
        assert_eq!(login(&key, &sealed).unwrap(), "fake-test-token");
        assert!(decrypt(&key, &Value::String("not base64!".into())).is_err());
    }
}
