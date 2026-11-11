// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Godwit's own preferences, in `settings.json` next to the contacts.
//! Nothing here leaves the device.

use std::io;
use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use godwit_core::ids::{AccountId, RoomId};
use godwit_core::network::Network;
use godwit_voice::call::VoiceSettings;
use serde::{Deserialize, Serialize};

/// Muzzle mode's choices in Settings, in seconds (labels in `MUZZLE_*_LABELS`).
pub const MUZZLE_PAUSES: [u64; 5] = [30, 60, 120, 300, 600];
pub const MUZZLE_PAUSE_LABELS: [&str; 5] = ["30 seconds", "1 minute", "2 minutes", "5 minutes", "10 minutes"];
pub const MUZZLE_MAXES: [u64; 5] = [120, 300, 600, 1800, 3600];
pub const MUZZLE_MAX_LABELS: [&str; 5] = ["2 minutes", "5 minutes", "10 minutes", "30 minutes", "1 hour"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Convert completed emoticons in the composer to Unicode emoji.
    pub convert_emoticons: bool,
    pub dark: bool,
    /// The accent the colour scheme is built from (`theme::ACCENTS`; empty: the default).
    pub accent: String,
    /// The colour style's row in `theme::STYLES`.
    pub style: usize,
    /// The interface scale on top of the display's (1: as the display says; see `scale`).
    pub ui_scale: f32,
    /// Clock style for every time shown (see `clock`).
    pub time_format: crate::clock::TimeFormat,
    /// Numeric date order for message times (see `clock`).
    pub date_format: crate::clock::DateFormat,
    /// Start Godwit when the user signs in to the computer (see `autostart`).
    pub autostart: bool,
    /// With `autostart`: show the window minimized at that start.
    pub start_minimized: bool,
    /// The Unicode emoji pack (an id from `godwit_core::emoji_packs::PACKS`).
    pub emoji_pack: String,
    pub notifications: bool,
    /// Message, ringing and call join/leave sounds.
    pub sounds: bool,
    /// Whether new-message sounds play when sounds are enabled.
    pub message_sound: bool,
    /// Whether incoming calls ring when sounds are enabled.
    pub call_ring_sound: bool,
    /// Whether call join/leave cues play when sounds are enabled.
    pub call_event_sounds: bool,
    /// Muzzle mode: a burst of messages from one sender makes noise once,
    /// the rest come in quietly until they pause (see `alerts`).
    pub muzzle: bool,
    /// Muzzle mode's times, in seconds: a pause this long makes the next
    /// message noisy again, and someone who never pauses is heard at most
    /// once per `muzzle_max_secs`.
    pub muzzle_pause_secs: u64,
    pub muzzle_max_secs: u64,
    /// Chats (`network:user|room`) muzzled whatever the default says, and
    /// chats never muzzled: a person's DM, a channel or a group.
    pub muzzle_rooms: Vec<String>,
    pub unmuzzle_rooms: Vec<String>,
    /// Chats whose notifications differ from their network's own rules:
    /// "all" (every message) or "off" (none). Absent: the network decides.
    pub room_alerts: BTreeMap<String, String>,
    /// Do not disturb: no notifications, sounds or ringing from any account
    /// (messages and calls still arrive and show in the window).
    pub dnd: bool,
    /// Each account's alerts (`network:user` → mode); missing: the network's default.
    pub alert_modes: BTreeMap<String, AlertMode>,
    /// Accounts taken offline: signed in, but not connected, so nothing
    /// comes through until they're switched back on (`network:user`).
    pub offline: Vec<String>,
    pub voice: VoiceSettings,
    /// Accounts whose rail section is collapsed (`network:user`).
    pub collapsed: Vec<String>,
    /// Discord accounts already asked the first-run privacy question.
    pub privacy_asked: Vec<String>,
    /// Accounts already told that Godwit stays quiet while their network's
    /// own app runs here (`network:user`): told once, on the first alert it kept quiet.
    pub quiet_told: Vec<String>,
    /// Panel widths in logical pixels, as dragged (0: the default).
    pub sidebar_width: f32,
    pub members_width: f32,
    /// The new-message sound: its row in `godwit_voice::sounds::Tone::ALL`.
    pub message_tone: usize,
    /// The version of Godwit the network debug log was switched on in
    /// (empty: off). Another version, after an update, switches it off.
    pub net_debug_log: String,
    /// Media diagnostics are opt-in and are reset after each app update.
    pub media_debug_log: String,
    /// The emoji picker's recently used, newest first (an emoji's reaction:
    /// the Unicode glyph, or a custom emoji's id).
    pub recent_emoji: Vec<String>,
    /// Home's pinned and hidden chats (`network:user|room`).
    pub pinned_rooms: Vec<String>,
    pub hidden_rooms: Vec<String>,
    /// Accounts whose notes chat was pinned once already (`network:user`),
    /// so an unpinned one stays unpinned.
    pub pins_seeded: Vec<String>,
    /// The rail's servers in the user's order (`network:user|server`); ones
    /// not listed keep their network's order after these.
    pub server_order: Vec<String>,
    /// The rail's accounts in the user's order (`network:user`); ones not
    /// listed follow in the order they signed in.
    pub account_order: Vec<String>,
    /// Accounts their network signed out (not us): the rail asks to sign
    /// in again until they do, so they don't just disappear.
    pub sign_in_needed: Vec<SignInNeeded>,
    /// Collapsed channel categories (`network:user|server#title`).
    pub collapsed_categories: Vec<String>,
    /// The call we're in (`network:user|room`) while it's connected. Left
    /// set when Godwit stops during it, so the next start offers to rejoin
    /// (a normal quit keeps or clears it as `rejoin_offer` says).
    pub last_call: Option<String>,
    /// When the next start offers to rejoin the call Godwit stopped in.
    pub rejoin_offer: RejoinOffer,
    /// Our own mute and deafen (the account card's buttons), kept between
    /// calls and restarts, as Discord keeps them.
    pub self_muted: bool,
    pub self_deafened: bool,
    /// Starred GIFs, newest first: the picker's Favourites. Kept here only.
    pub gif_favourites: Vec<FavouriteGif>,
    /// Network-scoped people starred in the local People view.
    pub person_favourites: Vec<String>,
    /// Profiles: named sets of accounts with their own view (see `profiles`).
    /// The current profile's view lives in the top-level fields above.
    pub profiles: Vec<crate::profiles::Profile>,
    pub default_profile: u32,
    /// The next id to give a new profile, never reused: a recreated id
    /// would inherit a deleted profile's contacts file.
    pub next_profile_id: u32,
    /// Accounts seen signed in, to start a profile's accounts later.
    pub known_accounts: Vec<crate::profiles::KnownAccount>,
    /// Switching profiles asks first (the dialog's "Don't ask again" turns
    /// it off; Settings → Profiles turns it back on). A call still asks.
    pub ask_profile_switch: bool,
    /// Soft lock: the other profiles' accounts stay connected in the
    /// background (see `background`), so their notifications still come and
    /// a switch is instant. Off: a switch disconnects them, as before.
    pub background_profiles: bool,
    /// Background profiles' notifications show who and what; off, only
    /// the profile's name and the kind of alert.
    pub background_previews: bool,
    /// The profile in use; not saved (each start opens the default one).
    #[serde(skip)]
    pub current_profile: u32,
}

