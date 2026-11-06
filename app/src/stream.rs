// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Large animations play the way browsers play them: the compressed file is
//! kept, and a worker decodes each frame just before it's shown, at full
//! drawn size, two frames ahead. Decoding every frame up front had to shrink
//! long GIFs to fit a byte cap, which looked soft next to the official client.
//! The worker runs only while its animation is near the view (anim.rs).

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use image::imageops::FilterType;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::images::{Fit, delay_ms, open_frames, shape, target};

type Pixels = SharedPixelBuffer<Rgba8Pixel>;

/// Frames decoded ahead of the one on screen.
const AHEAD: usize = 2;

/// An animation to stream: its file and how it's drawn.
pub struct Source {
    /// The compressed file, while held. With `reload`, it's let go once no
    /// player shows the animation, and read back from the disk cache (the
    /// same bytes, so the same quality) when one starts again.
    bytes: std::sync::Mutex<Option<Arc<[u8]>>>,
    size: usize,
    pub fit: Fit,
    pub webm: bool,
    reload: Option<(Arc<godwit_cache::EncryptedCache>, Arc<str>)>,
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source")
            .field("size", &self.size)
            .field("fit", &self.fit)
            .finish_non_exhaustive()
    }
}

impl Source {
    pub fn new(bytes: Arc<[u8]>, fit: Fit, webm: bool) -> Self {
        Self {
            size: bytes.len(),
            bytes: std::sync::Mutex::new(Some(bytes)),
            fit,
            webm,
            reload: None,
        }
    }

    /// Lets the file go while unused: `url` reads it back from `disk`.
    pub fn reloadable(&mut self, disk: Arc<godwit_cache::EncryptedCache>, url: Arc<str>) {
        self.reload = Some((disk, url));
    }

    /// Bytes it holds in the cache (the compressed file), at most.
    pub fn size(&self) -> usize {
        self.size
    }

    /// The file: held, or read back from the disk cache (on a worker).
    fn bytes(&self) -> Option<Arc<[u8]>> {
        let mut held = self.bytes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(bytes) = held.as_ref() {
            return Some(bytes.clone());
        }
        let (disk, url) = self.reload.as_ref()?;
        let bytes: Arc<[u8]> = disk.get_bytes(url).ok().flatten()?.into();
        *held = Some(bytes.clone());
        Some(bytes)
    }

    /// No player shows it now: let the file go if it can be read back.
    /// Running players keep their own reference, so nothing they use goes.
    pub fn release(&self) {
        if self.reload.is_some() {
            *self.bytes.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
    }
}

/// A running worker; dropping it stops it (its next send fails).
pub struct Player {
    frames: Receiver<(Pixels, u32)>,
    source: Arc<Source>,
}

impl Drop for Player {
    fn drop(&mut self) {
        self.source.release();
    }
}

impl Player {
    pub fn start(source: &Arc<Source>) -> Option<Self> {
        let (sender, frames) = mpsc::sync_channel(AHEAD);
        let held = source.clone();
        let source = source.clone();
        let spawned = std::thread::Builder::new().name("anim-stream".into()).spawn(move || {
            let Some(mut decoder) = Decoder::open(&source) else {
                return;
            };
            while let Some(frame) = decoder.next() {
                if sender.send(frame).is_err() {
                    return;
                }
            }
        });
        match spawned {
            Ok(_) => Some(Self { frames, source: held }),
            Err(e) => {
                eprintln!("error: starting an animation thread: {e}");
                None
            }
        }
    }

    /// The next frame and how long it shows, if it's decoded yet.
    pub fn next(&self) -> Option<(Image, u32)> {
        self.frames
            .try_recv()
            .ok()
            .map(|(pixels, delay)| (Image::from_rgba8(pixels), delay))
    }
}

/// Decodes frames one by one, looping.
enum Decoder {
    Image {
        bytes: Arc<[u8]>,
        fit: Fit,
        frames: Option<image::Frames<'static>>,
    },
    Webm {
        stream: godwit_media::Stream,
        fit: Fit,
    },
}

impl Decoder {
    fn open(source: &Arc<Source>) -> Option<Self> {
        let bytes = source.bytes()?;
        if source.webm {
            let (w, h) = webm_box(source.fit);
            let stream = godwit_media::Stream::new(bytes, w, h)?;
            return Some(Self::Webm {
                stream,
                fit: source.fit,
            });
        }
        Some(Self::Image {
            bytes,
            fit: source.fit,
            frames: None,
        })
    }

