// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Google Chat WebChannel, following mautrix-googlechat's maugclib/channel.py.
//! These are server-held streaming requests, not periodic history polling.
use crate::{proto, session::Saved};
use anyhow::{Context, Result, bail};
use reqwest::{Client, Response, header};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::mpsc;

/// The name requests carry in the network layer's log.
const NET: &str = "gchat";

const FAILURE_NOTE_AFTER: u32 = 3;
const FAILURE_NOTE_WINDOW: Duration = Duration::from_secs(5 * 60);

const BASE: &str = "https://chat.google.com/u/0/webchannel/";
const LIMIT: usize = 16 * 1024 * 1024;
const IO: Duration = Duration::from_secs(60);

/// Framing lengths count JavaScript UTF-16 code units, not UTF-8 bytes.
#[derive(Default)]
struct Chunks {
    bytes: Vec<u8>,
}
impl Chunks {
    fn push(&mut self, data: &[u8]) -> Result<Vec<Value>> {
        if self.bytes.len().saturating_add(data.len()) > LIMIT {
            bail!("Google Chat stream frame too large");
        }
        self.bytes.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            let Some(newline) = self.bytes.iter().position(|b| *b == b'\n') else {
                if self.bytes.len() > 20 {
                    bail!("Google Chat stream length is invalid");
                }
                break;
            };
            let length: usize = std::str::from_utf8(&self.bytes[..newline])
                .ok()
                .and_then(|n| n.parse().ok())
                .context("Google Chat stream length is invalid")?;
            if length > LIMIT {
                bail!("Google Chat stream frame too large");
            }
            let payload = &self.bytes[newline + 1..];
            let text = match std::str::from_utf8(payload) {
                Ok(s) => s,
                Err(e) if e.error_len().is_none() => {
                    std::str::from_utf8(&payload[..e.valid_up_to()]).context("invalid stream text")?
                }
                Err(_) => bail!("Google Chat stream text is invalid"),
            };
            let mut units = 0;
            let mut end = 0;
            for c in text.chars() {
                if units == length {
                    break;
                }
                units += c.len_utf16();
                end += c.len_utf8();
            }
            if units < length {
                break;
            }
            if units != length {
                bail!("Google Chat stream length splits a character");
            }
            out.push(serde_json::from_slice(&payload[..end]).context("Google Chat stream JSON is invalid")?);
            self.bytes.drain(..newline + 1 + end);
        }
        Ok(out)
    }
}

pub enum News {
    Connected,
    Disconnected { signed_out: bool },
    Event(Box<proto::Event>),
}

#[derive(Default)]
struct Downtime {
    since: Option<tokio::time::Instant>,
    failures: u32,
}

impl Downtime {
    fn connected(&mut self) {
        self.since = None;
        self.failures = 0;
    }

    fn failed(&mut self, now: tokio::time::Instant) -> bool {
        let since = *self.since.get_or_insert(now);
        self.failures = self.failures.saturating_add(1);
        self.failures >= FAILURE_NOTE_AFTER || now.duration_since(since) >= FAILURE_NOTE_WINDOW
    }
}

struct Channel {
    http: Client,
    sid: String,
    aid: i64,
    rid: u64,
    offset: u64,
    connected: bool,
    active: tokio::sync::watch::Receiver<bool>,
    base: String,
}

