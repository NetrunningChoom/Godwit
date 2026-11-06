// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Opus at 48 kHz, from the libopus inside libwebrtc, which this crate links
//! already through LiveKit. A second libopus (an `opus` crate building its
//! own) would put the same symbols in the binary twice.
//!
//! Ported from Yapfast's `opus.rs` (GPL-3.0-or-later), under AGPL-3.0-only.

use std::ffi::c_int;
use std::ptr::NonNull;

use anyhow::{Result, bail};

#[repr(C)]
struct RawEncoder {
    _private: [u8; 0],
}

#[repr(C)]
struct RawDecoder {
    _private: [u8; 0],
}

// These live in libwebrtc's archive (third_party/opus), which webrtc-sys links
// for LiveKit; every desktop build of it carries them.
unsafe extern "C" {
    fn opus_encoder_create(rate: i32, channels: c_int, application: c_int, error: *mut c_int) -> *mut RawEncoder;
    fn opus_encoder_destroy(encoder: *mut RawEncoder);
    fn opus_encode_float(
        encoder: *mut RawEncoder,
        pcm: *const f32,
        frame_size: c_int,
        data: *mut u8,
        max_data_bytes: i32,
    ) -> i32;
    fn opus_encoder_ctl(encoder: *mut RawEncoder, request: c_int, ...) -> c_int;
    fn opus_decoder_create(rate: i32, channels: c_int, error: *mut c_int) -> *mut RawDecoder;
    fn opus_decoder_destroy(decoder: *mut RawDecoder);
    fn opus_decode(
        decoder: *mut RawDecoder,
        data: *const u8,
        len: i32,
        pcm: *mut i16,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;
}

const RATE: i32 = 48_000;
const APPLICATION_VOIP: c_int = 2048;
const SET_BITRATE: c_int = 4002;
const SET_INBAND_FEC: c_int = 4012;
const SET_PACKET_LOSS_PERC: c_int = 4014;

/// An Opus encoder at 48 kHz, tuned for speech.
pub struct Encoder {
    raw: NonNull<RawEncoder>,
    channels: usize,
}

// SAFETY: an encoder is used from one thread at a time, through `&mut`.
unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new(channels: u16, bitrate: c_int) -> Result<Self> {
        let mut error: c_int = 0;
        // SAFETY: valid arguments; `error` is written before returning.
        let raw = unsafe { opus_encoder_create(RATE, c_int::from(channels), APPLICATION_VOIP, &mut error) };
        let Some(raw) = NonNull::new(raw).filter(|_| error == 0) else {
            bail!("could not start the Opus encoder ({error})");
        };
        let encoder = Self {
            raw,
            channels: usize::from(channels.max(1)),
        };
        let (fec, loss): (c_int, c_int) = (1, 5);
        // SAFETY: requests that take one int, as libopus's macros pass it.
        // Forward error correction lets a receiver rebuild a lost packet from
        // the next one; it costs bits only when loss is expected.
        unsafe {
            opus_encoder_ctl(encoder.raw.as_ptr(), SET_BITRATE, bitrate);
            opus_encoder_ctl(encoder.raw.as_ptr(), SET_INBAND_FEC, fec);
            opus_encoder_ctl(encoder.raw.as_ptr(), SET_PACKET_LOSS_PERC, loss);
        }
        Ok(encoder)
    }

    /// Encodes one frame of interleaved samples into `out`, returning the
    /// packet's length.
    pub fn encode_float(&mut self, pcm: &[f32], out: &mut [u8]) -> Result<usize> {
        let frame = c_int::try_from(pcm.len() / self.channels).unwrap_or(c_int::MAX);
        let room = i32::try_from(out.len()).unwrap_or(i32::MAX);
        // SAFETY: `pcm` holds `frame` samples per channel and `out` `room` bytes.
        let length = unsafe { opus_encode_float(self.raw.as_ptr(), pcm.as_ptr(), frame, out.as_mut_ptr(), room) };
        let Ok(length) = usize::try_from(length) else {
            bail!("Opus couldn't encode a frame ({length})");
        };
        Ok(length)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: made by opus_encoder_create, destroyed once.
        unsafe { opus_encoder_destroy(self.raw.as_ptr()) };
    }
}

/// An Opus decoder at 48 kHz. Mono decoding mixes a stereo stream down,
/// which is what the mixer plays.
pub struct Decoder {
    raw: NonNull<RawDecoder>,
    channels: usize,
}

// SAFETY: as for the encoder.
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new(channels: u16) -> Result<Self> {
        let mut error: c_int = 0;
        // SAFETY: valid arguments; `error` is written before returning.
        let raw = unsafe { opus_decoder_create(RATE, c_int::from(channels), &mut error) };
        let Some(raw) = NonNull::new(raw).filter(|_| error == 0) else {
            bail!("could not start the Opus decoder ({error})");
        };
        Ok(Self {
            raw,
            channels: usize::from(channels.max(1)),
        })
    }

    /// Decodes a packet into interleaved samples, returning the samples per
    /// channel. `fec` reads the forward error correction a packet carries
    /// for the one before it.
    pub fn decode(&mut self, packet: &[u8], pcm: &mut [i16], fec: bool) -> Result<usize> {
        self.run(Some(packet), pcm, fec)
    }

    /// Conceals a lost packet, filling all of `pcm`: a frame's worth of sound
    /// that carries on from the last.
    pub fn conceal(&mut self, pcm: &mut [i16]) -> Result<usize> {
        self.run(None, pcm, false)
    }

    fn run(&mut self, packet: Option<&[u8]>, pcm: &mut [i16], fec: bool) -> Result<usize> {
        let (data, length) = packet.map_or((std::ptr::null(), 0), |p| {
            (p.as_ptr(), i32::try_from(p.len()).unwrap_or(i32::MAX))
        });
        let frame = c_int::try_from(pcm.len() / self.channels).unwrap_or(c_int::MAX);
        // SAFETY: `pcm` holds `frame` samples per channel; a null packet asks
        // for concealment.
        let samples = unsafe {
            opus_decode(
                self.raw.as_ptr(),
                data,
                length,
                pcm.as_mut_ptr(),
                frame,
                c_int::from(fec),
            )
        };
        let Ok(samples) = usize::try_from(samples) else {
            bail!("Opus couldn't decode a packet ({samples})");
        };
        Ok(samples)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: made by opus_decoder_create, destroyed once.
        unsafe { opus_decoder_destroy(self.raw.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_survives_encoding_and_loss() {
        let mut encoder = Encoder::new(2, 64_000).unwrap();
        let mut decoder = Decoder::new(1).unwrap();
        let mut packet = [0u8; 1500];
        let mut pcm = [0i16; 960];
        let mut loudest = 0;
        for frame in 0..10 {
            let tone: Vec<f32> = (0..960)
                .flat_map(|at| {
                    let s = ((frame * 960 + at) as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5;
                    [s, s]
                })
                .collect();
            let length = encoder.encode_float(&tone, &mut packet).unwrap();
            assert!(length > 3, "a real packet");
            assert_eq!(decoder.decode(&packet[..length], &mut pcm, false).unwrap(), 960);
            loudest = loudest.max(pcm.iter().map(|s| s.unsigned_abs()).max().unwrap());
        }
        assert!(loudest > 8_000, "the tone came through: {loudest}");
        assert_eq!(decoder.conceal(&mut pcm).unwrap(), 960, "a lost frame fills in");
    }
}
