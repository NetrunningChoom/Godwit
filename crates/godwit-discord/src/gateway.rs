// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Discord's gateway, as its web client uses it (ported from Yapfast's
//! `gateway.rs`): JSON compressed as one zlib stream, IDENTIFY with the web
//! client's capabilities and properties, heartbeats starting after a random
//! part of the interval, and a lost connection resumed (op 6) rather than
//! identified again. Discord allows 120 commands a minute on a connection;
//! Godwit holds commands back at 40 so heartbeats have ample room. Reconnects back
//! off from 10 seconds to 5 minutes with jitter, and at most 10 IDENTIFYs go in an
//! hour: an account that keeps identifying gets flagged.

use std::collections::VecDeque;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, header};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::client;

const URL: &str = "wss://gateway.discord.gg";
/// What the web client asks of the gateway: JSON, one zlib stream.
const QUERY: &str = "/?encoding=json&v=9&compress=zlib-stream";
/// The capability bits the web client sends (as mautrix-discord and Yapfast).
const CAPABILITIES: u64 = 1734653;

const DISPATCH: u64 = 0;
/// Discord asks for a heartbeat at once with op 1.
const HEARTBEAT: u64 = 1;
/// The official client heartbeats with op 40 (QoS Heartbeat) rather than
/// op 1; both are accepted (docs.discord.food, Sending Heartbeats).
const QOS_HEARTBEAT: u64 = 40;
/// Update Time Spent Session ID: on connect and with each new session.
const TIME_SPENT: u64 = 41;
const IDENTIFY: u64 = 2;
pub const PRESENCE_UPDATE: u64 = 3;
/// Joining, leaving, or muting in a voice channel or DM call.
pub const VOICE_STATE_UPDATE: u64 = 4;
const RESUME: u64 = 6;
const RECONNECT: u64 = 7;
const INVALID_SESSION: u64 = 9;
const HELLO: u64 = 10;
const HEARTBEAT_ACK: u64 = 11;
/// Start our own Go Live stream in the call; answered with STREAM_CREATE
/// and STREAM_SERVER_UPDATE (docs.discord.food, Gateway Opcodes).
pub const STREAM_CREATE: u64 = 18;
/// Stop watching a Go Live stream (or end our own).
pub const STREAM_DELETE: u64 = 19;
/// Watch someone's Go Live stream; answered with STREAM_CREATE and
/// STREAM_SERVER_UPDATE (docs.discord.food, Gateway Opcodes).
pub const STREAM_WATCH: u64 = 20;
/// Pause or resume our own stream (the official client unpauses it once created).
pub const STREAM_SET_PAUSED: u64 = 22;
/// Fetch soundboard sounds for voice channels (Soundboard resource).
pub const REQUEST_SOUNDBOARD_SOUNDS: u64 = 31;
/// Subscribing to a server's typing, activities and a channel's member list.
pub const GUILD_SUBSCRIPTIONS_BULK: u64 = 37;
/// Extra voice channel fields for a server (`fields`: "status",
/// "voice_start_time"), answered with CHANNEL_INFO (docs.discord.food,
/// Gateway Opcodes: 43, superseding 36's channel statuses).
pub const REQUEST_CHANNEL_INFO: u64 = 43;

const SEND_LIMIT: usize = 40;
const SEND_SPACING: Duration = Duration::from_millis(500);
const SEND_WINDOW: Duration = Duration::from_secs(60);
const RESUME_TRIES: u32 = 3;
const RESUME_WAIT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const FIRST_RETRY: Duration = Duration::from_secs(10);
const LAST_RETRY: Duration = Duration::from_secs(300);
/// A session up this long counts as working, and the wait starts over.
const STABLE: Duration = Duration::from_secs(60);
const IDENTIFY_LIMIT: usize = 10;
const IDENTIFY_WINDOW: Duration = Duration::from_secs(60 * 60);

/// Discord closed the gateway, with its close code.
#[derive(Debug)]
pub struct Closed {
    pub code: u16,
    pub reason: String,
}

