// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! H264 encoding for Discord Go Live through x264 (`native/h264.c`, built
//! against `vendor/x264-src`).
//!
//! Why x264 and not the OpenH264 inside libwebrtc: by Yapfast's measurements
//! (its docs/discord/go-live.md, "Choosing the H264 encoder") OpenH264 drops
//! frames on anything that moves, and with frame skipping off its rate
//! control runs far past the target; every Go Live sender known to work
//! sends H264. The settings are Yapfast's: superfast, zero latency, Main
//! profile, a keyframe at least every three seconds, parameter sets before
//! each IDR.

use std::ffi::{c_int, c_long, c_longlong, c_uchar, c_uint, c_ulong};

use crate::vpx::encode::{EncodeError, Planes};

#[repr(C)]
struct Raw {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn godwit_h264_new(
        width: c_uint,
        height: c_uint,
        fps: c_uint,
        kbps: c_uint,
        threads: c_uint,
        effort: c_uint,
    ) -> *mut Raw;
    #[allow(clippy::too_many_arguments)]
    fn godwit_h264_encode(
        h264: *mut Raw,
        y: *const c_uchar,
        y_stride: c_int,
        u: *const c_uchar,
        u_stride: c_int,
        v: *const c_uchar,
        v_stride: c_int,
        pts: c_longlong,
        duration: c_ulong,
        keyframe: c_int,
        out: *mut c_uchar,
        capacity: usize,
        is_key: *mut c_int,
    ) -> c_long;
    fn godwit_h264_set_bitrate(h264: *mut Raw, kbps: c_uint) -> c_int;
    fn godwit_h264_free(h264: *mut Raw);
}

/// An H264 encoder for one frame size. Freed on drop.
pub struct H264Encoder {
    raw: *mut Raw,
    width: u32,
    height: u32,
    kbps: u32,
    out: Vec<u8>,
}

// SAFETY: x264 has no thread affinity; every use takes `&mut self`.
unsafe impl Send for H264Encoder {}

impl H264Encoder {
    /// An encoder for `width` by `height` pictures (both even) at `fps`,
    /// aiming for `kbps`; None if x264 refuses the settings.
    pub fn new(width: u32, height: u32, fps: u32, kbps: u32) -> Option<Self> {
        if width < 2 || height < 2 || !width.is_multiple_of(2) || !height.is_multiple_of(2) || fps == 0 {
            return None;
        }
        // A slice for each of up to four threads (Yapfast's choice).
        let threads = std::thread::available_parallelism().map_or(1, |cores| cores.get().min(4));
        let threads = c_uint::try_from(threads).unwrap_or(1);
        // SAFETY: plain values; null on failure.
        let raw = unsafe { godwit_h264_new(width, height, fps, kbps.max(1), threads, 0) };
        if raw.is_null() {
            return None;
        }
        Some(Self {
            raw,
            width,
            height,
            kbps: kbps.max(1),
            // Even a keyframe of noise fits.
            out: vec![0; width as usize * height as usize * 2 + 4096],
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn bitrate(&self) -> u32 {
        self.kbps
    }

    /// Aims for `kbps` from the next frame on, without a keyframe.
    pub fn set_bitrate(&mut self, kbps: u32) -> bool {
        // SAFETY: opened in `new`.
        let ok = unsafe { godwit_h264_set_bitrate(self.raw, kbps.max(1)) } == 0;
        if ok {
            self.kbps = kbps.max(1);
        }
        ok
    }

    /// Encodes a picture of the encoder's size at `pts` (90 kHz): the frame
    /// in Annex B form and whether it's a keyframe, or None when x264 gave
    /// no frame for it.
    pub fn encode(
        &mut self,
        picture: Planes<'_>,
        pts: i64,
        keyframe: bool,
    ) -> Result<Option<(&[u8], bool)>, EncodeError> {
        if (picture.width, picture.height) != (self.width, self.height) {
            return Err(EncodeError::InvalidPicture);
        }
        let [ys, us, vs] = picture.strides;
        let (w, h) = (self.width as usize, self.height as usize);
        let fits = |plane: &[u8], stride: u32, rows: usize, row: usize| {
            stride as usize >= row && plane.len() >= (rows - 1) * stride as usize + row
        };
        if !fits(picture.y, ys, h, w) || !fits(picture.u, us, h / 2, w / 2) || !fits(picture.v, vs, h / 2, w / 2) {
            return Err(EncodeError::InvalidPicture);
        }
        let stride = |s: u32| c_int::try_from(s).map_err(|_| EncodeError::InvalidPicture);
        let mut is_key: c_int = 0;
        // SAFETY: the planes hold the picture at their strides (checked
        // above) and x264 reads them during the call only (zero latency: a
        // frame out for each frame in); `out` has `capacity` bytes.
        let size = unsafe {
            godwit_h264_encode(
                self.raw,
                picture.y.as_ptr(),
                stride(ys)?,
                picture.u.as_ptr(),
                stride(us)?,
                picture.v.as_ptr(),
                stride(vs)?,
                pts,
                0,
                c_int::from(keyframe),
                self.out.as_mut_ptr(),
                self.out.len(),
                &mut is_key,
            )
        };
        match usize::try_from(size) {
            Ok(0) => Ok(None),
            Ok(n) => Ok(self.out.get(..n).map(|data| (data, is_key != 0))),
            Err(_) => Err(EncodeError::Codec(c_int::try_from(size).unwrap_or(-1))),
        }
    }
}

impl Drop for H264Encoder {
    fn drop(&mut self) {
        // SAFETY: opened in `new` and freed only here.
        unsafe { godwit_h264_free(self.raw) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A moving gradient: an IDR with its parameter sets first, then
    /// deltas, then a forced IDR; every frame Annex B, decodable by the H264
    /// decoder Godwit watches streams with where this build has one.
    #[test]
    fn encodes_annex_b_with_parameter_sets_on_keyframes() {
        let (w, h) = (64u32, 36u32);
        let mut encoder = H264Encoder::new(w, h, 30, 500).unwrap();
        assert!(H264Encoder::new(63, 36, 30, 500).is_none(), "odd sizes");
        let (cw, ch) = (w as usize / 2, h as usize / 2);
        let mut keys = vec![];
        for frame in 0..4u8 {
            let y: Vec<u8> = (0..w as usize * h as usize)
                .map(|i| (i as u8).wrapping_add(frame * 8))
                .collect();
            let (u, v) = (vec![90u8; cw * ch], vec![200u8; cw * ch]);
            if frame == 1 {
                assert!(encoder.set_bitrate(250));
            }
            let planes = Planes {
                width: w,
                height: h,
                y: &y,
                u: &u,
                v: &v,
                strides: [w, w / 2, w / 2],
            };
            let (data, key) = encoder
                .encode(planes, i64::from(frame) * 3000, frame == 2)
                .unwrap()
                .unwrap();
            assert!(data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1]));
            let types: Vec<u8> = data
                .windows(4)
                .filter(|w| w[..3] == [0, 0, 1])
                .map(|w| w[3] & 0x1f)
                .collect();
            if key {
                assert!(
                    types.contains(&7) && types.contains(&8) && types.contains(&5),
                    "SPS, PPS, IDR: {types:?}"
                );
            }
            keys.push(key);
        }
        assert_eq!(keys, [true, false, true, false]);
    }
}
