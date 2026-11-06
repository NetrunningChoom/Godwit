// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Receiving video on a voice connection: a call's cameras, or a Go Live
//! stream's screen. Each sender's video SSRC is a feed: its packets are put
//! back in order (`depacketize::Reorder`, asking again for lost ones), into
//! frames (`depacketize::Assembler`), DAVE-decrypted with the sender's id,
//! and handed to a decoder thread of the connection's own, which shows them
//! in `godwit_voice::video::FRAMES` for the call grid. After a loss, or a
//! frame the decoder refused, the sender is asked for a keyframe (PLI), at
//! most every half second, as Yapfast's viewers do.
//!
//! Decoding runs off the media thread so a 1080p keyframe never delays a
//! frame of sound. Only what's shown is decoded: while the call isn't on
//! screen, frames are dropped before decoding (and a call's cameras aren't
//! even asked for), and showing them again starts from a keyframe.
//!
//! After Yapfast's `call_video.rs` and `stream_connection.rs` (GPL-3.0-or-later).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as channel;
use std::time::{Duration, Instant};

use anyhow::Result;
use godwit_media::live::LiveDecoder;
use godwit_voice::video::FrameSlot;
use serde_json::{Map, Value, json};

use super::connection::{Connection, op, send_json};
use super::depacketize::{self, Assembler, Codec, Held, Reorder};
use super::note;
use super::rtp::Packet;

/// How often a viewer may ask for a keyframe while frames are lost.
const KEYFRAME_REQUESTS: Duration = Duration::from_millis(500);
/// Frames waiting to be decoded; more means decoding can't keep up, and the
/// feed waits for a keyframe instead of falling ever further behind. A
/// dropped frame costs everything until the next keyframe (a slideshow), so
/// a stream gets a quarter second of room for a hiccup; cameras, up to nine
/// and small, a little less.
const QUEUED_STREAM: usize = 8;
const QUEUED_CAMERA: usize = 4;
/// The largest frame shown, per side (as LiveKit calls' `video.rs`), and the
/// most frames converted for showing a second; every frame is still decoded.
/// A frame shows once this long after the last: under the 33 ms of 30 fps,
/// so a 30 fps stream's jitter (frames 30 ms apart, then 36) doesn't skip
/// every other one, while 60 fps still comes down to about 40.
const MAX_SIDE: u32 = 1920;
const SHOWN_EVERY: Duration = Duration::from_millis(25);
/// The pixels a camera tile shows at, as op 15's `pixelCounts` tells the
/// server (a 720p tile): it forwards the layer that fits.
const TILE_PIXELS: u32 = 1280 * 720;

