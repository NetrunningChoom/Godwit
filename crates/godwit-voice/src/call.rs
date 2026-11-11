// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! One LiveKit call, structured the way the official client works in a browser:
//!
//! - Microphone → WebRTC audio processing (echo cancellation, auto gain and
//!   optionally WebRTC noise suppression; what the browser's getUserMedia does)
//!   → our high-pass/RNNoise/compressor chain (the client's Web Audio graph) →
//!   input volume → LiveKit.
//! - Each remote participant is played separately with its own volume (the
//!   client's per-user `<audio>` elements), mixed, and the mix is fed back to
//!   the echo canceller as its reference, as the browser does internally.

use crate::sync::lock;

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use livekit::RtcAudioSource;
use livekit::options::TrackPublishOptions;
use livekit::prelude::*;
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::AudioSourceOptions;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::native::apm::AudioProcessingModule;
use livekit::webrtc::video_source::RtcVideoSource;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::dsp::{FRAME, NoiseSuppression, Processor, SAMPLE_RATE};

/// Voice preferences, defaults and ranges matching the official client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceSettings {
    pub noise_suppression: NoiseSuppression,
    pub echo_cancellation: bool,
    pub auto_gain_control: bool,
    /// 0..3, like the official slider.
    pub input_volume: f32,
    /// 0..3, like the official slider.
    pub output_volume: f32,
    /// Device names as the OS reports them; None means the system default.
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    /// Per-user playback volume (Stoat user id → 0..3), from the user menu.
    pub user_volumes: HashMap<String, f32>,
    /// Users muted locally from the user menu.
    pub muted_users: Vec<String>,
    /// Per-user screen share volume (0..3), from the screen share's menu.
    pub screen_share_volumes: HashMap<String, f32>,
    /// Per-user screen share mute; unset means muted, as officially.
    pub screen_share_mutes: HashMap<String, bool>,
    /// Screen share quality, and whether to ask each time (official defaults: low, ask).
    pub screen_quality: super::screen::ScreenQuality,
    pub screen_quality_ask: bool,
    /// Camera name; None means the first one.
    pub camera: Option<String>,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            noise_suppression: NoiseSuppression::Enhanced,
            echo_cancellation: true,
            auto_gain_control: true,
            input_volume: 1.0,
            output_volume: 1.0,
            input_device: None,
            output_device: None,
            user_volumes: HashMap::new(),
            muted_users: Vec::new(),
            screen_share_volumes: HashMap::new(),
            screen_share_mutes: HashMap::new(),
            screen_quality: super::screen::ScreenQuality::Low,
            screen_quality_ask: true,
            camera: None,
        }
    }
}

impl VoiceSettings {
    pub fn user_volume(&self, user: &str) -> f32 {
        self.user_volumes.get(user).copied().unwrap_or(1.0)
    }

    pub fn user_muted(&self, user: &str) -> bool {
        self.muted_users.iter().any(|u| u == user)
    }

    pub fn screen_share_volume(&self, user: &str) -> f32 {
        self.screen_share_volumes.get(user).copied().unwrap_or(1.0)
    }

    /// Screen share audio stays muted until unmuted for that person, as officially.
    pub fn screen_share_muted(&self, user: &str) -> bool {
        self.screen_share_mutes.get(user).copied().unwrap_or(true)
    }
}

pub enum VoiceCmd {
    SetMuted(bool),
    SetDeafened(bool),
    /// New settings mid-call: volumes and devices apply immediately;
    /// processing options apply on the next join.
    Update(Box<VoiceSettings>),
    /// Video tracks (sids) the UI is showing; only these are received.
    Watch(Vec<String>),
    StartScreenShare(super::screen::ScreenOptions),
    /// Turn the camera on (with the device from settings) or off.
    SetCamera {
        active: bool,
        resolution: [u32; 2],
    },
    StopScreenShare,
    Leave,
}

/// A camera or screen share someone in the call is publishing.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoTrackInfo {
    pub sid: String,
    pub identity: String,
    pub screen_share: bool,
}

#[derive(Debug, Clone)]
pub enum VoiceEvent {
    Connected {
        channel: String,
    },
    /// Latest ICE candidate-pair round trip in milliseconds, if available.
    Latency(Option<u16>),
    Disconnected {
        reason: Option<String>,
    },
    /// LiveKit identities (Stoat user ids) currently in the room, including us.
    Participants(Vec<String>),
    Speaking(Vec<String>),
    Muted {
        identity: String,
        muted: bool,
    },
    /// Everyone's published cameras and screen shares.
    Videos(Vec<VideoTrackInfo>),
    /// A watched video track has a new frame (see `video::FRAMES`).
    VideoFrame,
    /// Whether we're sharing our screen, and why it stopped if it did on its own.
    ScreenShare {
        active: bool,
        error: Option<String>,
    },
    /// Whether our camera is on, and why it went off if it did on its own.
    Camera {
        active: bool,
        error: Option<String>,
    },
}

/// Joins `url` with `token` and runs until told to leave or disconnected.
pub async fn run(
    channel: String,
    url: String,
    token: String,
    settings: VoiceSettings,
    mut cmds: mpsc::UnboundedReceiver<VoiceCmd>,
    emit: Arc<dyn Fn(VoiceEvent) + Send + Sync>,
) {
    let reason = match call(&channel, &url, &token, settings, &mut cmds, &emit).await {
        Ok(()) => None,
        Err(e) => Some(format!("{e:#}")),
    };
    emit(VoiceEvent::Disconnected { reason });
}

