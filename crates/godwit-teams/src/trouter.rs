// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Trouter, Teams' push connection: a socket.io 0.9 websocket that the chat
//! service posts news to (new messages, edits, conversation changes) once
//! this endpoint is registered for them. Flow as purple-teams'
//! `teams_trouter.c`:
//!
//! 1. `POST go.trouter.teams.microsoft.com/v4/a?epid=…` gives the socket's
//!    host (`socketio`), our address on it (`surl`) and signed `connectparams`.
//! 2. `GET {socketio}socket.io/1/?…` gives a session id; the websocket is
//!    `{socketio}socket.io/1/websocket/{id}?…`.
//! 3. On `1::` (connected): `user.authenticate`, `user.activity`, and the
//!    registrar is told to deliver `TeamsCDLWebWorker` news to `surl`.
//! 4. Each `3:::{request}` is acked at once; `…/messaging` bodies holding
//!    an `EventMessage` are passed on.
//!
//! The connection carries tokens (the Skype token as a header, the access
//! token in `user.authenticate`): frames are never logged.

use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use reqwest::header;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::api::{self, Api};

const GO: &str = "https://go.trouter.teams.microsoft.com";
const REGISTRAR: &str = "https://teams.microsoft.com/registrar/prod/V2/registrations";
/// The `tc` parameter (purple-teams' `TEAMS_TROUTER_TCCV` and client version).
const TC: &str = r#"{"cv":"2024.23.01.2","ua":"TeamsCDL","hr":"","v":"49/25113001312"}"#;
/// How long a registration lasts; renewed a minute early.
const REGISTRATION_TTL: Duration = Duration::from_secs(86_400);
/// The web client pings every 30 s (purple-teams).
const PING_EVERY: Duration = Duration::from_secs(30);
/// Nothing at all for this long (pings get answers) means the line is dead.
const SILENCE: Duration = Duration::from_secs(120);
const IO_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest frame or inflated body accepted.
const MAX_FRAME: usize = 16 * 1024 * 1024;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Why a connection ended.
#[derive(Debug)]
pub struct Ended {
    pub reason: String,
    /// The Skype token was refused: get a new one before connecting again.
    pub unauthorized: bool,
}

impl Ended {
    fn lost(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            unauthorized: false,
        }
    }
}

/// What step 1 gives.
#[derive(Debug, Clone, PartialEq)]
struct Info {
    /// `https://…trouter.teams.microsoft.com:443/`
    socketio: String,
    surl: String,
    /// The query every later step sends.
    query: String,
    /// Sent back in `user.authenticate`.
    connectparams: Value,
}

