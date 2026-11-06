// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Hand-written bindings to the libvpx decoder and the few libyuv routines
//! used, as compiled into livekit's libwebrtc. Declared by hand rather than
//! through bindgen: it's a handful of functions, and bindgen would need
//! libclang on every build machine.
//!
//! Layouts follow libvpx's `vpx/vpx_codec.h`, `vpx_decoder.h` and
//! `vpx_image.h` (VPX_IMAGE_ABI_VERSION 5) and libyuv's `convert_argb.h` and
//! `scale_argb.h` in `$LK_CUSTOM_WEBRTC/include/third_party`.

use std::ffi::{c_char, c_int, c_long, c_uint, c_void};
use std::ptr;

use crate::webm::Codec;

pub(crate) mod encode;

/// `VPX_DECODER_ABI_VERSION` = 3 + `VPX_CODEC_ABI_VERSION`, which is
/// 4 + `VPX_IMAGE_ABI_VERSION` (5). A mismatch makes init fail cleanly
/// with `VPX_CODEC_ABI_MISMATCH` rather than corrupt memory.
const VPX_DECODER_ABI_VERSION: c_int = 3 + 4 + 5;
const VPX_CODEC_OK: c_int = 0;

const VPX_IMG_FMT_PLANAR: c_int = 0x100;
const VPX_IMG_FMT_UV_FLIP: c_int = 0x200;
const VPX_IMG_FMT_I420: c_int = VPX_IMG_FMT_PLANAR | 2;
const VPX_IMG_FMT_YV12: c_int = VPX_IMG_FMT_PLANAR | VPX_IMG_FMT_UV_FLIP | 1;
const VPX_IMG_FMT_I422: c_int = VPX_IMG_FMT_PLANAR | 5;
const VPX_IMG_FMT_I444: c_int = VPX_IMG_FMT_PLANAR | 6;

const VPX_CS_BT_709: c_int = 2;
const VPX_CS_BT_2020: c_int = 5;
const VPX_CR_FULL_RANGE: c_int = 1;

/// libyuv's `kFilterBox`: averaging, the best for shrinking.
const FILTER_BOX: c_int = 3;

#[repr(C)]
struct VpxCodecIface {
    _opaque: [u8; 0],
}

#[repr(C)]
struct YuvConstants {
    _opaque: [u8; 0],
}

/// `vpx_codec_ctx_t`. `init_flags` is a C `long`: 32-bit on Windows.
#[repr(C)]
struct VpxCodecCtx {
    name: *const c_char,
    iface: *mut VpxCodecIface,
    err: c_int,
    err_detail: *const c_char,
    init_flags: c_long,
    config: *const c_void,
    priv_: *mut c_void,
}

/// `vpx_codec_dec_cfg_t`.
#[repr(C)]
struct VpxCodecDecCfg {
    threads: c_uint,
    w: c_uint,
    h: c_uint,
}

/// `vpx_image_t`.
#[repr(C)]
pub(crate) struct VpxImage {
    fmt: c_int,
    cs: c_int,
    range: c_int,
    w: c_uint,
    h: c_uint,
    bit_depth: c_uint,
    d_w: c_uint,
    d_h: c_uint,
    r_w: c_uint,
    r_h: c_uint,
    x_chroma_shift: c_uint,
    y_chroma_shift: c_uint,
    planes: [*mut u8; 4],
    stride: [c_int; 4],
    bps: c_int,
    user_priv: *mut c_void,
    img_data: *mut u8,
    img_data_owner: c_int,
    self_allocd: c_int,
    fb_priv: *mut c_void,
}

type ToArgbMatrix = unsafe extern "C" fn(
    *const u8,
    c_int,
    *const u8,
    c_int,
    *const u8,
    c_int,
    *mut u8,
    c_int,
    *const YuvConstants,
    c_int,
    c_int,
) -> c_int;

type AlphaToArgbMatrix = unsafe extern "C" fn(
    *const u8,
    c_int,
    *const u8,
    c_int,
    *const u8,
    c_int,
    *const u8,
    c_int,
    *mut u8,
    c_int,
    *const YuvConstants,
    c_int,
    c_int,
    c_int,
) -> c_int;