impl Closed {
    /// Codes after which the same token can't help: authentication failed,
    /// and sharding/version/intent errors.
    pub fn is_final(&self) -> bool {
        matches!(self.code, 4004 | 4010 | 4011 | 4012 | 4013 | 4014)
    }

    /// Codes after which the session can't resume.
    fn ends_session(&self) -> bool {
        self.is_final() || matches!(self.code, 4007 | 4009)
    }
}

impl std::fmt::Display for Closed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Discord closed the connection ({} {})", self.code, self.reason)
    }
}

impl std::error::Error for Closed {}

/// Discord ended the session; the next connection must identify again.
#[derive(Debug)]
pub struct Invalidated;

impl std::fmt::Display for Invalidated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Discord ended the session")
    }
}

impl std::error::Error for Invalidated {}

/// Too many IDENTIFYs within the hour; Godwit stopped reconnecting.
#[derive(Debug)]
pub struct TooManyIdentifies;

impl std::fmt::Display for TooManyIdentifies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Godwit started too many Discord sessions within the hour, so it stopped. Restart Godwit later.")
    }
}

impl std::error::Error for TooManyIdentifies {}

/// When to connect again after a session ends.
pub struct Reconnects {
    retry: Duration,
    identifies: VecDeque<Instant>,
}

impl Default for Reconnects {
    fn default() -> Self {
        Self {
            retry: FIRST_RETRY,
            identifies: VecDeque::new(),
        }
    }
}

impl Reconnects {
    fn identify(&mut self, now: Instant) -> Result<()> {
        while self
            .identifies
            .front()
            .is_some_and(|t| now.duration_since(*t) >= IDENTIFY_WINDOW)
        {
            self.identifies.pop_front();
        }
        if self.identifies.len() >= IDENTIFY_LIMIT {
            return Err(TooManyIdentifies.into());
        }
        self.identifies.push_back(now);
        Ok(())
    }

    /// How long to wait before connecting again, after a session that was up
    /// for `up` (or never connected) ended with `error`. Doubles until a
    /// session stays up, spread so clients don't return together.
    pub fn wait(&mut self, error: &anyhow::Error, up: Option<Duration>) -> Duration {
        if up.is_some_and(|up| up >= STABLE) {
            self.retry = FIRST_RETRY;
        }
        let mut wait = self.retry.mul_f64(rand::random_range(0.5..1.0));
        if error.downcast_ref::<Invalidated>().is_some() {
            // Discord asks for a random one to five seconds after ending a session.
            wait = wait.max(Duration::from_millis(rand::random_range(1_000..5_000)));
        }
        self.retry = (self.retry * 2).min(LAST_RETRY);
        wait
    }
}

/// A dispatched event: its name and data.
pub struct Dispatch {
    pub event: String,
    pub data: Value,
}

enum Command {
    Send(u64, Value),
    Close,
}

/// State changes supersede queued older values; don't drain a burst of
/// obsolete channel subscriptions, presence choices or mute toggles later.
#[derive(Default)]
struct Pending(VecDeque<(u64, Value)>);

impl Pending {
    fn push(&mut self, op: u64, data: Value) {
        for (old_op, old) in &mut self.0 {
            if matches!(op, STREAM_WATCH | STREAM_DELETE)
                && matches!(*old_op, STREAM_WATCH | STREAM_DELETE)
                && old["stream_key"] == data["stream_key"]
            {
                *old_op = op;
                *old = data;
                return;
            }
            if *old_op != op {
                continue;
            }
            if op == GUILD_SUBSCRIPTIONS_BULK
                && let (Some(target), Some(new)) =
                    (old["subscriptions"].as_object_mut(), data["subscriptions"].as_object())
            {
                target.extend(new.clone());
                return;
            }
            let same = match op {
                PRESENCE_UPDATE => true,
                VOICE_STATE_UPDATE => old["guild_id"] == data["guild_id"],
                STREAM_WATCH | STREAM_DELETE => old["stream_key"] == data["stream_key"],
                _ => old == &data,
            };
            if same {
                *old = data;
                return;
            }
        }
        // Joining/leaving voice takes priority over optional subscriptions.
        if op == VOICE_STATE_UPDATE {
            // Preserve leave-old-guild then join-new-guild ordering.
            let at = self
                .0
                .iter()
                .position(|(op, _)| *op != VOICE_STATE_UPDATE)
                .unwrap_or(self.0.len());
            self.0.insert(at, (op, data));
        } else {
            self.0.push_back((op, data));
        }
    }
}

