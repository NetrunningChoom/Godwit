// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! A connection to a Discord voice server: the voice gateway (version 8, as
//! Discord's clients use it), UDP with Discord's transport encryption, and
//! DAVE. `session.rs` runs the media over it.
//!
//! Ported from Yapfast's `voice_connection.rs` (GPL-3.0-or-later). Shapes:
//! <https://docs.discord.food/topics/voice-connections>.

use std::net::IpAddr;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use serenity_voice_model::{Event, serialize_binary_event};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, header};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::dave::{Dave, Out};
use super::depacketize::Codec;
use super::note;
use super::rtp::{self, Transport};

/// The voice gateway version Discord's clients use.
const VERSION: u8 = 8;
pub(crate) const HANDSHAKE: Duration = Duration::from_secs(15);

/// Voice gateway opcodes.
pub(crate) mod op {
    pub const IDENTIFY: u8 = 0;
    pub const SELECT_PROTOCOL: u8 = 1;
    pub const READY: u64 = 2;
    pub const HEARTBEAT: u8 = 3;
    pub const SESSION_DESCRIPTION: u64 = 4;
    pub const SPEAKING: u8 = 5;
    pub const RESUME: u8 = 7;
    pub const HELLO: u64 = 8;
    pub const RESUMED: u64 = 9;
    pub const CLIENTS_CONNECT: u64 = 11;
    pub const VIDEO: u64 = 12;
    pub const CLIENT_DISCONNECT: u64 = 13;
    /// Session Update: the video codec Discord chose changed.
    pub const SESSION_UPDATE: u64 = 14;
    /// Media Sink Wants: which video streams to forward, at what quality.
    pub const MEDIA_SINK_WANTS: u8 = 15;
}

/// The voice gateway's address: always TLS.
fn gateway_url(endpoint: &str) -> String {
    let host = endpoint.trim_start_matches("wss://").trim_end_matches('/');
    // Discord gives "name.discord.media:443"; the port is TLS's default.
    let host = host.strip_suffix(":443").unwrap_or(host);
    format!("wss://{host}/?v={VERSION}")
}

/// The heartbeat interval the voice server asks for, held to a sane range.
fn heartbeat(milliseconds: f64) -> Duration {
    let seconds = if milliseconds.is_finite() {
        milliseconds / 1000.0
    } else {
        41.25
    };
    Duration::from_secs_f64(seconds.clamp(1.0, 300.0))
}

pub(crate) type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Opens the voice gateway as the web client's browser does: its
/// User-Agent, and Discord's origin.
async fn open(endpoint: &str) -> Result<Socket> {
    let mut request = gateway_url(endpoint).into_client_request()?;
    let headers = request.headers_mut();
    headers.insert(
        header::USER_AGENT,
        HeaderValue::from_str(&crate::client::user_agent()).unwrap_or_else(|_| HeaderValue::from_static("Mozilla/5.0")),
    );
    headers.insert(header::ORIGIN, HeaderValue::from_static("https://discord.com"));
    let (socket, _) = timeout(HANDSHAKE, tokio_tungstenite::connect_async(request))
        .await
        .context("the voice server didn't answer")?
        .context("could not reach the voice server")?;
    Ok(socket)
}

pub(crate) async fn send_json(ws: &mut Socket, op: impl Into<u64>, data: Value) -> Result<()> {
    let op = op.into();
    ws.send(Message::text(json!({"op": op, "d": data}).to_string())).await?;
    Ok(())
}

async fn send_out(ws: &mut Socket, out: Vec<Out>) -> Result<()> {
    for out in out {
        let message = match out {
            Out::Json(event) => Message::text(serde_json::to_string(&event)?),
            Out::Binary(event) => Message::binary(serialize_binary_event(&event).map_err(|e| anyhow!("{e}"))?),
        };
        ws.send(message).await?;
    }
    Ok(())
}

/// The voice gateway closed, with Discord's close code if it gave one.
#[derive(Debug)]
pub(crate) struct Closed {
    pub code: Option<u16>,
    pub reason: String,
}

