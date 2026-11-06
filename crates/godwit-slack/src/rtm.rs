// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A workspace's websocket (`rtm.connect`): Slack pushes events as JSON
//! (`message`, `user_typing`, `channel_marked`, …); a `ping` every 30 s
//! keeps it up (<https://api.slack.com/rtm>). The address carries a
//! one-time ticket: it's never logged.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::api::Api;

const PING_EVERY: Duration = Duration::from_secs(30);
/// Nothing at all (pongs included) for this long: the line is dead.
const SILENCE: Duration = Duration::from_secs(90);
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Why a connection ended.
#[derive(Debug)]
pub struct Ended {
    pub reason: String,
    pub signed_out: bool,
}

fn lost(reason: impl Into<String>) -> Ended {
    Ended {
        reason: reason.into(),
        signed_out: false,
    }
}

/// A websocket error's kind, without anything the request carried.
fn kind(e: &tokio_tungstenite::tungstenite::Error) -> String {
    use tokio_tungstenite::tungstenite::Error as E;
    match e {
        E::Http(r) => format!("HTTP {}", r.status()),
        E::ConnectionClosed | E::AlreadyClosed => "closed".into(),
        E::Io(io) => format!("I/O: {}", io.kind()),
        E::Tls(_) => "TLS".into(),
        E::Protocol(p) => format!("protocol: {p}"),
        E::Capacity(_) => "too large".into(),
        _ => "other".into(),
    }
}

/// One connection, until it ends: every event goes to `events`; `connected`
/// runs on Slack's `hello`.
/// `presence`: the people whose presence we follow (Slack's `presence_sub`,
/// sent again on each connection and whenever the list changes; Slack then
/// sends their `presence_change` events).
pub async fn run(
    api: &Api,
    events: &mpsc::UnboundedSender<Value>,
    presence: &mut tokio::sync::watch::Receiver<Vec<String>>,
    connected: impl Fn(),
) -> Ended {
    let url = match api.rtm_url().await {
        Ok(u) => u,
        Err(e) => {
            let signed_out = e
                .downcast_ref::<crate::api::Refused>()
                .is_some_and(crate::api::Refused::signed_out);
            return Ended {
                reason: format!("{e:#}"),
                signed_out,
            };
        }
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME));
    // The web client's cookie and origin: with the token alone, the client
    // token's socket is dropped at once.
    let Ok(mut request) = url.as_str().into_client_request() else {
        return lost("Slack's websocket address isn't valid");
    };
    let Ok(cookie) = api.cookie() else {
        return lost("the Slack cookie isn't valid");
    };
    let Ok(cookie) = HeaderValue::from_bytes(cookie.as_bytes()) else {
        return lost("the Slack cookie isn't valid");
    };
    request.headers_mut().insert("cookie", cookie);
    request
        .headers_mut()
        .insert("origin", HeaderValue::from_static("https://app.slack.com"));
    let mut socket = match timeout(
        IO_TIMEOUT,
        tokio_tungstenite::connect_async_with_config(request, Some(config), false),
    )
    .await
    {
        Ok(Ok((s, _))) => s,
        // Only the error's kind: the request carried the cookie.
        Ok(Err(e)) => return lost(format!("Can't connect to Slack's live updates ({})", kind(&e))),
        Err(_) => return lost("Slack's live updates didn't answer"),
    };
    // The web client's activity frame (distinct from transport pings), as
    // documented by pannal's Slack activity userscript linked in MEMORY.md.
    // A manual Away disables it; only one frame every two minutes.
    let mut active = api.active_list();
    let mut tickle = tokio::time::interval(Duration::from_secs(120));
    tickle.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tickle.reset();
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ping.reset();
    let mut id: u64 = 1;
    let mut last_received = tokio::time::Instant::now();
    loop {
        tokio::select! {
            next = tokio::time::timeout_at(last_received + SILENCE, socket.next()) => {
                if matches!(&next, Ok(Some(Ok(_)))) { last_received = tokio::time::Instant::now(); }
                let text = match next {
                    Err(_) => return lost("Slack's live updates went quiet"),
                    Ok(None) => return lost("Slack closed the live updates"),
                    Ok(Some(Err(e))) => return lost(format!("Slack's live updates dropped ({})", kind(&e))),
                    Ok(Some(Ok(Message::Text(t)))) => t,
                    Ok(Some(Ok(Message::Close(_)))) => return lost("Slack closed the live updates"),
                    Ok(Some(Ok(_))) => continue,
                };
                let Ok(event) = serde_json::from_str::<Value>(&text) else { continue };
                match event["type"].as_str().unwrap_or_default() {
                    "hello" => {
                        connected();
                        let ids = presence.borrow_and_update().clone();
                        if !ids.is_empty() {
                            let frame = json!({"type": "presence_sub", "ids": ids}).to_string();
                            if !matches!(timeout(IO_TIMEOUT, socket.send(Message::text(frame))).await, Ok(Ok(()))) {
                                return lost("Slack's live updates stalled");
                            }
                        }
                    }
                    // Slack asks clients to reconnect elsewhere.
                    "goodbye" => return lost("Slack moved the live updates"),
                    "pong" => {}
                    _ => {
                        let _ = events.send(event);
                    }
                }
            }
            Ok(()) = presence.changed() => {
                let ids = presence.borrow_and_update().clone();
                let frame = json!({"type": "presence_sub", "ids": ids}).to_string();
                match timeout(IO_TIMEOUT, socket.send(Message::text(frame))).await {
                    Ok(Ok(())) => {}
                    _ => return lost("Slack's live updates stalled"),
                }
            }
            _ = tickle.tick(), if *active.borrow() => {
                let frame = json!({"id": id, "type": "tickle"}).to_string();
                id = id.wrapping_add(1);
                if !matches!(timeout(IO_TIMEOUT, socket.send(Message::text(frame))).await, Ok(Ok(()))) {
                    return lost("Slack's activity update stalled");
                }
            }
            Ok(()) = active.changed() => {
                let enabled = *active.borrow_and_update();
                if enabled {
                    let frame = json!({"id": id, "type": "tickle"}).to_string();
                    id = id.wrapping_add(1);
                    if !matches!(timeout(IO_TIMEOUT, socket.send(Message::text(frame))).await, Ok(Ok(()))) {
                        return lost("Slack's activity update stalled");
                    }
                    tickle.reset();
                }
            }
            _ = ping.tick() => {
                let frame = json!({"id": id, "type": "ping"}).to_string();
                id += 1;
                match timeout(IO_TIMEOUT, socket.send(Message::text(frame))).await {
                    Ok(Ok(())) => {}
                    _ => return lost("Slack's live updates stalled"),
                }
            }
        }
    }
}
