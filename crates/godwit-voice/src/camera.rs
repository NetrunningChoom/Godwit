// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Sending your webcam in a call (the official camera toggle). Capture uses
//! `nokhwa`: V4L2 on Linux, Media Foundation on Windows, AVFoundation on macOS.

use std::sync::Arc;
use std::time::Instant;

use livekit::webrtc::native::yuv_helper;
use livekit::webrtc::prelude::{I420Buffer, VideoResolution};
use livekit::webrtc::video_source::native::NativeVideoSource;
use nokhwa::Camera;
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{ApiBackend, CameraFormat, FrameFormat, RequestedFormat, RequestedFormatType, Resolution};

/// LiveKit's camera preset the official client publishes with (h720): size,
/// frame rate and maximum bitrate.
pub const PRESET: (u32, u32, u32, u64) = (1280, 720, 30, 1_700_000);

/// The cameras that can capture, in the system's order. On Linux each UVC
/// camera also has a metadata node with the same name that never delivers
/// frames; those are left out (and nokhwa lists newest first).
fn cameras() -> Vec<nokhwa::utils::CameraInfo> {
    let mut cams: Vec<_> = nokhwa::query(ApiBackend::Auto)
        .unwrap_or_default()
        .into_iter()
        .filter(capture_node)
        .collect();
    cams.sort_by_key(|c| c.index().as_index().unwrap_or(u32::MAX));
    cams
}

#[cfg(target_os = "linux")]
fn capture_node(c: &nokhwa::utils::CameraInfo) -> bool {
    // The kernel numbers a device's nodes from 0; the capture node is 0.
    let Ok(n) = c.index().as_index() else { return true };
    std::fs::read_to_string(format!("/sys/class/video4linux/video{n}/index")).map_or(true, |i| i.trim() == "0")
}

#[cfg(not(target_os = "linux"))]
fn capture_node(_: &nokhwa::utils::CameraInfo) -> bool {
    true
}

fn choose_camera<'a>(
    cameras: &'a [nokhwa::utils::CameraInfo],
    selected: Option<&str>,
) -> Option<&'a nokhwa::utils::CameraInfo> {
    match selected {
        Some(name) => cameras.iter().find(|camera| camera.human_name() == name),
        None => cameras.first(),
    }
}

/// Camera names, in the system's order.
pub fn list() -> Vec<String> {
    cameras().into_iter().map(|i| i.human_name()).collect()
}

/// The video source to publish the camera through.
pub fn video_source(limit: [u32; 2]) -> NativeVideoSource {
    let (width, height) = super::screen::fit_size(PRESET.0, PRESET.1, limit);
    NativeVideoSource::new(VideoResolution { width, height }, false)
}

/// A running capture; dropping it closes the camera and waits (bounded)
/// until it's released.
pub struct Capture {
    _worker: super::worker::Worker,
}

impl Capture {
    pub fn request_stop(&self) {
        super::video::stop(super::video::LOCAL_CAMERA);
        self._worker.request_stop();
    }

    pub fn stop_requested(&self) -> bool {
        self._worker.stop_requested()
    }
}

/// Expands RGB to RGBA (libyuv's "ABGR" byte order) for the I420 conversion.
fn rgb_to_rgba(rgb: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(rgb.len() / 3 * 4);
    for &[r, g, b] in rgb.as_chunks::<3>().0 {
        out.extend_from_slice(&[r, g, b, 255]);
    }
}

