// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Watching someone's Go Live stream. A stream is named by its key, and
//! Discord serves it from a voice server of its own: watching sends main
//! gateway op 20 (Stream Watch), and Discord answers STREAM_CREATE, naming
//! the stream's RTC server, and STREAM_SERVER_UPDATE with the endpoint and
//! token to connect with; op 19 (Stream Delete) stops watching. The stream
//! connection reuses the voice session id of the call it belongs to, and
//! runs on a thread of its own: the screen's video through `video::Feeds`,
//! its sound handed to the call to mix.
//!
//! After Yapfast's `stream.rs` and `stream_connection.rs` (GPL-3.0-or-later).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as channel;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::time::Instant;

use super::connection::{Connection, Identity, Kind, op, receive, send_json};
use super::depacketize::RTCP_CLEAR;
use super::video::{FeedKind, Feeds};
use super::{note, rtp};

/// The key of `user`'s stream: in a server's voice channel, or with no
/// server, in a DM or group DM call.
pub fn key(guild: Option<&str>, channel: &str, user: &str) -> String {
    match guild {
        Some(guild) => format!("guild:{guild}:{channel}:{user}"),
        None => format!("call:{channel}:{user}"),
    }
}

/// The server (none for a DM call), channel and streamer a key names.
pub fn parse(key: &str) -> Option<(Option<&str>, &str, &str)> {
    let parts: Vec<&str> = key.split(':').collect();
    match parts.as_slice() {
        ["guild", guild, channel, user] => Some((Some(guild), channel, user)),
        ["call", channel, user] => Some((None, channel, user)),
        _ => None,
    }
}

/// What a stream connection needs, as [`super::VoiceServer`] for a call.
#[derive(Clone, PartialEq, Eq)]
pub struct StreamServer {
    pub stream_key: String,
    pub user_id: String,
    /// The voice session of the call the stream is in.
    pub session_id: String,
    /// The server id the stream connection identifies with, in place of a
    /// call's server.
    pub rtc_server_id: String,
    pub token: String,
    pub endpoint: String,
}

impl std::fmt::Debug for StreamServer {
    // The token stays out of logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamServer")
            .field("stream_key", &self.stream_key)
            .field("rtc_server_id", &self.rtc_server_id)
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

/// A stream asked for, until Discord has said where it's served; then
/// where it is, which a later STREAM_CREATE or STREAM_SERVER_UPDATE moves.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Opening {
    rtc_server_id: Option<String>,
    server: Option<(String, String)>,
}

impl Opening {
    /// Takes in STREAM_CREATE, STREAM_UPDATE or STREAM_SERVER_UPDATE about this stream.
    pub fn apply(&mut self, event: &str, data: &Value) {
        match event {
            "STREAM_CREATE" | "STREAM_UPDATE" => {
                if let Some(rtc) = data["rtc_server_id"].as_str() {
                    self.rtc_server_id = Some(rtc.to_owned());
                }
            }
            // The endpoint is null while Discord is still allocating a server.
            "STREAM_SERVER_UPDATE" => {
                if let (Some(token), Some(endpoint)) = (data["token"].as_str(), data["endpoint"].as_str()) {
                    self.server = Some((token.to_owned(), endpoint.to_owned()));
                }
            }
            _ => {}
        }
    }

    /// The finished details, once both answers are in.
    pub fn server(&self, key: &str, user_id: &str, session_id: &str) -> Option<StreamServer> {
        let (token, endpoint) = self.server.clone()?;
        Some(StreamServer {
            stream_key: key.to_owned(),
            user_id: user_id.to_owned(),
            session_id: session_id.to_owned(),
            rtc_server_id: self.rtc_server_id.clone()?,
            token,
            endpoint,
        })
    }
}

/// Why a stream STREAM_DELETE ended, as the official client words them.
pub fn ended_because(data: &Value) -> Option<&'static str> {
    match data["reason"].as_str() {
        Some("stream_full") => Some("The stream is full."),
        Some("unauthorized") => Some("You aren't allowed to watch this stream."),
        Some("user_requested") | None => None,
        Some(_) => Some("The stream ended."),
    }
}

/// What the stream's chip says when watching stopped on our side: short,
/// and the same for every connection trouble (the details go to the log).
pub fn shown_reason(reason: &str) -> &'static str {
    if reason.starts_with(NO_VIDEO) {
        NO_VIDEO
    } else {
        "The stream stopped. Watch to try again."
    }
}

/// No video at all this long after connecting: give up and say so, rather
/// than show a black tile for good.
const FIRST_VIDEO: Duration = Duration::from_secs(20);
const NO_VIDEO: &str = "No video arrived. Watch to try again.";