/// A starred GIF: what sending it posts, and the looping picture to show.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavouriteGif {
    /// Its page link, as the picker sends GIFs.
    pub url: String,
    /// The animated picture (WebM, GIF) the picker shows.
    pub picture: String,
}

/// Favourites kept at most; the oldest go first.
pub const MAX_GIF_FAVOURITES: usize = 500;

impl Default for Settings {
    fn default() -> Self {
        Self {
            convert_emoticons: true,
            dark: true,
            accent: String::new(),
            style: 0,
            ui_scale: 1.0,
            time_format: crate::clock::TimeFormat::default(),
            date_format: crate::clock::DateFormat::default(),
            autostart: false,
            start_minimized: false,
            emoji_pack: "twemoji".to_owned(),
            notifications: true,
            sounds: true,
            message_sound: true,
            call_ring_sound: true,
            call_event_sounds: true,
            // Off unless asked for: quieter alerts would be unexpected.
            muzzle: false,
            muzzle_pause_secs: MUZZLE_PAUSES[1],
            muzzle_max_secs: MUZZLE_MAXES[1],
            muzzle_rooms: Vec::new(),
            unmuzzle_rooms: Vec::new(),
            room_alerts: BTreeMap::new(),
            dnd: false,
            alert_modes: BTreeMap::new(),
            offline: Vec::new(),
            voice: VoiceSettings::default(),
            collapsed: Vec::new(),
            privacy_asked: Vec::new(),
            quiet_told: Vec::new(),
            sidebar_width: 0.0,
            members_width: 0.0,
            message_tone: 0,
            net_debug_log: String::new(),
            media_debug_log: String::new(),
            recent_emoji: Vec::new(),
            pinned_rooms: Vec::new(),
            pins_seeded: Vec::new(),
            hidden_rooms: Vec::new(),
            server_order: Vec::new(),
            account_order: Vec::new(),
            sign_in_needed: Vec::new(),
            collapsed_categories: Vec::new(),
            last_call: None,
            rejoin_offer: RejoinOffer::default(),
            self_muted: false,
            self_deafened: false,
            gif_favourites: Vec::new(),
            person_favourites: Vec::new(),
            profiles: Vec::new(),
            default_profile: 0,
            next_profile_id: 0,
            known_accounts: Vec::new(),
            ask_profile_switch: true,
            background_profiles: false,
            background_previews: false,
            current_profile: 0,
        }
    }
}

