// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Opt-in, bounded media diagnostics. Callers must pass event descriptions,
//! never message text, names, or full signed URLs.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

pub const FILE_NAME: &str = "media-debug.log";
static ON: AtomicBool = AtomicBool::new(false);
static SUMMARY_WRITTEN: AtomicBool = AtomicBool::new(false);
static DIRECTORY: Mutex<Option<PathBuf>> = Mutex::new(None);

/// A lock, poisoned or not.
pub fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

pub fn path() -> Option<PathBuf> {
    DIRECTORY
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map(|dir| dir.join(FILE_NAME))
}

pub fn set(dir: Option<&Path>) {
    let on = godwit_media::debug::set_debug_log(dir);
    *DIRECTORY.lock().unwrap_or_else(PoisonError::into_inner) = on.then(|| dir.map(Path::to_path_buf)).flatten();
    ON.store(on, Ordering::Relaxed);
    if on && !SUMMARY_WRITTEN.swap(true, Ordering::Relaxed) {
        line(&environment_summary());
    }
}

pub fn line(message: &str) {
    if !enabled() {
        return;
    }
    godwit_media::debug::debug_log(message);
}

pub fn record(message: impl FnOnce() -> String) {
    if enabled() {
        line(&message());
    }
}

/// Keep only a URL's origin and path; signed query values carry credentials.
pub fn safe_url(value: &str) -> String {
    let Ok(url) = reqwest::Url::parse(value) else {
        return "<invalid URL>".into();
    };
    let host = url.host_str().unwrap_or_default();
    let port = url.port().map(|port| format!(":{port}")).unwrap_or_default();
    format!("{}://{}{}{}", url.scheme(), host, port, url.path())
}

pub fn safe_stderr(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut at = 0;
    while at < line.len() {
        let tail = &line[at..];
        let start = ["https://", "http://"]
            .into_iter()
            .filter_map(|scheme| tail.find(scheme))
            .min();
        let Some(start) = start else {
            output.push_str(tail);
            break;
        };
        output.push_str(&tail[..start]);
        let url_start = at + start;
        let rest = &line[url_start..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | ')' | ']' | '}' | ',' | ';'))
            .unwrap_or(rest.len());
        let token = &rest[..end];
        output.push_str(&safe_url(token));
        at = url_start + end;
    }
    output
}

/// Whether WebKit and GStreamer details can follow: the helper reports them
/// only when it could load.
fn helper_details() -> String {
    match crate::inline_player::missing_library() {
        Some(lib) => format!("the player helper could not load {lib}, so no WebKit or GStreamer details follow"),
        None => "WebKit and GStreamer runtime details follow from the player helper".to_owned(),
    }
}

fn environment_summary() -> String {
    format!(
        "environment os={} arch={}; {}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        helper_details()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_url_query_is_removed() {
        assert_eq!(
            safe_url("https://media.example/a.mp4?token=secret"),
            "https://media.example/a.mp4"
        );
        assert_eq!(
            safe_stderr("uri=(string)https://media.example/a.mp4?token=secret!"),
            "uri=(string)https://media.example/a.mp4"
        );
        assert_eq!(
            safe_stderr("webkit error https://media.example/a?token=secret failed"),
            "webkit error https://media.example/a failed"
        );
    }
}