async fn call(
    channel: &str,
    url: &str,
    token: &str,
    mut settings: VoiceSettings,
    cmds: &mut mpsc::UnboundedReceiver<VoiceCmd>,
    emit: &Arc<dyn Fn(VoiceEvent) + Send + Sync>,
) -> Result<()> {
    // Like the official client, subscribe by hand: all audio, but video only
    // while it's on screen, so we don't download streams nobody watches.
    let mut options = RoomOptions::default();
    options.auto_subscribe = false;
    // Our signalling socket, IPv4 first: broken IPv6 otherwise eats the connect timeout on Windows.
    crate::transport::install();
    // Leaving while connecting must stop here, before any microphone or
    // speaker is opened. Other commands sent meanwhile (mute, settings) are
    // kept and run once the call is up.
    let mut early: VecDeque<VoiceCmd> = VecDeque::new();
    let Some(connected) = until_leave(Room::connect(url, token, options), cmds, &mut early).await else {
        return Ok(());
    };
    let (room, mut events) = connected.context("connecting to voice server")?;
    if leave_requested(cmds, &mut early) {
        log_error("closing voice room", room.close().await);
        return Ok(());
    }
    emit(VoiceEvent::Connected {
        channel: channel.to_string(),
    });

    // Publish an empty, disabled source before opening devices. A slow or
    // cancelled publication must not keep the microphone open while waiting.
    let source = NativeAudioSource::new(AudioSourceOptions::default(), SAMPLE_RATE, 1, 0);
    let track = LocalAudioTrack::create_audio_track("microphone", RtcAudioSource::Native(source.clone()));
    // LiveKit 0.10 local_participant::publish_track reads track.is_muted().
    // Keep the RTC track disabled until commands arriving during publication
    // and device startup have also been applied.
    let mut controls = InitialControls::default();
    controls.take(&mut early);
    track.disable();
    if controls.muted {
        track.mute();
    }
    let publish = TrackPublishOptions {
        source: TrackSource::Microphone,
        ..Default::default()
    };
    let published = until_leave(
        room.local_participant()
            .publish_track(LocalTrack::Audio(track.clone()), publish),
        cmds,
        &mut early,
    )
    .await;
    if !matches!(published, Some(Ok(_))) || leave_requested(cmds, &mut early) {
        log_error("closing voice room", room.close().await);
        return match published {
            Some(Err(error)) => Err(error).context("publishing microphone"),
            _ => Ok(()),
        };
    }
    // The SDK enables the RTC track at the end of publish_track, even when
    // its muted flag was already true. No devices have been opened yet.
    track.disable();
    // Preferences changed while joining must choose the initial devices and
    // DSP options, rather than briefly opening the old microphone first.
    early.retain(|cmd| match cmd {
        VoiceCmd::Update(new) => {
            settings = (**new).clone();
            false
        }
        _ => true,
    });
    controls.take(&mut early);
    let loud: Loudness = Arc::new(Mutex::new(HashMap::new()));
    let mixer = Arc::new(Mutex::new(Mixer::default()));
    {
        let mut mixer = lock(&mixer);
        mixer.apply(&settings);
        mixer.deafened = controls.deafened;
    }

    let (render_tx, render_rx) = std::sync::mpsc::channel::<Vec<i16>>();
    let mut output = match Output::start(settings.output_device.clone(), mixer.clone(), render_tx.clone()) {
        Ok(output) => Some(output),
        Err(error) => {
            log_error("closing voice room", room.close().await);
            return Err(error);
        }
    };
    let input_gain = Arc::new(AtomicU32::new(settings.input_volume.to_bits()));
    let chain = Arc::new(AtomicU32::new(Chain::of(&settings).pack()));
    let (mic_tx, mic_rx) = mpsc::unbounded_channel::<Vec<f32>>();
    let engine = match start_engine(
        source,
        chain.clone(),
        input_gain.clone(),
        mic_rx,
        render_rx,
        loud.clone(),
    ) {
        Ok(engine) => engine,
        Err(error) => {
            drop(output);
            log_error("closing voice room", room.close().await);
            return Err(error);
        }
    };
    if leave_requested(cmds, &mut early) {
        drop(mic_tx);
        drop(output);
        join_engine(engine);
        log_error("closing voice room", room.close().await);
        return Ok(());
    }
    let mut mic = match Mic::start(settings.input_device.clone(), mic_tx.clone()) {
        Ok(mic) => Some(mic),
        Err(error) => {
            drop(mic_tx);
            drop(output);
            join_engine(engine);
            log_error("closing voice room", room.close().await);
            return Err(error);
        }
    };
    // Device APIs are synchronous; recheck before enabling the published track.
    if leave_requested(cmds, &mut early) {
        drop(mic);
        drop(mic_tx);
        drop(output);
        join_engine(engine);
        log_error("closing voice room", room.close().await);
        return Ok(());
    }
    controls.take(&mut early);
    set_microphone_muted(&track, controls.muted);
    lock(&mixer).deafened = controls.deafened;

    let me = room.local_participant().identity().to_string();
    emit(VoiceEvent::Muted {
        identity: me.clone(),
        muted: controls.muted,
    });
    let participants = |room: &Room| {
        let mut v: Vec<String> = room.remote_participants().keys().map(|k| k.to_string()).collect();
        v.push(me.clone());
        v
    };
    emit(VoiceEvent::Participants(participants(&room)));

    let mut players: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    let mut videos: Vec<VideoTrackInfo> = Vec::new();
    // Our screen share: its publication and the capture feeding it.
    let mut share: Option<(TrackSid, super::screen::Capture)> = None;
    // Its sound (where this system can record apps), as LiveKit's
    // ScreenShareAudio beside the video, as the official client.
    let mut share_audio: Option<(TrackSid, super::stream_audio::Capture)> = None;
    let (share_end_tx, mut share_end_rx) = mpsc::unbounded_channel::<(TrackSid, String)>();
    let mut camera: Option<(TrackSid, super::camera::Capture)> = None;
    let mut camera_resolution = [1280, 720];
    let (camera_end_tx, mut camera_end_rx) = mpsc::unbounded_channel::<(TrackSid, String)>();
    let mut watched: Vec<String> = Vec::new();
    let repaint: Arc<dyn Fn() + Send + Sync> = {
        let emit = emit.clone();
        Arc::new(move || emit(VoiceEvent::VideoFrame))
    };
    // Tracks published before we joined.
    for p in room.remote_participants().values() {
        for publication in p.track_publications().values() {
            consider_publication(publication, p, &mut videos, &watched);
        }
    }
    emit(VoiceEvent::Videos(videos.clone()));
    let mut muted = controls.muted;
    let mut speaking_shown: Vec<String> = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut stats_tick = tokio::time::interval(Duration::from_secs(5));
    stats_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_latency = None;

    // Media negotiation can wait on the server. Mute, deafen and Leave must
    // still take effect while a camera/share is being published or removed.
    macro_rules! media_request {
        ($operation:expr) => {{
            let Some(result) = until_leave_with($operation, cmds, &mut early, |cmd| match cmd {
                VoiceCmd::SetMuted(value) => {
                    muted = *value;
                    set_microphone_muted(&track, *value);
                    emit(VoiceEvent::Muted {
                        identity: me.clone(),
                        muted: *value,
                    });
                    true
                }
                VoiceCmd::SetDeafened(value) => {
                    lock(&mixer).deafened = *value;
                    true
                }
                VoiceCmd::Watch(sids) => {
                    set_watched(&room, sids, &mut players, &repaint);
                    watched = sids.clone();
                    true
                }
                VoiceCmd::Update(new) => {
                    lock(&mixer).apply(new);
                    input_gain.store(new.input_volume.to_bits(), Ordering::Relaxed);
                    chain.store(Chain::of(new).pack(), Ordering::Relaxed);
                    false // Device changes run after negotiation finishes.
                }
                VoiceCmd::SetCamera { active: false, .. } => {
                    if let Some((sid, capture)) = camera.as_mut() {
                        disable_local_video(&room, sid);
                        capture.request_stop();
                    }
                    false // Keep the command to unpublish when the current request ends.
                }
                VoiceCmd::StopScreenShare => {
                    if let Some((sid, capture)) = share.as_mut() {
                        disable_local_video(&room, sid);
                        capture.request_stop();
                    }
                    false
                }
                _ => false,
            })
            .await
            else {
                break Ok(());
            };
            result
        }};
    }

    // Starts the share's sound: no echo cancelling or noise suppression,
    // which are for voices and would mangle music and games. Without a way
    // to record apps here, the share goes on without sound.
    macro_rules! start_share_audio {
        () => {{
            use super::stream_audio as sa;
            let options = AudioSourceOptions {
                echo_cancellation: false,
                noise_suppression: false,
                auto_gain_control: false,
            };
            let source = NativeAudioSource::new(options, sa::SAMPLE_RATE, sa::CHANNELS, 0);
            let feed = source.clone();
            let mut errors = ErrorLog::default();
            let started = sa::start(sa::target_for_share(), move |frame: &[i16]| {
                let af = AudioFrame {
                    data: Cow::Borrowed(frame),
                    sample_rate: sa::SAMPLE_RATE,
                    num_channels: sa::CHANNELS,
                    samples_per_channel: (frame.len() / sa::CHANNELS as usize) as u32,
                };
                errors.once(
                    "feeding screen share sound",
                    futures_executor::block_on(feed.capture_frame(&af)),
                );
            });
            match started {
                Err(error) => {
                    eprintln!("note: sharing the screen without its sound: {error:#}");
                    None
                }
                Ok(capture) => {
                    let track = LocalAudioTrack::create_audio_track("screen-audio", RtcAudioSource::Native(source));
                    let options = TrackPublishOptions {
                        source: TrackSource::ScreenshareAudio,
                        // Music and games: no silence skipping, more bits.
                        dtx: false,
                        audio_encoding: Some(livekit::options::AudioEncoding { max_bitrate: 128_000 }),
                        ..Default::default()
                    };
                    match media_request!(
                        room.local_participant()
                            .publish_track(LocalTrack::Audio(track), options)
                    ) {
                        Ok(publication) => Some((publication.sid(), capture)),
                        Err(error) => {
                            eprintln!("error: publishing the screen share's sound: {error}");
                            None
                        }
                    }
                }
            }
        }};
    }
    macro_rules! stop_share_audio {
        () => {
            if let Some((sid, capture)) = share_audio.take() {
                drop(capture);
                log_error(
                    "unpublishing screen share sound",
                    media_request!(room.local_participant().unpublish_track(&sid)),
                );
            }
        };
    }

    let result = loop {
        tokio::select! {
            _ = tick.tick() => {
                // Speaking = recent speech-level audio we measured ourselves, or the
                // server's per-participant flag. Held for 300 ms to avoid flicker.
                let now = Instant::now();
                let recent = |id: &str| lock(&loud).get(id).is_some_and(|t| now.duration_since(*t).as_millis() < 300);
                let mut speaking: Vec<String> = room
                    .remote_participants()
                    .iter()
                    .filter(|(id, p)| p.is_speaking() || recent(id.as_str()))
                    .map(|(id, _)| id.to_string())
                    .collect();
                if !muted && (room.local_participant().is_speaking() || recent(LOCAL)) {
                    speaking.push(me.clone());
                }
                speaking.sort();
                if speaking != speaking_shown {
                    speaking_shown = speaking.clone();
                    emit(VoiceEvent::Speaking(speaking));
                }
            }
            _ = stats_tick.tick() => {
                use livekit::webrtc::stats::{IceCandidatePairState, RtcStats};
                let latency = room.get_stats().await.ok().and_then(|stats| {
                    stats.publisher_stats.into_iter().chain(stats.subscriber_stats)
                        .filter_map(|stat| match stat {
                            RtcStats::CandidatePair(pair)
                                if pair.candidate_pair.nominated
                                    && pair.candidate_pair.state == Some(IceCandidatePairState::Succeeded)
                                    && pair.candidate_pair.current_round_trip_time.is_finite()
                                    && pair.candidate_pair.current_round_trip_time > 0.0 =>
                                    Some((pair.candidate_pair.current_round_trip_time * 1000.0)
                                        .round().clamp(1.0, f64::from(u16::MAX)) as u16),
                            _ => None,
                        }).min()
                });
                if latency != last_latency {
                    last_latency = latency;
                    emit(VoiceEvent::Latency(latency));
                }
            }
            cmd = next_cmd(&mut early, cmds) => match cmd {
                Some(VoiceCmd::SetMuted(m)) => {
                    muted = m;
                    set_microphone_muted(&track, m);
                    emit(VoiceEvent::Muted { identity: me.clone(), muted: m });
                }
                Some(VoiceCmd::SetDeafened(d)) => lock(&mixer).deafened = d,
                Some(VoiceCmd::Update(new)) => {
                    lock(&mixer).apply(&new);
                    input_gain.store(new.input_volume.to_bits(), Ordering::Relaxed);
                    chain.store(Chain::of(&new).pack(), Ordering::Relaxed);
                    let restart = restart_for(&settings, &new);
                    // Devices switch live; keep the old one if the new one won't open.
                    if restart.output {
                        drop(output.take());
                        match Output::start(new.output_device.clone(), mixer.clone(), render_tx.clone()) {
                            Ok(o) => output = Some(o),
                            Err(e) => {
                                eprintln!("error: switching speakers: {e:#}");
                                output = match Output::start(settings.output_device.clone(), mixer.clone(), render_tx.clone()) {
                                    Ok(output) => Some(output),
                                    Err(error) => { log_error("restoring speakers", Err::<(), _>(error)); None }
                                };
                            }
                        }
                    }
                    if restart.input {
                        match Mic::start(new.input_device.clone(), mic_tx.clone()) {
                            Ok(m) => mic = Some(m),
                            Err(e) => eprintln!("error: switching microphone: {e:#}"),
                        }
                    }
                    let camera_changed = new.camera != settings.camera;
                    settings = *new;
                    if camera_changed && let Some((sid, capture)) = camera.take() {
                        disable_local_video(&room, &sid);
                        capture.request_stop();
                        drop(capture);
                        emit(VoiceEvent::Camera { active: false, error: None });
                        log_error("unpublishing voice video", media_request!(room.local_participant().unpublish_track(&sid)));
                        if !video_cancelled(&early, false) {
                            defer(&mut early, VoiceCmd::SetCamera { active: true, resolution: camera_resolution });
                        }
                    }
                }
                Some(VoiceCmd::Watch(sids)) => {
                    set_watched(&room, &sids, &mut players, &repaint);
                    watched = sids;
                }
                Some(VoiceCmd::StartScreenShare(share_options)) => {
                    if let Some((sid, capture)) = share.take() {
                        disable_local_video(&room, &sid);
                        super::video::stop(super::video::LOCAL_SCREEN);
                        drop(capture);
                        log_error("unpublishing voice video", media_request!(room.local_participant().unpublish_track(&sid)));
                    }
                    stop_share_audio!();
                    if video_cancelled(&early, true) {
                        emit(VoiceEvent::ScreenShare { active: false, error: None });
                        continue;
                    }
                    let source = super::screen::video_source(share_options);
                    let video_track = LocalVideoTrack::create_video_track("screen", RtcVideoSource::Native(source.clone()));
                    let (_, _, fps, bitrate) = share_options.quality.preset();
                    let options = TrackPublishOptions {
                        source: TrackSource::Screenshare,
                        video_encoding: Some(livekit::options::VideoEncoding { max_bitrate: bitrate, max_framerate: fps as f64 }),
                        simulcast: false,
                        ..Default::default()
                    };
                    match media_request!(room.local_participant().publish_track(LocalTrack::Video(video_track), options)) {
                        Ok(publication) if video_cancelled(&early, true) => {
                            log_error("unpublishing voice video", media_request!(room.local_participant().unpublish_track(&publication.sid())));
                            emit(VoiceEvent::ScreenShare { active: false, error: None });
                        }
                        Ok(publication) => {
                            let tx = share_end_tx.clone();
                            let capture_sid = publication.sid();
                            let capture = super::screen::start(source, share_options, move |why| {
                                // The call may have ended before capture reports stopping.
                                let _ = tx.send((capture_sid.clone(), why));
                            }, repaint.clone());
                            match capture {
                                Ok(capture) => {
                                    share = Some((publication.sid(), capture));
                                    emit(VoiceEvent::ScreenShare { active: true, error: None });
                                    share_audio = start_share_audio!();
                                }
                                Err(error) => {
                                    log_error("unpublishing voice video", media_request!(room.local_participant().unpublish_track(&publication.sid())));
                                    emit(VoiceEvent::ScreenShare { active: false, error: Some(format!("Couldn't start capture worker: {error}")) });
                                }
                            }
                        }
                        Err(e) => emit(VoiceEvent::ScreenShare { active: false, error: Some(format!("couldn't share your screen: {e}")) }),
                    }
                }
                Some(VoiceCmd::SetCamera { active: true, resolution }) if camera.as_ref().is_none_or(|(_, capture)| capture.stop_requested()) => {
                    if let Some((sid, capture)) = camera.take() {
                        disable_local_video(&room, &sid);
                        drop(capture);
                        log_error("unpublishing voice video", media_request!(room.local_participant().unpublish_track(&sid)));
                    }
                    if video_cancelled(&early, false) {
                        emit(VoiceEvent::Camera { active: false, error: None });
                        continue;
                    }
                    camera_resolution = resolution;
                    let source = super::camera::video_source(resolution);
                    let video_track = LocalVideoTrack::create_video_track("camera", RtcVideoSource::Native(source.clone()));
                    let (_, _, fps, bitrate) = super::camera::PRESET;
                    let options = TrackPublishOptions {
                        source: TrackSource::Camera,
                        video_encoding: Some(livekit::options::VideoEncoding { max_bitrate: bitrate, max_framerate: fps as f64 }),
                        ..Default::default()
                    };
                    match media_request!(room.local_participant().publish_track(LocalTrack::Video(video_track), options)) {
                        Ok(publication) if video_cancelled(&early, false) => {
                            log_error("unpublishing voice video", media_request!(room.local_participant().unpublish_track(&publication.sid())));
                            emit(VoiceEvent::Camera { active: false, error: None });
                        }
                        Ok(publication) => {
                            let tx = camera_end_tx.clone();
                            let capture_sid = publication.sid();
                            let capture = super::camera::start(source, settings.camera.clone(), resolution, move |why| {
                                // The call may have ended before capture reports stopping.
                                let _ = tx.send((capture_sid.clone(), why));
                            }, repaint.clone());
                            match capture {
                                Ok(capture) => {
                                    camera = Some((publication.sid(), capture));
                                    emit(VoiceEvent::Camera { active: true, error: None });
                                }
                                Err(error) => {
                                    log_error("unpublishing voice video", media_request!(room.local_participant().unpublish_track(&publication.sid())));
                                    emit(VoiceEvent::Camera { active: false, error: Some(format!("Couldn't start capture worker: {error}")) });
                                }
                            }
                        }
                        Err(e) => emit(VoiceEvent::Camera { active: false, error: Some(format!("couldn't turn on the camera: {e}")) }),
                    }
                }
                Some(VoiceCmd::SetCamera { active: on, .. }) => {
                    if !on && let Some((sid, capture)) = camera.take() {
                        disable_local_video(&room, &sid);
                        super::video::stop(super::video::LOCAL_CAMERA);
                        drop(capture);
                        log_error("unpublishing voice video", media_request!(room.local_participant().unpublish_track(&sid)));
                    }
                    emit(VoiceEvent::Camera { active: camera.is_some(), error: None });
                }
                Some(VoiceCmd::StopScreenShare) => {
                    if let Some((sid, capture)) = share.take() {
                        disable_local_video(&room, &sid);
                        super::video::stop(super::video::LOCAL_SCREEN);
                        drop(capture);
                        log_error("unpublishing voice video", media_request!(room.local_participant().unpublish_track(&sid)));
                    }
                    stop_share_audio!();
                    emit(VoiceEvent::ScreenShare { active: false, error: None });
                }
                Some(VoiceCmd::Leave) | None => break Ok(()),
            },
            Some((ended_sid, why)) = camera_end_rx.recv() => {
                if !camera.as_ref().is_some_and(|(sid, _)| sid == &ended_sid) { continue; }
                if let Some((sid, capture)) = camera.take() {
                    disable_local_video(&room, &sid);
                    super::video::stop(super::video::LOCAL_CAMERA);
                    drop(capture);
                    log_error("unpublishing voice video", media_request!(room.local_participant().unpublish_track(&sid)));
                }
                emit(VoiceEvent::Camera { active: false, error: Some(why) });
            }
            // The capture ended by itself (picker cancelled, capture failed).
            Some((ended_sid, why)) = share_end_rx.recv() => {
                if !share.as_ref().is_some_and(|(sid, _)| sid == &ended_sid) { continue; }
                if let Some((sid, capture)) = share.take() {
                    disable_local_video(&room, &sid);
                    super::video::stop(super::video::LOCAL_SCREEN);
                    drop(capture);
                    log_error("unpublishing voice video", media_request!(room.local_participant().unpublish_track(&sid)));
                }
                stop_share_audio!();
                emit(VoiceEvent::ScreenShare { active: false, error: Some(why) });
            }
            ev = events.recv() => {
                let Some(ev) = ev else { break Ok(()) };
                match ev {
                    RoomEvent::ParticipantConnected(_) => {
                        emit(VoiceEvent::Participants(participants(&room)));
                    }
                    RoomEvent::ParticipantDisconnected(participant) => {
                        let identity = participant.identity().to_string();
                        let prefix = format!("{identity}:");
                        players.retain(|key, player| {
                            if key.starts_with(&prefix) { player.abort(); false } else { true }
                        });
                        lock(&mixer).tracks.retain(|_, track| track.identity != identity);
                        lock(&loud).remove(&identity);
                        videos.retain(|video| {
                            if video.identity == identity { super::video::stop(&video.sid); false } else { true }
                        });
                        emit(VoiceEvent::Videos(videos.clone()));
                        emit(VoiceEvent::Participants(participants(&room)));
                    }
                    RoomEvent::TrackSubscribed { track: RemoteTrack::Audio(audio), publication, participant } => {
                        let key = format!("{}:{}", participant.identity(), publication.sid());
                        let screen_share = publication.source() == TrackSource::ScreenshareAudio;
                        let player = play(audio, participant.identity().to_string(), key.clone(), screen_share, mixer.clone(), loud.clone());
                        if let Some(old) = players.insert(key, player) {
                            old.abort();
                        }
                    }
                    RoomEvent::TrackSubscribed { track: RemoteTrack::Video(track), publication, participant } => {
                        if !watched.contains(&publication.sid().to_string()) {
                            // An earlier subscribe may complete after the view closed.
                            publication.set_subscribed(false);
                            continue;
                        }
                        let key = format!("{}:{}", participant.identity(), publication.sid());
                        let watcher = super::video::watch(track, publication.sid().to_string(), repaint.clone());
                        if let Some(old) = players.insert(key, watcher) {
                            old.abort();
                        }
                    }
                    RoomEvent::TrackUnsubscribed { publication, participant, .. } => {
                        let key = format!("{}:{}", participant.identity(), publication.sid());
                        if let Some(p) = players.remove(&key) {
                            p.abort();
                        }
                        lock(&mixer).tracks.remove(&key);
                        super::video::stop(&publication.sid().to_string());
                    }
                    RoomEvent::TrackPublished { publication, participant } => {
                        consider_publication(&publication, &participant, &mut videos, &watched);
                        emit(VoiceEvent::Videos(videos.clone()));
                    }
                    RoomEvent::TrackUnpublished { publication, participant, .. } => {
                        let sid = publication.sid().to_string();
                        let key = format!("{}:{sid}", participant.identity());
                        if let Some(player) = players.remove(&key) { player.abort(); }
                        lock(&mixer).tracks.remove(&key);
                        videos.retain(|v| v.sid != sid);
                        super::video::stop(&sid);
                        emit(VoiceEvent::Videos(videos.clone()));
                    }
                    // Only the microphone decides the muted icon: a camera or screen
                    // share pausing says nothing about whether someone can be heard.
                    RoomEvent::TrackMuted { participant, publication } if publication.source() == TrackSource::Microphone => {
                        emit(VoiceEvent::Muted { identity: participant.identity().to_string(), muted: true });
                    }
                    RoomEvent::TrackUnmuted { participant, publication } if publication.source() == TrackSource::Microphone => {
                        emit(VoiceEvent::Muted { identity: participant.identity().to_string(), muted: false });
                    }
                    RoomEvent::Disconnected { reason } => break Err(anyhow!("disconnected by server: {reason:?}")),
                    _ => {}
                }
            }
        }
    };

    // Close every outgoing media gate immediately. A capture worker can take
    // up to its bounded stop timeout; audio/video must not continue during it.
    track.disable();
    if let Some((sid, _)) = &camera {
        disable_local_video(&room, sid);
    }
    if let Some((sid, _)) = &share {
        disable_local_video(&room, sid);
    }
    drop(mic);
    drop(mic_tx);
    drop(output);
    join_engine(engine);
    super::video::stop(super::video::LOCAL_CAMERA);
    super::video::stop(super::video::LOCAL_SCREEN);
    drop(camera.take());
    drop(share.take());
    drop(share_audio.take());
    for p in players.into_values() {
        p.abort();
    }
    for video in &videos {
        super::video::stop(&video.sid);
    }
    log_error("closing voice room", room.close().await);
    result
}