/// When the next start offers to rejoin the call Godwit stopped in
/// (Settings › Voice, in this order).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejoinOffer {
    /// After any stop during a call: quitting with the window, or a crash.
    #[default]
    EveryQuit,
    /// Only when Godwit didn't close normally.
    AfterCrash,
    Never,
}

impl RejoinOffer {
    pub const ALL: [(Self, &'static str); 3] = [
        (Self::EveryQuit, "Every time Godwit closes"),
        (Self::AfterCrash, "Only after a crash"),
        (Self::Never, "Never"),
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|(o, _)| *o == self).unwrap_or(0)
    }

    pub fn at(index: i32) -> Option<Self> {
        usize::try_from(index)
            .ok()
            .and_then(|i| Self::ALL.get(i))
            .map(|(o, _)| *o)
    }
}

/// How an account alerts (notifications, sounds, ringing).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertMode {
    /// Godwit notifies and rings.
    Normal,
    /// Quiet while the network's own app runs on this computer: many people
    /// keep it running, and both alerting means every message and call twice.
    /// Only where that app can be detected ([`detects_official_app`]).
    OfficialApp,
    /// Never alerts (messages and calls still show in the window).
    Muted,
}

impl AlertMode {
    /// What an account of `network` does until the user chooses.
    pub fn default_for(network: Network) -> Self {
        if detects_official_app(network) {
            Self::OfficialApp
        } else {
            Self::Normal
        }
    }
}

/// The alert modes offered for an account of `network`, in the list's order.
pub fn alert_choices(network: Network) -> Vec<AlertMode> {
    if detects_official_app(network) {
        vec![AlertMode::Normal, AlertMode::OfficialApp, AlertMode::Muted]
    } else {
        vec![AlertMode::Normal, AlertMode::Muted]
    }
}

impl AlertMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Notify",
            Self::OfficialApp => "Quiet while the official app runs here",
            Self::Muted => "Muted",
        }
    }
}

/// Whether Godwit can tell that `network`'s own app is running.
pub fn detects_official_app(network: Network) -> bool {
    network.official_app().is_some()
}

impl Settings {
    /// The accent to build the colour scheme from.
    pub fn accent(&self) -> &str {
        if self.accent.is_empty() {
            crate::theme::DEFAULT_ACCENT
        } else {
            &self.accent
        }
    }

    pub fn alert_mode(&self, account: &AccountId) -> AlertMode {
        self.alert_modes
            .get(&account_key(account))
            .copied()
            .unwrap_or_else(|| AlertMode::default_for(account.network))
    }

    pub fn set_alert_mode(&mut self, account: &AccountId, mode: AlertMode) {
        if mode == AlertMode::default_for(account.network) {
            self.alert_modes.remove(&account_key(account));
        } else {
            self.alert_modes.insert(account_key(account), mode);
        }
    }

    /// Whether a Discord account was asked the first-run privacy question.
    pub fn privacy_asked(&self, account: &AccountId) -> bool {
        self.privacy_asked.contains(&account_key(account))
    }

