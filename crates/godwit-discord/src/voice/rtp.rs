// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! RTP for a call's audio: Opus packets out, any packet's header read back,
//! and Discord's "rtpsize" transport encryption around both.
//!
//! Ported from Yapfast's `rtp.rs` (GPL-3.0-or-later), its audio part only.

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{AeadInPlace, KeyInit};
use anyhow::{Result, anyhow, bail};
use chacha20poly1305::XChaCha20Poly1305;

/// Opus's payload type, as Discord's clients number it.
pub const OPUS: u8 = 120;
/// RTP's fixed header.
const HEADER: usize = 12;

/// An RTP packet carrying one Opus frame from `ssrc`, with `timestamp` in
/// 48 kHz samples and no extensions, as a call's audio goes. The marker
/// starts a burst of sound.
pub fn opus_packet(ssrc: u32, sequence: u16, timestamp: u32, marker: bool, opus: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(HEADER + opus.len() + 20);
    packet.extend_from_slice(&[0x80, u8::from(marker) << 7 | OPUS]);
    packet.extend_from_slice(&sequence.to_be_bytes());
    packet.extend_from_slice(&timestamp.to_be_bytes());
    packet.extend_from_slice(&ssrc.to_be_bytes());
    packet.extend_from_slice(opus);
    packet
}

/// A decrypted RTP packet.
#[derive(Debug, PartialEq, Eq)]
pub struct Packet<'a> {
    pub payload_type: u8,
    pub marker: bool,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub payload: &'a [u8],
}

/// Reads a decrypted RTP packet, skipping CSRCs, extensions and padding.
/// With "rtpsize" encryption the extension's body was encrypted along with
/// the payload, so it's skipped here, after decryption.
pub fn parse(packet: &[u8]) -> Option<Packet<'_>> {
    let first = *packet.first()?;
    if first >> 6 != 2 || packet.len() < HEADER {
        return None;
    }
    let mut start = HEADER + usize::from(first & 0x0f) * 4;
    if first & 0x10 != 0 {
        let words = u16::from_be_bytes([*packet.get(start + 2)?, *packet.get(start + 3)?]);
        start += 4 + usize::from(words) * 4;
    }
    let padding = if first & 0x20 != 0 {
        usize::from(*packet.last()?)
    } else {
        0
    };
    let end = packet.len().checked_sub(padding)?;
    Some(Packet {
        payload_type: packet[1] & 0x7f,
        marker: packet[1] & 0x80 != 0,
        sequence: u16::from_be_bytes([packet[2], packet[3]]),
        timestamp: u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]),
        ssrc: u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]),
        payload: packet.get(start..end)?,
    })
}

/// The bytes authenticated but not encrypted at the start of an RTP packet:
/// the fixed header, CSRCs, and the extension preamble but not its body.
pub fn rtp_clear_len(packet: &[u8]) -> Option<usize> {
    let first = *packet.first()?;
    let csrcs = usize::from(first & 0x0f) * 4;
    let extension = if first & 0x10 != 0 { 4 } else { 0 };
    let clear = HEADER + csrcs + extension;
    (packet.len() >= clear).then_some(clear)
}

/// Whether a datagram is RTCP rather than RTP, by its packet type.
pub fn is_rtcp(packet: &[u8]) -> bool {
    packet.get(1).is_some_and(|kind| (192..=223).contains(kind))
}

enum Cipher {
    Aes(Box<Aes256Gcm>),
    XChaCha(Box<XChaCha20Poly1305>),
}

/// Discord's transport encryption: the clear part is authenticated data,
/// the rest is encrypted, then the tag and a 32-bit counter, which is the
/// start of the nonce, follow.
pub struct Transport {
    cipher: Cipher,
    counter: u32,
}

const TAG: usize = 16;

impl Transport {
    /// The modes asked for, best first: Discord's clients prefer AES-GCM, and
    /// XChaCha20 is the one every voice server offers.
    pub const MODES: [&str; 2] = ["aead_aes256_gcm_rtpsize", "aead_xchacha20_poly1305_rtpsize"];

    pub fn new(mode: &str, key: &[u8]) -> Result<Self> {
        let cipher = match mode {
            "aead_aes256_gcm_rtpsize" => Cipher::Aes(Box::new(
                Aes256Gcm::new_from_slice(key).map_err(|_| anyhow!("the voice key isn't 32 bytes"))?,
            )),
            "aead_xchacha20_poly1305_rtpsize" => Cipher::XChaCha(Box::new(
                XChaCha20Poly1305::new_from_slice(key).map_err(|_| anyhow!("the voice key isn't 32 bytes"))?,
            )),
            _ => bail!("Discord chose an encryption mode Godwit doesn't know: {mode}"),
        };
        Ok(Self {
            cipher,
            counter: rand::random(),
        })
    }

