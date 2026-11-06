// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! One Discord call's media: our microphone out as Opus every 20 ms while we
//! speak, everyone else's in, decoded per speaker and mixed. It runs on a
//! thread of its own with a small runtime (as Yapfast's calls do), so the
//! network thread's big gateway payloads never delay a frame.
//!
//! Ported from Yapfast's `call_connection.rs` (GPL-3.0-or-later): the voice
//! gate, silent frames, speaker jitter buffers, resume and reconnect rules.
//!
//! Others' cameras come in on the same connection (`video::Feeds`); Go Live
//! streams watched in the call run on threads of their own
//! (`stream::Watching`) and hand their sound here, to be mixed under the
//! screen share volume.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use godwit_voice::call::VoiceSettings;
use godwit_voice::opus;
use godwit_voice::raw::RawAudio;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior};

use super::connection::{Closed, Connection, Identity, Kind, op, receive, send_json};
use super::depacketize::RTCP_CLEAR;
use super::outgoing::{self, Encoding, Profile, Sender};
use super::packetize::Content;
use super::stream::Watching;
use super::video::{FeedKind, Feeds};
use super::{CallCmd, CallEvent, VoiceServer, note, rtp};

/// Samples per channel in one 20 ms frame at 48 kHz.
const FRAME: usize = 960;
/// Stereo at Discord's default voice channel bit rate (64 kbit/s); its
/// clients send stereo Opus, so the microphone goes out in both channels.
const CHANNELS: u16 = 2;
const BITRATE: i32 = 64_000;
/// 20 ms of Opus silence, which Discord's clients send five of when they
/// stop talking.
const SILENCE: [u8; 3] = [0xf8, 0xff, 0xfe];
const SILENT_FRAMES: u8 = 5;
/// Op 5's flag for a microphone.
const MICROPHONE: u8 = 1;
/// While the voice gate is shut, the newest 40 ms are kept, and sent first
/// when it opens, so the start of a word isn't cut off.
const PREROLL: usize = FRAME * 2;
/// How often UDP carries a keepalive, so a quiet connection stays open
/// through NAT, as songbird and Yapfast send one.
const KEEPALIVE: Duration = Duration::from_secs(5);
/// A speaker's audio starts playing once this many packets wait: 60 ms.
const START: usize = 3;
/// At most this many packets wait, 400 ms; beyond, the oldest go.
const MOST: usize = 20;
/// A speaker missing this many packets in a row has stopped: 100 ms.
const GONE: u8 = 5;
/// A speaker heard from no more in this long gives up their decoder.
const FORGET: Duration = Duration::from_secs(10);
/// A tick this late means the call stalled rather than woke late: its
/// schedule starts again from now instead of rushing out what it missed.
const STALLED: Duration = Duration::from_millis(100);
/// Gateway drops in a row that resuming or reconnecting may follow.
const RETRIES: u32 = 3;
/// How long a connection holds before its drop no longer counts with those before.
const STEADY: Duration = Duration::from_secs(60);
/// How much longer each resume after the first waits.
const RESUME_WAIT: Duration = Duration::from_secs(1);
/// How long joining waits for Discord to assign a voice server (Yapfast's
/// join timeout).
const JOIN_TIMEOUT: Duration = Duration::from_secs(15);
/// Discord ends the voice connection when it moves a call to another
/// server, perhaps before saying where: how long to wait to hear.
const MOVE_GRACE: Duration = Duration::from_secs(5);
/// Speaking is reported to the account at most this often.
const SPEAKING_EVERY: u32 = 5;
/// Ticks between receiver reports about the cameras: a second.
const REPORT_EVERY: u32 = 50;
/// The most cameras received at once: the call grid's nine, as Stoat calls
/// take at most.
const CAMERAS: usize = 9;

/// Where the call's events go; shared with the threads that decode video.
type Emit = Arc<dyn Fn(CallEvent) + Send + Sync>;

/// Our camera's next encoded frame, or never while it's off.
async fn camera_frame(camera: &mut Option<OwnCamera>) -> Option<outgoing::Encoded> {
    match camera {
        Some(camera) => camera.sender.encoding.frames.recv().await,
        None => std::future::pending().await,
    }
}

/// Starts a call on a thread of its own. Commands come in on `commands`;
/// what happens goes out through `emit`, ending with [`CallEvent::Ended`].
pub fn start(
    settings: VoiceSettings,
    muted: bool,
    deafened: bool,
    commands: mpsc::UnboundedReceiver<CallCmd>,
    emit: impl Fn(CallEvent) + Send + Sync + 'static,
) {
    let emit: Emit = Arc::new(emit);
    let spawned = std::thread::Builder::new().name("discord-call".into()).spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(e) => return emit(CallEvent::Ended(Some(format!("couldn't start the call: {e}")))),
        };
        let reason = runtime.block_on(run(settings, muted, deafened, commands, &emit));
        emit(CallEvent::Ended(reason));
    });
    if let Err(e) = spawned {
        note(&format!("couldn't start the call's thread: {e}"));
    }
}

/// Who identifies with the voice server for `server`.
fn identify_as(server: &VoiceServer) -> Option<Identity> {
    let id = |v: &str| v.parse::<u64>().ok().filter(|&id| id != 0);
    Some(Identity {
        endpoint: server.endpoint.clone(),
        server_id: server.server_id.clone(),
        user_id: server.user_id.clone(),
        session_id: server.session_id.clone(),
        token: server.token.clone(),
        user: id(&server.user_id)?,
        group: id(&server.channel_id)?,
        kind: Kind::Call,
    })
}

