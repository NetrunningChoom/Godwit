// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Godwit's sounds, made from formulas instead of audio files: each is a few
//! sine tones with soft envelopes, computed at the speakers' own sample rate
//! (so nothing is resampled and nothing ships as an asset).
//!
//! A note is `a · env(t) · (sin 2πft + h · sin 4πft)`: the fundamental plus a
//! quieter octave for a bell-like colour. `env` rises over 6 ms (no click) and
//! then decays as `e^(−t/τ)`.

use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, StreamTrait};

use crate::call::find_device;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sound {
    /// A new message: two quick rising notes.
    Message,
    /// Someone is calling: a rising three-note phrase, twice, then a pause; loops.
    Ring,
    /// We (or someone) joined a call: a rising fifth.
    Join,
    /// We (or someone) left a call: a falling fifth.
    Leave,
    /// Our microphone went off: one short low note, falling.
    Mute,
    /// Our microphone came back: one short note, rising.
    Unmute,
    /// We stopped hearing the call: two low notes, falling.
    Deafen,
    /// We hear the call again: two notes, rising.
    Undeafen,
    /// We started watching a stream: three quick notes, rising.
    WatchStart,
    /// We stopped watching a stream: three quick notes, falling.
    WatchStop,
    /// Someone in the call went live, or stopped: one soft, high note.
    StreamChanged,
    /// A new message in the tone chosen in Settings.
    MessageTone(Tone),
}

/// The message sounds to choose from (Settings → Notifications).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Two quick rising high notes (the original).
    Chime,
    /// Two low, gentle notes.
    Soft,
    /// One short, round note.
    Pop,
    /// Two falling notes.
    Drop,
    /// Three quick notes up a chord, wooden and dry.
    Marimba,
    /// A short high trill.
    Bubble,
}

impl Tone {
    /// In Settings' order, with their names; the first is the default.
    pub const ALL: [(Tone, &'static str); 6] = [
        (Tone::Marimba, "Marimba"),
        (Tone::Chime, "Chime"),
        (Tone::Soft, "Soft"),
        (Tone::Pop, "Pop"),
        (Tone::Drop, "Drop"),
        (Tone::Bubble, "Bubble"),
    ];
}

/// One note of a sound.
#[derive(Clone, Copy)]
struct Note {
    /// When it starts, in seconds from the sound's start.
    at: f32,
    hz: f32,
    /// How long it sounds before being cut (with a short fade).
    len: f32,
    /// Decay time constant τ of `e^(−t/τ)`.
    decay: f32,
    amp: f32,
}

const ATTACK: f32 = 0.006;
const RELEASE: f32 = 0.012;
/// The octave's share: a little brightness without harshness.
const OCTAVE: f32 = 0.18;

// Equal-tempered pitches (A4 = 440 Hz).
const C5: f32 = 523.25;
const E5: f32 = 659.26;
const G5: f32 = 783.99;
const A4: f32 = 440.00;
const D5: f32 = 587.33;
const A5: f32 = 880.00;
const C6: f32 = 1046.50;
const E6: f32 = 1318.51;
const G6: f32 = 1567.98;
const A6: f32 = 1760.00;

impl Sound {
    fn notes(self) -> Vec<Note> {
        let n = |at, hz, len, decay, amp| Note {
            at,
            hz,
            len,
            decay,
            amp,
        };
        match self {
            Self::Message => vec![n(0.0, E6, 0.12, 0.05, 0.22), n(0.08, A6, 0.22, 0.08, 0.20)],
            Self::Ring => {
                let phrase = |start: f32| {
                    [
                        n(start, C5, 0.16, 0.10, 0.24),
                        n(start + 0.14, E5, 0.16, 0.10, 0.24),
                        n(start + 0.28, G5, 0.30, 0.16, 0.24),
                    ]
                };
                phrase(0.0).into_iter().chain(phrase(0.6)).collect()
            }
            Self::Join => vec![n(0.0, C5, 0.10, 0.06, 0.20), n(0.09, G5, 0.20, 0.09, 0.20)],
            Self::Leave => vec![n(0.0, G5, 0.10, 0.06, 0.20), n(0.09, C5, 0.20, 0.09, 0.20)],
            // Short and quiet: they answer a click, they don't announce.
            Self::Mute => vec![n(0.0, E5, 0.06, 0.03, 0.16), n(0.05, A4, 0.10, 0.05, 0.14)],
            Self::Unmute => vec![n(0.0, A4, 0.06, 0.03, 0.14), n(0.05, E5, 0.10, 0.05, 0.16)],
            Self::Deafen => vec![n(0.0, D5, 0.08, 0.04, 0.16), n(0.08, A4, 0.16, 0.07, 0.14)],
            Self::Undeafen => vec![n(0.0, A4, 0.08, 0.04, 0.14), n(0.08, D5, 0.16, 0.07, 0.16)],
            Self::WatchStart => vec![
                n(0.0, E5, 0.07, 0.04, 0.15),
                n(0.06, G5, 0.07, 0.04, 0.15),
                n(0.12, C6, 0.14, 0.06, 0.15),
            ],
            Self::WatchStop => vec![
                n(0.0, C6, 0.07, 0.04, 0.15),
                n(0.06, G5, 0.07, 0.04, 0.15),
                n(0.12, E5, 0.14, 0.06, 0.15),
            ],
            Self::StreamChanged => vec![n(0.0, A5, 0.12, 0.06, 0.12)],
            Self::MessageTone(Tone::Chime) => Self::Message.notes(),
            Self::MessageTone(Tone::Soft) => vec![n(0.0, A4, 0.22, 0.12, 0.18), n(0.12, D5, 0.32, 0.16, 0.16)],
            Self::MessageTone(Tone::Pop) => vec![n(0.0, A5, 0.07, 0.025, 0.30)],
            Self::MessageTone(Tone::Drop) => vec![n(0.0, A6, 0.10, 0.05, 0.20), n(0.08, E6, 0.20, 0.08, 0.18)],
            Self::MessageTone(Tone::Marimba) => vec![
                n(0.0, C5, 0.09, 0.04, 0.24),
                n(0.07, E5, 0.09, 0.04, 0.22),
                n(0.14, G5, 0.16, 0.06, 0.22),
            ],
            Self::MessageTone(Tone::Bubble) => vec![
                n(0.0, C6, 0.05, 0.03, 0.16),
                n(0.05, E6, 0.05, 0.03, 0.16),
                n(0.10, G6, 0.10, 0.05, 0.16),
            ],
        }
    }

