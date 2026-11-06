// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Receiving video in calls: cameras and screen shares. LiveKit's libwebrtc
//! decodes the stream; frames are converted to RGBA here and the UI turns the
//! latest one per track into a texture.

use crate::sync::lock;

use std::collections::HashMap;
use std::sync::Weak;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use livekit::prelude::*;
use livekit::webrtc::prelude::{VideoBuffer, VideoFormatType};
use livekit::webrtc::video_frame::native::VideoFrameBufferExt as _;
use livekit::webrtc::video_stream::native::NativeVideoStream;

/// Largest frame kept, per side; bigger streams are scaled down first.
const MAX_SIDE: u32 = 1920;
/// Frames converted per second at most; the rest are skipped.
const MAX_FPS: u64 = 30;
/// The least time between two shown frames of a received track.
const SHOWN_GAP_MS: u64 = 25;

pub struct Frame {
    pub size: [usize; 2],
    /// Shared, so the UI takes it out under the lock and copies it after:
    /// copying (8 MB for 1080p) while holding the lock held up every
    /// decoder handing over its next frame.
    pub rgba: Arc<Vec<u8>>,
    /// Increases with every new frame, so the UI only uploads changes.
    pub serial: u64,
}

/// The latest frame of each watched video track, by track sid.
pub static FRAMES: Mutex<Option<HashMap<String, Frame>>> = Mutex::new(None);

type RemoteOwners = HashMap<String, Weak<()>>;
static REMOTE_OWNERS: Mutex<Option<RemoteOwners>> = Mutex::new(None);

/// A stopped or replaced decoder must never restore a frame after it was
/// released, nor remove a replacement decoder's frame when it finishes.
struct RemoteOwner {
    sid: String,
    token: Arc<()>,
}

impl RemoteOwner {
    fn new(sid: String) -> Self {
        let owner = Self {
            sid,
            token: Arc::new(()),
        };
        let mut owners = lock(&REMOTE_OWNERS);
        owners
            .get_or_insert_with(HashMap::new)
            .insert(owner.sid.clone(), Arc::downgrade(&owner.token));
        clear_frame(&owner.sid);
        owner
    }

    fn is_current(&self, owners: &Option<RemoteOwners>) -> bool {
        owners
            .as_ref()
            .and_then(|owners| owners.get(&self.sid))
            .is_some_and(|token| token.ptr_eq(&Arc::downgrade(&self.token)))
    }

    fn put(&self, size: [usize; 2], rgba: Vec<u8>) -> bool {
        let owners = lock(&REMOTE_OWNERS);
        if !self.is_current(&owners) {
            return false;
        }
        lock(&FRAMES).get_or_insert_with(HashMap::new).insert(
            self.sid.clone(),
            Frame {
                size,
                rgba: Arc::new(rgba),
                serial: FRAME_SERIAL.fetch_add(1, Ordering::Relaxed),
            },
        );
        true
    }
}

impl Drop for RemoteOwner {
    fn drop(&mut self) {
        let mut owners = lock(&REMOTE_OWNERS);
        if self.is_current(&owners) {
            if let Some(owners) = owners.as_mut() {
                owners.remove(&self.sid);
            }
            clear_frame(&self.sid);
        }
    }
}

/// A video slot in [`FRAMES`] for a decoder outside LiveKit (Discord's
/// calls decode their own RTP video): it owns `sid`'s frame until dropped,
/// and a newer slot for the same sid replaces it, as a LiveKit subscription does.
pub struct FrameSlot(RemoteOwner);

impl FrameSlot {
    pub fn new(sid: String) -> Self {
        Self(RemoteOwner::new(sid))
    }

    /// Shows `rgba` (`size[0]` x `size[1]` pixels) as the slot's latest
    /// frame; false once the slot was replaced or stopped.
    pub fn put(&self, size: [usize; 2], rgba: Vec<u8>) -> bool {
        self.0.put(size, rgba)
    }

    /// Whether the slot still owns its frame (not replaced or stopped).
    pub fn is_current(&self) -> bool {
        self.0.is_current(&lock(&REMOTE_OWNERS))
    }
}