/// The call from opening the devices until it ends; the reason it ended,
/// if it wasn't us leaving.
async fn run(
    settings: VoiceSettings,
    muted: bool,
    deafened: bool,
    mut commands: mpsc::UnboundedReceiver<CallCmd>,
    emit: &Emit,
) -> Option<String> {
    let audio = match RawAudio::new(&settings) {
        Ok(audio) => audio,
        Err(e) => return Some(format!("couldn't open the microphone or speakers: {e:#}")),
    };
    let frames = emit.clone();
    let mut media = match Media::new(muted, deafened, Arc::new(move || frames(CallEvent::Frame))) {
        Ok(media) => media,
        Err(e) => return Some(format!("{e:#}")),
    };
    audio.set_deafened(deafened);

    // Discord's voice server for us, once the main gateway says it.
    let deadline = tokio::time::sleep(JOIN_TIMEOUT);
    tokio::pin!(deadline);
    let mut identity = loop {
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(CallCmd::Leave) => return None,
                Some(CallCmd::Server(server)) => match identify_as(&server) {
                    Some(identity) => break identity,
                    None => return Some("Discord sent a voice server Godwit couldn't use.".into()),
                },
                Some(other) => media.apply(other, &audio),
            },
            () = &mut deadline => return Some("Discord didn't assign a voice server. Try again.".into()),
        }
    };
    let mut connection = match connect(&identity, &mut commands, &mut media, &audio).await {
        Ok(Some(connection)) => connection,
        Ok(None) => return None,
        Err(e) => return Some(format!("couldn't connect to Discord's voice server: {e:#}")),
    };
    emit(CallEvent::Connected);

    let mut drops = 0;
    loop {
        let up = std::time::Instant::now();
        match media.run(&mut connection, &audio, &mut commands, emit).await {
            Ended::Left => {
                let _ = connection.ws.close(None).await;
                return None;
            }
            Ended::Moved(server) => {
                let _ = connection.ws.close(None).await;
                let Some(to) = identify_as(&server) else {
                    return Some("Discord sent a voice server Godwit couldn't use.".into());
                };
                identity = to;
                match connect(&identity, &mut commands, &mut media, &audio).await {
                    Ok(Some(moved)) => {
                        connection = moved;
                        media.reset();
                        drops = 0;
                    }
                    Ok(None) => return None,
                    Err(e) => return Some(format!("Discord moved the call and Godwit couldn't follow: {e:#}")),
                }
            }
            Ended::Dropped(error) => {
                // Drops count in a row: a connection that held a while starts
                // the count again, so a long call survives the odd blip.
                if up.elapsed() >= STEADY {
                    drops = 0;
                }
                drops += 1;
                let closed = error.downcast_ref::<Closed>();
                if closed.is_some_and(Closed::is_final) {
                    // A move ends the connection, perhaps before Discord
                    // says where to: wait a moment to hear.
                    match moved_meanwhile(&mut commands, &mut media, &audio).await {
                        Some(Some(server)) => {
                            let Some(to) = identify_as(&server) else {
                                return Some("Discord sent a voice server Godwit couldn't use.".into());
                            };
                            identity = to;
                            match connect(&identity, &mut commands, &mut media, &audio).await {
                                Ok(Some(moved)) => {
                                    connection = moved;
                                    media.reset();
                                    drops = 0;
                                    continue;
                                }
                                Ok(None) => return None,
                                Err(e) => return Some(format!("{e:#}")),
                            }
                        }
                        Some(None) => return None,
                        None => return Some(format!("{error}")),
                    }
                }
                if drops > RETRIES {
                    return Some(format!("the voice connection kept dropping ({error:#})"));
                }
                note(&format!("the voice connection dropped ({error:#}); resuming"));
                // The first try goes at once, so a blip passes unheard; the
                // rest wait longer each time.
                tokio::time::sleep(RESUME_WAIT * (drops - 1)).await;
                if let Err(resume) = connection.resume(&identity).await {
                    note(&format!("couldn't resume ({resume:#}); connecting again"));
                    match Connection::open(&identity, &[]).await {
                        Ok(again) => {
                            connection = again;
                            media.reset();
                        }
                        Err(again) => return Some(format!("{error:#}; then {again:#}")),
                    }
                }
            }
        }
    }
}

/// Connects, still taking commands meanwhile; None if we left.
async fn connect(
    identity: &Identity,
    commands: &mut mpsc::UnboundedReceiver<CallCmd>,
    media: &mut Media,
    audio: &RawAudio,
) -> Result<Option<Connection>> {
    let opening = Connection::open(identity, &[]);
    tokio::pin!(opening);
    loop {
        tokio::select! {
            connection = &mut opening => return connection.map(Some),
            command = commands.recv() => match command {
                None | Some(CallCmd::Leave) => return Ok(None),
                // A newer server while connecting: Discord moved us already.
                Some(CallCmd::Server(server)) => {
                    let Some(newer) = identify_as(&server) else { continue };
                    return Box::pin(connect(&newer, commands, media, audio)).await;
                }
                Some(other) => media.apply(other, audio),
            },
        }
    }
}

