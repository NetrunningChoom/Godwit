// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Problems the networks and the app ran into, kept as a log instead of
//! pop-ups: Settings → Problems lists them, and `errors.log` keeps them
//! across restarts for the user to look at or share. It sits with the debug
//! logs in Godwit's cache folder, so every log is in one place.
//!
//! Errors are written without secrets where they're made (AGENTS.md), so the
//! log never holds tokens or passwords.

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

pub const FILE_NAME: &str = "errors.log";

/// Problems kept in memory for Settings.
const KEEP: usize = 200;
/// The log file is cut to its newer half past this size.
const MAX_FILE: u64 = 256 * 1024;

pub struct Entry {
    /// Local time, `2026-11-01 18:04:12`.
    pub when: String,
    pub text: Arc<str>,
}

pub struct ErrorLog {
    entries: VecDeque<Entry>,
    /// Logged since Settings → Problems was last open.
    unseen: usize,
    /// None in demo mode, or when there's no cache directory.
    path: Option<PathBuf>,
}

impl ErrorLog {
    pub fn new(demo: bool) -> Self {
        let path = if demo {
            None
        } else {
            crate::settings::net_log_dir().map(|dir| {
                let path = dir.join(FILE_NAME);
                // Older versions kept it in the config folder: moved here once.
                if let Some(old) = godwit_core::config_dir::get().map(|d| d.join(FILE_NAME))
                    && old != path
                    && old.is_file()
                    && !path.exists()
                {
                    let _ = std::fs::create_dir_all(&dir);
                    if std::fs::rename(&old, &path).is_err() && std::fs::copy(&old, &path).is_ok() {
                        let _ = std::fs::remove_file(&old);
                    }
                }
                path
            })
        };
        Self {
            entries: VecDeque::new(),
            unseen: 0,
            path,
        }
    }

    pub fn push(&mut self, text: Arc<str>) {
        let when = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        // Network errors arrive in bursts (a dropped connection fails every
        // request): one line per distinct problem in a row.
        if self.entries.back().is_some_and(|e| e.text == text) {
            return;
        }
        if let Some(path) = &self.path {
            append(path, &format!("{when}  {text}\n"));
        }
        self.entries.push_back(Entry { when, text });
        if self.entries.len() > KEEP {
            self.entries.pop_front();
        }
        self.unseen += 1;
    }

    /// Newest first.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().rev()
    }

    pub fn unseen(&self) -> usize {
        self.unseen
    }

    pub fn seen(&mut self) {
        self.unseen = 0;
    }

    /// Settings → Problems → Clear: the list and the file.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.unseen = 0;
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }

    /// The whole list as text, oldest first, for copying.
    pub fn text(&self) -> String {
        self.entries
            .iter()
            .map(|e| format!("{}  {}\n", e.when, e.text))
            .collect()
    }

    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }
}

/// Appends a line, owner-only, cutting the file to its newer half when big.
fn append(path: &PathBuf, line: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_FILE)
        && let Ok(old) = std::fs::read_to_string(path)
    {
        let half = old.len() / 2;
        let start = old[half..].find('\n').map_or(old.len(), |i| half + i + 1);
        let _ = std::fs::write(path, &old[start..]);
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    if let Ok(mut file) = options.open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_in_a_row_are_logged_once_and_the_list_is_bounded() {
        let mut log = ErrorLog::new(true);
        log.push("a".into());
        log.push("a".into());
        log.push("b".into());
        assert_eq!(log.unseen(), 2);
        assert_eq!(log.entries().next().map(|e| e.text.as_ref()), Some("b"));
        for i in 0..(KEEP + 10) {
            log.push(i.to_string().into());
        }
        assert_eq!(log.entries().count(), KEEP);
        log.seen();
        assert_eq!(log.unseen(), 0);
        assert!(log.text().ends_with(&format!("{}\n", KEEP + 9)));
    }

    #[test]
    fn the_file_is_cut_to_its_newer_half_when_big() {
        let dir = std::env::temp_dir().join(format!("godwit-errors-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("errors.log");
        let _ = std::fs::write(&path, "old line\n".repeat((MAX_FILE / 9 + 10) as usize));
        append(&path, "new line\n");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(text.len() < MAX_FILE as usize);
        assert!(text.ends_with("new line\n") && text.starts_with("old line\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