impl Closed {
    /// Codes after which neither resuming nor connecting again helps: 4014
    /// (disconnected from the call: kicked, moved, or joined elsewhere), 4006
    /// (the session is no longer valid), 4004 (authentication failed).
    pub fn is_final(&self) -> bool {
        matches!(self.code, Some(4004 | 4006 | 4014))
    }
}

impl std::fmt::Display for Closed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.code {
            Some(code) => write!(f, "Discord closed the voice connection ({code}: {})", self.reason),
            None => write!(f, "Discord closed the voice connection"),
        }
    }
}

impl std::error::Error for Closed {}

/// A message from the voice gateway: its op and data, or a DAVE event.
pub(crate) enum Incoming {
    Json(u64, Value),
    Dave(Event),
}

/// The next message, noting its sequence number for heartbeats to
/// acknowledge: version 8 numbers JSON messages in `seq`, and binary ones in
/// their first two bytes.
pub(crate) async fn receive(ws: &mut Socket, seq: &mut Option<u64>) -> Result<Incoming> {
    loop {
        let Some(message) = ws.next().await else {
            return Err(Closed {
                code: None,
                reason: String::new(),
            }
            .into());
        };
        match message? {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(&text)?;
                if let Some(number) = value["seq"].as_u64() {
                    *seq = Some(number);
                }
                let op = value["op"].as_u64().unwrap_or(u64::MAX);
                if let Some(event) = dave_json(op, &value["d"]) {
                    return Ok(Incoming::Dave(event));
                }
                return Ok(Incoming::Json(op, value["d"].clone()));
            }
            Message::Binary(bytes) => match binary(&bytes) {
                Ok((number, event)) => {
                    *seq = Some(number);
                    return Ok(Incoming::Dave(event));
                }
                Err(error) => note(&format!("an unknown binary voice message: {error}")),
            },
            Message::Close(frame) => {
                return Err(Closed {
                    code: frame.as_ref().map(|f| u16::from(f.code)),
                    reason: frame.map(|f| f.reason.to_string()).unwrap_or_default(),
                }
                .into());
            }
            _ => {}
        }
    }
}

/// DAVE's JSON ops, and who's connected, as the voice model knows them.
/// Its parser for a whole message wants `op` first and nothing after `d`,
/// which version 8's `seq` breaks, so only the data goes through it.
fn dave_json(op: u64, data: &Value) -> Option<Event> {
    use serenity_voice_model::payload::{
        ClientDisconnect, ClientsConnect, DaveExecuteTransition, DavePrepareEpoch, DavePrepareTransition,
    };
    let event = match op {
        op::CLIENTS_CONNECT => ClientsConnect::deserialize(data).map(Event::from),
        op::CLIENT_DISCONNECT => ClientDisconnect::deserialize(data).map(Event::from),
        21 => DavePrepareTransition::deserialize(data).map(Event::from),
        22 => DaveExecuteTransition::deserialize(data).map(Event::from),
        24 => DavePrepareEpoch::deserialize(data).map(Event::from),
        _ => return None,
    };
    event
        .inspect_err(|error| note(&format!("an unreadable voice gateway message (op {op}): {error}")))
        .ok()
}

/// A binary DAVE message and its sequence number. Version 8 puts the number
/// before the opcode; the voice model reads from the opcode on.
fn binary(bytes: &[u8]) -> Result<(u64, Event)> {
    let Some((number, message)) = bytes.split_first_chunk::<2>() else {
        bail!("a binary voice message too short for its sequence number");
    };
    let event = serenity_voice_model::deserialize_binary_event(message).map_err(|e| anyhow!("{e}"))?;
    Ok((u64::from(u16::from_be_bytes(*number)), event))
}

/// The 74-byte IP discovery request for `ssrc`.
fn discovery_request(ssrc: u32) -> [u8; 74] {
    let mut request = [0u8; 74];
    request[..2].copy_from_slice(&1u16.to_be_bytes());
    request[2..4].copy_from_slice(&70u16.to_be_bytes());
    request[4..8].copy_from_slice(&ssrc.to_be_bytes());
    request
}