/// After a final close: a new voice server within the grace period
/// (`Some(Some)`), us leaving (`Some(None)`), or nothing (`None`).
async fn moved_meanwhile(
    commands: &mut mpsc::UnboundedReceiver<CallCmd>,
    media: &mut Media,
    audio: &RawAudio,
) -> Option<Option<VoiceServer>> {
    let grace = tokio::time::sleep(MOVE_GRACE);
    tokio::pin!(grace);
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(CallCmd::Leave) => return Some(None),
                Some(CallCmd::Server(server)) => return Some(Some(server)),
                Some(other) => media.apply(other, audio),
            },
            () = &mut grace => return None,
        }
    }
}

/// Why the media loop stopped.
enum Ended {
    Left,
    Moved(VoiceServer),
    Dropped(anyhow::Error),
}

/// The call's audio state, kept across moves and reconnections.
struct Media {
    encoder: opus::Encoder,
    /// One frame of the microphone, then in both channels, and its packet.
    mono: Vec<f32>,
    stereo: Vec<f32>,
    packet: Vec<u8>,
    sequence: u16,
    /// Goes on 20 ms every tick, sending or not, as a microphone's clock
    /// does: WebRTC's jitter buffer in Discord's clients takes a pause the
    /// timestamps don't show as lateness and raises its delay.
    timestamp: u32,
    /// Whether op 5 says we're speaking, and the silent frames still to
    /// send since we stopped.
    speaking: bool,
    silent: u8,
    muted: bool,
    deafened: bool,
    /// Everyone else, by their audio SSRC, and whose each SSRC is.
    speakers: HashMap<u32, Speaker>,
    users: HashMap<u32, u64>,
    /// Ticks until speaking is reported again, and what was last reported.
    report_in: u32,
    reported: Option<(bool, Vec<String>)>,
    /// Whether the microphone last went out end-to-end encrypted, as logged.
    encrypting: Option<bool>,
    /// Everyone else's cameras, and whether op 15 or the account needs
    /// telling that they changed.
    cameras: Feeds,
    wants_changed: bool,
    cameras_changed: bool,
    /// Ticks until the next receiver reports.
    report_video_in: u32,
    /// Whether the call is on screen, shared with the streams watched.
    visible: Arc<AtomicBool>,
    /// The Go Live streams watched, by key, and their sound's jitter buffers.
    streams: HashMap<String, Stream>,
    /// Wakes the UI after a frame is shown; given to each stream.
    shown: Arc<dyn Fn() + Send + Sync>,
    /// Streams that ended, still to report.
    ended: Vec<(String, Option<String>)>,
    /// Our camera, while it's on.
    camera: Option<OwnCamera>,
    /// Our camera stopped: op 12 must say so.
    withdraw: bool,
    /// Our camera's changes, still to report.
    camera_events: Vec<CallEvent>,
    /// Our Go Live stream, while it's sent.
    live: Option<super::golive::GoingLive>,
}

/// Our camera going out: its capture, and the sender of its encoded frames
/// (`outgoing`, after Yapfast's `call_video.rs`).
struct OwnCamera {
    capture: Option<godwit_voice::camera::Capture>,
    sender: Sender,
    /// Bound to the current connection (its SSRCs, op 12).
    bound: bool,
    /// Why the capture ended on its own (no camera, unplugged).
    ended: std::sync::mpsc::Receiver<String>,
}

/// The camera's frame rate and the frames kept to resend (a camera's few).
const CAMERA_FPS: u32 = 30;
const CAMERA_HISTORY: usize = 256;
/// The camera's largest picture (Discord's 720p camera step).
const CAMERA_LIMIT: [u32; 2] = [1280, 720];

/// A stream watched: its connection, and its sound on the way to the mix.
struct Stream {
    watching: Watching,
    /// Where it's served, so a repeated command doesn't reconnect.
    server: super::stream::StreamServer,
    sound: Option<Speaker>,
}

impl Media {
    fn new(muted: bool, deafened: bool, shown: Arc<dyn Fn() + Send + Sync>) -> Result<Self> {
        Ok(Self {
            encoder: opus::Encoder::new(CHANNELS, BITRATE)?,
            mono: vec![0.0; FRAME],
            stereo: vec![0.0; FRAME * usize::from(CHANNELS)],
            packet: vec![0; 1500],
            sequence: rand::random(),
            timestamp: rand::random(),
            speaking: false,
            silent: 0,
            muted,
            deafened,
            speakers: HashMap::new(),
            users: HashMap::new(),
            report_in: 0,
            reported: None,
            encrypting: None,
            cameras: Feeds::new(FeedKind::Camera, CAMERAS, shown.clone()),
            // Op 15 goes once connected, before anyone's camera is named.
            wants_changed: true,
            cameras_changed: false,
            report_video_in: REPORT_EVERY,
            visible: Arc::new(AtomicBool::new(false)),
            streams: HashMap::new(),
            shown,
            ended: Vec::new(),
            camera: None,
            withdraw: false,
            camera_events: Vec::new(),
            live: None,
        })
    }

    /// A new connection: speakers and their SSRCs start over.
    fn reset(&mut self) {
        self.speakers.clear();
        self.users.clear();
        self.speaking = false;
        self.silent = 0;
        self.encrypting = None;
        self.cameras.reset();
        self.cameras_changed = true;
        // Our camera binds to the new connection's SSRCs with its next frame.
        if let Some(camera) = self.camera.as_mut() {
            camera.bound = false;
        }
        self.withdraw = false;
        // Op 15 goes again once the new connection names the cameras.
        self.wants_changed = true;
    }