/// The sid a feed's frames are shown under, as the core's `CallVideo` names it.
pub fn sid(kind: FeedKind, user: u64) -> String {
    match kind {
        FeedKind::Camera => format!("discord:camera:{user}"),
        FeedKind::Screen => format!("discord:stream:{user}"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedKind {
    Camera,
    Screen,
}

impl FeedKind {
    fn name(self) -> &'static str {
        match self {
            Self::Camera => "camera",
            Self::Screen => "stream",
        }
    }
}

/// Seconds of the video tally in the log (see `Feeds::report`).
const TALLIES: u32 = 30;

/// One sender's video on its way to frames.
struct Feed {
    user: u64,
    sid: Arc<str>,
    rtx: Option<u32>,
    reorder: Reorder,
    assembler: Option<Assembler>,
    /// When a keyframe was last asked for.
    asked: Instant,
    /// Set by the decoder when a frame wouldn't decode.
    keyframe: Arc<AtomicBool>,
    /// For receiver reports: the counts at the last one.
    reported: (u32, u32),
}

/// The feeds of one connection, and its decoder thread.
pub(crate) struct Feeds {
    kind: FeedKind,
    /// By video SSRC.
    incoming: HashMap<u32, Feed>,
    /// The most feeds kept: a call's grid shows nine cameras at most (as
    /// Stoat calls do), and a stream's senders are few.
    most: usize,
    /// Cameras past `most`, by user: named in op 15 at quality 0, as `any`
    /// would otherwise have the server forward them in full.
    spare: HashMap<u64, u32>,
    visible: bool,
    decoder: Decoder,
    /// The newest sender report heard per SSRC: the middle of its NTP time
    /// and when, which receiver reports echo for the round trip.
    sender_reports: HashMap<u32, (u32, Instant)>,
    /// What the log was already told (once each): a black tile otherwise
    /// says nothing about where the video stopped.
    noted: Noted,
}

/// One-time log notes about why video isn't showing.
#[derive(Default)]
struct Noted {
    unknown_sender: bool,
    undecryptable: bool,
    hidden: bool,
    /// Video packets heard, by (SSRC, payload type), for the tally the
    /// log gets in the first half minute.
    packets: std::collections::BTreeMap<(u32, u8), u32>,
    /// Frames completed, and handed to the decoder.
    assembled: u32,
    decoded: u32,
    tallies: u32,
}

enum Job {
    Frame {
        sid: Arc<str>,
        codec: Codec,
        data: Vec<u8>,
        keyframe: Arc<AtomicBool>,
    },
    /// A feed ended: its decoder and frame go.
    Forget(Arc<str>),
}

/// The decoder thread of a connection's feeds, started with the first frame
/// to decode: a call nobody films in never has one.
struct Decoder {
    kind: FeedKind,
    jobs: Option<channel::SyncSender<Job>>,
    /// Wakes the UI after a frame is shown.
    shown: Arc<dyn Fn() + Send + Sync>,
}

impl Decoder {
    /// Hands `job` to the decoder thread, starting it if need be; false
    /// when its queue is full (or it couldn't start).
    fn send(&mut self, job: Job) -> bool {
        if self.jobs.is_none() {
            let queued = match self.kind {
                FeedKind::Camera => QUEUED_CAMERA,
                FeedKind::Screen => QUEUED_STREAM,
            };
            let (decoder, jobs) = channel::sync_channel(queued);
            let (name, threads) = match self.kind {
                // Cameras are small, and up to nine: one thread each.
                FeedKind::Camera => ("discord-cameras", 1),
                FeedKind::Screen => ("discord-stream-video", 4),
            };
            let shown = self.shown.clone();
            match std::thread::Builder::new()
                .name(name.into())
                .spawn(move || decode(&jobs, threads, &*shown))
            {
                Ok(_) => self.jobs = Some(decoder),
                Err(e) => {
                    note(&format!("couldn't start decoding video: {e}"));
                    return false;
                }
            }
        }
        self.jobs.as_ref().is_some_and(|jobs| jobs.try_send(job).is_ok())
    }

    /// Lets a feed's decoder and frame go.
    fn forget(&mut self, sid: Arc<str>) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.try_send(Job::Forget(sid));
        }
    }
}

