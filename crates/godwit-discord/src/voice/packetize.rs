// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Sent video: encoded frames cut into RTP packets with the header
//! extensions a Discord client sends, resent from RTX when a viewer reports
//! a loss (RFC 4588), the RTCP a sender reads (picture loss, NACKs, the
//! server's bitrate estimate) and the sender reports it writes, and op 12's
//! announcement of the video.
//!
//! Ported from Yapfast's `rtp.rs` and `stream_connection.rs`
//! (GPL-3.0-or-later), their sending part for VP8 and H264, the codecs
//! Godwit encodes.

use serde_json::{Value, json};

use super::connection::STREAM_RID;
use super::depacketize::Codec;
use super::rtp;

/// Payload bytes in a packet before the next starts: a Discord client's
/// packets stay well under a path's MTU after encryption and extensions.
const CHUNK: usize = 1100;
/// RTP's fixed header.
const HEADER: usize = 12;
/// One-byte extension ids in the numbering a Discord client sends with,
/// which isn't the numbering in Discord's WebRTC SDP: here 5 is the
/// transport-wide sequence number, and with playout delay as 5 Discord's
/// server dropped every packet (Yapfast's finding). Playout delay itself is
/// left out: Discord's iOS app showed such a stream black.
const TRANSPORT_SEQUENCE: u8 = 5;
const CONTENT_TYPE: u8 = 7;
const RID: u8 = 11;
/// The stream id on a retransmission, in place of [`RID`].
const REPAIRED_RID: u8 = 12;
/// The extension block: its preamble, then three words of elements.
const EXTENSIONS: usize = 4 + 12;

/// What the video is, for the content type extension: a screen marks
/// itself as one (1), a camera is plain video (0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Content {
    Camera,
    Screen,
}

/// Cuts encoded frames into RTP packets for one SSRC.
pub struct Packetizer {
    ssrc: u32,
    codec: Codec,
    content: Content,
    sequence: u16,
    /// Retransmissions' own numbering, on the RTX SSRC.
    rtx_sequence: u16,
    /// Counts every packet sent, for Discord's congestion control.
    transport: u16,
    /// VP8's 15-bit picture id.
    picture: u16,
}

impl Packetizer {
    /// Packetizes frames in `codec` from the next one, keeping the numbering
    /// (Discord may switch a stream's codec as viewers come and go).
    pub fn set_codec(&mut self, codec: Codec) {
        self.codec = codec;
    }

    pub fn new(ssrc: u32, codec: Codec, content: Content) -> Self {
        Self {
            ssrc,
            codec,
            content,
            sequence: rand::random(),
            rtx_sequence: rand::random(),
            transport: 1,
            picture: rand::random::<u16>() & 0x7fff,
        }
    }

    /// The packets carrying `frame` (as DAVE sealed it), unencrypted, with
    /// `timestamp` in the 90 kHz clock; the last has the marker bit.
    pub fn packets(&mut self, frame: &[u8], timestamp: u32) -> Vec<Vec<u8>> {
        let payloads = match self.codec {
            Codec::Vp8 => self.vp8_payloads(frame),
            Codec::H264 => h264_payloads(frame),
        };
        let count = payloads.len();
        let payload_type = self.codec.payload_types().0;
        payloads
            .into_iter()
            .enumerate()
            .map(|(index, payload)| {
                let sequence = self.sequence;
                self.sequence = self.sequence.wrapping_add(1);
                self.header(
                    payload_type,
                    index + 1 == count,
                    sequence,
                    timestamp,
                    self.ssrc,
                    RID,
                    &payload,
                )
            })
            .collect()
    }