/// Reads the IP discovery answer: our public address and port.
fn discovery_answer(answer: &[u8]) -> Result<(IpAddr, u16)> {
    if answer.len() < 74 || u16::from_be_bytes([answer[0], answer[1]]) != 2 {
        bail!("the voice server's address discovery answer was malformed");
    }
    let address = &answer[8..72];
    let end = address.iter().position(|&b| b == 0).unwrap_or(address.len());
    let ip = std::str::from_utf8(&address[..end])?.parse()?;
    Ok((ip, u16::from_be_bytes([answer[72], answer[73]])))
}

/// Discord's address discovery: the server tells the client its public
/// address and port, which it then selects the protocol with.
async fn discover(udp: &UdpSocket, ssrc: u32) -> Result<(IpAddr, u16)> {
    udp.send(&discovery_request(ssrc)).await?;
    let mut answer = [0u8; 74];
    let length = timeout(Duration::from_secs(5), udp.recv(&mut answer))
        .await
        .context("the voice server didn't answer address discovery")??;
    discovery_answer(&answer[..length])
}

/// Who a connection identifies as, and with which server.
#[derive(Clone)]
pub struct Identity {
    pub endpoint: String,
    /// A call's server, or for a DM or group DM call, its channel.
    pub server_id: String,
    pub user_id: String,
    /// The main gateway session's voice session.
    pub session_id: String,
    pub token: String,
    /// This account, and the DAVE group's id: the call's channel (for a
    /// stream, a guess the group's messages correct).
    pub user: u64,
    pub group: u64,
    pub kind: Kind,
}

/// What a voice connection carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A call: everyone's sound, and their cameras.
    Call,
    /// Someone's Go Live stream: its screen and sound.
    Stream,
    /// Our own Go Live stream, sent.
    GoLive,
}

/// The stream id op 12 and identify name a video stream under, as Discord's
/// clients do.
pub(crate) const STREAM_RID: &str = "100";

impl std::fmt::Debug for Identity {
    // The token stays out of logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("endpoint", &self.endpoint)
            .field("server_id", &self.server_id)
            .field("group", &self.group)
            .finish_non_exhaustive()
    }
}

/// What identify says: who we are, the newest DAVE version we speak, and
/// that video may flow: a call's cameras or a stream's screen, as Discord's
/// clients identify (Yapfast's `voice_connection.rs`).
fn identify(identity: &Identity) -> Value {
    let kind = match identity.kind {
        Kind::Call => "video",
        Kind::Stream | Kind::GoLive => "screen",
    };
    json!({
        "server_id": identity.server_id,
        "user_id": identity.user_id,
        "session_id": identity.session_id,
        "token": identity.token,
        "video": true,
        "streams": [{"type": kind, "rid": STREAM_RID, "quality": 100}],
        "max_dave_protocol_version": davey::DAVE_PROTOCOL_VERSION,
    })
}

/// What a connection offers: Opus, and video codecs most wanted first,
/// each with whether Godwit encodes and decodes it. Discord picks, for each
/// sender, the first it encodes that every viewer decodes, and names it in
/// the session description (Yapfast's `stream_connection.rs`).
///
/// A call offers what this build decodes, H264 marked as encoded too, for
/// the camera. Watching a stream decodes only. Our own stream offers H264
/// alone, to encode, as Yapfast's does (Yapfast marks it decodable too).
fn codecs(kind: Kind) -> Value {
    let send = super::outgoing::SEND_CODEC;
    let video: Vec<(Codec, bool, bool)> = match kind {
        Kind::Call => {
            // What we send first (encoded, and decoded where this build
            // can), then the rest this build decodes.
            let decoded = Codec::decoded();
            std::iter::once((send, true, decoded.contains(&send)))
                .chain(decoded.into_iter().filter(|&c| c != send).map(|c| (c, false, true)))
                .collect()
        }
        Kind::Stream => Codec::decoded().into_iter().map(|c| (c, false, true)).collect(),
        Kind::GoLive => vec![(send, true, true)],
    };
    let mut codecs = vec![json!({"name": "opus", "type": "audio", "priority": 1000, "payload_type": rtp::OPUS})];
    for (index, (codec, encode, decode)) in video.into_iter().enumerate() {
        let (payload_type, rtx_payload_type) = codec.payload_types();
        codecs.push(json!({
            "name": codec.name(), "type": "video", "priority": 1000 * (index + 1),
            "payload_type": payload_type, "rtx_payload_type": rtx_payload_type,
            "encode": encode, "decode": decode,
        }));
    }
    Value::Array(codecs)
}

