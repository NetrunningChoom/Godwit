// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Our video on its way out: a capture (`godwit_voice::camera` or
//! `godwit_voice::screen`) hands each picture to an encoder thread of its
//! own, which encodes H264 with x264 (`godwit_media::encode`), and the
//! connection's thread sends the frames: DAVE-encrypted, packetized
//! (`packetize`), announced with op 12, resent on NACK, a keyframe on a
//! picture loss indication, and the bitrate following the server's
//! estimate as Discord's client does (Yapfast's `stream_worker.rs` and
//! `call_video.rs`, GPL-3.0-or-later).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc as channel;
use std::time::Duration;

use anyhow::Result;
use godwit_media::encode::{CLOCK, H264Encoder, Planes};
use godwit_voice::video::{Picture, Sink};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::connection::{Connection, op, send_json};
use super::depacketize::{Codec, RTCP_CLEAR};
use super::packetize::{self, Content, History, Packetizer, Ssrcs};
use super::{note, rtp};

/// The codec cameras and Go Live go out in: H264 through x264, as Yapfast
/// sends both and every Go Live sender known to work does (libwebrtc's
/// OpenH264 drops frames under motion, by Yapfast's tests).
pub const SEND_CODEC: Codec = Codec::H264;
/// Discord's free tier, which Godwit never goes past (no Nitro features,
/// ever): 720p (either way round) at 30 frames a second, for cameras and
/// Go Live alike. A frame past it is never sent.
pub const FREE_SIZE: (u32, u32) = (1280, 720);
pub const FREE_FPS: u32 = 30;
/// How long after the first frame the frame rate sent is logged.
const MEASURE: Duration = Duration::from_secs(10);

/// Whether `size` keeps to [`FREE_SIZE`], in either orientation.
pub fn within_free(size: (u32, u32)) -> bool {
    let (long, short) = (size.0.max(size.1), size.0.min(size.1));
    long <= FREE_SIZE.0 && short <= FREE_SIZE.1
}

/// How often a sender report goes out.
const SENDER_REPORTS: Duration = Duration::from_secs(1);
/// The most packets resent for one NACK, which can name hundreds.
const RESENDS: usize = 128;
/// The bitrate the server's estimate never pushes the encoder below, and
/// how far apart the aim and the estimate must be for the aim to change:
/// Discord's client's rules, as Yapfast follows them.
const FLOOR: u64 = 500_000;
const HYSTERESIS: u64 = 10;
/// Encoded frames waiting for the connection: beyond that the network
/// can't keep up, a frame is dropped and the next is a keyframe.
const QUEUED: usize = 4;

/// What a video is and how it's sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    pub content: Content,
    pub fps: u32,
}

impl Profile {
    /// The codec this video goes out in.
    pub fn codec(self) -> Codec {
        SEND_CODEC
    }

    /// The most bit/s the video may take at `height` lines, as op 12
    /// announces: for a stream, Discord's client's 3.5 Mbit/s up to 720p
    /// and 9 Mbit/s above (Yapfast's `max_bitrate`); for a camera, the
    /// 2.5 Mbit/s of the client's 720p step.
    pub fn max_bitrate(self, height: u32) -> u64 {
        match self.content {
            Content::Screen if height > 720 => 9_000_000,
            Content::Screen => 3_500_000,
            Content::Camera => 2_500_000,
        }
    }

    /// What the encoder aims for at `height` lines, before the server's
    /// estimate: a stream at its cap, a camera at Yapfast's 1.5 Mbit/s.
    fn aim(self, height: u32) -> u64 {
        match self.content {
            Content::Screen => self.max_bitrate(height),
            Content::Camera => 1_500_000,
        }
    }

    /// The bitrate to encode at: the aim, or the server's estimate (0
    /// until it says) where that's lower, never below the floor.
    pub fn target(self, height: u32, estimate: u64) -> u64 {
        let aim = self.aim(height);
        if estimate > 0 && estimate < aim {
            estimate.max(FLOOR)
        } else {
            aim
        }
    }
}

