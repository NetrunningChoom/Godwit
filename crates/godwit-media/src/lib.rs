// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! WebM (VP8/VP9) clips decoded to RGBA frames with per-frame delays, for
//! Godwit's animated-image path.
//!
//! Why: GIF pickers (gifbox, Tenor) serve their "GIFs" only as short looping
//! WebM videos, so showing them like GIFs needs a video decoder. libvpx (the
//! VP8/VP9 decoder) and libyuv (colour conversion and scaling) are already
//! compiled into livekit's prebuilt libwebrtc, which Godwit links for calls,
//! so this crate calls them directly instead of adding another native
//! dependency. It works wherever livekit's libwebrtc does (Windows, macOS,
//! Linux, and the mobile targets livekit ships). The container is read by a
//! small pure-Rust WebM demuxer.
//!
//! Decoding runs on the caller's thread. [`decode`] decodes a whole clip at
//! once; [`Stream`] decodes one frame at a time and loops, and can move to a
//! worker thread. Nothing is shared between calls or streams.

use std::ops::Range;
use std::sync::Arc;

pub mod live;
mod vpx;
mod vpx_size;

/// Encoding live video to send: VP8 through libvpx (Discord's cameras) and
/// H264 through x264 (Go Live), which hand their pictures to a call themselves.
pub mod encode {
    pub use crate::h264::H264Encoder;
    pub use crate::vpx::encode::{CLOCK, Content, EncodeError, Planes, Vp8Encoder};
}
mod h264;
mod webm;

// Only linked for its native library: `webrtc-sys`'s build script adds
// libwebrtc (with libvpx and libyuv inside) to the link line.
use webrtc_sys as _;
// Likewise x264-src, for its static library: `native/h264.c` calls into it,
// and an unreferenced crate would be left off the link line.
use x264_src as _;

/// The fastest an animation plays: browsers treat shorter GIF delays
/// (0–10 ms) as 100 ms, and anything under 20 ms as unintended. Same policy
/// as the app's GIF decoding.
const MIN_DELAY_MS: u32 = 20;

/// The largest decoded frame accepted (4096 x 4096): a frame is converted at
/// full size before shrinking, so a hostile header can't demand gigabytes.
const MAX_SOURCE_PIXELS: u64 = 4096 * 4096;

/// A decoded clip: every frame at the same size, RGBA (not premultiplied),
/// each with how long it shows in milliseconds.
#[derive(Debug, Clone)]
pub struct Clip {
    pub width: u32,
    pub height: u32,
    pub frames: Vec<(Vec<u8>, u32)>,
}

/// Whether `bytes` look like a WebM file: the EBML magic and DocType "webm".
/// Cheap: reads only the header.
pub fn is_webm(bytes: &[u8]) -> bool {
    webm::is_webm(bytes)
}

/// Decodes a WebM clip's first VP8/VP9 video track, every frame up front.
///
/// Frames are scaled down (never up) to fit `max_w` x `max_h`, then further
/// so all frames together stay within `max_total_bytes`: long clips trade
/// resolution for keeping every frame, as the app's GIF path does. At most
/// `max_frames` frames are decoded. Malformed or truncated input gives `None`
/// or the frames before the damage; it never panics.
pub fn decode(bytes: &[u8], max_w: u32, max_h: u32, max_total_bytes: usize, max_frames: usize) -> Option<Clip> {
    // Every frame takes at least one RGBA pixel.
    let max_frames = max_frames.min(max_total_bytes / 4);
    let mut stream = Stream::open(Arc::from(bytes), max_w, max_h, Some(max_total_bytes), max_frames)?;
    let (width, height) = stream.size();
    let mut frames = Vec::with_capacity(stream.frame_count());
    frames.extend(stream.pending.take());
    loop {
        let mut pixels = Vec::new();
        match stream.step(&mut pixels) {
            Step::Frame(delay) => frames.push((pixels, delay)),
            Step::End | Step::Failed => break,
        }
    }
    Some(Clip { width, height, frames })
}

/// One compressed frame: where it and its alpha lie in the file, and how long
/// until the next block in milliseconds (before the browser delay policy).
struct Entry {
    data: Range<usize>,
    alpha: Option<Range<usize>>,
    ms: f64,
}

enum Step {
    /// A frame was written, showing this many milliseconds.
    Frame(u32),
    /// No blocks left.
    End,
    /// The block just tried can't be decoded.
    Failed,
}

