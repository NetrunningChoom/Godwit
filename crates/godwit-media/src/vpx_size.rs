// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The picture size a VP8 or VP9 frame asks for, read from its header before
//! libvpx sees it. libvpx allocates whatever a frame header claims (up to
//! 16384 × 16384, or 65536² for VP9 without a size limit in its build), so a
//! crafted frame could make Godwit take gigabytes before the size checks
//! after decoding run. Frames claiming more than [`MAX_PIXELS`] are refused
//! here, and so are headers that can't be read.
//!
//! VP8: only keyframes carry a size (RFC 6386 §9.1); other frames keep it.
//! VP9 (the VP9 bitstream spec, §6.2 "Uncompressed header syntax"):
//! keyframes and intra-only frames carry a size; inter frames either reuse
//! a reference frame's size (already checked when that frame came) or carry
//! a new one, which is read here too. Superframes (several frames in one
//! block, §B.3) are checked frame by frame.

use crate::webm::Codec;

/// The most pixels a frame may claim: a 4096 × 4096 picture, as the clip and
/// live decoders accept after decoding.
pub(crate) const MAX_PIXELS: u64 = 4096 * 4096;

/// Whether every frame in `data` asks for a picture within [`MAX_PIXELS`]
/// (or asks for no new size). False for unreadable headers.
pub(crate) fn acceptable(codec: Codec, data: &[u8]) -> bool {
    match codec {
        Codec::Vp8 => vp8(data).is_some_and(fits),
        Codec::Vp9 => superframe(data).is_some_and(|frames| frames.iter().all(|f| vp9(f).is_some_and(fits))),
    }
}

/// None (no new size) fits; a size fits within MAX_PIXELS.
fn fits(size: Option<(u32, u32)>) -> bool {
    size.is_none_or(|(w, h)| w > 0 && h > 0 && u64::from(w) * u64::from(h) <= MAX_PIXELS)
}

/// A VP8 frame's size: Some(None) for an interframe, None when unreadable.
fn vp8(data: &[u8]) -> Option<Option<(u32, u32)>> {
    let tag = *data.first()?;
    // Bit 0 of the frame tag: 0 is a keyframe.
    if tag & 1 == 1 {
        return Some(None);
    }
    let header = data.get(3..10)?;
    if header[..3] != [0x9d, 0x01, 0x2a] {
        return None;
    }
    // 14 bits of size, then 2 of scaling (which only changes how it's shown).
    let w = u32::from(u16::from_le_bytes([header[3], header[4]]) & 0x3fff);
    let h = u32::from(u16::from_le_bytes([header[5], header[6]]) & 0x3fff);
    Some(Some((w, h)))
}

/// The frames in a VP9 block: the superframe index's (§B.3), or the block.
fn superframe(data: &[u8]) -> Option<Vec<&[u8]>> {
    let &marker = data.last()?;
    if marker & 0xe0 != 0xc0 {
        return Some(vec![data]);
    }
    let frames = usize::from(marker & 0x7) + 1;
    let size_bytes = usize::from((marker >> 3) & 0x3) + 1;
    let index_len = 2 + size_bytes * frames;
    let Some(index_start) = data.len().checked_sub(index_len) else {
        return Some(vec![data]);
    };
    // A real index starts with the same marker byte; otherwise the last byte
    // only looked like one, and the block is one frame.
    if data.get(index_start) != Some(&marker) {
        return Some(vec![data]);
    }
    let sizes = data.get(index_start + 1..data.len() - 1)?;
    let mut out = Vec::with_capacity(frames);
    let mut at = 0usize;
    for chunk in sizes.chunks_exact(size_bytes) {
        let len = chunk.iter().rev().fold(0usize, |acc, &b| (acc << 8) | usize::from(b));
        let end = at.checked_add(len)?;
        if end > index_start {
            return None;
        }
        out.push(data.get(at..end)?);
        at = end;
    }
    Some(out)
}

/// Reads bits most significant first, as VP9's headers are written.
struct Bits<'a> {
    data: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.at / 8)?;
        let bit = (byte >> (7 - self.at % 8)) & 1;
        self.at += 1;
        Some(u32::from(bit))
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        (0..n).try_fold(0, |acc, _| Some((acc << 1) | self.bit()?))
    }
}

