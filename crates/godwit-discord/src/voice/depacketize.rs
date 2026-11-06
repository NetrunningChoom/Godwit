// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Received video: RTP packets put back in order (asking again for lost
//! ones), then together into whole frames (VP8 by RFC 7741, H264 by RFC
//! 6184, in Annex B form as DAVE expects them), and the RTCP a viewer sends:
//! NACKs, picture loss indications and receiver reports.
//!
//! Ported from Yapfast's `rtp.rs` (GPL-3.0-or-later), its receiving part for
//! the codecs Godwit decodes.

use godwit_media::live::LiveCodec;

use super::rtp::Packet;

/// Video payload types, as Discord's clients number them, and those of
/// their retransmissions (RFC 4588).
pub const H264: u8 = 101;
pub const H264_RTX: u8 = 102;
pub const VP8: u8 = 105;
pub const VP8_RTX: u8 = 106;

/// The video codecs Godwit receives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    Vp8,
    H264,
}

impl Codec {
    pub fn from_payload_type(payload_type: u8) -> Option<Self> {
        match payload_type {
            VP8 => Some(Self::Vp8),
            H264 => Some(Self::H264),
            _ => None,
        }
    }

    /// As Discord names it in session descriptions and Select Protocol.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "VP8" => Some(Self::Vp8),
            "H264" => Some(Self::H264),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Vp8 => "VP8",
            Self::H264 => "H264",
        }
    }

    /// The codec's payload type and its retransmissions'.
    pub fn payload_types(self) -> (u8, u8) {
        match self {
            Self::Vp8 => (VP8, VP8_RTX),
            Self::H264 => (H264, H264_RTX),
        }
    }

    pub fn live(self) -> LiveCodec {
        match self {
            Self::Vp8 => LiveCodec::Vp8,
            Self::H264 => LiveCodec::H264,
        }
    }

    /// What this build decodes, most wanted first, as offered to Discord.
    pub fn decoded() -> Vec<Self> {
        godwit_media::live::decodes()
            .iter()
            .filter_map(|codec| match codec {
                LiveCodec::Vp8 => Some(Self::Vp8),
                LiveCodec::H264 => Some(Self::H264),
                LiveCodec::Vp9 => None,
            })
            .collect()
    }
}

/// A whole received frame, as the sender encoded it (and DAVE sealed it).
#[derive(Debug, PartialEq, Eq)]
pub struct Assembled {
    pub data: Vec<u8>,
    pub keyframe: bool,
}

/// The largest frame put together, far above a 4K keyframe.
const MAX_FRAME: usize = 8 << 20;

/// Puts one SSRC's packets back together into frames. A frame missing a
/// packet is dropped, and frames are dropped until the next keyframe.
pub struct Assembler {
    codec: Codec,
    frame: Vec<u8>,
    timestamp: Option<u32>,
    expected: Option<u16>,
    damaged: bool,
    keyframe: bool,
    waiting: bool,
}

/// What a packet completed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pushed {
    pub frame: Option<Assembled>,
    /// A frame was lost, or none came yet to decode from, so the sender
    /// should send a keyframe.
    pub lost: bool,
}