/// A looping WebM clip decoded one frame at a time, as browsers play video:
/// only the compressed file and the decoder's few reference frames stay in
/// memory, so large clips keep their full resolution. Owns its input, so it
/// can move to a worker thread.
pub struct Stream {
    bytes: Arc<[u8]>,
    codec: webm::Codec,
    entries: Vec<Entry>,
    max: (u32, u32),
    /// Total bytes all frames may take (only for [`decode`]).
    budget: Option<usize>,
    /// Output size, fixed by the first frame.
    size: Option<(u32, u32)>,
    /// The next entry to decode.
    next: usize,
    decoder: Option<vpx::Decoder>,
    alpha_decoder: Option<vpx::Decoder>,
    /// Full-size RGBA before scaling, reused between frames.
    scratch: Vec<u8>,
    /// Time from hidden frames (VP8 alt-ref), added to the next shown one so
    /// the loop keeps its length.
    carried_ms: f64,
    /// The first frame, decoded by `new` to learn the size.
    pending: Option<(Vec<u8>, u32)>,
}

impl Stream {
    /// Opens a WebM clip; `None` if it isn't one this can decode. Frames are
    /// fitted within `max_w` x `max_h` (never enlarged), keeping the aspect
    /// ratio. Decodes the first frame right away.
    pub fn new(bytes: Arc<[u8]>, max_w: u32, max_h: u32) -> Option<Self> {
        Self::open(bytes, max_w, max_h, None, usize::MAX)
    }

    fn open(bytes: Arc<[u8]>, max_w: u32, max_h: u32, budget: Option<usize>, max_frames: usize) -> Option<Self> {
        if max_w == 0 || max_h == 0 {
            return None;
        }
        let demuxed = webm::demux(&bytes, max_frames)?;
        let entries = entries(&demuxed);
        let mut stream = Self {
            bytes,
            codec: demuxed.track.codec,
            entries,
            max: (max_w, max_h),
            budget,
            size: None,
            next: 0,
            decoder: None,
            alpha_decoder: None,
            scratch: Vec::new(),
            carried_ms: 0.0,
            pending: None,
        };
        let mut first = Vec::new();
        let Step::Frame(delay) = stream.step(&mut first) else {
            return None;
        };
        stream.pending = Some((first, delay));
        Some(stream)
    }

    /// The output frame size.
    pub fn size(&self) -> (u32, u32) {
        self.size.unwrap_or((1, 1))
    }

    /// How many frames the clip has (counting the rare hidden frames, so at
    /// most this many), for budgeting. Shrinks if a damaged frame is found.
    pub fn frame_count(&self) -> usize {
        self.entries.len()
    }

    /// The next frame, RGBA (straight alpha) at [`Stream::size`], and how
    /// long it shows in milliseconds. After the last frame it starts over.
    /// `None` only when nothing more can be decoded.
    pub fn next_frame(&mut self) -> Option<(Vec<u8>, u32)> {
        let mut pixels = Vec::new();
        let delay = self.next_frame_into(&mut pixels)?;
        Some((pixels, delay))
    }

    /// [`Stream::next_frame`] into a reused buffer (resized to fit).
    pub fn next_frame_into(&mut self, out: &mut Vec<u8>) -> Option<u32> {
        if let Some((pixels, delay)) = self.pending.take() {
            *out = pixels;
            return Some(delay);
        }
        let mut restarted = false;
        loop {
            match self.step(out) {
                Step::Frame(delay) => return Some(delay),
                Step::End if !restarted => {}
                Step::Failed if !restarted => {
                    // Later frames predict from the damaged one: from now on
                    // the loop ends just before it.
                    self.entries.truncate(self.next.saturating_sub(1));
                }
                Step::End | Step::Failed => return None,
            }
            // Back to the first frame (a keyframe) with fresh decoders, so no
            // state from the end of the clip leaks into its start.
            self.next = 0;
            self.decoder = None;
            self.alpha_decoder = None;
            self.carried_ms = 0.0;
            restarted = true;
        }
    }

    /// Decodes entries until one shows a frame, written to `out`.
    fn step(&mut self, out: &mut Vec<u8>) -> Step {
        loop {
            let Some(entry) = self.entries.get(self.next) else {
                return Step::End;
            };
            self.next += 1;
            if self.decoder.is_none() {
                self.decoder = vpx::Decoder::new(self.codec);
            }
            let Some(decoder) = self.decoder.as_mut() else {
                return Step::Failed;
            };
            let Some(data) = self.bytes.get(entry.data.clone()) else {
                return Step::Failed;
            };
            let Ok(image) = decoder.decode(data) else {
                return Step::Failed;
            };
            let Some(image) = image else {
                self.carried_ms += entry.ms;
                continue;
            };
            let alpha = match entry.alpha.clone().and_then(|r| self.bytes.get(r)) {
                Some(data) => {
                    if self.alpha_decoder.is_none() {
                        self.alpha_decoder = vpx::Decoder::new(self.codec);
                    }
                    // A bad alpha frame only costs transparency, not the frame.
                    self.alpha_decoder.as_mut().and_then(|d| d.decode(data).ok().flatten())
                }
                None => None,
            };
            let (fw, fh) = image.size();
            if fw == 0 || fh == 0 || u64::from(fw) * u64::from(fh) > MAX_SOURCE_PIXELS {
                return Step::Failed;
            }
            // Sized from the first frame: VP9 may change size mid-clip, and
            // later frames are scaled to match.
            let (w, h) = *self.size.get_or_insert_with(|| {
                let (w, h) = fit(fw, fh, self.max.0, self.max.1);
                match self.budget {
                    Some(bytes) => budget(w, h, self.entries.len(), bytes),
                    None => (w, h),
                }
            });
            let converted = if (fw, fh) == (w, h) {
                image.to_rgba(alpha, out)
            } else {
                image
                    .to_rgba(alpha, &mut self.scratch)
                    .and_then(|()| vpx::scale_rgba(&self.scratch, fw, fh, out, w, h))
            };
            if converted.is_none() {
                return Step::Failed;
            }
            let ms = std::mem::take(&mut self.carried_ms) + entry.ms;
            return Step::Frame(browser_delay(ms));
        }
    }
}

