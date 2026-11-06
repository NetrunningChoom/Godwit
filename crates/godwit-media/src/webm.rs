// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A minimal WebM (Matroska) demuxer: just enough to pull one video track's
//! frames and timestamps out of a GIF-style clip. Tenor's files are simple
//! (one video track, SimpleBlocks in Clusters), so this walks the file once
//! instead of pulling in a general Matroska crate.

use std::ops::Range;

/// Element IDs, from the Matroska specification (RFC 9559).
mod id {
    pub const EBML: u32 = 0x1A45_DFA3;
    pub const DOC_TYPE: u32 = 0x4282;
    pub const SEGMENT: u32 = 0x1853_8067;
    pub const INFO: u32 = 0x1549_A966;
    pub const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
    pub const DURATION: u32 = 0x4489;
    pub const TRACKS: u32 = 0x1654_AE6B;
    pub const TRACK_ENTRY: u32 = 0xAE;
    pub const TRACK_NUMBER: u32 = 0xD7;
    pub const TRACK_TYPE: u32 = 0x83;
    pub const CODEC_ID: u32 = 0x86;
    pub const DEFAULT_DURATION: u32 = 0x23_E383;
    pub const CLUSTER: u32 = 0x1F43_B675;
    pub const TIMESTAMP: u32 = 0xE7;
    pub const SIMPLE_BLOCK: u32 = 0xA3;
    pub const BLOCK_GROUP: u32 = 0xA0;
    pub const BLOCK: u32 = 0xA1;
    pub const BLOCK_DURATION: u32 = 0x9B;
    pub const BLOCK_ADDITIONS: u32 = 0x75A1;
    pub const BLOCK_MORE: u32 = 0xA6;
    pub const BLOCK_ADD_ID: u32 = 0xEE;
    pub const BLOCK_ADDITIONAL: u32 = 0xA5;
}

/// Where `part`, a subslice of `base`, lies within it.
fn range_in(base: &[u8], part: &[u8]) -> Range<usize> {
    let start = part.as_ptr().addr().saturating_sub(base.as_ptr().addr());
    start..start + part.len()
}

/// The magic every EBML file starts with.
const MAGIC: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Codec {
    Vp8,
    Vp9,
}

#[derive(Debug)]
pub(crate) struct Track {
    number: u64,
    pub codec: Codec,
    /// Nanoseconds per frame, if the muxer wrote it.
    pub default_duration_ns: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct Block {
    /// Presentation time in milliseconds.
    pub pts_ms: f64,
    /// The block's own duration in milliseconds (BlockGroup only).
    pub duration_ms: Option<f64>,
    /// Where the compressed frame lies in the file: ranges rather than
    /// slices, so a stream can own the file and its index together.
    pub data: Range<usize>,
    /// The alpha channel as a second VP8/VP9 stream (BlockAddID 1), as
    /// WebM stores transparent video.
    pub alpha: Option<Range<usize>>,
}

#[derive(Debug)]
pub(crate) struct Demuxed {
    pub track: Track,
    pub blocks: Vec<Block>,
    /// The segment's duration in milliseconds, if known.
    pub duration_ms: Option<f64>,
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn byte(&mut self) -> Option<u8> {
        let b = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    /// An EBML variable-length integer: the leading zeros of the first byte
    /// give the length. Returns the value without its length marker, and
    /// whether all value bits were set (an "unknown" size).
    fn vint(&mut self, max_len: u32) -> Option<(u64, u64, bool)> {
        let first = self.byte()?;
        let len = first.leading_zeros() + 1;
        if len > max_len {
            return None;
        }
        let mut raw = u64::from(first);
        for _ in 1..len {
            raw = (raw << 8) | u64::from(self.byte()?);
        }
        let bits = 7 * len;
        let mask = (1u64 << bits) - 1;
        let value = raw & mask;
        Some((raw, value, value == mask))
    }

    /// An element header: its ID (marker kept, as the spec writes IDs) and
    /// its size, `None` when unknown (live-streamed Segments and Clusters).
    fn header(&mut self) -> Option<(u32, Option<u64>)> {
        let (raw_id, _, _) = self.vint(4)?;
        let id = u32::try_from(raw_id).ok()?;
        let (_, size, unknown) = self.vint(8)?;
        Some((id, (!unknown).then_some(size)))
    }

    /// The body of an element with a known size that fits in the buffer.
    fn body(&mut self, size: Option<u64>) -> Option<&'a [u8]> {
        self.take(usize::try_from(size?).ok()?)
    }
}

fn uint(body: &[u8]) -> Option<u64> {
    if body.len() > 8 {
        return None;
    }
    Some(body.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b)))
}

