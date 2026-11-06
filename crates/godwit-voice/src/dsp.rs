// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Microphone processing, mirroring the official client's Web Audio chain:
//! high-pass (50 Hz) → RNNoise → compressor (−3 dB, 20:1, with Chromium's
//! make-up gain) → input gain. Echo cancellation and auto gain run before
//! this, in WebRTC's audio processing (see `call.rs`), as the browser's
//! getUserMedia does for the official client.
//! Works on 48 kHz mono `f32` samples in `[-1, 1]`.

use std::collections::VecDeque;

use nnnoiseless::DenoiseState;
use serde::{Deserialize, Serialize};

pub const SAMPLE_RATE: u32 = 48_000;
/// RNNoise works on 10 ms frames.
pub const FRAME: usize = DenoiseState::FRAME_SIZE;

/// Same choices as the official Voice settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoiseSuppression {
    Disabled,
    /// WebRTC's built-in suppressor (the official client's "Browser" option).
    WebRtc,
    /// RNNoise, the official client's "Enhanced" option and default.
    Enhanced,
}

/// Second-order filter, transposed direct form II.
#[derive(Debug, Clone)]
pub struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Biquad {
    /// RBJ cookbook high-pass.
    pub fn highpass(sample_rate: f32, cutoff: f32, q: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * cutoff / sample_rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b: [(1.0 + cos) / 2.0 / a0, -(1.0 + cos) / a0, (1.0 + cos) / 2.0 / a0],
            a: [-2.0 * cos / a0, (1.0 - alpha) / a0],
            z: [0.0; 2],
        }
    }

    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// Hard-knee feed-forward compressor with an attack/release envelope.
#[derive(Debug, Clone)]
pub struct Compressor {
    threshold_db: f32,
    ratio: f32,
    attack: f32,
    release: f32,
    env_db: f32,
}

impl Compressor {
    pub fn new(sample_rate: f32, threshold_db: f32, ratio: f32, attack_s: f32, release_s: f32) -> Self {
        let coef = |t: f32| (-1.0 / (t * sample_rate)).exp();
        Self {
            threshold_db,
            ratio,
            attack: coef(attack_s),
            release: coef(release_s),
            env_db: -120.0,
        }
    }

    pub fn process(&mut self, x: f32) -> f32 {
        let level_db = 20.0 * x.abs().max(1e-6).log10();
        let coef = if level_db > self.env_db {
            self.attack
        } else {
            self.release
        };
        self.env_db = coef * self.env_db + (1.0 - coef) * level_db;
        let over = self.env_db - self.threshold_db;
        if over <= 0.0 {
            return x;
        }
        let reduction_db = over - over / self.ratio;
        x * 10f32.powf(-reduction_db / 20.0)
    }
}

/// Make-up gain Chromium's DynamicsCompressorNode applies automatically:
/// `(1 / full_range_gain)^0.6`. With a hard knee at −3 dB and 20:1, a 0 dB
/// input comes out at −2.85 dB, so this is about +1.7 dB.
const COMPRESSOR_MAKEUP: f32 = 1.218;

/// The whole chain. Buffers input into RNNoise frames, so output lags input by
/// up to one frame (10 ms) when noise suppression is on.
pub struct Processor {
    mode: NoiseSuppression,
    pub gain: f32,
    highpass: Biquad,
    compressor: Compressor,
    denoise: Box<DenoiseState<'static>>,
    pending: Vec<f32>,
    ready: VecDeque<f32>,
    frame_out: Vec<f32>,
    /// Voice-activity probability of the latest RNNoise frame, 0..1.
    pub voice_probability: f32,
}

impl Processor {
    pub fn new(mode: NoiseSuppression, gain: f32) -> Self {
        let sr = SAMPLE_RATE as f32;
        Self {
            mode,
            gain,
            highpass: Biquad::highpass(sr, 50.0, std::f32::consts::FRAC_1_SQRT_2),
            compressor: Compressor::new(sr, -3.0, 20.0, 0.003, 0.05),
            denoise: DenoiseState::new(),
            pending: Vec::with_capacity(FRAME),
            ready: VecDeque::new(),
            frame_out: vec![0.0; FRAME],
            voice_probability: 0.0,
        }
    }