impl Assembler {
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            frame: Vec::new(),
            timestamp: None,
            expected: None,
            damaged: false,
            keyframe: false,
            waiting: true,
        }
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Drops frames until the next keyframe, as after one the decoder never got.
    pub fn wait_for_keyframe(&mut self) {
        self.waiting = true;
    }

    pub fn push(&mut self, packet: &Packet) -> Pushed {
        let gap = self.expected.is_some_and(|expected| expected != packet.sequence);
        if packet.payload.is_empty() {
            // Padding, sent to probe the path: no part of a frame, but a gap
            // before it still counts against the next packet.
            if !gap {
                self.expected = Some(packet.sequence.wrapping_add(1));
            }
            return Pushed::default();
        }
        self.expected = Some(packet.sequence.wrapping_add(1));
        let mut lost = false;
        if self.timestamp != Some(packet.timestamp) {
            // The last frame never saw its marker.
            lost = !self.frame.is_empty();
            self.frame.clear();
            self.timestamp = Some(packet.timestamp);
            self.damaged = gap;
            self.keyframe = false;
        } else {
            self.damaged |= gap;
        }
        let first = self.frame.is_empty();
        match self.codec {
            Codec::Vp8 => self.vp8(packet.payload, first),
            Codec::H264 => self.h264(packet.payload),
        }
        // Packets that never end their frame would grow it without end.
        if self.frame.len() > MAX_FRAME {
            self.frame = Vec::new();
            self.damaged = true;
        }
        if !packet.marker {
            return Pushed { frame: None, lost };
        }
        let data = std::mem::take(&mut self.frame);
        self.timestamp = None;
        if self.damaged || data.is_empty() {
            self.waiting = true;
            return Pushed {
                frame: None,
                lost: true,
            };
        }
        if self.waiting && !self.keyframe {
            return Pushed {
                frame: None,
                lost: true,
            };
        }
        self.waiting = false;
        Pushed {
            frame: Some(Assembled {
                data,
                keyframe: self.keyframe,
            }),
            lost,
        }
    }

    /// RFC 7741: the payload descriptor, then the frame's bytes.
    fn vp8(&mut self, payload: &[u8], first: bool) {
        let Some(&descriptor) = payload.first() else {
            self.damaged = true;
            return;
        };
        let mut at = 1;
        if descriptor & 0x80 != 0 {
            let Some(&extended) = payload.get(1) else {
                self.damaged = true;
                return;
            };
            at = 2;
            if extended & 0x80 != 0 {
                // The picture id, in one byte or two.
                at += if payload.get(2).is_some_and(|byte| byte & 0x80 != 0) {
                    2
                } else {
                    1
                };
            }
            if extended & 0x40 != 0 {
                at += 1;
            }
            if extended & 0x30 != 0 {
                at += 1;
            }
        }
        let start = descriptor & 0x10 != 0 && descriptor & 0x07 == 0;
        if first != start {
            self.damaged = true;
        }
        let Some(data) = payload.get(at..) else {
            self.damaged = true;
            return;
        };
        if first {
            // A keyframe's frame tag has bit 0 clear.
            self.keyframe = data.first().is_some_and(|tag| tag & 1 == 0);
        }
        self.frame.extend_from_slice(data);
    }

    /// RFC 6184: single units, STAP-A and FU-A, written as Annex B with
    /// four-byte start codes. An IDR slice (type 5) makes a keyframe.
    fn h264(&mut self, payload: &[u8]) {
        let Some(&indicator) = payload.first() else {
            self.damaged = true;
            return;
        };
        let unit = |frame: &mut Vec<u8>, keyframe: &mut bool, nal: &[u8]| {
            *keyframe |= nal.first().is_some_and(|header| header & 0x1f == 5);
            frame.extend_from_slice(&[0, 0, 0, 1]);
            frame.extend_from_slice(nal);
        };
        match indicator & 0x1f {
            1..=23 => unit(&mut self.frame, &mut self.keyframe, payload),
            // STAP-A: sizes and units after the indicator.
            24 => {
                let mut rest = &payload[1..];
                while rest.len() >= 2 {
                    let size = usize::from(u16::from_be_bytes([rest[0], rest[1]]));
                    let Some(nal) = rest.get(2..2 + size) else {
                        self.damaged = true;
                        return;
                    };
                    unit(&mut self.frame, &mut self.keyframe, nal);
                    rest = &rest[2 + size..];
                }
            }
            // FU-A: one unit across packets, its header rebuilt from the
            // indicator and the fragment header.
            28 => {
                let Some(&header) = payload.get(1) else {
                    self.damaged = true;
                    return;
                };
                if header & 0x80 != 0 {
                    let nal_header = (indicator & 0xe0) | (header & 0x1f);
                    self.keyframe |= header & 0x1f == 5;
                    self.frame.extend_from_slice(&[0, 0, 0, 1, nal_header]);
                } else if self.frame.is_empty() {
                    // A continuation without its start.
                    self.damaged = true;
                }
                self.frame.extend_from_slice(&payload[2..]);
            }
            _ => {}
        }
    }
}

