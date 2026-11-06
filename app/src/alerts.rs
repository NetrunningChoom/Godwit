// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Telling the user about things outside the window: desktop notifications
//! and sounds for [`Event::Alert`](godwit_core::application::Event::Alert)s,
//! the ringtone while a call rings, and join/leave sounds in calls.
//!
//! Networks decide *what* is worth an alert (their own mutes and levels);
//! this decides *whether and how*, from the app's settings: Do Not Disturb
//! silences everything, and each account is Normal, Muted, or quiet while its
//! network's own app runs on this computer (it alerts for the same things).

use std::sync::mpsc;
use std::time::{Duration, Instant};

use godwit_core::application::AlertKind;
use godwit_core::ids::{RoomId, UserId};
use godwit_core::network::{Network, OfficialApp};
use godwit_voice::sounds::{self, Playing, Sound};

use crate::settings::{AlertMode, Settings};

/// A selected conversation only suppresses alerts while its window has focus.
pub(crate) fn notify_message(
    ours: bool,
    system: bool,
    pending: bool,
    selected: bool,
    focused: bool,
    dnd: bool,
) -> bool {
    // Teams and Google Chat keep their own Do Not Disturb (their DND is our
    // Focus presence): the official clients stay quiet then, so do we.
    !dnd && !ours && !system && !pending && !(selected && focused)
}

/// How long "is the network's app running?" is trusted before looking again.
const APP_CHECK_EVERY: Duration = Duration::from_secs(30);
/// Sounds play a little under the call volume: they're cues, not content.
const SOUND_VOLUME: f32 = 0.7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Allowed {
    Yes,
    No,
    /// Kept quiet because the network's own app runs here.
    OfficialApp,
}

/// Wakes the UI thread (after a notification was clicked).
pub type Wake = std::sync::Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
pub struct Alerts {
    /// Rooms whose notification was clicked, from the notification
    /// threads, and how those threads wake the UI to open them.
    clicked: Option<(mpsc::Sender<RoomId>, mpsc::Receiver<RoomId>, Wake)>,
    /// The call ringing now, and its ringtone (stops when dropped).
    ringing: Option<(RoomId, Option<Playing>)>,
    /// One-shot sounds still playing (kept until they've played out).
    playing: Vec<(Instant, Playing)>,
    /// Per network with a detectable app: when we last looked, and whether it ran.
    official_apps: std::collections::HashMap<Network, (Instant, bool)>,
    /// Muzzle mode, per room and sender: when their last alert made noise,
    /// and when their last one came.
    muzzled: std::collections::HashMap<(RoomId, UserId), (Instant, Instant)>,
}

impl Alerts {
    /// Lets clicked notifications open their chat: `wake` runs the UI's
    /// pump, which takes them with [`Alerts::take_clicked`].
    pub fn set_wake(&mut self, wake: Wake) {
        let (tx, rx) = mpsc::channel();
        self.clicked = Some((tx, rx, wake));
    }

    /// The room of the latest clicked notification, if any.
    pub fn take_clicked(&mut self) -> Option<RoomId> {
        let (_, rx, _) = self.clicked.as_ref()?;
        rx.try_iter().last()
    }

