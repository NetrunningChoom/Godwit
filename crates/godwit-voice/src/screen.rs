// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Sharing your screen in a call, with the official client's quality
//! presets. Capture uses libwebrtc's desktop capturer (bundled with LiveKit):
//! on Linux and macOS through the system picker (the desktop portal on
//! Wayland); on Windows, which has no such picker, Godwit lists the screens
//! and the apps' windows itself ([`sources`]) and shares the one [`pick`]ed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use livekit::webrtc::desktop_capturer::{DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions};
use livekit::webrtc::native::yuv_helper;
use livekit::webrtc::prelude::{I420Buffer, VideoResolution};
use livekit::webrtc::video_source::native::NativeVideoSource;
use serde::{Deserialize, Serialize};

/// The official screen share qualities (LiveKit's screen-share presets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenQuality {
    /// "720p 30FPS", the default.
    Low,
    /// "1080p 30FPS".
    High,
    /// "Source 5FPS": full resolution for reading text.
    Text,
}

impl ScreenQuality {
    pub const ALL: [ScreenQuality; 3] = [Self::Low, Self::High, Self::Text];

    /// Official rtc/state.tsx: low is always offered; high/text require 1080p.
    pub fn allowed(self, limit: [u32; 2]) -> bool {
        self == Self::Low || ((limit[0] == 0 || limit[0] >= 1920) && (limit[1] == 0 || limit[1] >= 1080))
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "720p 30FPS",
            Self::High => "1080p 30FPS",
            Self::Text => "Source 5FPS",
        }
    }

    /// Largest width × height sent (0 = source size), frames per second and
    /// maximum bitrate.
    pub fn preset(self) -> (u32, u32, u32, u64) {
        match self {
            Self::Low => (1280, 720, 30, 2_000_000),
            Self::High => (1920, 1080, 30, 5_000_000),
            Self::Text => (0, 0, 5, 7_000_000),
        }
    }
}

/// Per-share limits are runtime metadata, never persisted as user preferences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenOptions {
    pub quality: ScreenQuality,
    pub resolution: [u32; 2],
}
impl ScreenOptions {
    pub fn new(quality: ScreenQuality, resolution: [u32; 2]) -> Self {
        Self {
            quality: if quality.allowed(resolution) {
                quality
            } else {
                ScreenQuality::Low
            },
            resolution,
        }
    }
    pub fn size(self, w: u32, h: u32) -> (u32, u32) {
        let (mw, mh, _, _) = self.quality.preset();
        fit_size(w, h, [bounded(mw, self.resolution[0]), bounded(mh, self.resolution[1])])
    }
}
impl From<ScreenQuality> for ScreenOptions {
    fn from(quality: ScreenQuality) -> Self {
        Self::new(quality, [0, 0])
    }
}
fn bounded(a: u32, b: u32) -> u32 {
    if a == 0 {
        b
    } else if b == 0 {
        a
    } else {
        a.min(b)
    }
}

/// Keep aspect ratio and even I420 sides, never upscaling. Zero means unlimited.
pub fn fit_size(w: u32, h: u32, limit: [u32; 2]) -> (u32, u32) {
    let sx = if limit[0] == 0 {
        1.0
    } else {
        limit[0] as f32 / w.max(1) as f32
    };
    let sy = if limit[1] == 0 {
        1.0
    } else {
        limit[1] as f32 / h.max(1) as f32
    };
    let scale = sx.min(sy).min(1.0);
    (
        ((w as f32 * scale) as u32).max(2) & !1,
        ((h as f32 * scale) as u32).max(2) & !1,
    )
}

pub fn send_size(w: u32, h: u32, quality: ScreenQuality) -> (u32, u32) {
    ScreenOptions::from(quality).size(w, h)
}

/// Whether Godwit lists screens and windows itself: where the system has no
/// picker of its own (Windows), as Yapfast does.
pub const LISTS_SOURCES: bool = cfg!(windows);

/// A screen or an app's window that can be shared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// libwebrtc's id for it (a screen's index or a window's handle).
    pub id: u64,
    pub title: String,
    /// A window rather than a whole screen.
    pub window: bool,
}

