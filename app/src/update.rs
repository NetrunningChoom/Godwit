// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Updates itself from the project's releases on GitHub.
//!
//! At start, at most every six hours, one request asks for the latest
//! release. When it's newer than this build, the app asks first ("Update
//! available: download now and restart?"). On yes, the binary for this
//! platform and its signature are downloaded, the signature is checked
//! against the public key this build was made with, the new binary replaces
//! this one on disk, and Godwit starts again. Nothing is replaced unless the
//! signature checks out.
//!
//! Only release builds carry the key (`GODWIT_UPDATE_KEY` at build time, the
//! Ed25519 public key in hex); builds without it, debug builds and runs with
//! `GODWIT_NO_UPDATE` set never update themselves.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

// GitHub's API requires a user agent (set below) and rate-limits anonymous
// requests at 60 an hour per IP: fine at this cadence.
const LATEST: &str = "https://api.github.com/repos/NetrunningChoom/Godwit/releases/latest";
const EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// No release binary is anywhere near this; a bigger answer isn't one.
const MAX_BYTES: usize = 400 * 1024 * 1024;
const KEY: Option<&str> = option_env!("GODWIT_UPDATE_KEY");

/// The release asset for this platform, if releases have one.
fn asset_name() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("godwit-linux-x86_64")
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Some("godwit-windows-x86_64.exe")
    } else {
        None
    }
}

/// A newer release found, waiting for the user's yes.
struct Release {
    version: String,
    binary: String,
    signature: String,
}

static FOUND: std::sync::Mutex<Option<Release>> = std::sync::Mutex::new(None);

/// This binary's path, read at start: after the swap the OS reports the
/// replaced file instead (Linux adds " (deleted)", Windows names the .old).
static EXE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn exe() -> std::io::Result<PathBuf> {
    match EXE.get() {
        Some(path) => Ok(path.clone()),
        None => std::env::current_exe(),
    }
}

fn found() -> std::sync::MutexGuard<'static, Option<Release>> {
    FOUND.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Runs `job` on a thread of its own with a small async runtime.
fn background(job: impl std::future::Future<Output = ()> + Send + 'static) {
    let spawned = std::thread::Builder::new().name("update".into()).spawn(move || {
        match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime.block_on(job),
            Err(e) => eprintln!("update: no runtime: {e}"),
        }
    });
    if let Err(e) = spawned {
        eprintln!("update: {e}");
    }
}

/// Looks for a newer release in the background, when this build may update
/// itself; `offer` gets its version (on the update thread) to ask the user.
pub fn start(offer: impl FnOnce(String) + Send + 'static) {
    if let Ok(path) = std::env::current_exe() {
        let _ = EXE.set(path);
    }
    remove_previous();
    if KEY.and_then(hex).is_none() || cfg!(debug_assertions) || std::env::var_os("GODWIT_NO_UPDATE").is_some() {
        return;
    }
    let Some(asset) = asset_name() else {
        return;
    };
    if !due() {
        return;
    }
    background(async move {
        match latest(asset).await {
            Ok(Some(release)) => {
                let version = release.version.clone();
                *found() = Some(release);
                offer(version);
            }
            Ok(None) => {}
            Err(e) => eprintln!("update: {e}"),
        }
    });
}

/// After the user said yes: downloads, checks and installs the release
/// found; `done` hears how it went (on the update thread).
pub fn install(progress: impl Fn(f32) + Send + Sync + 'static, done: impl FnOnce(Result<(), String>) + Send + 'static) {
    let Some(release) = found().take() else {
        return done(Err("no update is waiting".into()));
    };
    let Some(key) = KEY.and_then(hex) else {
        return done(Err("this build can't update itself".into()));
    };
    background(async move { done(fetch(&release, &key, progress).await) });
}

/// Starts the (new) binary again with the same arguments; the caller quits.
pub fn restart() -> std::io::Result<()> {
    std::process::Command::new(exe()?)
        .args(std::env::args_os().skip(1))
        .spawn()
        .map(drop)
}

/// When the last check is older than [`EVERY`]; marks this one.
fn due() -> bool {
    let Some(stamp) = godwit_core::config_dir::get().map(|d| d.join("update-checked")) else {
        return false;
    };
    let recent = std::fs::metadata(&stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| SystemTime::now().duration_since(at).ok())
        .is_some_and(|age| age < EVERY);
    if recent {
        return false;
    }
    if let Some(dir) = stamp.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&stamp, b"");
    true
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("Godwit/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())
}