    pub fn quiet_told(&self, account: &AccountId) -> bool {
        self.quiet_told.contains(&account_key(account))
    }

    pub fn mark_quiet_told(&mut self, account: &AccountId) {
        let key = account_key(account);
        if !self.quiet_told.contains(&key) {
            self.quiet_told.push(key);
        }
    }

    pub fn mark_privacy_asked(&mut self, account: &AccountId) {
        let key = account_key(account);
        if !self.privacy_asked.contains(&key) {
            self.privacy_asked.push(key);
        }
    }

    pub fn is_offline(&self, account: &AccountId) -> bool {
        self.offline.contains(&account_key(account))
    }

    pub fn set_offline(&mut self, account: &AccountId, offline: bool) {
        let key = account_key(account);
        self.offline.retain(|k| *k != key);
        if offline {
            self.offline.push(key);
        }
    }
}

/// An account as kept in the settings file: `network:user`.
impl Settings {
    /// Switches the network debug log off when it was switched on in
    /// another version than `version`; returns whether it did.
    pub fn expire_net_debug_log(&mut self, version: &str) -> bool {
        if self.net_debug_log.is_empty() || self.net_debug_log == version {
            return false;
        }
        self.net_debug_log.clear();
        true
    }

    pub fn expire_media_debug_log(&mut self, version: &str) -> bool {
        if self.media_debug_log.is_empty() || self.media_debug_log == version {
            return false;
        }
        self.media_debug_log.clear();
        true
    }

    /// Starts or stops the network debug log as these settings say.
    pub fn apply_net_debug_log(&self) {
        let dir = if self.net_debug_log.is_empty() {
            None
        } else {
            net_log_dir()
        };
        godwit_core::net::log::set(dir.as_deref());
    }

    pub fn apply_media_debug_log(&self) {
        let dir = (!self.media_debug_log.is_empty()).then(net_log_dir).flatten();
        crate::media_log::set(dir.as_deref());
    }
}

/// Where the network debug log is written: Godwit's cache folder.
pub fn net_log_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "godwit").map(|d| d.cache_dir().to_path_buf())
}

pub fn account_key(id: &AccountId) -> String {
    format!("{}:{}", id.network.key(), id.user)
}

/// A server as kept in settings.
pub fn server_key(server: &godwit_core::ids::ServerId) -> String {
    format!("{}|{}", account_key(&server.account), server.raw)
}

/// A room as kept in settings: its account's key and its own id.
/// An account to sign in to again (see `Settings::sign_in_needed`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignInNeeded {
    /// `network:user`.
    pub account: String,
    /// The network's key (`slack`, `gchat`, …).
    pub network: String,
    pub name: String,
    /// What the network said.
    pub reason: String,
}

/// A chat's own notification choice (the chat menu's Notifications row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomAlerts {
    /// The network's own rules (mentions, its mute and level settings).
    Default,
    /// Every message from someone else.
    All,
    /// None at all.
    Off,
}

impl Settings {
    pub fn room_alerts(&self, room: &RoomId) -> RoomAlerts {
        match self.room_alerts.get(&room_key(room)).map(String::as_str) {
            Some("all") => RoomAlerts::All,
            Some("off") => RoomAlerts::Off,
            _ => RoomAlerts::Default,
        }
    }

    /// Default → All → Off → Default.
    pub fn cycle_room_alerts(&mut self, room: &RoomId) {
        let key = room_key(room);
        match self.room_alerts(room) {
            RoomAlerts::Default => self.room_alerts.insert(key, "all".into()),
            RoomAlerts::All => self.room_alerts.insert(key, "off".into()),
            RoomAlerts::Off => self.room_alerts.remove(&key),
        };
    }

    /// Whether muzzle mode applies to `room`: its own choice, or the default.
    pub fn muzzled(&self, room: &RoomId) -> bool {
        let key = room_key(room);
        if self.muzzle_rooms.contains(&key) {
            true
        } else if self.unmuzzle_rooms.contains(&key) {
            false
        } else {
            self.muzzle
        }
    }
}

pub fn room_key(room: &RoomId) -> String {
    format!("{}|{}", account_key(&room.account), room.raw)
}

pub fn parse_room_key(key: &str) -> Option<RoomId> {
    let (account, raw) = key.split_once('|')?;
    Some(RoomId::new(parse_account_key(account)?, raw))
}

