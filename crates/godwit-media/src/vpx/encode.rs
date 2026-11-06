// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! libvpx's VP8 encoder, as compiled into livekit's libwebrtc, for sending
//! live video (Discord's cameras and Go Live). Declared by hand like the
//! decoder: libvpx's `vpx/vpx_encoder.h` and `vpx/vp8cx.h` in
//! `$LK_CUSTOM_WEBRTC/include/third_party/libvpx/source/libvpx`.
//!
//! The settings are Yapfast's `native/vp8.c` (GPL-3.0-or-later): realtime,
//! constant bitrate, no lag, error resilient, a keyframe at least every
//! three seconds.
//!
//! Only the leading part of `vpx_codec_enc_cfg_t` is spelled out, the fields
//! set here; the rest is room the library fills in, larger than it needs.
//! A test checks the fields line up with libvpx's own defaults.

use std::ffi::{c_int, c_long, c_uint, c_ulong, c_void};
use std::ptr;

use super::{VPX_CODEC_OK, VPX_IMG_FMT_I420, VpxCodecCtx, VpxCodecIface, VpxImage};

/// `VPX_ENCODER_ABI_VERSION`: 18 + `VPX_CODEC_ABI_VERSION` (4 + 5) +
/// `VPX_EXT_RATECTRL_ABI_VERSION` (7 + `VPX_TPL_ABI_VERSION` 5).
const VPX_ENCODER_ABI_VERSION: c_int = 18 + (4 + 5) + (7 + 5);
const VPX_CBR: c_int = 1;
const VPX_KF_AUTO: c_int = 1;
const VPX_ERROR_RESILIENT_DEFAULT: u32 = 1;
const VPX_EFLAG_FORCE_KF: c_long = 1;
const VPX_DL_REALTIME: c_ulong = 1;
const VPX_CODEC_CX_FRAME_PKT: c_int = 0;
const VPX_FRAME_IS_KEY: u32 = 1;
/// Controls from `vp8cx.h`'s `vp8e_enc_control_id`.
const VP8E_SET_CPUUSED: c_int = 13;
const VP8E_SET_STATIC_THRESHOLD: c_int = 17;
const VP8E_SET_MAX_INTRA_BITRATE_PCT: c_int = 26;
const VP8E_SET_SCREEN_CONTENT_MODE: c_int = 31;

/// RTP's video clock, which the encoder is timed in.
pub const CLOCK: u32 = 90_000;

/// `vpx_fixed_buf_t`.
#[repr(C)]
struct FixedBuf {
    buf: *mut c_void,
    sz: usize,
}

/// The start of `vpx_codec_enc_cfg_t`, through `kf_max_dist`.
#[repr(C)]
struct EncCfg {
    g_usage: c_uint,
    g_threads: c_uint,
    g_profile: c_uint,
    g_w: c_uint,
    g_h: c_uint,
    g_bit_depth: c_int,
    g_input_bit_depth: c_uint,
    g_timebase: [c_int; 2],
    g_error_resilient: u32,
    g_pass: c_int,
    g_lag_in_frames: c_uint,
    rc_dropframe_thresh: c_uint,
    rc_resize_allowed: c_uint,
    rc_scaled_width: c_uint,
    rc_scaled_height: c_uint,
    rc_resize_up_thresh: c_uint,
    rc_resize_down_thresh: c_uint,
    rc_end_usage: c_int,
    rc_twopass_stats_in: FixedBuf,
    rc_firstpass_mb_stats_in: FixedBuf,
    rc_target_bitrate: c_uint,
    rc_min_quantizer: c_uint,
    rc_max_quantizer: c_uint,
    rc_undershoot_pct: c_uint,
    rc_overshoot_pct: c_uint,
    rc_buf_sz: c_uint,
    rc_buf_initial_sz: c_uint,
    rc_buf_optimal_sz: c_uint,
    rc_2pass_vbr_bias_pct: c_uint,
    rc_2pass_vbr_minsection_pct: c_uint,
    rc_2pass_vbr_maxsection_pct: c_uint,
    rc_2pass_vbr_corpus_complexity: c_uint,
    kf_mode: c_int,
    kf_min_dist: c_uint,
    kf_max_dist: c_uint,
    /// The layer and tuning fields after: 83 words in this libvpx, given
    /// twice the room.
    _rest: [u32; 168],
}