/// Only Microsoft's Trouter hosts get our tokens.
fn trusted(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest
        .split(['/', ':', '?'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    host == "go.trouter.teams.microsoft.com"
        || host.ends_with(".trouter.teams.microsoft.com")
        || host.ends_with(".trouter.skype.com")
}

fn info(v: &Value, endpoint: &str) -> Result<Info> {
    let socketio = v["socketio"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("https://go.trouter.teams.microsoft.com/");
    let socketio = if socketio.ends_with('/') {
        socketio.to_owned()
    } else {
        format!("{socketio}/")
    };
    let surl = v["surl"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Trouter gave no address")?;
    if !trusted(&socketio) || !trusted(surl) {
        bail!("Trouter named a host that isn't Microsoft's");
    }
    let mut query = String::from("v=v4&");
    for (k, value) in v["connectparams"].as_object().into_iter().flatten() {
        if let Some(value) = value.as_str() {
            query.push_str(&format!("{}={}&", api::seg(k), api::seg(value)));
        }
    }
    query.push_str(&format!("tc={}&", api::seg(TC)));
    // The web client numbers its connections; one fixed number works (purple-teams).
    query.push_str("con_num=1234567890123_1&");
    query.push_str(&format!("epid={}&", api::seg(endpoint)));
    if let Some(ccid) = v["ccid"].as_str() {
        query.push_str(&format!("ccid={}&", api::seg(ccid)));
    }
    query.push_str("auth=true&timeout=40&");
    Ok(Info {
        socketio,
        surl: surl.to_owned(),
        query,
        connectparams: v["connectparams"].clone(),
    })
}

/// A socket.io frame.
#[derive(Debug, PartialEq)]
enum Frame {
    Disconnect,
    Connected,
    Heartbeat,
    /// `3:::{json}`: the server's request, acked at once.
    Request(Value),
    Other,
}

fn frame(text: &str) -> Frame {
    let mut parts = text.splitn(4, ':');
    let kind = parts.next().unwrap_or_default();
    if parts.next().is_none() {
        return Frame::Other;
    }
    match kind {
        "0" => Frame::Disconnect,
        "1" => Frame::Connected,
        "2" => Frame::Heartbeat,
        "3" => {
            let _endpoint = parts.next();
            serde_json::from_str(parts.next().unwrap_or_default()).map_or(Frame::Other, Frame::Request)
        }
        _ => Frame::Other,
    }
}

/// `3:::{"id":…,"status":200,"body":""}`, keys in the web client's order.
fn ack(id: i64) -> String {
    format!("3:::{{\"id\":{id},\"status\":200,\"body\":\"\"}}")
}

fn gunzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(data)
        .take(MAX_FRAME as u64 + 1)
        .read_to_end(&mut out)
        .context("a Trouter body didn't inflate")?;
    if out.len() > MAX_FRAME {
        bail!("a Trouter body was too large");
    }
    Ok(out)
}

/// A request's body as JSON: gzip + base64 when its headers say so, then a
/// `cp` (gzip + base64 JSON) or `gp` (base64 JSON) inside (purple-teams).
fn body(request: &Value) -> Option<Value> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let raw = request["body"].as_str()?;
    let gzipped = request["headers"]
        .as_object()
        .into_iter()
        .flatten()
        .any(|(k, v)| k.eq_ignore_ascii_case("X-Microsoft-Skype-Content-Encoding") && v == "gzip");
    let text = if gzipped {
        String::from_utf8(gunzip(&b64.decode(raw).ok()?).ok()?).ok()?
    } else {
        raw.to_owned()
    };
    let v: Value = serde_json::from_str(&text).ok()?;
    if let Some(cp) = v["cp"].as_str() {
        return serde_json::from_slice(&gunzip(&b64.decode(cp).ok()?).ok()?).ok();
    }
    if let Some(gp) = v["gp"].as_str() {
        return serde_json::from_slice(&b64.decode(gp).ok()?).ok();
    }
    Some(v)
}

/// The news in a request: an `EventMessage` posted to `…/messaging`.
fn news(request: &Value) -> Option<Value> {
    let url = request["url"].as_str()?;
    if url.ends_with("/TeamsUnifiedPresenceService") {
        let body = body(request)?;
        body["presence"].as_array()?;
        return Some(json!({"resourceType":"Presence", "resource":body}));
    }
    if !url.ends_with("/messaging") {
        return None;
    }
    let body = body(request)?;
    (body["type"] == "EventMessage").then_some(body)
}

/// The registrar entry that routes chat news to `surl` (purple-teams'
/// `TeamsCDLWebWorker_2.1`; its registration id is our endpoint).
fn registration(endpoint: &str, surl: &str) -> Value {
    json!({
        "clientDescription": {
            "appId": "TeamsCDLWebWorker",
            "aesKey": "",
            "languageId": "en-US",
            "platform": "edge",
            "templateKey": "TeamsCDLWebWorker_2.1",
            "platformUIVersion": api::CLIENT_VERSION,
        },
        "registrationId": endpoint,
        "nodeId": "",
        "transports": {"TROUTER": [{"context": "", "path": surl, "ttl": REGISTRATION_TTL.as_secs()}]},
    })
}

/// A random correlation vector for `user.activity` (purple-teams).
fn correlation_vector() -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789/+";
    let mut cv: String = (0..21)
        .map(|_| char::from(CHARS[rand::random_range(0..CHARS.len())]))
        .collect();
    cv.push(char::from(b"AgQw"[rand::random_range(0..4)]));
    cv
}

fn status_error(what: &str, status: u16) -> Ended {
    Ended {
        reason: format!("Teams' push service refused {what} ({status})"),
        unauthorized: status == 401,
    }
}

async fn register(api: &Api, endpoint: &str, surl: &str) -> Result<(), Ended> {
    let t = api.tokens();
    let answer = api
        .http()
        .post(REGISTRAR)
        .header("x-skypetoken", t.skype)
        .bearer_auth(t.access)
        .json(&registration(endpoint, surl))
        .send()
        .await
        .map_err(|_| Ended::lost("Can't reach Teams' push registrar"))?;
    let status = answer.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(status_error("the registration", status));
    }
    Ok(())
}

