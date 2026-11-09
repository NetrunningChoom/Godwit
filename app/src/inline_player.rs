// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Inline web players run in small helper processes, as Godwit's did. Broadway
//! gives GTK and WebKit their own invisible software display; each helper
//! streams its frames to Slint, which draws them in the message and sends the
//! pointer back. WebKit won't start media in an offscreen GTK window, so a real
//! (but unseen) window is what makes this work.
//!
//! Players load once their message is on screen, as the official client's lazy
//! iframes and `<video preload="metadata">` do, at most [`MAX_PLAYERS`] at a
//! time; they stop when scrolled away.
//!
//! Wire format, helper → app on stdout: `F`, width and height (u32 LE), then
//! BGRA premultiplied pixels; `U`, the URL's length (u32 LE), four zero bytes,
//! then the URL (a link the page opened in a new window, for the browser); `X`
//! and eight zero bytes (the page asked for fullscreen). App → helper on stdin,
//! one command per line: `m x y` (move), `d x y` / `u x y` (left button down /
//! up), `s width height` (resize without reloading), `q` (quit).
//! Coordinates are the helper's pixels.

use std::io::{self, BufRead, Read, Write};
use std::process::{Child, ChildStderr, ChildStdin};
use std::sync::mpsc::{self, Receiver};

use std::rc::Rc;

use slint::{Image, Model, Rgba8Pixel, SharedPixelBuffer, VecModel};

use godwit_core::backend::Waker;

/// Players alive at once: each is a WebKit process (Godwit's limit).
pub const MAX_PLAYERS: usize = 3;
static MEDIA_SUMMARY_SENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The largest frame accepted from a helper: 1600 × 1000 pixels.
const MAX_FRAME_BYTES: usize = FULL_WIDTH as usize * FULL_HEIGHT as usize * 4;
/// Fullscreen frames' limit: a 1440p screen at its own pixels (1600 × 1000
/// was stretched to fill it, and looked low-res). Inline ones stay smaller.
const FULL_WIDTH: u32 = 2560;
const FULL_HEIGHT: u32 = 1440;
/// The longest link accepted from a helper.
const MAX_LINK_BYTES: usize = 4096;

/// What the page's player is (the official element it stands in for).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A provider's player URL in the official iframe.
    Embed,
    /// A video file, as the official `<video controls preload="metadata">`.
    Video,
    /// A GIF provider's video, as the official GIF `<video>`: muted, looping
    /// and playing, without controls.
    Gif,
}

impl Kind {
    pub fn parse(kind: &str) -> Option<Self> {
        match kind {
            "embed" => Some(Self::Embed),
            "video" => Some(Self::Video),
            "gif" => Some(Self::Gif),
            _ => None,
        }
    }

    fn arg(self) -> &'static str {
        match self {
            Self::Embed => "embed",
            Self::Video => "video",
            Self::Gif => "gif",
        }
    }
}

/// A player to load: its URL, kind and size on screen (logical pixels).
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub url: String,
    pub kind: Kind,
    pub width: u32,
    pub height: u32,
}

enum Msg {
    Frame(Frame),
    Link(String),
    Fullscreen,
}

struct Frame {
    width: u32,
    height: u32,
    /// Built on the reader thread, so the UI thread only wraps it.
    pixels: SharedPixelBuffer<Rgba8Pixel>,
}

struct RunningPlayer {
    spec: Spec,
    /// Inline size to restore after fullscreen, and the last received size.
    inline_size: (u32, u32),
    size: (u32, u32),
    child: Option<Child>,
    input: ChildStdin,
    messages: Receiver<Msg>,
    image: Option<Image>,
    /// Its place in the shared frame list (the `Players` global).
    slot: usize,
    /// When it was last asked for (a counter): the oldest goes first.
    used: u64,
    /// Clicked into: it may be playing, so scrolling doesn't stop it (only
    /// leaving the room, or making room for another).
    touched: bool,
}

impl Drop for RunningPlayer {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            stop_child(child);
        }
    }
}