    /// A command that doesn't change the connection.
    fn apply(&mut self, command: CallCmd, audio: &RawAudio) {
        match command {
            CallCmd::Muted(muted) => self.muted = muted,
            CallCmd::Deafened(deafened) => {
                self.deafened = deafened;
                audio.set_deafened(deafened);
            }
            CallCmd::Settings(settings) => audio.update(&settings),
            CallCmd::Watch(server) => self.watch(server),
            CallCmd::Unwatch(key) => {
                if self.streams.remove(&key).is_some() {
                    audio.forget(&key);
                }
            }
            CallCmd::Visible(visible) => {
                if self.visible.swap(visible, Ordering::Relaxed) != visible {
                    self.cameras.set_visible(visible);
                    self.wants_changed = true;
                }
            }
            CallCmd::Camera { on, device } => self.set_camera(on, device),
            CallCmd::PrepareLive => self.prepare_live(),
            CallCmd::GoLive(server) => self.go_live(server),
            CallCmd::Server(_) | CallCmd::Leave => {}
        }
    }

    /// Turns our camera on (capture → encoder → sender) or off.
    fn set_camera(&mut self, on: bool, device: Option<String>) {
        if !on {
            self.stop_camera(None);
            return;
        }
        if self.camera.is_some() {
            return;
        }
        let profile = Profile {
            content: Content::Camera,
            fps: CAMERA_FPS,
        };
        let started = Encoding::start(profile).and_then(|(sink, encoding)| {
            let (end, ended) = std::sync::mpsc::channel();
            let capture = godwit_voice::camera::start(
                sink,
                device,
                CAMERA_LIMIT,
                move |why| {
                    let _ = end.send(why);
                },
                self.shown.clone(),
            )?;
            Ok(OwnCamera {
                capture: Some(capture),
                sender: Sender::new(encoding, profile, CAMERA_HISTORY),
                bound: false,
                ended,
            })
        });
        match started {
            Ok(camera) => {
                self.camera = Some(camera);
                self.camera_events.push(CallEvent::OwnCamera {
                    active: true,
                    error: None,
                });
            }
            Err(e) => self.camera_events.push(CallEvent::OwnCamera {
                active: false,
                error: Some(format!("The camera couldn't start: {e}")),
            }),
        }
    }

    /// Starts capturing for our stream: the screen is chosen, then
    /// `ScreenPicked` lets the account tell Discord.
    fn prepare_live(&mut self) {
        if self.live.is_some() {
            return;
        }
        match super::golive::GoingLive::start(self.shown.clone()) {
            Ok(live) => self.live = Some(live),
            Err(e) => self.camera_events.push(CallEvent::OwnStream {
                active: false,
                error: Some(format!("{e:#}")),
            }),
        }
    }

    /// Sends our stream to `server` (or moves it there), or stops it.
    fn go_live(&mut self, server: Option<super::stream::StreamServer>) {
        let Some(server) = server else {
            if self.live.take().is_some() {
                self.camera_events.push(CallEvent::OwnStream {
                    active: false,
                    error: None,
                });
            }
            return;
        };
        if self.live.is_none() {
            self.prepare_live();
        }
        let Some(live) = self.live.as_mut() else { return };
        let first = live.server.is_none();
        live.send_to(server);
        if first {
            self.camera_events.push(CallEvent::OwnStream {
                active: true,
                error: None,
            });
        }
    }

    /// Our camera off: the device let go off this thread, op 12 told.
    fn stop_camera(&mut self, error: Option<String>) {
        let Some(mut camera) = self.camera.take() else { return };
        if let Some(capture) = camera.capture.take() {
            outgoing::release(capture, godwit_voice::camera::Capture::request_stop);
        }
        self.withdraw = camera.bound;
        self.camera_events.push(CallEvent::OwnCamera { active: false, error });
    }

    /// Sends our camera's next frame (binding to the connection first).
    async fn send_camera(&mut self, connection: &mut Connection, frame: outgoing::Encoded) -> Result<()> {
        let Some(camera) = self.camera.as_mut() else {
            return Ok(());
        };
        if !camera.bound {
            camera.sender.bind(connection);
            camera.bound = true;
        }
        camera.sender.send(connection, frame, false).await
    }

    /// Starts watching a stream, or follows it to where Discord moved it.
    fn watch(&mut self, server: super::stream::StreamServer) {
        let key = server.stream_key.clone();
        if self.streams.get(&key).is_some_and(|stream| stream.server == server) {
            return;
        }
        match Watching::start(server.clone(), self.visible.clone(), self.shown.clone()) {
            Ok(watching) => {
                self.streams.insert(
                    key,
                    Stream {
                        watching,
                        server,
                        sound: None,
                    },
                );
            }
            Err(e) => self.ended.push((key, Some(format!("{e:#}")))),
        }
    }