fn float(body: &[u8]) -> Option<f64> {
    match body.len() {
        4 => Some(f64::from(f32::from_be_bytes(body.try_into().ok()?))),
        8 => Some(f64::from_be_bytes(body.try_into().ok()?)),
        _ => None,
    }
}

/// The DocType from a leading EBML header, and where the header ends.
fn doc_type(r: &mut Reader<'_>) -> Option<String> {
    if r.buf.get(..4)? != MAGIC {
        return None;
    }
    let (id, size) = r.header()?;
    if id != id::EBML {
        return None;
    }
    let mut header = Reader::new(r.body(size)?);
    let mut doc_type = None;
    while !header.at_end() {
        let (id, size) = header.header()?;
        let body = header.body(size)?;
        if id == id::DOC_TYPE {
            // Strings may be zero-padded.
            let end = body.iter().position(|b| *b == 0).unwrap_or(body.len());
            doc_type = Some(String::from_utf8_lossy(&body[..end]).into_owned());
        }
    }
    doc_type
}

/// Whether `bytes` start like a WebM file (EBML header with DocType "webm").
pub(crate) fn is_webm(bytes: &[u8]) -> bool {
    doc_type(&mut Reader::new(bytes)).is_some_and(|t| t == "webm")
}

/// The first VP8/VP9 video track in a Tracks element.
fn video_track(tracks: &[u8]) -> Option<Track> {
    let mut r = Reader::new(tracks);
    while !r.at_end() {
        let (id, size) = r.header()?;
        let body = r.body(size)?;
        if id != id::TRACK_ENTRY {
            continue;
        }
        let mut entry = Reader::new(body);
        // The Video child's pixel sizes are not read: the decoded frames' own
        // size is what counts (VP9 may even change it mid-stream).
        let (mut number, mut kind, mut codec, mut default_duration_ns) = (None, None, None, None);
        while !entry.at_end() {
            let Some((id, size)) = entry.header() else {
                break;
            };
            let Some(body) = entry.body(size) else {
                break;
            };
            match id {
                id::TRACK_NUMBER => number = uint(body),
                id::TRACK_TYPE => kind = uint(body),
                id::CODEC_ID => {
                    codec = match body {
                        b"V_VP8" => Some(Codec::Vp8),
                        b"V_VP9" => Some(Codec::Vp9),
                        _ => None,
                    }
                }
                id::DEFAULT_DURATION => default_duration_ns = uint(body).filter(|d| *d > 0),
                _ => {}
            }
        }
        // TrackType 1 is video.
        if let (Some(number), Some(1), Some(codec)) = (number, kind, codec) {
            return Some(Track {
                number,
                codec,
                default_duration_ns,
            });
        }
    }
    None
}

/// A Block or SimpleBlock body: track number, timecode relative to the
/// cluster, flags, then the frame. Laced blocks (several frames in one) are
/// skipped: muxers only lace audio.
fn block_frame(body: &[u8]) -> Option<(u64, i16, &[u8])> {
    let mut r = Reader::new(body);
    let (_, track, _) = r.vint(8)?;
    let rel = i16::from_be_bytes(r.take(2)?.try_into().ok()?);
    let flags = r.byte()?;
    if flags & 0x06 != 0 {
        return None;
    }
    let data = r.buf.get(r.pos..)?;
    (!data.is_empty()).then_some((track, rel, data))
}

/// The alpha stream (BlockAddID 1) from a BlockAdditions element.
fn alpha_addition(body: &[u8]) -> Option<&[u8]> {
    let mut r = Reader::new(body);
    while !r.at_end() {
        let (id, size) = r.header()?;
        let more = r.body(size)?;
        if id != id::BLOCK_MORE {
            continue;
        }
        let mut m = Reader::new(more);
        // BlockAddID defaults to 1.
        let (mut add_id, mut data) = (1, None);
        while !m.at_end() {
            let (id, size) = m.header()?;
            let body = m.body(size)?;
            match id {
                id::BLOCK_ADD_ID => add_id = uint(body)?,
                id::BLOCK_ADDITIONAL => data = Some(body),
                _ => {}
            }
        }
        if add_id == 1 && data.is_some() {
            return data;
        }
    }
    None
}

