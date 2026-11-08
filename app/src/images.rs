// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Pictures: avatars, icons, emoji and attachment previews. Fetched on several
//! worker threads, decoded straight to the size they're drawn at, and kept
//! in a cache with a fixed byte budget. Only what the UI asks for is
//! fetched (see docs/design.md, "Light").

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock, mpsc};
use std::time::Duration;

use godwit_cache::EncryptedCache;
use godwit_core::account::MediaFetch;
use godwit_core::backend::Waker;
use image::imageops::FilterType;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::anim::Animator;

/// How a picture is decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fit {
    /// Cropped to fill a square (emoji).
    Square(u32),
    /// Cropped to fill a square, then cut to a circle (avatars, icons). Slint
    /// can't clip a picture to a rounded shape, so the picture is the shape.
    Circle(u32),
    /// Scaled down to fit inside, keeping its shape (banners).
    Within(u32, u32),
    /// As `Within`, with corners rounded by the last value (attachment previews).
    RoundedWithin(u32, u32, u32),
}

/// Avatars and icons: covers a 36–42 px circle on a 2× screen.
pub const AVATAR: Fit = Fit::Circle(84);
/// The 14 px picture on a reply line.
pub const REPLY_AVATAR: Fit = Fit::Circle(32);
/// The image viewer's largest picture (Yapfast's `VIEW_WIDTH`/`VIEW_HEIGHT`):
/// sharp on most screens, at most 8 MiB decoded.
/// The image viewer: up to 4K, so zooming stays sharp (one picture at a
/// time: at most 33 MB decoded).
pub const VIEWER: Fit = Fit::Within(3840, 2160);
/// Large emoji (messages that are only emoji): 96 px on a 2× screen.
pub const JUMBO: Fit = Fit::Square(192);
/// Emoji in reactions.
pub const EMOJI: Fit = Fit::Square(40);
/// Emoji in the picker: still, so hundreds of animated server emoji don't
/// rebuild the picker every frame.
pub const PICKER_EMOJI: Fit = Fit::Square(44);
/// Profile banners: a 340 × 96 card, cropped to fill.
pub const BANNER: Fit = Fit::Within(680, 192);
/// A server's banner above its channels: the panel is up to ~360 px wide.
pub const SIDEBAR_BANNER: Fit = Fit::Within(720, 272);
/// Banners in the server and profile editors: sharp at their full width on a
/// 2× screen (the low-res `BANNER` looked soft there).
pub const EDIT_BANNER: Fit = Fit::Within(1360, 480);
/// A link preview's small picture beside its text (the official 80 px).
pub const THUMBNAIL: Fit = Fit::RoundedWithin(80, 80, 8);
/// A link preview's site icon (16 px on screen).
pub const EMBED_ICON: Fit = Fit::Square(32);
/// An activity invite's art: Steam's 184 × 69 game capsule, as its card.
pub const CAPSULE: Fit = Fit::RoundedWithin(184, 69, 4);
/// Attachment previews, at most this big on screen, with Godwit's 12 px corners.
pub const PREVIEW: Fit = Fit::RoundedWithin(400, 300, 12);

/// Decoded pixels kept at most (RGBA bytes). Generous, for usability: a big
/// server's avatars, icons and member pictures must all fit, or pictures on
/// screen get thrown out and vanish. Pictures used in the last two window
/// refreshes are never thrown out.
const BUDGET: usize = 64 * 1024 * 1024;
/// Animations' frames have a budget of their own, so a few GIFs can't push
/// still pictures on screen out of the cache.
const ANIMATION_BUDGET: usize = 32 * 1024 * 1024;
/// After this much leaves the cache, freed memory goes back to the system
/// (at most every `SETTLE_EVERY`).
const SETTLE_AFTER: usize = 1024 * 1024;
const SETTLE_EVERY: Duration = Duration::from_secs(5);
/// Larger downloads are refused.
const MAX_BYTES: u64 = 20 * 1024 * 1024;
const MAX_REDIRECTS: usize = 3;
/// A failed picture is tried again after this long.
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// Download threads (most pictures are small avatars and emoji, waiting on
/// the network more than the CPU: a screen of the emoji picker is ~100).
const WORKERS: usize = 8;

type Key = (Arc<str>, Fit);
type Pixels = SharedPixelBuffer<Rgba8Pixel>;
/// Decoded on a worker straight into Slint's pixel buffer, so the UI thread
/// takes the pixels without copying them (as Yapfast's loader), with how an
/// animated one plays.
type Decoded = (Key, Option<(Pixels, Moving)>);

/// How a picture moves, as a worker hands it over.
enum Moving {
    Still,
    /// Every frame (pixels, delay in ms): small animations, such as emoji.
    Frames(Vec<(Pixels, u32)>),
    /// Decoded while shown (stream.rs): larger animations.
    Stream(Arc<crate::stream::Source>),
}

/// How a cached animation plays (anim.rs).
pub enum Animation {
    /// Its frames and delays (ms), and their total length.
    Frames(Vec<(Image, u32)>, u32),
    Stream(Arc<crate::stream::Source>),
}

/// Animations whose frames, all decoded at full drawn size, fit in this
/// many bytes keep them all; larger ones stream (stream.rs), so they're
/// never shrunk to fit.
const PRELOAD: usize = 4 * 1024 * 1024;
/// The fastest an animation plays: browsers treat shorter GIF delays
/// (0–10 ms) as 100 ms, and anything under 20 ms as unintended.
const MIN_DELAY_MS: u32 = 20;

/// An animation's name in the animator: its address and size.
fn anim_key(key: &Key) -> String {
    format!("{:?}|{}", key.1, key.0)
}

/// Which pictures animate: emoji and media, as officially. Avatars, icons
/// and banners stay still (the official client animates them only on hover).
fn animates(fit: Fit) -> bool {
    // The viewer too: a GIF opened large moves there (big ones stream).
    matches!(fit, Fit::Square(40 | 192) | Fit::RoundedWithin(..)) || fit == VIEWER
}

/// Pictures waiting to be fetched (with how many times they failed), newest
/// last: workers take the newest, so what just came on screen loads before
/// an older backlog.
type Queue = Arc<(Mutex<Waiting>, Condvar)>;