/// Dropping std::process::Child neither stops nor reaps it. Stop the helper
/// immediately when its slot goes away; wait off the UI thread to avoid
/// leaving zombies (Broadway follows its helper via PR_SET_PDEATHSIG).
fn stop_child(mut child: Child) {
    let _ = child.kill();
    if let Err(error) = std::thread::Builder::new()
        .name("godwit-player-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        })
    {
        eprintln!("note: starting the player reaper: {error}");
    }
}

/// What the players asked for since the last poll.
#[derive(Default)]
pub struct Polled {
    /// A frame arrived or a player stopped: rows need rebuilding.
    pub changed: bool,
    /// Links to open in the browser.
    pub links: Vec<String>,
    /// Players whose page asked for fullscreen.
    pub fullscreen: Vec<Spec>,
}

#[derive(Default)]
pub struct InlinePlayers {
    players: Vec<RunningPlayer>,
    /// Each player's newest frame, by slot: the timeline shows
    /// `Players.frames[slot]`, so a new frame swaps one picture instead of
    /// rebuilding its row (which re-laid out the timeline on every frame).
    frames: Rc<VecModel<Image>>,
    clock: u64,
    /// URLs whose helper failed to start or crashed: not retried by
    /// themselves, so a broken page doesn't respawn on every scroll.
    failed: std::collections::HashSet<String>,
}

impl InlinePlayers {
    /// Loads `spec`'s player unless it's running. With `evict`, the least
    /// recently used player makes room; without, nothing starts when full.
    /// Ok(false): not started (full, or it failed before).
    pub fn start(&mut self, spec: &Spec, scale: f32, evict: bool, wake: Waker) -> io::Result<bool> {
        if !valid_player_url(&spec.url) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "unsupported player URL"));
        }
        self.clock += 1;
        if let Some(p) = self.players.iter_mut().find(|p| p.spec.url == spec.url) {
            p.used = self.clock;
            return Ok(true);
        }
        if self.failed.contains(&spec.url) && !evict {
            return Ok(false);
        }
        if self.players.len() >= MAX_PLAYERS {
            if !evict {
                return Ok(false);
            }
            if let Some(oldest) = self
                .players
                .iter()
                .enumerate()
                .min_by_key(|(_, p)| p.used)
                .map(|(i, _)| i)
            {
                self.players.remove(oldest);
            }
        }
        // Below 1 too (a smaller interface size): a bigger frame would be
        // shrunk to fit, softening the player's own controls.
        let scale = scale.clamp(0.5, 3.0);
        // Even sizes, within what a frame may be.
        let px = |v: u32, max: u32| (((v as f32 * scale) as u32).clamp(90, max)) & !1;
        let (width, height) = (px(spec.width, 1600), px(spec.height, 1000));
        let executable = player_executable()?;
        let mut command = std::process::Command::new(executable);
        command
            .args([
                spec.kind.arg(),
                &spec.url,
                &width.to_string(),
                &height.to_string(),
                &format!("{scale:.2}"),
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        if let Some(path) = crate::media_log::path() {
            command.env("GODWIT_MEDIA_LOG", path);
            command.env("GST_DEBUG", "2,decodebin:5,playbin:5,GST_CAPS:5,GST_ELEMENT_FACTORY:5");
            if !MEDIA_SUMMARY_SENT.swap(true, std::sync::atomic::Ordering::Relaxed) {
                command.env("GODWIT_MEDIA_SUMMARY", "1");
            }
            command.stderr(std::process::Stdio::piped());
        } else {
            command.stderr(std::process::Stdio::null());
        }
        let spawned = command.spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                self.failed.insert(spec.url.clone());
                return Err(e);
            }
        };
        crate::media_log::record(|| {
            format!(
                "player spawned kind={:?} url={}",
                spec.kind,
                crate::media_log::safe_url(&spec.url)
            )
        });
        if let Some(stderr) = child.stderr.take() {
            read_player_stderr(stderr);
        }
        let Some(input) = child.stdin.take() else {
            stop_child(child);
            return Err(io::Error::other("player helper has no command pipe"));
        };
        let Some(output) = child.stdout.take() else {
            stop_child(child);
            return Err(io::Error::other("player helper has no frame pipe"));
        };
        // Backpressure bounds decoded frames even when software drawing or
        // a busy UI cannot keep up with the helper's capture rate.
        let (sender, messages) = mpsc::sync_channel(1);
        if let Err(error) = std::thread::Builder::new()
            .name("godwit-player-frames".into())
            .spawn(move || read_messages(output, &sender, &wake))
        {
            stop_child(child);
            return Err(error);
        }
        self.failed.remove(&spec.url);
        self.reclaim();
        let slot = (0..self.frames.row_count())
            .find(|i| !self.players.iter().any(|p| p.slot == *i))
            .unwrap_or_else(|| {
                self.frames.push(Image::default());
                self.frames.row_count() - 1
            });
        self.players.push(RunningPlayer {
            spec: spec.clone(),
            inline_size: (width, height),
            size: (width, height),
            child: Some(child),
            input,
            messages,
            image: None,
            slot,
            used: self.clock,
            touched: false,
        });
        Ok(true)
    }

    /// Players running.
    pub fn len(&self) -> usize {
        self.players.len()
    }

    pub fn is_empty(&self) -> bool {
        self.players.is_empty()
    }

    pub fn image(&self, url: &str) -> Option<Image> {
        self.find(url)?.image.clone()
    }

    /// The shared frame list, for the window's `Players` global.
    pub fn frames(&self) -> Rc<VecModel<Image>> {
        self.frames.clone()
    }

    /// The slot in `frames` showing `url`'s player, once it has a frame.
    pub fn slot(&self, url: &str) -> Option<usize> {
        self.find(url).filter(|p| p.image.is_some()).map(|p| p.slot)
    }

    /// Lets go of the frames of players that stopped.
    fn reclaim(&self) {
        for i in 0..self.frames.row_count() {
            let empty = self.frames.row_data(i).is_none_or(|f| f.size().width == 0);
            if !empty && !self.players.iter().any(|p| p.slot == i) {
                self.frames.set_row_data(i, Image::default());
            }
        }
    }

    pub fn active(&self, url: &str) -> bool {
        self.find(url).is_some()
    }

    /// A fullscreen player is in use even if its timeline row scrolls away.
    pub fn keep_playing(&mut self, url: &str) {
        if let Some(player) = self.players.iter_mut().find(|p| p.spec.url == url) {
            player.touched = true;
        }
    }

    /// Resize the existing WebView, retaining its DOM and playback state.
    pub fn set_fullscreen(&mut self, url: &str, fullscreen: bool) {
        let Some(player) = self.players.iter_mut().find(|p| p.spec.url == url) else {
            return;
        };
        let (w, h) = if fullscreen {
            fullscreen_size(player.spec.width, player.spec.height)
        } else {
            player.inline_size
        };
        let _ = writeln!(player.input, "s {w} {h}");
    }

    /// Whether `url` can't load by itself (it failed before).
    pub fn failed(&self, url: &str) -> bool {
        self.failed.contains(url)
    }

    pub fn stop(&mut self, url: &str) {
        self.players.retain(|p| p.spec.url != url);
        self.reclaim();
    }

    pub fn stop_all(&mut self) {
        self.players.clear();
        self.reclaim();
    }

    /// Frees a slot for a player on screen: stops the least recently used
    /// player that's neither on screen (`keep`) nor clicked into. False if
    /// every player is one of those.
    pub fn make_room(&mut self, keep: &std::collections::HashSet<String>) -> bool {
        let oldest = self
            .players
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.touched && !keep.contains(&p.spec.url))
            .min_by_key(|(_, p)| p.used)
            .map(|(i, _)| i);
        match oldest {
            Some(i) => {
                self.players.remove(i);
                self.reclaim();
                true
            }
            None => false,
        }
    }

    /// Stops players far from the view, except ones clicked into.
    pub fn stop_unless_near(&mut self, urls: &std::collections::HashSet<String>) {
        self.players.retain(|p| p.touched || urls.contains(&p.spec.url));
        self.reclaim();
    }

    pub fn pointer(&mut self, url: &str, x: i32, y: i32, down: bool) {
        if down && let Some(player) = self.players.iter_mut().find(|p| p.spec.url == url) {
            player.touched = true;
        }
        self.send(url, if down { "d" } else { "u" }, x, y);
    }

    pub fn move_pointer(&mut self, url: &str, x: i32, y: i32) {
        self.send(url, "m", x, y);
    }

    fn send(&mut self, url: &str, op: &str, x: i32, y: i32) {
        let Some(player) = self.players.iter_mut().find(|p| p.spec.url == url) else {
            return;
        };
        // Map each axis separately: helper dimensions are rounded and
        // bounded independently, and can change on entering fullscreen.
        let x = (x as f32 * player.size.0 as f32 / player.spec.width.max(1) as f32) as i32;
        let y = (y as f32 * player.size.1 as f32 / player.spec.height.max(1) as f32) as i32;
        let _ = writeln!(player.input, "{op} {x} {y}");
    }

    fn find(&self, url: &str) -> Option<&RunningPlayer> {
        self.players.iter().find(|p| p.spec.url == url)
    }

    pub fn poll(&mut self) -> Polled {
        let mut polled = Polled::default();
        for player in &mut self.players {
            let mut latest = None;
            while let Ok(msg) = player.messages.try_recv() {
                match msg {
                    Msg::Frame(frame) => {
                        latest = Some(frame);
                    }
                    Msg::Link(url) => polled.links.push(url),
                    Msg::Fullscreen => polled.fullscreen.push(player.spec.clone()),
                }
            }
            // Only the newest queued frame can reach the display. Copying
            // superseded frames wastes UI time and delays input handling.
            // The row changes only for the first frame (the player replaces
            // the thumbnail); later ones just swap the slot's picture.
            if let Some(frame) = latest {
                player.size = (frame.width, frame.height);
                // Cairo's pixels are premultiplied, as this constructor expects.
                let image = Image::from_rgba8_premultiplied(frame.pixels);
                polled.changed |= player.image.is_none();
                if player.slot < self.frames.row_count() {
                    self.frames.set_row_data(player.slot, image.clone());
                }
                player.image = Some(image);
            }
        }
        let before = self.players.len();
        let failed = &mut self.failed;
        self.players.retain_mut(|player| {
            let exited = player.child.as_mut().and_then(|child| child.try_wait().ok()).flatten();
            match exited {
                Some(status) => {
                    crate::media_log::record(|| format!("player exited kind={:?} status={status}", player.spec.kind));
                    // Crashed or refused: don't start it again by itself.
                    if !status.success() {
                        failed.insert(player.spec.url.clone());
                    }
                    // try_wait already reaped it; Drop need not start a reaper.
                    player.child = None;
                    false
                }
                None => true,
            }
        });
        if self.players.len() != before {
            polled.changed = true;
            self.reclaim();
        }
        polled
    }
}