unsafe extern "C" {
    fn vpx_codec_vp8_dx() -> *mut VpxCodecIface;
    fn vpx_codec_vp9_dx() -> *mut VpxCodecIface;
    fn vpx_codec_dec_init_ver(
        ctx: *mut VpxCodecCtx,
        iface: *mut VpxCodecIface,
        cfg: *const VpxCodecDecCfg,
        flags: c_long,
        ver: c_int,
    ) -> c_int;
    fn vpx_codec_decode(
        ctx: *mut VpxCodecCtx,
        data: *const u8,
        data_sz: c_uint,
        user_priv: *mut c_void,
        deadline: c_long,
    ) -> c_int;
    fn vpx_codec_get_frame(ctx: *mut VpxCodecCtx, iter: *mut *const c_void) -> *mut VpxImage;
    fn vpx_codec_destroy(ctx: *mut VpxCodecCtx) -> c_int;

    fn I420ToARGBMatrix(
        y: *const u8,
        ys: c_int,
        u: *const u8,
        us: c_int,
        v: *const u8,
        vs: c_int,
        dst: *mut u8,
        ds: c_int,
        k: *const YuvConstants,
        w: c_int,
        h: c_int,
    ) -> c_int;
    fn I422ToARGBMatrix(
        y: *const u8,
        ys: c_int,
        u: *const u8,
        us: c_int,
        v: *const u8,
        vs: c_int,
        dst: *mut u8,
        ds: c_int,
        k: *const YuvConstants,
        w: c_int,
        h: c_int,
    ) -> c_int;
    fn I444ToARGBMatrix(
        y: *const u8,
        ys: c_int,
        u: *const u8,
        us: c_int,
        v: *const u8,
        vs: c_int,
        dst: *mut u8,
        ds: c_int,
        k: *const YuvConstants,
        w: c_int,
        h: c_int,
    ) -> c_int;
    fn I420AlphaToARGBMatrix(
        y: *const u8,
        ys: c_int,
        u: *const u8,
        us: c_int,
        v: *const u8,
        vs: c_int,
        a: *const u8,
        a_s: c_int,
        dst: *mut u8,
        ds: c_int,
        k: *const YuvConstants,
        w: c_int,
        h: c_int,
        attenuate: c_int,
    ) -> c_int;
    fn I422AlphaToARGBMatrix(
        y: *const u8,
        ys: c_int,
        u: *const u8,
        us: c_int,
        v: *const u8,
        vs: c_int,
        a: *const u8,
        a_s: c_int,
        dst: *mut u8,
        ds: c_int,
        k: *const YuvConstants,
        w: c_int,
        h: c_int,
        attenuate: c_int,
    ) -> c_int;
    fn I444AlphaToARGBMatrix(
        y: *const u8,
        ys: c_int,
        u: *const u8,
        us: c_int,
        v: *const u8,
        vs: c_int,
        a: *const u8,
        a_s: c_int,
        dst: *mut u8,
        ds: c_int,
        k: *const YuvConstants,
        w: c_int,
        h: c_int,
        attenuate: c_int,
    ) -> c_int;
    fn ARGBScale(
        src: *const u8,
        src_stride: c_int,
        src_w: c_int,
        src_h: c_int,
        dst: *mut u8,
        dst_stride: c_int,
        dst_w: c_int,
        dst_h: c_int,
        filter: c_int,
    ) -> c_int;

    // The "Yvu" constants are the matrices with U and V swapped: libyuv's
    // ARGB is B,G,R,A in memory, so converting with U and V (and the
    // matrix) swapped writes R,G,B,A, which is what Slint's Rgba8Pixel
    // wants. This is exactly how libyuv's own I420ToABGR is defined.
    static kYvuI601Constants: YuvConstants;
    static kYvuJPEGConstants: YuvConstants;
    static kYvuH709Constants: YuvConstants;
    static kYvuF709Constants: YuvConstants;
    static kYvu2020Constants: YuvConstants;
    static kYvuV2020Constants: YuvConstants;
}

/// One libvpx decoder. Freed on drop, so every early return releases it.
pub(crate) struct Decoder {
    // Boxed: libvpx is handed this address at init and may keep it.
    ctx: Box<VpxCodecCtx>,
    /// Which headers to read before decoding (crate::vpx_size).
    codec: Codec,
}

impl Decoder {
    pub(crate) fn new(codec: Codec) -> Option<Self> {
        let mut ctx = Box::new(VpxCodecCtx {
            name: ptr::null(),
            iface: ptr::null_mut(),
            err: 0,
            err_detail: ptr::null(),
            init_flags: 0,
            config: ptr::null(),
            priv_: ptr::null_mut(),
        });
        // One thread: this runs on one of the caller's image workers, which
        // already decode in parallel with each other.
        let cfg = VpxCodecDecCfg { threads: 1, w: 0, h: 0 };
        // SAFETY: the interface getters take no arguments and return static
        // tables; `ctx` and `cfg` are valid for the call, and the ABI
        // version matches the struct layouts above.
        let err = unsafe {
            let iface = match codec {
                Codec::Vp8 => vpx_codec_vp8_dx(),
                Codec::Vp9 => vpx_codec_vp9_dx(),
            };
            vpx_codec_dec_init_ver(&mut *ctx, iface, &cfg, 0, VPX_DECODER_ABI_VERSION)
        };
        // On failure libvpx frees what it allocated; destroy must not run.
        (err == VPX_CODEC_OK).then_some(Self { ctx, codec })
    }

