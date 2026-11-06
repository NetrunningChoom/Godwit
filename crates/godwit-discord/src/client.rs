// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! How Godwit presents itself to Discord: as Discord's own desktop app
//! (Electron) on Windows 11, whatever Godwit runs on. The most common setup
//! there is, so a Godwit install looks as generic as possible: no distro,
//! desktop or kernel that would set it apart. The gateway, the HTTP API and the voice servers all carry the
//! same description, and calls use the desktop app's UDP voice, so nothing
//! about Godwit reads as a browser.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

/// The Electron and Chromium the desktop app ships with. They change only
/// with Discord's own updates: compare with a current desktop app (its
/// User-Agent, or Settings → Advanced's versions) when updating these.
/// Last checked 2026-11-03 against a measured desktop 1.0.167 on Linux
/// (neucord issue #92's baseline).
const ELECTRON: &str = "44.9.3";
const CHROME: &str = "152.0.7977.167";

/// The Windows desktop app's version when last checked, for when Discord's
/// update server can't be asked and nothing was kept (see [`learn_client_version`]).
fn fallback_client_version() -> &'static str {
    kept().client_version.as_deref().unwrap_or("1.0.9215")
}

/// The versions Discord's servers last gave, kept in the config directory
/// (not secret): a run that can't reach them starts from these rather than
/// from the ones built in, which age.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Versions {
    #[serde(default)]
    client_version: Option<String>,
    #[serde(default)]
    native_build: u64,
    #[serde(default)]
    build_number: u64,
}

fn versions_path() -> Option<std::path::PathBuf> {
    godwit_core::config_dir::get().map(|d| d.join("discord-versions.json"))
}

fn read_versions() -> Versions {
    if cfg!(test) {
        return Versions::default();
    }
    versions_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// What was kept when this run started.
fn kept() -> &'static Versions {
    static KEPT: OnceLock<Versions> = OnceLock::new();
    KEPT.get_or_init(read_versions)
}

/// Saves a newly learned version for later runs.
fn keep(update: impl FnOnce(&mut Versions)) {
    if cfg!(test) {
        return;
    }
    static WRITING: Mutex<()> = Mutex::new(());
    let _guard = WRITING.lock().unwrap_or_else(PoisonError::into_inner);
    let mut versions = read_versions();
    let before = versions.clone();
    update(&mut versions);
    if versions == before {
        return;
    }
    if let (Some(path), Ok(text)) = (versions_path(), serde_json::to_vec_pretty(&versions)) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("discord: could not keep the client versions: {e}");
        }
    }
}

/// The desktop app's version as Discord's update server gave it this run.
static CLIENT_VERSION: OnceLock<String> = OnceLock::new();

fn client_version() -> &'static str {
    CLIENT_VERSION
        .get()
        .map_or_else(fallback_client_version, String::as_str)
}

/// Takes the version from Discord's Windows update server's answer:
/// `{"full": {"host_version": [1, 0, 9215]}}` (`{"name": "0.0.116"}` is the
/// old Linux/macOS form, still read).
pub(crate) fn learn_client_version(answer: &Value) -> bool {
    let version = answer["name"].as_str().map(str::to_owned).or_else(|| {
        let parts = answer["full"]["host_version"].as_array()?;
        let parts: Vec<String> = parts.iter().filter_map(Value::as_u64).map(|n| n.to_string()).collect();
        (parts.len() == 3).then(|| parts.join("."))
    });
    let Some(version) =
        version.filter(|v| v.split('.').count() == 3 && v.chars().all(|c| c.is_ascii_digit() || c == '.'))
    else {
        return false;
    };
    // Set once: a run keeps the version it started with, as the app does.
    keep(|v| v.client_version = Some(version.clone()));
    let _ = CLIENT_VERSION.set(version);
    // Windows' new update system also has a native build number
    // (`native_build_number`; docs.discord.food, Client Properties).
    if let Some(native) = answer["full"]["metadata_version"].as_u64().filter(|&n| n > 0) {
        NATIVE_BUILD.store(native, Ordering::Relaxed);
        keep(|v| v.native_build = native);
    }
    true
}

/// The Windows app's native build number from its update manifest, or 0.
static NATIVE_BUILD: AtomicU64 = AtomicU64::new(0);

fn native_build_number() -> Value {
    match NATIVE_BUILD.load(Ordering::Relaxed) {
        0 if kept().native_build == 0 => Value::Null,
        0 => kept().native_build.into(),
        n => n.into(),
    }
}