/// The frame part of `vpx_codec_cx_pkt_t`: only read, from the library's
/// packets, so its tail needn't be spelled out.
#[repr(C)]
struct CxPkt {
    kind: c_int,
    buf: *const u8,
    sz: usize,
    pts: i64,
    duration: c_ulong,
    flags: u32,
}

unsafe extern "C" {
    fn vpx_codec_vp8_cx() -> *mut VpxCodecIface;
    fn vpx_codec_enc_config_default(iface: *mut VpxCodecIface, cfg: *mut EncCfg, usage: c_uint) -> c_int;
    fn vpx_codec_enc_init_ver(
        ctx: *mut VpxCodecCtx,
        iface: *mut VpxCodecIface,
        cfg: *const EncCfg,
        flags: c_long,
        ver: c_int,
    ) -> c_int;
    fn vpx_codec_enc_config_set(ctx: *mut VpxCodecCtx, cfg: *const EncCfg) -> c_int;
    fn vpx_codec_control_(ctx: *mut VpxCodecCtx, id: c_int, ...) -> c_int;
    fn vpx_codec_encode(
        ctx: *mut VpxCodecCtx,
        img: *const VpxImage,
        pts: i64,
        duration: c_ulong,
        flags: c_long,
        deadline: c_ulong,
    ) -> c_int;
    fn vpx_codec_get_cx_data(ctx: *mut VpxCodecCtx, iter: *mut *const c_void) -> *const CxPkt;
    fn vpx_img_wrap(
        img: *mut VpxImage,
        fmt: c_int,
        d_w: c_uint,
        d_h: c_uint,
        stride_align: c_uint,
        img_data: *mut u8,
    ) -> *mut VpxImage;
    fn vpx_codec_destroy(ctx: *mut VpxCodecCtx) -> c_int;
}

/// What the video shows, which tunes the encoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Content {
    /// A camera: natural pictures.
    Camera,
    /// A shared screen: text and flat areas that mostly stay still.
    Screen,
}

/// An I420 picture: three planes, each with its stride.
#[derive(Clone, Copy, Debug)]
pub struct Planes<'a> {
    pub width: u32,
    pub height: u32,
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub strides: [u32; 3],
}