/// The encryption mode to ask for from those Ready offers.
fn choose_mode(ready: &Value) -> Option<&'static str> {
    let modes: Vec<&str> = ready["modes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    Transport::MODES.into_iter().find(|mode| modes.contains(mode))
}

/// A connection after its handshake.
pub struct Connection {
    pub ws: Socket,
    pub udp: UdpSocket,
    pub transport: Transport,
    pub dave: Dave,
    pub ssrc: u32,
    /// Our video's SSRC and its retransmissions', as Ready assigned them.
    pub video_ssrc: u32,
    pub rtx_ssrc: u32,
    pub interval: Duration,
    /// Gateway messages that arrived during the handshake.
    pub early: Vec<(u64, Value)>,
    /// The last sequence number the server sent, for heartbeats.
    pub seq: Option<u64>,
    /// The video codec Discord chose, when it said (op 4, then op 14).
    pub video_codec: Option<Codec>,
}

impl Connection {
    /// Identifies, finds our address, selects the protocol and agrees the
    /// keys; DAVE starts with the version the session description names,
    /// knowing `known` are in the call.
    pub async fn open(identity: &Identity, known: &[u64]) -> Result<Self> {
        let mut ws = open(&identity.endpoint).await?;
        send_json(&mut ws, op::IDENTIFY, identify(identity)).await?;

        let mut dave_early = vec![];
        let mut early = vec![];
        let mut seq = None;
        let handshake = async {
            let (mut interval, mut ready) = (None, None);
            while interval.is_none() || ready.is_none() {
                match receive(&mut ws, &mut seq).await? {
                    Incoming::Json(op::HELLO, hello) => interval = hello["heartbeat_interval"].as_f64(),
                    Incoming::Json(op::READY, data) => ready = Some(data),
                    Incoming::Json(op, data) => early.push((op, data)),
                    Incoming::Dave(event) => dave_early.push(event),
                }
            }
            let (interval, ready) = (interval.unwrap_or(41_250.0), ready.unwrap_or_default());
            let ssrc = ready["ssrc"]
                .as_u64()
                .and_then(|s| u32::try_from(s).ok())
                .context("the voice server sent no SSRC")?;
            // Ready's streams carry the video SSRCs identify asked for;
            // Discord numbers them after the audio one when it doesn't say.
            let assigned = |field: &str, after: u32| {
                ready["streams"][0][field]
                    .as_u64()
                    .and_then(|s| u32::try_from(s).ok())
                    .filter(|&s| s != 0)
                    .unwrap_or(ssrc.wrapping_add(after))
            };
            let (video_ssrc, rtx_ssrc) = (assigned("ssrc", 1), assigned("rtx_ssrc", 2));
            let ip: IpAddr = ready["ip"]
                .as_str()
                .context("the voice server sent no address")?
                .parse()?;
            let port = ready["port"]
                .as_u64()
                .and_then(|p| u16::try_from(p).ok())
                .context("the voice server sent no port")?;
            let mode = choose_mode(&ready).context("the voice server offers no encryption Godwit knows")?;

            let udp = UdpSocket::bind(if ip.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }).await?;
            udp.connect((ip, port)).await?;
            let (address, port) = discover(&udp, ssrc).await?;
            send_json(
                &mut ws,
                op::SELECT_PROTOCOL,
                json!({
                    "protocol": "udp",
                    "codecs": codecs(identity.kind),
                    "data": {"address": address.to_string(), "port": port, "mode": mode},
                }),
            )
            .await?;
            let session = loop {
                match receive(&mut ws, &mut seq).await? {
                    Incoming::Json(op::SESSION_DESCRIPTION, session) => break session,
                    Incoming::Json(op, data) => early.push((op, data)),
                    Incoming::Dave(event) => dave_early.push(event),
                }
            };
            let key: Vec<u8> = session["secret_key"]
                .as_array()
                .context("the voice server sent no key")?
                .iter()
                .filter_map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
                .collect();
            let transport = Transport::new(session["mode"].as_str().unwrap_or(mode), &key)?;
            let version = session["dave_protocol_version"]
                .as_u64()
                .and_then(|v| u16::try_from(v).ok())
                .unwrap_or(0);
            let video_codec = session["video_codec"].as_str().and_then(Codec::from_name);
            anyhow::Ok((
                interval,
                (ssrc, video_ssrc, rtx_ssrc),
                udp,
                transport,
                version,
                video_codec,
            ))
        };
        let (interval, (ssrc, video_ssrc, rtx_ssrc), udp, transport, version, video_codec) =
            timeout(HANDSHAKE, handshake)
                .await
                .context("the voice server took too long to connect")??;