/// Where the Windows desktop app asks for its updates: its version and
/// native build number follow Discord's releases by themselves.
pub(crate) fn update_url() -> &'static str {
    "https://updates.discord.com/distributions/app/manifests/latest?channel=stable&platform=win&arch=x64"
}

/// The desktop app's `User-Agent` (Electron's, with `discord/<version>` in
/// it). Made each time: the version may be learned after the first request.
pub fn user_agent() -> String {
    format!(
        "Mozilla/5.0 ({}) AppleWebKit/537.36 (KHTML, like Gecko) discord/{} Chrome/{CHROME} Electron/{ELECTRON} Safari/537.36",
        platform().1,
        client_version()
    )
}

/// The `os` property and the User-Agent's platform part (Windows 11 still
/// says `Windows NT 10.0` there).
fn platform() -> (&'static str, &'static str) {
    ("Windows", "Windows NT 10.0; Win64; x64")
}

/// The OS details sent: never this computer's. Each install picks one of
/// the current Windows 11 builds at random the first time and keeps it, so
/// Godwit installs don't all share one description (a fingerprint of their
/// own) and an account keeps looking like the same computer.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Identity {
    /// Windows' `10.0.<build>`.
    os_version: String,
}

/// The current Windows 11 releases, weighted alike: 25H2 (26200) and 24H2
/// (26100), which share their updates. Older ones are left out: a new install
/// reporting one would stand out. When Microsoft ships the next release, add
/// it and drop the oldest (last checked 2026-11-05); kept identities then
/// move on by themselves (see [`Identity::current`]).
const WINDOWS: &[(&str, u32)] = &[("10.0.26200", 1), ("10.0.26100", 1)];

fn pick<T>(choices: &[T], weight: impl Fn(&T) -> u32, roll: u32) -> Option<&T> {
    let total: u32 = choices.iter().map(&weight).sum();
    let mut roll = roll % total.max(1);
    choices.iter().find(|c| {
        let w = weight(c);
        if roll < w {
            true
        } else {
            roll -= w;
            false
        }
    })
}

impl Identity {
    fn random() -> Self {
        let roll = rand::random::<u32>();
        let version = pick(WINDOWS, |c| c.1, roll).map_or(WINDOWS[0].0, |c| c.0);
        Self {
            os_version: version.to_owned(),
        }
    }

    /// Whether a kept identity is one this version would still pick: an
    /// older one (a Linux kernel, a retired Windows build) is replaced, like
    /// a computer that was updated.
    fn current(&self) -> bool {
        WINDOWS.iter().any(|(version, _)| *version == self.os_version)
    }

    /// Kept in the config directory: not secret, and the same for every
    /// account on this install, as the official app's would be.
    fn load_or_create() -> Self {
        let path = godwit_core::config_dir::get().map(|d| d.join("discord-identity.json"));
        if cfg!(test) {
            return Self::random();
        }
        if let Some(kept) = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Self>(&b).ok())
            .filter(Identity::current)
        {
            return kept;
        }
        let made = Self::random();
        if let (Some(path), Ok(text)) = (path, serde_json::to_vec_pretty(&made)) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::write(&path, text) {
                eprintln!("discord: could not keep the client identity: {e}");
            }
        }
        made
    }
}

fn identity() -> &'static Identity {
    static IDENTITY: OnceLock<Identity> = OnceLock::new();
    IDENTITY.get_or_init(Identity::load_or_create)
}

/// The CPU architecture in Node's terms (`x64`, `arm64`).
fn arch() -> &'static str {
    if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" }
}

/// The web client's build number when last checked, for when its page can't
/// be read.
const CLIENT_BUILD_NUMBER: u64 = 657850;
/// The build number read from the web client's page this run, or 0.
static BUILD_NUMBER: AtomicU64 = AtomicU64::new(0);

/// The build number sent: the web client's current one once it's known.
fn build_number() -> u64 {
    match BUILD_NUMBER.load(Ordering::Relaxed) {
        0 if kept().build_number == 0 => CLIENT_BUILD_NUMBER,
        0 => kept().build_number,
        n => n,
    }
}

pub(crate) fn knows_build_number() -> bool {
    BUILD_NUMBER.load(Ordering::Relaxed) != 0
}