#[derive(Default)]
struct SendBudget {
    sent: VecDeque<Instant>,
    presence: Option<Instant>,
}

impl SendBudget {
    fn due(&mut self, now: Instant, op: Option<u64>) -> Instant {
        while self.sent.front().is_some_and(|t| now.duration_since(*t) >= SEND_WINDOW) {
            self.sent.pop_front();
        }
        let window = match self.sent.front() {
            Some(oldest) if self.sent.len() >= SEND_LIMIT => *oldest + SEND_WINDOW,
            _ => now,
        };
        let paced = window.max(self.sent.back().map_or(now, |last| *last + SEND_SPACING));
        if op == Some(PRESENCE_UPDATE) {
            // Discord allows 5 presence updates / 20 s; keep a wider margin.
            paced.max(self.presence.map_or(now, |last| last + Duration::from_secs(10)))
        } else {
            paced
        }
    }

    fn record(&mut self, op: u64, now: Instant) {
        if op != HEARTBEAT && op != QOS_HEARTBEAT {
            self.sent.push_back(now);
        }
        if op == PRESENCE_UPDATE {
            self.presence = Some(now);
        }
    }
}

/// The gateway session. Its connection runs in a task of its own, keeping
/// up heartbeats and resuming a lost connection, while commands wait their
/// turn under the send limit.
pub struct Gateway {
    commands: mpsc::UnboundedSender<Command>,
    events: mpsc::UnboundedReceiver<Result<Dispatch>>,
    task: JoinHandle<()>,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.task.abort();
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn open(url: &str) -> Result<Socket> {
    let mut request = url.into_client_request()?;
    let headers = request.headers_mut();
    if let Ok(agent) = HeaderValue::from_str(&client::user_agent()) {
        headers.insert(header::USER_AGENT, agent);
    }
    headers.insert(header::ORIGIN, HeaderValue::from_static("https://discord.com"));
    let (socket, _) = timeout(IO_TIMEOUT, connect_async(request))
        .await
        .context("Discord's gateway didn't answer")?
        .context("could not reach Discord's gateway")?;
    Ok(socket)
}

impl Gateway {
    /// Connects and identifies with `presence`, returning the gateway and
    /// READY's data.
    pub async fn connect(token: &str, presence: Value, reconnects: &mut Reconnects) -> Result<(Self, Value)> {
        let mut connection = Connection {
            socket: open(&format!("{URL}{QUERY}")).await?,
            inflater: Inflater::default(),
            sequence: None,
            interval: Duration::from_secs(40),
            next_heartbeat: Instant::now(),
            acked: true,
            token: token.to_owned(),
            session_id: None,
            resume_url: None,
            budget: SendBudget::default(),
            resumes: 0,
            time_spent: None,
        };
        connection.greeted().await?;
        reconnects.identify(Instant::now())?;
        let identify = json!({
            "token": token,
            "capabilities": CAPABILITIES,
            "properties": client::properties(),
            "presence": presence,
            "compress": false,
            "client_state": {"guild_versions": {}, "api_code_version": 0},
        });
        connection.write(IDENTIFY, &identify).await?;
        let (commands, received) = mpsc::unbounded_channel();
        let (sent, events) = mpsc::unbounded_channel();
        let task = tokio::spawn(connection.run(received, sent));
        let mut gateway = Self { commands, events, task };
        loop {
            let event = gateway.next().await?;
            if event.event == "READY" {
                return Ok((gateway, event.data));
            }
        }
    }

    /// The next event, or the error that ended the session.
    pub async fn next(&mut self) -> Result<Dispatch> {
        self.events
            .recv()
            .await
            .unwrap_or_else(|| Err(anyhow!("the Discord connection stopped")))
    }

    /// Queues a command; it goes when the send limit allows.
    pub fn send(&self, op: u64, data: Value) -> Result<()> {
        self.commands
            .send(Command::Send(op, data))
            .map_err(|_| anyhow!("the Discord connection stopped"))
    }

