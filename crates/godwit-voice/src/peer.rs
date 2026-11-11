// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A WebRTC call straight to a voice server that isn't LiveKit (Steam's).
//!
//! One peer connection: our microphone goes through the same processing as
//! LiveKit calls (WebRTC's echo canceller, gain control, noise suppression)
//! and every remote audio track plays through the same mixer and speakers.
//! Signaling is the caller's job: offers and answers go in and out as SDP
//! text, and new remote tracks come back as [`PeerEvent::Track`] so the caller
//! can say whose they are.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context as _, Result, anyhow};
use livekit::webrtc::audio_source::AudioSourceOptions;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_track::RtcAudioTrack;
use livekit::webrtc::media_stream_track::MediaStreamTrack;
use livekit::webrtc::peer_connection::{IceConnectionState, OfferOptions, PeerConnection};
use livekit::webrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use livekit::webrtc::peer_connection_factory::{PeerConnectionFactory, RtcConfiguration};
use livekit::webrtc::session_description::{SdpType, SessionDescription};
use tokio::sync::mpsc;

use super::call::{Chain, Devices, LOCAL, Loudness, Mic, Mixer, Output, VoiceSettings, play_rtc, start_engine};
use super::dsp::SAMPLE_RATE;
use crate::sync::lock;

/// What the peer connection reports.
pub enum PeerEvent {
    /// The media path is up.
    Connected,
    /// The media path failed or closed.
    Lost,
    /// A remote audio track arrived; `streams` are its stream ids, `mid` its m-section.
    Track {
        track: RtcAudioTrack,
        streams: Vec<String>,
        mid: Option<String>,
    },
}

pub struct Peer {
    pc: PeerConnection,
    mic_track: RtcAudioTrack,
    mixer: Arc<Mutex<Mixer>>,
    loud: Loudness,
    input_gain: Arc<AtomicU32>,
    players: HashMap<String, tokio::task::JoinHandle<()>>,
    // Dropped in this order: capture stops, then the processing thread ends,
    // then the speakers close.
    devices: Mutex<Devices>,
    chain: Arc<AtomicU32>,
    engine: Option<std::thread::JoinHandle<()>>,
    _factory: PeerConnectionFactory,
}

impl Peer {
    /// Opens the speakers and microphone and an audio peer connection
    /// (send and receive). Call inside a tokio runtime.
    pub fn new(settings: &VoiceSettings, events: mpsc::UnboundedSender<PeerEvent>) -> Result<Self> {
        let factory = PeerConnectionFactory::default();
        // Steam's voice servers are ICE-lite with public addresses: no STUN/TURN.
        let pc = factory
            .create_peer_connection(RtcConfiguration::default())
            .map_err(|e| anyhow!("creating the connection: {e:?}"))?;

        let loud: Loudness = Arc::new(Mutex::new(HashMap::new()));
        let mixer = Arc::new(Mutex::new(Mixer::default()));
        lock(&mixer).apply(settings);
        let (render_tx, render_rx) = std::sync::mpsc::channel::<Vec<i16>>();
        let output = Output::start(settings.output_device.clone(), mixer.clone(), render_tx.clone())?;

        let source = NativeAudioSource::new(AudioSourceOptions::default(), SAMPLE_RATE, 1, 0);
        let input_gain = Arc::new(AtomicU32::new(settings.input_volume.to_bits()));
        let chain = Arc::new(AtomicU32::new(Chain::of(settings).pack()));
        let (mic_tx, mic_rx) = mpsc::unbounded_channel::<Vec<f32>>();
        let engine = start_engine(
            source.clone(),
            chain.clone(),
            input_gain.clone(),
            mic_rx,
            render_rx,
            loud.clone(),
        )?;
        let mic = Mic::start(settings.input_device.clone(), mic_tx.clone())?;

        let mic_track = factory.create_audio_track("microphone", source);
        pc.add_track(MediaStreamTrack::Audio(mic_track.clone()), &["godwit"])
            .map_err(|e| anyhow!("adding the microphone: {e:?}"))?;

        let ice = events.clone();
        pc.on_ice_connection_state_change(Some(Box::new(move |state| {
            let event = match state {
                IceConnectionState::Connected | IceConnectionState::Completed => PeerEvent::Connected,
                IceConnectionState::Failed | IceConnectionState::Closed => PeerEvent::Lost,
                _ => return,
            };
            let _ = ice.send(event);
        })));
        pc.on_track(Some(Box::new(move |event| {
            let MediaStreamTrack::Audio(track) = event.track else {
                return;
            };
            let _ = events.send(PeerEvent::Track {
                track,
                streams: event.streams.iter().map(|s| s.id()).collect(),
                mid: event.transceiver.mid(),
            });
        })));

        let devices_mixer = mixer.clone();
        Ok(Self {
            pc,
            mic_track,
            mixer,
            loud,
            input_gain,
            players: HashMap::new(),
            devices: Mutex::new(Devices::new(settings, mic, mic_tx, output, render_tx, devices_mixer)),
            chain,
            engine: Some(engine),
            _factory: factory,
        })
    }