/// The latest release, when it's newer than this build.
async fn latest(asset: &str) -> Result<Option<Release>, String> {
    let release: serde_json::Value = client()?
        .get(LATEST)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let tag = release["tag_name"].as_str().unwrap_or_default();
    let version = tag.trim_start_matches('v');
    if !newer(version, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let url = |name: &str| {
        release["assets"]
            .as_array()
            .and_then(|assets| assets.iter().find(|a| a["name"].as_str() == Some(name)))
            .and_then(|a| a["browser_download_url"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| format!("release {tag} has no {name}"))
    };
    Ok(Some(Release {
        version: version.to_owned(),
        binary: url(asset)?,
        signature: url(&format!("{asset}.sig"))?,
    }))
}

/// Downloads the release, checks its signature and installs it.
async fn fetch(release: &Release, key: &[u8], progress: impl Fn(f32) + Send + Sync) -> Result<(), String> {
    let client = client()?;
    let binary = download(&client, &release.binary, &progress, 0.0, 0.94).await?;
    progress(0.95);
    let signature = download(&client, &release.signature, &progress, 0.95, 0.02).await?;
    progress(0.98);
    let signature = std::str::from_utf8(&signature)
        .ok()
        .and_then(|s| hex(s.trim()))
        .ok_or("the signature isn't hex")?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(&binary, &signature)
        .map_err(|_| format!("{}: the signature doesn't match; not installed", release.version))?;
    progress(0.99);
    replace(&binary).map_err(|e| format!("installing {}: {e}", release.version))
}

async fn download(
    client: &reqwest::Client,
    url: &str,
    progress: &(impl Fn(f32) + Send + Sync),
    start: f32,
    span: f32,
) -> Result<Vec<u8>, String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    let total = response.content_length();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > MAX_BYTES {
            return Err("the download is too big".into());
        }
        if let Some(total) = total.filter(|total| *total > 0) {
            progress(start + span * (bytes.len() as f32 / total as f32).clamp(0.0, 1.0));
        }
    }
    Ok(bytes)
}

/// Whether dotted version `a` is newer than `b` (numeric parts; a missing
/// or unreadable part counts as 0).
fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    let (a, b) = (parts(a), parts(b));
    let len = a.len().max(b.len());
    let at = |v: &[u64], i: usize| v.get(i).copied().unwrap_or(0);
    (0..len)
        .map(|i| at(&a, i).cmp(&at(&b, i)))
        .find(|o| o.is_ne())
        .is_some_and(std::cmp::Ordering::is_gt)
}

fn hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| s.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()))
        .collect()
}

/// The binary being replaced, kept aside on Windows (a running .exe can be
/// renamed, not overwritten) until the next start removes it.
fn previous(exe: &std::path::Path) -> PathBuf {
    exe.with_extension("old")
}

fn remove_previous() {
    if cfg!(windows)
        && let Ok(exe) = exe()
    {
        let _ = std::fs::remove_file(previous(&exe));
    }
}

/// Puts the new binary in this one's place: written beside it, then renamed
/// over it, so a failure halfway leaves the old one working.
fn replace(binary: &[u8]) -> std::io::Result<()> {
    let exe = exe()?;
    let new = exe.with_extension("new");
    std::fs::write(&new, binary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?;
    }
    if cfg!(windows) {
        let old = previous(&exe);
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&exe, &old)?;
        if let Err(e) = std::fs::rename(&new, &exe) {
            let _ = std::fs::rename(&old, &exe);
            return Err(e);
        }
        Ok(())
    } else {
        std::fs::rename(&new, &exe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.1.10", "0.1.9"));
        assert!(newer("0.2", "0.1.9"));
        assert!(!newer("0.1.1", "0.1.1"));
        assert!(!newer("0.1.0", "0.1.1"));
        assert!(!newer("", "0.1.1"));
    }

    #[test]
    fn hex_reads_pairs_only() {
        assert_eq!(hex("00ff10"), Some(vec![0, 255, 16]));
        assert_eq!(hex("abc"), None);
        assert_eq!(hex("zz"), None);
    }
}
