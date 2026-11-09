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
    let line = format!("{} media {message}", timestamp());
    if writeln!(file, "{line}").is_ok() {
        *length += line.len() as u64 + 1;
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