    /// Closes the connection, ending the session (signing out).
    pub fn close(&self) {
        let _ = self.commands.send(Command::Close);
    }
}

struct Connection {
    socket: Socket,
    inflater: Inflater,
    sequence: Option<u64>,
    interval: Duration,
    next_heartbeat: Instant,
    acked: bool,
    token: String,
    session_id: Option<String>,
    resume_url: Option<String>,
    /// When the commands of the last minute went.
    budget: SendBudget,
    resumes: u32,
    /// The time-spent session last sent on this socket.
    time_spent: Option<String>,
}

impl Connection {
    async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<Command>,
        events: mpsc::UnboundedSender<Result<Dispatch>>,
    ) {
        let mut held = Pending::default();
        loop {
            let started = Instant::now();
            let lost = match self.serve(&mut commands, &events, &mut held).await {
                Ok(()) => return,
                Err(e) => e,
            };
            // A RESUMED frame alone doesn't prove a stable connection.
            // Resetting there could cause endless rapid reconnect cycles.
            if started.elapsed() >= STABLE {
                self.resumes = 0;
            }
            if let Err(e) = self.resume(lost).await {
                let _ = events.send(Err(e));
                return;
            }
        }
    }

    async fn serve(
        &mut self,
        commands: &mut mpsc::UnboundedReceiver<Command>,
        events: &mpsc::UnboundedSender<Result<Dispatch>>,
        held: &mut Pending,
    ) -> Result<()> {
        loop {
            let free_at = self.budget.due(Instant::now(), held.0.front().map(|(op, _)| *op));
            tokio::select! {
                () = tokio::time::sleep_until(self.next_heartbeat) => {
                    if !self.acked {
                        bail!("Discord's gateway stopped answering heartbeats");
                    }
                    self.write(QOS_HEARTBEAT, &json!({"seq": self.sequence, "qos": client::qos()})).await?;
                    self.acked = false;
                    let session = client::heartbeat_session().0;
                    if self.time_spent.as_ref() != Some(&session) {
                        self.write(TIME_SPENT, &client::time_spent_session()).await?;
                        self.time_spent = Some(session);
                    }
                    self.next_heartbeat = Instant::now() + self.interval;
                }
                () = tokio::time::sleep_until(free_at), if !held.0.is_empty() => {
                    if let Some((op, data)) = held.0.pop_front() {
                        self.write(op, &data).await?;
                    }
                }
                command = commands.recv() => match command {
                    Some(Command::Send(op, data)) => held.push(op, data),
                    Some(Command::Close) | None => {
                        let _ = timeout(IO_TIMEOUT, self.socket.close(None)).await;
                        return Ok(());
                    }
                },
                message = self.receive() => {
                    if let Some(dispatch) = self.handle(message?).await? {
                        let _ = events.send(Ok(dispatch));
                    }
                }
            }
        }
    }

    async fn handle(&mut self, mut message: Value) -> Result<Option<Dispatch>> {
        if let Some(sequence) = message["s"].as_u64() {
            self.sequence = Some(sequence);
        }
        match message["op"].as_u64() {
            Some(DISPATCH) => {
                let event = message["t"].as_str().unwrap_or_default().to_owned();
                let data = message.get_mut("d").map(Value::take).unwrap_or(Value::Null);
                if event == "READY" {
                    if let Some(token) = data["auth_token"].as_str().filter(|t| !t.is_empty()) {
                        self.token = token.to_owned();
                    }
                    self.session_id = data["session_id"].as_str().map(str::to_owned);
                    self.resume_url = data["resume_gateway_url"]
                        .as_str()
                        .filter(|u| u.starts_with("wss://"))
                        .map(str::to_owned);
                }
                return Ok(Some(Dispatch { event, data }));
            }
            Some(HEARTBEAT) => {
                self.write(QOS_HEARTBEAT, &json!({"seq": self.sequence, "qos": client::qos()}))
                    .await?;
            }
            Some(HEARTBEAT_ACK) => self.acked = true,
            Some(RECONNECT) => bail!("Discord asked to reconnect"),
            Some(INVALID_SESSION) if message["d"] == true => bail!("Discord asked to resume"),
            Some(INVALID_SESSION) => {
                self.session_id = None;
                return Err(Invalidated.into());
            }
            _ => {}
        }
        Ok(None)
    }