#[derive(Default)]
struct Waiting {
    items: Vec<(Key, u32)>,
    /// Workers loading a profile picture now.
    profiles_busy: usize,
}

/// Profile pictures (avatars, icons, banners) wait behind chat content
/// (attachments, embeds, emoji, GIFs): a large server's member list asks for
/// hundreds, which kept the messages' own pictures waiting.
fn is_profile(fit: Fit) -> bool {
    matches!(fit, Fit::Circle(_) | Fit::Within(..))
}

/// At most this many workers load profile pictures at once, so one is
/// always free for chat content arriving behind a backlog of them.
const PROFILE_WORKERS: usize = WORKERS - 1;

impl Waiting {
    /// The next picture a free worker takes: the newest chat content, else
    /// the newest profile picture while too few workers are on those.
    fn take(&mut self) -> Option<(Key, u32)> {
        let at = match self.items.iter().rposition(|(key, _)| !is_profile(key.1)) {
            Some(at) => at,
            None if self.profiles_busy < PROFILE_WORKERS && !self.items.is_empty() => self.items.len() - 1,
            None => return None,
        };
        let item = self.items.remove(at);
        if is_profile(item.0.1) {
            self.profiles_busy += 1;
        }
        Some(item)
    }
}

/// Downloaded pictures kept on disk, encrypted (pictures from private chats
/// are private): a picture seen before shows without the network, as a
/// browser's cache does for the official client. Generous: disk is cheap,
/// asking the server again for what we have isn't. Least recently used go
/// first.
const DISK_BUDGET: u64 = 1024 * 1024 * 1024;
/// Pictures written to disk between trims, so a long session stays near
/// the budget too (besides the trim at start).
const PRUNE_EVERY: usize = 500;
/// A failed download is tried again after these waits (or longer when the
/// server says so), then given up until `RETRY_AFTER`.
const BACKOFF: [Duration; 3] = [Duration::from_secs(2), Duration::from_secs(8), Duration::from_secs(30)];

/// Why a download failed.
enum Failure {
    /// Worth trying again (network trouble, 429, 5xx), at least after this.
    Again(Option<Duration>),
    /// Won't change (404, too big, not a picture).
    Final,
}

/// Failed downloads waiting to go back in the queue.
#[derive(Clone)]
struct Retries(mpsc::Sender<(std::time::Instant, Key, u32)>);

impl Retries {
    /// A thread that puts each back in `queue` when its wait is over.
    fn start(queue: Queue) -> Self {
        let (send, waiting) = mpsc::channel::<(std::time::Instant, Key, u32)>();
        let _ = std::thread::Builder::new().name("images-retry".into()).spawn(move || {
            let mut due: Vec<(std::time::Instant, Key, u32)> = Vec::new();
            loop {
                let now = std::time::Instant::now();
                let (ready, later): (Vec<_>, Vec<_>) = due.drain(..).partition(|d| d.0 <= now);
                due = later;
                if !ready.is_empty() {
                    let (list, wake) = &*queue;
                    list.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .items
                        .extend(ready.into_iter().map(|(_, key, attempt)| (key, attempt)));
                    wake.notify_all();
                }
                let next = due.iter().map(|d| d.0).min();
                let got = match next {
                    Some(at) => waiting.recv_timeout(at.saturating_duration_since(now)),
                    None => waiting.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
                };
                match got {
                    Ok(item) => due.push(item),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        });
        Self(send)
    }

    /// Schedules `key`'s next try after its `attempt`-th failure; false
    /// when it has had all its tries.
    fn schedule(&self, key: &Key, attempt: u32, hint: Option<Duration>) -> bool {
        let Some(wait) = BACKOFF.get(attempt as usize) else {
            return false;
        };
        let wait = hint.map_or(*wait, |h| h.max(*wait));
        self.0
            .send((
                godwit_core::backend::checked_deadline(wait).into_std(),
                key.clone(),
                attempt + 1,
            ))
            .is_ok()
    }
}

/// What a download thread works with.
struct Worker {
    queue: Queue,
    retries: Retries,
    agent: ureq::Agent,
    media: Arc<RwLock<Vec<(String, MediaFetch)>>>,
    disk: Option<Arc<EncryptedCache>>,
    /// Pictures written to disk, shared by the workers (see `PRUNE_EVERY`).
    written: Arc<std::sync::atomic::AtomicUsize>,
}

impl Worker {
    /// The next picture to load, waiting for one.
    fn next(&self) -> (Key, u32) {
        let (list, ready) = &*self.queue;
        let mut list = list.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(item) = list.take() {
                return item;
            }
            list = ready.wait(list).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// `key` is done (loaded, failed or waiting to retry): a profile
    /// picture's place frees up for the next.
    fn finished(&self, key: &Key) {
        if is_profile(key.1) {
            let (list, ready) = &*self.queue;
            let mut list = list.lock().unwrap_or_else(PoisonError::into_inner);
            list.profiles_busy = list.profiles_busy.saturating_sub(1);
            ready.notify_one();
        }
    }

    /// Loads and decodes `key`: Some(result) to hand over (None inside for
    /// a picture that won't load), or None when it's tried again later.
    fn load(&self, key: &Key, attempt: u32) -> Option<Option<(Pixels, Moving)>> {
        let url = &key.0;
        let fetcher = self
            .media
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(scheme, _)| url.starts_with(scheme.as_str()))
            .map(|(_, f)| f.clone());
        let (bytes, from_disk) = if let Some(f) = fetcher {
            (f(url).filter(|b| b.len() as u64 <= MAX_BYTES), false)
        } else if let Some((disk, bytes)) = self
            .disk
            .as_ref()
            .and_then(|d| Some((d, d.get_bytes(url).ok().flatten()?)))
        {
            let _ = disk.touch(url);
            (Some(bytes), true)
        } else {
            match fetch(&self.agent, url) {
                Ok(bytes) => (Some(bytes), false),
                Err(Failure::Again(hint)) if self.retries.schedule(key, attempt, hint) => return None,
                Err(_) => (None, false),
            }
        };
        let Some(bytes) = bytes else {
            return Some(None);
        };
        let mut pixels = decode_any(bytes.as_slice(), key);
        if let Some(disk) = &self.disk {
            let web = url.starts_with("https://") || url.starts_with("http://");
            let mut on_disk = from_disk;
            if pixels.is_none() && from_disk {
                // Unreadable from disk: gone, so the next try downloads it.
                let _ = disk.remove(url);
            } else if pixels.is_some() && web && !from_disk && disk.set_bytes(url, &bytes).is_ok() {
                on_disk = true;
                let written = self.written.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if written.is_multiple_of(PRUNE_EVERY) {
                    let _ = disk.prune(DISK_BUDGET);
                }
            }
            // A streamed animation on disk needn't keep its file in memory
            // while off screen: it reads the same bytes back to play again.
            if on_disk
                && let Some((_, Moving::Stream(source))) = pixels.as_mut()
                && let Some(source) = Arc::get_mut(source)
            {
                source.reloadable(disk.clone(), url.clone());
            }
        }
        Some(pixels)
    }
}

/// Decodes downloaded bytes as `key` says: GIF services' WebM (played like a
/// GIF, without a browser helper), animations, or a still.
fn decode_any(bytes: &[u8], key: &Key) -> Option<(Pixels, Moving)> {
    if godwit_media::is_webm(bytes) {
        return decode_webm(bytes.into(), key.1);
    }
    if animates(key.1)
        && let Some(animated) = decode_animated(bytes, key.1)
    {
        return Some(animated);
    }
    let Some((w, h, rgba)) = decode(bytes, key.1) else {
        // An emoji's public asset id identifies failures without
        // printing credentials or signed URLs from other media.
        let emoji = key
            .0
            .split_once("/emojis/")
            .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or_default())
            .filter(|id| id.len() == 26 && id.bytes().all(|b| b.is_ascii_alphanumeric()));
        godwit_core::verbose!(
            "error: decoding image: custom_emoji={} format={:?} bytes={} fit={:?}",
            emoji.unwrap_or("none"),
            image::guess_format(bytes).ok(),
            bytes.len(),
            key.1,
        );
        return None;
    };
    Some((
        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba, w, h),
        Moving::Still,
    ))
}

