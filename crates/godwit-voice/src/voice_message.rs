// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Playing a voice message or audio attachment in the conversation view.
//!
//! The file is demuxed with symphonia (Ogg is already a dependency for the
//! soundboard). Ogg Opus, which is what voice messages are, stays as its
//! compressed packets (about 4 KB per second) and is decoded with the same
//! libopus the calls use, one 20 ms packet at a time, inside the speakers'
//! callback: nothing is decoded ahead and nothing runs while paused. Other
//! formats symphonia reads (Vorbis, MP3) are decoded whole to 16-bit mono,
//! within the soundboard's size cap. Speed is plain resampling (pitch moves
//! with it). One [`Player`] means one stream; dropping it closes the stream
//! and frees the clip.

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc;

use anyhow::{Result, anyhow, bail};
use cpal::traits::{DeviceTrait, StreamTrait};
use symphonia::core::codecs::CODEC_TYPE_OPUS;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::call::find_device;
use crate::opus::Decoder;

/// Opus always decodes at 48 kHz.
const RATE: u32 = 48_000;
/// The most one Opus packet holds: 120 ms.
const MAX_PACKET: usize = 5760;
/// The largest recording taken, in bytes (Discord's voice messages are tiny;
/// this only stops an audio attachment of any size filling memory).
pub const MAX_BYTES: usize = 24 * 1024 * 1024;

enum Source {
    /// Compressed packets, with the sample each starts at (before `skip`).
    Opus {
        packets: Vec<Box<[u8]>>,
        starts: Vec<u64>,
        total: u64,
    },
    /// Whole decoded sound, 48 kHz mono.
    Pcm(Vec<i16>),
}

/// A recording ready to play.
pub struct Clip {
    source: Source,
    /// Samples at the start the encoder says to drop (Opus pre-skip).
    skip: u64,
}

impl Clip {
    /// Reads a recording: Ogg Opus kept compressed, anything else symphonia
    /// reads decoded to PCM. Off the UI thread.
    pub fn load(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() > MAX_BYTES {
            bail!("the recording is too large");
        }
        let data: Arc<[u8]> = bytes.into();
        match Self::opus(data.clone()) {
            Ok(Some(clip)) => Ok(clip),
            _ => Self::pcm(&data),
        }
    }

    /// The compressed packets of an Ogg Opus file; None for another format.
    fn opus(data: Arc<[u8]>) -> Result<Option<Self>> {
        let source = MediaSourceStream::new(Box::new(Cursor::new(data)), Default::default());
        let probed = symphonia::default::get_probe().format(
            &Hint::new(),
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )?;
        let mut format = probed.format;
        let track = format.default_track().ok_or_else(|| anyhow!("no audio track"))?;
        let (id, codec) = (track.id, track.codec_params.clone());
        if codec.codec != CODEC_TYPE_OPUS {
            return Ok(None);
        }
        let mut packets = Vec::new();
        loop {
            match format.next_packet() {
                Ok(p) if p.track_id() == id => packets.push(p.data),
                Ok(_) => {}
                Err(SymphoniaError::IoError(_)) => break,
                Err(error) => return Err(error.into()),
            }
        }
        Self::from_packets(packets, u64::from(codec.delay.unwrap_or(0))).map(Some)
    }