/// Takes the build number from the web client's page, which sets it in
/// `GLOBAL_ENV`. Returns whether the page had one.
pub(crate) fn learn_build_number(page: &[u8]) -> bool {
    const KEY: &[u8] = br#""BUILD_NUMBER":""#;
    let Some(at) = page.windows(KEY.len()).position(|w| w == KEY) else {
        return false;
    };
    let digits = page.get(at + KEY.len()..).unwrap_or_default();
    let end = digits.iter().position(|b| !b.is_ascii_digit()).unwrap_or(digits.len());
    let Some(number) = std::str::from_utf8(digits.get(..end).unwrap_or_default())
        .ok()
        .and_then(|d| d.parse::<u64>().ok())
        .filter(|&n| n > 0)
    else {
        return false;
    };
    BUILD_NUMBER.store(number, Ordering::Relaxed);
    keep(|v| v.build_number = number);
    true
}

/// A random UUID as the web client makes them, with `cleared` bits unset.
fn uuid(cleared: u128) -> String {
    let mut bits = rand::random::<u128>();
    // Version 4, variant 1.
    bits = (bits & !(0xf << 76)) | (0x4 << 76);
    bits = (bits & !(0x3 << 62)) | (0x2 << 62);
    bits &= !cleared;
    let hex = format!("{bits:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// Identifies this run, as a launch of the client.
fn launch_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| uuid(0))
}

/// The launch signature: a random UUID with the bits that report client
/// modifications unset.
fn launch_signature() -> &'static str {
    const MODIFICATIONS: u128 = 0x0080_1010_0810_0800_2081_0040_0100_0800;
    static SIGNATURE: OnceLock<String> = OnceLock::new();
    SIGNATURE.get_or_init(|| uuid(MODIFICATIONS))
}

/// The analytics heartbeat session, which the web client renews every 30
/// minutes: its id and when it started (Unix milliseconds).
pub(crate) fn heartbeat_session() -> (String, u64) {
    const AGE: Duration = Duration::from_secs(30 * 60);
    static SESSION: Mutex<Option<(Instant, String, u64)>> = Mutex::new(None);
    let mut session = SESSION.lock().unwrap_or_else(PoisonError::into_inner);
    if session.as_ref().is_none_or(|(started, _, _)| started.elapsed() >= AGE) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0));
        *session = Some((Instant::now(), uuid(0), now));
    }
    session
        .as_ref()
        .map(|(_, id, at)| (id.clone(), *at))
        .unwrap_or_default()
}

/// The time-spent session for the gateway's op 41: renewed with the
/// heartbeat session, sent on connect and whenever it changes.
pub(crate) fn time_spent_session() -> Value {
    let (session_id, initialization_timestamp) = heartbeat_session();
    json!({
        "initialization_timestamp": initialization_timestamp,
        "session_id": session_id,
        "client_launch_id": launch_id(),
    })
}

/// Whether one of our accounts is in a call, for the QoS heartbeat's
/// `rtc_connected` reason.
static IN_CALL: AtomicU64 = AtomicU64::new(0);

/// One of our calls, for as long as it's kept.
#[derive(Debug)]
pub struct InCall(());

impl InCall {
    pub fn start() -> Self {
        IN_CALL.fetch_add(1, Ordering::Relaxed);
        Self(())
    }
}

impl Drop for InCall {
    fn drop(&mut self) {
        let _ = IN_CALL.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
    }
}

/// Op 40's QoS payload (docs.discord.food, QoS Heartbeat; version 31).
/// The window counts as focused, as `client_app_state` says.
pub(crate) fn qos() -> Value {
    let mut reasons = vec!["foregrounded"];
    if IN_CALL.load(Ordering::Relaxed) > 0 {
        reasons.push("rtc_connected");
    }
    json!({"ver": 31, "active": true, "reasons": reasons})
}

/// The `properties` object sent when identifying on the gateway: the desktop
/// app's, in its order (the measured desktop session in neucord issue #92;
/// fields after `native_build_number` as discord.py-self records them). Key
/// order shows in the header, so serde_json keeps insertion order here.
pub fn properties() -> Value {
    let identity = identity();
    let mut props = serde_json::Map::new();
    let mut put = |key: &str, value: Value| {
        props.insert(key.to_owned(), value);
    };
    put("os", platform().0.into());
    put("browser", "Discord Client".into());
    put("release_channel", "stable".into());
    put("client_version", client_version().into());
    put("os_version", identity.os_version.as_str().into());
    put("os_arch", arch().into());
    put("app_arch", arch().into());
    put("system_locale", "en-US".into());
    put("has_client_mods", false.into());
    put("client_launch_id", launch_id().into());
    put("browser_user_agent", user_agent().into());
    put("browser_version", ELECTRON.into());
    put("client_build_number", build_number().into());
    put("native_build_number", native_build_number());
    put("client_event_source", Value::Null);
    put("launch_signature", launch_signature().into());
    put("client_heartbeat_session_id", heartbeat_session().0.into());
    put("client_app_state", "focused".into());
    Value::Object(props)
}