/// Splits a WebM/Matroska file into its first video track's frames, at most
/// `max_blocks`. Stops quietly at truncated or malformed data, keeping what
/// came before; `None` when there is no usable video track or frame.
pub(crate) fn demux(bytes: &[u8], max_blocks: usize) -> Option<Demuxed> {
    let mut r = Reader::new(bytes);
    let doc = doc_type(&mut r)?;
    if doc != "webm" && doc != "matroska" {
        return None;
    }
    // Default TimestampScale: one tick is a millisecond.
    let mut scale_ns: u64 = 1_000_000;
    let mut duration_ticks: Option<f64> = None;
    let mut track: Option<Track> = None;
    let mut cluster_ts: u64 = 0;
    let mut blocks = Vec::new();
    let ticks_to_ms = |ticks: f64, scale_ns: u64| ticks * scale_ns as f64 / 1_000_000.0;

    // One flat walk: Segment and Cluster are entered rather than skipped, so
    // their sizes may be unknown (as in live-muxed files) or wrong after a
    // truncated download; everything else must fit.
    while !r.at_end() && blocks.len() < max_blocks {
        let Some((id, size)) = r.header() else {
            break;
        };
        if matches!(id, id::SEGMENT | id::CLUSTER) {
            if id == id::CLUSTER {
                cluster_ts = 0;
            }
            continue;
        }
        let Some(body) = r.body(size) else {
            break;
        };
        match id {
            id::INFO => {
                let mut info = Reader::new(body);
                while !info.at_end() {
                    let Some((id, size)) = info.header() else {
                        break;
                    };
                    let Some(body) = info.body(size) else {
                        break;
                    };
                    match id {
                        id::TIMESTAMP_SCALE => {
                            if let Some(s) = uint(body).filter(|s| *s > 0) {
                                scale_ns = s;
                            }
                        }
                        id::DURATION => duration_ticks = float(body),
                        _ => {}
                    }
                }
            }
            id::TRACKS => {
                if track.is_none() {
                    track = video_track(body);
                }
            }
            id::TIMESTAMP => cluster_ts = uint(body).unwrap_or(0),
            id::SIMPLE_BLOCK | id::BLOCK_GROUP => {
                let Some(track) = &track else {
                    continue;
                };
                let (frame, duration, alpha) = if id == id::SIMPLE_BLOCK {
                    (block_frame(body), None, None)
                } else {
                    let mut group = Reader::new(body);
                    let (mut frame, mut duration, mut alpha) = (None, None, None);
                    while !group.at_end() {
                        let Some((id, size)) = group.header() else {
                            break;
                        };
                        let Some(body) = group.body(size) else {
                            break;
                        };
                        match id {
                            id::BLOCK => frame = block_frame(body),
                            id::BLOCK_DURATION => duration = uint(body),
                            id::BLOCK_ADDITIONS => alpha = alpha_addition(body),
                            _ => {}
                        }
                    }
                    (frame, duration, alpha)
                };
                let Some((number, rel, data)) = frame else {
                    continue;
                };
                if number != track.number {
                    continue;
                }
                let ticks = cluster_ts as f64 + f64::from(rel);
                blocks.push(Block {
                    pts_ms: ticks_to_ms(ticks, scale_ns),
                    duration_ms: duration.map(|d| ticks_to_ms(d as f64, scale_ns)),
                    data: range_in(bytes, data),
                    alpha: alpha.map(|a| range_in(bytes, a)),
                });
            }
            _ => {}
        }
    }
    let track = track?;
    if blocks.is_empty() {
        return None;
    }
    Some(Demuxed {
        track,
        blocks,
        duration_ms: duration_ticks
            .filter(|d| d.is_finite() && *d > 0.0)
            .map(|d| ticks_to_ms(d, scale_ns)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vints_decode_lengths_and_unknown_sizes() {
        let mut r = Reader::new(&[0x81, 0x40, 0x02, 0xFF, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(r.vint(8), Some((0x81, 1, false)));
        assert_eq!(r.vint(8), Some((0x4002, 2, false)));
        assert_eq!(r.vint(8).map(|v| v.2), Some(true));
        assert_eq!(r.vint(8).map(|v| v.2), Some(true));
        // A zero first byte has no length marker.
        assert_eq!(Reader::new(&[0x00, 0x01]).vint(8), None);
    }

    #[test]
    fn only_webm_doc_types_count_as_webm() {
        // EBML header, size 7: DocType (0x4282), size 4, "webm".
        let webm = [0x1A, 0x45, 0xDF, 0xA3, 0x87, 0x42, 0x82, 0x84, b'w', b'e', b'b', b'm'];
        assert!(is_webm(&webm));
        let mkv = [
            0x1A, 0x45, 0xDF, 0xA3, 0x88, 0x42, 0x82, 0x85, b'm', b'a', b't', b'r', b'o',
        ];
        assert!(!is_webm(&mkv));
        assert!(!is_webm(&webm[..8]));
        assert!(!is_webm(b"GIF89a"));
        assert!(!is_webm(&[]));
    }
}