fn read_player_stderr(stderr: ChildStderr) {
    let _ = std::thread::Builder::new()
        .name("godwit-player-stderr".into())
        .spawn(move || {
            for line in io::BufReader::new(stderr).lines().map_while(Result::ok) {
                if crate::media_log::enabled() {
                    if let Some(event) = line.strip_prefix("GODWIT_MEDIA: ") {
                        crate::media_log::line(&crate::media_log::safe_stderr(event));
                    } else {
                        let safe = crate::media_log::safe_stderr(&line);
                        crate::media_log::record(|| format!("player stderr: {safe}"));
                    }
                }
            }
        });
}

/// Preserve aspect ratio inside the fullscreen frame limit.
fn fullscreen_size(width: u32, height: u32) -> (u32, u32) {
    let width = width.max(1);
    let height = height.max(1);
    let scale = (f64::from(FULL_WIDTH) / f64::from(width)).min(f64::from(FULL_HEIGHT) / f64::from(height));
    let even = |v: f64| (v.floor() as u32).max(2) & !1;
    (even(f64::from(width) * scale), even(f64::from(height) * scale))
}

/// The helper's messages, until its pipe closes or it sends nonsense.
fn read_messages(mut output: impl Read, sender: &mpsc::SyncSender<Msg>, wake: &Waker) {
    loop {
        let mut header = [0; 9];
        if output.read_exact(&mut header).is_err() {
            break;
        }
        let a = u32::from_le_bytes([header[1], header[2], header[3], header[4]]);
        let b = u32::from_le_bytes([header[5], header[6], header[7], header[8]]);
        let msg = match header[0] {
            b'F' => {
                let fits = (a as usize)
                    .checked_mul(b as usize)
                    .and_then(|px| px.checked_mul(4))
                    .is_some_and(|n| a > 0 && b > 0 && n <= MAX_FRAME_BYTES);
                if !fits {
                    break;
                }
                // Read straight into the buffer Slint draws: no copies.
                let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(a, b);
                let rgba = pixels.make_mut_bytes();
                if output.read_exact(rgba).is_err() {
                    break;
                }
                // Cairo ARGB32 is BGRA in memory on the supported Linux
                // targets (and the Windows helper sends the same); Slint's
                // shared image buffer is RGBA.
                for pixel in rgba.as_chunks_mut::<4>().0 {
                    pixel.swap(0, 2);
                }
                Msg::Frame(Frame {
                    width: a,
                    height: b,
                    pixels,
                })
            }
            b'U' => {
                let len = a as usize;
                if len > MAX_LINK_BYTES {
                    break;
                }
                let mut url = vec![0; len];
                if output.read_exact(&mut url).is_err() {
                    break;
                }
                match String::from_utf8(url) {
                    // Only web links reach the browser.
                    Ok(url) if url.starts_with("https://") || url.starts_with("http://") => Msg::Link(url),
                    _ => continue,
                }
            }
            b'X' => Msg::Fullscreen,
            _ => break,
        };
        if sender.send(msg).is_err() {
            return;
        }
        wake();
    }
    wake();
}