/// A received video packet, kept until those before it arrive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub payload_type: u8,
    pub marker: bool,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub payload: Vec<u8>,
}

impl Held {
    pub fn new(packet: &Packet) -> Self {
        Self {
            payload_type: packet.payload_type,
            marker: packet.marker,
            sequence: packet.sequence,
            timestamp: packet.timestamp,
            ssrc: packet.ssrc,
            payload: packet.payload.to_vec(),
        }
    }

    pub fn packet(&self) -> Packet<'_> {
        Packet {
            payload_type: self.payload_type,
            marker: self.marker,
            sequence: self.sequence,
            timestamp: self.timestamp,
            ssrc: self.ssrc,
            payload: &self.payload,
        }
    }

    /// The packet a retransmission on `media`'s RTX SSRC carries (RFC 4588):
    /// its sequence number, then its payload. None for padding, which a
    /// server sends there to probe the path.
    pub fn unwrap_rtx(packet: &Packet, media: u32) -> Option<Self> {
        let (sequence, payload) = packet.payload.split_first_chunk::<2>()?;
        if payload.is_empty() {
            return None;
        }
        let payload_type = [Codec::Vp8, Codec::H264]
            .into_iter()
            .map(Codec::payload_types)
            .find(|&(_, rtx)| rtx == packet.payload_type)
            .map_or(packet.payload_type, |(media, _)| media);
        Some(Self {
            payload_type,
            marker: packet.marker,
            sequence: u16::from_be_bytes(*sequence),
            timestamp: packet.timestamp,
            ssrc: media,
            payload: payload.to_vec(),
        })
    }
}

/// How long a missing packet is waited for, how often it's asked for again,
/// and how many times, before the frames after it go on without it.
/// Discord's servers are a few tens of milliseconds away (Yapfast's values).
const GIVE_UP: std::time::Duration = std::time::Duration::from_millis(300);
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_millis(80);
const ASKS: u32 = 3;
/// Packets held at most while one is missing, about a second at 9 Mbit/s.
const HELD: usize = 1000;
/// Missing packets waited for at most, and asked for at once: a sender
/// skipping far ahead again and again would otherwise run up thousands, and
/// a request too large to send.
const MAX_MISSING: usize = HELD;
const MAX_ASKED: usize = 256;

/// Puts one SSRC's packets back in sequence, as they can arrive out of
/// order, and asks for missing ones again (NACK), as Discord's client does,
/// rather than losing every frame until the next keyframe.
#[derive(Default)]
pub struct Reorder {
    /// The extended sequence number of the next packet to hand on.
    next: Option<u64>,
    highest: Option<u64>,
    held: std::collections::BTreeMap<u64, Held>,
    missing: std::collections::BTreeMap<u64, Missing>,
    /// For receiver reports: the first extended sequence number, and the
    /// packets received since.
    first: Option<u64>,
    received: u64,
}

struct Missing {
    since: std::time::Instant,
    asked: Option<std::time::Instant>,
    asks: u32,
}

impl Reorder {
    /// `sequence` extended to 64 bits, nearest the highest seen so far.
    fn extend(&self, sequence: u16) -> u64 {
        match self.highest {
            None => (1 << 32) + u64::from(sequence),
            Some(highest) => {
                let step = sequence.wrapping_sub(highest as u16) as i16;
                highest.wrapping_add_signed(i64::from(step))
            }
        }
    }