impl Channel {
    async fn new(saved: &Saved, active: tokio::sync::watch::Receiver<bool>) -> Result<Self> {
        Self::new_at(saved, active, BASE).await
    }
    async fn new_at(saved: &Saved, active: tokio::sync::watch::Receiver<bool>, base: &str) -> Result<Self> {
        let mut cookies =
            header::HeaderValue::from_str(&crate::auth::cookie_header(saved)).context("invalid Chat cookies")?;
        cookies.set_sensitive(true);
        let mut headers = header::HeaderMap::new();
        headers.insert(header::COOKIE, cookies);
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static(crate::api::USER_AGENT),
        );
        headers.insert(
            header::REFERER,
            header::HeaderValue::from_static("https://chat.google.com/"),
        );
        let http = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .retry(reqwest::retry::never())
            .build()
            .context("could not prepare Chat stream")?;
        let response = tokio::time::timeout(
            IO,
            godwit_core::net::stream(
                NET,
                godwit_core::net::Class::Auth,
                http.get(format!("{base}register?ignore_compass_cookie=1"))
                    .header(header::CONTENT_TYPE, "application/x-protobuf"),
            ),
        )
        .await
        .context("Chat stream registration timed out")?
        .map_err(|_| anyhow::anyhow!("Chat stream registration failed"))?;
        check(&response)?;
        // Google's COMPASS cookie binds the registered dynamite-ui channel.
        let compass = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|h| h.to_str().ok())
            .find_map(|h| h.split(';').next().filter(|v| v.starts_with("COMPASS=")))
            .map(str::to_owned);
        let http = if let Some(compass) = compass {
            let mut current = saved.clone();
            current.cookies.retain(|(name, _)| name != "COMPASS");
            let mut cookie =
                header::HeaderValue::from_str(&format!("{}; {compass}", crate::auth::cookie_header(&current)))
                    .context("invalid Chat stream cookie")?;
            cookie.set_sensitive(true);
            let mut headers = header::HeaderMap::new();
            headers.insert(header::COOKIE, cookie);
            headers.insert(
                header::USER_AGENT,
                header::HeaderValue::from_static(crate::api::USER_AGENT),
            );
            headers.insert(
                header::REFERER,
                header::HeaderValue::from_static("https://chat.google.com/"),
            );
            Client::builder()
                .default_headers(headers)
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(15))
                .retry(reqwest::retry::never())
                .build()
                .context("could not prepare Chat stream")?
        } else {
            http
        };
        Ok(Self {
            http,
            sid: String::new(),
            aid: 0,
            rid: 10_000 + rand_id() % 90_000,
            offset: 0,
            connected: false,
            active,
            base: base.to_owned(),
        })
    }

    async fn ping(&mut self) -> Result<()> {
        // StreamEventsRequest.ping_event: active, background, interactive,
        // notifications enabled (field numbers from googlechat.proto).
        let body = json!([
            null,
            [if *self.active.borrow() { 1 } else { 2 }, null, 2, null, 1, true]
        ])
        .to_string();
        let response = tokio::time::timeout(
            IO,
            godwit_core::net::stream(
                NET,
                godwit_core::net::Class::Poll(Duration::from_secs(1)),
                self.http
                    .post(format!("{}events", self.base))
                    .query(&[
                        ("VER", "8".to_owned()),
                        ("RID", self.rid.to_string()),
                        ("t", "1".into()),
                        ("SID", self.sid.clone()),
                        ("AID", self.aid.to_string()),
                    ])
                    .form(&[
                        ("count", "1".to_owned()),
                        ("ofs", self.offset.to_string()),
                        ("req0_data", body),
                    ]),
            ),
        )
        .await
        .context("Chat stream ping timed out")?
        .map_err(|_| anyhow::anyhow!("Chat stream ping failed"))?;
        check(&response)?;
        self.rid += 1;
        self.offset += 1;
        Ok(())
    }

    async fn receive(&mut self, news: &mpsc::UnboundedSender<News>) -> Result<()> {
        let initial = self.sid.is_empty();
        let mut query = vec![
            ("VER", "8".to_owned()),
            ("RID", if initial { self.rid.to_string() } else { "rpc".into() }),
            ("SID", if initial { "null".into() } else { self.sid.clone() }),
            ("t", "1".into()),
            ("zx", rand_id().to_string()),
        ];
        if initial {
            query.extend([("CVER", "22".into()), ("$req", "count=1&ofs=0&req0_data=%5B%5D".into())]);
            self.rid += 1;
        } else {
            query.extend([
                ("CI", "0".into()),
                ("TYPE", "xmlhttp".into()),
                ("AID", self.aid.to_string()),
            ]);
        }
        let mut response = tokio::time::timeout(
            IO,
            godwit_core::net::stream(
                NET,
                godwit_core::net::Class::Poll(Duration::from_secs(1)),
                self.http.get(format!("{}events", self.base)).query(&query),
            ),
        )
        .await
        .context("Chat stream connection timed out")?
        .map_err(|_| anyhow::anyhow!("Chat stream connection failed"))?;
        check(&response)?;
        if initial {
            let header = response
                .headers()
                .get("x-http-initial-response")
                .and_then(|h| h.to_str().ok())
                .context("Chat stream gave no session")?;
            let value: Value = serde_json::from_str(header).context("invalid Chat stream session")?;
            self.sid = value[0][1][1]
                .as_str()
                .filter(|s| !s.is_empty())
                .context("Chat stream gave no session")?
                .to_owned();
            // Acknowledge the SID before the initial ping, as maugclib does.
            let ack = tokio::time::timeout(
                IO,
                godwit_core::net::stream(
                    NET,
                    // Sent right after the stream opens, on the same endpoint.
                    godwit_core::net::Class::Live,
                    self.http.get(format!("{}events", self.base)).query(&[
                        ("VER", "8"),
                        ("RID", "rpc"),
                        ("SID", self.sid.as_str()),
                        ("AID", "0"),
                        ("CI", "0"),
                        ("TYPE", "xmlhttp"),
                        ("t", "1"),
                    ]),
                ),
            )
            .await
            .context("Chat stream acknowledgement timed out")?
            .map_err(|_| anyhow::anyhow!("Chat stream acknowledgement failed"))?;
            check(&ack)?;
            self.ping().await?;
        }
        let mut chunks = Chunks::default();
        let mut ping = tokio::time::interval(Duration::from_secs(30));
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ping.reset();
        let mut last_data = tokio::time::Instant::now();
        loop {
            tokio::select! {
                chunk = tokio::time::timeout_at(last_data + IO, response.chunk()) => {
                    let Some(chunk) = chunk.context("Chat stream went quiet")?.map_err(|_| anyhow::anyhow!("Chat stream interrupted"))? else { return Ok(()) };
                    last_data = tokio::time::Instant::now();
                    for value in chunks.push(&chunk)? {
                        if !self.connected { let _ = news.send(News::Connected); self.connected = true; }
                        for item in value.as_array().context("invalid Chat stream envelope")? {
                            let id = item[0].as_i64().context("invalid Chat stream sequence")?;
                            let data = &item[1];
                            if data[0] == "stop" || data[0] == "close" { bail!("Chat stream session ended"); }
                            if data[0] != "noop" && data[0] != "c" {
                                let mut response = proto::StreamEventsResponse::new();
                                crate::pblite::decode(&mut response, ".StreamEventsResponse", &data[0])?;
                                if let Some(event) = response.event.into_option() {
                                    let _ = news.send(News::Event(Box::new(event)));
                                }
                            }
                            self.aid = self.aid.max(id);
                        }
                    }
                }
                Ok(()) = self.active.changed() => { self.active.borrow_and_update(); self.ping().await?; }
                _ = ping.tick() => self.ping().await?,
            }
        }
    }
}