    /// An alert from a network: a notification and a sound, as `settings`
    /// allow. Returns `true` when it was kept quiet only because the
    /// network's own app runs here (the account's mode "Quiet while Steam runs").
    pub fn alert(
        &mut self,
        settings: &Settings,
        room: &RoomId,
        kind: AlertKind,
        from: Option<&UserId>,
        title: &str,
        body: &str,
    ) -> bool {
        let allowed = self.allowed(settings, room);
        // Where an alert went, for `--verbose`: never who or what.
        godwit_core::verbose!(
            "note: alert ({kind:?}, {}): {allowed:?}, notifications {}, sounds {}",
            room.network().name(),
            settings.notifications,
            settings.sounds
        );
        match allowed {
            Allowed::Yes => {}
            Allowed::No => return false,
            Allowed::OfficialApp => return true,
        }
        let quiet = kind == AlertKind::Message
            && settings.muzzled(room)
            && from.is_some_and(|from| {
                let pause = Duration::from_secs(settings.muzzle_pause_secs.max(1));
                let max = Duration::from_secs(settings.muzzle_max_secs.max(1));
                self.muzzle(room, from, Instant::now(), pause, max)
            });
        if settings.notifications {
            let opened = self
                .clicked
                .as_ref()
                .map(|(tx, _, wake)| (tx.clone(), wake.clone(), room.clone()));
            notify(title, body, opened, quiet);
        }
        match kind {
            AlertKind::Message if quiet => {}
            AlertKind::Message => {
                let tone = godwit_voice::sounds::Tone::ALL
                    .get(settings.message_tone)
                    .map_or(godwit_voice::sounds::Tone::Marimba, |(t, _)| *t);
                self.sound(settings, Sound::MessageTone(tone));
            }
            AlertKind::Call => {
                let tone = settings
                    .sounds
                    .then(|| sounds::play(Sound::Ring, settings.voice.output_device.clone(), SOUND_VOLUME))
                    .and_then(|r| r.map_err(|e| eprintln!("error: ringing: {e:#}")).ok());
                self.ringing = Some((room.clone(), tone));
            }
        }
        false
    }

    /// `room`'s call stopped ringing (answered, declined or gone).
    pub fn done(&mut self, room: &RoomId) {
        if self.ringing.as_ref().is_some_and(|(r, _)| r == room) {
            self.ringing = None;
        }
    }

    /// Whether this message alert from `from` in `room` comes in quietly
    /// (muzzle mode): yes while they keep writing after a noisy one, until
    /// they pause for `pause` or `max` has passed since the noisy one (both
    /// set in Settings).
    fn muzzle(&mut self, room: &RoomId, from: &UserId, now: Instant, pause: Duration, max: Duration) -> bool {
        // Forget senders who went quiet long ago (bounded memory).
        let forget = pause.max(max);
        self.muzzled
            .retain(|_, (_, seen)| now.saturating_duration_since(*seen) < forget);
        let key = (room.clone(), from.clone());
        let quiet = self.muzzled.get(&key).is_some_and(|(noisy, seen)| {
            now.saturating_duration_since(*seen) < pause && now.saturating_duration_since(*noisy) < max
        });
        let entry = self.muzzled.entry(key).or_insert((now, now));
        entry.1 = now;
        if !quiet {
            entry.0 = now;
        }
        quiet
    }

    /// A one-shot cue (a call joined or left), if sounds are on.
    pub fn sound(&mut self, settings: &Settings, sound: Sound) {
        let now = Instant::now();
        self.playing.retain(|(until, _)| *until > now);
        if !settings.sounds {
            return;
        }
        match sounds::play(sound, settings.voice.output_device.clone(), SOUND_VOLUME) {
            Ok(playing) => {
                godwit_core::verbose!("note: sound started");
                self.playing.push((now + Duration::from_secs(3), playing));
            }
            Err(e) => eprintln!("error: playing a sound: {e:#}"),
        }
    }

    /// Whether `room`'s account may alert now.
    fn allowed(&mut self, settings: &Settings, room: &RoomId) -> Allowed {
        if settings.dnd {
            return Allowed::No;
        }
        match settings.alert_mode(&room.account) {
            AlertMode::Normal => Allowed::Yes,
            AlertMode::Muted => Allowed::No,
            AlertMode::OfficialApp if self.official_app_running(room.network()) => Allowed::OfficialApp,
            AlertMode::OfficialApp => Allowed::Yes,
        }
    }

    /// Whether `network`'s own app runs here (where it can be told).
    fn official_app_running(&mut self, network: Network) -> bool {
        let Some(app) = network.official_app() else {
            return false;
        };
        let now = Instant::now();
        match self.official_apps.get(&network) {
            Some(&(at, running)) if now.duration_since(at) < APP_CHECK_EVERY => running,
            _ => {
                let running = app_running(&app);
                self.official_apps.insert(network, (now, running));
                running
            }
        }
    }
}