    /// Encrypts `packet` after its first `clear` bytes, in place.
    pub fn seal(&mut self, packet: &mut Vec<u8>, clear: usize) -> Result<()> {
        let counter = self.counter.to_be_bytes();
        self.counter = self.counter.wrapping_add(1);
        let clear = clear.min(packet.len());
        let (header, body) = packet.split_at_mut(clear);
        let tag = match &self.cipher {
            Cipher::Aes(cipher) => {
                let mut nonce = aes_gcm::Nonce::default();
                nonce[..4].copy_from_slice(&counter);
                cipher
                    .encrypt_in_place_detached(&nonce, header, body)
                    .map(|tag| tag.to_vec())
            }
            Cipher::XChaCha(cipher) => {
                let mut nonce = chacha20poly1305::XNonce::default();
                nonce[..4].copy_from_slice(&counter);
                cipher
                    .encrypt_in_place_detached(&nonce, header, body)
                    .map(|tag| tag.to_vec())
            }
        }
        .map_err(|_| anyhow!("could not encrypt a voice packet"))?;
        packet.extend_from_slice(&tag);
        packet.extend_from_slice(&counter);
        Ok(())
    }

    /// Decrypts a sealed packet, returning its clear part and plaintext;
    /// None when it doesn't authenticate.
    pub fn open(&self, packet: &[u8], clear: usize) -> Option<Vec<u8>> {
        if packet.len() < clear + TAG + 4 {
            return None;
        }
        let (rest, counter) = packet.split_at(packet.len() - 4);
        let (sealed, tag) = rest.split_at(rest.len() - TAG);
        let (header, body) = sealed.split_at(clear);
        let mut plain = body.to_vec();
        let opened = match &self.cipher {
            Cipher::Aes(cipher) => {
                let mut nonce = aes_gcm::Nonce::default();
                nonce[..4].copy_from_slice(counter);
                cipher.decrypt_in_place_detached(&nonce, header, &mut plain, tag.into())
            }
            Cipher::XChaCha(cipher) => {
                let mut nonce = chacha20poly1305::XNonce::default();
                nonce[..4].copy_from_slice(counter);
                cipher.decrypt_in_place_detached(&nonce, header, &mut plain, tag.into())
            }
        };
        opened.ok()?;
        let mut whole = header.to_vec();
        whole.extend_from_slice(&plain);
        Some(whole)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_reads_an_opus_packet() {
        let packet = opus_packet(0xdead_beef, 65_535, 960, true, b"opus");
        assert_eq!(&packet[..2], &[0x80, 0x80 | OPUS]);
        let read = parse(&packet).unwrap();
        assert_eq!(
            read,
            Packet {
                payload_type: OPUS,
                marker: true,
                sequence: 65_535,
                timestamp: 960,
                ssrc: 0xdead_beef,
                payload: b"opus",
            }
        );
        assert_eq!(rtp_clear_len(&packet), Some(12));
        assert!(!is_rtcp(&packet));
        assert!(is_rtcp(&[0x81, 201, 0, 1]));
    }

    #[test]
    fn skips_csrcs_extensions_and_padding() {
        // One CSRC, a one-word extension, two bytes of padding.
        let mut packet = vec![0x80 | 0x20 | 0x10 | 1, OPUS, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3];
        packet.extend_from_slice(&[9, 9, 9, 9]); // CSRC
        packet.extend_from_slice(&[0xbe, 0xde, 0, 1, 7, 7, 7, 7]); // extension
        packet.extend_from_slice(b"voice");
        packet.extend_from_slice(&[0, 2]);
        assert_eq!(parse(&packet).unwrap().payload, b"voice");
        assert_eq!(rtp_clear_len(&packet), Some(12 + 4 + 4));
        assert!(parse(&[0x80, 0]).is_none(), "too short");
        assert!(parse(&[0u8; 12]).is_none(), "not version 2");
    }

    #[test]
    fn seals_and_opens_both_modes() {
        for mode in Transport::MODES {
            let mut transport = Transport::new(mode, &[7; 32]).unwrap();
            let mut packet = opus_packet(1, 2, 3, false, b"a frame of sound");
            let plain = packet.clone();
            let clear = rtp_clear_len(&packet).unwrap();
            transport.seal(&mut packet, clear).unwrap();
            assert_eq!(packet.len(), plain.len() + 20);
            assert_eq!(packet[..clear], plain[..clear], "the header stays readable");
            assert_ne!(packet[clear..plain.len()], plain[clear..]);
            assert_eq!(transport.open(&packet, clear), Some(plain));
            packet[clear] ^= 1;
            assert_eq!(transport.open(&packet, clear), None, "tampering fails");
        }
        assert!(Transport::new("xsalsa20_poly1305", &[0; 32]).is_err());
        assert!(Transport::new(Transport::MODES[0], &[0; 16]).is_err());
    }
}
