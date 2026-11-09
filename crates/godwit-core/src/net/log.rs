// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The network debug log: one line per request or connection event, for
//! troubleshooting. Off unless switched on; while off nothing is kept.
//!
//! A line holds only an endpoint's shape (see [`super::redact`]), status,
//! timing, sizes and short hashes of the request and the answer, so a
//! repeated answer stands out. Headers, cookies, tokens, query values and
//! bodies are never written; protobuf bodies show only their field numbers.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Lines kept in memory for a debug view.
const RECENT: usize = 2000;
/// The file is rotated (to `.1`) past this size.
const ROTATE: u64 = 4 << 20;
pub const FILE_NAME: &str = "network-debug.log";

static ON: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct Sink {
    recent: VecDeque<String>,
    path: Option<PathBuf>,
    file: Option<File>,
    written: u64,
}

fn sink() -> MutexGuard<'static, Sink> {
    static SINK: Mutex<Sink> = Mutex::new(Sink {
        recent: VecDeque::new(),
        path: None,
        file: None,
        written: 0,
    });
    SINK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the log is on.
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Switches the log on, writing to `dir/network-debug.log` (and memory), or
/// off (`None`), which also drops what's kept in memory.
pub fn set(dir: Option<&Path>) {
    let mut s = sink();
    s.file = None;
    s.written = 0;
    match dir {
        Some(dir) => {
            let path = dir.join(FILE_NAME);
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!("error: network debug log folder: {e}");
            }
            s.written = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            s.file = open(&path);
            s.path = Some(path);
            ON.store(true, Ordering::Relaxed);
        }
        None => {
            ON.store(false, Ordering::Relaxed);
            s.recent.clear();
            s.path = None;
        }
    }
}

/// The file the log writes to, when on.
pub fn path() -> Option<PathBuf> {
    sink().path.clone()
}

/// The newest lines, oldest first.
pub fn recent() -> Vec<String> {
    sink().recent.iter().cloned().collect()
}

fn open(path: &Path) -> Option<File> {
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!("error: network debug log: {e}");
            None
        }
    }
}

fn timestamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Writes one line (a timestamp goes in front), if the log is on.
pub fn line(text: &str) {
    if !enabled() {
        return;
    }
    let line = format!("{} {text}", timestamp());
    let mut s = sink();
    if s.recent.len() >= RECENT {
        s.recent.pop_front();
    }
    s.recent.push_back(line.clone());
    if s.written > ROTATE
        && let Some(path) = s.path.clone()
    {
        s.file = None;
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
        s.file = open(&path);
        s.written = 0;
    }
    let len = line.len() as u64 + 1;
    if let Some(f) = s.file.as_mut()
        && writeln!(f, "{line}").is_ok()
    {
        s.written += len;
    }
}

/// A connection event: `what` names the connection (`gateway`, `voice`,
/// `push`…), `event` says what happened (`connected`, `closed 4000`,
/// `retry in 2s`). Neither may hold ids or text from the network.
pub fn connection(network: &str, what: &str, event: &str) {
    if enabled() {
        line(&format!("{network} conn {what}: {event}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_only_while_on() {
        let dir = std::env::temp_dir().join(format!("godwit-netlog-test-{}", std::process::id()));
        set(None);
        line("dropped");
        assert!(recent().is_empty());
        set(Some(&dir));
        line("kept");
        assert!(recent().iter().any(|l| l.ends_with(" kept")));
        let file = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap_or_default();
        assert!(file.contains(" kept"));
        set(None);
        assert!(recent().is_empty());
        let _ = std::fs::remove_file(dir.join(FILE_NAME));
        let _ = std::fs::remove_dir(&dir);
    }
}