/// The screens, then the windows with a title ([`LISTS_SOURCES`] only;
/// empty elsewhere). Screens are named "Entire screen", or "Screen 1", 2, …
#[cfg(windows)]
pub fn sources() -> (Vec<Source>, Vec<Source>) {
    let list = |kind| {
        DesktopCapturer::new(DesktopCapturerOptions::new(kind))
            .map(|capturer| capturer.get_source_list())
            .unwrap_or_default()
    };
    let screens = list(DesktopCaptureSourceType::Screen);
    let count = screens.len();
    let screens = screens
        .iter()
        .enumerate()
        .map(|(index, screen)| Source {
            id: screen.id(),
            title: if count == 1 {
                "Entire screen".to_owned()
            } else {
                format!("Screen {}", index + 1)
            },
            window: false,
        })
        .collect();
    let windows = list(DesktopCaptureSourceType::Window)
        .iter()
        // Godwit's own windows (the chat, a player) aren't offered: sharing
        // them shows the call inside itself.
        .filter(|window| !own_window(window.id()))
        .filter_map(|window| {
            let title = window.title();
            (!title.trim().is_empty()).then(|| Source {
                id: window.id(),
                title,
                window: true,
            })
        })
        .collect();
    (screens, windows)
}

/// The process owning a window (an HWND, libwebrtc's window id).
#[cfg(windows)]
pub(crate) fn window_process(hwnd: u64) -> Option<u32> {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetWindowThreadProcessId(hwnd: isize, process_id: *mut u32) -> u32;
    }
    let mut pid = 0u32;
    // SAFETY: reads only the window's owner; an HWND that has gone since
    // the listing returns 0 and leaves `pid` at 0.
    unsafe { GetWindowThreadProcessId(hwnd as isize, &mut pid) };
    (pid != 0).then_some(pid)
}

/// Whether the window is this process's.
#[cfg(windows)]
fn own_window(hwnd: u64) -> bool {
    window_process(hwnd) == Some(std::process::id())
}

#[cfg(not(windows))]
pub fn sources() -> (Vec<Source>, Vec<Source>) {
    (Vec::new(), Vec::new())
}

/// What the next share captures where Godwit lists sources itself: `None`
/// is the first screen.
static PICKED: std::sync::Mutex<Option<Source>> = std::sync::Mutex::new(None);

/// Shares `source` from the next share on (see [`LISTS_SOURCES`]).
pub fn pick(source: Option<Source>) {
    *super::sync::lock(&PICKED) = source;
}

/// The source picked for the next share (Windows' own picker).
#[cfg(windows)]
pub(crate) fn picked() -> Option<Source> {
    super::sync::lock(&PICKED).clone()
}

/// A running capture; dropping it stops capturing and waits (bounded) until
/// the capturer is gone.
pub struct Capture {
    _worker: super::worker::Worker,
}

impl Capture {
    pub fn request_stop(&self) {
        super::video::stop(super::video::LOCAL_SCREEN);
        self._worker.request_stop();
    }
}

/// Starts capturing into `sink` (LiveKit's video source, or a transport's
/// own encoder) on its own thread. `on_end` runs if the capture can't start
/// or the user cancels the system picker.
pub fn start(
    sink: impl Into<super::video::Sink>,
    options: ScreenOptions,
    on_end: impl Fn(String) + Send + 'static,
    repaint: Arc<dyn Fn() + Send + Sync>,
) -> std::io::Result<Capture> {
    let preview = super::video::PreviewOwner::new(super::video::LOCAL_SCREEN);
    let mut sink = sink.into();
    let worker = super::worker::Worker::spawn("screen-capture", move |flag| {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let kind = DesktopCaptureSourceType::Generic;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let picked = super::sync::lock(&PICKED).clone();
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let kind = if picked.as_ref().is_some_and(|p| p.window) {
            DesktopCaptureSourceType::Window
        } else {
            DesktopCaptureSourceType::Screen
        };
        let mut capturer_options = DesktopCapturerOptions::new(kind);
        capturer_options.set_include_cursor(true);
        let Some(mut capturer) = DesktopCapturer::new(capturer_options) else {
            on_end("Screen capture isn't available on this system.".into());
            return;
        };
        // With the system picker (Generic) the user chooses; otherwise the
        // one picked in Godwit's list, else the first screen.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let pick = None;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let pick = {
            let mut listed = capturer.get_source_list().into_iter();
            let found = match &picked {
                Some(picked) => listed.find(|source| source.id() == picked.id),
                None => listed.next(),
            };
            if found.is_none() {
                on_end("That screen or window is gone.".into());
                return;
            }
            found
        };
        let started = Instant::now();
        let (mut preview_at, mut preview_serial) = (started - std::time::Duration::from_secs(1), 0u64);
        let failed = Arc::new(AtomicBool::new(false));
        let failed_cb = failed.clone();
        let stopped = flag.clone();
        capturer.start_capture(pick, move |result| match result {
            // Stopped while a frame was on its way: it must not go out.
            Ok(_) if stopped.stopped() => {}
            Ok(frame) => {
                let (w, h) = (frame.width() as u32, frame.height() as u32);
                if w < 2 || h < 2 {
                    return;
                }
                // Desktop frames are BGRA in memory, libyuv's "ARGB".
                let mut full = I420Buffer::new(w & !1, h & !1);
                let (sy, su, sv) = full.strides();
                let (y, u, v) = full.data_mut();
                yuv_helper::argb_to_i420(
                    frame.data(),
                    frame.stride(),
                    y,
                    sy,
                    u,
                    su,
                    v,
                    sv,
                    (w & !1) as i32,
                    (h & !1) as i32,
                );
                let (tw, th) = options.size(w, h);
                let mut buffer = if (tw, th) == (w & !1, h & !1) {
                    full
                } else {
                    full.scale(tw as i32, th as i32)
                };
                let ts = started.elapsed().as_micros() as i64;
                let previous_serial = preview_serial;
                super::video::preview(&preview, &mut buffer, &mut preview_at, &mut preview_serial);
                if previous_serial != preview_serial {
                    repaint();
                }
                sink.deliver(buffer, ts);
            }
            // A temporary error just skips the frame; a permanent one (e.g. the
            // picker was cancelled) ends the share.
            Err(e) => {
                if format!("{e:?}").contains("Permanent") {
                    failed_cb.store(true, Ordering::Relaxed);
                }
            }
        });
        let (_, _, fps, _) = options.quality.preset();
        let interval = Duration::from_millis(1000 / fps as u64);
        while !flag.stopped() {
            if failed.load(Ordering::Relaxed) {
                on_end("Screen sharing stopped.".into());
                return;
            }
            // The system's picker answers through GLib (Linux): run its events.
            #[cfg(target_os = "linux")]
            glib::pump();
            capturer.capture_frame();
            std::thread::sleep(interval);
        }
    })?;
    Ok(Capture { _worker: worker })
}