fn log_error<T, E: std::fmt::Display>(operation: &str, result: std::result::Result<T, E>) {
    if let Err(error) = result {
        eprintln!("error: {operation}: {error}");
    }
}

fn join_engine(engine: std::thread::JoinHandle<()>) {
    if engine.join().is_err() {
        eprintln!("error: voice processing thread panicked");
    }
}

#[derive(Default)]
struct ErrorLog(bool);
impl ErrorLog {
    // Audio callbacks run every 10 ms; report a failing stage once per engine
    // instead of flooding stderr while keeping the existing audio fallback.
    fn once<T, E: std::fmt::Display>(&mut self, operation: &str, result: std::result::Result<T, E>) {
        if result.is_err() && !self.0 {
            self.0 = true;
            log_error(operation, result);
        }
    }
}

fn disable_local_video(room: &Room, sid: &TrackSid) {
    if let Some(publication) = room.local_participant().track_publications().get(sid)
        && let Some(LocalTrack::Video(track)) = publication.track()
    {
        track.disable();
    }
}

fn set_watched(
    room: &Room,
    sids: &[String],
    players: &mut HashMap<String, tokio::task::JoinHandle<()>>,
    repaint: &Arc<dyn Fn() + Send + Sync>,
) {
    for p in room.remote_participants().values() {
        for (sid, publication) in p.track_publications() {
            if publication.kind() == TrackKind::Video {
                let sid = sid.to_string();
                let visible = sids.contains(&sid);
                if !visible {
                    // Release decoded frames now, without waiting for the
                    // server to acknowledge the unsubscribe.
                    super::video::stop(&sid);
                    let key = format!("{}:{sid}", p.identity());
                    if let Some(player) = players.remove(&key) {
                        player.abort();
                    }
                } else {
                    // The view can reopen before the unsubscribe is
                    // acknowledged, while the SDK still has the track.
                    let key = format!("{}:{sid}", p.identity());
                    if !players.contains_key(&key)
                        && let Some(RemoteTrack::Video(track)) = publication.track()
                    {
                        players.insert(key, super::video::watch(track, sid, repaint.clone()));
                    }
                }
                publication.set_subscribed(visible);
            }
        }
    }
}