/// Scale `w`×`h` down to fit `MAX_SIDE`, keeping the aspect ratio.
fn fit(w: u32, h: u32) -> (u32, u32) {
    let s = (MAX_SIDE as f32 / w.max(h).max(1) as f32).min(1.0);
    (((w as f32 * s) as u32).max(2) & !1, ((h as f32 * s) as u32).max(2) & !1)
}

/// Converts frames of `track` until it ends, calling `repaint` after each one.
pub fn watch(
    track: RemoteVideoTrack,
    sid: String,
    repaint: Arc<dyn Fn() + Send + Sync>,
) -> tokio::task::JoinHandle<()> {
    use futures_util::StreamExt;
    // Own the slot before spawning, so even an abort before the first poll
    // releases it. A new subscription immediately replaces an older owner.
    let owner = RemoteOwner::new(sid);
    tokio::spawn(async move {
        let mut stream = NativeVideoStream::new(track.rtc_track());
        let mut last = Instant::now() - Duration::from_secs(1);
        while let Some(frame) = stream.next().await {
            if !owner.is_current(&lock(&REMOTE_OWNERS)) {
                break;
            }
            // A little under 30 fps's 33 ms, so a 30 fps track's jitter
            // doesn't skip every other frame (60 fps comes down to ~40).
            if last.elapsed() < Duration::from_millis(SHOWN_GAP_MS) {
                continue;
            }
            last = Instant::now();
            let mut i420 = frame.buffer.to_i420();
            let (w, h) = fit(i420.width(), i420.height());
            if (w, h) != (i420.width(), i420.height()) {
                i420 = i420.scale(w as i32, h as i32);
            }
            let mut rgba = vec![0u8; (w * h * 4) as usize];
            // libyuv's "ABGR" is R, G, B, A in memory: the RGBA order UIs take.
            i420.to_argb(VideoFormatType::ABGR, &mut rgba, w * 4, w as i32, h as i32);
            if owner.put([w as usize, h as usize], rgba) {
                repaint();
            }
        }
    })
}

/// An outgoing picture in I420, for a transport that encodes video itself
/// (Discord's calls) rather than through LiveKit's video source.
#[derive(Clone, Copy, Debug)]
pub struct Picture<'a> {
    pub width: u32,
    pub height: u32,
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub strides: [u32; 3],
    /// Since the capture started.
    pub timestamp_us: i64,
}