impl Feeds {
    /// Feeds for `kind`, decoded on a thread of their own that wakes the UI
    /// through `shown` after each frame it shows.
    pub fn new(kind: FeedKind, most: usize, shown: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            kind,
            incoming: HashMap::new(),
            most,
            spare: HashMap::new(),
            visible: false,
            noted: Noted::default(),
            decoder: Decoder {
                kind,
                jobs: None,
                shown,
            },
            sender_reports: HashMap::new(),
        }
    }

    /// A new connection: SSRCs start over, and are announced again. A
    /// decoder whose goodbye doesn't fit the queue lasts until the thread
    /// ends with the connection; its frame is no longer listed meanwhile.
    pub fn reset(&mut self) {
        let feeds: Vec<Feed> = self.incoming.drain().map(|(_, feed)| feed).collect();
        for feed in feeds {
            self.decoder.forget(feed.sid);
        }
        self.spare.clear();
        self.sender_reports.clear();
    }

    /// Who sends video now, by user id, oldest SSRC first.
    pub fn senders(&self) -> Vec<u64> {
        let mut senders: Vec<(u32, u64)> = self.incoming.iter().map(|(&ssrc, f)| (ssrc, f.user)).collect();
        senders.sort_unstable();
        let mut users: Vec<u64> = senders.into_iter().map(|(_, user)| user).collect();
        users.dedup();
        users
    }

    /// Learns who sends video on which SSRC from op 12; a zero video SSRC
    /// means their video stopped. Whether that changed anything.
    pub fn learn(&mut self, op: u64, data: &Value) -> bool {
        if op != op::VIDEO {
            return false;
        }
        let Some(user) = data["user_id"].as_str().and_then(|id| id.parse::<u64>().ok()) else {
            return false;
        };
        let ssrc = |value: &Value| value.as_u64().filter(|&s| s != 0).and_then(|s| u32::try_from(s).ok());
        let video = ssrc(&data["video_ssrc"]);
        let before = self.incoming.len();
        let unchanged = video.is_some_and(|video| self.incoming.get(&video).is_some_and(|f| f.user == user));
        if unchanged {
            return false;
        }
        let spare_before = self.spare.remove(&user);
        let gone: Vec<u32> = self
            .incoming
            .iter()
            .filter(|(_, feed)| feed.user == user)
            .map(|(&ssrc, _)| ssrc)
            .collect();
        for ssrc in gone {
            if let Some(feed) = self.incoming.remove(&ssrc) {
                self.decoder.forget(feed.sid);
            }
        }
        let Some(video) = video else {
            return self.incoming.len() != before || spare_before.is_some();
        };
        if self.incoming.len() >= self.most {
            self.spare.insert(user, video);
            return self.incoming.len() != before || spare_before != Some(video);
        }
        let stream = data["streams"]
            .as_array()
            .and_then(|streams| streams.iter().find(|s| s["ssrc"].as_u64() == Some(u64::from(video))));
        // RTX follows the video SSRC when the stream doesn't say.
        let rtx = stream
            .and_then(|stream| ssrc(&stream["rtx_ssrc"]))
            .or_else(|| ssrc(&data["rtx_ssrc"]))
            .unwrap_or(video.wrapping_add(1));
        self.incoming.insert(
            video,
            Feed {
                user,
                sid: sid(self.kind, user).into(),
                rtx: Some(rtx),
                reorder: Reorder::default(),
                assembler: None,
                asked: Instant::now() - KEYFRAME_REQUESTS,
                keyframe: Arc::new(AtomicBool::new(true)),
                reported: (0, 0),
            },
        );
        true
    }

    /// Shows the video, or stops decoding it while it can't be seen.
    /// Showing again starts from a keyframe.
    pub fn set_visible(&mut self, visible: bool) {
        if visible && !self.visible {
            for feed in self.incoming.values_mut() {
                if let Some(assembler) = feed.assembler.as_mut() {
                    assembler.wait_for_keyframe();
                }
                feed.keyframe.store(true, Ordering::Relaxed);
            }
        }
        self.visible = visible;
    }

    /// Op 15 for a call: each camera at full quality while the call is on
    /// screen, none while it isn't. `any` covers what's left unnamed (sound
    /// above all) and must stay at 100; 0 stops a stream (Yapfast's
    /// `call_video.rs`).
    pub fn wants(&self) -> Value {
        let mut wants = Map::new();
        let mut pixels = Map::new();
        wants.insert("any".into(), json!(100));
        for &ssrc in self.incoming.keys() {
            let quality = if self.visible { 100 } else { 0 };
            wants.insert(ssrc.to_string(), json!(quality));
            if self.visible {
                pixels.insert(ssrc.to_string(), json!(TILE_PIXELS));
            }
        }
        for ssrc in self.spare.values() {
            wants.insert(ssrc.to_string(), json!(0));
        }
        wants.insert("pixelCounts".into(), Value::Object(pixels));
        Value::Object(wants)
    }

    pub async fn send_wants(&self, connection: &mut Connection) -> Result<()> {
        send_json(&mut connection.ws, op::MEDIA_SINK_WANTS, self.wants()).await
    }

    /// Notes a sender report from decrypted RTCP, for receiver reports.
    pub fn heard_rtcp(&mut self, rtcp: &[u8]) {
        for (sender, middle) in depacketize::sender_reports(rtcp) {
            if self.incoming.contains_key(&sender) {
                self.sender_reports.insert(sender, (middle, Instant::now()));
            }
        }
    }

    /// Takes a received video packet; false if it's no feed's. `chosen` is
    /// the codec Discord chose, for payload types that don't say.
    pub fn receive(&mut self, packet: &Packet, chosen: Option<Codec>) -> bool {
        if self.noted.tallies < TALLIES {
            *self
                .noted
                .packets
                .entry((packet.ssrc, packet.payload_type))
                .or_default() += 1;
        }
        let held = if let Some((&video, _)) = self.incoming.iter().find(|(_, feed)| feed.rtx == Some(packet.ssrc)) {
            match Held::unwrap_rtx(packet, video) {
                Some(held) => held,
                // Padding, to probe the path.
                None => return true,
            }
        } else if self.incoming.contains_key(&packet.ssrc) {
            Held::new(packet)
        } else {
            if !self.noted.unknown_sender && Codec::from_payload_type(packet.payload_type).is_some() {
                self.noted.unknown_sender = true;
                note(&format!(
                    "{} video arrives on ssrc {} that no op 12 named (known: {:?})",
                    self.kind.name(),
                    packet.ssrc,
                    self.incoming.keys().collect::<Vec<_>>()
                ));
            }
            return false;
        };
        let Some(feed) = self.incoming.get_mut(&held.ssrc) else {
            return true;
        };
        let Some(codec) = Codec::from_payload_type(held.payload_type)
            .or(chosen)
            .or_else(|| feed.assembler.as_ref().map(Assembler::codec))
        else {
            return true;
        };
        if feed.assembler.as_ref().is_none_or(|a| a.codec() != codec) {
            feed.assembler = Some(Assembler::new(codec));
        }
        feed.reorder.insert(held, std::time::Instant::now());
        true
    }

    /// Puts frames together from what arrived, hands them to the decoder,
    /// and asks again for what's missing, and for keyframes after a loss.
    pub async fn repair(&mut self, connection: &mut Connection) -> Result<()> {
        let now = std::time::Instant::now();
        for (&ssrc, feed) in &mut self.incoming {
            let Some(assembler) = feed.assembler.as_mut() else {
                // Nothing came yet: ask for a keyframe anyway. Discord's
                // server forwards a stream to a new viewer only from a
                // keyframe, so waiting for video first waits forever
                // (Yapfast asks as soon as op 12 names the SSRC).
                if self.visible && feed.keyframe.load(Ordering::Relaxed) && feed.asked.elapsed() >= KEYFRAME_REQUESTS {
                    feed.asked = Instant::now();
                    let mut request = depacketize::picture_loss(connection.ssrc, ssrc);
                    connection.transport.seal(&mut request, depacketize::RTCP_CLEAR)?;
                    connection.udp.send(&request).await?;
                }
                continue;
            };
            let codec = assembler.codec();
            let mut lost = feed.keyframe.load(Ordering::Relaxed);
            for held in feed.reorder.ready(now) {
                let pushed = assembler.push(&held.packet());
                lost |= pushed.lost;
                let Some(frame) = pushed.frame else { continue };
                self.noted.assembled = self.noted.assembled.saturating_add(1);
                if !self.visible {
                    if !self.noted.hidden {
                        self.noted.hidden = true;
                        note(&format!(
                            "{} video arrives while the call isn't on screen",
                            self.kind.name()
                        ));
                    }
                    continue;
                }
                let Some(data) = connection.dave.decrypt_video(feed.user, frame.data) else {
                    if !self.noted.undecryptable {
                        self.noted.undecryptable = true;
                        note(&format!(
                            "{} video won't decrypt ({}; DAVE {})",
                            self.kind.name(),
                            connection.dave.unreadable.as_deref().unwrap_or("?"),
                            connection.dave.status()
                        ));
                    }
                    // What follows predicts from this frame.
                    assembler.wait_for_keyframe();
                    lost = true;
                    continue;
                };
                let job = Job::Frame {
                    sid: feed.sid.clone(),
                    codec,
                    data,
                    keyframe: feed.keyframe.clone(),
                };
                self.noted.decoded = self.noted.decoded.saturating_add(1);
                if !self.decoder.send(job) {
                    // Decoding is behind; what follows needs this frame.
                    assembler.wait_for_keyframe();
                    lost = true;
                }
            }
            let missing = feed.reorder.to_ask(now);
            if !missing.is_empty() {
                let mut request = depacketize::nack(connection.ssrc, ssrc, &missing);
                connection.transport.seal(&mut request, depacketize::RTCP_CLEAR)?;
                connection.udp.send(&request).await?;
            }
            if lost && self.visible && feed.asked.elapsed() >= KEYFRAME_REQUESTS {
                feed.asked = Instant::now();
                feed.keyframe.store(false, Ordering::Relaxed);
                let mut request = depacketize::picture_loss(connection.ssrc, ssrc);
                connection.transport.seal(&mut request, depacketize::RTCP_CLEAR)?;
                connection.udp.send(&request).await?;
            }
        }
        Ok(())
    }

    /// Receiver reports for every feed, sent about once a second, as WebRTC
    /// receivers do: the sender's rate control goes by them.
    pub async fn report(&mut self, connection: &mut Connection) -> Result<()> {
        // Every 5 s for the first 30: what arrives and how far it gets, so a
        // black tile shows where its video stops.
        if self.noted.tallies < TALLIES {
            self.noted.tallies += 1;
            if self.noted.tallies.is_multiple_of(5) {
                note(&format!(
                    "{} video tally: packets (ssrc, payload type) {:?}; feeds {:?}; codec {:?}; visible {}; {} frames assembled, {} decrypted",
                    self.kind.name(),
                    self.noted.packets,
                    self.incoming
                        .iter()
                        .map(|(s, f)| (*s, f.rtx, f.assembler.as_ref().map(Assembler::codec)))
                        .collect::<Vec<_>>(),
                    connection.video_codec,
                    self.visible,
                    self.noted.assembled,
                    self.noted.decoded,
                ));
            }
        }
        for (&ssrc, feed) in &mut self.incoming {
            let Some((highest, lost)) = feed.reorder.counts() else {
                continue;
            };
            let (expected, missed) = (
                highest.wrapping_sub(feed.reported.0),
                lost.saturating_sub(feed.reported.1),
            );
            let fraction = u8::try_from((u64::from(missed) * 256 / u64::from(expected.max(1))).min(255)).unwrap_or(255);
            feed.reported = (highest, lost);
            let (middle, delay) = self.sender_reports.get(&ssrc).map_or((0, 0), |&(middle, heard)| {
                (middle, (heard.elapsed().as_secs_f64() * 65536.0) as u32)
            });
            let mut report =
                depacketize::receiver_report(connection.ssrc, ssrc, (highest, lost, fraction), (middle, delay));
            connection.transport.seal(&mut report, depacketize::RTCP_CLEAR)?;
            connection.udp.send(&report).await?;
        }
        Ok(())
    }
}

