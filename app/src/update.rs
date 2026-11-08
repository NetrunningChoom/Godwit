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
/// Bound the expanded executable while it is being decoded.
const MAX_DECOMPRESSED_BYTES: usize = 512 * 1024 * 1024;
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
    /// The `.xz` copy, tried first (a quarter of the download).
    compressed: Option<String>,
    /// The raw binary: used when there's no `.xz`, or when anything on the
    /// `.xz` path fails (download, decompression, size cap, signature).
    raw: Option<String>,
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
        // It takes over once this one has quit (single_instance.rs).
        .env(crate::single_instance::REPLACING, "1")
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
    let (compressed, raw) = binary_urls(&release, asset);
    if compressed.is_none() && raw.is_none() {
        return Err(format!("release {tag} has no {asset}"));
    }
    Ok(Some(Release {
        version: version.to_owned(),
        compressed,
        raw,
        signature: asset_url(&release, &format!("{asset}.sig"))
            .ok_or_else(|| format!("release {tag} has no {asset}.sig"))?,
    }))
}

fn asset_url(release: &serde_json::Value, name: &str) -> Option<String> {
    release["assets"]
        .as_array()?
        .iter()
        .find(|asset| asset["name"].as_str() == Some(name))?["browser_download_url"]
        .as_str()
        .map(str::to_owned)
}

/// The release's `.xz` and raw copies of `asset` (either may be missing).
fn binary_urls(release: &serde_json::Value, asset: &str) -> (Option<String>, Option<String>) {
    (asset_url(release, &format!("{asset}.xz")), asset_url(release, asset))
}

/// Downloads the release, checks its signature and installs it.
async fn fetch(release: &Release, key: &[u8], progress: impl Fn(f32) + Send + Sync) -> Result<(), String> {
    let client = client()?;
    // The signature (over the raw binary) first: both paths check against it.
    let signature = download(&client, &release.signature, &progress, 0.0, 0.02).await?;
    let signature = std::str::from_utf8(&signature)
        .ok()
        .and_then(|s| hex(s.trim()))
        .ok_or("the signature isn't hex")?;
    let verify = |binary: Vec<u8>| -> Result<Vec<u8>, String> {
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
            .verify(&binary, &signature)
            .map_err(|_| format!("{}: the signature doesn't match; not installed", release.version))?;
        Ok(binary)
    };
    let binary = obtain(
        release,
        |url, start, span| {
            let (client, progress) = (&client, &progress);
            async move { download(client, &url, progress, start, span).await }
        },
        verify,
    )
    .await?;
    progress(0.99);
    replace(&binary).map_err(|e| format!("installing {}: {e}", release.version))
}

/// The verified raw binary: from the `.xz` copy when there is one, else
/// (or when anything on that path fails) from the raw copy.
async fn obtain<G, F>(
    release: &Release,
    get: G,
    verify: impl Fn(Vec<u8>) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String>
where
    G: Fn(String, f32, f32) -> F,
    F: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    let mut failed = None;
    if let Some(url) = &release.compressed {
        let attempt = match get(url.clone(), 0.02, 0.92).await {
            Ok(packed) => decompress_xz(&packed).and_then(&verify),
            Err(e) => Err(e),
        };
        match attempt {
            Ok(binary) => return Ok(binary),
            Err(e) => failed = Some(e),
        }
    }
    let Some(url) = &release.raw else {
        return Err(failed.unwrap_or_else(|| format!("{} has no binary to install", release.version)));
    };
    if let Some(e) = &failed {
        godwit_core::verbose!("note: the compressed update failed ({e}); downloading the full binary");
    }
    verify(get(url.clone(), 0.02, 0.96).await?)
}

fn decompress_xz(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = BoundedWriter {
        bytes: Vec::new(),
        limit: MAX_DECOMPRESSED_BYTES,
    };
    lzma_rs::xz_decompress(&mut std::io::Cursor::new(input), &mut output)
        .map_err(|_| "the compressed update is invalid or exceeds the size limit".to_owned())?;
    Ok(output.bytes)
}

struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl std::io::Write for BoundedWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if buffer.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("decompressed update exceeds size limit"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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

    fn run<T>(f: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    fn release(compressed: bool, raw: bool) -> Release {
        Release {
            version: "9.9.9".into(),
            compressed: compressed.then(|| "xz".into()),
            raw: raw.then(|| "raw".into()),
            signature: "sig".into(),
        }
    }

    fn packed(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        lzma_rs::xz_compress(&mut std::io::Cursor::new(bytes), &mut out).unwrap();
        out
    }

    /// "Verifies" by comparing with the expected binary, standing in for the signature.
    fn verify_is(expected: &'static [u8]) -> impl Fn(Vec<u8>) -> Result<Vec<u8>, String> {
        move |b| {
            if b == expected {
                Ok(b)
            } else {
                Err("signature mismatch".into())
            }
        }
    }

    #[test]
    fn uses_the_xz_copy_when_it_works() {
        let asked = std::sync::Mutex::new(Vec::new());
        let got = run(obtain(
            &release(true, true),
            |url, _, _| {
                asked.lock().unwrap().push(url.clone());
                let body = if url == "xz" { packed(b"bin") } else { b"bin".to_vec() };
                async move { Ok(body) }
            },
            verify_is(b"bin"),
        ));
        assert_eq!(got.unwrap(), b"bin");
        assert_eq!(*asked.lock().unwrap(), ["xz"]);
    }

    #[test]
    fn falls_back_to_the_raw_binary_on_any_xz_failure() {
        // Download error, corrupt archive, and a wrong signature over the unpacked bytes.
        for case in 0..3 {
            let asked = std::sync::Mutex::new(Vec::new());
            let got = run(obtain(
                &release(true, true),
                |url, _, _| {
                    asked.lock().unwrap().push(url.clone());
                    let body: Result<Vec<u8>, String> = match (url.as_str(), case) {
                        ("xz", 0) => Err("HTTP 500".into()),
                        ("xz", 1) => Ok(b"not xz".to_vec()),
                        ("xz", _) => Ok(packed(b"tampered")),
                        _ => Ok(b"bin".to_vec()),
                    };
                    async move { body }
                },
                verify_is(b"bin"),
            ));
            assert_eq!(got.unwrap(), b"bin", "case {case}");
            assert_eq!(*asked.lock().unwrap(), ["xz", "raw"], "case {case}");
        }
    }

    #[test]
    fn raw_only_releases_and_double_failures() {
        let got = run(obtain(
            &release(false, true),
            |_, _, _| async { Ok(b"bin".to_vec()) },
            verify_is(b"bin"),
        ));
        assert_eq!(got.unwrap(), b"bin");
        // Both copies bad: the raw path's error is reported, nothing installed.
        let got = run(obtain(
            &release(true, true),
            |_, _, _| async { Ok(b"junk".to_vec()) },
            verify_is(b"bin"),
        ));
        assert!(got.is_err());
        // Only an .xz and it fails: its error is reported.
        let got = run(obtain(
            &release(true, false),
            |_, _, _| async { Err("HTTP 404".to_owned()) },
            verify_is(b"bin"),
        ));
        assert_eq!(got.unwrap_err(), "HTTP 404");
    }

    #[test]
    fn finds_both_copies_and_either_alone() {
        let release = serde_json::json!({ "assets": [
            { "name": "godwit-linux-x86_64", "browser_download_url": "raw" },
            { "name": "godwit-linux-x86_64.xz", "browser_download_url": "compressed" }
        ] });
        assert_eq!(
            binary_urls(&release, "godwit-linux-x86_64"),
            (Some("compressed".into()), Some("raw".into()))
        );
        let raw_only = serde_json::json!({ "assets": [
            { "name": "godwit-linux-x86_64", "browser_download_url": "raw" }
        ] });
        assert_eq!(
            binary_urls(&raw_only, "godwit-linux-x86_64"),
            (None, Some("raw".into()))
        );
        let xz_only = serde_json::json!({ "assets": [
            { "name": "godwit-linux-x86_64.xz", "browser_download_url": "compressed" }
        ] });
        assert_eq!(
            binary_urls(&xz_only, "godwit-linux-x86_64"),
            (Some("compressed".into()), None)
        );
    }

    #[test]
    fn xz_decompression_round_trips() {
        let expected = b"small executable fixture";
        let mut compressed = Vec::new();
        lzma_rs::xz_compress(&mut std::io::Cursor::new(expected), &mut compressed).unwrap();
        assert_eq!(decompress_xz(&compressed).unwrap(), expected);
    }

    #[test]
    fn xz_decompression_enforces_size_limit_during_write() {
        let mut compressed = Vec::new();
        lzma_rs::xz_compress(&mut std::io::Cursor::new(vec![0u8; 32]), &mut compressed).unwrap();
        let mut output = BoundedWriter {
            bytes: Vec::new(),
            limit: 8,
        };
        assert!(lzma_rs::xz_decompress(&mut std::io::Cursor::new(compressed), &mut output).is_err());
        assert!(output.bytes.len() <= 8);
    }

    #[test]
    fn signature_is_checked_against_decompressed_bytes() {
        use ring::signature::KeyPair;

        let expected = b"decompressed executable";
        let mut compressed = Vec::new();
        lzma_rs::xz_compress(&mut std::io::Cursor::new(expected), &mut compressed).unwrap();
        let binary = decompress_xz(&compressed).unwrap();
        let rng = ring::rand::SystemRandom::new();
        let key = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key_pair = ring::signature::Ed25519KeyPair::from_pkcs8(key.as_ref()).unwrap();
        let signature = key_pair.sign(b"different bytes");
        assert!(
            ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key_pair.public_key().as_ref())
                .verify(&binary, signature.as_ref())
                .is_err()
        );
    }
}