    async fn run(
        &mut self,
        connection: &mut Connection,
        audio: &RawAudio,
        commands: &mut mpsc::UnboundedReceiver<CallCmd>,
        emit: &Emit,
    ) -> Ended {
        for (op, data) in std::mem::take(&mut connection.early) {
            self.learn(op, &data);
        }
        let mut heartbeat = tokio::time::interval_at(Instant::now() + connection.interval, connection.interval);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut ticks = tokio::time::interval(Duration::from_millis(20));
        // Windows wakes timers on its 15.6 ms clock, so a 20 ms tick often
        // comes at 31 ms: catching up on missed ticks keeps 50 frames a
        // second each way.
        ticks.set_missed_tick_behavior(MissedTickBehavior::Burst);
        let mut keepalive = tokio::time::interval(KEEPALIVE);
        keepalive.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut datagram = vec![0u8; 2048];
        let result: Result<Ended> = async {
            loop {
                tokio::select! {
                    command = commands.recv() => match command {
                        None | Some(CallCmd::Leave) => return Ok(Ended::Left),
                        Some(CallCmd::Server(server)) => return Ok(Ended::Moved(server)),
                        Some(other) => self.apply(other, audio),
                    },
                    due = ticks.tick() => {
                        if due.elapsed() > STALLED {
                            // The ticks a stall skips pass in the timestamp all the same.
                            let skipped = u32::try_from(due.elapsed().as_millis() / 20).unwrap_or(0);
                            self.timestamp = self.timestamp.wrapping_add(skipped.wrapping_mul(FRAME as u32));
                            ticks.reset();
                        }
                        let (me, others) = audio.speaking();
                        self.send(connection, audio, me).await?;
                        self.hear(audio);
                        self.hear_streams(audio, emit);
                        self.report(me, others, &**emit);
                        // After the sound, which mustn't wait on video.
                        self.video(connection, emit).await?;
                    }
                    _ = heartbeat.tick() => connection.heartbeat().await?,
                    _ = keepalive.tick() => {
                        let mut packet = [0u8; 8];
                        packet[..4].copy_from_slice(&connection.ssrc.to_be_bytes());
                        connection.udp.send(&packet).await?;
                    }
                    message = receive(&mut connection.ws, &mut connection.seq) => {
                        if let Some((op, data)) = connection.handle(message?).await? {
                            // Someone joined: announce our camera again, as
                            // the server may not pass the last one on.
                            if op == super::connection::op::CLIENTS_CONNECT
                                && let Some(camera) = self.camera.as_mut().filter(|c| c.bound)
                            {
                                camera.sender.announce(connection).await?;
                            }
                            self.learn(op, &data);
                        }
                    }
                    length = connection.udp.recv(&mut datagram) => {
                        if let Some(rtcp) = self.receive(connection, &datagram[..length?])
                            && let Some(camera) = self.camera.as_mut().filter(|c| c.bound)
                        {
                            camera.sender.feedback(connection, &rtcp).await?;
                        }
                    }
                    frame = camera_frame(&mut self.camera) => {
                        if let Some(frame) = frame {
                            self.send_camera(connection, frame).await?;
                        }
                    }
                }
            }
        }
        .await;
        result.unwrap_or_else(Ended::Dropped)
    }

    /// The cameras' part of a tick: op 15 when what's wanted changed, who
    /// films to the account, frames put together and repaired, and now and
    /// then receiver reports.
    async fn video(&mut self, connection: &mut Connection, emit: &Emit) -> Result<()> {
        // Our camera: stopped on its own, withdrawn, and its changes reported.
        if let Some(why) = self.camera.as_ref().and_then(|c| c.ended.try_recv().ok()) {
            self.stop_camera(Some(why));
        }
        // The screen for our stream chosen: the account tells Discord.
        if self.live.as_ref().is_some_and(|l| l.picked.try_recv().is_ok()) {
            self.camera_events.push(CallEvent::ScreenPicked);
        }
        // Our stream ended on its own (the picker cancelled, the connection lost).
        let live_ended = match self.live.as_ref().map(|l| l.ended.try_recv()) {
            Some(Ok(reason)) => Some(reason),
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => Some(None),
            _ => None,
        };
        if let Some(reason) = live_ended {
            self.live = None;
            self.camera_events.push(CallEvent::OwnStream {
                active: false,
                error: reason,
            });
        }
        if std::mem::take(&mut self.withdraw) {
            outgoing::withdraw(connection).await?;
        }
        for event in self.camera_events.drain(..) {
            emit(event);
        }
        if std::mem::take(&mut self.wants_changed) {
            self.cameras.send_wants(connection).await?;
        }
        if std::mem::take(&mut self.cameras_changed) {
            emit(CallEvent::Cameras(
                self.cameras.senders().iter().map(u64::to_string).collect(),
            ));
        }
        self.cameras.repair(connection).await?;
        self.report_video_in = self.report_video_in.saturating_sub(1);
        if self.report_video_in == 0 {
            self.report_video_in = REPORT_EVERY;
            self.cameras.report(connection).await?;
        }
        Ok(())
    }

    /// Plays each stream's next 20 ms under its streamer's screen share
    /// volume, and reports those that ended.
    fn hear_streams(&mut self, audio: &RawAudio, emit: &Emit) {
        for (key, stream) in &mut self.streams {
            let ended = match stream.watching.ended.try_recv() {
                Ok(reason) => Some(reason),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(None),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
            };
            if let Some(reason) = ended {
                self.ended.push((key.clone(), reason));
                continue;
            }
            let streamer = stream.watching.streamer;
            while let Ok((sequence, opus)) = stream.watching.sound.try_recv() {
                if stream.sound.is_none() {
                    match Speaker::new(streamer) {
                        Ok(speaker) => stream.sound = Some(speaker),
                        Err(e) => {
                            note(&format!("couldn't decode a stream's sound: {e:#}"));
                            break;
                        }
                    }
                }
                if let Some(speaker) = stream.sound.as_mut() {
                    speaker.push(sequence, opus);
                }
            }
            if let Some(samples) = stream.sound.as_mut().and_then(Speaker::next) {
                audio.play_shared(key, &streamer.to_string(), samples);
            }
        }
        for (key, reason) in std::mem::take(&mut self.ended) {
            if self.streams.remove(&key).is_some() {
                audio.forget(&key);
            }
            emit(CallEvent::StreamEnded { key, reason });
        }
    }