    fn pcm(bytes: &[u8]) -> Result<Self> {
        let samples = crate::sounds::decode_sound(bytes, RATE)?;
        let pcm = samples
            .into_iter()
            .map(|s| (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)
            .collect();
        Ok(Self {
            source: Source::Pcm(pcm),
            skip: 0,
        })
    }

    /// A clip from Opus packets as the Ogg stream carries them.
    pub fn from_packets(packets: Vec<Box<[u8]>>, skip: u64) -> Result<Self> {
        let mut starts = Vec::with_capacity(packets.len());
        let mut total = 0u64;
        for packet in &packets {
            starts.push(total);
            total += packet_samples(packet).ok_or_else(|| anyhow!("a damaged Opus packet"))? as u64;
        }
        if total <= skip {
            bail!("the recording is empty");
        }
        Ok(Self {
            source: Source::Opus { packets, starts, total },
            skip,
        })
    }

    /// Samples at 48 kHz.
    fn len(&self) -> u64 {
        match &self.source {
            Source::Opus { total, .. } => total.saturating_sub(self.skip),
            Source::Pcm(pcm) => pcm.len() as u64,
        }
    }

    pub fn duration_ms(&self) -> u64 {
        self.len() * 1000 / u64::from(RATE)
    }
}

/// A packet's length in samples at 48 kHz, from its table-of-contents byte
/// (RFC 6716, section 3.1).
fn packet_samples(packet: &[u8]) -> Option<usize> {
    let toc = usize::from(*packet.first()?);
    let config = toc >> 3;
    let frame = match config {
        0..=11 => [480, 960, 1920, 2880][config % 4],
        12..=15 => [480, 960][config % 2],
        _ => [120, 240, 480, 960][config % 4],
    };
    let frames = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => usize::from(*packet.get(1)? & 0x3f),
    };
    let samples = frame * frames;
    (frames > 0 && samples <= MAX_PACKET).then_some(samples)
}

/// Reads a clip's samples in order, decoding a packet when one is needed.
struct Engine {
    clip: Clip,
    decoder: Option<Decoder>,
    buf: Vec<i16>,
    buf_start: u64,
    buf_len: usize,
    /// The packet the decoder has decoded up to.
    next: usize,
}

impl Engine {
    fn new(clip: Clip) -> Result<Self> {
        let opus = matches!(clip.source, Source::Opus { .. });
        Ok(Self {
            decoder: if opus { Some(Decoder::new(1)?) } else { None },
            buf: if opus { vec![0; MAX_PACKET] } else { Vec::new() },
            buf_start: 0,
            buf_len: 0,
            next: 0,
            clip,
        })
    }

    /// The sample `i` samples into the recording, None past its end.
    fn sample(&mut self, i: u64) -> Option<i16> {
        if i >= self.clip.len() {
            return None;
        }
        let j = i + self.clip.skip;
        let (packets, starts) = match &self.clip.source {
            Source::Pcm(pcm) => return pcm.get(usize::try_from(i).ok()?).copied(),
            Source::Opus { packets, starts, .. } => (packets, starts),
        };
        if j < self.buf_start || j >= self.buf_start + self.buf_len as u64 {
            let p = starts.partition_point(|s| *s <= j).checked_sub(1)?;
            let decoder = self.decoder.as_mut()?;
            // After a jump, a packet before warms the decoder up (its first
            // output is discarded), as Opus decoders carry state.
            if p != self.next && p > 0 {
                let _ = decoder.decode(&packets[p - 1], &mut self.buf, false);
            }
            let wanted = packet_samples(&packets[p])?;
            let out = self.buf.get_mut(..wanted)?;
            // A packet that won't decode plays as silence rather than stopping.
            if decoder.decode(&packets[p], out, false).is_err() {
                out.fill(0);
            }
            self.buf_start = starts[p];
            self.buf_len = wanted;
            self.next = p + 1;
        }
        self.buf.get(usize::try_from(j - self.buf_start).ok()?).copied()
    }
}

/// The next output sample at `*pos` source samples in, moving on by `step`:
/// linear interpolation. None at the end.
fn next_sample(engine: &mut Engine, pos: &mut f64, step: f64) -> Option<f32> {
    let i = *pos as u64;
    let frac = (*pos - i as f64) as f32;
    let a = f32::from(engine.sample(i)?) / 32768.0;
    let b = engine.sample(i + 1).map_or(a, |b| f32::from(b) / 32768.0);
    *pos += step;
    Some(a + (b - a) * frac)
}

/// What the person and the speakers' callback share.
struct Shared {
    /// The play position, in source samples.
    position: AtomicU64,
    /// A position to jump to (u64::MAX: none).
    seek: AtomicU64,
    speed: AtomicU32,
    volume: AtomicU32,
    finished: AtomicBool,
    total: u64,
}

const NO_SEEK: u64 = u64::MAX;

enum Control {
    Play,
    Pause,
}

/// A recording playing (or paused) on the speakers. Dropping it stops it.
pub struct Player {
    shared: Arc<Shared>,
    control: mpsc::Sender<Control>,
}