        let mut dave = Dave::new(identity.user, identity.group);
        for &user in known {
            dave.recognise(user);
        }
        for (op, data) in &early {
            if let Some(user) = announced(*op, data) {
                dave.recognise(user);
            }
        }
        send_out(&mut ws, dave.start(version)).await?;
        for event in dave_early {
            send_out(&mut ws, dave.handle(event)).await?;
        }
        note(&format!("connected to the voice server (DAVE version {version})"));
        Ok(Self {
            ws,
            udp,
            transport,
            dave,
            ssrc,
            video_ssrc,
            rtx_ssrc,
            interval: heartbeat(interval),
            early,
            seq,
            video_codec,
        })
    }

    /// Opens the voice gateway again after it dropped, carrying on the same
    /// session: the UDP socket, its keys and DAVE's group stay as they were.
    pub async fn resume(&mut self, identity: &Identity) -> Result<()> {
        let resumed = async {
            let mut ws = open(&identity.endpoint).await?;
            send_json(
                &mut ws,
                op::RESUME,
                json!({
                    "server_id": identity.server_id,
                    "session_id": identity.session_id,
                    "token": identity.token,
                    "seq_ack": self.seq.map_or(-1, |s| i64::try_from(s).unwrap_or(-1)),
                }),
            )
            .await?;
            let mut seq = self.seq;
            loop {
                match receive(&mut ws, &mut seq).await? {
                    Incoming::Json(op::RESUMED, _) => break,
                    Incoming::Json(op::HELLO, hello) => {
                        if let Some(interval) = hello["heartbeat_interval"].as_f64() {
                            self.interval = heartbeat(interval);
                        }
                    }
                    Incoming::Json(op, data) => self.early.push((op, data)),
                    Incoming::Dave(event) => {
                        let out = self.dave.handle(event);
                        send_out(&mut ws, out).await?;
                    }
                }
            }
            anyhow::Ok((ws, seq))
        };
        let (ws, seq) = timeout(HANDSHAKE, resumed)
            .await
            .context("the voice server took too long to resume")??;
        self.ws = ws;
        self.seq = seq;
        note("resumed the voice connection");
        Ok(())
    }

    /// The SSRCs our sound and video go out on.
    pub fn ssrcs(&self) -> super::packetize::Ssrcs {
        super::packetize::Ssrcs {
            audio: self.ssrc,
            video: self.video_ssrc,
            rtx: self.rtx_ssrc,
        }
    }

    pub async fn heartbeat(&mut self) -> Result<()> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(0));
        let seq_ack = self.seq.map_or(-1, |s| i64::try_from(s).unwrap_or(-1));
        send_json(&mut self.ws, op::HEARTBEAT, json!({"t": nonce, "seq_ack": seq_ack})).await
    }

    /// Round trip carried by Discord's heartbeat ACK, or none if malformed.
    pub fn acked(data: &Value) -> Option<u16> {
        let sent = data["t"].as_u64()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis();
        let elapsed = now.checked_sub(u128::from(sent))?;
        Some(u16::try_from(elapsed.min(u128::from(u16::MAX))).unwrap_or(u16::MAX))
    }

    /// Answers DAVE; other messages go back to the caller.
    pub(crate) async fn handle(&mut self, message: Incoming) -> Result<Option<(u64, Value)>> {
        match message {
            Incoming::Dave(event) => {
                let out = self.dave.handle(event);
                send_out(&mut self.ws, out).await?;
                Ok(None)
            }
            Incoming::Json(op, data) => {
                if let Some(user) = announced(op, &data) {
                    self.dave.recognise(user);
                }
                if op == op::SESSION_UPDATE
                    && let Some(codec) = data["video_codec"].as_str().and_then(Codec::from_name)
                {
                    self.video_codec = Some(codec);
                }
                Ok(Some((op, data)))
            }
        }
    }
}