/// Reassert the media gate as well as the signalling flag. LiveKit's mute is
/// a no-op if the flag already matches, while publication enables the track.
fn set_microphone_muted(track: &LocalAudioTrack, muted: bool) {
    if muted {
        track.mute();
        track.disable();
    } else {
        track.unmute();
        track.enable();
    }
}

/// Controls queued before publication must settle once, without replaying an
/// earlier unmute after a later mute. Other commands retain their order.
#[derive(Default)]
struct InitialControls {
    muted: bool,
    deafened: bool,
}

impl InitialControls {
    fn take(&mut self, early: &mut VecDeque<VoiceCmd>) {
        early.retain(|cmd| match cmd {
            VoiceCmd::SetMuted(muted) => {
                self.muted = *muted;
                false
            }
            VoiceCmd::SetDeafened(deafened) => {
                self.deafened = *deafened;
                false
            }
            _ => true,
        });
    }
}

/// A stalled network must not accumulate every slider move or toggle. These
/// commands set independent preferences; only the newest of each is needed.
fn defer(early: &mut VecDeque<VoiceCmd>, cmd: VoiceCmd) {
    early.retain(|old| {
        !matches!(
            (old, &cmd),
            (VoiceCmd::SetMuted(_), VoiceCmd::SetMuted(_))
                | (VoiceCmd::SetDeafened(_), VoiceCmd::SetDeafened(_))
                | (VoiceCmd::Update(_), VoiceCmd::Update(_))
                | (VoiceCmd::Watch(_), VoiceCmd::Watch(_))
                | (VoiceCmd::SetCamera { .. }, VoiceCmd::SetCamera { .. })
                | (
                    VoiceCmd::StartScreenShare(_) | VoiceCmd::StopScreenShare,
                    VoiceCmd::StartScreenShare(_) | VoiceCmd::StopScreenShare
                )
        )
    });
    early.push_back(cmd);
}

/// Connection and publication both await the network. Cancellation wins when
/// it and completion are ready together, and dropping the operation abandons
/// its wait. The caller closes an established room and releases capture.
async fn until_leave<F: std::future::Future>(
    operation: F,
    cmds: &mut mpsc::UnboundedReceiver<VoiceCmd>,
    early: &mut VecDeque<VoiceCmd>,
) -> Option<F::Output> {
    until_leave_with(operation, cmds, early, |_| false).await
}

async fn until_leave_with<F: std::future::Future>(
    operation: F,
    cmds: &mut mpsc::UnboundedReceiver<VoiceCmd>,
    early: &mut VecDeque<VoiceCmd>,
    mut handle: impl FnMut(&VoiceCmd) -> bool,
) -> Option<F::Output> {
    early.retain(|cmd| !handle(cmd));
    tokio::pin!(operation);
    loop {
        tokio::select! {
            biased;
            cmd = cmds.recv() => match cmd {
                Some(VoiceCmd::Leave) | None => return None,
                Some(cmd) => { if !handle(&cmd) { defer(early, cmd); } }
            },
            result = &mut operation => return Some(result),
        }
    }
}

fn video_cancelled(early: &VecDeque<VoiceCmd>, screen: bool) -> bool {
    early
        .iter()
        .rev()
        .find_map(|cmd| match (screen, cmd) {
            (false, VoiceCmd::SetCamera { active, .. }) => Some(!active),
            (true, VoiceCmd::StopScreenShare) => Some(true),
            (true, VoiceCmd::StartScreenShare(_)) => Some(false),
            _ => None,
        })
        .unwrap_or(false)
}

/// Whether a Leave is waiting (or the sender is gone). Other waiting commands
/// move to `early`, in order, to run once the call is up.
fn leave_requested(cmds: &mut mpsc::UnboundedReceiver<VoiceCmd>, early: &mut VecDeque<VoiceCmd>) -> bool {
    loop {
        match cmds.try_recv() {
            Ok(VoiceCmd::Leave) | Err(mpsc::error::TryRecvError::Disconnected) => return true,
            Ok(cmd) => defer(early, cmd),
            Err(mpsc::error::TryRecvError::Empty) => return false,
        }
    }
}