/// Whether the bitrate aimed for should move to `wanted`: only when the
/// two differ by more than a tenth, so the encoder isn't retuned for noise.
fn retune(current: u64, wanted: u64) -> bool {
    current.abs_diff(wanted) * HYSTERESIS > current
}

/// One encoded frame, with its time in the 90 kHz clock.
#[derive(Debug)]
pub struct Encoded {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub timestamp: u32,
    pub size: (u32, u32),
}

/// What the connection asks of the encoder.
#[derive(Default)]
pub struct Control {
    keyframe: AtomicBool,
    /// The server's bitrate estimate in bit/s, 0 until it said.
    estimate: AtomicU64,
}

impl Control {
    pub fn want_keyframe(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
    }
}

/// A captured picture, packed, on its way to the encoder.
struct Raw {
    width: u32,
    height: u32,
    timestamp_us: i64,
    /// Y, then U, then V, without padding.
    data: Vec<u8>,
}

/// Copies a picture's planes into `out`, without their row padding.
fn pack(picture: &Picture<'_>, out: &mut Vec<u8>) -> bool {
    let (w, h) = (picture.width as usize, picture.height as usize);
    out.clear();
    for (plane, stride, (width, rows)) in [
        (picture.y, picture.strides[0], (w, h)),
        (picture.u, picture.strides[1], (w / 2, h / 2)),
        (picture.v, picture.strides[2], (w / 2, h / 2)),
    ] {
        let stride = stride as usize;
        for row in 0..rows {
            let Some(line) = plane.get(row * stride..row * stride + width) else {
                return false;
            };
            out.extend_from_slice(line);
        }
    }
    true
}

/// The encoder thread's side: frames out, and what the connection asks.
pub struct Encoding {
    pub frames: mpsc::Receiver<Encoded>,
    pub control: Arc<Control>,
}

impl Encoding {
    /// Starts an encoder thread for `profile`, and the sink to hand a
    /// capture. The thread ends when the capture drops its sink or the
    /// receiving side goes.
    pub fn start(profile: Profile) -> std::io::Result<(Sink, Self)> {
        // One picture waits while one is encoded; a newer one finding the
        // encoder busy is dropped at the capture, before it costs anything.
        let (pictures, waiting) = channel::sync_channel::<Raw>(1);
        let (spare_back, spare) = channel::sync_channel::<Vec<u8>>(2);
        let (frames, receiver) = mpsc::channel(QUEUED);
        let control = Arc::new(Control::default());
        control.want_keyframe();
        let shared = control.clone();
        std::thread::Builder::new()
            .name("discord-encode".into())
            .spawn(move || encode(profile, &waiting, &spare_back, &frames, &shared))?;
        let mut held: Option<Vec<u8>> = None;
        let sink = Sink::Pictures(Box::new(move |picture: Picture<'_>| {
            let mut data = held.take().or_else(|| spare.try_recv().ok()).unwrap_or_default();
            if !pack(&picture, &mut data) {
                held = Some(data);
                return;
            }
            let raw = Raw {
                width: picture.width,
                height: picture.height,
                timestamp_us: picture.timestamp_us,
                data,
            };
            if let Err(channel::TrySendError::Full(raw) | channel::TrySendError::Disconnected(raw)) =
                pictures.try_send(raw)
            {
                held = Some(raw.data);
            }
        }));
        Ok((
            sink,
            Self {
                frames: receiver,
                control,
            },
        ))
    }
}