#[derive(Debug)]
struct RateLimited(Duration);
impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Google Chat asked the stream to slow down")
    }
}
impl std::error::Error for RateLimited {}

fn check(response: &Response) -> Result<()> {
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(crate::api::Unauthorized.into());
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let wait = godwit_core::backend::retry_after(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Duration::from_secs(60),
        );
        return Err(RateLimited(wait).into());
    }
    if !status.is_success() {
        bail!("Chat stream HTTP {status}");
    }
    Ok(())
}
fn rand_id() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |d| d.as_millis() as u64)
}

/// Re-register with backoff after transport failures. The owner aborts this
/// task when switching sessions or signing out.
pub async fn run(saved: Saved, news: mpsc::UnboundedSender<News>, active: tokio::sync::watch::Receiver<bool>) {
    let mut wait = Duration::from_secs(5);
    let mut downtime = Downtime::default();
    loop {
        let start = tokio::time::Instant::now();
        let result = async {
            let mut channel = Channel::new(&saved, active.clone()).await?;
            loop {
                let began = tokio::time::Instant::now();
                channel.receive(&news).await?;
                if began.elapsed() < Duration::from_secs(1) {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            #[allow(unreachable_code)]
            anyhow::Ok(())
        }
        .await;
        if news.is_closed() {
            return;
        }
        let mut retry_after = Duration::ZERO;
        if let Err(e) = result {
            let signed_out = crate::api::signed_out(&e);
            let _ = news.send(News::Disconnected { signed_out });
            if signed_out {
                return;
            }
            let should_note = downtime.failed(tokio::time::Instant::now());
            if let Some(rate) = e.downcast_ref::<RateLimited>() {
                retry_after = rate.0;
            }
            if should_note {
                eprintln!("note: Google Chat stream is having trouble reconnecting");
            } else {
                eprintln!("debug: Google Chat stream ended; reconnecting");
            }
        } else {
            downtime.connected();
        }
        if start.elapsed() > Duration::from_secs(120) {
            wait = Duration::from_secs(5);
        }
        // A rate limit is waited out in full; otherwise the wait follows the network.
        let network_back = if retry_after > wait {
            tokio::time::sleep_until(godwit_core::backend::checked_deadline(retry_after)).await;
            false
        } else {
            godwit_core::net::watch::reconnect_pause(NET, "stream", wait).await
        };
        wait = if network_back {
            Duration::from_secs(5)
        } else {
            (wait * 2).min(Duration::from_secs(300))
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downtime_notes_only_after_repeated_failures_and_resets_on_connect() {
        let start = tokio::time::Instant::now();
        let mut downtime = Downtime::default();
        assert!(!downtime.failed(start));
        assert!(!downtime.failed(start + Duration::from_secs(5)));
        assert!(downtime.failed(start + Duration::from_secs(15)));

        downtime.connected();
        assert!(!downtime.failed(start + Duration::from_secs(20)));
        assert!(!downtime.failed(start + Duration::from_secs(25)));
        assert!(downtime.failed(start + Duration::from_secs(30)));
    }

    #[test]
    fn downtime_notes_after_a_long_outage_even_before_three_failures() {
        let start = tokio::time::Instant::now();
        let mut downtime = Downtime::default();
        assert!(!downtime.failed(start));
        assert!(downtime.failed(start + FAILURE_NOTE_WINDOW));
    }

    #[tokio::test]
    async fn mock_handshake_registers_cookie_acknowledges_sid_and_receives_a_message() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let message = json!([
            [[null, null, null, [null, "topic", [["SPACE"]]]], "root"],
            [["ann"], "Ann"],
            42,
            null,
            null,
            null,
            null,
            null,
            null,
            "hello"
        ]);
        let event = json!([[["SPACE"]], null, 6, [null, null, null, null, null, [message]]]);
        let response = json!([event]);
        let text = json!([[1, [response]]]).to_string();
        let frame = format!("{}\n{text}", text.encode_utf16().count());
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for i in 0..4 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut buffer = [0; 2048];
                    let n = socket.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length:")
                                    .and_then(|n| n.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                    assert!(bytes.len() < 16_384);
                }
                requests.push(String::from_utf8(bytes).unwrap());
                let (headers, body) = match i {
                    0 => (
                        "Set-Cookie: COMPASS=dynamite-ui=fixture; Path=/\r\n".to_owned(),
                        String::new(),
                    ),
                    1 => (
                        "X-HTTP-Initial-Response: [[0,[\"c\",\"fixture\",\"\",8]]]\r\n".to_owned(),
                        frame.clone(),
                    ),
                    _ => (String::new(), String::new()),
                };
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
            requests
        });
        let saved = Saved {
            cookies: vec![
                ("SID".into(), "synthetic-only".into()),
                ("SAPISID".into(), "synthetic-only".into()),
                ("COMPASS".into(), "stale-fixture".into()),
            ],
        };
        // Production installs this in Api::new; the test builds a Client directly.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let active = tokio::sync::watch::Sender::new(true);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut channel = Channel::new_at(&saved, active.subscribe(), &format!("http://{address}/"))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), channel.receive(&tx))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(rx.try_recv(), Ok(News::Connected)));
        let News::Event(event) = rx.try_recv().unwrap() else {
            panic!("missing event")
        };
        assert_eq!(event.body.message_posted().message.text_body(), "hello");
        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("GET /register?ignore_compass_cookie=1 "));
        assert!(requests[1].contains("COMPASS=dynamite-ui=fixture"));
        assert_eq!(requests[1].matches("COMPASS=").count(), 1);
        assert!(!requests[1].contains("stale-fixture"));
        assert!(requests[2].contains("SID=fixture"));
        assert!(requests[3].starts_with("POST /events?"));
        assert_eq!(
            requests.len(),
            4,
            "one registration, stream, acknowledgement and initial ping"
        );
    }

    #[test]
    fn chunks_allow_every_utf8_split_and_count_utf16_units() {
        let text = r#"[[3,["hello 🦆"]]]"#;
        let frame = format!("{}\n{text}", text.encode_utf16().count()).into_bytes();
        for split in 0..=frame.len() {
            let mut parser = Chunks::default();
            let mut chunks = parser.push(&frame[..split]).unwrap();
            chunks.extend(parser.push(&frame[split..]).unwrap());
            assert_eq!(chunks, vec![serde_json::from_str::<Value>(text).unwrap()]);
        }
    }
    #[test]
    fn coalesced_frames_and_bad_lengths() {
        let mut parser = Chunks::default();
        assert_eq!(parser.push(b"2\n[]2\n[]").unwrap().len(), 2);
        assert!(Chunks::default().push(b"999999999\n").is_err());
        assert!(Chunks::default().push(b"x\n").is_err());
        assert!(Chunks::default().push(b"1\n\xff").is_err());
    }
}