/// Someone a voice gateway message says is in the call, for DAVE's check of
/// the people proposed into its group: speaking (5) and video (12) carry
/// one; op 11, which lists them, goes to DAVE itself.
fn announced(op: u64, data: &Value) -> Option<u64> {
    match op {
        5 | op::VIDEO => data["user_id"].as_str()?.parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gateway_is_always_tls_at_version_8() {
        assert_eq!(
            gateway_url("c-ams-1.discord.media:443"),
            "wss://c-ams-1.discord.media/?v=8"
        );
        assert_eq!(gateway_url("c-ams-1.discord.media"), "wss://c-ams-1.discord.media/?v=8");
    }

    #[test]
    fn heartbeats_stay_in_a_sane_range() {
        assert_eq!(heartbeat(13_750.0), Duration::from_millis(13_750));
        assert_eq!(heartbeat(10.0), Duration::from_secs(1));
        assert_eq!(heartbeat(f64::NAN), Duration::from_millis(41_250));
    }

    /// As version 8 sends them: `seq` after `d`.
    #[test]
    fn reads_daves_json_messages() {
        let read = |text: &str| {
            let value: Value = serde_json::from_str(text).unwrap();
            dave_json(value["op"].as_u64().unwrap(), &value["d"])
        };
        assert!(matches!(
            read(r#"{"op":22,"d":{"transition_id":7},"seq":12}"#),
            Some(Event::DaveExecuteTransition(e)) if e.transition_id == 7
        ));
        assert!(matches!(
            read(r#"{"op":21,"d":{"transition_id":7,"protocol_version":1},"seq":12}"#),
            Some(Event::DavePrepareTransition(p)) if p.transition_id == 7
        ));
        assert!(matches!(
            read(r#"{"op":24,"d":{"epoch":1,"protocol_version":1},"seq":3}"#),
            Some(Event::DavePrepareEpoch(p)) if p.epoch == 1
        ));
        assert!(matches!(
            read(r#"{"op":11,"d":{"user_ids":["1","2"]},"seq":3}"#),
            Some(Event::ClientsConnect(c)) if c.user_ids.len() == 2
        ));
        assert!(matches!(
            read(r#"{"op":13,"d":{"user_id":"1"},"seq":3}"#),
            Some(Event::ClientDisconnect(d)) if d.user_id.0 == 1
        ));
        assert!(read(r#"{"op":5,"d":{"user_id":"1","ssrc":2},"seq":3}"#).is_none());
    }

    #[test]
    fn reads_binary_messages_after_their_sequence_number() {
        // Sequence 5, op 30 (a welcome) for transition 4.
        let (seq, event) = binary(&[0, 5, 30, 0, 4, 1, 2, 3]).unwrap();
        assert_eq!(seq, 5);
        assert!(
            matches!(&event, Event::DaveMlsWelcome(w) if w.transition_id == 4 && w.welcome == [1, 2, 3]),
            "{event:?}"
        );
        assert!(binary(&[0]).is_err());
    }

    #[test]
    fn dave_answers_serialize_as_the_gateway_expects() {
        use serenity_voice_model::payload::{DaveMlsKeyPackage, DaveTransitionReady};
        let ready = serde_json::to_value(Event::from(DaveTransitionReady {
            transition_id: 3,
            protocol_version: 1,
        }))
        .unwrap();
        assert_eq!(ready["op"], 23);
        assert_eq!(ready["d"]["transition_id"], 3);
        // Binary: the opcode (26), then the key package.
        let package = serialize_binary_event(&Event::DaveMlsKeyPackage(DaveMlsKeyPackage {
            key_package: vec![9, 8],
        }))
        .unwrap();
        assert_eq!(package, [26, 9, 8]);
    }

    #[test]
    fn ip_discovery_round_trips() {
        let request = discovery_request(0x0102_0304);
        assert_eq!(&request[..8], &[0, 1, 0, 70, 1, 2, 3, 4]);
        let mut answer = [0u8; 74];
        answer[..2].copy_from_slice(&2u16.to_be_bytes());
        answer[8..19].copy_from_slice(b"203.0.113.9");
        answer[72..].copy_from_slice(&50_000u16.to_be_bytes());
        assert_eq!(
            discovery_answer(&answer).unwrap(),
            ("203.0.113.9".parse::<IpAddr>().unwrap(), 50_000)
        );
        assert!(discovery_answer(&answer[..10]).is_err());
    }

    #[test]
    fn identify_and_select_ask_for_dave_and_aead() {
        let identity = Identity {
            endpoint: "e".into(),
            server_id: "10".into(),
            user_id: "1".into(),
            session_id: "s".into(),
            token: "t".into(),
            user: 1,
            group: 11,
            kind: Kind::Call,
        };
        let d = identify(&identity);
        assert_eq!(d["server_id"], "10");
        assert_eq!(
            (d["video"].clone(), d["streams"][0]["type"].clone()),
            (json!(true), json!("video"))
        );
        let stream = identify(&Identity {
            kind: Kind::Stream,
            ..identity.clone()
        });
        assert_eq!(stream["streams"][0]["type"], "screen");
        assert_eq!(d["max_dave_protocol_version"], davey::DAVE_PROTOCOL_VERSION);
        assert!(!format!("{identity:?}").contains("\"t\""), "no token in logs");
        let ready =
            json!({"modes": ["aead_xchacha20_poly1305_rtpsize", "aead_aes256_gcm_rtpsize", "xsalsa20_poly1305"]});
        assert_eq!(choose_mode(&ready), Some("aead_aes256_gcm_rtpsize"));
        assert_eq!(choose_mode(&json!({"modes": ["xsalsa20_poly1305"]})), None);
        let offered = codecs(Kind::Call);
        assert_eq!(offered[0]["payload_type"], 120);
        let video: Vec<&Value> = offered.as_array().unwrap()[1..].iter().collect();
        assert!(video.iter().all(|c| c["type"] == "video"));
        // The camera goes out in H264; VP8 is decoded only.
        assert!(video.iter().any(|c| c["name"] == "H264"
            && c["payload_type"] == 101
            && c["rtx_payload_type"] == 102
            && c["encode"] == true));
        assert!(video.iter().all(|c| c["name"] == "H264" || c["encode"] == false));
        let watching = codecs(Kind::Stream);
        assert!(watching.as_array().unwrap()[1..].iter().all(|c| c["encode"] == false));
        let streaming = codecs(Kind::GoLive);
        assert_eq!(streaming.as_array().unwrap().len(), 2, "Opus and H264");
        assert_eq!(
            (streaming[1]["name"].as_str(), streaming[1]["encode"].as_bool()),
            (Some("H264"), Some(true))
        );
        assert_eq!(streaming[1]["payload_type"], 101);
        let ours = identify(&Identity {
            kind: Kind::GoLive,
            ..identity.clone()
        });
        assert_eq!(ours["streams"][0]["type"], "screen");
    }
}