impl Drop for Feeds {
    fn drop(&mut self) {
        // Dropping the sender ends the decoder thread, which lets every
        // frame go; a full queue is no matter then.
        self.reset();
    }
}

/// One feed's decoder and where its frames show.
struct Shown {
    decoder: LiveDecoder,
    codec: Codec,
    slot: FrameSlot,
    last: Instant,
    rgba: Vec<u8>,
    /// Frames shown and refused so far, for the log's first-of-each note.
    shown: u32,
    failed: u32,
}

/// The decoder thread: decodes every frame, shows at most 30 a second.
fn decode(jobs: &channel::Receiver<Job>, threads: usize, shown: &(dyn Fn() + Send + Sync)) {
    let mut feeds: HashMap<Arc<str>, Shown> = HashMap::new();
    while let Ok(job) = jobs.recv() {
        let (sid, codec, data, keyframe) = match job {
            Job::Forget(sid) => {
                feeds.remove(&sid);
                continue;
            }
            Job::Frame {
                sid,
                codec,
                data,
                keyframe,
            } => (sid, codec, data, keyframe),
        };
        let entry = feeds.entry(sid.clone());
        let feed = match entry {
            std::collections::hash_map::Entry::Occupied(occupied) if occupied.get().codec == codec => {
                occupied.into_mut()
            }
            entry => {
                let Some(decoder) = LiveDecoder::with_threads(codec.live(), threads) else {
                    note(&format!("can't decode {} video", codec.name()));
                    continue;
                };
                let fresh = Shown {
                    decoder,
                    codec,
                    slot: FrameSlot::new(sid.to_string()),
                    last: Instant::now() - SHOWN_EVERY,
                    rgba: Vec::new(),
                    shown: 0,
                    failed: 0,
                };
                match entry {
                    std::collections::hash_map::Entry::Occupied(mut occupied) => {
                        occupied.insert(fresh);
                        occupied.into_mut()
                    }
                    std::collections::hash_map::Entry::Vacant(vacant) => vacant.insert(fresh),
                }
            }
        };
        let show = feed.last.elapsed() >= SHOWN_EVERY;
        match feed.decoder.decode(&data, show, MAX_SIDE, &mut feed.rgba) {
            Ok(Some(picture)) if show => {
                feed.last = Instant::now();
                let size = [picture.width as usize, picture.height as usize];
                if feed.shown == 0 {
                    note(&format!("{sid}: first frame, {}x{} {}", size[0], size[1], codec.name()));
                }
                feed.shown = feed.shown.saturating_add(1);
                if feed.slot.put(size, std::mem::take(&mut feed.rgba)) {
                    shown();
                }
            }
            Ok(_) => {}
            Err(e) => {
                if feed.failed == 0 {
                    note(&format!("{sid}: a frame didn't decode ({e:?})"));
                }
                feed.failed = feed.failed.saturating_add(1);
                keyframe.store(true, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feeds() -> Feeds {
        Feeds::new(FeedKind::Camera, 2, Arc::new(|| {}))
    }

    #[test]
    fn learns_cameras_from_op_12_and_forgets_stopped_ones() {
        let mut feeds = feeds();
        assert!(feeds.learn(
            12,
            &json!({"user_id": "7", "audio_ssrc": 100, "video_ssrc": 101,
                    "streams": [{"type": "video", "rid": "100", "ssrc": 101, "rtx_ssrc": 102, "active": true}]})
        ));
        assert_eq!(feeds.incoming[&101].rtx, Some(102));
        assert!(
            !feeds.learn(12, &json!({"user_id": "7", "audio_ssrc": 100, "video_ssrc": 101})),
            "again"
        );
        assert!(feeds.learn(
            12,
            &json!({"user_id": "8", "audio_ssrc": 200, "video_ssrc": 201, "streams": []})
        ));
        assert_eq!(feeds.incoming[&201].rtx, Some(202), "RTX follows when unsaid");
        assert_eq!(&*feeds.incoming[&201].sid, "discord:camera:8");
        assert!(
            feeds.learn(12, &json!({"user_id": "9", "video_ssrc": 301})),
            "past the most kept"
        );
        assert_eq!(feeds.senders(), [7, 8], "not shown");
        assert_eq!(feeds.wants()["301"], 0, "and not forwarded");
        assert!(feeds.learn(12, &json!({"user_id": "7", "audio_ssrc": 100, "video_ssrc": 0})));
        assert_eq!(feeds.senders(), [8]);
        assert!(!feeds.learn(5, &json!({"user_id": "8", "ssrc": 200})));
    }

    #[test]
    fn asks_for_cameras_only_while_shown() {
        let mut feeds = feeds();
        feeds.learn(12, &json!({"user_id": "7", "video_ssrc": 101}));
        assert_eq!(feeds.wants(), json!({"any": 100, "101": 0, "pixelCounts": {}}));
        feeds.set_visible(true);
        assert_eq!(
            feeds.wants(),
            json!({"any": 100, "101": 100, "pixelCounts": {"101": TILE_PIXELS}})
        );
    }

    #[test]
    fn takes_video_and_its_retransmissions_but_not_strangers() {
        let mut feeds = feeds();
        feeds.learn(12, &json!({"user_id": "7", "video_ssrc": 101, "rtx_ssrc": 102}));
        let packet = |ssrc, payload_type, payload: &'static [u8]| Packet {
            payload_type,
            marker: false,
            sequence: 5,
            timestamp: 9,
            ssrc,
            payload,
        };
        assert!(feeds.receive(&packet(101, depacketize::VP8, &[0x10, 0]), None));
        assert_eq!(
            feeds.incoming[&101].assembler.as_ref().map(Assembler::codec),
            Some(Codec::Vp8)
        );
        // A resent packet: its sequence number first. A duplicate of the
        // one already held, so nothing new is held.
        assert!(feeds.receive(&packet(102, depacketize::VP8_RTX, &[0, 5, 0x10, 0]), None));
        assert_eq!(feeds.incoming[&101].reorder.counts(), Some((5, 0)));
        assert!(
            !feeds.receive(&packet(900, depacketize::VP8, &[0x10]), None),
            "nobody's"
        );
        // Discord's choice covers a payload type that doesn't say.
        feeds.learn(12, &json!({"user_id": "8", "video_ssrc": 201}));
        assert!(feeds.receive(&packet(201, 96, &[0x41]), Some(Codec::H264)));
        assert_eq!(
            feeds.incoming[&201].assembler.as_ref().map(Assembler::codec),
            Some(Codec::H264)
        );
    }

    #[test]
    fn the_decoder_thread_shows_frames_under_their_sid() {
        if !Codec::decoded().contains(&Codec::H264) {
            return;
        }
        // 32x24 red, then lime (see godwit-media's live.rs tests).
        let stream: &[u8] = include_bytes!("../../../godwit-media/tests/fixtures/rgb-h264.h264");
        let at = (0..stream.len() - 3)
            .find(|&at| stream[at..at + 3] == [0, 0, 1] && stream[at + 3] & 0x1f == 1)
            .unwrap();
        let first = stream[..at - 1].to_vec();
        let (tx, jobs) = channel::sync_channel(4);
        let (shown_tx, shown) = channel::channel();
        let thread = std::thread::spawn(move || decode(&jobs, 1, &move || shown_tx.send(()).unwrap()));
        let keyframe = Arc::new(AtomicBool::new(false));
        let sid: Arc<str> = "test:discord:camera:7".into();
        let job = |data: Vec<u8>| Job::Frame {
            sid: sid.clone(),
            codec: Codec::H264,
            data,
            keyframe: keyframe.clone(),
        };
        tx.send(job(vec![0, 0, 0, 1, 0x65, 0xff])).unwrap();
        tx.send(job(first)).unwrap();
        shown.recv_timeout(Duration::from_secs(5)).expect("a frame shown");
        {
            let frames = godwit_voice::video::FRAMES.lock().unwrap();
            let frame = &frames.as_ref().unwrap()[&*sid];
            assert_eq!(frame.size, [32, 24]);
        }
        assert!(
            keyframe.load(Ordering::Relaxed),
            "the broken frame asked for a keyframe"
        );
        tx.send(Job::Forget(sid.clone())).unwrap();
        drop(tx);
        thread.join().unwrap();
        let frames = godwit_voice::video::FRAMES.lock().unwrap();
        assert!(!frames.as_ref().unwrap().contains_key(&*sid), "forgotten");
    }

    #[test]
    fn sids_name_the_sender_and_what_they_share() {
        assert_eq!(sid(FeedKind::Camera, 7), "discord:camera:7");
        assert_eq!(sid(FeedKind::Screen, 7), "discord:stream:7");
    }
}