/// Whether this desktop lists a low-urgency notification without a banner
/// (GNOME Shell does; Plasma shows low-priority popups by default).
fn lists_quietly() -> bool {
    // Windows and macOS have no such variable, so they never do.
    desktop_lists_quietly(std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref())
}

/// `XDG_CURRENT_DESKTOP` is a colon-separated list ("ubuntu:GNOME").
fn desktop_lists_quietly(desktop: Option<&str>) -> bool {
    desktop.is_some_and(|d| d.split(':').any(|name| name.eq_ignore_ascii_case("GNOME")))
}

/// A desktop notification, off the UI thread (D-Bus on Linux can block
/// briefly). Clicking it opens its chat, as the official clients do: the
/// thread waits for the click (or for the notification to close), then
/// hands `room` to the UI. Only the freedesktop backend reports clicks.
/// `quiet` (muzzle mode): no banner, straight to the notification list, so
/// it's there to read without popping up.
fn notify(title: &str, body: &str, opened: Option<(mpsc::Sender<RoomId>, Wake, RoomId)>, quiet: bool) {
    // Only GNOME keeps a low-urgency notification out of banners. KDE Plasma
    // (by default) and Windows pop it up like any other, so a muzzled alert
    // shows nothing there: the chat's unread badge still counts it.
    if quiet && !lists_quietly() {
        godwit_core::verbose!("note: muzzled alert, no list-only notifications here");
        return;
    }
    let (title, body) = (title.to_owned(), body.to_owned());
    let shown = std::thread::Builder::new()
        .name("notify".into())
        // It mostly waits; a small stack keeps many open ones cheap.
        .stack_size(256 * 1024)
        .spawn(move || {
            let mut notification = notify_rust::Notification::new();
            notification.appname("Godwit").summary(&title).body(&body);
            // GNOME shows a notification under the app whose desktop entry
            // it names, with that entry's icon; without the hint it's an
            // anonymous one. Godwit plays its own tones, so the shell's sound
            // stays off (no double sound).
            #[cfg(all(unix, not(target_os = "macos")))]
            notification
                .icon("godwit")
                .hint(notify_rust::Hint::DesktopEntry("godwit".into()))
                .hint(notify_rust::Hint::Category("im.received".into()))
                .hint(notify_rust::Hint::SuppressSound(true));
            // GNOME shows low-urgency notifications in its list without a banner.
            #[cfg(all(unix, not(target_os = "macos")))]
            if quiet {
                notification.urgency(notify_rust::Urgency::Low);
            }
            // Windows names the toast, and shows its icon, from this id's
            // registry entry (win32.rs); without it the toast says "Windows
            // PowerShell". Toasts there are silent unless given a sound.
            #[cfg(windows)]
            notification.app_id(crate::win32::APP_ID);
            // Windows: no list-only toast through notify-rust, so a quiet one
            // still shows its banner.
            #[cfg(not(all(unix, not(target_os = "macos"))))]
            let _ = quiet;
            #[cfg(all(unix, not(target_os = "macos")))]
            if opened.is_some() {
                notification.action("default", "Open");
            }
            match notification.show() {
                Ok(_handle) => {
                    #[cfg(all(unix, not(target_os = "macos")))]
                    if let Some((tx, wake, room)) = opened {
                        _handle.wait_for_action(|action| {
                            if action == "default" && tx.send(room).is_ok() {
                                wake();
                            }
                        });
                    }
                    // A click on the toast's body arrives as `Default`;
                    // `wait_for_action` would report it as closed.
                    #[cfg(windows)]
                    if let Some((tx, wake, room)) = opened {
                        let _ = _handle.wait_for_response(move |response: &notify_rust::NotificationResponse| {
                            if matches!(response, notify_rust::NotificationResponse::Default) && tx.send(room).is_ok() {
                                wake();
                            }
                        });
                    }
                    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
                    drop(opened);
                }
                Err(e) => eprintln!("error: showing notification: {e}"),
            }
        });
    if let Err(e) = shown {
        eprintln!("error: showing notification: {e}");
    }
}