/// A VP9 frame's size: Some(None) when it asks for none (shown again, or a
/// reference's size), None when unreadable.
fn vp9(data: &[u8]) -> Option<Option<(u32, u32)>> {
    // A zero-length frame in a superframe is skipped by libvpx.
    if data.is_empty() {
        return Some(None);
    }
    let mut r = Bits { data, at: 0 };
    if r.bits(2)? != 2 {
        return None; // frame_marker
    }
    let profile = r.bit()? | (r.bit()? << 1);
    if profile == 3 {
        r.bit()?; // reserved_zero
    }
    if r.bit()? == 1 {
        return Some(None); // show_existing_frame: no new picture
    }
    let key = r.bit()? == 0; // frame_type
    let show_frame = r.bit()?;
    let error_resilient = r.bit()?;
    let sync = |r: &mut Bits| (r.bits(24)? == 0x49_83_42).then_some(());
    let color_config = |r: &mut Bits| -> Option<()> {
        if profile >= 2 {
            r.bit()?; // ten_or_twelve_bit
        }
        let color_space = r.bits(3)?;
        if color_space != 7 {
            r.bit()?; // color_range
            if profile == 1 || profile == 3 {
                r.bits(3)?; // subsampling_x, subsampling_y, reserved_zero
            }
        } else if profile == 1 || profile == 3 {
            r.bit()?; // reserved_zero
        }
        Some(())
    };
    let frame_size = |r: &mut Bits| Some((r.bits(16)? + 1, r.bits(16)? + 1));
    if key {
        sync(&mut r)?;
        color_config(&mut r)?;
        return Some(Some(frame_size(&mut r)?));
    }
    let intra_only = if show_frame == 0 { r.bit()? } else { 0 };
    if error_resilient == 0 {
        r.bits(2)?; // reset_frame_context
    }
    if intra_only == 1 {
        sync(&mut r)?;
        if profile > 0 {
            color_config(&mut r)?;
        }
        r.bits(8)?; // refresh_frame_flags
        return Some(Some(frame_size(&mut r)?));
    }
    r.bits(8)?; // refresh_frame_flags
    for _ in 0..3 {
        r.bits(4)?; // ref_frame_idx, ref_frame_sign_bias
    }
    // frame_size_with_refs: a reference's size, or a new one.
    for _ in 0..3 {
        if r.bit()? == 1 {
            return Some(None);
        }
    }
    Some(Some(frame_size(&mut r)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp8_key(w: u16, h: u16) -> Vec<u8> {
        let mut f = vec![0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a];
        f.extend_from_slice(&w.to_le_bytes());
        f.extend_from_slice(&h.to_le_bytes());
        f.extend_from_slice(&[0; 8]);
        f
    }

    /// Writes bits most significant first.
    fn pack(bits: &[(u32, u32)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut at = 0;
        for &(value, n) in bits {
            for i in (0..n).rev() {
                if at % 8 == 0 {
                    out.push(0);
                }
                let bit = ((value >> i) & 1) as u8;
                if let Some(last) = out.last_mut() {
                    *last |= bit << (7 - at % 8);
                }
                at += 1;
            }
        }
        out.extend_from_slice(&[0; 8]);
        out
    }

    /// A profile 0 VP9 keyframe header asking for `w` × `h`.
    fn vp9_key(w: u32, h: u32) -> Vec<u8> {
        pack(&[
            (2, 2), // frame_marker
            (0, 2), // profile 0
            (0, 1), // show_existing_frame
            (0, 1), // keyframe
            (1, 1), // show_frame
            (0, 1), // error_resilient_mode
            (0x49_83_42, 24),
            (1, 3), // color_space BT.601
            (0, 1), // color_range
            (w - 1, 16),
            (h - 1, 16),
        ])
    }

    /// A profile 0 VP9 inter frame: a reference's size, or `size`.
    fn vp9_inter(size: Option<(u32, u32)>) -> Vec<u8> {
        let mut bits = vec![(2, 2), (0, 2), (0, 1), (1, 1), (1, 1), (0, 1), (0, 2), (1, 8), (0, 12)];
        match size {
            Some((w, h)) => bits.extend([(0, 3), (w - 1, 16), (h - 1, 16)]),
            None => bits.push((1, 1)),
        }
        pack(&bits)
    }

    #[test]
    fn vp8_keyframes_are_held_to_the_limit() {
        assert!(acceptable(Codec::Vp8, &vp8_key(1920, 1080)));
        assert!(!acceptable(Codec::Vp8, &vp8_key(16383, 16383)));
        assert!(!acceptable(Codec::Vp8, &vp8_key(0, 1080)));
        // Interframes carry no size.
        assert!(acceptable(Codec::Vp8, &[0x01, 0, 0]));
        // Truncated or without the start code.
        assert!(!acceptable(Codec::Vp8, &vp8_key(64, 64)[..7]));
        assert!(!acceptable(Codec::Vp8, &[0x10, 0x02, 0x00, 1, 2, 3, 0, 1, 0, 1]));
        assert!(!acceptable(Codec::Vp8, &[]));
    }

    #[test]
    fn vp9_sizes_are_read_from_every_kind_of_frame() {
        assert_eq!(vp9(&vp9_key(640, 360)), Some(Some((640, 360))));
        assert!(acceptable(Codec::Vp9, &vp9_key(4096, 4096)));
        assert!(!acceptable(Codec::Vp9, &vp9_key(65536, 65536)));
        // An inter frame that brings a new size, and one that reuses a reference's.
        assert_eq!(vp9(&vp9_inter(Some((320, 240)))), Some(Some((320, 240))));
        assert!(!acceptable(Codec::Vp9, &vp9_inter(Some((60000, 60000)))));
        assert!(acceptable(Codec::Vp9, &vp9_inter(None)));
        assert!(!acceptable(Codec::Vp9, &[0x00]));
    }

    #[test]
    fn vp9_superframes_are_checked_frame_by_frame() {
        let (a, b) = (vp9_key(64, 64), vp9_inter(Some((60000, 60000))));
        let block = |second: &[u8]| {
            let mut block = a.clone();
            block.extend_from_slice(second);
            // Two frames, one-byte sizes: marker 0b110_00_001.
            let marker = 0xc1;
            block.extend_from_slice(&[marker, a.len() as u8, second.len() as u8, marker]);
            block
        };
        assert!(!acceptable(Codec::Vp9, &block(&b)));
        assert!(acceptable(Codec::Vp9, &block(&vp9_inter(None))));
    }
}