    /// Takes a packet in; false for a duplicate or one too late to use.
    pub fn insert(&mut self, packet: Held, now: std::time::Instant) -> bool {
        let sequence = self.extend(packet.sequence);
        let next = *self.next.get_or_insert(sequence);
        self.first.get_or_insert(sequence);
        if sequence < next || self.held.contains_key(&sequence) {
            return false;
        }
        self.received += 1;
        self.missing.remove(&sequence);
        match self.highest {
            // Far ahead: the sender started again, so start with it.
            Some(highest) if sequence > highest + HELD as u64 => {
                self.held.clear();
                self.missing.clear();
                self.next = Some(sequence);
            }
            Some(highest)
                if sequence > highest + 1 && self.missing.len() + (sequence - highest - 1) as usize <= MAX_MISSING =>
            {
                for gap in highest + 1..sequence {
                    self.missing.insert(
                        gap,
                        Missing {
                            since: now,
                            asked: None,
                            asks: 0,
                        },
                    );
                }
            }
            _ => {}
        }
        if self.highest.is_none_or(|highest| sequence > highest) {
            self.highest = Some(sequence);
        }
        self.held.insert(sequence, packet);
        true
    }

    /// The packets ready in sequence: those in order, and past a gap waited
    /// on long enough, the ones after it.
    pub fn ready(&mut self, now: std::time::Instant) -> Vec<Held> {
        let mut ready = Vec::new();
        while let Some(next) = self.next {
            if let Some(packet) = self.held.remove(&next) {
                ready.push(packet);
                self.next = Some(next + 1);
                continue;
            }
            let Some(&after) = self.held.keys().next() else {
                break;
            };
            let waited = self
                .missing
                .get(&next)
                .is_none_or(|missing| now.duration_since(missing.since) >= GIVE_UP);
            if !waited && self.held.len() < HELD {
                break;
            }
            self.missing.retain(|&sequence, _| sequence > after);
            self.next = Some(after);
        }
        ready
    }

    /// The missing packets to ask for now: newly missed ones, and those
    /// still missing a while after the last ask.
    pub fn to_ask(&mut self, now: std::time::Instant) -> Vec<u16> {
        let mut asking = Vec::new();
        for (&sequence, missing) in &mut self.missing {
            if asking.len() >= MAX_ASKED {
                break;
            }
            let due = missing.asked.is_none_or(|asked| now.duration_since(asked) >= ASK_AGAIN);
            if missing.asks < ASKS && due {
                missing.asked = Some(now);
                missing.asks += 1;
                asking.push(sequence as u16);
            }
        }
        asking
    }

    /// The highest extended sequence number received, and the packets
    /// expected so far that never came, for a receiver report.
    pub fn counts(&self) -> Option<(u32, u32)> {
        let (first, highest) = (self.first?, self.highest?);
        let expected = highest + 1 - first;
        Some((highest as u32, expected.saturating_sub(self.received) as u32))
    }
}

/// RTCP's clear part under "rtpsize" encryption: its header and the
/// sender's SSRC.
pub const RTCP_CLEAR: usize = 8;

/// A picture loss indication (RTCP 206, format 1) from `sender` about
/// `media`: asking for a keyframe.
pub fn picture_loss(sender: u32, media: u32) -> Vec<u8> {
    let mut packet = vec![0x81, 206, 0, 2];
    packet.extend_from_slice(&sender.to_be_bytes());
    packet.extend_from_slice(&media.to_be_bytes());
    packet
}

/// A generic NACK (RTCP 205, format 1) from `sender` asking for `media`'s
/// packets `lost` again: each entry names one, and a bitmask the 16 after it.
pub fn nack(sender: u32, media: u32, lost: &[u16]) -> Vec<u8> {
    let mut entries: Vec<(u16, u16)> = Vec::new();
    for &sequence in lost {
        match entries.last_mut() {
            Some((first, mask)) if (1..=16).contains(&sequence.wrapping_sub(*first)) => {
                *mask |= 1 << (sequence.wrapping_sub(*first) - 1);
            }
            _ => entries.push((sequence, 0)),
        }
    }
    let words = u16::try_from(2 + entries.len()).unwrap_or(u16::MAX);
    let mut packet = vec![0x81, 205];
    packet.extend_from_slice(&words.to_be_bytes());
    packet.extend_from_slice(&sender.to_be_bytes());
    packet.extend_from_slice(&media.to_be_bytes());
    for (first, mask) in entries {
        packet.extend_from_slice(&first.to_be_bytes());
        packet.extend_from_slice(&mask.to_be_bytes());
    }
    packet
}

