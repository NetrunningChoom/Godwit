// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Decoding live call video (cameras, screen shares) frame by frame to RGBA:
//! VP8 and VP9 through libvpx, and H264 through the FFmpeg inside livekit's
//! libwebrtc, where its build has one.
//!
//! Why these: they're what livekit's prebuilt libwebrtc already carries, so
//! no codec library is added. Its Linux and Windows builds use Chromium's
//! FFmpeg with Chrome branding (`rtc_use_h264`, `ffmpeg_branding="Chrome"` in
//! webrtc-sys's `libwebrtc/build_*.sh`), which has the H264 decoder; the macOS
//! build has no Chrome branding, so there H264 isn't offered ([`decodes`]).
//! On Linux the build renames FFmpeg's symbols with an `LK_` prefix, so they
//! can't clash with a system FFmpeg; the bindings below follow that.
//!
//! FFmpeg is declared by hand as libvpx is (`vpx.rs`): a handful of functions,
//! and only the leading fields of `AVPacket` and `AVFrame`, which FFmpeg keeps
//! stable across versions (libavcodec 62 and libavutil 60 in the archive's
//! `include/third_party/ffmpeg`).

use std::ffi::{CString, c_char, c_int, c_void};
use std::ptr;

use crate::vpx;
use crate::webm;

/// A live video codec Godwit can decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LiveCodec {
    Vp8,
    Vp9,
    H264,
}

/// The codecs this build decodes, most wanted first: H264 where FFmpeg is
/// in libwebrtc, as it's what most senders encode in hardware, then VP8,
/// which every WebRTC sender can fall back to.
pub fn decodes() -> &'static [LiveCodec] {
    if H264_AVAILABLE {
        &[LiveCodec::H264, LiveCodec::Vp8]
    } else {
        &[LiveCodec::Vp8]
    }
}

const H264_AVAILABLE: bool = cfg!(any(target_os = "linux", target_os = "windows"));

/// The widest or tallest picture accepted: a 4K stream fits, and a stream
/// can't make Godwit take more memory than that by claiming a bigger one.
const MAX_SIDE: u32 = 4096;

/// A decoded picture, converted to RGBA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
}

/// A frame that couldn't be decoded (or shown): the caller asks the sender
/// for a keyframe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Undecodable;

/// A live video decoder. Undecodable frames come back as errors, after which
/// the caller asks the sender for a keyframe.
pub struct LiveDecoder {
    inner: Inner,
    scratch: Vec<u8>,
}

enum Inner {
    Vpx(vpx::Decoder),
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    H264(h264::Decoder),
}