    /// Decodes one compressed frame; the shown picture, if any. Hidden
    /// frames (VP8's alt-ref) give none. The image lives until the next call.
    pub(crate) fn decode(&mut self, data: &[u8]) -> Result<Option<&VpxImage>, ()> {
        // libvpx allocates whatever size a header claims: refuse oversized
        // (or unreadable) frames before it does. Video comes from strangers.
        if !crate::vpx_size::acceptable(self.codec, data) {
            return Err(());
        }
        let len = c_uint::try_from(data.len()).map_err(|_| ())?;
        // SAFETY: the context was initialised; `data` outlives the call and
        // libvpx copies what it keeps.
        let err = unsafe { vpx_codec_decode(&mut *self.ctx, data.as_ptr(), len, ptr::null_mut(), 0) };
        if err != VPX_CODEC_OK {
            return Err(());
        }
        let mut iter: *const c_void = ptr::null();
        let mut last = ptr::null_mut();
        loop {
            // SAFETY: as above; `iter` starts null as the API requires.
            let img = unsafe { vpx_codec_get_frame(&mut *self.ctx, &mut iter) };
            if img.is_null() {
                break;
            }
            last = img;
        }
        // SAFETY: a non-null image stays valid until the decoder is used
        // again, which the `&mut self` borrow on the result prevents.
        Ok(unsafe { last.as_ref() })
    }
}

// SAFETY: a libvpx decoder context has no thread affinity (no thread-locals,
// no callbacks); it only must not be used from two threads at once, which
// `&mut self` on every use guarantees. This lets a stream move to a worker.
unsafe impl Send for Decoder {}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: initialised in `new` and destroyed only here.
        unsafe {
            vpx_codec_destroy(&mut *self.ctx);
        }
    }
}

impl VpxImage {
    /// The displayed size.
    pub(crate) fn size(&self) -> (u32, u32) {
        (self.d_w, self.d_h)
    }

    /// The matrix for this image's colour space and range (BT.601 when
    /// unspecified, as browsers assume for WebM).
    fn constants(&self) -> *const YuvConstants {
        let full = self.range == VPX_CR_FULL_RANGE;
        // Taking an extern static's address is safe; only reading it isn't.
        match (self.cs, full) {
            (VPX_CS_BT_709, false) => &raw const kYvuH709Constants,
            (VPX_CS_BT_709, true) => &raw const kYvuF709Constants,
            (VPX_CS_BT_2020, false) => &raw const kYvu2020Constants,
            (VPX_CS_BT_2020, true) => &raw const kYvuV2020Constants,
            (_, false) => &raw const kYvuI601Constants,
            (_, true) => &raw const kYvuJPEGConstants,
        }
    }

    /// Writes the picture as RGBA (`d_w * d_h * 4` bytes) into `out`, alpha
    /// from `alpha`'s luma when given and the same size, else opaque.
    /// `None` for formats libyuv can't convert here (high bit depth, 4:4:0).
    pub(crate) fn to_rgba(&self, alpha: Option<&VpxImage>, out: &mut Vec<u8>) -> Option<()> {
        type Plain = ToArgbMatrix;
        let (plain, with_alpha): (Plain, AlphaToArgbMatrix) = match self.fmt {
            VPX_IMG_FMT_I420 | VPX_IMG_FMT_YV12 => (I420ToARGBMatrix, I420AlphaToARGBMatrix),
            VPX_IMG_FMT_I422 => (I422ToARGBMatrix, I422AlphaToARGBMatrix),
            VPX_IMG_FMT_I444 => (I444ToARGBMatrix, I444AlphaToARGBMatrix),
            _ => return None,
        };
        let (w, h) = (c_int::try_from(self.d_w).ok()?, c_int::try_from(self.d_h).ok()?);
        if w <= 0 || h <= 0 || self.planes[..3].iter().any(|p| p.is_null()) {
            return None;
        }
        let len = (self.d_w as usize).checked_mul(self.d_h as usize)?.checked_mul(4)?;
        out.clear();
        out.resize(len, 0);
        let alpha =
            alpha.filter(|a| a.d_w == self.d_w && a.d_h == self.d_h && !a.planes[0].is_null() && a.stride[0] > 0);
        // libvpx's planes[1] is U and planes[2] V for every format (YV12's
        // flip only changes their order in memory); they're passed swapped
        // to pair with the Yvu matrix (see the constants above).
        let (y, u, v) = (self.planes[0], self.planes[1], self.planes[2]);
        let (ys, us, vs) = (self.stride[0], self.stride[1], self.stride[2]);
        // SAFETY: the planes come from libvpx for an image of `w` x `h` in
        // this format with these strides; `out` holds `w * h * 4` bytes.
        let rc = unsafe {
            match alpha {
                Some(a) => with_alpha(
                    y,
                    ys,
                    v,
                    vs,
                    u,
                    us,
                    a.planes[0],
                    a.stride[0],
                    out.as_mut_ptr(),
                    w * 4,
                    self.constants(),
                    w,
                    h,
                    0,
                ),
                None => plain(y, ys, v, vs, u, us, out.as_mut_ptr(), w * 4, self.constants(), w, h),
            }
        };
        (rc == 0).then_some(())
    }
}

