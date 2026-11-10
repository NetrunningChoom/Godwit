// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Opt-in media diagnostics shared by the app and its media decoders.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

const MAX: u64 = 4 << 20;
static ON: AtomicBool = AtomicBool::new(false);
static SINK: Mutex<Option<(PathBuf, std::fs::File, u64)>> = Mutex::new(None);
static REPEATS: Mutex<Repeats> = Mutex::new(Repeats {
    shape: String::new(),
    count: 0,
});
/// Longest line kept; the rest of a long one (GStreamer's caps lists) is cut.
const LINE_MAX: usize = 400;

/// The last line's shape (digits aside) and how many lines like it followed.
struct Repeats {
    shape: String,
    count: u64,
}

/// The lines to write for `message`: cut to [`LINE_MAX`], without terminal
/// colour codes, and a run of lines that differ only in numbers (times,
/// addresses, sizes) kept as its first and a count.
fn condense(repeats: &mut Repeats, message: &str) -> Vec<String> {
    let mut clean = String::with_capacity(message.len().min(LINE_MAX + 4));
    let mut chars = message.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI: ESC [ … final byte in @..~
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        if clean.len() >= LINE_MAX {
            clean.push('…');
            break;
        }
        clean.push(c);
    }
    let mut shape = String::with_capacity(clean.len());
    for c in clean.chars() {
        if c.is_ascii_digit() {
            if !shape.ends_with('#') {
                shape.push('#');
            }
        } else {
            shape.push(c);
        }
    }
    if shape == repeats.shape {
        repeats.count += 1;
        return Vec::new();
    }
    let mut out = Vec::with_capacity(2);
    if repeats.count > 0 {
        out.push(format!("(×{} more like the line before)", repeats.count));
    }
    out.push(clean);
    *repeats = Repeats { shape, count: 0 };
    out
}

fn sink() -> MutexGuard<'static, Option<(PathBuf, std::fs::File, u64)>> {
    SINK.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

pub fn set_debug_log(dir: Option<&Path>) -> bool {
    let mut state = sink();
    *state = dir.and_then(|dir| {
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join("media-debug.log");
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).ok()?;
        let length = std::fs::metadata(&path).map(|metadata| metadata.len()).unwrap_or(0);
        Some((path, file, length))
    });
    let on = state.is_some();
    ON.store(on, Ordering::Relaxed);
    on
}

/// Writes a message that contains only media diagnostics and no user content.
pub fn debug_log(message: &str) {
    if !enabled() {
        return;
    }
    let mut state = sink();
    let Some((path, file, length)) = state.as_mut() else {
        return;
    };
    if *length >= MAX {
        let rotated = path.with_extension("log.1");
        let _ = std::fs::remove_file(&rotated);
        if std::fs::rename(&*path, &rotated).is_err() {
            return;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let Ok(next) = options.open(path) else { return };
        *file = next;
        *length = 0;
    }
    let lines = condense(&mut REPEATS.lock().unwrap_or_else(PoisonError::into_inner), message);
    for message in lines {
        let line = format!("{} media {message}", timestamp());
        if writeln!(file, "{line}").is_ok() {
            *length += line.len() as u64 + 1;
        }
    }
}

fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn repeats_collapse_and_long_coloured_lines_are_cut() {
        let mut r = Repeats {
            shape: String::new(),
            count: 0,
        };
        assert_eq!(condense(&mut r, "\u{1b}[32mDEBUG\u{1b}[0m pad 1"), ["DEBUG pad 1"]);
        assert!(condense(&mut r, "DEBUG pad 22").is_empty());
        assert!(condense(&mut r, "DEBUG pad 333").is_empty());
        assert_eq!(
            condense(&mut r, "player exited"),
            ["(×2 more like the line before)", "player exited"]
        );
        let long = "x".repeat(1000);
        assert_eq!(condense(&mut r, &long)[0].chars().count(), LINE_MAX + 1);
    }

    use super::*;

    #[test]
    fn log_rotation_keeps_only_the_current_and_previous_files_bounded() {
        let dir = std::env::temp_dir().join(format!("godwit-media-debug-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("media-debug.log");
        std::fs::write(&path, vec![b'x'; usize::try_from(MAX + 1).unwrap()]).unwrap();
        assert!(set_debug_log(Some(&dir)));
        debug_log("after rotation");
        let rotated = path.with_extension("log.1");
        assert!(std::fs::metadata(&rotated).is_ok_and(|metadata| metadata.len() == MAX + 1));
        assert!(std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() < 256));
        let _ = set_debug_log(None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