/// Opens `device` (by name; None = the first camera) and streams it into
/// `sink` (LiveKit's video source, or a transport's own encoder) until
/// dropped. `on_end` runs if the camera can't be used.
pub fn start(
    sink: impl Into<super::video::Sink>,
    device: Option<String>,
    limit: [u32; 2],
    on_end: impl Fn(String) + Send + 'static,
    repaint: Arc<dyn Fn() + Send + Sync>,
) -> std::io::Result<Capture> {
    let preview = super::video::PreviewOwner::new(super::video::LOCAL_CAMERA);
    let mut sink = sink.into();
    let worker = super::worker::Worker::spawn("camera", move |flag| {
        let cameras = cameras();
        let info = choose_camera(&cameras, device.as_deref());
        let Some(index) = info.map(|i| i.index().clone()) else {
            on_end(
                if device.is_some() {
                    "The selected camera is not available."
                } else {
                    "No camera found."
                }
                .into(),
            );
            return;
        };
        let (w, h, fps, _) = PRESET;
        let want = CameraFormat::new(Resolution::new(w, h), FrameFormat::MJPEG, fps);
        let mut camera = match Camera::new(
            index.clone(),
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(want)),
        )
        .or_else(|_| {
            Camera::new(
                index,
                RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate),
            )
        }) {
            Ok(c) => c,
            Err(e) => {
                on_end(format!("Couldn't open the camera: {e}"));
                return;
            }
        };
        if let Err(e) = camera.open_stream() {
            on_end(format!("Couldn't start the camera: {e}"));
            return;
        }
        let started = Instant::now();
        let (mut preview_at, mut preview_serial) = (started - std::time::Duration::from_secs(1), 0u64);
        let (mut rgb, mut rgba) = (Vec::new(), Vec::new());
        let mut last_frame = Instant::now();
        while !flag.stopped() {
            let Ok(frame) = camera.frame() else {
                // A camera that stops delivering ends the share instead of spinning.
                if last_frame.elapsed() > std::time::Duration::from_secs(5) {
                    on_end("The camera stopped sending video.".into());
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
                continue;
            };
            last_frame = Instant::now();
            let res = frame.resolution();
            let (fw, fh) = (res.width() & !1, res.height() & !1);
            rgb.resize((res.width() * res.height() * 3) as usize, 0);
            if frame.decode_image_to_buffer::<RgbFormat>(&mut rgb).is_err() || fw < 2 || fh < 2 {
                continue;
            }
            rgb_to_rgba(&rgb, &mut rgba);
            let mut i420 = I420Buffer::new(fw, fh);
            let (sy, su, sv) = i420.strides();
            let (y, u, v) = i420.data_mut();
            yuv_helper::abgr_to_i420(&rgba, res.width() * 4, y, sy, u, su, v, sv, fw as i32, fh as i32);
            // Larger cameras are scaled to the preset, keeping the aspect ratio.
            let preset_size = super::screen::fit_size(fw, fh, [w, h]);
            let (tw, th) = super::screen::fit_size(preset_size.0, preset_size.1, limit);
            let mut buffer = if (tw, th) == (fw, fh) {
                i420
            } else {
                i420.scale(tw as i32, th as i32)
            };
            // Turned off while this frame was read: it must not go out.
            if flag.stopped() {
                break;
            }
            let ts = started.elapsed().as_micros() as i64;
            let previous_serial = preview_serial;
            super::video::preview(&preview, &mut buffer, &mut preview_at, &mut preview_serial);
            if previous_serial != preview_serial {
                repaint();
            }
            sink.deliver(buffer, ts);
        }
        let _ = camera.stop_stream();
    })?;
    Ok(Capture { _worker: worker })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unavailable_camera_does_not_silently_select_another_device() {
        use nokhwa::utils::{CameraIndex, CameraInfo};
        let cameras = [
            CameraInfo::new("First", "", "", CameraIndex::Index(0)),
            CameraInfo::new("Second", "", "", CameraIndex::Index(1)),
        ];
        assert_eq!(choose_camera(&cameras, None).unwrap().human_name(), "First");
        assert_eq!(choose_camera(&cameras, Some("Second")).unwrap().human_name(), "Second");
        assert!(choose_camera(&cameras, Some("Unplugged")).is_none());
    }

    #[test]
    fn rgb_expands_to_rgba() {
        let mut out = Vec::new();
        rgb_to_rgba(&[1, 2, 3, 4, 5, 6], &mut out);
        assert_eq!(out, [1, 2, 3, 255, 4, 5, 6, 255]);
    }
}