/// Whether a process named as `app` declares runs on this computer.
#[cfg(target_os = "linux")]
fn app_running(app: &OfficialApp) -> bool {
    // Every process's name, in /proc/<pid>/comm.
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return false;
    };
    dir.flatten()
        .filter(|e| e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|e| std::fs::read_to_string(e.path().join("comm")).ok())
        .any(|name| is_app_process(app.linux, name.trim()))
}

#[cfg(target_os = "windows")]
fn app_running(app: &OfficialApp) -> bool {
    app.windows.iter().any(|image| {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("IMAGENAME eq {image}"), "/NH"])
            .output()
            .is_ok_and(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .to_ascii_lowercase()
                    .contains(&image.to_ascii_lowercase())
            })
    })
}

#[cfg(target_os = "macos")]
fn app_running(app: &OfficialApp) -> bool {
    app.macos.iter().any(|name| {
        std::process::Command::new("pgrep")
            .args(["-x", name])
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn app_running(_: &OfficialApp) -> bool {
    false
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_app_process(names: &[&str], name: &str) -> bool {
    names.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_selected_chat_still_notifies() {
        assert!(notify_message(false, false, false, true, false, false));
        assert!(!notify_message(false, false, false, true, true, false));
        assert!(notify_message(false, false, false, false, true, false));
        assert!(notify_message(false, false, false, false, false, false));
        // Do Not Disturb keeps everything quiet.
        assert!(!notify_message(false, false, false, false, false, true));
        for (ours, system, pending) in [(true, false, false), (false, true, false), (false, false, true)] {
            assert!(!notify_message(ours, system, pending, true, false, false));
        }
    }

    #[test]
    fn muzzle_mode_quiets_a_burst_from_one_sender_but_not_for_long() {
        let mut alerts = Alerts::default();
        let account = godwit_core::ids::AccountId::new(Network::Discord, "me");
        let room = RoomId::new(account.clone(), "r");
        let (ann, bob) = (UserId::new(account.clone(), "ann"), UserId::new(account, "bob"));
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        const MIN: Duration = Duration::from_secs(60);
        const FIVE: Duration = Duration::from_secs(300);
        assert!(
            !alerts.muzzle(&room, &ann, at(0), MIN, FIVE),
            "the first one makes noise"
        );
        assert!(
            alerts.muzzle(&room, &ann, at(10), MIN, FIVE),
            "a burst comes in quietly"
        );
        assert!(
            !alerts.muzzle(&room, &bob, at(11), MIN, FIVE),
            "someone else still makes noise"
        );
        assert!(
            !alerts.muzzle(&room, &ann, at(80), MIN, FIVE),
            "after a minute's pause, noise again"
        );
        // Writing nonstop (every 30 s): noise again once 5 minutes have passed.
        let noisy: Vec<u64> = (1..=12)
            .map(|n| 80 + n * 30)
            .filter(|&s| !alerts.muzzle(&room, &ann, at(s), MIN, FIVE))
            .collect();
        assert_eq!(noisy, [380]);
    }

    #[test]
    fn only_gnome_lists_muzzled_alerts_without_a_banner() {
        assert!(desktop_lists_quietly(Some("GNOME")));
        assert!(desktop_lists_quietly(Some("ubuntu:GNOME")));
        assert!(!desktop_lists_quietly(Some("KDE")));
        assert!(!desktop_lists_quietly(None));
    }

    #[test]
    fn muzzle_mode_follows_the_chosen_times() {
        let mut alerts = Alerts::default();
        let account = godwit_core::ids::AccountId::new(Network::Discord, "me");
        let room = RoomId::new(account.clone(), "r");
        let ann = UserId::new(account, "ann");
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let (pause, max) = (Duration::from_secs(30), Duration::from_secs(120));
        assert!(!alerts.muzzle(&room, &ann, at(0), pause, max));
        assert!(alerts.muzzle(&room, &ann, at(20), pause, max), "within the 30 s pause");
        assert!(
            !alerts.muzzle(&room, &ann, at(60), pause, max),
            "a 40 s pause is long enough"
        );
        // Nonstop every 20 s: noisy again once 2 minutes have passed.
        let noisy: Vec<u64> = (1..=8)
            .map(|n| 60 + n * 20)
            .filter(|&s| !alerts.muzzle(&room, &ann, at(s), pause, max))
            .collect();
        assert_eq!(noisy, [180]);
    }
    use godwit_core::ids::AccountId;

    fn quiet() -> Settings {
        Settings {
            notifications: false,
            sounds: false,
            ..Settings::default()
        }
    }

    fn room(network: Network, user: &str) -> RoomId {
        RoomId::new(AccountId::new(network, user), "1")
    }

    #[test]
    fn dnd_and_each_accounts_mode_decide() {
        let mut alerts = Alerts::default();
        let mut settings = quiet();
        let stoat = room(Network::Stoat, "a");
        let other = room(Network::Stoat, "b");
        assert_eq!(alerts.allowed(&settings, &stoat), Allowed::Yes, "normal by default");
        settings.set_alert_mode(&stoat.account, AlertMode::Muted);
        assert_eq!(alerts.allowed(&settings, &stoat), Allowed::No);
        assert_eq!(
            alerts.allowed(&settings, &other),
            Allowed::Yes,
            "per account, not per network"
        );
        settings.dnd = true;
        assert_eq!(alerts.allowed(&settings, &other), Allowed::No);
        // The default isn't stored.
        settings.set_alert_mode(&stoat.account, AlertMode::Normal);
        assert!(settings.alert_modes.is_empty());
        assert_eq!(
            settings.alert_mode(&room(Network::Steam, "s").account),
            AlertMode::OfficialApp
        );
    }

    #[test]
    fn ringing_stops_for_its_own_room_only() {
        let mut alerts = Alerts::default();
        let mut settings = quiet();
        let room = RoomId::new(AccountId::new(Network::Steam, "me"), "1");
        let other = RoomId::new(AccountId::new(Network::Steam, "me"), "2");
        settings.set_alert_mode(&room.account, AlertMode::Normal);
        alerts.alert(
            &settings,
            &room,
            AlertKind::Call,
            None,
            "Friend",
            "Calling you on Steam",
        );
        assert!(alerts.ringing.is_some());
        alerts.done(&other);
        assert!(alerts.ringing.is_some());
        alerts.done(&room);
        assert!(alerts.ringing.is_none());
    }

    #[test]
    fn says_when_only_the_steam_app_kept_it_quiet() {
        let mut alerts = Alerts {
            official_apps: std::collections::HashMap::from([(Network::Steam, (Instant::now(), true))]),
            ..Alerts::default()
        };
        let mut settings = quiet();
        let room = RoomId::new(AccountId::new(Network::Steam, "me"), "1");
        assert!(alerts.alert(
            &settings,
            &room,
            AlertKind::Call,
            None,
            "Friend",
            "Calling you on Steam"
        ));
        assert!(alerts.ringing.is_none());
        settings.set_alert_mode(&room.account, AlertMode::Muted);
        assert!(!alerts.alert(
            &settings,
            &room,
            AlertKind::Call,
            None,
            "Friend",
            "Calling you on Steam"
        ));
    }

    #[test]
    fn a_muted_account_does_not_ring() {
        let mut alerts = Alerts::default();
        let mut settings = quiet();
        let room = RoomId::new(AccountId::new(Network::Steam, "me"), "1");
        settings.set_alert_mode(&room.account, AlertMode::Muted);
        alerts.alert(
            &settings,
            &room,
            AlertKind::Call,
            None,
            "Friend",
            "Calling you on Steam",
        );
        assert!(alerts.ringing.is_none());
    }

    #[test]
    fn steam_process_names() {
        let Some(app) = Network::Steam.official_app() else {
            panic!("Steam's app is detectable");
        };
        assert!(is_app_process(app.linux, "steam"));
        assert!(is_app_process(app.linux, "steamwebhelper"));
        assert!(!is_app_process(app.linux, "steamy"));
    }
}