struct Entry {
    image: Image,
    size: usize,
    /// The window refresh it was last used in.
    used: u64,
    /// How it moves, if it's an animation.
    frames: Option<Animation>,
}

pub struct Images {
    media: Arc<RwLock<Vec<(String, MediaFetch)>>>,
    cache: HashMap<Key, Entry>,
    /// Bytes held by still pictures, and by animations.
    used: usize,
    used_animated: usize,
    /// Counts window refreshes (see `next_frame`).
    frame: u64,
    pending: HashSet<Key>,
    /// Downloads that failed, and when: tried again after `RETRY_AFTER`
    /// (a timeout while many load mustn't hide a picture for good).
    failed: HashMap<Key, std::time::Instant>,
    /// None for the offline loader.
    queue: Option<Queue>,
    results: mpsc::Receiver<Decoded>,
    /// Bytes dropped from the cache since memory was last given back.
    evicted: usize,
    settled: std::time::Instant,
    /// Plays animations (see anim.rs); None for the preview's loader.
    animator: Option<Rc<RefCell<Animator>>>,
    /// The pictures kept on disk (None in demo mode and offline).
    disk: Option<Arc<EncryptedCache>>,
}

impl Images {
    /// `media`: networks' own address schemes and how to fetch them (see
    /// `NetworkAccount::media`); everything else is fetched over the web.
    pub fn new(wake: Waker, media: Vec<(String, MediaFetch)>, disk: Option<EncryptedCache>) -> Self {
        let media = Arc::new(RwLock::new(media));
        let queue: Queue = Arc::default();
        let (done, results) = mpsc::channel();
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            // A configured HTTP(S)_PROXY is kept (ureq's default): direct
            // connections are filtered in `PublicResolver`, see there.
            // Set per request (see `fetch`): Steam's servers get none.
            .user_agent(ureq::config::AutoHeaderValue::None)
            .max_redirects(0)
            // Statuses are read here: a 429's Retry-After decides the wait.
            .http_status_as_error(false)
            .build();
        let agent = godwit_core::public_address::PublicResolver::agent(config, |url| {
            godwit_core::hooks::image_headers(url).is_some()
        });
        let disk = disk.map(Arc::new);
        if let Some(disk) = disk.clone() {
            // Trimmed once per start, off the UI thread.
            let _ = std::thread::Builder::new().name("images-prune".into()).spawn(move || {
                if let Err(e) = disk.prune(DISK_BUDGET) {
                    eprintln!("error: trimming the picture cache: {e}");
                }
            });
        }
        let kept = disk.clone();
        let retries = Retries::start(queue.clone());
        let written = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for i in 0..WORKERS {
            let worker = Worker {
                queue: queue.clone(),
                retries: retries.clone(),
                agent: agent.clone(),
                media: media.clone(),
                disk: disk.clone(),
                written: written.clone(),
            };
            let (done, wake) = (done.clone(), wake.clone());
            let spawned = std::thread::Builder::new().name(format!("images-{i}")).spawn(move || {
                loop {
                    let (key, attempt) = worker.next();
                    let loaded = worker.load(&key, attempt);
                    worker.finished(&key);
                    let Some(pixels) = loaded else {
                        // Tried again later (Retries); still pending meanwhile.
                        continue;
                    };
                    if done.send((key, pixels)).is_err() {
                        return;
                    }
                    wake();
                }
            });
            if let Err(e) = spawned {
                eprintln!("error: starting an image thread: {e}");
            }
        }
        Self {
            cache: HashMap::new(),
            used: 0,
            used_animated: 0,
            frame: 0,
            pending: HashSet::new(),
            failed: HashMap::new(),
            queue: Some(queue),
            results,
            evicted: 0,
            settled: std::time::Instant::now(),
            animator: None,
            disk: kept,
            media,
        }
    }

    /// Accounts may be added after the loader starts. The workers share this
    /// registry; a fetch happens after releasing the registry lock.
    pub fn set_media(&mut self, media: Vec<(String, MediaFetch)>) {
        *self.media.write().unwrap_or_else(PoisonError::into_inner) = media;
    }

    /// Bytes the pictures kept on disk take.
    pub fn disk_size(&self) -> u64 {
        self.disk.as_ref().and_then(|d| d.size().ok()).unwrap_or(0)
    }