/// Opens `spec`'s player in its own window, fullscreen: what the player's
/// fullscreen button and the page's own fullscreen request do (Godwit's
/// pop-out). Plays from the start.
pub fn pop_out(spec: &Spec) -> io::Result<()> {
    if !valid_player_url(&spec.url) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "unsupported player URL"));
    }
    if !cfg!(any(target_os = "linux", windows)) {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "no player window here"));
    }
    std::process::Command::new(player_executable()?)
        .args(["--window", spec.kind.arg(), &spec.url])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
}

/// The player helper (`godwit-player`), installed beside this program. A
/// binary of its own so Godwit itself never loads WebKit, GTK and
/// GStreamer (~15 MB) unless a player starts. On Linux `/proc/self/exe`
/// leads to the running binary's folder even after an update replaced it.
/// A one-file release carries it inside (see `build.rs`, `embedded_player`)
/// and unpacks it once into the cache, in a folder named by its hash.
pub(crate) fn player_executable() -> io::Result<std::path::PathBuf> {
    let name = if cfg!(windows) {
        "godwit-player.exe"
    } else {
        "godwit-player"
    };
    let beside = beside_exe(name)?;
    if EMBEDDED_PLAYER.is_empty() || beside.is_file() {
        return Ok(beside);
    }
    static UNPACKED: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    if let Some(path) = UNPACKED.get() {
        return Ok(path.clone());
    }
    let path = unpack_player(name)?;
    Ok(UNPACKED.get_or_init(|| path).clone())
}