/// The next command: those that arrived while connecting first.
async fn next_cmd(early: &mut VecDeque<VoiceCmd>, cmds: &mut mpsc::UnboundedReceiver<VoiceCmd>) -> Option<VoiceCmd> {
    match early.pop_front() {
        Some(cmd) => Some(cmd),
        None => cmds.recv().await,
    }
}

/// Key for our own microphone in the loudness map.
pub(crate) const LOCAL: &str = "\u{0}local";

pub(crate) type Loudness = Arc<Mutex<HashMap<String, Instant>>>;

/// Roughly speech level: above −40 dBFS RMS.
pub(crate) fn is_speech(frame: &[i16]) -> bool {
    let sum: f64 = frame.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum();
    (sum / frame.len().max(1) as f64).sqrt() > 0.01
}

fn to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

/// The processing thread: owns WebRTC's audio processing module (not
/// thread-safe), runs the echo reference and mic frames through it, then our
/// chain, then hands frames to LiveKit.
pub(crate) fn start_engine(
    source: NativeAudioSource,
    chain: Arc<AtomicU32>,
    input_gain: Arc<AtomicU32>,
    mic_rx: mpsc::UnboundedReceiver<Vec<f32>>,
    render_rx: std::sync::mpsc::Receiver<Vec<i16>>,
    loud: Loudness,
) -> Result<std::thread::JoinHandle<()>> {
    let mut errors = ErrorLog::default();
    let sink = move |frame: &[i16]| {
        let af = AudioFrame {
            data: Cow::Borrowed(frame),
            sample_rate: SAMPLE_RATE,
            num_channels: 1,
            samples_per_channel: FRAME as u32,
        };
        // Fast path (no queue): completes immediately.
        errors.once(
            "feeding microphone audio",
            futures_executor::block_on(source.capture_frame(&af)),
        );
    };
    start_engine_with(Box::new(sink), chain, input_gain, mic_rx, render_rx, loud)
}

/// Where the processing thread hands each processed 10 ms frame.
pub(crate) type FrameSink = Box<dyn FnMut(&[i16]) + Send>;

/// The processing thread as [`start_engine`], handing each processed 10 ms
/// frame (48 kHz mono) to `sink` instead: for transports that encode the
/// audio themselves (Discord's voice servers take raw Opus over UDP).
pub(crate) fn start_engine_with(
    mut sink: FrameSink,
    chain: Arc<AtomicU32>,
    input_gain: Arc<AtomicU32>,
    mut mic_rx: mpsc::UnboundedReceiver<Vec<f32>>,
    render_rx: std::sync::mpsc::Receiver<Vec<i16>>,
    loud: Loudness,
) -> Result<std::thread::JoinHandle<()>> {
    let handle = std::thread::Builder::new().name("voice-dsp".into()).spawn(move || {
        let mut current = Chain::unpack(chain.load(Ordering::Relaxed));
        let mut apm = build_apm(current);
        let mut reverse_errors = ErrorLog::default();
        let mut processing_errors = ErrorLog::default();
        let mut processor = Processor::new(current.mode, 1.0);
        let mut pending: Vec<f32> = Vec::with_capacity(FRAME * 2);
        let mut processed: Vec<f32> = Vec::with_capacity(FRAME * 2);
        let mut out: Vec<i16> = Vec::with_capacity(FRAME);

        while let Some(mut chunk) = mic_rx.blocking_recv() {
            // Never let a backlog build up: skip to the newest audio instead of sending it late.
            while mic_rx.len() > 3 {
                if let Ok(newer) = mic_rx.try_recv() {
                    chunk = newer;
                }
            }
            // A changed noise mode, echo or gain setting rebuilds only this
            // chain, between two chunks; capture, output and the call go on.
            let wanted = Chain::unpack(chain.load(Ordering::Relaxed));
            if wanted != current {
                current = wanted;
                apm = build_apm(current);
                processor = Processor::new(current.mode, 1.0);
                // Half-processed audio belongs to the old chain.
                pending.clear();
                out.clear();
            }
            while let Ok(mut reference) = render_rx.try_recv() {
                reverse_errors.once(
                    "processing speaker reference",
                    apm.process_reverse_stream(&mut reference, SAMPLE_RATE as i32, 1),
                );
            }
            pending.extend_from_slice(&chunk);
            while pending.len() >= FRAME {
                let mut frame: Vec<i16> = pending.drain(..FRAME).map(to_i16).collect();
                processing_errors.once(
                    "processing microphone audio",
                    apm.process_stream(&mut frame, SAMPLE_RATE as i32, 1),
                );
                processor.gain = f32::from_bits(input_gain.load(Ordering::Relaxed));
                let floats: Vec<f32> = frame.iter().map(|&s| s as f32 / 32768.0).collect();
                processed.clear();
                processor.process(&floats, &mut processed);
                for &s in &processed {
                    out.push(to_i16(s));
                    if out.len() == FRAME {
                        let voice = current.mode != NoiseSuppression::Enhanced || processor.voice_probability > 0.5;
                        if voice && is_speech(&out) {
                            lock(&loud).insert(LOCAL.to_string(), Instant::now());
                        }
                        sink(&out);
                        out.clear();
                    }
                }
            }
        }
    })?;
    Ok(handle)
}

/// What the processing thread rebuilds itself from: the settings that shape
/// the audio chain (not the volumes, which it reads every frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Chain {
    pub(crate) mode: NoiseSuppression,
    pub(crate) echo: bool,
    pub(crate) agc: bool,
}

impl Chain {
    pub(crate) fn of(s: &VoiceSettings) -> Self {
        Self {
            mode: s.noise_suppression,
            echo: s.echo_cancellation,
            agc: s.auto_gain_control,
        }
    }

    pub(crate) fn pack(self) -> u32 {
        let mode = match self.mode {
            NoiseSuppression::Disabled => 0,
            NoiseSuppression::WebRtc => 1,
            NoiseSuppression::Enhanced => 2,
        };
        mode | (u32::from(self.echo) << 2) | (u32::from(self.agc) << 3)
    }

    pub(crate) fn unpack(bits: u32) -> Self {
        let mode = match bits & 3 {
            0 => NoiseSuppression::Disabled,
            1 => NoiseSuppression::WebRtc,
            _ => NoiseSuppression::Enhanced,
        };
        Self {
            mode,
            echo: bits & 4 != 0,
            agc: bits & 8 != 0,
        }
    }
}

fn build_apm(chain: Chain) -> AudioProcessingModule {
    let mut apm = AudioProcessingModule::new(chain.echo, chain.agc, true, chain.mode == NoiseSuppression::WebRtc);
    // A starting hint; WebRTC's echo canceller estimates the real delay itself.
    log_error("setting echo cancellation delay", apm.set_stream_delay_ms(40));
    apm
}

/// What a settings change needs restarted in a running call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Restart {
    pub(crate) input: bool,
    pub(crate) output: bool,
    pub(crate) chain: bool,
}

pub(crate) fn restart_for(old: &VoiceSettings, new: &VoiceSettings) -> Restart {
    Restart {
        input: old.input_device != new.input_device,
        output: old.output_device != new.output_device,
        chain: Chain::of(old) != Chain::of(new),
    }
}

/// The devices of a call that has no room to manage them itself (Steam's
/// peer, Discord's raw audio): switched live when the settings change.
pub(crate) struct Devices {
    settings: VoiceSettings,
    mic: Option<Mic>,
    mic_tx: Option<mpsc::UnboundedSender<Vec<f32>>>,
    speakers: Option<Output>,
    render_tx: std::sync::mpsc::Sender<Vec<i16>>,
    mixer: Arc<Mutex<Mixer>>,
}

impl Devices {
    pub(crate) fn new(
        settings: &VoiceSettings,
        mic: Mic,
        mic_tx: mpsc::UnboundedSender<Vec<f32>>,
        speakers: Output,
        render_tx: std::sync::mpsc::Sender<Vec<i16>>,
        mixer: Arc<Mutex<Mixer>>,
    ) -> Self {
        Self {
            settings: settings.clone(),
            mic: Some(mic),
            mic_tx: Some(mic_tx),
            speakers: Some(speakers),
            render_tx,
            mixer,
        }
    }

    /// Switches to `new`'s devices; a device that won't open leaves the old one running.
    pub(crate) fn switch(&mut self, new: &VoiceSettings) {
        let restart = restart_for(&self.settings, new);
        if restart.output {
            drop(self.speakers.take());
            match Output::start(new.output_device.clone(), self.mixer.clone(), self.render_tx.clone()) {
                Ok(o) => self.speakers = Some(o),
                Err(e) => {
                    eprintln!("error: switching speakers: {e:#}");
                    match Output::start(
                        self.settings.output_device.clone(),
                        self.mixer.clone(),
                        self.render_tx.clone(),
                    ) {
                        Ok(o) => self.speakers = Some(o),
                        Err(error) => log_error("restoring speakers", Err::<(), _>(error)),
                    }
                }
            }
        }
        if restart.input
            && let Some(tx) = self.mic_tx.clone()
        {
            match Mic::start(new.input_device.clone(), tx) {
                Ok(m) => self.mic = Some(m),
                Err(e) => eprintln!("error: switching microphone: {e:#}"),
            }
        }
        // A device that failed to open keeps its old name, so a later change retries from the right state.
        if self.speakers.is_some() && restart.output {
            self.settings.output_device = new.output_device.clone();
        }
        if restart.input && self.mic.is_some() {
            self.settings.input_device = new.input_device.clone();
        }
        self.settings.noise_suppression = new.noise_suppression;
        self.settings.echo_cancellation = new.echo_cancellation;
        self.settings.auto_gain_control = new.auto_gain_control;
    }