    /// Deletes the pictures kept on disk, off the UI thread.
    pub fn clear_disk(&self) {
        if let Some(disk) = self.disk.clone() {
            let _ = std::thread::Builder::new().name("images-clear".into()).spawn(move || {
                if let Err(e) = disk.clear() {
                    eprintln!("error: clearing the picture cache: {e}");
                }
            });
        }
    }

    /// A loader that never fetches anything (for the off-screen preview):
    /// every picture stays missing.
    pub fn offline() -> Self {
        let (_, results) = mpsc::channel();
        Self {
            cache: HashMap::new(),
            used: 0,
            used_animated: 0,
            frame: 0,
            pending: HashSet::new(),
            failed: HashMap::new(),
            queue: None,
            results,
            evicted: 0,
            settled: std::time::Instant::now(),
            animator: None,
            disk: None,
            media: Arc::default(),
        }
    }

    /// The picture at `url` decoded as `fit`, if it's ready; otherwise
    /// starts loading it.
    pub fn get(&mut self, url: &str, fit: Fit) -> Option<Image> {
        let key: Key = (url.into(), fit);
        if let Some(e) = self.cache.get_mut(&key) {
            e.used = self.frame;
            return Some(e.image.clone());
        }
        if !self.pending.contains(&key)
            && self.failed.get(&key).is_none_or(|at| at.elapsed() > RETRY_AFTER)
            && let Some((list, ready)) = self.queue.as_deref()
        {
            list.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .items
                .push((key.clone(), 0));
            ready.notify_one();
            self.pending.insert(key);
        }
        None
    }

    /// A new window refresh begins: pictures used from now on count as on
    /// screen (and aren't thrown out) until two refreshes later.
    pub fn next_frame(&mut self) {
        self.frame += 1;
    }

    /// Plays animated pictures through `animator` from now on.
    pub fn set_animator(&mut self, animator: Rc<RefCell<Animator>>) {
        self.animator = Some(animator);
    }

    /// As `get`, with the animation slot (anim.rs) when the picture moves:
    /// the UI then shows `Anim.frames[slot]`, which plays without the row
    /// being rebuilt. -1 for a still picture.
    pub fn get_animated(&mut self, url: &str, fit: Fit) -> Option<(Image, i32)> {
        let image = self.get(url, fit)?;
        let key: Key = (url.into(), fit);
        let slot = match (self.cache.get(&key).and_then(|e| e.frames.as_ref()), &self.animator) {
            (Some(animation), Some(animator)) => animator
                .try_borrow_mut()
                .ok()
                .and_then(|mut a| i32::try_from(a.slot(&anim_key(&key), animation, &image)).ok())
                .unwrap_or(-1),
            _ => -1,
        };
        Some((image, slot))
    }

    /// A picture bundled with the app (`png` bytes), decoded once and then
    /// kept like any other under `key`.
    pub fn bundled(&mut self, key: &str, fit: Fit, png: &[u8]) -> Option<Image> {
        let cache_key: Key = (key.into(), fit);
        if self.cache.contains_key(&cache_key) {
            return self.get(key, fit);
        }
        let (w, h, rgba) = decode(png, fit)?;
        let size = rgba.len();
        let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba, w, h);
        let img = Image::from_rgba8(buf);
        self.cache.insert(
            cache_key,
            Entry {
                image: img.clone(),
                size,
                used: self.frame,
                frames: None,
            },
        );
        self.used += size;
        Some(img)
    }

    /// Hands in a picture directly (RGBA, `w` × `h`), as if it had loaded:
    /// for pictures made locally, such as the preview's.
    pub fn put(&mut self, url: &str, fit: Fit, w: u32, h: u32, rgba: &[u8]) {
        let key: Key = (url.into(), fit);
        let mut rgba = rgba.to_vec();
        shape(fit, w, h, &mut rgba);
        let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba, w, h);
        self.used += rgba.len();
        self.cache.insert(
            key,
            Entry {
                image: Image::from_rgba8(buf),
                size: rgba.len(),
                used: self.frame,
                frames: None,
            },
        );
    }

    /// Hands in an animation directly (RGBA frames of `w` × `h` with their
    /// delays in ms), as if it had loaded: for the preview and benchmarks.
    pub fn put_animated(&mut self, url: &str, fit: Fit, w: u32, h: u32, frames: &[(Vec<u8>, u32)]) {
        let key: Key = (url.into(), fit);
        let frames: Vec<(Image, u32)> = frames
            .iter()
            .map(|(rgba, delay)| {
                let mut rgba = rgba.clone();
                shape(fit, w, h, &mut rgba);
                let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba, w, h);
                (Image::from_rgba8(buf), *delay)
            })
            .collect();
        let Some(first) = frames.first().map(|(image, _)| image.clone()) else {
            return;
        };
        let size = frames.len() * (w * h * 4) as usize;
        let total = frames.iter().map(|(_, d)| d).sum();
        self.used_animated += size;
        self.cache.insert(
            key,
            Entry {
                image: first,
                size,
                used: self.frame,
                frames: Some(Animation::Frames(frames, total)),
            },
        );
    }

    /// Takes in finished downloads. True if any picture became available.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((key, pixels)) = self.results.try_recv() {
            self.pending.remove(&key);
            match pixels {
                Some((buf, moving)) => {
                    let still = buf.as_bytes().len();
                    let (size, frames) = match moving {
                        Moving::Frames(frames) if frames.len() > 1 => {
                            let size = still + frames.iter().map(|(f, _)| f.as_bytes().len()).sum::<usize>();
                            let total = frames.iter().map(|(_, d)| d).sum::<u32>();
                            let frames = frames.into_iter().map(|(f, d)| (Image::from_rgba8(f), d)).collect();
                            (size, Some(Animation::Frames(frames, total)))
                        }
                        Moving::Stream(source) => (still + source.size(), Some(Animation::Stream(source))),
                        _ => (still, None),
                    };
                    if frames.is_some() {
                        self.used_animated += size;
                    } else {
                        self.used += size;
                    }
                    self.cache.insert(
                        key,
                        Entry {
                            image: Image::from_rgba8(buf),
                            size,
                            used: self.frame,
                            frames,
                        },
                    );
                    changed = true;
                }
                None => {
                    self.failed.insert(key, std::time::Instant::now());
                }
            }
        }
        if self.used > BUDGET {
            self.evict(false);
        }
        if self.used_animated > ANIMATION_BUDGET {
            self.evict(true);
        }
        if self.evicted > SETTLE_AFTER && self.settled.elapsed() > SETTLE_EVERY {
            self.evicted = 0;
            self.settled = std::time::Instant::now();
            crate::memory::settle();
        }
        changed
    }
}