/// Writes an I420 picture from another decoder (`planes` Y, U, V with their
/// `strides`) as RGBA into `out`: BT.601, limited range unless `full`, as
/// libwebrtc's own I420 conversion assumes.
///
/// # Safety
///
/// The planes must hold a `width` x `height` I420 picture with these strides.
pub(crate) unsafe fn i420_to_rgba(
    planes: [*mut u8; 3],
    strides: [c_int; 3],
    (width, height): (u32, u32),
    full: bool,
    out: &mut Vec<u8>,
) -> Option<()> {
    let (w, h) = (c_int::try_from(width).ok()?, c_int::try_from(height).ok()?);
    if w <= 0 || h <= 0 || planes.iter().any(|p| p.is_null()) {
        return None;
    }
    out.clear();
    out.resize((width as usize).checked_mul(height as usize)?.checked_mul(4)?, 0);
    let constants = if full {
        &raw const kYvuJPEGConstants
    } else {
        &raw const kYvuI601Constants
    };
    // SAFETY: the caller vouches for the planes; `out` holds `w * h * 4`
    // bytes. U and V go swapped to pair with the Yvu matrix (see above).
    let rc = unsafe {
        I420ToARGBMatrix(
            planes[0],
            strides[0],
            planes[2],
            strides[2],
            planes[1],
            strides[1],
            out.as_mut_ptr(),
            w * 4,
            constants,
            w,
            h,
        )
    };
    (rc == 0).then_some(())
}

/// Box-filtered resize of RGBA pixels into `dst` (libyuv treats them as
/// opaque 4-byte pixels, so the channel order doesn't matter).
pub(crate) fn scale_rgba(src: &[u8], sw: u32, sh: u32, dst: &mut Vec<u8>, dw: u32, dh: u32) -> Option<()> {
    let src_len = (sw as usize).checked_mul(sh as usize)?.checked_mul(4)?;
    if src.len() < src_len {
        return None;
    }
    let [sw, sh, dw, dh] = [sw, sh, dw, dh].map(|v| c_int::try_from(v).unwrap_or(0));
    if sw <= 0 || sh <= 0 || dw <= 0 || dh <= 0 {
        return None;
    }
    dst.clear();
    dst.resize((dw as usize).checked_mul(dh as usize)?.checked_mul(4)?, 0);
    // SAFETY: both buffers were checked to hold their width x height pixels.
    let rc = unsafe {
        ARGBScale(
            src.as_ptr(),
            sw * 4,
            sw,
            sh,
            dst.as_mut_ptr(),
            dw * 4,
            dw,
            dh,
            FILTER_BOX,
        )
    };
    (rc == 0).then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layouts_match_the_c_headers() {
        // 64-bit Unix (LP64) sizes, computed from the header definitions.
        if cfg!(all(target_pointer_width = "64", not(windows))) {
            assert_eq!(std::mem::size_of::<VpxCodecCtx>(), 56);
            assert_eq!(std::mem::offset_of!(VpxImage, planes), 48);
            assert_eq!(std::mem::offset_of!(VpxImage, stride), 80);
            assert_eq!(std::mem::size_of::<VpxImage>(), 136);
        }
        assert_eq!(std::mem::size_of::<VpxCodecDecCfg>(), 12);
    }

    #[test]
    fn both_decoders_initialise_and_free() {
        assert!(Decoder::new(Codec::Vp8).is_some());
        assert!(Decoder::new(Codec::Vp9).is_some());
    }
}