    async fn write(&mut self, op: u64, data: &Value) -> Result<()> {
        self.budget.record(op, Instant::now());
        let payload = json!({"op": op, "d": data}).to_string();
        timeout(IO_TIMEOUT, self.socket.send(Message::text(payload)))
            .await
            .context("Discord's gateway stopped taking commands")?
            .context("could not send to Discord's gateway")
    }

    /// Reads HELLO and sets the heartbeat from it.
    async fn greeted(&mut self) -> Result<()> {
        let hello = timeout(IO_TIMEOUT, self.receive())
            .await
            .context("Discord's gateway didn't greet")??;
        if hello["op"] != HELLO {
            bail!("Discord's gateway didn't greet as expected");
        }
        let ms = hello["d"]["heartbeat_interval"]
            .as_u64()
            .context("no heartbeat interval")?;
        self.interval =
            crate::limits::sent_seconds(ms as f64 / 1000.0, Duration::from_secs(1), Duration::from_secs(300));
        // Discord asks for the first heartbeat after a random part of the interval.
        self.next_heartbeat = Instant::now() + self.interval.mul_f64(rand::random::<f64>());
        self.acked = true;
        Ok(())
    }

    /// Connects again and resumes after the connection was `lost`; Discord
    /// replays what was missed, ending with RESUMED.
    async fn resume(&mut self, lost: anyhow::Error) -> Result<()> {
        let resumable = lost.downcast_ref::<Invalidated>().is_none()
            && !lost.downcast_ref::<Closed>().is_some_and(Closed::ends_session);
        let (Some(session_id), Some(resume_url), true) = (self.session_id.clone(), self.resume_url.clone(), resumable)
        else {
            return Err(lost);
        };
        let url = format!("{}{QUERY}", resume_url.trim_end_matches('/'));
        loop {
            if self.resumes >= RESUME_TRIES {
                return Err(lost.context("could not resume the session"));
            }
            let wait = RESUME_WAIT * 2u32.pow(self.resumes) + rand::random_range(Duration::ZERO..RESUME_WAIT);
            tokio::time::sleep(wait).await;
            self.resumes += 1;
            if self.reconnect(&url, &session_id).await.is_ok() {
                return Ok(());
            }
        }
    }

    async fn reconnect(&mut self, url: &str, session_id: &str) -> Result<()> {
        // Dropping the old connection without a close frame keeps the
        // session; a normal close would end it.
        self.socket = open(url).await?;
        self.inflater = Inflater::default();
        self.time_spent = None;
        // Resuming must not reset the local send budget and release a burst.
        self.greeted().await?;
        let resume = json!({"token": self.token, "session_id": session_id, "seq": self.sequence});
        self.write(RESUME, &resume).await
    }

    async fn receive(&mut self) -> Result<Value> {
        loop {
            let message = self
                .socket
                .next()
                .await
                .ok_or_else(|| anyhow!("Discord closed the gateway"))?
                .context("lost the connection to Discord's gateway")?;
            match message {
                Message::Text(text) => {
                    return serde_json::from_str(&text).context("could not understand Discord's gateway");
                }
                Message::Binary(data) => {
                    if let Some(payload) = self.inflater.push(&data)? {
                        return Ok(payload);
                    }
                }
                Message::Close(frame) => {
                    let (code, reason) = frame
                        .map(|f| (f.code, f.reason.to_string()))
                        .unwrap_or((CloseCode::Status, String::new()));
                    return Err(Closed {
                        code: u16::from(code),
                        reason,
                    }
                    .into());
                }
                _ => {}
            }
        }
    }
}

/// The gateway's zlib-stream compression: one zlib stream through the whole
/// connection, each payload ending with a sync flush, so a payload may come
/// in more than one message.
struct Inflater {
    zlib: flate2::Decompress,
    partial: Vec<u8>,
    inflated: Vec<u8>,
}