    /// A new offer (not yet set as our local description).
    pub async fn offer(&self) -> Result<String> {
        let offer = self
            .pc
            .create_offer(OfferOptions {
                offer_to_receive_audio: true,
                ..OfferOptions::default()
            })
            .await
            .map_err(|e| anyhow!("creating an offer: {e:?}"))?;
        Ok(offer.to_string())
    }

    pub async fn set_local_offer(&self, sdp: &str) -> Result<()> {
        let desc = SessionDescription::parse(sdp, SdpType::Offer).map_err(|e| anyhow!("our offer: {e:?}"))?;
        self.pc
            .set_local_description(desc)
            .await
            .map_err(|e| anyhow!("setting our offer: {e:?}"))
    }

    pub async fn set_remote_answer(&self, sdp: &str) -> Result<()> {
        let desc = SessionDescription::parse(sdp, SdpType::Answer).map_err(|e| anyhow!("their answer: {e:?}"))?;
        self.pc
            .set_remote_description(desc)
            .await
            .map_err(|e| anyhow!("applying their answer: {e:?}"))
    }

    /// Applies the server's new offer and answers it locally.
    pub async fn apply_offer(&self, sdp: &str) -> Result<()> {
        let desc = SessionDescription::parse(sdp, SdpType::Offer).map_err(|e| anyhow!("their offer: {e:?}"))?;
        self.pc
            .set_remote_description(desc)
            .await
            .map_err(|e| anyhow!("applying their offer: {e:?}"))?;
        let answer = self
            .pc
            .create_answer(Default::default())
            .await
            .map_err(|e| anyhow!("answering: {e:?}"))?;
        self.pc
            .set_local_description(answer)
            .await
            .context("setting our answer")
            .map_err(|e| anyhow!("{e:#}"))
    }

    /// Plays a remote track; `identity` is who it is (for volumes and the speaking ring).
    pub fn play(&mut self, track: RtcAudioTrack, identity: String) {
        let key = format!("{identity}:{}", track.id());
        let player = play_rtc(
            track,
            identity,
            key.clone(),
            false,
            self.mixer.clone(),
            self.loud.clone(),
        );
        if let Some(old) = self.players.insert(key, player) {
            old.abort();
        }
    }

    /// One line of media counters for diagnosing a call: what we sent and
    /// received per stream (ssrc), and our microphone's level.
    pub async fn stats(&self) -> String {
        use livekit::webrtc::stats::RtcStats;
        let Ok(stats) = self.pc.get_stats().await else {
            return "no stats".to_owned();
        };
        let mut parts = Vec::new();
        for s in stats {
            match s {
                RtcStats::OutboundRtp(o) => parts.push(format!(
                    "sent ssrc {} {} packets/{} bytes",
                    o.stream.ssrc, o.sent.packets_sent, o.sent.bytes_sent
                )),
                RtcStats::InboundRtp(i) => parts.push(format!(
                    "received ssrc {} {} packets",
                    i.stream.ssrc, i.received.packets_received
                )),
                RtcStats::MediaSource(m) => parts.push(format!("mic level {:.3}", m.audio.audio_level)),
                _ => {}
            }
        }
        if parts.is_empty() {
            "no media streams".to_owned()
        } else {
            parts.join(", ")
        }
    }

    /// Who sounded like speech in the last 300 ms: `(we did, their identities)`.
    pub fn speaking(&self) -> (bool, Vec<String>) {
        let now = Instant::now();
        let loud = lock(&self.loud);
        let recent = |t: &Instant| now.duration_since(*t).as_millis() < 300;
        let me = loud.get(LOCAL).is_some_and(recent);
        let mut others: Vec<String> = loud
            .iter()
            .filter(|(id, t)| id.as_str() != LOCAL && recent(t))
            .map(|(id, _)| id.clone())
            .collect();
        others.sort();
        (me, others)
    }

    /// Muted: the track stays (Steam keeps our ssrc) but sends silence.
    pub fn set_muted(&self, muted: bool) {
        self.mic_track.set_enabled(!muted);
    }

    pub fn set_deafened(&self, deafened: bool) {
        lock(&self.mixer).deafened = deafened;
    }

    /// New settings mid-call: volumes, the noise mode and the devices all apply at once.
    pub fn update(&self, settings: &VoiceSettings) {
        lock(&self.mixer).apply(settings);
        self.input_gain
            .store(settings.input_volume.to_bits(), Ordering::Relaxed);
        self.chain.store(Chain::of(settings).pack(), Ordering::Relaxed);
        lock(&self.devices).switch(settings);
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.pc.close();
        for player in self.players.values() {
            player.abort();
        }
        // Closing the microphone channel ends the processing thread.
        lock(&self.devices).close_input();
        if let Some(engine) = self.engine.take() {
            let _ = engine.join();
        }
        lock(&self.devices).close_output();
    }
}