    /// Who's speaking, to the account, when it changes: we count only
    /// while our microphone is live.
    fn report(&mut self, me: bool, others: Vec<String>, emit: &dyn Fn(CallEvent)) {
        if self.report_in > 0 {
            self.report_in -= 1;
            return;
        }
        self.report_in = SPEAKING_EVERY - 1;
        let now = (me && self.live(), others);
        if self.reported.as_ref() != Some(&now) {
            emit(CallEvent::Speaking {
                me: now.0,
                users: now.1.clone(),
            });
            self.reported = Some(now);
        }
    }

    /// Whether our microphone may be heard: Discord's clients mute
    /// themselves while deafened.
    fn live(&self) -> bool {
        !self.muted && !self.deafened
    }

    /// Learns whose audio each SSRC carries from their speaking messages
    /// and their video ones. Someone already talking when we join may send
    /// no speaking message again; their video message, sent for everyone in
    /// the call, names their audio too.
    fn learn(&mut self, op: u64, data: &Value) {
        if self.cameras.learn(op, data) {
            self.cameras_changed = true;
            self.wants_changed = true;
        }
        let ssrc = match op {
            5 => &data["ssrc"],
            op::VIDEO => &data["audio_ssrc"],
            _ => return,
        };
        if let (Some(user), Some(ssrc)) = (
            data["user_id"].as_str().and_then(|id| id.parse().ok()),
            ssrc.as_u64().filter(|&s| s != 0).and_then(|s| u32::try_from(s).ok()),
        ) {
            self.users.insert(ssrc, user);
        }
    }

    /// Sends 20 ms of the microphone while the voice gate is open (we're
    /// audible and sound like speech), then five silent frames when it
    /// shuts, as Discord's clients do. Each call is a tick.
    async fn send(&mut self, connection: &mut Connection, audio: &RawAudio, voice: bool) -> Result<()> {
        let playing = voice && self.live();
        if playing {
            let count = audio.take(&mut self.mono);
            self.mono[count..].fill(0.0);
            for (out, &sample) in self.stereo.as_chunks_mut::<2>().0.iter_mut().zip(&self.mono) {
                out.fill(sample);
            }
        } else {
            audio.keep_newest(PREROLL);
        }
        let marker = playing && !self.speaking;
        if marker {
            send_json(
                &mut connection.ws,
                op::SPEAKING,
                json!({"speaking": MICROPHONE, "delay": 0, "ssrc": connection.ssrc}),
            )
            .await?;
            self.speaking = true;
        }
        if playing && self.encrypting != Some(connection.dave.encrypting()) {
            self.encrypting = Some(connection.dave.encrypting());
            note(&format!(
                "microphone sending, end-to-end encrypted: {} (DAVE {})",
                connection.dave.encrypting(),
                connection.dave.status()
            ));
        }
        if playing {
            let length = self.encoder.encode_float(&self.stereo, &mut self.packet)?;
            if let Some(payload) = connection.dave.encrypt_opus(&self.packet[..length]) {
                self.transmit(connection, &payload, marker).await?;
            }
            self.silent = SILENT_FRAMES;
        } else if self.silent > 0 {
            if let Some(payload) = connection.dave.encrypt_opus(&SILENCE) {
                self.transmit(connection, &payload, false).await?;
            }
            self.silent -= 1;
            if self.silent == 0 {
                self.speaking = false;
                send_json(
                    &mut connection.ws,
                    op::SPEAKING,
                    json!({"speaking": 0, "delay": 0, "ssrc": connection.ssrc}),
                )
                .await?;
            }
        }
        self.timestamp = self.timestamp.wrapping_add(FRAME as u32);
        Ok(())
    }

    async fn transmit(&mut self, connection: &mut Connection, payload: &[u8], marker: bool) -> Result<()> {
        let mut packet = rtp::opus_packet(connection.ssrc, self.sequence, self.timestamp, marker, payload);
        self.sequence = self.sequence.wrapping_add(1);
        let clear = rtp::rtp_clear_len(&packet).unwrap_or(packet.len());
        connection.transport.seal(&mut packet, clear)?;
        connection.udp.send(&packet).await?;
        Ok(())
    }