/// zlib's sync flush marker, which ends each payload.
const FLUSHED: [u8; 4] = [0x00, 0x00, 0xff, 0xff];
/// The most one payload may inflate to (READY is the largest).
const MAX_PAYLOAD: usize = 256 << 20;
/// Bigger buffers are freed after the payload that needed them.
const KEPT_BUFFER: usize = 256 << 10;

impl Default for Inflater {
    fn default() -> Self {
        Self {
            zlib: flate2::Decompress::new(true),
            partial: Vec::new(),
            inflated: Vec::new(),
        }
    }
}

impl Inflater {
    fn push(&mut self, data: &[u8]) -> Result<Option<Value>> {
        if self.partial.len() + data.len() > MAX_PAYLOAD {
            bail!("Discord's gateway sent too much at once");
        }
        self.partial.extend_from_slice(data);
        if !self.partial.ends_with(&FLUSHED) {
            return Ok(None);
        }
        let inflated = inflate(&mut self.zlib, &self.partial, &mut self.inflated);
        let payload = inflated
            .and_then(|()| serde_json::from_slice(&self.inflated).context("could not understand Discord's gateway"));
        self.partial.clear();
        self.inflated.clear();
        for buffer in [&mut self.partial, &mut self.inflated] {
            if buffer.capacity() > KEPT_BUFFER {
                *buffer = Vec::new();
            }
        }
        payload.map(Some)
    }
}