/// The video source to publish for a share at `quality`.
pub fn video_source(options: ScreenOptions) -> NativeVideoSource {
    let (w, h, _, _) = options.quality.preset();
    let (w, h) = options.size(if w == 0 { 1920 } else { w }, if h == 0 { 1080 } else { h });
    NativeVideoSource::new(VideoResolution { width: w, height: h }, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_limits_filter_quality_and_bound_even_source_resolution() {
        assert!(!ScreenQuality::High.allowed([1280, 720]));
        assert!(!ScreenQuality::Text.allowed([1280, 720]));
        assert!(ScreenQuality::Text.allowed([0, 1080]));
        let fallback = ScreenOptions::new(ScreenQuality::High, [1280, 720]);
        assert_eq!(fallback.quality, ScreenQuality::Low);
        assert_eq!(fallback.size(3840, 2160), (1280, 720));
        assert_eq!(
            ScreenOptions::new(ScreenQuality::Text, [1920, 1080]).size(3840, 2160),
            (1920, 1080)
        );
        assert_eq!(
            ScreenOptions::new(ScreenQuality::Text, [0, 1080]).size(3440, 1440),
            (2580, 1080)
        );
        assert_eq!(
            ScreenOptions::new(ScreenQuality::Low, [640, 360]).size(1920, 1080),
            (640, 360)
        );
    }

    #[test]
    fn shares_fit_their_preset() {
        assert_eq!(send_size(3840, 2160, ScreenQuality::Low), (1280, 720));
        assert_eq!(send_size(3840, 2160, ScreenQuality::High), (1920, 1080));
        assert_eq!(send_size(3840, 2160, ScreenQuality::Text), (3840, 2160), "source size");
        // Ultrawide keeps its shape inside 720p.
        assert_eq!(send_size(3440, 1440, ScreenQuality::Low), (1280, 534));
        assert_eq!(send_size(800, 600, ScreenQuality::Low), (800, 600), "never upscaled");
    }
}

/// libwebrtc's screen capture on Linux asks the desktop portal for a screen
/// or window over D-Bus through GIO, and the answers (the system's picker,
/// then the PipeWire stream) arrive only while GLib's main context runs:
/// without this the picker never opened. Yapfast pumps it the same way
/// (`run_glib` in its `stream_worker.rs`). GLib is linked by build.rs.
#[cfg(target_os = "linux")]
mod glib {
    use std::ffi::c_void;

    unsafe extern "C" {
        fn g_main_context_iteration(context: *mut c_void, may_block: i32) -> i32;
    }

    /// Runs what's pending in GLib's default main context, without waiting.
    pub fn pump() {
        // Bounded, so a flood of events can't hold up capturing.
        for _ in 0..64 {
            // SAFETY: a null context means GLib's default one, and
            // `may_block` 0 only dispatches what's ready; the call has no
            // other preconditions.
            if unsafe { g_main_context_iteration(std::ptr::null_mut(), 0) } == 0 {
                break;
            }
        }
    }
}