/// Chromium's `Sec-CH-UA`: its brand list with a GREASE brand, seeded by
/// the major version (`GenerateBrandVersionList` in Chromium's
/// `user_agent_utils.cc`). Electron lists only Chromium besides the GREASE
/// brand (electron/electron#34481).
pub fn sec_ch_ua() -> String {
    let major: usize = CHROME.split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
    grease_brands(major, &[("Chromium", major)])
}

fn grease_brands(seed: usize, brands: &[(&str, usize)]) -> String {
    const CHARS: [&str; 11] = [" ", "(", ":", "-", ".", "/", ")", ";", "=", "?", "_"];
    const VERSIONS: [&str; 3] = ["8", "99", "24"];
    let grease = format!(
        "\"Not{}A{}Brand\";v=\"{}\"",
        CHARS[seed % CHARS.len()],
        CHARS[(seed + 1) % CHARS.len()],
        VERSIONS[seed % VERSIONS.len()]
    );
    let mut listed: Vec<String> = brands.iter().map(|(b, v)| format!("\"{b}\";v=\"{v}\"")).collect();
    // GetRandomOrder: the permutation for this seed, the GREASE brand first
    // in the unshuffled list.
    let mut all = vec![grease];
    all.append(&mut listed);
    let order: Vec<usize> = match all.len() {
        2 => [[0, 1], [1, 0]][seed % 2].to_vec(),
        3 => [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]][seed % 6].to_vec(),
        n => (0..n).collect(),
    };
    let mut out = vec![String::new(); all.len()];
    for (item, &at) in all.into_iter().zip(&order) {
        if let Some(slot) = out.get_mut(at) {
            *slot = item;
        }
    }
    out.join(", ")
}

/// `Sec-CH-UA-Platform` for the platform we describe.
pub fn sec_ch_ua_platform() -> &'static str {
    "\"Windows\""
}

/// What Chromium accepts in compressed responses (and reqwest can undo).
pub const ACCEPT_ENCODING: &str = "gzip, deflate, br, zstd";