fn inflate(zlib: &mut flate2::Decompress, mut compressed: &[u8], out: &mut Vec<u8>) -> Result<()> {
    loop {
        if out.len() == out.capacity() {
            if out.len() >= MAX_PAYLOAD {
                bail!("Discord's gateway sent too much at once");
            }
            out.reserve((compressed.len() * 4).clamp(16 << 10, MAX_PAYLOAD - out.len()));
        }
        let (read, written) = (zlib.total_in(), out.len());
        zlib.decompress_vec(compressed, out, flate2::FlushDecompress::Sync)
            .context("could not inflate what Discord's gateway sent")?;
        let used = usize::try_from(zlib.total_in() - read).unwrap_or(compressed.len());
        compressed = compressed.get(used..).unwrap_or_default();
        let room = out.len() < out.capacity();
        if compressed.is_empty() && room {
            return Ok(());
        }
        if used == 0 && out.len() == written && room {
            bail!("Discord's gateway sent a compressed stream that stopped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_budget_spaces_bursts_and_preserves_presence_headroom() {
        let now = Instant::now();
        let mut budget = SendBudget::default();
        assert_eq!(budget.due(now, Some(VOICE_STATE_UPDATE)), now);
        budget.record(PRESENCE_UPDATE, now);
        assert_eq!(budget.due(now, Some(VOICE_STATE_UPDATE)), now + SEND_SPACING);
        assert_eq!(budget.due(now, Some(PRESENCE_UPDATE)), now + Duration::from_secs(10));
        for _ in 1..SEND_LIMIT {
            budget.record(VOICE_STATE_UPDATE, now);
        }
        assert_eq!(budget.due(now, Some(VOICE_STATE_UPDATE)), now + SEND_WINDOW);
        budget.record(HEARTBEAT, now);
        assert_eq!(budget.sent.len(), SEND_LIMIT, "heartbeats use reserved headroom");
        assert_eq!(
            budget.due(now + SEND_WINDOW, Some(VOICE_STATE_UPDATE)),
            now + SEND_WINDOW
        );
    }

    #[test]
    fn pending_gateway_state_changes_replace_obsolete_requests() {
        let mut pending = Pending::default();
        for i in 0..100 {
            pending.push(
                GUILD_SUBSCRIPTIONS_BULK,
                json!({"subscriptions":{"10":{"typing":true,"channels":{i.to_string():[[0,99]]}}}}),
            );
            pending.push(PRESENCE_UPDATE, json!({"status":i.to_string()}));
            pending.push(
                VOICE_STATE_UPDATE,
                json!({"guild_id":"10","channel_id":"15","self_mute":i % 2 == 0}),
            );
        }
        pending.push(
            GUILD_SUBSCRIPTIONS_BULK,
            json!({"subscriptions":{"20":{"typing":true}}}),
        );
        assert_eq!(pending.0.len(), 3, "100 changes become three latest states");
        assert_eq!(pending.0.front().unwrap().0, VOICE_STATE_UPDATE);
        assert_eq!(pending.0.front().unwrap().1["self_mute"], false);
        let subscriptions = pending
            .0
            .iter()
            .find(|(op, _)| *op == GUILD_SUBSCRIPTIONS_BULK)
            .unwrap();
        assert_eq!(
            subscriptions.1["subscriptions"]["10"]["channels"],
            json!({"99":[[0,99]]})
        );
        assert_eq!(subscriptions.1["subscriptions"]["20"]["typing"], true);
        pending.push(VOICE_STATE_UPDATE, json!({"guild_id":"10","channel_id":null}));
        assert!(
            pending.0.front().unwrap().1["channel_id"].is_null(),
            "leave supersedes queued join/mute"
        );
        pending.push(VOICE_STATE_UPDATE, json!({"guild_id":"20","channel_id":"25"}));
        assert_eq!(pending.0[0].1["guild_id"], "10");
        assert_eq!(pending.0[1].1["guild_id"], "20", "leave old before joining new");
        let mut pending = Pending::default();
        for op in [STREAM_WATCH, STREAM_DELETE, STREAM_WATCH] {
            pending.push(op, json!({"stream_key":"guild:10:15:100"}));
        }
        assert_eq!(pending.0.len(), 1);
        assert_eq!(pending.0[0].0, STREAM_WATCH, "only the final stream choice goes");
    }
    use flate2::{Compress, Compression, FlushCompress};

    /// Compresses payloads as one zlib stream with a sync flush after each,
    /// as Discord's gateway does.
    fn stream(payloads: &[&str]) -> Vec<Vec<u8>> {
        let mut z = Compress::new(Compression::default(), true);
        payloads
            .iter()
            .map(|p| {
                let mut out = Vec::with_capacity(p.len() + 64);
                z.compress_vec(p.as_bytes(), &mut out, FlushCompress::Sync).unwrap();
                out
            })
            .collect()
    }

    #[test]
    fn inflates_one_stream_across_payloads_and_split_messages() {
        let parts = stream(&[
            r#"{"op":10,"d":{"heartbeat_interval":41250}}"#,
            r#"{"op":0,"t":"READY","s":1,"d":{}}"#,
        ]);
        let mut inflater = Inflater::default();
        let hello = inflater.push(&parts[0]).unwrap().unwrap();
        assert_eq!(hello["d"]["heartbeat_interval"], 41250);
        let (a, b) = parts[1].split_at(parts[1].len() / 2);
        assert!(inflater.push(a).unwrap().is_none(), "half a payload waits");
        assert_eq!(inflater.push(b).unwrap().unwrap()["t"], "READY");
    }

    #[test]
    fn identifies_at_most_ten_times_an_hour() {
        let mut r = Reconnects::default();
        let now = Instant::now();
        for _ in 0..IDENTIFY_LIMIT {
            r.identify(now).unwrap();
        }
        assert!(r.identify(now).is_err());
        assert!(r.identify(now + IDENTIFY_WINDOW).is_ok(), "the window moves on");
    }

    #[test]
    fn waits_grow_and_reset_after_a_stable_session() {
        let mut r = Reconnects::default();
        let e = anyhow!("lost");
        let first = r.wait(&e, None);
        assert!(first <= FIRST_RETRY);
        for _ in 0..10 {
            r.wait(&e, None);
        }
        assert!(r.wait(&e, None) <= LAST_RETRY);
        r.wait(&e, Some(STABLE));
        assert_eq!(r.retry, FIRST_RETRY * 2, "a stable session starts the doubling over");
        assert!(r.wait(&Invalidated.into(), None) >= Duration::from_secs(1));
        assert!(
            Closed {
                code: 4004,
                reason: String::new()
            }
            .is_final()
        );
        assert!(
            Closed {
                code: 4009,
                reason: String::new()
            }
            .ends_session()
        );
    }
}