    /// A packet sent before, again: on `rtx_ssrc` with the RTX payload type
    /// and numbering, the original sequence number before its payload, and
    /// the repaired stream id, as Discord's client resends. None for a
    /// packet that isn't this packetizer's.
    pub fn retransmit(&mut self, original: &[u8], rtx_ssrc: u32) -> Option<Vec<u8>> {
        let sent = rtp::parse(original)?;
        let codec = Codec::from_payload_type(sent.payload_type)?;
        if sent.ssrc != self.ssrc {
            return None;
        }
        let payload = [&sent.sequence.to_be_bytes()[..], sent.payload].concat();
        let sequence = self.rtx_sequence;
        self.rtx_sequence = self.rtx_sequence.wrapping_add(1);
        Some(self.header(
            codec.payload_types().1,
            sent.marker,
            sequence,
            sent.timestamp,
            rtx_ssrc,
            REPAIRED_RID,
            &payload,
        ))
    }

    /// An RTP packet with a Discord client's extensions, naming the stream
    /// with `rid`, and the next transport sequence number.
    #[allow(clippy::too_many_arguments)]
    fn header(
        &mut self,
        payload_type: u8,
        marker: bool,
        sequence: u16,
        timestamp: u32,
        ssrc: u32,
        rid: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut packet = Vec::with_capacity(HEADER + EXTENSIONS + payload.len() + 20);
        // Version 2 with the extension bit.
        packet.extend_from_slice(&[0x90, u8::from(marker) << 7 | payload_type]);
        packet.extend_from_slice(&sequence.to_be_bytes());
        packet.extend_from_slice(&timestamp.to_be_bytes());
        packet.extend_from_slice(&ssrc.to_be_bytes());
        packet.extend_from_slice(&[0xbe, 0xde, 0, ((EXTENSIONS - 4) / 4) as u8]);
        let mut extension = |id: u8, value: &[u8]| {
            packet.push(id << 4 | (value.len().saturating_sub(1) as u8 & 0x0f));
            packet.extend_from_slice(value);
        };
        extension(TRANSPORT_SEQUENCE, &self.transport.to_be_bytes());
        extension(CONTENT_TYPE, &[u8::from(self.content == Content::Screen)]);
        extension(rid, STREAM_RID.as_bytes());
        // The rest of the last word is padding (id 0).
        packet.resize(HEADER + EXTENSIONS, 0);
        packet.extend_from_slice(payload);
        self.transport = self.transport.wrapping_add(1);
        packet
    }

    /// VP8 payloads (RFC 7741): each chunk after the extended descriptor
    /// with a 15-bit picture id; S marks the start of the frame.
    fn vp8_payloads(&mut self, frame: &[u8]) -> Vec<Vec<u8>> {
        let payloads = frame
            .chunks(CHUNK)
            .enumerate()
            .map(|(index, chunk)| {
                let start = if index == 0 { 0x10 } else { 0 };
                let mut payload = vec![0x80 | start, 0x80];
                payload.extend_from_slice(&(0x8000 | self.picture).to_be_bytes());
                payload.extend_from_slice(chunk);
                payload
            })
            .collect();
        self.picture = (self.picture + 1) & 0x7fff;
        payloads
    }
}

/// The video packets sent lately, unencrypted, to resend when a viewer
/// reports one lost. Slots fill as packets go, so a quiet video keeps little.
pub struct History {
    packets: Vec<Option<(u16, Vec<u8>)>>,
}

impl History {
    /// Room for `capacity` packets (at least one).
    pub fn new(capacity: usize) -> Self {
        Self {
            packets: vec![None; capacity.max(1)],
        }
    }

    pub fn capacity(&self) -> usize {
        self.packets.len()
    }

    pub fn remember(&mut self, packet: &[u8]) {
        if let Some(sent) = rtp::parse(packet) {
            let slot = usize::from(sent.sequence) % self.packets.len();
            if let Some(kept) = self.packets.get_mut(slot) {
                // The slot's buffer is reused: no allocation once warm.
                match kept {
                    Some((sequence, bytes)) => {
                        *sequence = sent.sequence;
                        bytes.clear();
                        bytes.extend_from_slice(packet);
                    }
                    None => *kept = Some((sent.sequence, packet.to_vec())),
                }
            }
        }
    }