    /// Stops capture and ends the processing thread's input.
    pub(crate) fn close_input(&mut self) {
        drop(self.mic.take());
        drop(self.mic_tx.take());
    }

    pub(crate) fn close_output(&mut self) {
        drop(self.speakers.take());
    }
}

/// Per-participant playback buffers and gains, mixed by the output stream.
#[derive(Default)]
pub(crate) struct Mixer {
    /// Keyed by participant identity + track sid.
    pub(crate) tracks: HashMap<String, TrackBuffer>,
    user_gain: HashMap<String, f32>,
    /// Screen share audio, from its own volume and mute.
    settings: VoiceSettings,
    output: f32,
    pub(crate) deafened: bool,
}

#[derive(Default)]
pub(crate) struct TrackBuffer {
    identity: String,
    /// A screen share's audio rather than the microphone.
    screen_share: bool,
    /// Heard until muted (Discord's streams), rather than muted until
    /// unmuted (Stoat's screen shares, as officially).
    heard: bool,
    samples: VecDeque<f32>,
    /// Waits for a little audio before playing, so network jitter doesn't crackle.
    primed: bool,
}

/// Start playing a track once this much (20 ms) is buffered.
const PRIME: usize = FRAME * 2;
/// Never hold more than 100 ms per participant: drop the oldest instead.
const MAX_BUFFER: usize = FRAME * 10;

impl Mixer {
    /// Queues `samples` (48 kHz mono) to play as track `key` (the mic test's own voice).
    pub(crate) fn feed(&mut self, key: &str, samples: &[f32]) {
        let t = self.tracks.entry(key.to_owned()).or_insert_with(|| TrackBuffer {
            identity: key.to_owned(),
            ..Default::default()
        });
        t.samples.extend(samples.iter().copied());
        let excess = t.samples.len().saturating_sub(MAX_BUFFER * 8);
        t.samples.drain(..excess);
    }

    pub(crate) fn apply(&mut self, s: &VoiceSettings) {
        self.output = s.output_volume;
        self.user_gain = s.user_volumes.clone();
        for u in &s.muted_users {
            self.user_gain.insert(u.clone(), 0.0);
        }
        self.settings = s.clone();
    }

    /// A track's gain before the output volume: the person's, or their screen share's.
    fn gain(user_gain: &HashMap<String, f32>, settings: &VoiceSettings, t: &TrackBuffer) -> f32 {
        if t.screen_share {
            let muted = match settings.screen_share_mutes.get(&t.identity) {
                Some(muted) => *muted,
                None => !t.heard,
            };
            if muted {
                0.0
            } else {
                settings.screen_share_volume(&t.identity)
            }
        } else {
            user_gain.get(&t.identity).copied().unwrap_or(1.0)
        }
    }

    /// Queues a track's next samples (48 kHz mono) under `key`, keeping at
    /// most [`MAX_BUFFER`] so a stall never turns into lasting delay.
    pub(crate) fn push(&mut self, key: &str, identity: &str, screen_share: bool, samples: impl Iterator<Item = f32>) {
        let t = self.tracks.entry(key.to_owned()).or_insert_with(|| TrackBuffer {
            identity: identity.to_owned(),
            screen_share,
            ..Default::default()
        });
        t.samples.extend(samples);
        let excess = t.samples.len().saturating_sub(MAX_BUFFER);
        t.samples.drain(..excess);
    }

    /// As [`Self::push`] for a screen share that's heard until muted.
    pub(crate) fn push_heard(&mut self, key: &str, identity: &str, samples: impl Iterator<Item = f32>) {
        self.tracks.entry(key.to_owned()).or_insert_with(|| TrackBuffer {
            identity: identity.to_owned(),
            screen_share: true,
            heard: true,
            ..Default::default()
        });
        self.push(key, identity, true, samples);
    }

    /// Samples waiting to play under `key`.
    pub(crate) fn queued(&self, key: &str) -> usize {
        self.tracks.get(key).map_or(0, |t| t.samples.len())
    }

    /// Mixes the next `out.len()` samples (48 kHz mono).
    fn mix(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        for t in self.tracks.values_mut() {
            if !t.primed {
                if t.samples.len() < PRIME {
                    continue;
                }
                t.primed = true;
            }
            let gain = if self.deafened {
                0.0
            } else {
                self.output * Self::gain(&self.user_gain, &self.settings, t)
            };
            for o in out.iter_mut() {
                match t.samples.pop_front() {
                    Some(s) => *o += s * gain,
                    None => {
                        t.primed = false;
                        break;
                    }
                }
            }
        }
        for o in out.iter_mut() {
            *o = o.clamp(-1.0, 1.0);
        }
    }
}

/// Receives one remote audio track into the mixer (and measures it for the speaking ring).
/// Subscribes to audio straight away; records video (and subscribes only if
/// the UI is already showing it).
fn consider_publication(
    publication: &RemoteTrackPublication,
    participant: &RemoteParticipant,
    videos: &mut Vec<VideoTrackInfo>,
    watched: &[String],
) {
    let sid = publication.sid().to_string();
    match publication.kind() {
        TrackKind::Audio => publication.set_subscribed(true),
        TrackKind::Video => {
            if !videos.iter().any(|v| v.sid == sid) {
                videos.push(VideoTrackInfo {
                    sid: sid.clone(),
                    identity: participant.identity().to_string(),
                    screen_share: publication.source() == TrackSource::Screenshare,
                });
            }
            publication.set_subscribed(watched.contains(&sid));
        }
    }
}

fn play(
    track: RemoteAudioTrack,
    identity: String,
    key: String,
    screen_share: bool,
    mixer: Arc<Mutex<Mixer>>,
    loud: Loudness,
) -> tokio::task::JoinHandle<()> {
    play_rtc(track.rtc_track(), identity, key, screen_share, mixer, loud)
}

/// Plays a WebRTC audio track (from LiveKit or a plain peer connection)
/// through the mixer, keyed by `key`; `identity` lights the speaking ring.
pub(crate) fn play_rtc(
    track: livekit::webrtc::audio_track::RtcAudioTrack,
    identity: String,
    key: String,
    screen_share: bool,
    mixer: Arc<Mutex<Mixer>>,
    loud: Loudness,
) -> tokio::task::JoinHandle<()> {
    use futures_util::StreamExt;
    use livekit::webrtc::audio_stream::native::NativeAudioStream;
    tokio::spawn(async move {
        let mut stream = NativeAudioStream::new(track, SAMPLE_RATE as i32, 1);
        while let Some(frame) = stream.next().await {
            // Only the microphone lights the speaking ring.
            if !screen_share && is_speech(&frame.data) {
                lock(&loud).insert(identity.clone(), Instant::now());
            }
            lock(&mixer).push(
                &key,
                &identity,
                screen_share,
                frame.data.iter().map(|&s| s as f32 / 32768.0),
            );
        }
    })
}

/// Holds a cpal stream on its own thread (streams aren't `Send` everywhere)
/// until dropped.
struct StreamThread {
    stop: Option<std::sync::mpsc::Sender<()>>,
}

impl StreamThread {
    fn spawn(name: &str, open: impl FnOnce() -> Result<cpal::Stream> + Send + 'static) -> Result<Self> {
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        std::thread::Builder::new().name(name.into()).spawn(move || {
            let stream = match open() {
                Ok(s) => s,
                Err(e) => {
                    // Startup may have been cancelled by the caller.
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            // A dropped caller or stop sender also requests normal teardown.
            let _ = ready_tx.send(Ok(()));
            let _ = stop_rx.recv();
            drop(stream);
        })?;
        ready_rx.recv().context("audio thread exited")??;
        Ok(Self { stop: Some(stop_tx) })
    }
}

impl Drop for StreamThread {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            // The device thread may already have exited.
            let _ = stop.send(());
        }
    }
}