impl Player {
    /// Opens the speakers (`device`: None for the default) and plays `clip`
    /// from the start. `failed` is told, from the player's thread, if the
    /// speakers can't be opened.
    pub fn start(
        clip: Clip,
        device: Option<String>,
        speed: f32,
        volume: f32,
        failed: impl FnOnce(String) + Send + 'static,
    ) -> Result<Self> {
        let shared = Arc::new(Shared {
            position: AtomicU64::new(0),
            seek: AtomicU64::new(NO_SEEK),
            speed: AtomicU32::new(speed.to_bits()),
            volume: AtomicU32::new(volume.to_bits()),
            finished: AtomicBool::new(false),
            total: clip.len(),
        });
        let engine = Engine::new(clip)?;
        let (control, controls) = mpsc::channel();
        let state = shared.clone();
        std::thread::Builder::new()
            .name("voice-message".into())
            .spawn(move || {
                let stream = match open(engine, device.as_deref(), state) {
                    Ok(stream) => stream,
                    Err(error) => return failed(format!("{error:#}")),
                };
                // Blocks until told: no timer runs while paused.
                while let Ok(command) = controls.recv() {
                    let _ = match command {
                        Control::Play => stream.play(),
                        Control::Pause => stream.pause(),
                    };
                }
            })
            .map_err(|e| anyhow!("starting the voice message thread: {e}"))?;
        let player = Self { shared, control };
        player.play();
        Ok(player)
    }

    pub fn play(&self) {
        let _ = self.control.send(Control::Play);
    }

    pub fn pause(&self) {
        let _ = self.control.send(Control::Pause);
    }

    /// Jumps to `ms` (also while paused, or finished: it plays on from there).
    pub fn seek_ms(&self, ms: u64) {
        let to = (ms * u64::from(RATE) / 1000).min(self.shared.total.saturating_sub(1));
        self.shared.position.store(to, Relaxed);
        self.shared.seek.store(to, Relaxed);
        self.shared.finished.store(false, Relaxed);
    }

    /// 0.5 to 2 times as fast.
    pub fn set_speed(&self, speed: f32) {
        self.shared.speed.store(speed.to_bits(), Relaxed);
    }

    /// 0 to 1.
    pub fn set_volume(&self, volume: f32) {
        self.shared.volume.store(volume.to_bits(), Relaxed);
    }

    pub fn position_ms(&self) -> u64 {
        self.shared.position.load(Relaxed) * 1000 / u64::from(RATE)
    }

    pub fn duration_ms(&self) -> u64 {
        self.shared.total * 1000 / u64::from(RATE)
    }

    /// It reached its end and went quiet; seek to play it again.
    pub fn finished(&self) -> bool {
        self.shared.finished.load(Relaxed)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // The thread ends when the channel closes, dropping the stream and
        // with it the clip.
        let _ = self.control.send(Control::Pause);
    }
}