    /// Queues a received Opus packet for its speaker, and a camera's video
    /// packet for its feed. Anything that doesn't authenticate is dropped;
    /// of RTCP, only cameras' sender reports are kept.
    /// Takes in a datagram; RTCP comes back too, for our camera's sender.
    fn receive(&mut self, connection: &mut Connection, datagram: &[u8]) -> Option<Vec<u8>> {
        if rtp::is_rtcp(datagram) {
            let rtcp = connection.transport.open(datagram, RTCP_CLEAR)?;
            self.cameras.heard_rtcp(&rtcp);
            return Some(rtcp.to_vec());
        }
        let clear = rtp::rtp_clear_len(datagram)?;
        let plain = connection.transport.open(datagram, clear)?;
        let packet = rtp::parse(&plain)?;
        if packet.payload_type != rtp::OPUS {
            self.cameras.receive(&packet, connection.video_codec);
            return None;
        }
        // DAVE keys each sender's frames by who they are, which only the
        // voice gateway tells.
        let &user = self.users.get(&packet.ssrc)?;
        let opus = connection.dave.decrypt_opus(user, packet.payload.to_vec())?;
        let speaker = match self.speakers.entry(packet.ssrc) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => match Speaker::new(user) {
                Ok(speaker) => entry.insert(speaker),
                Err(e) => {
                    note(&format!("couldn't decode a speaker: {e:#}"));
                    return None;
                }
            },
        };
        speaker.user = user;
        speaker.push(packet.sequence, opus);
        None
    }

    /// Plays everyone's next 20 ms, and forgets those long gone.
    fn hear(&mut self, audio: &RawAudio) {
        let now = std::time::Instant::now();
        self.speakers.retain(|_, speaker| {
            let keep = now.duration_since(speaker.heard) < FORGET;
            if !keep {
                audio.forget(&speaker.user.to_string());
            }
            keep
        });
        for speaker in self.speakers.values_mut() {
            let user = speaker.user.to_string();
            if let Some(samples) = speaker.next() {
                audio.play(&user, samples);
            }
        }
    }
}

/// One speaker's audio on its way to the mix: packets in order, waiting a
/// little for those that come late, and decoded one 20 ms frame a tick.
struct Speaker {
    user: u64,
    decoder: opus::Decoder,
    /// Packets by sequence number, oldest first.
    waiting: VecDeque<(u16, Vec<u8>)>,
    /// The sequence number to play next, while playing.
    next: Option<u16>,
    /// Packets missing in a row.
    missing: u8,
    /// The last packet decoded, mono: room for the longest an Opus packet
    /// can be, 120 ms (music bots pass 40 or 60 ms packets through).
    pcm: Vec<i16>,
    /// What of `pcm` is still to play, 20 ms a tick.
    left: std::ops::Range<usize>,
    heard: std::time::Instant,
}

/// The most samples an Opus packet decodes to: 120 ms.
const LONGEST: usize = FRAME * 6;

/// Whether sequence number `a` comes before `b`, across wrapping.
fn before(a: u16, b: u16) -> bool {
    a != b && b.wrapping_sub(a) < 0x8000
}

impl Speaker {
    fn new(user: u64) -> Result<Self> {
        Ok(Self {
            user,
            decoder: opus::Decoder::new(1)?,
            waiting: VecDeque::with_capacity(MOST + 1),
            next: None,
            missing: 0,
            pcm: vec![0; LONGEST],
            left: 0..0,
            heard: std::time::Instant::now(),
        })
    }

    fn push(&mut self, sequence: u16, opus: Vec<u8>) {
        self.heard = std::time::Instant::now();
        if self.next.is_some_and(|next| before(sequence, next)) {
            return;
        }
        let at = self
            .waiting
            .iter()
            .rposition(|(waiting, _)| !before(sequence, *waiting))
            .map_or(0, |at| at + 1);
        if at > 0 && self.waiting[at - 1].0 == sequence {
            return;
        }
        self.waiting.insert(at, (sequence, opus));
        while self.waiting.len() > MOST {
            self.waiting.pop_front();
            self.next = self.waiting.front().map(|(sequence, _)| *sequence);
        }
    }

    /// The next 20 ms, or nothing while waiting to start or once stopped.
    fn next(&mut self) -> Option<&[i16]> {
        if self.left.is_empty() {
            let samples = self.decode_next()?;
            self.left = 0..samples.min(self.pcm.len());
        }
        let start = self.left.start;
        let end = self.left.end.min(start + FRAME);
        self.left.start = end;
        self.pcm.get(start..end)
    }