impl Images {
    /// Throws out the least recently used pictures, down to 90% of the
    /// budget, but never one used in the last two refreshes (on screen).
    /// Stills and animations each have their own budget (`animated`).
    fn evict(&mut self, animated: bool) {
        let recent = self.frame.saturating_sub(1);
        let mut old: Vec<(u64, Key)> = {
            // Stable rows are not rebuilt on every refresh, so their last
            // cache access alone cannot tell whether an animation is visible.
            let animator = if animated {
                match self.animator.as_ref().map(|a| a.try_borrow()) {
                    Some(Ok(animator)) => Some(animator),
                    Some(Err(_)) => return,
                    None => None,
                }
            } else {
                None
            };
            self.cache
                .iter()
                .filter(|(key, entry)| {
                    entry.used < recent
                        && entry.frames.is_some() == animated
                        && !animator.as_ref().is_some_and(|a| a.is_live(&anim_key(key)))
                })
                .map(|(key, entry)| (entry.used, key.clone()))
                .collect()
        };
        old.sort_by_key(|(used, _)| *used);
        let budget = if animated { ANIMATION_BUDGET } else { BUDGET };
        for (_, key) in old {
            let used = if animated { self.used_animated } else { self.used };
            if used <= budget / 10 * 9 {
                break;
            }
            if let Some(e) = self.cache.remove(&key) {
                if animated
                    && let Some(a) = &self.animator
                    && let Ok(mut a) = a.try_borrow_mut()
                {
                    a.release(&anim_key(&key));
                }
                if animated {
                    self.used_animated -= e.size;
                } else {
                    self.used -= e.size;
                }
                self.evicted += e.size;
            }
        }
    }
}

/// A web file's bytes, fetched as pictures are (the same headers per host):
/// for saving the image the viewer shows. Blocking; call it off the UI thread.
pub(crate) fn download(url: &str) -> Option<Vec<u8>> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .user_agent(ureq::config::AutoHeaderValue::None)
        .max_redirects(0)
        .http_status_as_error(false)
        .build();
    let agent = godwit_core::public_address::PublicResolver::agent(config, |url| {
        godwit_core::hooks::image_headers(url).is_some()
    });
    fetch(&agent, url).ok()
}

fn fetch(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, Failure> {
    let mut url = match reqwest::Url::parse(url) {
        Ok(url) if matches!(url.scheme(), "https" | "http") => url,
        _ => return Err(Failure::Final),
    };
    for hop in 0..=MAX_REDIRECTS {
        let address = url.as_str();
        let request = agent.get(address);
        let request = if let Some(headers) = godwit_core::hooks::image_headers(address) {
            // The network's own pictures, fetched as its official client does.
            headers
                .into_iter()
                .fold(request, |request, (name, value)| request.header(name, value))
        } else {
            // Generic: no version, nothing about this machine.
            request.header("User-Agent", "Godwit")
        };
        let mut response = match request.call() {
            Ok(r) => r,
            Err(e) => {
                // Network trouble: worth another try.
                godwit_core::verbose!("error: fetching image: {e}");
                return Err(Failure::Again(None));
            }
        };
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            if !redirect_allowed(hop) {
                return Err(Failure::Final);
            }
            let Some(location) = response.headers().get("location").and_then(|v| v.to_str().ok()) else {
                return Err(Failure::Final);
            };
            url = url.join(location).map_err(|_| Failure::Final)?;
            if !matches!(url.scheme(), "https" | "http") {
                return Err(Failure::Final);
            }
            continue;
        }
        if !(200..300).contains(&status) {
            let hint = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            godwit_core::verbose!("error: fetching image: status {status}");
            return Err(if status == 429 || status >= 500 {
                Failure::Again(hint)
            } else {
                Failure::Final
            });
        }
        let mut bytes = Vec::new();
        return match response.body_mut().as_reader().take(MAX_BYTES).read_to_end(&mut bytes) {
            Ok(_) => Ok(bytes),
            Err(e) => {
                godwit_core::verbose!("error: reading image: {e}");
                Err(Failure::Again(None))
            }
        };
    }
    Err(Failure::Final)
}

fn redirect_allowed(redirects_followed: usize) -> bool {
    redirects_followed < MAX_REDIRECTS
}

/// The target size for a picture of `w`×`h`.
pub(crate) fn target(fit: Fit, w: u32, h: u32) -> (u32, u32) {
    match fit {
        Fit::Square(s) | Fit::Circle(s) => (s, s),
        Fit::Within(mw, mh) | Fit::RoundedWithin(mw, mh, _) => {
            let scale = (f64::from(mw) / f64::from(w.max(1)))
                .min(f64::from(mh) / f64::from(h.max(1)))
                .min(1.0);
            let dim = |d: u32| ((f64::from(d) * scale).round() as u32).max(1);
            (dim(w), dim(h))
        }
    }
}

/// Opens a frame iterator over an animated GIF, WebP or PNG (None for any
/// other picture). It owns the bytes, so a streaming worker can keep it.
pub(crate) fn open_frames(bytes: Arc<[u8]>) -> Option<image::Frames<'static>> {
    use image::AnimationDecoder;
    let format = image::guess_format(&bytes).ok()?;
    let cursor = std::io::Cursor::new(bytes);
    match format {
        image::ImageFormat::Gif => Some(image::codecs::gif::GifDecoder::new(cursor).ok()?.into_frames()),
        image::ImageFormat::WebP => {
            let decoder = image::codecs::webp::WebPDecoder::new(cursor).ok()?;
            decoder.has_animation().then(|| decoder.into_frames())
        }
        image::ImageFormat::Png => {
            let decoder = image::codecs::png::PngDecoder::new(cursor).ok()?;
            if !decoder.is_apng().unwrap_or(false) {
                return None;
            }
            Some(decoder.apng().ok()?.into_frames())
        }
        _ => None,
    }
}