/// An RTCP receiver report from `sender` about `media`: the highest
/// sequence number received, extended, packets lost, the fraction lost
/// since the last report (out of 256), and the last sender report heard
/// (the middle of its NTP time) with the delay since, in 1/65536 s.
pub fn receiver_report(
    sender: u32,
    media: u32,
    (highest, lost, fraction): (u32, u32, u8),
    (last_report, delay): (u32, u32),
) -> Vec<u8> {
    let mut packet = vec![0x81, 201, 0, 7];
    packet.extend_from_slice(&sender.to_be_bytes());
    packet.extend_from_slice(&media.to_be_bytes());
    packet.extend_from_slice(&((u32::from(fraction) << 24) | lost.min(0x7f_ffff)).to_be_bytes());
    for word in [highest, 0, last_report, delay] {
        packet.extend_from_slice(&word.to_be_bytes());
    }
    packet
}

/// The sender reports in a decrypted compound RTCP packet: each sender's
/// SSRC and the middle 32 bits of its NTP time, which a receiver report echoes.
pub fn sender_reports(mut rtcp: &[u8]) -> Vec<(u32, u32)> {
    let mut reports = Vec::new();
    while rtcp.len() >= 4 {
        let length = (usize::from(u16::from_be_bytes([rtcp[2], rtcp[3]])) + 1) * 4;
        if rtcp[1] == 200
            && let Some(header) = rtcp.get(4..16)
        {
            let sender = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
            let middle = u32::from_be_bytes([header[6], header[7], header[8], header[9]]);
            reports.push((sender, middle));
        }
        rtcp = rtcp.get(length..).unwrap_or_default();
    }
    reports
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn packet(sequence: u16, timestamp: u32, marker: bool, payload: &[u8]) -> Held {
        Held {
            payload_type: VP8,
            marker,
            sequence,
            timestamp,
            ssrc: 7,
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn vp8_frames_come_together_from_a_keyframe_on() {
        let mut assembler = Assembler::new(Codec::Vp8);
        // An interframe first (tag bit 0 set): dropped, asking for a keyframe.
        let pushed = assembler.push(&packet(1, 100, true, &[0x10, 0x01, 9]).packet());
        assert_eq!(
            pushed,
            Pushed {
                frame: None,
                lost: true
            }
        );
        // A keyframe in two packets, with an extended descriptor (one-byte
        // picture id) on the first.
        assert_eq!(
            assembler
                .push(&packet(2, 200, false, &[0x90, 0x80, 0x05, 0x00, 1]).packet())
                .frame,
            None
        );
        let pushed = assembler.push(&packet(3, 200, true, &[0x00, 2, 3]).packet());
        assert_eq!(
            pushed.frame,
            Some(Assembled {
                data: vec![0x00, 1, 2, 3],
                keyframe: true,
            })
        );
        // A gap within the next frame drops it.
        assembler.push(&packet(4, 300, false, &[0x10, 0x01]).packet());
        let pushed = assembler.push(&packet(6, 300, true, &[0x00, 4]).packet());
        assert!(pushed.lost && pushed.frame.is_none());
    }

    #[test]
    fn h264_units_come_out_in_annex_b() {
        let mut assembler = Assembler::new(Codec::H264);
        let mut stap = vec![24];
        for nal in [&[0x67u8, 1, 2][..], &[0x68, 3]] {
            stap.extend_from_slice(&(nal.len() as u16).to_be_bytes());
            stap.extend_from_slice(nal);
        }
        assembler.push(&packet(10, 1, false, &stap).packet());
        // An IDR slice split in an FU-A: indicator 0x7c (NRI 3, type 28),
        // headers with start and end bits.
        assembler.push(&packet(11, 1, false, &[0x7c, 0x85, 0xaa]).packet());
        let pushed = assembler.push(&packet(12, 1, true, &[0x7c, 0x45, 0xbb]).packet());
        let frame = pushed.frame.expect("a keyframe");
        assert!(frame.keyframe);
        assert_eq!(
            frame.data,
            [
                0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 0xaa, 0xbb
            ]
        );
        // A non-IDR frame follows as it is.
        let pushed = assembler.push(&packet(13, 2, true, &[0x41, 9]).packet());
        assert_eq!(
            pushed.frame.map(|f| (f.keyframe, f.data)),
            Some((false, vec![0, 0, 0, 1, 0x41, 9]))
        );
        // A continuation without its start damages the frame.
        let pushed = assembler.push(&packet(14, 3, true, &[0x7c, 0x45, 0xcc]).packet());
        assert!(pushed.lost && pushed.frame.is_none());
    }

    #[test]
    fn reorders_asks_for_lost_packets_and_gives_up_on_them() {
        let start = Instant::now();
        let mut reorder = Reorder::default();
        assert!(reorder.insert(packet(65_534, 0, false, &[1]), start));
        // 65 535 and 0 are missing, across the wrap.
        assert!(reorder.insert(packet(1, 0, false, &[1]), start));
        assert_eq!(
            reorder.ready(start).iter().map(|h| h.sequence).collect::<Vec<_>>(),
            [65_534]
        );
        assert_eq!(reorder.to_ask(start), [65_535, 0]);
        assert!(reorder.to_ask(start).is_empty(), "asked just now");
        // One comes back, resent; the other never does.
        assert!(reorder.insert(packet(65_535, 0, false, &[1]), start));
        assert!(!reorder.insert(packet(65_535, 0, false, &[1]), start), "a duplicate");
        assert_eq!(
            reorder.ready(start).iter().map(|h| h.sequence).collect::<Vec<_>>(),
            [65_535]
        );
        assert_eq!(reorder.to_ask(start + Duration::from_millis(100)), [0], "asked again");
        let later = start + GIVE_UP;
        assert_eq!(reorder.ready(later).iter().map(|h| h.sequence).collect::<Vec<_>>(), [1]);
        assert!(!reorder.insert(packet(0, 0, false, &[1]), later), "too late now");
        assert_eq!(reorder.counts(), Some((65_537, 1)), "extended past the wrap, one lost");
    }

    #[test]
    fn retransmissions_unwrap_to_their_packet() {
        let rtx = Packet {
            payload_type: H264_RTX,
            marker: true,
            sequence: 900,
            timestamp: 5,
            ssrc: 8,
            payload: &[0x12, 0x34, 0x41, 9],
        };
        let held = Held::unwrap_rtx(&rtx, 7).unwrap();
        assert_eq!((held.payload_type, held.sequence, held.ssrc), (H264, 0x1234, 7));
        assert_eq!(held.payload, [0x41, 9]);
        assert!(
            Held::unwrap_rtx(
                &Packet {
                    payload: &[0, 1],
                    ..rtx
                },
                7
            )
            .is_none(),
            "padding"
        );
    }

    #[test]
    fn rtcp_requests_are_well_formed() {
        assert_eq!(picture_loss(1, 2), [0x81, 206, 0, 2, 0, 0, 0, 1, 0, 0, 0, 2]);
        // 10, then 11 and 13 in its mask; 40 alone.
        let request = nack(1, 2, &[10, 11, 13, 40]);
        assert_eq!(&request[..4], &[0x81, 205, 0, 4]);
        assert_eq!(&request[12..], &[0, 10, 0, 0b101, 0, 40, 0, 0][..]);
        let report = receiver_report(1, 2, (1000, 3, 64), (0xabcd, 7));
        assert_eq!(report.len(), 32);
        assert_eq!(report[12], 64);
        let mut sender = vec![0x80, 200, 0, 6, 0, 0, 0, 9, 0, 0, 0x12, 0x34, 0x56, 0x78, 0, 0];
        sender.resize(28, 0);
        assert_eq!(sender_reports(&sender), [(9, 0x1234_5678)]);
    }

    #[test]
    fn offers_only_what_this_build_decodes() {
        let decoded = Codec::decoded();
        assert!(decoded.contains(&Codec::Vp8));
        for codec in decoded {
            assert_eq!(Codec::from_name(codec.name()), Some(codec));
            assert_eq!(Codec::from_payload_type(codec.payload_types().0), Some(codec));
        }
    }
}