    pub fn get(&self, sequence: u16) -> Option<&[u8]> {
        match self.packets.get(usize::from(sequence) % self.packets.len())? {
            Some((kept, packet)) if *kept == sequence => Some(packet),
            _ => None,
        }
    }
}

/// The packets of a decrypted compound RTCP packet.
fn rtcp_packets(mut rtcp: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        if rtcp.len() < 4 {
            return None;
        }
        let length = (usize::from(u16::from_be_bytes([rtcp[2], rtcp[3]])) + 1) * 4;
        let (packet, rest) = rtcp.split_at(length.min(rtcp.len()));
        rtcp = rest;
        Some(packet)
    })
}

/// Whether a decrypted compound RTCP packet asks for a keyframe, with a
/// picture loss indication (206/1) or a full intra request (206/4).
pub fn wants_keyframe(rtcp: &[u8]) -> bool {
    rtcp_packets(rtcp).any(|packet| packet[1] == 206 && matches!(packet[0] & 0x1f, 1 | 4))
}

/// The sequence numbers of `media`'s packets that generic NACKs (205/1)
/// report lost: each entry names one, and a bitmask the 16 after it.
pub fn nacked(rtcp: &[u8], media: u32) -> Vec<u16> {
    let mut lost = Vec::new();
    for packet in rtcp_packets(rtcp) {
        if packet[1] != 205 || packet[0] & 0x1f != 1 || packet.get(8..12) != Some(&media.to_be_bytes()[..]) {
            continue;
        }
        for entry in packet.get(12..).unwrap_or_default().as_chunks::<4>().0 {
            let first = u16::from_be_bytes([entry[0], entry[1]]);
            let mask = u16::from_be_bytes([entry[2], entry[3]]);
            lost.push(first);
            lost.extend(
                (0..16)
                    .filter(|bit| mask & (1 << bit) != 0)
                    .map(|bit| first.wrapping_add(bit + 1)),
            );
        }
    }
    lost
}

/// The lowest bitrate the receiver estimates (REMB, 206/15) for `ssrc`
/// in a decrypted compound RTCP packet, in bit/s.
pub fn bandwidth_estimate(rtcp: &[u8], ssrc: u32) -> Option<u64> {
    rtcp_packets(rtcp)
        .filter_map(|packet| {
            if packet[1] != 206 || packet[0] & 0x1f != 15 || packet.get(12..16)? != b"REMB" {
                return None;
            }
            let count = usize::from(*packet.get(16)?);
            let exponent = packet.get(17)? >> 2;
            let mantissa = u32::from_be_bytes(packet.get(16..20)?.try_into().ok()?) & 0x3_ffff;
            let covers = (0..count).any(|index| {
                let at = 20 + index * 4;
                packet.get(at..at + 4) == Some(&ssrc.to_be_bytes()[..])
            });
            // checked_shl checks the shift count, not bits lost at the top.
            // REMB's 18-bit mantissa and 6-bit exponent can exceed u64.
            covers.then(|| u64::try_from(u128::from(mantissa) << exponent).unwrap_or(u64::MAX))
        })
        .min()
}

/// An RTCP sender report from `ssrc`: that `timestamp` in the RTP clock is
/// `now`, after `packets` packets carrying `octets` bytes of payload, so
/// receivers can place the video in time.
pub fn sender_report(ssrc: u32, now: std::time::SystemTime, timestamp: u32, packets: u32, octets: u32) -> Vec<u8> {
    // NTP counts from 1900, the Unix clock from 1970.
    const NTP_UNIX: u64 = 2_208_988_800;
    let since = now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let seconds = (since.as_secs() + NTP_UNIX) as u32;
    let fraction = ((u64::from(since.subsec_nanos()) << 32) / 1_000_000_000) as u32;
    let mut packet = vec![0x80, 200, 0, 6];
    for word in [ssrc, seconds, fraction, timestamp, packets, octets] {
        packet.extend_from_slice(&word.to_be_bytes());
    }
    packet
}