    /// Feeds microphone samples in and appends processed samples to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.mode != NoiseSuppression::Enhanced {
            // Matches the official bypass: straight to the gain stage.
            out.extend(input.iter().map(|x| (x * self.gain).clamp(-1.0, 1.0)));
            return;
        }
        for &x in input {
            self.pending.push(self.highpass.process(x) * 32768.0);
            if self.pending.len() == FRAME {
                self.voice_probability = self.denoise.process_frame(&mut self.frame_out, &self.pending);
                self.pending.clear();
                for &y in &self.frame_out {
                    let y = self.compressor.process(y / 32768.0) * COMPRESSOR_MAKEUP * self.gain;
                    self.ready.push_back(y.clamp(-1.0, 1.0));
                }
            }
        }
        out.extend(self.ready.drain(..));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / SAMPLE_RATE as f32).sin())
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn highpass_cuts_rumble_keeps_voice() {
        let run = |freq| {
            let mut hp = Biquad::highpass(SAMPLE_RATE as f32, 50.0, std::f32::consts::FRAC_1_SQRT_2);
            let y: Vec<f32> = sine(freq, 0.5, 48_000).into_iter().map(|x| hp.process(x)).collect();
            rms(&y[24_000..]) / rms(&sine(freq, 0.5, 24_000))
        };
        assert!(run(10.0) < 0.1, "10 Hz should be strongly attenuated");
        assert!(run(1000.0) > 0.95, "1 kHz should pass");
    }

    #[test]
    fn compressor_only_touches_loud_signals() {
        let mut c = Compressor::new(SAMPLE_RATE as f32, -3.0, 20.0, 0.003, 0.05);
        let quiet: Vec<f32> = sine(440.0, 0.1, 4800).into_iter().map(|x| c.process(x)).collect();
        assert!((rms(&quiet) - rms(&sine(440.0, 0.1, 4800))).abs() < 1e-4);
        let loud: Vec<f32> = sine(440.0, 1.0, 9600).into_iter().map(|x| c.process(x)).collect();
        assert!(rms(&loud[4800..]) < 0.9 * rms(&sine(440.0, 1.0, 4800)));
    }

    #[test]
    fn enhanced_mode_suppresses_background_noise() {
        // Fan-like background: brown noise plus 50 Hz mains hum with harmonics.
        // (RNNoise barely touches synthetic *white* noise, which it reads as
        // voice, but real background noise is closer to this.)
        let mut seed = 0x1234_5678u32;
        let mut acc = 0.0f32;
        let noise: Vec<f32> = (0..96_000)
            .map(|i| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                acc = (acc + (seed as f32 / u32::MAX as f32 - 0.5) * 0.05) * 0.995;
                let t = i as f32 / SAMPLE_RATE as f32;
                let hum: f32 = (1..6)
                    .map(|h| 0.03 / h as f32 * (2.0 * std::f32::consts::PI * 50.0 * h as f32 * t).sin())
                    .sum();
                acc * 0.5 + hum
            })
            .collect();
        let mut p = Processor::new(NoiseSuppression::Enhanced, 1.0);
        let mut out = Vec::new();
        p.process(&noise, &mut out);
        assert_eq!(out.len(), noise.len() / FRAME * FRAME, "whole frames come out");
        // Skip the first half second while RNNoise adapts.
        assert!(
            rms(&out[24_000..]) < 0.1 * rms(&noise[24_000..out.len()]),
            "noise should drop by >20 dB"
        );
        assert!(p.voice_probability < 0.2, "background noise isn't voice");
    }

    #[test]
    fn disabled_mode_is_just_gain() {
        let mut p = Processor::new(NoiseSuppression::Disabled, 0.5);
        let mut out = Vec::new();
        p.process(&[0.2, -0.4], &mut out);
        assert_eq!(out, vec![0.1, -0.2]);
    }
}