pub(crate) struct Mic(#[allow(dead_code)] StreamThread);

impl Mic {
    pub(crate) fn start(device: Option<String>, tx: mpsc::UnboundedSender<Vec<f32>>) -> Result<Self> {
        Ok(Self(StreamThread::spawn("mic-capture", move || {
            open_input(device.as_deref(), tx)
        })?))
    }
}

pub(crate) struct Output(#[allow(dead_code)] StreamThread);

impl Output {
    pub(crate) fn start(
        device: Option<String>,
        mixer: Arc<Mutex<Mixer>>,
        render_tx: std::sync::mpsc::Sender<Vec<i16>>,
    ) -> Result<Self> {
        Ok(Self(StreamThread::spawn("voice-output", move || {
            open_output(device.as_deref(), mixer, render_tx)
        })?))
    }
}

pub(crate) fn find_device(name: Option<&str>, input: bool) -> Option<cpal::Device> {
    let host = audio_host();
    let named = name.and_then(|n| {
        let mut devices = if input {
            host.input_devices().ok()?
        } else {
            host.output_devices().ok()?
        };
        devices.find(|d| d.description().is_ok_and(|desc| desc.name() == n))
    });
    // Fall back to the default device if the saved one is gone.
    named.or_else(|| {
        if input {
            host.default_input_device()
        } else {
            host.default_output_device()
        }
    })
}

/// Opens the microphone and sends 48 kHz mono chunks to `tx`.
fn open_input(name: Option<&str>, tx: mpsc::UnboundedSender<Vec<f32>>) -> Result<cpal::Stream> {
    let device = find_device(name, true).context("no microphone found")?;
    let config = device.default_input_config().context("reading microphone format")?;
    let channels = config.channels() as usize;
    let rate = config.sample_rate();
    let format = config.sample_format();
    // Ask for 10 ms fragments so audio arrives promptly; fall back to the
    // backend default if the device refuses.
    let mut low_latency: cpal::StreamConfig = config.into();
    low_latency.buffer_size = cpal::BufferSize::Fixed(rate / 100);
    let try_open = |cfg: cpal::StreamConfig, tx: mpsc::UnboundedSender<Vec<f32>>| {
        let mut resampler = Resampler::new(rate, SAMPLE_RATE);
        let err = |e: cpal::Error| eprintln!("error: microphone stream: {e}");
        macro_rules! build {
            ($t:ty, $to_f32:expr) => {
                device.build_input_stream(
                    cfg,
                    move |data: &[$t], _: &cpal::InputCallbackInfo| {
                        let mono: Vec<f32> = data
                            .chunks(channels)
                            .map(|f| f.iter().map(|&s| $to_f32(s)).sum::<f32>() / channels as f32)
                            .collect();
                        // The DSP receiver closes before capture finishes stopping.
                        let _ = tx.send(resampler.process(&mono));
                    },
                    err,
                    None,
                )
            };
        }
        match format {
            cpal::SampleFormat::F32 => build!(f32, |s: f32| s),
            cpal::SampleFormat::I16 => build!(i16, |s: i16| s as f32 / 32768.0),
            cpal::SampleFormat::U16 => build!(u16, |s: u16| (s as f32 - 32768.0) / 32768.0),
            cpal::SampleFormat::I32 => build!(i32, |s: i32| s as f32 / 2_147_483_648.0),
            other => return Err(anyhow!("unsupported microphone sample format {other:?}")),
        }
        .context("opening microphone")
    };
    let stream = match try_open(low_latency, tx.clone()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("voice: 10 ms microphone buffer refused ({e:#}), using default");
            try_open(config.into(), tx)?
        }
    };
    stream.play().context("starting microphone")?;
    Ok(stream)
}

/// Opens the speakers; mixes participants at 48 kHz, resamples to the device
/// rate, and sends every 10 ms of the 48 kHz mix to `render_tx` as the echo reference.
fn open_output(
    name: Option<&str>,
    mixer: Arc<Mutex<Mixer>>,
    render_tx: std::sync::mpsc::Sender<Vec<i16>>,
) -> Result<cpal::Stream> {
    let device = find_device(name, false).context("no speakers found")?;
    let config = device.default_output_config().context("reading speaker format")?;
    let channels = config.channels() as usize;
    let rate = config.sample_rate();
    let format = config.sample_format();
    let mut low_latency: cpal::StreamConfig = config.into();
    low_latency.buffer_size = cpal::BufferSize::Fixed(rate / 100);

    let try_open = |cfg: cpal::StreamConfig| {
        let mixer = mixer.clone();
        let render_tx = render_tx.clone();
        let mut resampler = Resampler::new(SAMPLE_RATE, rate);
        let mut ready: VecDeque<f32> = VecDeque::new();
        let mut block = vec![0.0f32; FRAME];
        // Produces `frames` mono samples at the device rate.
        let mut next = move |frames: usize, out: &mut Vec<f32>| {
            while ready.len() < frames {
                lock(&mixer).mix(&mut block);
                // Playback can finish a callback after the DSP receiver closes.
                let _ = render_tx.send(block.iter().map(|&s| to_i16(s)).collect());
                ready.extend(resampler.process(&block));
            }
            out.clear();
            out.extend(ready.drain(..frames));
        };
        let mut mono = Vec::new();
        let err = |e: cpal::Error| eprintln!("error: speaker stream: {e}");
        macro_rules! build {
            ($t:ty, $from_f32:expr) => {
                device.build_output_stream(
                    cfg,
                    move |data: &mut [$t], _: &cpal::OutputCallbackInfo| {
                        next(data.len() / channels, &mut mono);
                        for (frame, &s) in data.chunks_mut(channels).zip(&mono) {
                            frame.fill($from_f32(s));
                        }
                    },
                    err,
                    None,
                )
            };
        }
        match format {
            cpal::SampleFormat::F32 => build!(f32, |s: f32| s),
            cpal::SampleFormat::I16 => build!(i16, |s: f32| (s * 32767.0) as i16),
            cpal::SampleFormat::U16 => build!(u16, |s: f32| (s * 32767.0 + 32768.0) as u16),
            cpal::SampleFormat::I32 => build!(i32, |s: f32| (s as f64 * 2_147_483_647.0) as i32),
            other => return Err(anyhow!("unsupported speaker sample format {other:?}")),
        }
        .context("opening speakers")
    };
    let stream = match try_open(low_latency) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("voice: 10 ms speaker buffer refused ({e:#}), using default");
            try_open(config.into())?
        }
    };
    stream.play().context("starting speakers")?;
    Ok(stream)
}

/// On Linux, prefer cpal's PulseAudio backend: clean device names matching
/// the system settings (raw ALSA lists low-level duplicates).
fn audio_host() -> cpal::Host {
    #[cfg(target_os = "linux")]
    if let Ok(h) = cpal::host_from_id(cpal::HostId::PulseAudio) {
        return h;
    }
    cpal::default_host()
}

/// Input and output device names, as offered in settings.
#[derive(Debug, Clone, Default)]
pub struct AudioDevices {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// Cameras, listed alongside for the Voice & Video page.
    pub cameras: Vec<String>,
}

pub fn list_devices() -> Result<AudioDevices> {
    let host = audio_host();
    let names = |devices: Vec<cpal::Device>| -> Vec<String> {
        devices
            .iter()
            .filter_map(|d| d.description().ok().map(|desc| desc.name().to_string()))
            // PulseAudio "Monitor of …" sources are loopbacks of outputs, not microphones.
            .filter(|n| !n.starts_with("Monitor of "))
            .collect()
    };
    Ok(AudioDevices {
        inputs: names(host.input_devices().map(|d| d.collect()).unwrap_or_default()),
        outputs: names(host.output_devices().map(|d| d.collect()).unwrap_or_default()),
        cameras: super::camera::list(),
    })
}