/// Each block's display time: until the next block; the last one gets its
/// own duration, the track's frame duration, what's left of the segment, or
/// the one before.
fn entries(demuxed: &webm::Demuxed) -> Vec<Entry> {
    let default_ms = demuxed.track.default_duration_ns.map(|ns| ns as f64 / 1_000_000.0);
    let blocks = &demuxed.blocks;
    let mut previous = None;
    let mut out = Vec::with_capacity(blocks.len());
    for (i, block) in blocks.iter().enumerate() {
        let ms = match blocks.get(i + 1) {
            Some(next) => Some(next.pts_ms - block.pts_ms),
            None => block
                .duration_ms
                .or(default_ms)
                .or_else(|| demuxed.duration_ms.map(|d| d - block.pts_ms).filter(|d| *d > 0.0))
                .or(previous),
        };
        previous = ms.or(previous);
        out.push(Entry {
            data: block.data.clone(),
            alpha: block.alpha.clone(),
            ms: ms.unwrap_or(0.0),
        });
    }
    out
}

/// A frame delay as browsers play it (see [`MIN_DELAY_MS`]).
fn browser_delay(ms: f64) -> u32 {
    let ms = if ms.is_finite() {
        ms.round().clamp(0.0, f64::from(u32::MAX)) as u32
    } else {
        0
    };
    if ms <= 10 { 100 } else { ms.max(MIN_DELAY_MS) }
}

/// Fits `w` x `h` within `max_w` x `max_h`, keeping the aspect ratio and
/// never enlarging.
fn fit(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w <= max_w && h <= max_h {
        return (w, h);
    }
    let scale = (f64::from(max_w) / f64::from(w)).min(f64::from(max_h) / f64::from(h));
    let w = ((f64::from(w) * scale).round() as u32).clamp(1, max_w);
    let h = ((f64::from(h) * scale).round() as u32).clamp(1, max_h);
    (w, h)
}

/// Shrinks `w` x `h` uniformly so `count` RGBA frames fit `max_bytes`,
/// rounding down to stay in budget (the app's `animation_size`).
fn budget(w: u32, h: u32, count: usize, max_bytes: usize) -> (u32, u32) {
    let pixels = (max_bytes / count.max(1) / 4).max(1);
    let scale = ((pixels as f64) / (f64::from(w) * f64::from(h))).sqrt().min(1.0);
    let h = ((f64::from(h) * scale).floor() as u32).clamp(1, u32::try_from(pixels).unwrap_or(u32::MAX));
    let w =
        ((f64::from(w) * scale).floor() as u32).clamp(1, u32::try_from(pixels / h as usize).unwrap_or(u32::MAX).max(1));
    (w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_can_move_to_worker_threads() {
        fn send<T: Send>() {}
        send::<Stream>();
    }

    #[test]
    fn delays_follow_the_browser_policy() {
        assert_eq!(browser_delay(0.0), 100);
        assert_eq!(browser_delay(10.0), 100);
        assert_eq!(browser_delay(15.0), 20);
        assert_eq!(browser_delay(40.0), 40);
        assert_eq!(browser_delay(-5.0), 100);
        assert_eq!(browser_delay(f64::NAN), 100);
    }

    #[test]
    fn sizes_fit_without_enlarging_and_respect_the_budget() {
        assert_eq!(fit(32, 24, 640, 640), (32, 24));
        assert_eq!(fit(640, 480, 320, 320), (320, 240));
        assert_eq!(fit(100, 1000, 50, 50), (5, 50));
        let (w, h) = budget(640, 480, 100, 1_000_000);
        assert!(w * h * 4 * 100 <= 1_000_000);
        assert!((f64::from(w) / f64::from(h) - 640.0 / 480.0).abs() < 0.05);
        assert_eq!(budget(32, 24, 3, 1 << 20), (32, 24));
    }
}