/// How often a receiver report goes, and lost packets are given up on or
/// asked for again even while nothing arrives.
const REPORTS: Duration = Duration::from_secs(1);
const REPAIRS: Duration = Duration::from_millis(20);
/// Sound waiting for the call to mix it: 320 ms of Opus frames.
const SOUND_QUEUED: usize = 16;

/// One Opus frame of a stream's sound, with its RTP sequence number.
pub type Sound = (u16, Vec<u8>);

/// A stream being watched, on a thread of its own; dropping it stops it.
pub struct Watching {
    pub streamer: u64,
    /// The stream's sound, decrypted Opus, for the call to mix.
    pub sound: channel::Receiver<Sound>,
    /// Why it ended (None: stopped), once it has.
    pub ended: channel::Receiver<Option<String>>,
    _stop: oneshot::Sender<()>,
}

impl Watching {
    /// Connects to the stream's server and receives the streamer's screen
    /// and sound. `visible` says whether the call is on screen; frames
    /// shown wake the UI through `shown`.
    pub fn start(server: StreamServer, visible: Arc<AtomicBool>, shown: Arc<dyn Fn() + Send + Sync>) -> Result<Self> {
        let identity = identity(&server)?;
        let streamer = parse(&server.stream_key)
            .and_then(|(_, _, user)| user.parse::<u64>().ok())
            .context("not a stream key")?;
        let (sounds, sound) = channel::sync_channel(SOUND_QUEUED);
        let (end, ended) = channel::sync_channel(1);
        let (stop, stopped) = oneshot::channel();
        std::thread::Builder::new()
            .name("discord-stream".into())
            .spawn(move || {
                let reason = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime
                        .block_on(run(&identity, streamer, &visible, shown, &sounds, stopped))
                        .err()
                        .map(|e| format!("{e:#}")),
                    Err(e) => Some(format!("couldn't start watching: {e}")),
                };
                if let Some(reason) = &reason {
                    note(&format!("the stream ended: {reason}"));
                }
                let _ = end.try_send(reason);
            })
            .context("couldn't start watching the stream")?;
        Ok(Self {
            streamer,
            sound,
            ended,
            _stop: stop,
        })
    }
}

/// Who the stream connection identifies as. Its DAVE group isn't its voice
/// channel's: Discord made it just before the stream's RTC server id in the
/// one stream Yapfast saw, so that's the guess, and DAVE moves to the group
/// the voice server's messages name if it's wrong.
fn identity(server: &StreamServer) -> Result<Identity> {
    identity_as(server, Kind::Stream)
}

/// A stream connection's identity: watching (`Kind::Stream`) or sending ours
/// (`Kind::GoLive`).
pub(crate) fn identity_as(server: &StreamServer, kind: Kind) -> Result<Identity> {
    let (_, channel, _) = parse(&server.stream_key).context("not a stream key")?;
    let channel: u64 = channel.parse().context("not a channel id")?;
    let group = server
        .rtc_server_id
        .parse::<u64>()
        .map_or(channel, |id| id.saturating_sub(1));
    Ok(Identity {
        endpoint: server.endpoint.clone(),
        server_id: server.rtc_server_id.clone(),
        user_id: server.user_id.clone(),
        session_id: server.session_id.clone(),
        token: server.token.clone(),
        user: server.user_id.parse().context("not a user id")?,
        group,
        kind,
    })
}

