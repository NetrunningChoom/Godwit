// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Fluxer's gateway (docs.fluxer.com, Gateway): `?v=1&encoding=json&
//! compress=zstd-stream&stream=1` (zstd-stream is the only compression: every
//! server frame is binary, one decompressor for the connection), HELLO, then
//! IDENTIFY (op 2) or RESUME (op 6, within 60 s), heartbeats (op 1) after a
//! random part of the interval, as Discord's.
//!
//! A lost connection resumes once; otherwise the caller connects again after
//! a back-off (10 s up to 5 min). Commands are spaced by 500 ms and held to
//! 40 a minute, as Godwit's Discord gateway.

use std::collections::VecDeque;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const QUERY: &str = "/?v=1&encoding=json&compress=zstd-stream&stream=1";
const DISPATCH: u64 = 0;
const HEARTBEAT: u64 = 1;
const IDENTIFY: u64 = 2;
const RESUME: u64 = 6;
const RECONNECT: u64 = 7;
const INVALID_SESSION: u64 = 9;
const HELLO: u64 = 10;
const HEARTBEAT_ACK: u64 = 11;
/// Member lists and typing per guild (docs: Lazy Request).
pub const LAZY_REQUEST: u64 = 14;
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const SPACING: Duration = Duration::from_millis(500);
const PER_MINUTE: usize = 40;
/// Close codes after which connecting again can't help (bad token, version).
const FINAL: [u16; 3] = [4004, 4011, 4012];

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The gateway's zstd stream, decompressed frame by frame.
struct Inflater {
    decoder: zstd::stream::raw::Decoder<'static>,
    buffer: Vec<u8>,
}

impl Inflater {
    fn new() -> Result<Self> {
        Ok(Self {
            decoder: zstd::stream::raw::Decoder::new().context("could not start zstd")?,
            buffer: vec![0; 64 << 10],
        })
    }

    /// One binary frame (a flushed part of the stream) as its JSON text.
    fn inflate(&mut self, frame: &[u8]) -> Result<Vec<u8>> {
        use zstd::stream::raw::{InBuffer, Operation, OutBuffer};
        const MOST: usize = 64 << 20;
        let mut input = InBuffer::around(frame);
        let mut out = Vec::new();
        loop {
            let mut output = OutBuffer::around(&mut self.buffer[..]);
            self.decoder
                .run(&mut input, &mut output)
                .context("Fluxer's gateway sent broken zstd")?;
            let written = output.pos();
            out.extend_from_slice(&self.buffer[..written]);
            if out.len() > MOST {
                bail!("Fluxer's gateway sent too much at once");
            }
            if input.pos() == frame.len() && written < self.buffer.len() {
                return Ok(out);
            }
        }
    }
}

/// Something the gateway said: a dispatch's name and data.
#[derive(Debug, Clone)]
pub struct Dispatch {
    pub event: String,
    pub data: Value,
}

/// Why the connection ended.
#[derive(Debug)]
pub struct Ended {
    pub reason: String,
    /// Connecting again won't help (the sign-in is gone).
    pub signed_out: bool,
}

impl std::fmt::Display for Ended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for Ended {}

/// A connection's state across a resume.
pub struct Gateway {
    url: String,
    token: String,
    session: Option<(String, String)>,
    sequence: Option<u64>,
}

impl Gateway {
    pub fn new(url: &str, token: &str) -> Self {
        Self {
            url: url.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
            session: None,
            sequence: None,
        }
    }

    /// Runs one connection: identifies (or resumes), passes dispatches on,
    /// sends what comes on `commands`; returns when it ends.
    pub async fn run(
        &mut self,
        commands: &mut mpsc::UnboundedReceiver<(u64, Value)>,
        dispatches: &mpsc::UnboundedSender<Dispatch>,
    ) -> Ended {
        match self.connection(commands, dispatches).await {
            Ok(()) => Ended {
                reason: String::new(),
                signed_out: false,
            },
            Err(e) => match e.downcast::<Ended>() {
                Ok(ended) => ended,
                Err(e) => Ended {
                    reason: format!("{e:#}"),
                    signed_out: false,
                },
            },
        }
    }