/// The SSRCs a voice connection sends on: its audio, and the video and
/// its retransmissions that Ready assigned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ssrcs {
    pub audio: u32,
    pub video: u32,
    pub rtx: u32,
}

/// Op 12 announcing our video: one stream at rid "100" with its size,
/// frame rate and the most bit/s it takes, as Discord's clients announce
/// (Yapfast's `video_attributes`).
pub fn video_attributes(ssrcs: Ssrcs, (width, height): (u32, u32), fps: u32, max_bitrate: u32) -> Value {
    json!({
        "audio_ssrc": ssrcs.audio,
        "video_ssrc": ssrcs.video,
        "rtx_ssrc": ssrcs.rtx,
        "streams": [{
            "type": "video",
            "rid": STREAM_RID,
            "ssrc": ssrcs.video,
            "active": true,
            "quality": 100,
            "rtx_ssrc": ssrcs.rtx,
            "max_bitrate": max_bitrate,
            "max_framerate": fps,
            "max_resolution": {"type": "fixed", "width": width, "height": height},
        }],
    })
}

/// Op 12 when the video stops: no video SSRC, and no streams.
pub fn no_video(audio: u32) -> Value {
    json!({"audio_ssrc": audio, "video_ssrc": 0, "rtx_ssrc": 0, "streams": []})
}

/// H264 payloads in packetization mode 1 (RFC 6184): a NAL unit that fits
/// goes whole, a bigger one in fragmentation units (FU-A). Yapfast's
/// `h264_payloads`. DAVE leaves start codes and NAL headers in the clear, so
/// a sealed frame splits the same way.
fn h264_payloads(frame: &[u8]) -> Vec<Vec<u8>> {
    let mut payloads = Vec::new();
    for unit in nal_units(frame) {
        let Some((&header, body)) = unit.split_first() else {
            continue;
        };
        if unit.len() <= CHUNK {
            payloads.push(unit.to_vec());
            continue;
        }
        let chunks: Vec<&[u8]> = body.chunks(CHUNK - 2).collect();
        let last = chunks.len().saturating_sub(1);
        for (index, chunk) in chunks.into_iter().enumerate() {
            // The indicator keeps the unit's F and NRI bits; the FU header
            // its type, with start and end flags.
            let mut fragment = header & 0x1f;
            if index == 0 {
                fragment |= 0x80;
            }
            if index == last {
                fragment |= 0x40;
            }
            let mut payload = vec![(header & 0xe0) | 28, fragment];
            payload.extend_from_slice(chunk);
            payloads.push(payload);
        }
    }
    payloads
}