async fn run(
    identity: &Identity,
    streamer: u64,
    visible: &AtomicBool,
    shown: Arc<dyn Fn() + Send + Sync>,
    sounds: &channel::SyncSender<Sound>,
    mut stopped: oneshot::Receiver<()>,
) -> Result<()> {
    let mut connection = Connection::open(identity, &[streamer]).await?;
    let mut feeds = Feeds::new(FeedKind::Screen, 4, shown);
    feeds.set_visible(visible.load(Ordering::Relaxed));
    for (op, data) in std::mem::take(&mut connection.early) {
        feeds.learn(op, &data);
    }
    // Op 15 as Yapfast's viewer sends it, once: `any: 100` takes the
    // stream as its sender sends it. A viewer can't ask for more than that
    // (the streamer's own plan sets resolution and frame rate), and asks
    // nothing extra. Video starts with the keyframe `Feeds::repair` asks for.
    send_json(&mut connection.ws, op::MEDIA_SINK_WANTS, json!({"any": 100})).await?;
    note("watching a stream");

    let interval = connection.interval;
    let mut heartbeat = tokio::time::interval_at(Instant::now() + interval, interval);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut repairs = tokio::time::interval(REPAIRS);
    let mut reports = tokio::time::interval_at(Instant::now() + REPORTS, REPORTS);
    let mut datagram = vec![0u8; 2048];
    let mut unreadable = false;
    // What the UDP socket hears in the first half minute, for the log:
    // datagrams, RTCP, ones that didn't open, sound, and video.
    let mut heard = [0u32; 5];
    let mut tallies = 0u32;
    let first_video = tokio::time::sleep(FIRST_VIDEO);
    tokio::pin!(first_video);
    loop {
        tokio::select! {
            _ = &mut stopped => {
                let _ = connection.ws.close(None).await;
                return Ok(());
            }
            _ = heartbeat.tick() => connection.heartbeat().await?,
            message = receive(&mut connection.ws, &mut connection.seq) => {
                if let Some((op, data)) = connection.handle(message?).await? {
                    feeds.learn(op, &data);
                }
            }
            () = &mut first_video, if heard[4] == 0 => {
                let _ = connection.ws.close(None).await;
                anyhow::bail!("{NO_VIDEO} ({} datagrams, {} sound)", heard[0], heard[3]);
            }
            _ = repairs.tick() => {
                feeds.set_visible(visible.load(Ordering::Relaxed));
            }
            _ = reports.tick() => {
                tallies += 1;
                if tallies <= 30 && tallies.is_multiple_of(5) {
                    note(&format!(
                        "stream udp tally: {} datagrams, {} rtcp, {} unopened, {} sound, {} video",
                        heard[0], heard[1], heard[2], heard[3], heard[4]
                    ));
                }
                feeds.report(&mut connection).await?;
            }
            length = connection.udp.recv(&mut datagram) => {
                let datagram = &datagram[..length?];
                heard[0] += 1;
                if rtp::is_rtcp(datagram) {
                    heard[1] += 1;
                    if let Some(rtcp) = connection.transport.open(datagram, RTCP_CLEAR) {
                        feeds.heard_rtcp(&rtcp);
                    }
                    continue;
                }
                let Some(clear) = rtp::rtp_clear_len(datagram) else { continue };
                let Some(plain) = connection.transport.open(datagram, clear) else {
                    heard[2] += 1;
                    continue;
                };
                let Some(packet) = rtp::parse(&plain) else { continue };
                if packet.payload_type == rtp::OPUS {
                    heard[3] += 1;
                    match connection.dave.decrypt_opus(streamer, packet.payload.to_vec()) {
                        // A full queue drops sound the call can't keep up with.
                        Some(opus) => drop(sounds.try_send((packet.sequence, opus))),
                        None if !unreadable => {
                            unreadable = true;
                            note(&format!(
                                "the stream's sound won't decrypt ({}; DAVE {})",
                                connection.dave.unreadable.as_deref().unwrap_or("?"),
                                connection.dave.status()
                            ));
                        }
                        None => {}
                    }
                    continue;
                }
                heard[4] += 1;
                let chosen = connection.video_codec;
                feeds.receive(&packet, chosen);
            }
        }
        feeds.repair(&mut connection).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_name_the_streamer() {
        let guild = key(Some("1"), "2", "3");
        assert_eq!(guild, "guild:1:2:3");
        assert_eq!(parse(&guild), Some((Some("1"), "2", "3")));
        assert_eq!(key(None, "2", "3"), "call:2:3");
        assert_eq!(parse("call:2:3"), Some((None, "2", "3")));
        assert_eq!(parse("guild:1:2"), None);
    }

    #[test]
    fn opens_once_both_answers_are_in() {
        let key = key(Some("1"), "2", "3");
        let mut opening = Opening::default();
        opening.apply(
            "STREAM_SERVER_UPDATE",
            &json!({"stream_key": key, "token": "t", "endpoint": null}),
        );
        opening.apply("STREAM_CREATE", &json!({"stream_key": key, "rtc_server_id": "77"}));
        assert_eq!(opening.server(&key, "9", "s"), None, "no endpoint yet");
        opening.apply(
            "STREAM_SERVER_UPDATE",
            &json!({"stream_key": key, "token": "t", "endpoint": "e:443"}),
        );
        let server = opening.server(&key, "9", "s").unwrap();
        assert_eq!(
            (
                server.rtc_server_id.as_str(),
                server.session_id.as_str(),
                server.endpoint.as_str()
            ),
            ("77", "s", "e:443")
        );
        assert!(!format!("{server:?}").contains("\"t\""), "no token in logs");
        let identity = identity(&server).unwrap();
        assert_eq!(
            (identity.server_id.as_str(), identity.group, identity.user),
            ("77", 76, 9)
        );
        assert_eq!(identity.kind, Kind::Stream);
    }

    #[test]
    fn says_why_a_stream_ended() {
        assert_eq!(
            ended_because(&json!({"reason": "stream_full"})),
            Some("The stream is full.")
        );
        assert_eq!(ended_because(&json!({"reason": "user_requested"})), None);
        assert_eq!(
            ended_because(&json!({"reason": "stream_ended"})),
            Some("The stream ended.")
        );
    }
}