    async fn connection(
        &mut self,
        commands: &mut mpsc::UnboundedReceiver<(u64, Value)>,
        dispatches: &mpsc::UnboundedSender<Dispatch>,
    ) -> Result<()> {
        let base = self.session.as_ref().map_or(self.url.clone(), |(_, url)| url.clone());
        let (mut socket, _) = timeout(IO_TIMEOUT, tokio_tungstenite::connect_async(format!("{base}{QUERY}")))
            .await
            .context("Fluxer's gateway didn't answer")?
            .context("could not reach Fluxer's gateway")?;
        let mut inflater = Inflater::new()?;
        let hello = next(&mut socket, &mut inflater).await?.context("the gateway closed")?;
        if hello["op"].as_u64() != Some(HELLO) {
            bail!("the gateway didn't say hello");
        }
        let interval = Duration::from_millis(hello["d"]["heartbeat_interval"].as_u64().unwrap_or(41_250));
        let mut beat = Instant::now() + interval.mul_f64(rand::random::<f64>());
        let mut acked = true;
        match &self.session {
            Some((id, _)) => {
                write(
                    &mut socket,
                    RESUME,
                    json!({"token": self.token, "session_id": id, "seq": self.sequence}),
                )
                .await?;
            }
            None => write(&mut socket, IDENTIFY, identify(&self.token)).await?,
        }
        let mut sent: VecDeque<Instant> = VecDeque::new();
        let mut held: VecDeque<(u64, Value)> = VecDeque::new();
        loop {
            let now = Instant::now();
            sent.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
            let free = match (sent.back(), sent.len() >= PER_MINUTE) {
                (_, true) => sent.front().map_or(now, |t| *t + Duration::from_secs(60)),
                (Some(last), false) => *last + SPACING,
                (None, false) => now,
            };
            tokio::select! {
                () = tokio::time::sleep_until(beat) => {
                    if !acked {
                        bail!("Fluxer's gateway stopped answering heartbeats");
                    }
                    write(&mut socket, HEARTBEAT, json!(self.sequence)).await?;
                    acked = false;
                    beat = Instant::now() + interval;
                }
                () = tokio::time::sleep_until(free), if !held.is_empty() => {
                    if let Some((op, data)) = held.pop_front() {
                        write(&mut socket, op, data).await?;
                        sent.push_back(Instant::now());
                    }
                }
                command = commands.recv() => match command {
                    Some((op, data)) => {
                        // A newer copy of the same command replaces a waiting one.
                        held.retain(|(o, _)| *o != op);
                        held.push_back((op, data));
                    }
                    None => {
                        let _ = socket.close(None).await;
                        return Ok(());
                    }
                },
                message = next(&mut socket, &mut inflater) => {
                    let Some(message) = message? else {
                        bail!("the gateway closed");
                    };
                    if let Some(s) = message["s"].as_u64() {
                        self.sequence = Some(s);
                    }
                    match message["op"].as_u64() {
                        Some(DISPATCH) => {
                            let event = message["t"].as_str().unwrap_or_default().to_owned();
                            let data = message["d"].clone();
                            if event == "READY" {
                                let id = data["session_id"].as_str().unwrap_or_default().to_owned();
                                self.session = (!id.is_empty()).then(|| (id, self.url.clone()));
                            }
                            let _ = dispatches.send(Dispatch { event, data });
                        }
                        Some(HEARTBEAT) => write(&mut socket, HEARTBEAT, json!(self.sequence)).await?,
                        Some(HEARTBEAT_ACK) => acked = true,
                        Some(RECONNECT) => bail!("Fluxer asked to reconnect"),
                        // Fluxer never resumes an invalid session (`d` is always false).
                        Some(INVALID_SESSION) => {
                            self.session = None;
                            self.sequence = None;
                            bail!("Fluxer ended the session");
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

/// Who we are, said plainly: Fluxer documents its API for clients like this.
fn identify(token: &str) -> Value {
    let os = if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    };
    json!({
        "token": token,
        "properties": {"os": os, "browser": "Godwit", "device": "desktop"},
        "presence": {"status": "online", "afk": false, "mobile": false},
        "flags": 0,
    })
}

async fn write(socket: &mut Socket, op: u64, data: Value) -> Result<()> {
    let text = json!({"op": op, "d": data}).to_string();
    timeout(IO_TIMEOUT, socket.send(WsMessage::Text(text.into())))
        .await
        .context("Fluxer's gateway stalled")?
        .context("could not write to Fluxer's gateway")
}

/// The next message (None when the socket closed cleanly).
async fn next(socket: &mut Socket, inflater: &mut Inflater) -> Result<Option<Value>> {
    loop {
        let Some(frame) = socket.next().await else {
            return Ok(None);
        };
        let bytes = match frame.context("Fluxer's gateway connection broke")? {
            WsMessage::Binary(b) => inflater.inflate(&b)?,
            WsMessage::Text(t) => t.as_bytes().to_vec(),
            WsMessage::Close(close) => {
                let code = close.as_ref().map(|c| u16::from(c.code)).unwrap_or(1000);
                let reason = close.map(|c| c.reason.to_string()).unwrap_or_default();
                return Err(Ended {
                    reason: if reason.is_empty() {
                        format!("Fluxer closed the connection ({code})")
                    } else {
                        format!("Fluxer closed the connection: {reason} ({code})")
                    },
                    signed_out: FINAL.contains(&code),
                }
                .into());
            }
            _ => continue,
        };
        if bytes.is_empty() {
            continue;
        }
        return serde_json::from_slice(&bytes)
            .map(Some)
            .context("couldn't read the gateway");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflates_a_zstd_stream_frame_by_frame() {
        use zstd::stream::raw::{Encoder, InBuffer, Operation, OutBuffer};
        let mut encoder = Encoder::new(3).unwrap();
        let mut frame = |text: &str| {
            let mut out = vec![0u8; 4096];
            let mut output = OutBuffer::around(&mut out[..]);
            let mut input = InBuffer::around(text.as_bytes());
            encoder.run(&mut input, &mut output).unwrap();
            encoder.flush(&mut output).unwrap();
            let n = output.pos();
            out.truncate(n);
            out
        };
        let (a, b) = (
            frame(r#"{"op":10,"d":{"heartbeat_interval":41250}}"#),
            frame(r#"{"op":11}"#),
        );
        let mut inflater = Inflater::new().unwrap();
        let hello: Value = serde_json::from_slice(&inflater.inflate(&a).unwrap()).unwrap();
        assert_eq!(hello["d"]["heartbeat_interval"], 41250);
        let ack: Value = serde_json::from_slice(&inflater.inflate(&b).unwrap()).unwrap();
        assert_eq!(ack["op"], 11);
    }

    #[test]
    fn identifies_plainly() {
        let d = identify("t");
        assert_eq!(d["properties"]["browser"], "Godwit");
        assert!(d.get("capabilities").is_none(), "Fluxer has none");
    }
}