static EMBEDDED_PLAYER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/player.bin"));

fn beside_exe(name: &str) -> io::Result<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    if let Ok(exe) = std::fs::read_link("/proc/self/exe") {
        // A replaced binary reads as "… (deleted)"; its folder is the same.
        return Ok(exe.with_file_name(name));
    }
    Ok(std::env::current_exe()?.with_file_name(name))
}

/// Writes the embedded helper to `<cache>/player-<hash>/` unless it's there,
/// and removes other versions' folders.
fn unpack_player(name: &str) -> io::Result<std::path::PathBuf> {
    let digest = ring::digest::digest(&ring::digest::SHA256, EMBEDDED_PLAYER);
    let id: String = digest.as_ref().iter().take(8).map(|b| format!("{b:02x}")).collect();
    let cache = directories::ProjectDirs::from("", "", "godwit")
        .map(|d| d.cache_dir().to_path_buf())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no cache directory"))?;
    let folder = format!("player-{id}");
    let dir = cache.join(&folder);
    let path = dir.join(name);
    let ready = std::fs::metadata(&path).is_ok_and(|m| m.len() == EMBEDDED_PLAYER.len() as u64);
    if !ready {
        std::fs::create_dir_all(&dir)?;
        let part = dir.join(format!("{name}.part"));
        std::fs::write(&part, EMBEDDED_PLAYER)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&part, &path)?;
        if let Ok(entries) = std::fs::read_dir(&cache) {
            for entry in entries.flatten() {
                let other = entry.file_name();
                let other = other.to_string_lossy();
                if other.starts_with("player-") && other != folder {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }
    Ok(path)
}

fn valid_player_url(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|url| url.scheme() == "https" && url.host_str().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(url: &str) -> Spec {
        Spec {
            url: url.into(),
            kind: Kind::Video,
            width: 400,
            height: 225,
        }
    }

    #[test]
    fn player_urls_must_be_https() {
        assert!(valid_player_url("https://media.example/video.mp4"));
        assert!(!valid_player_url("http://media.example/video.mp4"));
        assert!(!valid_player_url("javascript:alert(1)"));
    }

    #[test]
    fn making_room_spares_players_on_screen() {
        let mut players = InlinePlayers::default();
        assert!(!players.make_room(&std::collections::HashSet::new()), "nothing to stop");
    }

    #[test]
    fn fullscreen_frames_preserve_aspect_ratio_within_the_wire_limit() {
        assert_eq!(fullscreen_size(400, 225), (2560, 1440));
        assert_eq!(fullscreen_size(225, 400), (810, 1440));
        for (width, height) in [(400, 300), (1920, 1080), (10, 1000), (1000, 10), (0, 0)] {
            let (w, h) = fullscreen_size(width, height);
            assert!(w >= 2 && h >= 2 && w <= FULL_WIDTH && h <= FULL_HEIGHT);
            assert_eq!((w % 2, h % 2), (0, 0));
            assert!(w as usize * h as usize * 4 <= MAX_FRAME_BYTES);
        }
    }

    #[test]
    fn player_manager_rejects_untrusted_schemes_before_starting_a_process() {
        let mut players = InlinePlayers::default();
        let wake: Waker = std::sync::Arc::new(|| {});
        assert!(
            players
                .start(&spec("file:///etc/passwd"), 1.0, true, wake.clone())
                .is_err()
        );
        assert!(pop_out(&spec("http://media.example/v.mp4")).is_err());
        assert!(players.players.is_empty());
    }

    #[test]
    fn helper_messages_parse_and_reject_oversized_or_unknown_ones() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"F");
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let link = b"https://www.youtube.com/watch?v=x";
        bytes.extend_from_slice(b"U");
        bytes.extend_from_slice(&(link.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(link);
        let bad = b"file:///etc/passwd";
        bytes.extend_from_slice(b"U");
        bytes.extend_from_slice(&(bad.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(bad);
        bytes.extend_from_slice(b"X");
        bytes.extend_from_slice(&[0; 8]);
        bytes.extend_from_slice(b"F");
        bytes.extend_from_slice(&100_000u32.to_le_bytes());
        bytes.extend_from_slice(&100_000u32.to_le_bytes());
        let (sender, receiver) = mpsc::sync_channel(4);
        let wake: Waker = std::sync::Arc::new(|| {});
        read_messages(&bytes[..], &sender, &wake);
        let got: Vec<Msg> = receiver.try_iter().collect();
        assert_eq!(got.len(), 3, "the bad link is skipped, the huge frame ends the stream");
        assert!(matches!(&got[0], Msg::Frame(f) if f.width == 2 && f.pixels.as_bytes() == [3, 2, 1, 4, 7, 6, 5, 8]));
        assert!(matches!(&got[1], Msg::Link(u) if u.as_bytes() == link));
        assert!(matches!(got[2], Msg::Fullscreen));
    }
}