pub fn parse_account_key(key: &str) -> Option<AccountId> {
    let (network, user) = key.split_once(':')?;
    let network = Network::ALL.into_iter().find(|n| n.key() == network)?;
    Some(AccountId::new(network, user))
}

pub fn path() -> Option<PathBuf> {
    Some(godwit_core::config_dir::get()?.join("settings.json"))
}

/// The saved settings; defaults if there's no file yet.
pub fn load(path: &Path) -> io::Result<Settings> {
    let mut settings = match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Settings::default(),
        Err(e) => return Err(e),
    };
    settings.migrate_profiles();
    Ok(settings)
}

/// Written to a temporary file and renamed over the old one.
pub fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // The live view goes into its profile first; saving is rare, so a copy is fine.
    let mut stored = settings.clone();
    stored.sync_view();
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&stored).map_err(io::Error::other)?)?;
    std::fs::rename(&tmp, path)
}

/// This process's own memory, for the About page: its private bytes
/// (Task Manager's "Commit size").
#[cfg(windows)]
pub fn memory_in_use() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // Private bytes: memory only this process holds, as Linux's RssAnon.
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>()).ok()?,
        ..Default::default()
    };
    // SAFETY: the pseudo-handle needs no closing; the struct is the size
    // `cb` says, and the _EX form starts with the plain one.
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
    };
    (ok != 0).then(|| u64::try_from(counters.PrivateUsage).ok()).flatten()
}

/// Memory this process holds itself (RssAnon); None where there's no
/// `/proc` (macOS).
#[cfg(not(windows))]
pub fn memory_in_use() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("RssAnon:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_chats_notifications_cycle_and_survive_saving() {
        let room = RoomId::new(AccountId::new(Network::Slack, "T1:U1"), "T1:C1");
        let mut s = Settings::default();
        assert_eq!(s.room_alerts(&room), RoomAlerts::Default);
        s.cycle_room_alerts(&room);
        assert_eq!(s.room_alerts(&room), RoomAlerts::All);
        let saved: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(saved.room_alerts(&room), RoomAlerts::All);
        s.cycle_room_alerts(&room);
        assert_eq!(s.room_alerts(&room), RoomAlerts::Off);
        s.cycle_room_alerts(&room);
        assert_eq!(s.room_alerts(&room), RoomAlerts::Default);
        assert!(s.room_alerts.is_empty(), "the default isn't stored");
    }

    use super::*;

    #[test]
    fn round_trips_and_fills_in_missing_fields() {
        let dir = std::env::temp_dir().join(format!("godwit-settings-test-{}", std::process::id()));
        let path = dir.join("settings.json");
        // Loading always leaves one profile (see `migrate_profiles`).
        let mut fresh = Settings::default();
        fresh.migrate_profiles();
        assert_eq!(load(&path).unwrap(), fresh);
        let mut s = Settings {
            dark: false,
            ..Settings::default()
        };
        s.migrate_profiles();
        save(&path, &s).unwrap();
        assert_eq!(load(&path).unwrap(), s);
        std::fs::write(&path, br#"{"notifications": false}"#).unwrap();
        let partial = load(&path).unwrap();
        assert!(
            !partial.notifications && partial.dark,
            "missing fields take their defaults"
        );
        assert_eq!(
            partial.time_format,
            crate::clock::TimeFormat::Hour24,
            "old files keep today's look"
        );
        assert_eq!(partial.date_format, crate::clock::DateFormat::DayFirst);
        assert!(!partial.autostart && !partial.start_minimized);
        let on = Settings {
            autostart: true,
            start_minimized: true,
            ..Settings::default()
        };
        save(&path, &on).unwrap();
        let back = load(&path).unwrap();
        assert!(back.autostart && back.start_minimized);
        std::fs::write(&path, br#"{"notifications": false}"#).unwrap();
        assert!(partial.message_sound && partial.call_ring_sound && partial.call_event_sounds);
        let mut changed = Settings {
            message_sound: false,
            call_ring_sound: false,
            call_event_sounds: false,
            ..Settings::default()
        };
        changed.migrate_profiles();
        save(&path, &changed).unwrap();
        let loaded = load(&path).unwrap();
        assert!(!loaded.message_sound && !loaded.call_ring_sound && !loaded.call_event_sounds);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