/// Headers for a picture from Discord's CDN, in the order Chromium sends an
/// `<img>` request from discord.com (a cross-site, no-cors image fetch).
pub fn image_headers() -> [(&'static str, String); 12] {
    [
        ("sec-ch-ua-platform", sec_ch_ua_platform().to_owned()),
        ("user-agent", user_agent()),
        ("sec-ch-ua", sec_ch_ua()),
        ("sec-ch-ua-mobile", "?0".to_owned()),
        (
            "accept",
            "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8".to_owned(),
        ),
        ("sec-fetch-site", "cross-site".to_owned()),
        ("sec-fetch-mode", "no-cors".to_owned()),
        ("sec-fetch-dest", "image".to_owned()),
        ("referer", "https://discord.com/".to_owned()),
        ("accept-encoding", ACCEPT_ENCODING.to_owned()),
        ("accept-language", "en-US,en;q=0.9".to_owned()),
        ("priority", "i".to_owned()),
    ]
}

/// The same properties for HTTP requests' `X-Super-Properties` header.
pub fn super_properties() -> String {
    STANDARD.encode(properties().to_string())
}

/// The reader's time zone as the web client names it (`X-Discord-Timezone`),
/// or UTC when it can't be told.
pub fn timezone() -> &'static str {
    static ZONE: OnceLock<String> = OnceLock::new();
    ZONE.get_or_init(|| {
        std::env::var("TZ")
            .ok()
            .filter(|z| z.contains('/'))
            .or_else(|| {
                std::fs::read_link("/etc/localtime").ok().and_then(|p| {
                    let p = p.to_string_lossy().into_owned();
                    p.split_once("zoneinfo/").map(|(_, z)| z.to_owned())
                })
            })
            .unwrap_or_else(|| "Etc/UTC".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_apps_version_from_the_update_server() {
        assert!(!learn_client_version(&json!({"name": "not a version"})));
        assert!(
            learn_client_version(&json!({"full": {"host_version": [1, 0, 9300]}})) || CLIENT_VERSION.get().is_some()
        );
    }

    #[test]
    fn reads_the_build_number_from_the_web_page() {
        assert!(!learn_build_number(b"<script>window.GLOBAL_ENV = {}</script>"));
        assert!(!learn_build_number(br#"{"BUILD_NUMBER":""}"#));
        assert!(learn_build_number(br#"GLOBAL_ENV = {"BUILD_NUMBER":"640001","X":1}"#));
        assert_eq!(build_number(), 640001);
    }

    #[test]
    fn launch_signatures_report_no_modifications() {
        let signature = launch_signature().replace('-', "");
        let bits = u128::from_str_radix(&signature, 16).unwrap();
        assert_eq!(bits & 0x0080_1010_0810_0800_2081_0040_0100_0800, 0);
        assert_eq!(launch_id().len(), 36);
        assert_eq!(&launch_id()[14..15], "4");
    }

    #[test]
    fn sec_ch_ua_matches_real_chromium() {
        // Real Chrome headers for these versions.
        let chrome = |v: usize| grease_brands(v, &[("Chromium", v), ("Google Chrome", v)]);
        assert_eq!(
            chrome(120),
            r#""Not_A Brand";v="8", "Chromium";v="120", "Google Chrome";v="120""#
        );
        assert_eq!(
            chrome(131),
            r#""Google Chrome";v="131", "Chromium";v="131", "Not_A Brand";v="24""#
        );
        assert_eq!(
            chrome(138),
            r#""Not)A;Brand";v="8", "Chromium";v="138", "Google Chrome";v="138""#
        );
        assert_eq!(
            grease_brands(138, &[("Chromium", 138)]),
            r#""Not)A;Brand";v="8", "Chromium";v="138""#
        );
        assert!(sec_ch_ua().contains(r#""Chromium";v="152""#));
    }

    #[test]
    fn identities_are_current_windows_11_on_every_os() {
        for _ in 0..200 {
            let i = Identity::random();
            assert!(i.os_version.starts_with("10.0.2") && i.current());
        }
        // A kept Linux identity (or a retired build) is replaced.
        assert!(
            !Identity {
                os_version: "6.17.0-14-generic".into()
            }
            .current()
        );
        assert!(
            !Identity {
                os_version: "10.0.19045".into()
            }
            .current()
        );
        let props = properties();
        assert_eq!(props["os"], "Windows");
        assert!(props.get("distro").is_none() && props.get("window_manager").is_none());
        assert!(user_agent().contains("Windows NT 10.0; Win64; x64"));
        assert_eq!(sec_ch_ua_platform(), "\"Windows\"");
    }

    #[test]
    fn properties_keep_the_desktop_apps_order() {
        let props = properties();
        let keys: Vec<&str> = props.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(&keys[..4], ["os", "browser", "release_channel", "client_version"]);
        assert_eq!(keys.last(), Some(&"client_app_state"));
    }

    #[test]
    fn claims_to_be_the_desktop_app() {
        // Another test may learn a version meanwhile: settle it first.
        let _ = learn_client_version(&json!({"full": {"host_version": [1, 0, 9300]}}));
        let props = properties();
        assert_eq!(props["browser"], "Discord Client");
        assert_eq!(props["browser_version"], ELECTRON);
        assert_eq!(props["has_client_mods"], false);
        let agent = user_agent();
        assert!(agent.contains(&format!("discord/{}", client_version())) && agent.contains("Electron/"));
        assert_eq!(props["browser_user_agent"], agent, "the same version everywhere");
    }
}

/// Discord's hooks for godwit-core (registered at startup).
pub fn hooks() -> godwit_core::hooks::Hooks {
    godwit_core::hooks::Hooks {
        // Discord's pictures load in its desktop app's Chromium: the same one
        // the API is told about, with its headers.
        image_headers: |url| discord_host(url).then(|| image_headers().into_iter().collect()),
        ..Default::default()
    }
}

/// A Discord CDN or API host (not a look-alike such as `evildiscordapp.com`).
pub fn discord_host(url: &str) -> bool {
    let host = url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split(['/', '?', '#', ':']).next())
        .unwrap_or_default()
        .to_ascii_lowercase();
    ["discordapp.com", "discordapp.net", "discord.com"]
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

#[cfg(test)]
mod hook_tests {
    use super::*;

    #[test]
    fn only_discord_hosts_get_its_headers() {
        assert!(discord_host("https://cdn.discordapp.com/avatars/1/a.png"));
        assert!(discord_host("https://media.discordapp.net/x"));
        assert!(!discord_host("https://evildiscordapp.com/x"));
        assert!((hooks().image_headers)("https://cdn.discordapp.com/a.png").is_some());
        assert!((hooks().image_headers)("https://example.com/a.png").is_none());
    }
}