impl LiveDecoder {
    /// A decoder for `codec`, on up to four threads; None where this build
    /// can't decode it.
    pub fn new(codec: LiveCodec) -> Option<Self> {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(4));
        Self::with_threads(codec, threads)
    }

    /// A decoder on at most `threads` threads (H264 only; libvpx's VP8 and
    /// VP9 decode on one): several small pictures are better each on one.
    pub fn with_threads(codec: LiveCodec, threads: usize) -> Option<Self> {
        let inner = match codec {
            LiveCodec::Vp8 => Inner::Vpx(vpx::Decoder::new(webm::Codec::Vp8)?),
            LiveCodec::Vp9 => Inner::Vpx(vpx::Decoder::new(webm::Codec::Vp9)?),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            LiveCodec::H264 => Inner::H264(h264::Decoder::new(threads.max(1))?),
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            LiveCodec::H264 => {
                let _ = threads;
                return None;
            }
        };
        Some(Self {
            inner,
            scratch: Vec::new(),
        })
    }

    /// Decodes one whole frame (H264 as an Annex B access unit). With
    /// `convert`, the picture it shows is written to `out` as RGBA, shrunk
    /// to fit `max_side` on its longer side (sides kept even); without, the
    /// frame is only decoded, as frames that are skipped for showing must
    /// still be for the ones after them. `Ok(None)` when there's nothing to
    /// show yet, `Err` when the frame couldn't be decoded.
    pub fn decode(
        &mut self,
        data: &[u8],
        convert: bool,
        max_side: u32,
        out: &mut Vec<u8>,
    ) -> Result<Option<Picture>, Undecodable> {
        let full = match &mut self.inner {
            Inner::Vpx(decoder) => {
                let Some(image) = decoder.decode(data).map_err(|()| Undecodable)? else {
                    return Ok(None);
                };
                let (w, h) = image.size();
                if w > MAX_SIDE || h > MAX_SIDE {
                    return Err(Undecodable);
                }
                if !convert {
                    return Ok(Some(Picture { width: w, height: h }));
                }
                image.to_rgba(None, &mut self.scratch).ok_or(Undecodable)?;
                (w, h)
            }
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            Inner::H264(decoder) => {
                let Some((w, h)) = decoder.decode(data).map_err(|()| Undecodable)? else {
                    return Ok(None);
                };
                if w > MAX_SIDE || h > MAX_SIDE {
                    return Err(Undecodable);
                }
                if !convert {
                    return Ok(Some(Picture { width: w, height: h }));
                }
                decoder.to_rgba(&mut self.scratch).ok_or(Undecodable)?;
                (w, h)
            }
        };
        let (w, h) = fit(full.0, full.1, max_side);
        if (w, h) == full {
            std::mem::swap(out, &mut self.scratch);
        } else {
            vpx::scale_rgba(&self.scratch, full.0, full.1, out, w, h).ok_or(Undecodable)?;
        }
        Ok(Some(Picture { width: w, height: h }))
    }
}