    /// The whole sound's length; a ring includes its pause before repeating.
    fn length(self) -> f32 {
        match self {
            Self::Ring => 2.6,
            _ => self.notes().iter().map(|n| n.at + n.len + RELEASE).fold(0.0, f32::max),
        }
    }

    /// The sound as mono samples at `rate` Hz, between −1 and 1.
    pub fn samples(self, rate: u32) -> Vec<f32> {
        let rate = rate.max(1) as f32;
        let total = (self.length() * rate).ceil() as usize;
        let notes = self.notes();
        (0..total)
            .map(|i| {
                let t = i as f32 / rate;
                notes.iter().map(|n| note_at(n, t - n.at)).sum::<f32>().clamp(-1.0, 1.0)
            })
            .collect()
    }
}

/// A note's value `t` seconds after it starts.
fn note_at(n: &Note, t: f32) -> f32 {
    if t < 0.0 || t > n.len + RELEASE {
        return 0.0;
    }
    let rise = (t / ATTACK).min(1.0);
    let fall = if t > n.len { 1.0 - (t - n.len) / RELEASE } else { 1.0 };
    let env = rise * fall * (-t / n.decay).exp();
    n.amp * env * ((TAU * n.hz * t).sin() + OCTAVE * (2.0 * TAU * n.hz * t).sin())
}

/// A sound playing on its own thread; dropping it stops a ringing one.
pub struct Playing {
    stop: Arc<AtomicBool>,
}

impl Drop for Playing {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Plays `sound` once (a ring: until the returned handle is dropped) on
/// `device` (None: the default speakers) at `volume` (0–1, scaled down further
/// by the sound itself). Never blocks: opening the device happens on the thread.
pub fn play(sound: Sound, device: Option<String>, volume: f32) -> Result<Playing> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::Builder::new()
        .name("sound".into())
        .spawn(move || {
            if let Err(e) = run(sound, device.as_deref(), volume, &flag) {
                eprintln!("error: playing a sound: {e:#}");
            }
        })
        .map_err(|e| anyhow!("starting the sound thread: {e}"))?;
    Ok(Playing { stop })
}

fn run(sound: Sound, device: Option<&str>, volume: f32, stop: &Arc<AtomicBool>) -> Result<()> {
    let device = find_device(device, false).ok_or_else(|| anyhow!("no speakers"))?;
    let config = device.default_output_config()?;
    let rate = config.sample_rate();
    let channels = usize::from(config.channels()).max(1);
    let samples: Arc<Vec<f32>> = Arc::new(
        sound
            .samples(rate)
            .into_iter()
            .map(|s| s * volume.clamp(0.0, 1.0))
            .collect(),
    );
    let looping = sound == Sound::Ring;
    let mut pos = 0usize;
    // Set once the device has taken the last sample: the stream can start
    // late (PipeWire takes a while to open one), so a timer from `play`
    // cut the end of short tones off.
    let played = Arc::new(AtomicBool::new(false));
    // How far ahead of hearing the device takes samples, in ms, as it
    // reports it: a Bluetooth headset is hundreds of ms behind, and closing
    // the stream when the last sample was taken dropped the whole tone
    // still in its buffers.
    let behind = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let behind_cb = behind.clone();
    let mut next = {
        let samples = samples.clone();
        let stop = stop.clone();
        let played = played.clone();
        move || {
            if stop.load(Ordering::Acquire) || samples.is_empty() {
                return 0.0;
            }
            if pos >= samples.len() {
                if !looping {
                    played.store(true, Ordering::Release);
                    return 0.0;
                }
                pos = 0;
            }
            let s = samples[pos];
            pos += 1;
            s
        }
    };
    let err = |e: cpal::Error| eprintln!("error: sound stream: {e}");
    let cfg: cpal::StreamConfig = config.into();
    macro_rules! build {
        ($t:ty, $from_f32:expr) => {
            device.build_output_stream(
                cfg,
                move |data: &mut [$t], info: &cpal::OutputCallbackInfo| {
                    let t = info.timestamp();
                    let ahead = t.playback.duration_since(t.callback);
                    let ms = u64::try_from(ahead.as_millis()).unwrap_or(u64::MAX);
                    behind_cb.fetch_max(ms, Ordering::Relaxed);
                    for frame in data.chunks_mut(channels) {
                        frame.fill($from_f32(next()));
                    }
                },
                err,
                None,
            )
        };
    }
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => build!(i16, |s: f32| (s * 32767.0) as i16),
        cpal::SampleFormat::U16 => build!(u16, |s: f32| (s * 32767.0 + 32768.0) as u16),
        cpal::SampleFormat::I32 => build!(i32, |s: f32| (s as f64 * 2_147_483_647.0) as i32),
        other => return Err(anyhow!("unsupported speaker format {other:?}")),
    }?;
    stream.play()?;
    // A one-shot plays until the device has taken every sample, then a
    // little longer for its buffer; a ring runs until stopped. Checked often
    // so stopping a ring is immediate. The cap covers a device that never
    // starts.
    let cap = std::time::Instant::now() + Duration::from_secs_f32(sound.length() + 5.0);
    let mut drained: Option<std::time::Instant> = None;
    while !stop.load(Ordering::Acquire) {
        if !looping {
            if std::time::Instant::now() >= cap {
                break;
            }
            if played.load(Ordering::Acquire)
                && drained.get_or_insert_with(std::time::Instant::now).elapsed()
                    >= Duration::from_millis(250 + behind.load(Ordering::Relaxed).min(2000))
            {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sounds_stay_in_range_and_start_and_end_silently() {
        let tones = Tone::ALL.map(|(t, _)| Sound::MessageTone(t));
        for sound in [
            Sound::Message,
            Sound::Ring,
            Sound::Join,
            Sound::Leave,
            Sound::Mute,
            Sound::Unmute,
            Sound::Deafen,
            Sound::Undeafen,
            Sound::WatchStart,
            Sound::WatchStop,
            Sound::StreamChanged,
        ]
        .into_iter()
        .chain(tones)
        {
            let s = sound.samples(48_000);
            assert!(!s.is_empty());
            assert!(s.iter().all(|x| x.abs() <= 1.0), "{sound:?} clips");
            assert!(s.iter().any(|x| x.abs() > 0.05), "{sound:?} is audible");
            // No click: the first and last samples are (nearly) silent.
            assert!(s[0].abs() < 0.01 && s[s.len() - 1].abs() < 0.01, "{sound:?} clicks");
        }
    }

    #[test]
    fn length_follows_the_rate() {
        let a = Sound::Message.samples(48_000).len();
        let b = Sound::Message.samples(44_100).len();
        assert!((a as f32 / b as f32 - 48_000.0 / 44_100.0).abs() < 0.01);
    }

    #[test]
    fn a_note_has_its_pitch() {
        // Count zero crossings of a pure note over its first 50 ms: about 2·f·t.
        let n = Note {
            at: 0.0,
            hz: 440.0,
            len: 0.2,
            decay: 10.0,
            amp: 1.0,
        };
        let rate = 48_000.0;
        let mut crossings = 0;
        let mut last = 0.0f32;
        for i in 1..(0.05 * rate) as usize {
            let t = i as f32 / rate;
            // Without the octave: the fundamental alone.
            let v = (TAU * n.hz * t).sin() * (t / ATTACK).min(1.0);
            if last != 0.0 && v.signum() != last.signum() {
                crossings += 1;
            }
            last = v;
        }
        assert!((crossings - 44i32).abs() <= 2, "{crossings}");
        assert!(note_at(&n, -0.1) == 0.0 && note_at(&n, 1.0) == 0.0);
    }
}
