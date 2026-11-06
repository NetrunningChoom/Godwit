// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The microphone test in Settings: the microphone through the same chain a
//! call uses (WebRTC's echo canceller and gain control, then our noise
//! suppression and compressor), played back half a second later, with its
//! level. It checks devices and processing on any platform without calling
//! anyone.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use livekit::webrtc::native::apm::AudioProcessingModule;
use tokio::sync::mpsc;

use crate::call::{Mic, Mixer, Output, VoiceSettings};
use crate::dsp::{FRAME, NoiseSuppression, Processor, SAMPLE_RATE};
use crate::sync::lock;

/// How long after speaking you hear yourself: long enough to listen, short
/// enough to keep talking.
const DELAY: usize = SAMPLE_RATE as usize / 2;
/// The mixer track the test plays as.
const TRACK: &str = "mic-test";

/// A running microphone test; stops when dropped.
pub struct MicTest {
    level: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    _mic: Mic,
    _output: Output,
}

impl MicTest {
    pub fn start(settings: &VoiceSettings) -> Result<Self> {
        let mixer = Arc::new(Mutex::new(Mixer::default()));
        lock(&mixer).apply(settings);
        let (render_tx, render_rx) = std::sync::mpsc::channel::<Vec<i16>>();
        let output = Output::start(settings.output_device.clone(), mixer.clone(), render_tx)?;
        let (mic_tx, mut mic_rx) = mpsc::unbounded_channel::<Vec<f32>>();
        let mic = Mic::start(settings.input_device.clone(), mic_tx)?;
        let level = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (mode, echo, agc, gain) = (
            settings.noise_suppression,
            settings.echo_cancellation,
            settings.auto_gain_control,
            settings.input_volume,
        );
        let (lvl, stopping) = (level.clone(), stop.clone());
        std::thread::Builder::new().name("mic-test".into()).spawn(move || {
            let mut apm = AudioProcessingModule::new(echo, agc, true, mode == NoiseSuppression::WebRtc);
            let _ = apm.set_stream_delay_ms(40);
            let mut processor = Processor::new(mode, gain);
            let mut pending: Vec<f32> = Vec::with_capacity(FRAME * 2);
            let mut processed: Vec<f32> = Vec::with_capacity(FRAME * 2);
            let mut delay: VecDeque<f32> = VecDeque::from(vec![0.0; DELAY]);
            while !stopping.load(Ordering::Relaxed) {
                let Some(chunk) = mic_rx.blocking_recv() else { break };
                while let Ok(mut reference) = render_rx.try_recv() {
                    let _ = apm.process_reverse_stream(&mut reference, SAMPLE_RATE as i32, 1);
                }
                pending.extend_from_slice(&chunk);
                while pending.len() >= FRAME {
                    let mut frame: Vec<i16> = pending.drain(..FRAME).map(to_i16).collect();
                    let _ = apm.process_stream(&mut frame, SAMPLE_RATE as i32, 1);
                    let floats: Vec<f32> = frame.iter().map(|&s| f32::from(s) / 32768.0).collect();
                    processed.clear();
                    processor.process(&floats, &mut processed);
                    if !processed.is_empty() {
                        lvl.store(peak(&processed).to_bits(), Ordering::Relaxed);
                    }
                    delay.extend(processed.iter().copied());
                    let ready: Vec<f32> = delay.drain(..delay.len().saturating_sub(DELAY)).collect();
                    lock(&mixer).feed(TRACK, &ready);
                }
            }
        })?;
        Ok(Self {
            level,
            stop,
            _mic: mic,
            _output: output,
        })
    }

    /// The level as a shared value (`f32` bits), for a UI timer to read.
    pub fn level_handle(&self) -> Arc<AtomicU32> {
        self.level.clone()
    }

    /// The microphone's level after processing, 0..1 (for a meter).
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed)).clamp(0.0, 1.0)
    }
}

impl Drop for MicTest {
    fn drop(&mut self) {
        // The thread ends with the microphone's stream (dropped with `_mic`).
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// A frame's peak on a speech-friendly scale: quiet speech shows, shouting fills it.
fn peak(samples: &[f32]) -> f32 {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak <= 0.0 {
        return 0.0;
    }
    // -60 dB → 0, 0 dB → 1.
    ((20.0 * peak.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_meter_reads_decibels() {
        assert_eq!(peak(&[0.0; 4]), 0.0);
        assert!((peak(&[1.0]) - 1.0).abs() < 1e-6);
        assert!((peak(&[0.001]) - 0.0).abs() < 1e-3, "-60 dB is the bottom");
        assert!(peak(&[0.1]) > 0.6 && peak(&[0.1]) < 0.7);
    }
}