async fn open(api: &Api, endpoint: &str) -> Result<(Socket, Info), Ended> {
    let skype = api.tokens().skype;
    let answer = api
        .http()
        .post(format!("{GO}/v4/a?epid={}", api::seg(endpoint)))
        .header("x-skypetoken", &skype)
        .header(header::CONTENT_LENGTH, "0")
        .send()
        .await
        .map_err(|_| Ended::lost("Can't reach Teams' push service"))?;
    let status = answer.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(status_error("the connection", status));
    }
    let kind = answer
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("none")
        .to_owned();
    let bytes = answer.bytes().await.map_err(|e| {
        let e = e.without_url();
        let mut why = e.to_string();
        let mut cause = std::error::Error::source(&e);
        while let Some(c) = cause {
            why.push_str(&format!(": {c}"));
            cause = c.source();
        }
        Ended::lost(format!(
            "Teams' push service's answer was cut off ({status}, {kind}: {why})"
        ))
    })?;
    // The body holds signed credentials: only its shape goes in the error.
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| {
        Ended::lost(format!(
            "Teams' push service answered oddly ({status}, {kind}, {} bytes)",
            bytes.len()
        ))
    })?;
    let info = info(&v, endpoint).map_err(|e| Ended::lost(format!("{e:#}")))?;
    let answer = api
        .http()
        .get(format!("{}socket.io/1/?{}", info.socketio, info.query))
        .header("x-skypetoken", &skype)
        .send()
        .await
        .map_err(|_| Ended::lost("Can't reach Teams' push service"))?;
    let status = answer.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(status_error("the handshake", status));
    }
    let text = answer
        .text()
        .await
        .map_err(|_| Ended::lost("Teams' push handshake was cut off"))?;
    // `{session}:{heartbeat}:{timeout}:websocket,xhr-polling`
    let session = text.split(':').next().unwrap_or_default();
    if session.is_empty()
        || !session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(Ended::lost("Teams' push handshake answered oddly"));
    }
    let url = format!(
        "wss://{}socket.io/1/websocket/{session}?{}",
        info.socketio.trim_start_matches("https://"),
        info.query
    );
    let mut request = url
        .into_client_request()
        .map_err(|_| Ended::lost("Teams' push address isn't valid"))?;
    let headers = request.headers_mut();
    let Ok(mut token) = HeaderValue::from_str(&skype) else {
        return Err(Ended::lost("the Skype token isn't a valid header"));
    };
    token.set_sensitive(true);
    headers.insert("X-Skypetoken", token);
    headers.insert(header::USER_AGENT.as_str(), HeaderValue::from_static(api::USER_AGENT));
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME));
    let (socket, _) = timeout(
        IO_TIMEOUT,
        tokio_tungstenite::connect_async_with_config(request, Some(config), false),
    )
    .await
    .map_err(|_| Ended::lost("Teams' push service didn't answer"))?
    .map_err(|_| Ended::lost("Can't connect to Teams' push service"))?;
    Ok((socket, info))
}

async fn send(socket: &mut Socket, text: String) -> Result<(), Ended> {
    timeout(IO_TIMEOUT, socket.send(Message::text(text)))
        .await
        .map_err(|_| Ended::lost("Teams' push connection stalled"))?
        .map_err(|_| Ended::lost("Teams' push connection dropped"))
}