fn open(mut engine: Engine, device: Option<&str>, shared: Arc<Shared>) -> Result<cpal::Stream> {
    let device = find_device(device, false).ok_or_else(|| anyhow!("no speakers"))?;
    let config = device.default_output_config()?;
    let channels = usize::from(config.channels()).max(1);
    let rate = f64::from(config.sample_rate());
    let mut pos = 0.0f64;
    let mut next = move || -> f32 {
        let seek = shared.seek.swap(NO_SEEK, Relaxed);
        if seek != NO_SEEK {
            pos = seek as f64;
        }
        if shared.finished.load(Relaxed) {
            return 0.0;
        }
        let speed = f32::from_bits(shared.speed.load(Relaxed)).clamp(0.5, 2.0);
        // Squared, so the slider's middle sounds about half as loud.
        let gain = f32::from_bits(shared.volume.load(Relaxed)).clamp(0.0, 1.0).powi(2);
        let step = f64::from(speed) * f64::from(RATE) / rate;
        match next_sample(&mut engine, &mut pos, step) {
            Some(s) => {
                shared.position.store(pos as u64, Relaxed);
                s * gain
            }
            None => {
                shared.finished.store(true, Relaxed);
                shared.position.store(shared.total, Relaxed);
                0.0
            }
        }
    };
    let error = |e: cpal::Error| eprintln!("error: voice message stream: {e}");
    let cfg: cpal::StreamConfig = config.into();
    macro_rules! build {
        ($ty:ty, $convert:expr) => {
            device.build_output_stream(
                cfg,
                move |data: &mut [$ty], _| {
                    for frame in data.chunks_mut(channels) {
                        frame.fill($convert(next()));
                    }
                },
                error,
                None,
            )
        };
    }
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => build!(i16, |s: f32| (s * 32767.0) as i16),
        cpal::SampleFormat::U16 => build!(u16, |s: f32| (s * 32767.0 + 32768.0) as u16),
        other => bail!("unsupported speaker format {other:?}"),
    }?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opus::Encoder;

    fn pcm_engine(samples: Vec<i16>) -> Engine {
        Engine::new(Clip {
            source: Source::Pcm(samples),
            skip: 0,
        })
        .unwrap()
    }

    #[test]
    fn packet_lengths_come_from_the_table_of_contents() {
        // CELT 20 ms, one frame: config 19.
        assert_eq!(packet_samples(&[19 << 3, 0]), Some(960));
        // SILK 60 ms, two frames (code 1).
        assert_eq!(packet_samples(&[(3 << 3) | 1, 0]), Some(5760));
        // Hybrid 10 ms, code 3 with three frames.
        assert_eq!(packet_samples(&[(12 << 3) | 3, 3]), Some(1440));
        // Too long, empty, and zero frames.
        assert_eq!(packet_samples(&[(3 << 3) | 3, 3]), None);
        assert_eq!(packet_samples(&[]), None);
        assert_eq!(packet_samples(&[3, 0]), None);
    }

    #[test]
    fn speed_resamples_by_stepping_through_the_samples() {
        let ramp: Vec<i16> = (0..100).map(|i| i * 100).collect();
        let count = |step: f64| {
            let mut engine = pcm_engine(ramp.clone());
            let mut pos = 0.0;
            std::iter::from_fn(|| next_sample(&mut engine, &mut pos, step)).count()
        };
        assert_eq!(count(1.0), 100);
        assert_eq!(count(2.0), 50);
        assert_eq!(count(1.5), 67);
        // Half a step between two samples is their mean.
        let mut engine = pcm_engine(vec![0, 3276]);
        let mut pos = 0.5;
        let s = next_sample(&mut engine, &mut pos, 1.0).unwrap();
        assert!((s - 3276.0 / 65536.0).abs() < 1e-4);
    }

    #[test]
    fn opus_clips_play_and_seek() {
        let mut encoder = Encoder::new(1, 24_000).unwrap();
        let tone: Vec<f32> = (0..960).map(|i| (i as f32 * 0.1).sin() * 0.5).collect();
        let mut packets = Vec::new();
        for _ in 0..50 {
            let mut out = vec![0u8; 400];
            let n = encoder.encode_float(&tone, &mut out).unwrap();
            out.truncate(n);
            packets.push(out.into_boxed_slice());
        }
        let clip = Clip::from_packets(packets, 0).unwrap();
        assert_eq!(clip.duration_ms(), 1000);
        let mut engine = Engine::new(clip).unwrap();
        // In order, then jumping back and forward.
        assert!(engine.sample(0).is_some());
        assert!(engine.sample(959).is_some());
        assert!(engine.sample(960).is_some());
        assert!(engine.sample(30_000).is_some());
        assert!(engine.sample(100).is_some());
        assert!(engine.sample(47_999).is_some());
        assert_eq!(engine.sample(48_000), None);
        // Past the first packets the tone is audible.
        let loud = (30_000..30_960)
            .filter_map(|i| engine.sample(i))
            .any(|s| s.abs() > 1000);
        assert!(loud);
    }

    #[test]
    fn pre_skip_shortens_the_clip() {
        let packet: Box<[u8]> = vec![19 << 3, 0].into_boxed_slice();
        let clip = Clip::from_packets(vec![packet.clone(), packet.clone()], 312).unwrap();
        assert_eq!(clip.len(), 1920 - 312);
        assert!(Clip::from_packets(vec![packet], 960).is_err());
    }
}
