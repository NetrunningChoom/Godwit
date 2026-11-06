// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Our Go Live stream, sent: the screen captured (`godwit_voice::screen`),
//! encoded and sent over a voice connection of its own at the stream server
//! Discord named, on a `discord-golive` thread (after Yapfast's
//! `stream_worker.rs`). Discord's free quality only: 720p at 30 frames a
//! second, never more (no Nitro features, ever).
//!
//! As Yapfast, the screen is chosen first (the system's picker, where there
//! is one) and Discord hears of the stream only once a frame is encoded:
//! [`GoingLive::picked`] says so, and the account then asks Discord to go
//! live. A cancelled picker shares nothing. Discord's answer, the stream's
//! server, arrives through [`GoingLive::send_to`]; a later one (Discord
//! moved the stream) reconnects without asking for the screen again.

use std::sync::Arc;
use std::sync::mpsc as channel;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use godwit_voice::screen::{ScreenOptions, ScreenQuality};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, MissedTickBehavior};

use super::connection::{Connection, Kind, op, receive};
use super::depacketize::RTCP_CLEAR;
use super::outgoing::{self, Encoding, Profile, Sender};
use super::packetize::Content;
use super::stream::{StreamServer, identity_as};
use super::{note, rtp};

/// Discord's free Go Live: 720p, 30 frames a second.
const LIMIT: [u32; 2] = [1280, 720];
const FPS: u32 = 30;
/// Packets kept to resend: about two seconds at the 720p cap.
const HISTORY: usize = 1024;
/// How often the capture is checked for having ended on its own.
const CHECK: Duration = Duration::from_millis(250);

/// Our stream: capturing from the start, sending once Discord says where.
/// Dropping it stops it.
pub struct GoingLive {
    /// Where it's sent, so a repeated command doesn't reconnect.
    pub server: Option<StreamServer>,
    servers: mpsc::UnboundedSender<StreamServer>,
    /// Once, when the screen was chosen and its first frame encoded.
    pub picked: channel::Receiver<()>,
    /// Why it ended (None: stopped), once it has.
    pub ended: channel::Receiver<Option<String>>,
    _stop: oneshot::Sender<()>,
}

/// The capture, let go off this thread however the stream ends.
struct Capture(Option<godwit_voice::screen::Capture>);

impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(capture) = self.0.take() {
            outgoing::release(capture, godwit_voice::screen::Capture::request_stop);
        }
    }
}

impl GoingLive {
    /// Starts capturing the screen (the system's picker chooses what, where
    /// there is one) and encoding; nothing is sent until [`Self::send_to`].
    pub fn start(shown: Arc<dyn Fn() + Send + Sync>) -> Result<Self> {
        let (end, ended) = channel::sync_channel(1);
        let (picked_tx, picked) = channel::sync_channel(1);
        let (servers, servers_rx) = mpsc::unbounded_channel();
        let (stop, stopped) = oneshot::channel();
        std::thread::Builder::new()
            .name("discord-golive".into())
            .spawn(move || {
                let reason = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime
                        .block_on(run(servers_rx, picked_tx, stopped, shown))
                        .err()
                        .map(|e| format!("{e:#}")),
                    Err(e) => Some(format!("couldn't start streaming: {e}")),
                };
                if let Some(reason) = &reason {
                    note(&format!("our stream ended: {reason}"));
                }
                let _ = end.try_send(reason);
            })
            .context("couldn't start streaming")?;
        Ok(Self {
            server: None,
            servers,
            picked,
            ended,
            _stop: stop,
        })
    }

    /// Sends the stream to `server`: first where Discord put it, then
    /// wherever it moves it. The same server again changes nothing.
    pub fn send_to(&mut self, server: StreamServer) {
        if self.server.as_ref() == Some(&server) {
            return;
        }
        self.server = Some(server.clone());
        let _ = self.servers.send(server);
    }
}

/// Connects to the stream's server: identifying as our own stream.
async fn connect(server: &StreamServer) -> Result<Connection> {
    let identity = identity_as(server, Kind::GoLive)?;
    Connection::open(&identity, &[]).await
}

async fn run(
    mut servers: mpsc::UnboundedReceiver<StreamServer>,
    picked: channel::SyncSender<()>,
    mut stopped: oneshot::Receiver<()>,
    shown: Arc<dyn Fn() + Send + Sync>,
) -> Result<()> {
    let profile = Profile {
        content: Content::Screen,
        fps: FPS,
    };
    let (sink, encoding) = Encoding::start(profile)?;
    let (end, capture_ended) = channel::channel::<String>();
    let _capture = Capture(Some(godwit_voice::screen::start(
        sink,
        ScreenOptions::new(ScreenQuality::Low, LIMIT),
        move |why| {
            let _ = end.send(why);
        },
        shown,
    )?));
    let mut sender = Sender::new(encoding, profile, HISTORY);
    let mut check = tokio::time::interval(CHECK);

    // The screen chosen and a frame encoded: Discord may hear of it now.
    let mut told = false;
    let server = loop {
        tokio::select! {
            _ = &mut stopped => return Ok(()),
            _ = check.tick() => {
                if let Ok(why) = capture_ended.try_recv() {
                    bail!("{why}");
                }
            }
            frame = sender.encoding.frames.recv() => {
                if frame.is_none() {
                    bail!("the encoder stopped");
                }
                // Until there's somewhere to send them, frames are dropped;
                // the first sent is a keyframe (`bind`).
                if !told {
                    told = true;
                    note("the screen to share was chosen");
                    let _ = picked.try_send(());
                }
            }
            server = servers.recv() => match server {
                Some(server) => break server,
                None => return Ok(()),
            },
        }
    };

    let mut connection = connect(&server).await?;
    sender.bind(&connection);
    note("going live");
    let interval = connection.interval;
    let mut heartbeat = tokio::time::interval_at(Instant::now() + interval, interval);
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut datagram = vec![0u8; 2048];
    loop {
        tokio::select! {
            _ = &mut stopped => {
                let _ = outgoing::withdraw(&mut connection).await;
                let _ = connection.ws.close(None).await;
                return Ok(());
            }
            // Discord moved the stream: the same capture, sent there.
            server = servers.recv() => {
                let Some(server) = server else { return Ok(()) };
                let _ = connection.ws.close(None).await;
                connection = connect(&server).await?;
                sender.bind(&connection);
                heartbeat = tokio::time::interval_at(Instant::now() + connection.interval, connection.interval);
                heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
                note("our stream moved to another server");
            }
            _ = heartbeat.tick() => connection.heartbeat().await?,
            _ = check.tick() => {
                if let Ok(why) = capture_ended.try_recv() {
                    let _ = outgoing::withdraw(&mut connection).await;
                    bail!("{why}");
                }
            }
            message = receive(&mut connection.ws, &mut connection.seq) => {
                if let Some((op, _)) = connection.handle(message?).await?
                    && op == op::CLIENTS_CONNECT
                {
                    // A new viewer: announce again, as the server may not pass the last one on.
                    sender.announce(&mut connection).await?;
                }
            }
            length = connection.udp.recv(&mut datagram) => {
                let datagram = &datagram[..length?];
                if rtp::is_rtcp(datagram)
                    && let Some(rtcp) = connection.transport.open(datagram, RTCP_CLEAR)
                {
                    sender.feedback(&mut connection, &rtcp).await?;
                }
            }
            frame = sender.encoding.frames.recv() => {
                let Some(frame) = frame else { bail!("the encoder stopped") };
                sender.send(&mut connection, frame, true).await?;
            }
        }
    }
}