/// The encoder thread: an encoder for each size the pictures come in.
fn encode(
    profile: Profile,
    pictures: &channel::Receiver<Raw>,
    spare: &channel::SyncSender<Vec<u8>>,
    frames: &mpsc::Sender<Encoded>,
    control: &Control,
) {
    let base: u32 = rand::random();
    let mut encoder: Option<H264Encoder> = None;
    let mut failed = false;
    while let Ok(raw) = pictures.recv() {
        let (w, h) = (raw.width, raw.height);
        let estimate = control.estimate.load(Ordering::Relaxed);
        let wanted = profile.target(h, estimate) / 1000;
        if encoder.as_ref().is_none_or(|e| e.size() != (w, h)) {
            encoder = H264Encoder::new(w, h, profile.fps, u32::try_from(wanted).unwrap_or(u32::MAX));
            if encoder.is_none() && !failed {
                failed = true;
                note(&format!("couldn't start the {:?} encoder at {w}x{h}", profile.codec()));
            }
            control.want_keyframe();
        }
        let Some(coder) = encoder.as_mut() else {
            let _ = spare.try_send(raw.data);
            continue;
        };
        if retune(u64::from(coder.bitrate()), wanted) {
            coder.set_bitrate(u32::try_from(wanted).unwrap_or(u32::MAX));
        }
        let (luma, chroma) = (w as usize * h as usize, (w as usize / 2) * (h as usize / 2));
        let (y, rest) = raw.data.split_at(luma.min(raw.data.len()));
        let (u, v) = rest.split_at(chroma.min(rest.len()));
        let planes = Planes {
            width: w,
            height: h,
            y,
            u,
            v,
            strides: [w, w / 2, w / 2],
        };
        let pts = raw.timestamp_us.saturating_mul(i64::from(CLOCK)) / 1_000_000;
        let keyframe = control.keyframe.swap(false, Ordering::Relaxed);
        let sent = match coder.encode(planes, pts, keyframe) {
            Ok(Some((data, keyframe))) => frames.try_send(Encoded {
                data: data.to_vec(),
                keyframe,
                timestamp: base.wrapping_add(pts as u32),
                size: (w, h),
            }),
            // Dropped to keep to the bitrate.
            Ok(None) => Ok(()),
            Err(_) => {
                control.want_keyframe();
                Ok(())
            }
        };
        match sent {
            Err(mpsc::error::TrySendError::Full(_)) => control.want_keyframe(),
            Err(mpsc::error::TrySendError::Closed(_)) => return,
            Ok(()) => {}
        }
        let _ = spare.try_send(raw.data);
    }
}

/// Sends one video over a connection: the encoder's frames, and answers
/// to what viewers and the server ask of it.
pub struct Sender {
    pub encoding: Encoding,
    profile: Profile,
    packetizer: Packetizer,
    history: History,
    /// The connection's SSRCs, once one is bound; frames before are dropped.
    ssrcs: Option<Ssrcs>,
    /// The size op 12 last announced.
    announced: Option<(u32, u32)>,
    /// Sender report counts: packets and payload bytes so far.
    packets: u32,
    octets: u32,
    reported: Instant,
    /// Whether frames last went out end-to-end encrypted, as logged.
    encrypting: Option<bool>,
    /// Frames sent since the first, and when it went: the rate is logged once.
    measured: Option<(Instant, u32)>,
    /// A frame past the free limit was refused (logged once).
    refused: bool,
}

impl Sender {
    /// `history` packets are kept to resend: a camera's few, a stream's
    /// two seconds at its cap.
    pub fn new(encoding: Encoding, profile: Profile, history: usize) -> Self {
        Self {
            encoding,
            profile,
            packetizer: Packetizer::new(0, profile.codec(), profile.content),
            history: History::new(history),
            ssrcs: None,
            announced: None,
            packets: 0,
            octets: 0,
            reported: Instant::now(),
            encrypting: None,
            measured: None,
            refused: false,
        }
    }

    /// Sends on `connection` from now on: a new connection's SSRCs, its
    /// own numbering, op 12 again with the next frame, which is a keyframe.
    pub fn bind(&mut self, connection: &Connection) {
        let ssrcs = connection.ssrcs();
        self.packetizer = Packetizer::new(ssrcs.video, self.profile.codec(), self.profile.content);
        self.history = History::new(self.history.capacity());
        self.ssrcs = Some(ssrcs);
        self.announced = None;
        self.encrypting = None;
        self.encoding.control.want_keyframe();
    }