    /// Decodes the next packet into `pcm`, or conceals a missing one, and
    /// says how many samples it made.
    fn decode_next(&mut self) -> Option<usize> {
        let next = match self.next {
            Some(next) => next,
            None if self.waiting.len() >= START => self.waiting.front()?.0,
            None => return None,
        };
        while self
            .waiting
            .front()
            .is_some_and(|(sequence, _)| before(*sequence, next))
        {
            self.waiting.pop_front();
        }
        self.next = Some(next);
        let decoded = match self.waiting.front() {
            Some((sequence, _)) if *sequence == next => {
                self.missing = 0;
                let (_, opus) = self.waiting.pop_front()?;
                self.next = Some(next.wrapping_add(1));
                self.decoder.decode(&opus, &mut self.pcm, false)
            }
            waiting => {
                self.missing = self.missing.saturating_add(1);
                if self.missing >= GONE && waiting.is_none() {
                    // Stopped talking: wait for them to start again.
                    self.next = None;
                    self.missing = 0;
                    return None;
                }
                match waiting {
                    // Nothing after it has come either: it's late rather
                    // than lost, so it plays next.
                    None => self.decoder.conceal(&mut self.pcm[..FRAME]),
                    // The next packet carries this one's forward error correction.
                    Some((sequence, opus)) if *sequence == next.wrapping_add(1) => {
                        self.next = Some(next.wrapping_add(1));
                        self.decoder.decode(opus, &mut self.pcm[..FRAME], true)
                    }
                    Some(_) => {
                        self.next = Some(next.wrapping_add(1));
                        self.decoder.conceal(&mut self.pcm[..FRAME])
                    }
                }
            }
        };
        decoded.ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(encoder: &mut opus::Encoder, frame: usize) -> Vec<u8> {
        let pcm: Vec<f32> = (0..FRAME)
            .flat_map(|at| {
                let s = ((frame * FRAME + at) as f32 * 330.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5;
                [s, s]
            })
            .collect();
        let mut packet = vec![0; 1500];
        let length = encoder.encode_float(&pcm, &mut packet).unwrap();
        packet.truncate(length);
        packet
    }

    #[test]
    fn a_speaker_waits_reorders_conceals_and_stops() {
        let mut encoder = opus::Encoder::new(CHANNELS, BITRATE).unwrap();
        let mut speaker = Speaker::new(7).unwrap();
        let packets: Vec<Vec<u8>> = (0..8).map(|frame| tone(&mut encoder, frame)).collect();
        // Two packets aren't enough to start.
        speaker.push(65_534, packets[0].clone());
        speaker.push(65_535, packets[1].clone());
        assert!(speaker.next().is_none(), "waits for 60 ms");
        // The fourth arrives before the third, across the wrap.
        speaker.push(1, packets[3].clone());
        speaker.push(0, packets[2].clone());
        for _ in 0..4 {
            assert_eq!(speaker.next().map(<[i16]>::len), Some(FRAME));
        }
        assert!(speaker.waiting.is_empty());
        // A packet that comes too late is dropped, not played.
        speaker.push(65_535, packets[1].clone());
        assert!(speaker.waiting.is_empty());
        // Packet 2 is lost: it's rebuilt from packet 3's error correction.
        speaker.push(3, packets[5].clone());
        assert!(speaker.next().is_some(), "packet 2, rebuilt");
        assert!(speaker.next().is_some(), "packet 3");
        // Then silence: concealed briefly, then the speaker stops.
        let played = std::iter::from_fn(|| speaker.next().map(|_| ())).count();
        assert_eq!(played, usize::from(GONE) - 1);
        assert!(speaker.next.is_none());
    }

    #[test]
    fn a_long_stall_keeps_only_the_newest() {
        let mut encoder = opus::Encoder::new(CHANNELS, BITRATE).unwrap();
        let mut speaker = Speaker::new(7).unwrap();
        let packet = tone(&mut encoder, 0);
        for sequence in 0..(MOST as u16 + 10) {
            speaker.push(sequence, packet.clone());
        }
        assert_eq!(speaker.waiting.len(), MOST);
        assert_eq!(speaker.waiting.front().map(|(s, _)| *s), Some(10));
    }

    #[test]
    fn a_long_packet_plays_over_as_many_ticks() {
        let mut encoder = opus::Encoder::new(CHANNELS, BITRATE).unwrap();
        let mut speaker = Speaker::new(7).unwrap();
        let music: Vec<f32> = (0..FRAME * 3 * 2).map(|at| (at as f32 * 0.01).sin() * 0.25).collect();
        for sequence in 0..START as u16 {
            let mut packet = vec![0; 4000];
            let length = encoder.encode_float(&music, &mut packet).unwrap();
            packet.truncate(length);
            speaker.push(sequence, packet);
        }
        for tick in 0..START * 3 {
            assert_eq!(speaker.next().map(<[i16]>::len), Some(FRAME), "tick {tick}");
        }
    }

    #[test]
    fn learns_whose_audio_from_speaking_and_video_messages() {
        let mut media = Media::new(false, false, Arc::new(|| {})).unwrap();
        media.learn(5, &json!({"user_id": "7", "ssrc": 100, "speaking": 1}));
        // Already talking when we joined: only op 12 names their audio.
        media.learn(12, &json!({"user_id": "8", "audio_ssrc": 200, "video_ssrc": 0}));
        media.learn(12, &json!({"user_id": "9", "audio_ssrc": 0, "video_ssrc": 0}));
        assert_eq!(media.users, HashMap::from([(100, 7), (200, 8)]));
    }

    #[test]
    fn speaking_is_reported_when_it_changes_and_only_while_audible() {
        let events = std::sync::Mutex::new(Vec::new());
        let emit = |e: CallEvent| events.lock().unwrap().push(e);
        let mut media = Media::new(false, false, Arc::new(|| {})).unwrap();
        media.report(true, vec!["7".into()], &emit);
        for _ in 0..SPEAKING_EVERY {
            media.report(true, vec!["7".into()], &emit);
        }
        media.muted = true;
        for _ in 0..SPEAKING_EVERY {
            media.report(true, vec!["7".into()], &emit);
        }
        let events = events.into_inner().unwrap();
        assert_eq!(events.len(), 2, "once, then once more when muting changed it");
        assert!(matches!(&events[0], CallEvent::Speaking { me: true, users } if users == &["7"]));
        assert!(matches!(&events[1], CallEvent::Speaking { me: false, .. }));
    }

    #[test]
    fn a_dm_call_identifies_with_its_channel_and_keys_dave_by_it() {
        let server = VoiceServer {
            endpoint: "voice.example".into(),
            token: "t".into(),
            session_id: "s".into(),
            user_id: "1".into(),
            server_id: "30".into(),
            channel_id: "30".into(),
        };
        let call = identify_as(&server).unwrap();
        assert_eq!((call.user, call.group), (1, 30));
        assert_eq!(call.server_id, "30");
        assert!(
            identify_as(&VoiceServer {
                channel_id: "x".into(),
                ..server
            })
            .is_none()
        );
    }
}