/// Where a capture's pictures go: LiveKit's video source, or a callback on
/// the capture thread, which should hand the picture on rather than encode
/// it there.
pub enum Sink {
    Source(livekit::webrtc::video_source::native::NativeVideoSource),
    Pictures(Box<dyn FnMut(Picture<'_>) + Send>),
}

impl From<livekit::webrtc::video_source::native::NativeVideoSource> for Sink {
    fn from(source: livekit::webrtc::video_source::native::NativeVideoSource) -> Self {
        Self::Source(source)
    }
}

impl Sink {
    pub(crate) fn deliver(&mut self, buffer: livekit::webrtc::prelude::I420Buffer, timestamp_us: i64) {
        match self {
            Self::Source(source) => {
                source.capture_frame(&livekit::webrtc::prelude::VideoFrame {
                    rotation: livekit::webrtc::prelude::VideoRotation::VideoRotation0,
                    timestamp_us,
                    frame_metadata: None,
                    buffer,
                });
            }
            Self::Pictures(take) => {
                let (y, u, v) = buffer.data();
                let (sy, su, sv) = buffer.strides();
                take(Picture {
                    width: buffer.width(),
                    height: buffer.height(),
                    y,
                    u,
                    v,
                    strides: [sy, su, sv],
                    timestamp_us,
                });
            }
        }
    }
}

/// Our own camera and screen share, previewed in the call grid.
pub const LOCAL_CAMERA: &str = "local:camera";
pub const LOCAL_SCREEN: &str = "local:screen";

/// Our screen share's preview, while it's focused (fullscreen): then kept
/// at the size it's sent and `MAX_FPS`; otherwise, like the camera's, small
/// and slow. Others' video isn't touched by this.
static FULL_PREVIEW: Mutex<Option<&'static str>> = Mutex::new(None);

/// The focused video's key: [`LOCAL_SCREEN`] makes our screen share's
/// preview full; anything else (or None) puts it back.
pub fn focus_preview(key: Option<&str>) {
    *lock(&FULL_PREVIEW) = key.filter(|k| *k == LOCAL_SCREEN).map(|_| LOCAL_SCREEN);
}

type PreviewOwners = HashMap<&'static str, Weak<()>>;
static PREVIEW_OWNERS: Mutex<Option<PreviewOwners>> = Mutex::new(None);
// Shared across subscriptions too: the presenter must upload the first frame
// after a resubscribe even if the previous decoder stopped at the same count.
static FRAME_SERIAL: AtomicU64 = AtomicU64::new(1);

/// A replacement capture takes ownership before its worker starts. A slow
/// previous worker can then neither overwrite nor remove the new preview.
pub struct PreviewOwner {
    key: &'static str,
    token: Arc<()>,
}

impl PreviewOwner {
    pub fn new(key: &'static str) -> Self {
        let owner = Self {
            key,
            token: Arc::new(()),
        };
        let mut owners = lock(&PREVIEW_OWNERS);
        owners
            .get_or_insert_with(HashMap::new)
            .insert(key, Arc::downgrade(&owner.token));
        clear_frame(key);
        owner
    }

    fn is_current(&self, owners: &Option<PreviewOwners>) -> bool {
        owners
            .as_ref()
            .and_then(|owners| owners.get(self.key))
            .is_some_and(|token| token.ptr_eq(&Arc::downgrade(&self.token)))
    }
}

impl Drop for PreviewOwner {
    fn drop(&mut self) {
        let mut owners = lock(&PREVIEW_OWNERS);
        if self.is_current(&owners) {
            if let Some(owners) = owners.as_mut() {
                owners.remove(self.key);
            }
            clear_frame(self.key);
        }
    }
}

/// Stores a preview of an outgoing frame under `key`, for our own tile.
/// Our screen share focused (fullscreen, see [`focus_preview`]) is the size
/// it's sent at `MAX_FPS`, to check what viewers get; otherwise ~10 a second
/// and 640 px on the long side, which is all a tile needs and costs little.
pub fn preview(
    owner: &PreviewOwner,
    frame: &mut livekit::webrtc::prelude::I420Buffer,
    last: &mut Instant,
    serial: &mut u64,
) {
    let full = *lock(&FULL_PREVIEW) == Some(owner.key);
    let every = if full { 1000 / MAX_FPS } else { 100 };
    if last.elapsed() < Duration::from_millis(every) {
        return;
    }
    *last = Instant::now();
    let (w, h) = (frame.width(), frame.height());
    let s = if full { 1.0 } else { (640.0 / w.max(h) as f32).min(1.0) };
    let (pw, ph) = (((w as f32 * s) as u32).max(2) & !1, ((h as f32 * s) as u32).max(2) & !1);
    let mut rgba = vec![0u8; (pw * ph * 4) as usize];
    if (pw, ph) == (w, h) {
        frame.to_argb(VideoFormatType::ABGR, &mut rgba, pw * 4, pw as i32, ph as i32);
    } else {
        frame
            .scale(pw as i32, ph as i32)
            .to_argb(VideoFormatType::ABGR, &mut rgba, pw * 4, pw as i32, ph as i32);
    }
    let owners = lock(&PREVIEW_OWNERS);
    if !owner.is_current(&owners) {
        return;
    }
    *serial = FRAME_SERIAL.fetch_add(1, Ordering::Relaxed);
    lock(&FRAMES).get_or_insert_with(HashMap::new).insert(
        owner.key.to_string(),
        Frame {
            size: [pw as usize, ph as usize],
            rgba: Arc::new(rgba),
            serial: *serial,
        },
    );
}

/// Forgets a track's last frame.
pub fn stop(sid: &str) {
    let mut previews = lock(&PREVIEW_OWNERS);
    if let Some(previews) = previews.as_mut() {
        previews.remove(sid);
    }
    let mut owners = lock(&REMOTE_OWNERS);
    if let Some(owners) = owners.as_mut() {
        owners.remove(sid);
    }
    clear_frame(sid);
}

fn clear_frame(sid: &str) {
    if let Some(f) = lock(&FRAMES).as_mut() {
        f.remove(sid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replaced_decoder_cannot_overwrite_or_clear_its_replacement() {
        let sid = "test:remote-replacement";
        let old = RemoteOwner::new(sid.into());
        assert!(old.put([2, 2], vec![1; 16]));
        let old_serial = lock(&FRAMES).as_ref().unwrap()[sid].serial;
        let current = RemoteOwner::new(sid.into());
        assert!(!lock(&FRAMES).as_ref().unwrap().contains_key(sid));
        assert!(current.put([2, 2], vec![2; 16]));
        let current_serial = lock(&FRAMES).as_ref().unwrap()[sid].serial;
        assert_ne!(old_serial, current_serial);
        assert!(!old.put([2, 2], vec![3; 16]));
        drop(old);
        assert_eq!(lock(&FRAMES).as_ref().unwrap()[sid].rgba, Arc::new(vec![2; 16]));
        drop(current);
        assert!(!lock(&FRAMES).as_ref().unwrap().contains_key(sid));
    }

    #[test]
    fn stopping_a_subscription_releases_its_frame_and_rejects_inflight_frames() {
        let sid = "test:remote-stop";
        let old = RemoteOwner::new(sid.into());
        assert!(old.put([2, 2], vec![1; 16]));
        stop(sid);
        assert!(!lock(&FRAMES).as_ref().unwrap().contains_key(sid));
        assert!(!old.put([2, 2], vec![2; 16]));
        let current = RemoteOwner::new(sid.into());
        assert!(current.put([2, 2], vec![3; 16]));
        drop(old);
        assert_eq!(lock(&FRAMES).as_ref().unwrap()[sid].rgba, Arc::new(vec![3; 16]));
        drop(current);
        assert!(!lock(&REMOTE_OWNERS).as_ref().unwrap().contains_key(sid));
    }

    #[test]
    fn stopping_a_preview_invalidates_its_owner_before_capture_finishes() {
        let key = "test:preview-stop";
        let old = PreviewOwner::new(key);
        lock(&FRAMES).get_or_insert_with(HashMap::new).insert(
            key.into(),
            Frame {
                size: [2, 2],
                rgba: Arc::new(vec![0; 16]),
                serial: 1,
            },
        );
        stop(key);
        assert!(!old.is_current(&lock(&PREVIEW_OWNERS)));
        assert!(!lock(&FRAMES).as_ref().unwrap().contains_key(key));
        let current = PreviewOwner::new(key);
        drop(old);
        assert!(current.is_current(&lock(&PREVIEW_OWNERS)));
    }

    #[test]
    fn replacing_capture_keeps_new_preview_when_old_owner_finishes() {
        let old = PreviewOwner::new("test:preview-ownership");
        let current = PreviewOwner::new("test:preview-ownership");
        assert!(!old.is_current(&lock(&PREVIEW_OWNERS)));
        assert!(current.is_current(&lock(&PREVIEW_OWNERS)));
        lock(&FRAMES).get_or_insert_with(HashMap::new).insert(
            current.key.into(),
            Frame {
                size: [2, 2],
                rgba: Arc::new(vec![0; 16]),
                serial: 42,
            },
        );
        drop(old);
        assert_eq!(lock(&FRAMES).as_ref().unwrap()[current.key].serial, 42);
        drop(current);
        assert!(!lock(&FRAMES).as_ref().unwrap().contains_key("test:preview-ownership"));
    }

    #[test]
    fn frames_fit_and_stay_even() {
        assert_eq!(fit(3840, 2160), (1920, 1080));
        assert_eq!(fit(1280, 720), (1280, 720));
        // I420 needs even sizes.
        assert_eq!(fit(641, 481), (640, 480));
    }
}