/// `w`×`h` shrunk to fit `max_side`, keeping the aspect ratio and even sides
/// (never enlarged; odd sides of a small picture stay as they are).
fn fit(w: u32, h: u32, max_side: u32) -> (u32, u32) {
    let longest = w.max(h).max(1);
    if longest <= max_side.max(2) {
        return (w, h);
    }
    let scale = |side: u32| ((u64::from(side) * u64::from(max_side) / u64::from(longest)) as u32).max(2) & !1;
    (scale(w), scale(h))
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
mod h264 {
    use super::*;

    /// `AV_INPUT_BUFFER_PADDING_SIZE`: FFmpeg's bitstream readers may read
    /// this far past a packet's end.
    const PADDING: usize = 64;
    const AV_PIX_FMT_YUV420P: c_int = 0;
    const AV_PIX_FMT_YUVJ420P: c_int = 12;

    #[repr(C)]
    struct Opaque {
        _private: [u8; 0],
    }

    /// The leading fields of `AVPacket` (libavcodec/packet.h).
    #[repr(C)]
    pub(super) struct PacketHead {
        pub(super) buf: *mut c_void,
        pub(super) pts: i64,
        pub(super) dts: i64,
        pub(super) data: *mut u8,
        pub(super) size: c_int,
    }

    /// The leading fields of `AVFrame` (libavutil/frame.h).
    #[repr(C)]
    pub(super) struct FrameHead {
        pub(super) data: [*mut u8; 8],
        pub(super) linesize: [c_int; 8],
        pub(super) extended_data: *mut *mut u8,
        pub(super) width: c_int,
        pub(super) height: c_int,
        pub(super) nb_samples: c_int,
        pub(super) format: c_int,
    }

    unsafe extern "C" {
        #[cfg_attr(target_os = "linux", link_name = "LK_avcodec_find_decoder_by_name")]
        fn avcodec_find_decoder_by_name(name: *const c_char) -> *const Opaque;
        #[cfg_attr(target_os = "linux", link_name = "LK_avcodec_alloc_context3")]
        fn avcodec_alloc_context3(codec: *const Opaque) -> *mut Opaque;
        #[cfg_attr(target_os = "linux", link_name = "LK_avcodec_open2")]
        fn avcodec_open2(context: *mut Opaque, codec: *const Opaque, options: *mut *mut Opaque) -> c_int;
        #[cfg_attr(target_os = "linux", link_name = "LK_avcodec_free_context")]
        fn avcodec_free_context(context: *mut *mut Opaque);
        #[cfg_attr(target_os = "linux", link_name = "LK_avcodec_send_packet")]
        fn avcodec_send_packet(context: *mut Opaque, packet: *const PacketHead) -> c_int;
        #[cfg_attr(target_os = "linux", link_name = "LK_avcodec_receive_frame")]
        fn avcodec_receive_frame(context: *mut Opaque, frame: *mut FrameHead) -> c_int;
        #[cfg_attr(target_os = "linux", link_name = "LK_av_packet_alloc")]
        fn av_packet_alloc() -> *mut PacketHead;
        #[cfg_attr(target_os = "linux", link_name = "LK_av_packet_free")]
        fn av_packet_free(packet: *mut *mut PacketHead);
        #[cfg_attr(target_os = "linux", link_name = "LK_av_frame_alloc")]
        fn av_frame_alloc() -> *mut FrameHead;
        #[cfg_attr(target_os = "linux", link_name = "LK_av_frame_free")]
        fn av_frame_free(frame: *mut *mut FrameHead);
        #[cfg_attr(target_os = "linux", link_name = "LK_av_frame_unref")]
        fn av_frame_unref(frame: *mut FrameHead);
        #[cfg_attr(target_os = "linux", link_name = "LK_av_dict_set")]
        fn av_dict_set(dict: *mut *mut Opaque, key: *const c_char, value: *const c_char, flags: c_int) -> c_int;
        #[cfg_attr(target_os = "linux", link_name = "LK_av_dict_free")]
        fn av_dict_free(dict: *mut *mut Opaque);
        #[cfg_attr(target_os = "linux", link_name = "LK_av_log_set_level")]
        fn av_log_set_level(level: c_int);
    }

    /// `AV_LOG_QUIET`: a damaged frame from the network is expected (the
    /// caller asks for a keyframe), not something to print. This FFmpeg is
    /// libwebrtc's own copy, so only its messages go.
    const AV_LOG_QUIET: c_int = -8;
    static QUIET: std::sync::Once = std::sync::Once::new();

    /// One FFmpeg H264 decoder, its packet and its frame; freed on drop.
    pub(super) struct Decoder {
        context: *mut Opaque,
        packet: *mut PacketHead,
        frame: *mut FrameHead,
        /// The frame's bytes and FFmpeg's padding after them.
        padded: Vec<u8>,
        /// Whether `frame` holds a picture.
        holding: bool,
    }

    // SAFETY: an FFmpeg decoder has no thread affinity; it only must not be
    // used from two threads at once, which `&mut self` on every use ensures.
    unsafe impl Send for Decoder {}

    impl Decoder {
        pub(super) fn new(threads: usize) -> Option<Self> {
            let options = [
                ("threads", threads.to_string()),
                // Slices rather than frames: frame threads hold a frame back
                // per thread, which a live picture can't afford.
                ("thread_type", "slice".to_owned()),
                // Show each picture as soon as it's decoded.
                ("flags", "low_delay".to_owned()),
                ("max_pixels", (u64::from(MAX_SIDE) * u64::from(MAX_SIDE)).to_string()),
            ];
            // SAFETY: sets a plain global of libwebrtc's FFmpeg, once.
            QUIET.call_once(|| unsafe { av_log_set_level(AV_LOG_QUIET) });
            let name = CString::new("h264").ok()?;
            // SAFETY: plain FFmpeg calls; every pointer is checked, and what
            // was allocated is freed by `Drop` (null fields are skipped there).
            unsafe {
                let codec = avcodec_find_decoder_by_name(name.as_ptr());
                if codec.is_null() {
                    return None;
                }
                let decoder = Self {
                    context: avcodec_alloc_context3(codec),
                    packet: av_packet_alloc(),
                    frame: av_frame_alloc(),
                    padded: Vec::new(),
                    holding: false,
                };
                if decoder.context.is_null() || decoder.packet.is_null() || decoder.frame.is_null() {
                    return None;
                }
                let mut dict: *mut Opaque = ptr::null_mut();
                for (key, value) in options {
                    let (Ok(key), Ok(value)) = (CString::new(key), CString::new(value)) else {
                        continue;
                    };
                    av_dict_set(&mut dict, key.as_ptr(), value.as_ptr(), 0);
                }
                let opened = avcodec_open2(decoder.context, codec, &mut dict);
                av_dict_free(&mut dict);
                (opened >= 0).then_some(decoder)
            }
        }

        /// Decodes one access unit; the size of the newest picture it gave.
        pub(super) fn decode(&mut self, data: &[u8]) -> Result<Option<(u32, u32)>, ()> {
            let size = c_int::try_from(data.len()).map_err(|_| ())?;
            self.padded.clear();
            self.padded.extend_from_slice(data);
            self.padded.resize(data.len() + PADDING, 0);
            // SAFETY: the packet was allocated in `new`; with no `buf` it
            // doesn't own `data`, and FFmpeg copies what it keeps before
            // `send_packet` returns. The frame is ours between calls.
            unsafe {
                (*self.packet).data = self.padded.as_mut_ptr();
                (*self.packet).size = size;
                let sent = avcodec_send_packet(self.context, self.packet);
                (*self.packet).data = ptr::null_mut();
                (*self.packet).size = 0;
                if sent < 0 {
                    return Err(());
                }
                // Only the newest picture is shown; the one before is let go.
                av_frame_unref(self.frame);
                self.holding = false;
                // With low delay and slice threads, each access unit gives
                // its picture straight away, or none (an error or a gap).
                if avcodec_receive_frame(self.context, self.frame) < 0 {
                    return Ok(None);
                }
                let frame = &*self.frame;
                let (Ok(w), Ok(h)) = (u32::try_from(frame.width), u32::try_from(frame.height)) else {
                    return Err(());
                };
                if !matches!(frame.format, AV_PIX_FMT_YUV420P | AV_PIX_FMT_YUVJ420P) || w == 0 || h == 0 {
                    return Err(());
                }
                self.holding = true;
                let newest = Some((w, h));
                Ok(newest)
            }
        }

        /// Writes the picture last decoded as RGBA.
        pub(super) fn to_rgba(&self, out: &mut Vec<u8>) -> Option<()> {
            if !self.holding {
                return None;
            }
            // SAFETY: `holding` means `frame` has an I420 picture from FFmpeg
            // of its width and height, with these strides.
            unsafe {
                let frame = &*self.frame;
                let full = frame.format == AV_PIX_FMT_YUVJ420P;
                vpx::i420_to_rgba(
                    [frame.data[0], frame.data[1], frame.data[2]],
                    [frame.linesize[0], frame.linesize[1], frame.linesize[2]],
                    (u32::try_from(frame.width).ok()?, u32::try_from(frame.height).ok()?),
                    full,
                    out,
                )
            }
        }
    }

    impl Drop for Decoder {
        fn drop(&mut self) {
            // SAFETY: each was allocated in `new` (or is null, which these
            // functions accept) and is freed only here.
            unsafe {
                avcodec_free_context(&mut self.context);
                av_packet_free(&mut self.packet);
                av_frame_free(&mut self.frame);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VP8: &[u8] = include_bytes!("../tests/fixtures/rgb-vp8.webm");
    /// 32x24, a red keyframe then a lime frame, from ffmpeg's libopenh264:
    /// `ffmpeg -f lavfi -i "color=c=red:s=32x24:r=10:d=0.1[a];color=c=lime:s=32x24:r=10:d=0.1[b];[a][b]concat=n=2,format=yuv420p" -c:v libopenh264 -profile:v constrained_baseline -b:v 200k -g 10 -f h264 rgb-h264.h264`
    const H264: &[u8] = include_bytes!("../tests/fixtures/rgb-h264.h264");

    fn centre(rgba: &[u8], picture: Picture) -> [u8; 3] {
        let (w, h) = (picture.width as usize, picture.height as usize);
        let at = ((h / 2) * w + w / 2) * 4;
        [rgba[at], rgba[at + 1], rgba[at + 2]]
    }

    fn close(px: [u8; 3], want: [u8; 3]) -> bool {
        px.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 16)
    }

    #[test]
    fn decodes_vp8_frames_and_shrinks_them() {
        let demuxed = webm::demux(VP8, 10).unwrap();
        let mut decoder = LiveDecoder::new(LiveCodec::Vp8).unwrap();
        let mut out = Vec::new();
        let first = &VP8[demuxed.blocks[0].data.clone()];
        let picture = decoder.decode(first, true, 1920, &mut out).unwrap().unwrap();
        assert_eq!((picture.width, picture.height), (32, 24));
        assert!(close(centre(&out, picture), [255, 0, 0]), "red");
        let second = &VP8[demuxed.blocks[1].data.clone()];
        let small = decoder.decode(second, true, 16, &mut out).unwrap().unwrap();
        assert_eq!((small.width, small.height), (16, 12));
        assert_eq!(out.len(), 16 * 12 * 4);
        assert!(close(centre(&out, small), [0, 255, 0]), "lime");
        assert!(decoder.decode(&[1, 2, 3], true, 1920, &mut out).is_err());
    }

    #[test]
    fn decodes_h264_where_built_in() {
        let Some(mut decoder) = LiveDecoder::new(LiveCodec::H264) else {
            assert!(!decodes().contains(&LiveCodec::H264));
            return;
        };
        // Split before the second frame's slice (NAL type 1) and its start code.
        let at = (0..H264.len() - 3)
            .find(|&at| H264[at..at + 3] == [0, 0, 1] && H264[at + 3] & 0x1f == 1)
            .unwrap();
        let at = if H264[at - 1] == 0 { at - 1 } else { at };
        let mut out = Vec::new();
        let picture = decoder.decode(&H264[..at], true, 1920, &mut out).unwrap().unwrap();
        assert_eq!((picture.width, picture.height), (32, 24));
        assert!(
            close(centre(&out, picture), [255, 0, 0]),
            "red: {:?}",
            centre(&out, picture)
        );
        // Decoded without converting, as a frame skipped for showing is.
        let skipped = decoder.decode(&H264[at..], false, 1920, &mut out).unwrap();
        assert!(skipped.is_some());
    }

    #[cfg(all(any(target_os = "linux", target_os = "windows"), target_pointer_width = "64"))]
    #[test]
    fn ffmpeg_struct_heads_match_the_c_headers() {
        // Offsets computed from libavcodec/packet.h and libavutil/frame.h.
        assert_eq!(std::mem::offset_of!(h264::PacketHead, data), 24);
        assert_eq!(std::mem::offset_of!(h264::PacketHead, size), 32);
        assert_eq!(std::mem::offset_of!(h264::FrameHead, width), 104);
        assert_eq!(std::mem::offset_of!(h264::FrameHead, format), 116);
    }

    #[test]
    fn fits_keep_aspect_and_even_sides() {
        assert_eq!(fit(3840, 2160, 1920), (1920, 1080));
        assert_eq!(fit(1280, 720, 1920), (1280, 720));
        assert_eq!(fit(1921, 1081, 1920), (1920, 1080));
        assert_eq!(fit(33, 25, 1920), (33, 25));
        assert!(decodes().contains(&LiveCodec::Vp8));
    }
}