    /// Announces the video again, as when someone connects and the server
    /// may not pass the last announcement on.
    pub async fn announce(&mut self, connection: &mut Connection) -> Result<()> {
        if let (Some(ssrcs), Some(size)) = (self.ssrcs, self.announced) {
            send_json(&mut connection.ws, op::VIDEO, self.attributes(ssrcs, size)).await?;
            self.soundshare(connection, ssrcs).await?;
        }
        Ok(())
    }

    /// A stream marks its audio SSRC as shared sound with op 5 (speaking
    /// flag 2, not a microphone), after each op 12, as Discord's client and
    /// Yapfast do. A camera's connection is the call's, whose microphone
    /// says its own.
    async fn soundshare(&self, connection: &mut Connection, ssrcs: Ssrcs) -> Result<()> {
        const SOUNDSHARE: u8 = 2;
        if self.profile.content == Content::Screen {
            send_json(
                &mut connection.ws,
                op::SPEAKING,
                serde_json::json!({"speaking": SOUNDSHARE, "delay": 0, "ssrc": ssrcs.audio}),
            )
            .await?;
        }
        Ok(())
    }

    fn attributes(&self, ssrcs: Ssrcs, size: (u32, u32)) -> serde_json::Value {
        let max = u32::try_from(self.profile.max_bitrate(size.1)).unwrap_or(u32::MAX);
        packetize::video_attributes(ssrcs, size, self.profile.fps, max)
    }

    /// Sends a frame: op 12 first when its size is new, then DAVE, packets
    /// and now and then a sender report. With `pace`, past a frame's first
    /// packets four go out a millisecond: a 1080p keyframe sent at once can
    /// overflow buffers on the way (Yapfast's finding).
    pub async fn send(&mut self, connection: &mut Connection, frame: Encoded, pace: bool) -> Result<()> {
        const BURST: usize = 8;
        const PACE: usize = 4;
        let Some(ssrcs) = self.ssrcs else { return Ok(()) };
        // Never past the free tier, whatever the capture handed over.
        if !within_free(frame.size) || self.profile.fps > FREE_FPS {
            if !std::mem::replace(&mut self.refused, true) {
                note(&format!(
                    "refused to send {}x{} at {} fps: past Discord's free {}x{} at {FREE_FPS}",
                    frame.size.0, frame.size.1, self.profile.fps, FREE_SIZE.0, FREE_SIZE.1
                ));
            }
            return Ok(());
        }
        if self.announced != Some(frame.size) {
            send_json(&mut connection.ws, op::VIDEO, self.attributes(ssrcs, frame.size)).await?;
            self.soundshare(connection, ssrcs).await?;
            self.announced = Some(frame.size);
            note(&format!(
                "{} sent at {}x{}, {} fps, {:?}, at most {} kbit/s (free limit {}x{} at {FREE_FPS} fps)",
                match self.profile.content {
                    Content::Screen => "our stream",
                    Content::Camera => "our camera",
                },
                frame.size.0,
                frame.size.1,
                self.profile.fps,
                self.profile.codec(),
                self.profile.max_bitrate(frame.size.1) / 1000,
                FREE_SIZE.0,
                FREE_SIZE.1,
            ));
        }
        match self.measured.as_mut() {
            None => self.measured = Some((Instant::now(), 1)),
            Some((since, count)) if *count != u32::MAX => {
                *count += 1;
                let elapsed = since.elapsed();
                if elapsed >= MEASURE {
                    note(&format!(
                        "{} frames went out at {:.1} a second over the first {} s",
                        match self.profile.content {
                            Content::Screen => "our stream's",
                            Content::Camera => "our camera's",
                        },
                        f64::from(*count) / elapsed.as_secs_f64(),
                        elapsed.as_secs()
                    ));
                    // Logged once.
                    *count = u32::MAX;
                }
            }
            Some(_) => {}
        }
        let encrypting = connection.dave.encrypting();
        if self.encrypting != Some(encrypting) {
            self.encrypting = Some(encrypting);
            note(&format!(
                "video sending, end-to-end encrypted: {encrypting} (DAVE {})",
                connection.dave.status()
            ));
        }
        let Some(payload) = connection.dave.encrypt_video(self.profile.codec(), &frame.data) else {
            // Held back until DAVE can seal it: the first sealed frame is a
            // keyframe, so viewers needn't wait for one.
            self.encoding.control.want_keyframe();
            return Ok(());
        };
        for (index, mut packet) in self
            .packetizer
            .packets(&payload, frame.timestamp)
            .into_iter()
            .enumerate()
        {
            if pace && index >= BURST && index % PACE == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            self.history.remember(&packet);
            let clear = rtp::rtp_clear_len(&packet).unwrap_or(packet.len());
            self.packets = self.packets.wrapping_add(1);
            self.octets = self
                .octets
                .wrapping_add(u32::try_from(packet.len() - clear).unwrap_or(0));
            connection.transport.seal(&mut packet, clear)?;
            connection.udp.send(&packet).await?;
        }
        if self.reported.elapsed() >= SENDER_REPORTS {
            self.reported = Instant::now();
            let mut report = packetize::sender_report(
                ssrcs.video,
                std::time::SystemTime::now(),
                frame.timestamp,
                self.packets,
                self.octets,
            );
            connection.transport.seal(&mut report, RTCP_CLEAR)?;
            connection.udp.send(&report).await?;
        }
        Ok(())
    }