    fn next(&mut self) -> Option<(Pixels, u32)> {
        match self {
            Self::Webm { stream, fit } => {
                let (rgba, delay) = stream.next_frame()?;
                let (w, h) = stream.size();
                Some((fit_rgba(rgba, w, h, *fit)?, delay))
            }
            Self::Image { bytes, fit, frames } => {
                // At the end, start over; a file that yields nothing twice ends.
                for _ in 0..2 {
                    if frames.is_none() {
                        *frames = open_frames(bytes.clone());
                    }
                    match frames.as_mut()?.next() {
                        Some(Ok(frame)) => {
                            let delay = delay_ms(frame.delay());
                            let img = frame.into_buffer();
                            let (w, h) = img.dimensions();
                            return Some((fit_rgba(img.into_raw(), w, h, *fit)?, delay));
                        }
                        _ => *frames = None,
                    }
                }
                None
            }
        }
    }
}

/// The box a WebM is decoded within: square fits are cropped afterwards, so
/// they need a little more.
pub fn webm_box(fit: Fit) -> (u32, u32) {
    match fit {
        Fit::Square(s) | Fit::Circle(s) => (s * 2, s * 2),
        Fit::Within(w, h) | Fit::RoundedWithin(w, h, _) => (w, h),
    }
}

/// One frame fitted and shaped as `fit`, at full drawn size.
pub fn fit_rgba(rgba: Vec<u8>, w: u32, h: u32, fit: Fit) -> Option<Pixels> {
    let (tw, th) = target(fit, w, h);
    let mut out = if (tw, th) == (w, h) && !matches!(fit, Fit::Square(_) | Fit::Circle(_)) {
        rgba
    } else {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(w, h, rgba)?);
        let img = match fit {
            Fit::Square(_) | Fit::Circle(_) => img.resize_to_fill(tw, th, FilterType::Triangle),
            _ => img.resize_exact(tw, th, FilterType::Triangle),
        };
        img.to_rgba8().into_raw()
    };
    shape(fit, tw, th, &mut out);
    Some(SharedPixelBuffer::clone_from_slice(&out, tw, th))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_fit_their_box_and_keep_coming_in_a_loop() {
        let bytes: Arc<[u8]> = crate::images::test_gif(3).into();
        let source = Arc::new(Source::new(bytes, Fit::RoundedWithin(8, 8, 2), false));
        let mut decoder = Decoder::open(&source).unwrap();
        let first = decoder.next().unwrap();
        for _ in 0..5 {
            let (pixels, delay) = decoder.next().unwrap();
            assert!(pixels.width() <= 8 && pixels.height() <= 8);
            assert!(delay >= 20);
        }
        assert_eq!(first.0.width(), decoder.next().unwrap().0.width());
    }

    #[test]
    fn an_off_screen_animation_lets_its_file_go_and_reads_the_same_back() {
        let dir = std::env::temp_dir().join(format!("godwit-stream-test-{}", std::process::id()));
        let disk = Arc::new(godwit_cache::EncryptedCache::new(&dir, &[9; 32]).unwrap());
        let gif = crate::images::test_gif(3);
        disk.set_bytes("https://example.invalid/a.gif", &gif).unwrap();
        let mut source = Source::new(gif.clone().into(), Fit::Square(4), false);
        source.reloadable(disk, "https://example.invalid/a.gif".into());
        let source = Arc::new(source);
        let player = Player::start(&source).unwrap();
        drop(player);
        assert!(
            source.bytes.lock().unwrap().is_none(),
            "released once no player shows it"
        );
        assert_eq!(&*source.bytes().unwrap(), &gif[..], "the very same bytes come back");
        // Without a disk copy, the file is kept.
        let kept = Source::new(gif.into(), Fit::Square(4), false);
        kept.release();
        assert!(kept.bytes.lock().unwrap().is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_player_hands_over_frames_and_stops_when_dropped() {
        let bytes: Arc<[u8]> = crate::images::test_gif(3).into();
        let source = Arc::new(Source::new(bytes, Fit::Square(4), false));
        let player = Player::start(&source).unwrap();
        let start = std::time::Instant::now();
        let frame = loop {
            if let Some(f) = player.next() {
                break f;
            }
            assert!(start.elapsed().as_secs() < 5, "a frame within 5 s");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(frame.0.size().width, 4);
        drop(player);
    }
}