/// A frame's delay as browsers play it: 0–10 ms count as 100 ms, and
/// nothing is faster than `MIN_DELAY_MS`.
pub(crate) fn delay_ms(delay: image::Delay) -> u32 {
    let (num, den) = delay.numer_denom_ms();
    let ms = num.checked_div(den).unwrap_or(0);
    if ms <= 10 { 100 } else { ms.max(MIN_DELAY_MS) }
}

/// An animated GIF, WebP or PNG at `fit` (None if it isn't one): its first
/// frame as the still, and every frame when they all fit `PRELOAD` at full
/// drawn size, else streamed. One pass that stops as soon as it's clear it
/// streams: counting frames first decoded a large GIF twice before it showed.
fn decode_animated(bytes: &[u8], fit: Fit) -> Option<(Pixels, Moving)> {
    let shared: Arc<[u8]> = bytes.into();
    let frames = open_frames(shared.clone())?;
    let mut out: Vec<(Pixels, u32)> = Vec::new();
    let mut size = 0;
    for frame in frames.take(1000).filter_map(Result::ok) {
        let delay = delay_ms(frame.delay());
        let img = frame.into_buffer();
        let (w, h) = img.dimensions();
        let Some(pixels) = crate::stream::fit_rgba(img.into_raw(), w, h, fit) else {
            continue;
        };
        size += pixels.as_bytes().len();
        out.push((pixels, delay));
        if size > PRELOAD {
            let still = out.swap_remove(0).0;
            let source = crate::stream::Source::new(shared, fit, false);
            return Some((still, Moving::Stream(Arc::new(source))));
        }
    }
    let still = out.first()?.0.clone();
    if out.len() < 2 {
        return Some((still, Moving::Still));
    }
    Some((still, Moving::Frames(out)))
}

#[cfg(test)]
/// Every frame of an animated GIF, WebP or PNG, at full drawn size, with its
/// delay; empty for still pictures.
fn decode_frames(bytes: Arc<[u8]>, fit: Fit) -> Vec<(Pixels, u32)> {
    let Some(frames) = open_frames(bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for frame in frames.take(1000).filter_map(Result::ok) {
        let delay = delay_ms(frame.delay());
        let img = frame.into_buffer();
        let (w, h) = img.dimensions();
        if let Some(pixels) = crate::stream::fit_rgba(img.into_raw(), w, h, fit) {
            out.push((pixels, delay));
        }
    }
    if out.len() < 2 {
        out.clear();
    }
    out
}

/// A WebM clip (godwit-media) fitted and shaped as `fit`: its first frame as
/// the still, and every frame when they fit `PRELOAD`, else streamed.
fn decode_webm(bytes: Arc<[u8]>, fit: Fit) -> Option<(Pixels, Moving)> {
    let (max_w, max_h) = crate::stream::webm_box(fit);
    let mut stream = godwit_media::Stream::new(bytes.clone(), max_w, max_h)?;
    let (w, h) = stream.size();
    let count = stream.frame_count();
    let (first, _) = stream.next_frame()?;
    let still = crate::stream::fit_rgba(first, w, h, fit)?;
    if !animates(fit) || count < 2 {
        return Some((still, Moving::Still));
    }
    let (tw, th) = (still.width() as usize, still.height() as usize);
    if count * tw * th * 4 > PRELOAD {
        let source = crate::stream::Source::new(bytes, fit, true);
        return Some((still, Moving::Stream(Arc::new(source))));
    }
    // Small: every frame now (the first again after a restart).
    let mut stream = godwit_media::Stream::new(bytes, max_w, max_h)?;
    let mut frames = Vec::with_capacity(count);
    for _ in 0..count {
        let Some((rgba, delay)) = stream.next_frame() else {
            break;
        };
        let (w, h) = stream.size();
        if let Some(pixels) = crate::stream::fit_rgba(rgba, w, h, fit) {
            frames.push((pixels, delay));
        }
    }
    Some((still, Moving::Frames(frames)))
}

fn decode(bytes: &[u8], fit: Fit) -> Option<(u32, u32, Vec<u8>)> {
    if is_svg(bytes) {
        return decode_svg(bytes, fit);
    }
    let img = image::load_from_memory(bytes).ok()?;
    let (w, h) = target(fit, img.width(), img.height());
    let out = match fit {
        Fit::Square(_) | Fit::Circle(_) => img.resize_to_fill(w, h, FilterType::Triangle),
        Fit::Within(..) | Fit::RoundedWithin(..) if (w, h) == (img.width(), img.height()) => img,
        Fit::Within(..) | Fit::RoundedWithin(..) => img.resize_exact(w, h, FilterType::Triangle),
    };
    let (w, h) = (out.width(), out.height());
    let mut rgba = out.to_rgba8().into_raw();
    shape(fit, w, h, &mut rgba);
    Some((w, h, rgba))
}

/// Cuts the picture to its fit's shape: a circle, or rounded corners. The
/// edge is anti-aliased over one pixel.
pub(crate) fn shape(fit: Fit, w: u32, h: u32, rgba: &mut [u8]) {
    let radius = match fit {
        Fit::Circle(_) => w.min(h) as f32 / 2.0,
        Fit::RoundedWithin(.., r) => (r as f32).min(w.min(h) as f32 / 2.0),
        Fit::Square(_) | Fit::Within(..) => return,
    };
    let (wf, hf) = (w as f32, h as f32);
    for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = ((i as u32 % w) as f32 + 0.5, (i as u32 / w) as f32 + 0.5);
        // Distance into the corner circle, if the pixel is in a corner.
        let cx = if x < radius {
            radius - x
        } else if x > wf - radius {
            x - (wf - radius)
        } else {
            0.0
        };
        let cy = if y < radius {
            radius - y
        } else if y > hf - radius {
            y - (hf - radius)
        } else {
            0.0
        };
        let outside = (cx * cx + cy * cy).sqrt() - radius;
        let coverage = (0.5 - outside).clamp(0.0, 1.0);
        px[3] = (f32::from(px[3]) * coverage).round() as u8;
    }
}

fn is_svg(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(256)]);
    let head = head.trim_start();
    head.starts_with("<svg") || (head.starts_with("<?xml") && head.contains("<svg"))
}