/// Linear-interpolation resampler; enough for speech.
pub struct Resampler {
    step: f64,
    pos: f64,
    last: f32,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        Self {
            step: from as f64 / to as f64,
            pos: 0.0,
            last: 0.0,
        }
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if (self.step - 1.0).abs() < f64::EPSILON {
            return input.to_vec();
        }
        let mut out = Vec::with_capacity((input.len() as f64 / self.step) as usize + 1);
        // `pos` is measured from the sample before input[0] (kept in `last`).
        while self.pos < input.len() as f64 {
            let i = self.pos.floor() as usize;
            let frac = (self.pos - i as f64) as f32;
            let a = if i == 0 { self.last } else { input[i - 1] };
            let b = input[i];
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        self.pos -= input.len() as f64;
        if let Some(&l) = input.last() {
            self.last = l;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_round_trips_through_its_packed_form() {
        for mode in [
            NoiseSuppression::Disabled,
            NoiseSuppression::WebRtc,
            NoiseSuppression::Enhanced,
        ] {
            for (echo, agc) in [(false, false), (true, false), (false, true), (true, true)] {
                let chain = Chain { mode, echo, agc };
                assert_eq!(Chain::unpack(chain.pack()), chain);
            }
        }
    }

    #[test]
    fn restart_names_only_what_changed() {
        let old = VoiceSettings::default();
        assert_eq!(
            restart_for(&old, &old),
            Restart {
                input: false,
                output: false,
                chain: false
            }
        );

        let mut new = old.clone();
        new.noise_suppression = NoiseSuppression::WebRtc;
        assert_eq!(
            restart_for(&old, &new),
            Restart {
                input: false,
                output: false,
                chain: true
            }
        );

        let mut new = old.clone();
        new.input_device = Some("mic".into());
        assert_eq!(
            restart_for(&old, &new),
            Restart {
                input: true,
                output: false,
                chain: false
            }
        );

        let mut new = old.clone();
        new.output_device = Some("speakers".into());
        new.input_volume = 2.0;
        assert_eq!(
            restart_for(&old, &new),
            Restart {
                input: false,
                output: true,
                chain: false
            }
        );

        let mut new = old.clone();
        new.echo_cancellation = !old.echo_cancellation;
        assert!(restart_for(&old, &new).chain);
    }

    #[test]
    fn voice_task_can_be_spawned_on_the_network_runtime() {
        fn sendable(_: impl Send) {}
        let (_tx, rx) = mpsc::unbounded_channel();
        // Construct but never poll: this checks the backend's spawn contract
        // without connecting, opening devices or starting a runtime.
        sendable(run(
            "test".into(),
            "wss://example.invalid".into(),
            "fixture".into(),
            VoiceSettings::default(),
            rx,
            Arc::new(|_| {}),
        ));
    }

    #[test]
    fn muted_microphone_stays_disabled_after_the_sdk_enables_its_publication() {
        // A memory-fed source, with no capture device or network connection.
        let source = NativeAudioSource::new(AudioSourceOptions::default(), SAMPLE_RATE, 1, 0);
        let track = LocalAudioTrack::create_audio_track("test-microphone", RtcAudioSource::Native(source));
        set_microphone_muted(&track, true);
        assert!(track.is_muted() && !track.is_enabled());
        track.enable(); // LiveKit publish_track's final step.
        assert!(track.is_muted() && track.is_enabled());
        set_microphone_muted(&track, true);
        assert!(track.is_muted() && !track.is_enabled());
        set_microphone_muted(&track, false);
        assert!(!track.is_muted() && track.is_enabled());
    }

    #[test]
    fn initial_controls_keep_the_last_choice_without_replaying_obsolete_toggles() {
        let mut pending = VecDeque::from([
            VoiceCmd::SetMuted(true),
            VoiceCmd::SetMuted(false),
            VoiceCmd::SetCamera {
                active: true,
                resolution: [1280, 720],
            },
            VoiceCmd::SetDeafened(true),
            VoiceCmd::SetMuted(true),
            VoiceCmd::Watch(vec!["remote".into()]),
        ]);
        let mut controls = InitialControls::default();
        controls.take(&mut pending);
        assert!(controls.muted && controls.deafened);
        assert!(matches!(
            pending.pop_front(),
            Some(VoiceCmd::SetCamera { active: true, .. })
        ));
        assert!(matches!(pending.pop_front(), Some(VoiceCmd::Watch(ids)) if ids == ["remote"]));
        assert!(pending.is_empty());
        pending.push_back(VoiceCmd::SetDeafened(false));
        controls.take(&mut pending);
        assert!(
            controls.muted && !controls.deafened,
            "later publication commands preserve the earlier mute"
        );
    }

    #[test]
    fn pending_controls_are_bounded_and_the_last_screen_choice_wins() {
        let mut early = VecDeque::new();
        for i in 0..1000 {
            defer(&mut early, VoiceCmd::SetMuted(i % 2 == 0));
            defer(&mut early, VoiceCmd::SetDeafened(i % 2 == 0));
            defer(
                &mut early,
                VoiceCmd::SetCamera {
                    active: i % 2 == 0,
                    resolution: [1280, 720],
                },
            );
            defer(&mut early, VoiceCmd::Watch(vec![i.to_string()]));
            defer(&mut early, VoiceCmd::Update(Box::default()));
            defer(
                &mut early,
                VoiceCmd::StartScreenShare(super::super::screen::ScreenQuality::High.into()),
            );
            defer(&mut early, VoiceCmd::StopScreenShare);
        }
        assert_eq!(early.len(), 6);
        assert!(matches!(early.back(), Some(VoiceCmd::StopScreenShare)));
        let mut controls = InitialControls::default();
        controls.take(&mut early);
        assert!(!controls.muted && !controls.deafened);
        assert_eq!(early.len(), 4);
        assert!(
            early
                .iter()
                .any(|cmd| matches!(cmd, VoiceCmd::Watch(ids) if ids == &["999"]))
        );
    }

    #[tokio::test]
    async fn a_queued_leave_wins_over_ready_connection_or_publication() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(VoiceCmd::Leave).unwrap();
        let mut early = VecDeque::new();
        assert!(until_leave(std::future::ready(42), &mut rx, &mut early).await.is_none());
        assert!(early.is_empty());
    }

    #[tokio::test]
    async fn a_closed_command_channel_cancels_a_network_wait() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        drop(tx);
        assert!(
            until_leave(std::future::pending::<()>(), &mut rx, &mut VecDeque::new())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_leave_during_publication_drops_the_operation_and_preserves_controls() {
        use std::sync::atomic::AtomicBool;
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = Dropped(dropped.clone());
        let (started, ready) = tokio::sync::oneshot::channel();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sender = tokio::spawn(async move {
            ready.await.unwrap();
            tx.send(VoiceCmd::SetMuted(true)).unwrap();
            tx.send(VoiceCmd::Leave).unwrap();
        });
        let operation = async move {
            let _guard = guard;
            let _ = started.send(());
            std::future::pending::<()>().await;
        };
        let mut early = VecDeque::new();
        assert!(until_leave(operation, &mut rx, &mut early).await.is_none());
        sender.await.unwrap();
        assert!(dropped.load(Ordering::SeqCst));
        assert!(matches!(early.pop_front(), Some(VoiceCmd::SetMuted(true))));
    }

    #[tokio::test]
    async fn queued_mute_is_retained_when_publication_completes() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(VoiceCmd::SetMuted(true)).unwrap();
        let mut early = VecDeque::new();
        assert_eq!(until_leave(std::future::ready(42), &mut rx, &mut early).await, Some(42));
        let mut controls = InitialControls::default();
        controls.take(&mut early);
        assert!(controls.muted);
    }

    #[tokio::test]
    async fn active_controls_apply_before_media_negotiation_finishes() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (handled, saw_mute) = tokio::sync::oneshot::channel();
        let (finish, finished) = tokio::sync::oneshot::channel();
        let sender = tx.clone();
        let send = tokio::spawn(async move {
            sender.send(VoiceCmd::SetMuted(true)).unwrap();
            saw_mute.await.unwrap();
            finish.send(42).unwrap();
        });
        let mut handled = Some(handled);
        let mut seen = Vec::new();
        let mut early = VecDeque::from([VoiceCmd::SetMuted(false)]);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            until_leave_with(
                async { finished.await.unwrap() },
                &mut rx,
                &mut early,
                |cmd| match cmd {
                    VoiceCmd::SetMuted(value) => {
                        seen.push(*value);
                        if *value && let Some(handled) = handled.take() {
                            let _ = handled.send(());
                        }
                        true
                    }
                    _ => false,
                },
            ),
        )
        .await
        .unwrap();
        send.await.unwrap();
        assert_eq!(result, Some(42));
        assert_eq!(seen, [false, true]);
        assert!(
            early.is_empty(),
            "applied controls must not replay obsolete values afterwards"
        );
    }

    #[test]
    fn turning_video_off_during_publication_prevents_capture_start() {
        let mut early = VecDeque::new();
        defer(
            &mut early,
            VoiceCmd::SetCamera {
                active: false,
                resolution: [1280, 720],
            },
        );
        defer(&mut early, VoiceCmd::StopScreenShare);
        assert!(video_cancelled(&early, false));
        assert!(video_cancelled(&early, true));
        defer(
            &mut early,
            VoiceCmd::SetCamera {
                active: true,
                resolution: [1280, 720],
            },
        );
        assert!(!video_cancelled(&early, false));
        assert!(video_cancelled(&early, true));
    }

    #[test]
    fn resampler_rates() {
        let mut r = Resampler::new(44_100, 48_000);
        let total: usize = (0..100).map(|_| r.process(&[0.0; 441]).len()).sum();
        // 44_100 samples in should give ~48_000 out.
        assert!((total as i64 - 48_000).abs() <= 2, "got {total}");

        let mut same = Resampler::new(48_000, 48_000);
        assert_eq!(same.process(&[0.1, 0.2]), vec![0.1, 0.2]);
    }

    #[test]
    fn resampler_preserves_dc() {
        let mut r = Resampler::new(16_000, 48_000);
        let out = r.process(&[0.5; 160]);
        assert!(out[3..].iter().all(|v| (v - 0.5).abs() < 1e-6));
    }

    #[test]
    fn mixer_volumes_mute_and_jitter_buffer() {
        let mut m = Mixer::default();
        let mut s = VoiceSettings::default();
        s.user_volumes.insert("A".into(), 2.0);
        s.muted_users.push("B".into());
        s.output_volume = 0.5;
        m.apply(&s);
        for (key, id) in [("a", "A"), ("b", "B")] {
            m.tracks.insert(
                key.into(),
                TrackBuffer {
                    identity: id.into(),
                    samples: vec![0.25; PRIME].into(),
                    ..Default::default()
                },
            );
        }
        let mut out = vec![0.0; 4];
        m.mix(&mut out);
        // A: 0.25 × user 2.0 × output 0.5; B muted.
        assert!(out.iter().all(|v| (v - 0.25).abs() < 1e-6), "{out:?}");

        // Not enough buffered yet → silence rather than crackle.
        m.tracks.clear();
        m.tracks.insert(
            "c".into(),
            TrackBuffer {
                identity: "C".into(),
                samples: vec![0.5; 10].into(),
                ..Default::default()
            },
        );
        m.mix(&mut out);
        assert_eq!(out, vec![0.0; 4]);

        // A screen share's audio: muted until unmuted, then at its own volume.
        m.tracks.clear();
        let share = || TrackBuffer {
            identity: "A".into(),
            screen_share: true,
            samples: vec![0.25; PRIME].into(),
            ..Default::default()
        };
        m.tracks.insert("s".into(), share());
        m.mix(&mut out);
        assert_eq!(out, vec![0.0; 4]);
        s.screen_share_mutes.insert("A".into(), false);
        s.screen_share_volumes.insert("A".into(), 3.0);
        m.apply(&s);
        m.tracks.insert("s".into(), share());
        m.mix(&mut out);
        // 0.25 × share 3.0 × output 0.5, not A's voice volume.
        assert!(out.iter().all(|v| (v - 0.375).abs() < 1e-6), "{out:?}");

        m.deafened = true;
        m.tracks.insert(
            "a".into(),
            TrackBuffer {
                identity: "A".into(),
                samples: vec![0.25; PRIME].into(),
                ..Default::default()
            },
        );
        m.mix(&mut out);
        assert_eq!(out, vec![0.0; 4]);
    }
}