/// Why an input picture could not be encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    InvalidPicture,
    Codec(c_int),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPicture => f.write_str("invalid picture size, planes or strides"),
            Self::Codec(code) => write!(f, "video encoding failed ({code})"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// A VP8 encoder for one frame size. Freed on drop.
pub struct Vp8Encoder {
    // Boxed: libvpx keeps both addresses (the context's, and the
    // configuration's, which `vpx_codec_enc_init` remembers).
    ctx: Box<VpxCodecCtx>,
    cfg: Box<EncCfg>,
    width: u32,
    height: u32,
    fps: u32,
    out: Vec<u8>,
}

// SAFETY: as the decoder: no thread affinity, and `&mut self` on every use.
unsafe impl Send for Vp8Encoder {}

impl Vp8Encoder {
    /// An encoder for `width` by `height` pictures (both even) at `fps`,
    /// aiming for `kbps`; None if libvpx refuses the settings.
    pub fn new(width: u32, height: u32, fps: u32, kbps: u32, content: Content) -> Option<Self> {
        if width < 2 || height < 2 || !width.is_multiple_of(2) || !height.is_multiple_of(2) || fps == 0 {
            return None;
        }
        let threads = std::thread::available_parallelism().map_or(1, |cores| cores.get().min(4));
        // SAFETY: an all-zero configuration is a valid starting point (plain
        // integers and null buffers), which the library then fills in.
        let mut cfg: Box<EncCfg> = Box::new(unsafe { std::mem::zeroed() });
        // SAFETY: the interface getter returns a static table; `cfg` has
        // room for the whole of libvpx's configuration.
        if unsafe { vpx_codec_enc_config_default(vpx_codec_vp8_cx(), &mut *cfg, 0) } != VPX_CODEC_OK {
            return None;
        }
        cfg.g_w = width;
        cfg.g_h = height;
        cfg.g_timebase = [1, CLOCK as c_int];
        cfg.g_threads = c_uint::try_from(threads).unwrap_or(1);
        cfg.g_lag_in_frames = 0;
        cfg.g_error_resilient = VPX_ERROR_RESILIENT_DEFAULT;
        cfg.rc_end_usage = VPX_CBR;
        cfg.rc_target_bitrate = kbps.max(1);
        cfg.rc_min_quantizer = 2;
        cfg.rc_max_quantizer = 52;
        cfg.rc_undershoot_pct = 100;
        cfg.rc_overshoot_pct = 15;
        cfg.rc_buf_initial_sz = 500;
        cfg.rc_buf_optimal_sz = 600;
        cfg.rc_buf_sz = 1000;
        cfg.kf_mode = VPX_KF_AUTO;
        // A new viewer waits at most this long for a picture.
        cfg.kf_max_dist = fps * 3;
        let mut ctx = Box::new(VpxCodecCtx {
            name: ptr::null(),
            iface: ptr::null_mut(),
            err: 0,
            err_detail: ptr::null(),
            init_flags: 0,
            config: ptr::null(),
            priv_: ptr::null_mut(),
        });
        // SAFETY: both boxes outlive the encoder, and the ABI version
        // matches the layouts above.
        let err = unsafe { vpx_codec_enc_init_ver(&mut *ctx, vpx_codec_vp8_cx(), &*cfg, 0, VPX_ENCODER_ABI_VERSION) };
        if err != VPX_CODEC_OK {
            return None;
        }
        let screen = c_uint::from(content == Content::Screen);
        // SAFETY: initialised above; each control takes the C type named in
        // vp8cx.h (CPUUSED an int, the rest unsigned ints).
        unsafe {
            vpx_codec_control_(&mut *ctx, VP8E_SET_CPUUSED, -8 as c_int);
            vpx_codec_control_(&mut *ctx, VP8E_SET_SCREEN_CONTENT_MODE, screen);
            vpx_codec_control_(&mut *ctx, VP8E_SET_STATIC_THRESHOLD, 1 as c_uint);
            vpx_codec_control_(&mut *ctx, VP8E_SET_MAX_INTRA_BITRATE_PCT, 300 as c_uint);
        }
        let pixels = width as usize * height as usize;
        Some(Self {
            ctx,
            cfg,
            width,
            height,
            fps,
            // Even a keyframe of noise fits.
            out: Vec::with_capacity(pixels * 2 + 4096),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn bitrate(&self) -> u32 {
        self.cfg.rc_target_bitrate
    }

    /// Aims for `kbps` from the next frame on, without a keyframe.
    pub fn set_bitrate(&mut self, kbps: u32) -> bool {
        let previous = self.cfg.rc_target_bitrate;
        self.cfg.rc_target_bitrate = kbps.max(1);
        // SAFETY: initialised; the configuration stays boxed for libvpx.
        let ok = unsafe { vpx_codec_enc_config_set(&mut *self.ctx, &*self.cfg) } == VPX_CODEC_OK;
        if !ok {
            self.cfg.rc_target_bitrate = previous;
        }
        ok
    }

    /// Encodes a picture of the encoder's size, shown at `pts` in
    /// [`CLOCK`] units: the frame and whether it's a keyframe, or None when
    /// the encoder dropped it to keep to the bitrate.
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
        // SAFETY: an all-zero image is valid input to `vpx_img_wrap`, which
        // fills it in for I420 at this size; the planes are then pointed at
        // the caller's, checked above to hold the picture at their strides.
        let mut image: VpxImage = unsafe { std::mem::zeroed() };
        let wrapped = unsafe {
            vpx_img_wrap(
                &mut image,
                VPX_IMG_FMT_I420,
                self.width,
                self.height,
                1,
                picture.y.as_ptr().cast_mut(),
            )
        };
        if wrapped.is_null() {
            return Err(EncodeError::InvalidPicture);
        }
        image.planes[0] = picture.y.as_ptr().cast_mut();
        image.planes[1] = picture.u.as_ptr().cast_mut();
        image.planes[2] = picture.v.as_ptr().cast_mut();
        for (stride, value) in image.stride.iter_mut().zip([ys, us, vs]) {
            *stride = c_int::try_from(value).map_err(|_| EncodeError::InvalidPicture)?;
        }
        let duration = c_ulong::from(CLOCK / self.fps.max(1));
        let flags = if keyframe { VPX_EFLAG_FORCE_KF } else { 0 };
        // SAFETY: initialised; libvpx reads the planes during the call only
        // (no lag: every frame is encoded before it returns).
        let err = unsafe { vpx_codec_encode(&mut *self.ctx, &image, pts, duration, flags, VPX_DL_REALTIME) };
        if err != VPX_CODEC_OK {
            return Err(EncodeError::Codec(err));
        }
        self.out.clear();
        let mut key = false;
        let mut iter: *const c_void = ptr::null();
        loop {
            // SAFETY: as above; `iter` starts null as the API requires, and
            // each packet stays valid until the next call.
            let packet = unsafe { vpx_codec_get_cx_data(&mut *self.ctx, &mut iter) };
            // SAFETY: null or a packet the library owns until the next call.
            let Some(packet) = (unsafe { packet.as_ref() }) else {
                break;
            };
            if packet.kind != VPX_CODEC_CX_FRAME_PKT || packet.buf.is_null() {
                continue;
            }
            // SAFETY: a frame packet's buffer holds `sz` bytes.
            let data = unsafe { std::slice::from_raw_parts(packet.buf, packet.sz) };
            self.out.extend_from_slice(data);
            key |= packet.flags & VPX_FRAME_IS_KEY != 0;
        }
        Ok((!self.out.is_empty()).then_some((self.out.as_slice(), key)))
    }
}

impl Drop for Vp8Encoder {
    fn drop(&mut self) {
        // SAFETY: initialised in `new` and destroyed only here.
        unsafe {
            vpx_codec_destroy(&mut *self.ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// libvpx's VP8 defaults (vp8_cx_iface.c) read back through the struct
    /// above: if a field were out of place, these would be other numbers.
    #[test]
    fn the_configuration_lines_up_with_libvpx() {
        let mut cfg: Box<EncCfg> = Box::new(unsafe { std::mem::zeroed() });
        assert_eq!(
            unsafe { vpx_codec_enc_config_default(vpx_codec_vp8_cx(), &mut *cfg, 0) },
            VPX_CODEC_OK
        );
        assert_eq!((cfg.g_w, cfg.g_h), (320, 240));
        assert_eq!(cfg.g_timebase, [1, 30]);
        assert_eq!(cfg.rc_target_bitrate, 256);
        assert_eq!((cfg.rc_min_quantizer, cfg.rc_max_quantizer), (4, 63));
        assert_eq!(
            (cfg.rc_buf_sz, cfg.rc_buf_initial_sz, cfg.rc_buf_optimal_sz),
            (6000, 4000, 5000)
        );
        assert_eq!((cfg.kf_mode, cfg.kf_max_dist), (VPX_KF_AUTO, 128));
    }

    /// A moving gradient: a keyframe, deltas, a forced keyframe, at the
    /// size asked, decodable by libvpx's decoder.
    #[test]
    fn encodes_vp8_keyframes_and_deltas_that_decode() {
        let (w, h) = (64u32, 36u32);
        let mut encoder = Vp8Encoder::new(w, h, 30, 500, Content::Camera).unwrap();
        assert!(Vp8Encoder::new(63, 36, 30, 500, Content::Camera).is_none(), "odd sizes");
        let mut decoder = super::super::Decoder::new(crate::webm::Codec::Vp8).unwrap();
        let (cw, ch) = (w as usize / 2, h as usize / 2);
        let mut keys = vec![];
        for frame in 0..4u8 {
            let y: Vec<u8> = (0..w as usize * h as usize)
                .map(|i| (i as u8).wrapping_add(frame * 8))
                .collect();
            let (u, v) = (vec![90u8; cw * ch], vec![200u8; cw * ch]);
            if frame == 1 {
                assert!(encoder.set_bitrate(250));
                assert_eq!(encoder.bitrate(), 250);
            }
            let planes = Planes {
                width: w,
                height: h,
                y: &y,
                u: &u,
                v: &v,
                strides: [w, w / 2, w / 2],
            };
            let pts = i64::from(frame) * 3000;
            let (data, key) = encoder.encode(planes, pts, frame == 2).unwrap().unwrap();
            // A keyframe's tag has bit 0 clear, then the start code and size.
            assert_eq!(key, data[0] & 1 == 0);
            if key {
                assert_eq!(&data[3..6], &[0x9d, 0x01, 0x2a]);
            }
            keys.push(key);
            let data = data.to_vec();
            let image = decoder.decode(&data).unwrap().expect("a picture");
            assert_eq!(image.size(), (w, h));
        }
        assert_eq!(keys[..3], [true, false, true]);
        let small = Planes {
            width: w,
            height: h,
            y: &[0; 10],
            u: &[0; 10],
            v: &[0; 10],
            strides: [w, w / 2, w / 2],
        };
        assert!(encoder.encode(small, 0, false).is_err(), "planes too short");
    }
}