/// Renders an SVG (the official emoji packs are SVGs).
fn decode_svg(bytes: &[u8], fit: Fit) -> Option<(u32, u32, Vec<u8>)> {
    use resvg::{tiny_skia, usvg};
    // Downloaded SVGs may embed raster data, but must never resolve local
    // file paths. Inline data URLs keep the normal decoder.
    let options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_string: Box::new(|_, _| None),
            ..Default::default()
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(bytes, &options).ok()?;
    let size = tree.size();
    let (w, h) = target(fit, size.width().ceil() as u32, size.height().ceil() as u32);
    let scale = (w as f32 / size.width()).min(h as f32 / size.height());
    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia is premultiplied; Slint's RGBA buffers aren't.
    let mut rgba = pixmap.take();
    for [r, g, b, alpha] in rgba.as_chunks_mut::<4>().0 {
        let a = u32::from(*alpha);
        if a > 0 && a < 255 {
            for c in [r, g, b] {
                *c = u8::try_from((u32::from(*c) * 255 + a / 2) / a).unwrap_or(u8::MAX);
            }
        }
    }
    Some((w, h, rgba))
}

/// A two-frame 8 × 8 GIF, for other modules' tests.
#[cfg(test)]
pub(crate) fn test_gif(delay: u16) -> Vec<u8> {
    tests::gif(delay)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_stop_after_three_hops() {
        assert!(redirect_allowed(0));
        assert!(redirect_allowed(2));
        assert!(!redirect_allowed(MAX_REDIRECTS));
    }

    fn red_png() -> Vec<u8> {
        use image::ImageEncoder;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[255, 0, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        png
    }

    #[test]
    fn authenticated_fetchers_can_be_registered_after_workers_start() {
        let mut images = Images::offline();
        let workers = images.media.clone();
        let fake: MediaFetch = Arc::new(|_| Some(vec![1, 2, 3]));
        images.set_media(vec![("account-media://1/".into(), fake)]);
        let fetch = workers.read().unwrap()[0].1.clone();
        assert_eq!(fetch("account-media://1/file"), Some(vec![1, 2, 3]));
        images.set_media(Vec::new());
        assert!(workers.read().unwrap().is_empty());
    }

    #[test]
    fn svg_with_embedded_png_renders_visible_pixels() {
        let png = red_png();
        let encoded: String = png.iter().map(|byte| format!("%{byte:02X}")).collect();
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="40" height="40"><image width="40" height="40" xlink:href="data:image/png,{encoded}"/></svg>"#
        );
        let (width, height, pixels) = decode(svg.as_bytes(), EMOJI).unwrap();
        let center = ((height / 2 * width + width / 2) * 4) as usize;
        assert_eq!(
            &pixels[center..center + 4],
            &[255, 0, 0, 255],
            "embedded raster images must not silently become transparent"
        );
    }

    #[test]
    fn downloaded_svg_cannot_load_a_local_image() {
        let path = std::env::temp_dir().join(format!("godwit-svg-external-image-{}.png", std::process::id()));
        std::fs::write(&path, red_png()).unwrap();
        let escaped = path.to_string_lossy().replace('&', "&amp;").replace('"', "&quot;");
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="40" height="40"><image width="40" height="40" xlink:href="{escaped}"/></svg>"#
        );
        let result = decode(svg.as_bytes(), EMOJI);
        std::fs::remove_file(path).unwrap();
        let (_, _, pixels) = result.unwrap();
        assert!(pixels.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0));
    }

    /// A two-frame GIF, red then blue, `delay` hundredths of a second each.
    pub(crate) fn gif(delay: u16) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut out);
            enc.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
            for colour in [[255, 0, 0, 255], [0, 0, 255, 255]] {
                let buf = image::RgbaImage::from_pixel(8, 8, image::Rgba(colour));
                let frame =
                    image::Frame::from_parts(buf, 0, 0, image::Delay::from_numer_denom_ms(u32::from(delay) * 10, 1));
                enc.encode_frame(frame).unwrap();
            }
        }
        out
    }

    #[test]
    fn animated_gifs_decode_every_frame_with_browser_delays() {
        let frames = decode_frames(gif(5).into(), EMOJI);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].1, 50);
        assert_eq!((frames[0].0.width(), frames[0].0.height()), (40, 40));
        assert_eq!(&frames[1].0.as_bytes()[..4], &[0, 0, 255, 255]);
        // Zero delays play at 100 ms, as in browsers.
        assert!(decode_frames(gif(0).into(), EMOJI).iter().all(|(_, d)| *d == 100));
        assert!(
            decode_frames(gif(5).into(), AVATAR).len() == 2,
            "decoding itself doesn't care"
        );
        assert!(!animates(AVATAR) && animates(EMOJI) && animates(PREVIEW));
    }

    #[test]
    fn chat_content_loads_before_profile_pictures_and_keeps_a_worker() {
        let key = |url: &str, fit| ((Arc::<str>::from(url), fit), 0);
        let mut waiting = Waiting {
            profiles_busy: 0,
            items: std::iter::once(key("preview", PREVIEW))
                .chain((1..=PROFILE_WORKERS + 2).map(|i| key(&format!("avatar-{i}"), AVATAR)))
                .chain(std::iter::once(key("emoji", EMOJI)))
                .collect(),
        };
        let mut order = Vec::new();
        while let Some(((url, _), _)) = waiting.take() {
            order.push(url.to_string());
        }
        // Chat content first (newest first), then profile pictures until
        // all but one worker are on them.
        let mut expected = vec!["emoji".to_owned(), "preview".to_owned()];
        expected.extend((3..=PROFILE_WORKERS + 2).rev().map(|i| format!("avatar-{i}")));
        assert_eq!(order, expected);
        assert_eq!(waiting.profiles_busy, PROFILE_WORKERS);
        waiting.items.push(key("gif", PREVIEW));
        assert_eq!(
            waiting.take().map(|((url, _), _)| url.to_string()).as_deref(),
            Some("gif")
        );
    }

    #[test]
    fn small_animations_keep_every_frame_and_large_ones_stream_at_full_size() {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            for i in 0..120u8 {
                let pixels = image::RgbaImage::from_pixel(160, 120, image::Rgba([i, 0, 0, 255]));
                let delay = if i % 2 == 0 { 30 } else { 70 };
                encoder
                    .encode_frame(image::Frame::from_parts(
                        pixels,
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(delay, 1),
                    ))
                    .unwrap();
            }
        }
        // 120 frames of 160 × 120 are ~9 MB decoded: streamed, never shrunk.
        assert!(matches!(decode_animated(&bytes, PREVIEW), Some((_, Moving::Stream(_)))));
        // As emoji they're 40 × 40: ~0.8 MB, every frame kept with its delay.
        let Some((_, Moving::Frames(frames))) = decode_animated(&bytes, EMOJI) else {
            panic!("small animations keep their frames");
        };
        assert_eq!(frames.len(), 120);
        assert_eq!((frames[0].0.width(), frames[0].0.height()), (40, 40));
        assert_eq!(frames.iter().map(|(_, delay)| delay).sum::<u32>(), 6000);
        assert!(
            decode_animated(&image_png(), PREVIEW).is_none(),
            "a still PNG isn't animated"
        );
    }

    fn image_png() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn animated_pictures_get_a_slot_and_stills_dont() {
        let mut images = Images::offline();
        images.set_animator(Animator::new());
        let frames = decode_frames(gif(5).into(), EMOJI);
        let first = frames[0].0.clone();
        let total = frames.iter().map(|(_, d)| d).sum();
        images.cache.insert(
            ("anim".into(), EMOJI),
            Entry {
                image: Image::from_rgba8(first.clone()),
                size: 0,
                used: 0,
                frames: Some(Animation::Frames(
                    frames.into_iter().map(|(f, d)| (Image::from_rgba8(f), d)).collect(),
                    total,
                )),
            },
        );
        images.cache.insert(
            ("still".into(), EMOJI),
            Entry {
                image: Image::from_rgba8(first),
                size: 0,
                used: 0,
                frames: None,
            },
        );
        assert_eq!(images.get_animated("anim", EMOJI).map(|(_, s)| s), Some(0));
        assert_eq!(
            images.get_animated("anim", EMOJI).map(|(_, s)| s),
            Some(0),
            "same slot again"
        );
        assert_eq!(images.get_animated("still", EMOJI).map(|(_, s)| s), Some(-1));
    }

    #[test]
    fn live_animations_survive_cache_pressure_until_they_leave_view() {
        let mut images = Images::offline();
        let animator = Animator::new();
        images.set_animator(animator.clone());
        let frames = decode_frames(gif(5).into(), EMOJI);
        let total = frames.iter().map(|(_, d)| d).sum();
        let key = ("live".into(), EMOJI);
        images.cache.insert(
            key.clone(),
            Entry {
                image: Image::from_rgba8(frames[0].0.clone()),
                // Synthetic accounting exercises eviction without allocating 32 MiB.
                size: ANIMATION_BUDGET + 1,
                used: 0,
                frames: Some(Animation::Frames(
                    frames.into_iter().map(|(f, d)| (Image::from_rgba8(f), d)).collect(),
                    total,
                )),
            },
        );
        images.used_animated = ANIMATION_BUDGET + 1;
        let (_, slot) = images.get_animated("live", EMOJI).unwrap();
        animator.borrow_mut().set_live([slot as usize].into_iter().collect());
        for _ in 0..4 {
            images.next_frame();
        }
        images.poll();
        assert!(images.cache.contains_key(&key));
        animator.borrow_mut().set_live(Default::default());
        images.poll();
        assert!(!images.cache.contains_key(&key));
        assert_eq!(images.used_animated, 0);
    }

    #[test]
    fn pictures_on_screen_are_never_thrown_out() {
        let mut images = Images::offline();
        // 1 MiB each (512 × 512 RGBA).
        let px = vec![0u8; 512 * 512 * 4];
        // 40 MiB used two refreshes ago, then one picture used now.
        for i in 0..40 {
            images.put(&format!("old{i}"), Fit::Square(512), 512, 512, &px);
        }
        images.next_frame();
        images.next_frame();
        assert!(images.get("old0", Fit::Square(512)).is_some());
        // 40 MiB more, now: over the budget.
        for i in 0..40 {
            images.put(&format!("new{i}"), Fit::Square(512), 512, 512, &px);
        }
        images.poll();
        assert!(
            images.used <= BUDGET,
            "back under the budget: {} of {}",
            images.used,
            BUDGET
        );
        assert!(
            images.get("old0", Fit::Square(512)).is_some(),
            "used this refresh: kept"
        );
        assert!(images.get("new39", Fit::Square(512)).is_some(), "just loaded: kept");
        // Only old, unused ones went (down to 90% of the budget).
        let old_left = (1..40)
            .filter(|i| images.get(&format!("old{i}"), Fit::Square(512)).is_some())
            .count();
        let new_left = (0..40)
            .filter(|i| images.get(&format!("new{i}"), Fit::Square(512)).is_some())
            .count();
        assert!(old_left < 39, "old and unused: some thrown out");
        assert_eq!(new_left, 40, "none of the pictures in use");
    }

    #[test]
    fn previews_only_shrink_and_keep_their_shape() {
        assert_eq!(target(PREVIEW, 800, 300), (400, 150));
        assert_eq!(target(PREVIEW, 200, 100), (200, 100));
        assert_eq!(target(PREVIEW, 300, 900), (100, 300));
        assert_eq!(target(AVATAR, 10, 999), (84, 84));
    }

    #[test]
    fn avatars_are_cut_round_and_previews_get_rounded_corners() {
        let mut px = vec![255u8; 84 * 84 * 4];
        shape(AVATAR, 84, 84, &mut px);
        let alpha = |x: usize, y: usize| px[(y * 84 + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0, "corner is outside the circle");
        assert_eq!(alpha(42, 42), 255, "centre is inside");
        assert!(alpha(42, 0) > 250, "top middle is on the (anti-aliased) edge");
        let mut photo = vec![255u8; 400 * 225 * 4];
        shape(PREVIEW, 400, 225, &mut photo);
        assert_eq!(photo[3], 0, "rounded corner");
        assert_eq!(photo[(100 * 400 + 200) * 4 + 3], 255);
    }

    #[test]
    fn svgs_render_unpremultiplied() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="red" fill-opacity="0.5"/></svg>"#;
        let (w, h, px) = decode(svg, EMOJI).unwrap();
        assert_eq!((w, h), (40, 40));
        assert_eq!(&px[..4], &[255, 0, 0, 128]);
    }
}
