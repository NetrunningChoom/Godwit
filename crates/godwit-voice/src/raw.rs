// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The call audio chain without a WebRTC transport, for networks whose voice
//! servers take Opus over their own UDP protocol (Discord).
//!
//! The same pieces as a LiveKit call (`call.rs`): the microphone through
//! WebRTC's processing and our chain, speakers fed by the mixer, the mix as
//! the echo reference. The difference is at the ends: processed microphone
//! audio collects in a buffer the caller takes 20 ms at a time to encode, and
//! decoded audio is handed in per speaker.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use tokio::sync::mpsc;

use crate::call::{LOCAL, Loudness, Mic, Mixer, Output, VoiceSettings, is_speech, start_engine_with};
use crate::sync::lock;

/// At most this much processed microphone audio waits to be taken (250 ms);
/// older audio is dropped, so a stall never becomes lasting delay.
const MOST_CAPTURED: usize = 48_000 / 4;
/// How long a voice counts as speaking after its last speech-level frame,
/// as the speaking ring holds it in LiveKit calls.
const SPEAKING_HOLD_MS: u128 = 300;
/// Silence queued ahead of a speaker starting: one 20 ms frame.
const CUSHION: usize = 960;

/// Microphone, processing and speakers for one call.
pub struct RawAudio {
    mixer: Arc<Mutex<Mixer>>,
    loud: Loudness,
    input_gain: Arc<AtomicU32>,
    captured: Arc<Mutex<VecDeque<f32>>>,
    // Dropped in this order: capture stops, then the processing thread ends,
    // then the speakers close.
    mic: Option<Mic>,
    mic_tx: Option<mpsc::UnboundedSender<Vec<f32>>>,
    engine: Option<std::thread::JoinHandle<()>>,
    output: Option<Output>,
}

impl RawAudio {
    /// Opens the speakers and the microphone with `settings`' devices.
    pub fn new(settings: &VoiceSettings) -> Result<Self> {
        let loud: Loudness = Arc::new(Mutex::new(Default::default()));
        let mixer = Arc::new(Mutex::new(Mixer::default()));
        lock(&mixer).apply(settings);
        let (render_tx, render_rx) = std::sync::mpsc::channel::<Vec<i16>>();
        let output = Output::start(settings.output_device.clone(), mixer.clone(), render_tx)?;

        let captured: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::with_capacity(MOST_CAPTURED)));
        let into = captured.clone();
        let sink = move |frame: &[i16]| {
            let mut buffer = lock(&into);
            buffer.extend(frame.iter().map(|&s| f32::from(s) / 32768.0));
            let excess = buffer.len().saturating_sub(MOST_CAPTURED);
            buffer.drain(..excess);
        };
        let input_gain = Arc::new(AtomicU32::new(settings.input_volume.to_bits()));
        let (mic_tx, mic_rx) = mpsc::unbounded_channel::<Vec<f32>>();
        let engine = start_engine_with(
            Box::new(sink),
            settings,
            input_gain.clone(),
            mic_rx,
            render_rx,
            loud.clone(),
        )?;
        let mic = Mic::start(settings.input_device.clone(), mic_tx.clone())?;
        Ok(Self {
            mixer,
            loud,
            input_gain,
            captured,
            mic: Some(mic),
            mic_tx: Some(mic_tx),
            engine: Some(engine),
            output: Some(output),
        })
    }

    /// Fills `out` with the oldest processed microphone audio (48 kHz mono),
    /// returning how many samples there were; the rest is left as it was.
    pub fn take(&self, out: &mut [f32]) -> usize {
        let mut buffer = lock(&self.captured);
        let count = buffer.len().min(out.len());
        for (o, s) in out.iter_mut().zip(buffer.drain(..count)) {
            *o = s;
        }
        count
    }

    /// Keeps only the newest `keep` samples of microphone audio: what's
    /// sent when the voice gate opens, so the start of a word isn't lost.
    pub fn keep_newest(&self, keep: usize) {
        let mut buffer = lock(&self.captured);
        let excess = buffer.len().saturating_sub(keep);
        buffer.drain(..excess);
    }

    /// Samples of microphone audio waiting.
    pub fn waiting(&self) -> usize {
        lock(&self.captured).len()
    }

    /// Plays someone's decoded audio (48 kHz mono); `who` keys their
    /// volume and lights their speaking ring.
    pub fn play(&self, who: &str, samples: &[i16]) {
        if is_speech(samples) {
            lock(&self.loud).insert(who.to_owned(), Instant::now());
        }
        let mut mixer = lock(&self.mixer);
        // Audio comes 20 ms at a time on the call's clock while the speakers
        // take it 10 ms at a time on theirs: starting from empty with 20 ms
        // of silence ahead keeps the two from running dry between packets.
        if mixer.queued(who) == 0 {
            mixer.push(who, who, false, std::iter::repeat_n(0.0, CUSHION));
        }
        mixer.push(who, who, false, samples.iter().map(|&s| f32::from(s) / 32768.0));
    }

    /// Plays the sound of `who`'s screen share (48 kHz mono) under their
    /// screen share volume and mute rather than their voice's, without
    /// lighting their speaking ring. `key` names the track apart from
    /// their voice.
    pub fn play_shared(&self, key: &str, who: &str, samples: &[i16]) {
        let mut mixer = lock(&self.mixer);
        // Discord's client plays a stream's sound from the start.
        if mixer.queued(key) == 0 {
            mixer.push_heard(key, who, std::iter::repeat_n(0.0, CUSHION));
        }
        mixer.push_heard(key, who, samples.iter().map(|&s| f32::from(s) / 32768.0));
    }

    /// Someone left: their buffer goes.
    pub fn forget(&self, who: &str) {
        lock(&self.mixer).tracks.remove(who);
        lock(&self.loud).remove(who);
    }

    /// Who sounded like speech in the last 300 ms: `(we did, the others)`.
    pub fn speaking(&self) -> (bool, Vec<String>) {
        let now = Instant::now();
        let loud = lock(&self.loud);
        let recent = |t: &Instant| now.duration_since(*t).as_millis() < SPEAKING_HOLD_MS;
        let me = loud.get(LOCAL).is_some_and(recent);
        let mut others: Vec<String> = loud
            .iter()
            .filter(|(id, t)| id.as_str() != LOCAL && recent(t))
            .map(|(id, _)| id.clone())
            .collect();
        others.sort();
        (me, others)
    }

    pub fn set_deafened(&self, deafened: bool) {
        lock(&self.mixer).deafened = deafened;
    }

    /// Volumes from new settings (devices stay as they were for this call).
    pub fn update(&self, settings: &VoiceSettings) {
        lock(&self.mixer).apply(settings);
        self.input_gain
            .store(settings.input_volume.to_bits(), Ordering::Relaxed);
    }
}

impl Drop for RawAudio {
    fn drop(&mut self) {
        // Closing the microphone channel ends the processing thread.
        drop(self.mic.take());
        drop(self.mic_tx.take());
        if let Some(engine) = self.engine.take() {
            let _ = engine.join();
        }
        drop(self.output.take());
    }
}