    /// Answers RTCP about the video: a keyframe for a picture loss
    /// indication, resends for NACKs, and the server's bitrate estimate to
    /// the encoder.
    pub async fn feedback(&mut self, connection: &mut Connection, rtcp: &[u8]) -> Result<()> {
        let Some(ssrcs) = self.ssrcs else { return Ok(()) };
        let control = &self.encoding.control;
        if packetize::wants_keyframe(rtcp) {
            control.want_keyframe();
        }
        if let Some(estimate) = packetize::bandwidth_estimate(rtcp, ssrcs.video) {
            control.estimate.store(estimate, Ordering::Relaxed);
        }
        for lost in packetize::nacked(rtcp, ssrcs.video).into_iter().take(RESENDS) {
            let Some(mut again) = self
                .history
                .get(lost)
                .and_then(|original| self.packetizer.retransmit(original, ssrcs.rtx))
            else {
                continue;
            };
            let clear = rtp::rtp_clear_len(&again).unwrap_or(again.len());
            connection.transport.seal(&mut again, clear)?;
            connection.udp.send(&again).await?;
        }
        Ok(())
    }
}

/// Tells a connection our video stopped.
pub async fn withdraw(connection: &mut Connection) -> Result<()> {
    send_json(&mut connection.ws, op::VIDEO, packetize::no_video(connection.ssrc)).await
}