/// One connection, until it ends: `news` gets each `EventMessage`, and
/// `connected` is sent once Trouter says it's up.
pub async fn run(api: &Api, endpoint: &str, news_tx: &mpsc::UnboundedSender<Value>, connected: impl Fn()) -> Ended {
    let (mut socket, info) = match open(api, endpoint).await {
        Ok(s) => s,
        Err(e) => return e,
    };
    let mut seq: u64 = 1;
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ping.reset();
    let renew = REGISTRATION_TTL - Duration::from_secs(60);
    let mut renew_at = tokio::time::Instant::now() + renew;
    let mut registered = false;
    let mut followed = api.presence_list();
    let mut subscribed = std::collections::HashSet::new();
    let mut presence_dirty = false;
    let mut subscribe_at = tokio::time::Instant::now();
    let mut presence_backoff = Duration::from_secs(30);
    let mut last_received = tokio::time::Instant::now();
    loop {
        tokio::select! {
            next = tokio::time::timeout_at(last_received + SILENCE, socket.next()) => {
                if matches!(&next, Ok(Some(Ok(_)))) { last_received = tokio::time::Instant::now(); }
                let text = match next {
                    Err(_) => return Ended::lost("Teams' push connection went quiet"),
                    Ok(None) => return Ended::lost("Teams' push connection closed"),
                    Ok(Some(Err(_))) => return Ended::lost("Teams' push connection dropped"),
                    Ok(Some(Ok(Message::Text(t)))) => t,
                    Ok(Some(Ok(Message::Close(_)))) => return Ended::lost("Teams closed the push connection"),
                    Ok(Some(Ok(_))) => continue,
                };
                match frame(&text) {
                    Frame::Connected => {
                        let t = api.tokens();
                        let authenticate = json!({"name": "user.authenticate", "args": [{
                            "headers": {
                                "X-Ms-Test-User": "False",
                                "Authorization": format!("Bearer {}", t.access),
                                "X-MS-Migration": "True",
                            },
                            "connectparams": info.connectparams,
                        }]});
                        if let Err(e) = send(&mut socket, format!("5:::{authenticate}")).await {
                            return e;
                        }
                        let activity = json!({"name": "user.activity", "args": [{"state": "active", "cv": format!("{}.0.1", correlation_vector())}]});
                        if let Err(e) = send(&mut socket, format!("5:{seq}+::{activity}")).await {
                            return e;
                        }
                        seq += 1;
                        if let Err(e) = register(api, endpoint, &info.surl).await {
                            return e;
                        }
                        registered = true;
                        presence_dirty = true;
                        subscribe_at = tokio::time::Instant::now() + Duration::from_millis(250);
                        renew_at = tokio::time::Instant::now() + renew;
                        connected();
                    }
                    // socket.io 0.9 heartbeats are echoed.
                    Frame::Heartbeat => {
                        if let Err(e) = send(&mut socket, "2::".into()).await {
                            return e;
                        }
                    }
                    Frame::Request(request) => {
                        if let Some(id) = request["id"].as_i64()
                            && let Err(e) = send(&mut socket, ack(id)).await
                        {
                            return e;
                        }
                        if let Some(n) = news(&request) {
                            let _ = news_tx.send(n);
                        }
                    }
                    Frame::Disconnect => return Ended::lost("Teams ended the push connection"),
                    Frame::Other => {}
                }
            }
            Ok(()) = followed.changed() => {
                followed.borrow_and_update();
                presence_dirty = true;
                subscribe_at = tokio::time::Instant::now() + Duration::from_millis(250);
            }
            _ = tokio::time::sleep_until(subscribe_at), if registered && presence_dirty => {
                presence_dirty = false;
                let ids = followed.borrow_and_update().clone();
                let add: Vec<_> = ids.into_iter().filter(|id| !subscribed.contains(id)).collect();
                for batch in add.chunks(100) {
                    match api.subscribe_presence(&info.surl, batch).await {
                        Ok(()) => {
                            subscribed.extend(batch.iter().cloned());
                            presence_backoff = Duration::from_secs(30);
                        }
                        Err(e) => {
                            // Report a failure streak once, not every retry.
                            if presence_backoff == Duration::from_secs(30) {
                                let _ = news_tx.send(json!({"resourceType":"PresenceError","reason":format!("{e:#}")}));
                            }
                            // Retry the rest later, backing off up to 10 minutes.
                            presence_dirty = true;
                            subscribe_at = tokio::time::Instant::now() + presence_backoff;
                            presence_backoff = (presence_backoff * 2).min(Duration::from_secs(600));
                            break;
                        }
                    }
                }
            }
            _ = ping.tick() => {
                if let Err(e) = send(&mut socket, format!("5:{seq}+::{{\"name\":\"ping\"}}")).await {
                    return e;
                }
                seq += 1;
            }
            () = tokio::time::sleep_until(renew_at), if registered => {
                if let Err(e) = register(api, endpoint, &info.surl).await {
                    return e;
                }
                renew_at = tokio::time::Instant::now() + renew;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn info_builds_the_query_and_refuses_foreign_hosts() {
        let v = json!({
            "socketio": "https://trouter2-x.trouter.teams.microsoft.com:443/",
            "surl": "https://trouter2-x.trouter.teams.microsoft.com:3443/v4/f/abc/",
            "connectparams": {"sr": "a b", "sig": "s"},
            "ccid": "c1"
        });
        let i = info(&v, "ep-1").unwrap();
        assert!(i.query.starts_with("v=v4&"));
        assert!(i.query.contains("sr=a%20b&") && i.query.contains("sig=s&"));
        assert!(i.query.contains("epid=ep-1&") && i.query.contains("ccid=c1&"));
        assert!(i.query.ends_with("auth=true&timeout=40&"));
        let mut evil = v.clone();
        evil["socketio"] = "https://trouter.teams.microsoft.com.evil.example/".into();
        assert!(info(&evil, "e").is_err());
        let mut evil = v;
        evil["surl"] = "http://x.trouter.teams.microsoft.com/".into();
        assert!(info(&evil, "e").is_err());
    }

    #[test]
    fn frames_parse() {
        assert_eq!(frame("1::"), Frame::Connected);
        assert_eq!(frame("2::"), Frame::Heartbeat);
        assert_eq!(frame("0::"), Frame::Disconnect);
        assert_eq!(frame("3:::{\"id\":5}"), Frame::Request(json!({"id": 5})));
        assert_eq!(frame("3:::not json"), Frame::Other);
        assert_eq!(frame("garbage"), Frame::Other);
        assert_eq!(ack(5), r#"3:::{"id":5,"status":200,"body":""}"#);
    }

    #[test]
    fn messaging_news_is_decoded_plain_gzipped_or_wrapped() {
        let event = json!({"type": "EventMessage", "resourceType": "NewMessage", "resource": {"id": "1"}});
        let plain = json!({"id": 1, "url": "/v4/f/abc/messaging", "headers": {}, "body": event.to_string()});
        assert_eq!(news(&plain), Some(event.clone()));

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(event.to_string().as_bytes()).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD;
        let packed = b64.encode(gz.finish().unwrap());
        let gzipped = json!({"id": 2, "url": "/v4/f/abc/messaging",
            "headers": {"X-Microsoft-Skype-Content-Encoding": "gzip"}, "body": packed});
        assert_eq!(news(&gzipped), Some(event.clone()));

        let gp = json!({"id": 3, "url": "/x/messaging", "headers": {},
            "body": json!({"gp": b64.encode(event.to_string())}).to_string()});
        assert_eq!(news(&gp), Some(event));

        let presence = json!({"id": 4, "url": "/v4/f/abc/TeamsUnifiedPresenceService", "body": "{}"});
        assert_eq!(news(&presence), None);
    }

    #[test]
    fn live_presence_is_routed_separately_from_chat_messages() {
        let request = json!({"url":"/v4/f/test/TeamsUnifiedPresenceService", "body":json!({"presence":[{"mri":"8:orgid:ann","presence":{"availability":"Away"}}]}).to_string()});
        let event = news(&request).unwrap();
        assert_eq!(event["resourceType"], "Presence");
        assert_eq!(
            crate::api::presences(&event["resource"]),
            vec![("8:orgid:ann".into(), "Away".into(), None)]
        );
    }

    #[test]
    fn registration_routes_chat_news_to_our_address() {
        let r = registration("ep-1", "https://t/v4/f/x/");
        assert_eq!(r["registrationId"], "ep-1");
        assert_eq!(r["transports"]["TROUTER"][0]["path"], "https://t/v4/f/x/");
        assert_eq!(r["clientDescription"]["templateKey"], "TeamsCDLWebWorker_2.1");
    }
}