/// An Annex B access unit's NAL units, without their start codes.
pub fn nal_units(frame: &[u8]) -> Vec<&[u8]> {
    let mut starts: Vec<(usize, usize)> = Vec::new();
    let mut at = 0;
    while at + 3 <= frame.len() {
        if frame[at..at + 3] == [0, 0, 1] {
            let code = if at > 0 && frame[at - 1] == 0 { at - 1 } else { at };
            starts.push((code, at + 3));
            at += 3;
        } else {
            at += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(index, &(_, start))| {
            let end = starts.get(index + 1).map_or(frame.len(), |&(code, _)| code);
            &frame[start..end.max(start)]
        })
        .filter(|unit| !unit.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vp8_frames_go_out_with_descriptors_extensions_and_a_marker() {
        let mut packetizer = Packetizer::new(0x0a0b_0c0d, Codec::Vp8, Content::Screen);
        let frame: Vec<u8> = (0..2500u32).map(|i| i as u8).collect();
        let packets = packetizer.packets(&frame, 90_000);
        assert_eq!(packets.len(), 3, "1100-byte chunks");
        let mut sequences = vec![];
        let mut body = vec![];
        for (index, packet) in packets.iter().enumerate() {
            let read = rtp::parse(packet).unwrap();
            assert_eq!(read.payload_type, super::super::depacketize::VP8);
            assert_eq!(read.ssrc, 0x0a0b_0c0d);
            assert_eq!(read.timestamp, 90_000);
            assert_eq!(read.marker, index == 2, "the last packet ends the frame");
            sequences.push(read.sequence);
            // The extension block: transport sequence (5), content type (7)
            // marking a screen, then the rid "100" (11).
            assert_eq!(&packet[12..16], &[0xbe, 0xde, 0, 3]);
            assert_eq!(packet[16], 5 << 4 | 1);
            assert_eq!(&packet[19..21], &[7 << 4, 1]);
            assert_eq!(&packet[21..25], &[11 << 4 | 2, b'1', b'0', b'0']);
            // The VP8 descriptor: X, S on the first only, a 15-bit picture id.
            assert_eq!(read.payload[0], if index == 0 { 0x90 } else { 0x80 });
            assert_eq!(read.payload[1], 0x80);
            assert!(read.payload[2] & 0x80 != 0);
            body.extend_from_slice(&read.payload[4..]);
            assert_eq!(rtp::rtp_clear_len(packet), Some(16), "the extension body is encrypted");
        }
        assert_eq!(body, frame);
        assert_eq!(sequences[1], sequences[0].wrapping_add(1));
        // The next frame has the next picture id.
        let next = packetizer.packets(b"x", 93_000);
        let picture = |p: &[u8]| {
            u16::from_be_bytes([rtp::parse(p).unwrap().payload[2], rtp::parse(p).unwrap().payload[3]]) & 0x7fff
        };
        assert_eq!(picture(&next[0]), (picture(&packets[0]) + 1) & 0x7fff);
        let camera = Packetizer::new(1, Codec::Vp8, Content::Camera).packets(b"x", 0);
        assert_eq!(&camera[0][19..21], &[7 << 4, 0], "a camera isn't screen content");
    }

    #[test]
    fn lost_packets_are_resent_from_rtx_and_the_depacketizer_takes_them_back() {
        let mut packetizer = Packetizer::new(50, Codec::Vp8, Content::Camera);
        let mut history = History::new(2048);
        let packets = packetizer.packets(&[7u8; 1500], 3000);
        for packet in &packets {
            history.remember(packet);
        }
        let lost = rtp::parse(&packets[1]).unwrap().sequence;
        let again = packetizer.retransmit(history.get(lost).unwrap(), 51).unwrap();
        let read = rtp::parse(&again).unwrap();
        assert_eq!((read.ssrc, read.payload_type), (51, super::super::depacketize::VP8_RTX));
        assert_eq!(
            &again[21..25],
            &[12 << 4 | 2, b'1', b'0', b'0'],
            "the repaired stream id"
        );
        let unwrapped = super::super::depacketize::Held::unwrap_rtx(&read, 50).unwrap();
        assert_eq!(unwrapped.packet().sequence, lost);
        assert_eq!(unwrapped.packet().payload, rtp::parse(&packets[1]).unwrap().payload);
        assert!(history.get(lost.wrapping_add(100)).is_none());
        assert!(
            Packetizer::new(9, Codec::Vp8, Content::Camera)
                .retransmit(&packets[0], 10)
                .is_none(),
            "another SSRC's"
        );
    }

    #[test]
    fn reads_what_viewers_and_the_server_ask_of_a_sender() {
        use super::super::depacketize::{nack, picture_loss, receiver_report};
        let report = receiver_report(1, 50, (10, 0, 0), (0, 0));
        let mut rtcp = report.clone();
        rtcp.extend_from_slice(&nack(1, 50, &[100, 101, 117, 300]));
        assert!(!wants_keyframe(&rtcp));
        assert_eq!(nacked(&rtcp, 50), [100, 101, 117, 300]);
        assert!(nacked(&rtcp, 51).is_empty(), "about another SSRC");
        rtcp.extend_from_slice(&picture_loss(1, 50));
        assert!(wants_keyframe(&rtcp));
        // REMB: 1.5 Mbit/s as an 18-bit mantissa (187500 << 3), for SSRC 50.
        let mut remb = vec![0x8f, 206, 0, 5, 0, 0, 0, 1, 0, 0, 0, 0];
        remb.extend_from_slice(b"REMB");
        let (exponent, mantissa) = (3u32, 187_500u32);
        remb.extend_from_slice(&((1 << 24) | (exponent << 18) | mantissa).to_be_bytes());
        remb.extend_from_slice(&50u32.to_be_bytes());
        assert_eq!(bandwidth_estimate(&remb, 50), Some(1_500_000));
        assert_eq!(bandwidth_estimate(&remb, 51), None);
        assert_eq!(bandwidth_estimate(&report, 50), None);
        let mut extreme = remb.clone();
        extreme[17..20].copy_from_slice(&[0xff, 0xff, 0xff]);
        assert_eq!(bandwidth_estimate(&extreme, 50), Some(u64::MAX));
        extreme[17..20].copy_from_slice(&[0xfc, 0, 0]);
        assert_eq!(bandwidth_estimate(&extreme, 50), Some(0));
        let mut compound = extreme;
        compound.extend_from_slice(&remb);
        assert_eq!(bandwidth_estimate(&compound, 50), Some(0), "the lowest estimate wins");
        let sr = sender_report(50, std::time::UNIX_EPOCH, 9, 3, 4);
        assert_eq!(&sr[..4], &[0x80, 200, 0, 6]);
        assert_eq!(
            super::super::depacketize::sender_reports(&sr),
            [(50, 2_208_988_800u32 << 16)]
        );
    }

    #[test]
    fn op_12_announces_and_withdraws_the_video() {
        let ssrcs = Ssrcs {
            audio: 10,
            video: 11,
            rtx: 12,
        };
        let on = video_attributes(ssrcs, (1280, 720), 30, 2_500_000);
        assert_eq!(
            (on["audio_ssrc"].as_u64(), on["video_ssrc"].as_u64()),
            (Some(10), Some(11))
        );
        let stream = &on["streams"][0];
        assert_eq!(stream["rid"], "100");
        assert_eq!(
            (stream["ssrc"].as_u64(), stream["rtx_ssrc"].as_u64()),
            (Some(11), Some(12))
        );
        assert_eq!(stream["max_resolution"]["width"], 1280);
        assert_eq!(stream["max_framerate"], 30);
        let off = no_video(10);
        assert_eq!(off["video_ssrc"], 0);
        assert!(off["streams"].as_array().unwrap().is_empty());
    }

    #[test]
    fn h264_units_go_whole_or_in_fragments() {
        // SPS, PPS and a slice too big for one packet.
        let mut frame = vec![0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3];
        frame.extend_from_slice(&[0, 0, 1, 0x65]);
        frame.extend(std::iter::repeat_n(0xaa, 2500));
        let units = nal_units(&frame);
        assert_eq!(units.len(), 3);
        assert_eq!(units[0], [0x67, 1, 2]);
        let mut packetizer = Packetizer::new(7, Codec::H264, Content::Screen);
        let packets = packetizer.packets(&frame, 90);
        let payloads: Vec<&[u8]> = packets.iter().map(|p| &p[HEADER + EXTENSIONS..]).collect();
        assert_eq!(payloads[0], [0x67, 1, 2]);
        assert_eq!(payloads[1], [0x68, 3]);
        // FU-A: type 28 with the slice's NRI, start then end flags.
        assert_eq!(payloads[2][0], 0x60 | 28);
        assert_eq!(payloads[2][1], 0x80 | 5);
        let last = payloads.last().unwrap();
        assert_eq!(last[1], 0x40 | 5);
        assert!(payloads.iter().all(|p| p.len() <= CHUNK));
        assert_eq!(packets.last().unwrap()[1] & 0x80, 0x80, "the marker on the last");
        // Every byte of the slice once.
        let body: usize = payloads[2..].iter().map(|p| p.len() - 2).sum();
        assert_eq!(body, 2500);
    }
}