/// Stops a capture without waiting on it here: its thread may take a frame's
/// time to notice, which the call's sound mustn't wait for. Stopping is
/// asked at once, so no frame goes out after; the device is released on a
/// thread of its own.
pub fn release<T: Send + 'static>(capture: T, request_stop: impl FnOnce(&T)) {
    request_stop(&capture);
    let spawned = std::thread::Builder::new()
        .name("discord-capture-stop".into())
        .spawn(move || drop(capture));
    if let Err(e) = spawned {
        note(&format!("couldn't stop a capture on its own thread: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrates_follow_discords_caps_and_the_servers_estimate() {
        let screen = Profile {
            content: Content::Screen,
            fps: 30,
        };
        assert_eq!(screen.max_bitrate(720), 3_500_000);
        assert_eq!(screen.max_bitrate(1080), 9_000_000);
        assert_eq!(screen.target(1080, 0), 9_000_000, "no estimate yet");
        assert_eq!(screen.target(1080, 4_000_000), 4_000_000);
        assert_eq!(screen.target(1080, 100_000), 500_000, "never below the floor");
        assert_eq!(screen.target(720, 20_000_000), 3_500_000, "never above the cap");
        let camera = Profile {
            content: Content::Camera,
            fps: 30,
        };
        assert_eq!(camera.target(720, 0), 1_500_000);
        assert_eq!(camera.max_bitrate(720), 2_500_000);
        assert!(!retune(1000, 1090));
        assert!(retune(1000, 1200));
        assert!(retune(1000, 800));
    }

    #[test]
    fn the_free_limit_holds_either_way_round() {
        assert!(within_free((1280, 720)));
        assert!(within_free((720, 1280)), "a portrait camera");
        assert!(within_free((1104, 720)));
        assert!(!within_free((1920, 1080)));
        assert!(!within_free((1280, 722)));
    }

    #[test]
    fn pictures_pack_without_their_padding() {
        let y = [1, 2, 0, 0, 3, 4, 0, 0];
        let (u, v) = ([5, 0], [6, 0]);
        let picture = Picture {
            width: 2,
            height: 2,
            y: &y,
            u: &u,
            v: &v,
            strides: [4, 2, 2],
            timestamp_us: 0,
        };
        let mut out = vec![9; 3];
        assert!(pack(&picture, &mut out));
        assert_eq!(out, [1, 2, 3, 4, 5, 6]);
        let short = Picture { y: &y[..5], ..picture };
        assert!(!pack(&short, &mut out));
    }

    /// Pictures handed to the sink come out of the encoder thread as H264,
    /// a keyframe first, timed in the 90 kHz clock; asking for a keyframe
    /// gets one. No capture: the pictures are made here.
    #[test]
    fn the_encoder_thread_turns_pictures_into_h264_frames() {
        let (mut sink, mut encoding) = Encoding::start(Profile {
            content: Content::Camera,
            fps: 30,
        })
        .unwrap();
        let Sink::Pictures(take) = &mut sink else {
            panic!("a picture sink")
        };
        let (y, u, v) = (vec![128u8; 64 * 36], vec![128u8; 32 * 18], vec![128u8; 32 * 18]);
        let mut frames = vec![];
        for index in 0..3i64 {
            if index == 2 {
                encoding.control.want_keyframe();
            }
            take(Picture {
                width: 64,
                height: 36,
                y: &y,
                u: &u,
                v: &v,
                strides: [64, 32, 32],
                timestamp_us: index * 100_000,
            });
            frames.push(encoding.frames.blocking_recv().unwrap());
        }
        assert!(frames.iter().all(|f| f.size == (64, 36)));
        assert_eq!(
            frames.iter().map(|f| f.keyframe).collect::<Vec<_>>(),
            [true, false, true]
        );
        assert_eq!(frames[1].timestamp.wrapping_sub(frames[0].timestamp), 9000);
        drop(sink);
        assert!(
            encoding.frames.blocking_recv().is_none(),
            "the thread ends with its sink"
        );
    }

    /// A stream's pictures come out as H264 in Annex B form, a keyframe
    /// first with its parameter sets.
    #[test]
    fn a_streams_pictures_come_out_as_h264() {
        let (mut sink, mut encoding) = Encoding::start(Profile {
            content: Content::Screen,
            fps: 30,
        })
        .unwrap();
        let Sink::Pictures(take) = &mut sink else {
            panic!("a picture sink")
        };
        let (y, u, v) = (vec![128u8; 64 * 36], vec![128u8; 32 * 18], vec![128u8; 32 * 18]);
        take(Picture {
            width: 64,
            height: 36,
            y: &y,
            u: &u,
            v: &v,
            strides: [64, 32, 32],
            timestamp_us: 0,
        });
        let first = encoding.frames.blocking_recv().unwrap();
        assert!(first.keyframe);
        let units = packetize::nal_units(&first.data);
        assert_eq!(units.first().map(|u| u[0] & 0x1f), Some(7), "SPS first");
    }
}
