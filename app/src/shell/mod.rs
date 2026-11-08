// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Ties the core, the Stoat account and the window together: runs effects,
//! feeds events back, and refreshes the window from the core's state.

mod accounts;
mod alerting;
mod calls;
mod forward;
mod games;
mod keep_awake;
mod media;
mod messages;
mod navigation;
mod notifications;
mod poll;
mod profiles;
mod report;
mod server_admin;
mod settings_pages;
mod threads;

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;

use crate::registry::{self, Account, Status};
use godwit_core::account::{MediaFetch, NetworkAccount};
use godwit_core::application::{Action, Application, Command, Effect, Event, Outgoing};
use godwit_core::backend::Waker;
use godwit_core::call::CallStatus;
use godwit_core::contacts::{ContactId, Contacts, Metacontact, store as contacts_store};
use godwit_core::ids::{AccountId, MessageId, RoomId, ServerId, UserId};
use godwit_core::message::AttachmentKind;
use godwit_core::network::Network;
use godwit_core::rich;
use godwit_voice::dsp::NoiseSuppression;
use slint::{ModelRc, SharedString, VecModel};

use crate::cache::Cache;
use crate::errors::ErrorLog;
use crate::images::Images;
use crate::inline_player::InlinePlayers;
use crate::picker::{self, Picker};
use crate::present::Presenter;
use crate::settings::{self, Settings, account_key, parse_account_key};
use crate::theme;
use crate::ui::{AppWindow, NetworkItem, ProblemItem, Screen};
use crate::view;

pub struct Shell {
    app: Application,
    navigation_back: Vec<NavigationTarget>,
    replaying_navigation: bool,
    /// Updated by native window focus events; selection alone is not attention.
    window_focused: bool,
    /// When each chat last alerted (see `alerted_lately`).
    recent_alerts: std::collections::HashMap<RoomId, std::time::Instant>,
    /// The Notifications page's list (fed in `alerting`), and its rows.
    notifications: crate::notifications::Notifications,
    notification_items: Rc<VecModel<crate::ui::NotificationItem>>,
    /// Stoat accounts, each with its own saved session (a `session_file`
    /// slot). Their sign-in, voice and devices are Stoat-specific, so they
    /// keep their own field; everything else goes through
    /// [`NetworkAccount`]. At most one isn't signed in: the one the sign-in
    /// form drives.
    /// Every network's accounts, in the registry's order.
    live: Vec<Box<dyn Account>>,
    /// The QR code shown for a Steam or Discord sign-in, and the URL it encodes.
    steam_qr: Option<(String, slint::Image)>,
    /// The captcha page last opened in the browser for a Discord sign-in.
    discord_captcha: Option<String>,
    /// Notifications, sounds and the ringtone.
    alerts: crate::alerts::Alerts,
    /// The account to tell, once, that Godwit kept its alert quiet because
    /// the network's own app runs here (the dialog shows while it's set).
    quiet_notice: Option<AccountId>,
    /// A call Godwit was in when it last stopped without closing (crashed or
    /// killed): offered to rejoin once its room is known, until answered.
    rejoin: Option<RoomId>,
    /// The call ringing us, as the incoming-call popup shows it: its room
    /// and who's calling. Shown even when its alert is kept quiet (Do not
    /// disturb, the Steam app running): calls still show in the window.
    incoming_call: Option<(RoomId, String, String)>,
    /// The image viewer: the message and which of its attachments it shows.
    viewer: Option<(MessageId, usize)>,
    /// The viewer shows a picture: dropped on close so the window doesn't
    /// keep it (Yapfast's `viewer_image`).
    viewer_shown: bool,
    /// The open viewer's animation slot: it plays, like the timeline's visible ones.
    viewer_anim: Option<usize>,
    /// A picked picture being cropped before upload.
    crop: Option<crate::cropper::Crop>,
    /// The message a thread is being started from (the dialog's).
    thread_from: Option<MessageId>,
    /// The tags picked in the new-post dialog, by id.
    post_tags: Vec<std::sync::Arc<str>>,
    /// The forum list as last shown, so a refresh that changed nothing
    /// there rebuilds nothing.
    forum_shown: std::cell::RefCell<Option<godwit_core::forum::Forum>>,
    /// Settings → Voice's test call, while it runs, and the timer showing its level.
    mic_test: Option<(godwit_voice::mictest::MicTest, slint::Timer)>,
    /// Wakes the UI: new accounts' network threads get it.
    wake: Waker,
    /// Wakes the UI when the oldest "… is typing" runs out (see `pump`).
    typing_timer: slint::Timer,
    /// Prevent idle lock/sleep only while a call video is being watched or sent.
    media_awake: keep_awake::MediaAwake,
    /// The demo's stand-in network (`godwit --demo`).
    demo: Option<crate::demo::DemoAccount>,
    /// The demo's Steam account's game libraries ("Games in common…").
    demo_steam: Option<crate::demo::DemoSteam>,
    /// Networks' own media schemes and fetchers (for attachments to save).
    media: Vec<(String, MediaFetch)>,
    images: Images,
    inline_players: InlinePlayers,
    fullscreen_player: Option<crate::player_window::FullscreenPlayer>,
    /// Plays animated pictures (anim.rs).
    animator: std::rc::Rc<std::cell::RefCell<crate::anim::Animator>>,
    inline_wake: Waker,
    contacts_path: Option<PathBuf>,
    cache: Option<Cache>,
    cached_accounts: HashSet<AccountId>,
    cache_loads_finished: HashSet<AccountId>,
    saved_cache: std::collections::HashMap<AccountId, godwit_core::cache::AccountSnapshot>,
    /// Cleared from Settings: nothing more is saved until the next start, so
    /// what's open now doesn't go straight back to disk.
    cache_cleared: bool,
    /// Errors, logged instead of shown as pop-ups (Settings → Problems).
    errors: ErrorLog,
    /// The last sign-in form shown, so "logging in" keeps showing it.
    mfa_form: bool,
    presenter: Presenter,
    picker: Picker,
    /// What the open picker picks for: a message's reactions, or the composer.
    picker_for: Option<Option<MessageId>>,
    picker_room: Option<RoomId>,
    /// The GIF picker's tiles (gifs.rs).
    gif_tiles: crate::gifs::GifTiles,
    /// The picker's Stickers tab (stickers.rs).
    sticker_tiles: crate::stickers::StickerTiles,
    /// The "Games in common" dialog's models (games.rs).
    games: crate::games::GamesDialog,
    /// The timeline's text selection (selection.rs).
    selection: crate::selection::ChatSelection,
    /// Text selected in the profile card (its row `view::CARD_ROW`).
    card_selection: crate::selection::BlockSelection,
    /// The room each quick-switcher result opens.
    switch_rows: Vec<RoomId>,
    /// The message the forward picker is for, and the room each result sends to.
    forward: Option<(RoomId, MessageId)>,
    forward_rows: Vec<RoomId>,
    /// What is typed in the poll dialog.
    poll: poll::PollDraft,
    settings: Settings,
    settings_path: Option<PathBuf>,
    /// The network whose sign-in form is open (None: the network picker).
    login_network: Option<Network>,
    /// "Add an account" was chosen while signed in.
    adding: bool,
    /// A profile switch (or taking its last account out) left nothing
    /// running: the empty state shows, not a sign-in form. Ends once an
    /// account is up.
    empty_profile: bool,
    networks: Rc<VecModel<NetworkItem>>,
    /// Whether the picker was last built signed in (availability changes).
    networks_signed_in: Option<Vec<bool>>,
    /// The device lists shown in Settings (index 0 is "System default").
    devices_shown: bool,
    /// This computer's audio devices and cameras, listed on a thread of
    /// their own when Settings opens: no account's network is needed.
    devices: Option<godwit_voice::call::AudioDevices>,
    devices_rx: Option<std::sync::mpsc::Receiver<godwit_voice::call::AudioDevices>>,
    /// The interface scale applied to the window (Settings › Appearance).
    scale: crate::scale::Scale,
    share_dialog: Option<crate::voice_ui::ShareDialog>,
    join_accounts: Vec<AccountId>,
    join_preview: Option<(AccountId, std::sync::Arc<str>)>,
    join_submitted: bool,
    join_finished: bool,
    profile_editor: crate::profile_settings::ProfileEditor,
    /// Settings → a Stoat account → Auto-delete.
    autodelete_page: crate::autodelete_page::AutoDeletePage,
    /// Settings → a Discord account → Privacy and Sessions.
    account_pages: crate::account_pages::AccountPages,
    /// The call's videos at the last refresh: how many others' videos, and
    /// whether our camera and screen share were on (None: no call), to give
    /// memory back once a call, a video or our own capture ends.
    call_videos_seen: Option<(usize, bool, bool)>,
    /// The voice settings last handed to the calls (with unwatched streams
    /// muted), so a refresh only re-sends them when they change.
    pushed_voice: Option<godwit_voice::call::VoiceSettings>,
    server_editor: crate::server_settings::ServerEditor,
    /// What the open report dialog is about, and whether it was sent.
    report: Option<godwit_core::report::ReportTarget>,
    report_submitted: bool,
    /// The addresses behind the declined-report popup's link buttons.
    report_links: Vec<String>,
    /// The addresses behind the privacy overview's source buttons, per paragraph.
    privacy_sources: Vec<Vec<String>>,
    channel_editor: crate::channel_settings::ChannelEditor,
    /// The call's account, and whether its video was last said to be on screen.
    call_shown: Option<(AccountId, bool)>,
    /// Drafts of the profiles not in use, kept for this session.
    stashed_drafts: std::collections::HashMap<u32, std::collections::HashMap<RoomId, String>>,
}

#[derive(Clone)]
enum NavigationTarget {
    Room(RoomId),
    Friends,
    People,
    Active,
    Notifications,
    /// Games in common, on that account (who was picked isn't kept).
    Games(AccountId),
    Threads,
}

impl Shell {
    /// `demo`: a made-up world and no accounts (`godwit --demo`): no session
    /// is read, nothing goes out, and settings and contacts aren't saved.
    pub fn new(ui: &AppWindow, wake: Waker, demo: bool) -> std::io::Result<Self> {
        registry::register_hooks();
        let settings_path = if demo { None } else { settings::path() };
        let settings = match settings_path.as_deref().map(settings::load) {
            Some(Ok(s)) => s,
            Some(Err(e)) if e.kind() == std::io::ErrorKind::InvalidData => {
                match settings_path.as_deref().map(crate::local_recovery::preserve_corrupt) {
                    Some(Ok(recovery)) => eprintln!(
                        "warning: settings were reset; the damaged file was preserved at {}",
                        recovery.display()
                    ),
                    Some(Err(_)) => {
                        eprintln!("error: settings were invalid and could not be preserved; using defaults")
                    }
                    None => eprintln!("error: settings were invalid; using defaults"),
                }
                Settings::default()
            }
            Some(Err(_)) => {
                eprintln!("error: could not read settings; using defaults");
                Settings::default()
            }
            None => Settings::default(),
        };
        let mut settings = settings;
        settings.migrate_profiles();
        if demo {
            // Two made-up profiles, in memory only (settings aren't saved in demo).
            settings.migrate_profiles();
            settings.rename_profile(0, "Work");
            settings.create_profile("Private");
        }
        let contacts_path = if demo {
            None
        } else {
            contacts_store::path_for(settings.current_profile)
        };
        let contacts = profiles::load_contacts(contacts_path.as_deref());
        // The default profile's saved sign-ins connect (and any whose account
        // isn't known yet, which joins the profile once ready); with none,
        // the default network's account waits for the sign-in form.
        let mut live: Vec<Box<dyn Account>> = Vec::new();
        if demo {
            for entry in &registry::NETWORKS {
                live.extend(entry.inert.map(|inert| inert()));
            }
        } else {
            let saved: Vec<(String, Vec<u32>)> = registry::NETWORKS
                .iter()
                .map(|e| (e.network.key().to_owned(), (e.saved)()))
                .collect();
            let profile = settings.profile(settings.current_profile).cloned().unwrap_or_default();
            for (network, slot) in crate::profiles::startup_slots(&saved, &profile, &settings.known_accounts) {
                if let Some(entry) = registry::NETWORKS.iter().find(|e| e.network.key() == network) {
                    live.push((entry.start)(wake.clone(), slot)?);
                }
            }
            if live.is_empty() {
                let entry = registry::entry(registry::default_network());
                let taken = saved
                    .iter()
                    .find(|(n, _)| *n == entry.network.key())
                    .map(|(_, s)| s.as_slice())
                    .unwrap_or(&[]);
                let slot = (0..).find(|s| !taken.contains(s)).unwrap_or(0);
                live.push((entry.start)(wake.clone(), slot)?);
            }
        }
        for a in &mut live {
            a.set_notifications(settings.notifications || settings.sounds);
            a.set_voice_settings(settings.voice.clone());
        }
        theme::apply(ui, settings.accent(), settings.style, settings.dark);
        crate::emoji::set_pack(&settings.emoji_pack);
        profiles::apply_widths(ui, &settings);
        let media: Vec<(String, MediaFetch)> = live.iter().take(1).filter_map(|a| a.media()).collect();
        let (app, demo_account) = if demo {
            let (mut app, _, _) = crate::demo::world(contacts);
            let account = crate::demo::DemoAccount::new(&app);
            // A second account (Steam), to try several at once.
            crate::demo::multi(&mut app);
            // And Discord, through its real bridge on made-up data.
            let (ready, history) = crate::demo::discord_world();
            let (account_d, events) = registry::demo_discord(ready, history);
            for event in events {
                app.apply(event);
            }
            live.push(account_d);
            (app, Some(account))
        } else {
            (Application::new(contacts), None)
        };
        let cache = if demo {
            None
        } else {
            match Cache::open() {
                Ok(cache) => Some(cache),
                Err(_) => {
                    eprintln!("note: the encrypted message cache is unavailable");
                    None
                }
            }
        };
        // Pictures seen before load from disk, encrypted with the message
        // cache's key; demo mode keeps nothing.
        let disk = if demo {
            None
        } else {
            crate::cache::open_pictures()
                .inspect_err(|_| eprintln!("note: the picture cache is unavailable"))
                .ok()
        };
        let mut images = Images::new(wake.clone(), media.clone(), disk);
        // Animated pictures play from one shared list of frames (anim.rs).
        let animator = crate::anim::Animator::new();
        images.set_animator(animator.clone());
        {
            use slint::ComponentHandle;
            let frames = animator.borrow().model();
            ui.global::<crate::ui::Anim>().set_frames(slint::ModelRc::from(frames));
        }
        let inline_players = InlinePlayers::default();
        {
            use slint::ComponentHandle;
            ui.global::<crate::ui::Players>()
                .set_frames(slint::ModelRc::from(inline_players.frames()));
        }
        let gif_tiles = crate::gifs::GifTiles::default();
        {
            use slint::ComponentHandle;
            ui.global::<crate::ui::Gifs>().set_tiles(gif_tiles.model());
        }
        let sticker_tiles = crate::stickers::StickerTiles::default();
        {
            use slint::ComponentHandle;
            ui.global::<crate::ui::Gifs>().set_sticker_tiles(sticker_tiles.model());
        }
        if demo {
            crate::demo::pictures(&mut images);
            crate::demo::steam_pictures(&mut images);
            crate::demo::discord_pictures(&mut images);
        }
        crate::gifs::set_starred(&settings.gif_favourites);
        let rejoin = (settings.rejoin_offer != settings::RejoinOffer::Never)
            .then(|| settings.last_call.as_deref().and_then(settings::parse_room_key))
            .flatten();
        let mut app = app;
        app.dispatch(Action::RestoreVoiceFlags {
            muted: settings.self_muted,
            deafened: settings.self_deafened,
        });
        let shell = Self {
            app,
            navigation_back: Vec::new(),
            replaying_navigation: false,
            window_focused: false,
            recent_alerts: std::collections::HashMap::new(),
            notifications: Default::default(),
            notification_items: Rc::default(),
            live,
            steam_qr: None,
            discord_captcha: None,
            alerts: {
                let mut alerts = crate::alerts::Alerts::default();
                alerts.set_wake(wake.clone());
                alerts
            },
            quiet_notice: None,
            rejoin,
            incoming_call: None,
            viewer: None,
            viewer_shown: false,
            viewer_anim: None,
            crop: None,
            thread_from: None,
            post_tags: Vec::new(),
            forum_shown: std::cell::RefCell::new(None),
            mic_test: None,
            wake: wake.clone(),
            typing_timer: slint::Timer::default(),
            media_awake: keep_awake::MediaAwake::new(),
            demo_steam: demo_account.as_ref().map(|_| crate::demo::DemoSteam::default()),
            demo: demo_account,
            images,
            inline_players,
            fullscreen_player: None,
            animator,
            inline_wake: wake,
            media,
            contacts_path,
            cache,
            cached_accounts: HashSet::new(),
            cache_loads_finished: HashSet::new(),
            saved_cache: std::collections::HashMap::new(),
            cache_cleared: false,
            errors: ErrorLog::new(demo),
            mfa_form: false,
            presenter: {
                let mut p = Presenter::new(ui);
                p.collapsed = settings.collapsed.iter().filter_map(|k| parse_account_key(k)).collect();
                p
            },
            picker: Picker::default(),
            picker_for: None,
            picker_room: None,
            gif_tiles,
            sticker_tiles,
            games: crate::games::GamesDialog::default(),
            selection: crate::selection::ChatSelection::default(),
            card_selection: crate::selection::BlockSelection::default(),
            switch_rows: Vec::new(),
            forward: None,
            forward_rows: Vec::new(),
            poll: poll::PollDraft::default(),
            settings,
            settings_path,
            login_network: None,
            adding: false,
            empty_profile: false,
            networks: Rc::new(VecModel::default()),
            networks_signed_in: None,
            devices_shown: false,
            devices: None,
            devices_rx: None,
            scale: crate::scale::Scale::default(),
            share_dialog: None,
            join_accounts: Vec::new(),
            join_preview: None,
            join_submitted: false,
            join_finished: false,
            server_editor: crate::server_settings::ServerEditor::default(),
            report: None,
            report_submitted: false,
            report_links: Vec::new(),
            privacy_sources: Vec::new(),
            channel_editor: crate::channel_settings::ChannelEditor::default(),
            call_shown: None,
            profile_editor: Default::default(),
            autodelete_page: Default::default(),
            account_pages: Default::default(),
            call_videos_seen: None,
            pushed_voice: None,
            stashed_drafts: Default::default(),
        };
        ui.set_emoji_rows(ModelRc::from(shell.picker.model.clone()));
        ui.set_notification_items(ModelRc::from(shell.notification_items.clone()));
        ui.set_networks(ModelRc::from(shell.networks.clone()));
        // The version and, for a build from a checkout, its commit.
        let commit = env!("GODWIT_COMMIT");
        ui.set_version(if commit.is_empty() {
            env!("CARGO_PKG_VERSION").into()
        } else {
            format!("{} ({commit})", env!("CARGO_PKG_VERSION")).into()
        });
        let mut shell = shell;
        // The picker's first screens load now, so it opens drawn.
        shell.picker_search("");
        if shell.demo.is_some() {
            for n in crate::demo::notifications(shell.app.state()) {
                shell.notifications.push(n);
            }
        }
        shell.apply_view_settings();
        // Home's kept arrangement.
        let keys = |list: &[String]| {
            list.iter()
                .filter_map(|k| settings::parse_room_key(k))
                .collect::<Vec<_>>()
        };
        let (pinned, hidden) = (keys(&shell.settings.pinned_rooms), keys(&shell.settings.hidden_rooms));
        shell.app.dispatch(Action::RestoreRoomPrefs { pinned, hidden });
        Ok(shell)
    }

    /// Handles whatever the network and the image loader produced.
    pub fn window_focus_changed(&mut self, focused: bool) {
        self.window_focused = focused;
    }

    pub fn pump(&mut self, ui: &AppWindow) {
        let selected = self.app.state().selected.clone();
        // Sign-in steps (a code came, it went through) aren't core events:
        // a change there is something to show too.
        let forms = |s: &Self| -> Vec<(Status, registry::LoginView)> {
            s.live
                .iter()
                .filter(|a| a.id().is_none())
                .map(|a| (a.status(), a.login_view()))
                .collect()
        };
        let before = forms(self);
        let events = self.poll_accounts();
        self.sync_media();
        let mut had_events = !events.is_empty() || forms(self) != before;
        self.apply_all(events);
        had_events |= self.app.expire_typing(std::time::Instant::now());
        // Nothing else may wake us when a notice runs out (no stop event, a
        // quiet network), so a notice left on screen gets its own wake-up.
        if let Some(at) = self.app.typing_expires_at() {
            let wake = self.wake.clone();
            let after = at.saturating_duration_since(std::time::Instant::now()) + std::time::Duration::from_millis(50);
            self.typing_timer
                .start(slint::TimerMode::SingleShot, after, move || wake());
        } else {
            self.typing_timer.stop();
        }
        had_events |= self.apply_cache_loads();
        if self.app.state().selected != selected {
            self.close_picker(ui);
            self.close_fullscreen_player();
            self.inline_players.stop_all();
            self.presenter.images_changed = true;
        }
        self.presenter.images_changed |= self.images.poll();
        // A clicked notification: its chat, in front.
        if let Some(room) = self.alerts.take_clicked()
            && self.app.state().room(&room).is_some()
        {
            self.act(ui, Action::SelectRoom(room));
            if let Err(e) = slint::ComponentHandle::show(ui) {
                eprintln!("error: showing the window: {e}");
            }
        }
        self.log_errors(ui);
        let players = self.inline_players.poll();
        if self
            .fullscreen_player
            .as_mut()
            .is_some_and(|window| !window.poll(&mut self.inline_players))
        {
            self.close_fullscreen_player();
        }
        self.presenter.frames_changed |= players.changed;
        // A page inside a player asked to open a link: asked first, as any link.
        if let Some(link) = players
            .links
            .last()
            .filter(|l| l.starts_with("https://") || l.starts_with("http://"))
        {
            ui.set_confirm_link(link.as_str().into());
        }
        if self.presenter.images_changed && self.picker_for.is_some() {
            self.picker.refresh(&mut self.images);
        }
        // Only a call's video frame (nothing else came): swap the pictures,
        // as Yapfast does, rather than rebuilding the whole window 30 times
        // a second per video.
        let only_frames = !had_events
            && self.app.state().selected == selected
            && !self.presenter.images_changed
            && !players.changed
            && players.links.is_empty()
            && players.fullscreen.is_empty();
        if only_frames && !self.presenter.update_call_frames() {
            return;
        }
        self.refresh(ui);
        for spec in players.fullscreen {
            self.player_fullscreen(ui, &spec.url);
        }
        self.autoload_players(ui);
        self.load_invites(ui);
    }

    pub fn act(&mut self, ui: &AppWindow, action: Action) {
        if !self.replaying_navigation
            && matches!(
                action,
                Action::SelectRoom(_)
                    | Action::ShowFriends
                    | Action::ShowPeople
                    | Action::ShowActive
                    | Action::ShowNotifications
                    | Action::ShowThreads
                    | Action::OpenGamesInCommon { .. }
            )
        {
            let state = self.app.state();
            let current = if state.showing_threads {
                Some(NavigationTarget::Threads)
            } else if state.showing_active {
                Some(NavigationTarget::Active)
            } else if state.showing_notifications {
                Some(NavigationTarget::Notifications)
            } else if let Some(g) = state.games.as_ref().filter(|_| state.showing_games) {
                Some(NavigationTarget::Games(g.account.clone()))
            } else if state.showing_people {
                Some(NavigationTarget::People)
            } else if state.showing_friends {
                Some(NavigationTarget::Friends)
            } else {
                state.selected.clone().map(NavigationTarget::Room)
            };
            if let Some(current) = current {
                self.navigation_back.push(current);
                if self.navigation_back.len() > 32 {
                    self.navigation_back.remove(0);
                }
            }
        }
        // Leaving: its sound now. The core forgets the call at once, so the
        // network's later "call ended" no longer matches it.
        if matches!(action, Action::LeaveCall) && self.app.state().call.is_some() {
            self.alerts.sound(&self.settings, godwit_voice::sounds::Sound::Leave);
        }
        if matches!(action, Action::JoinCall(_) | Action::LeaveCall) {
            self.share_dialog = None;
        }
        // A card's text selection goes with the card.
        if matches!(action, Action::OpenProfile(_) | Action::CloseProfile) && !self.card_selection.is_empty() {
            self.card_selection.clear();
            self.show_selection(ui);
        }
        // Opening a room or server: the "you" views follow it again.
        if matches!(action, Action::SelectRoom(_) | Action::SelectServer(_)) {
            self.presenter.focus = None;
        }
        let selected = self.app.state().selected.clone();
        let loaded = self.app.state().timelines.len();
        let toggles = matches!(action, Action::ToggleMute | Action::ToggleDeafen);
        // Watching another stream makes it the one heard, as Discord's
        // focused stream: the others watched are muted (each can be
        // unmuted again from its own control).
        if let Action::WatchStream { user, watch: true } = &action
            && let Some(call) = self.app.state().call.as_ref()
        {
            let others: Vec<String> = call
                .streams
                .iter()
                .filter(|s| s.watching && s.user != *user)
                .map(|s| s.user.raw.to_string())
                .collect();
            // Watching one stream hears it (and only it). Always unmute it:
            // LiveKit's stream sound starts muted, while the panel shows
            // unmuted, so a lone stream stayed silent.
            let mutes = &mut self.settings.voice.screen_share_mutes;
            for other in others {
                mutes.insert(other, true);
            }
            mutes.insert(user.raw.to_string(), false);
            self.push_voice_settings();
            self.save_settings();
        }
        // Watching a stream or stopping: its cue, as the official clients.
        if let Action::WatchStream { user, watch } = &action
            && self
                .app
                .state()
                .call
                .as_ref()
                .is_some_and(|c| c.streams.iter().any(|s| s.user == *user && s.watching != *watch))
        {
            use godwit_voice::sounds::Sound;
            self.alerts.sound(
                &self.settings,
                if *watch { Sound::WatchStart } else { Sound::WatchStop },
            );
        }
        let flags = |s: &godwit_core::application::AppState| (s.self_muted, s.self_deafened);
        let (muted, deafened) = flags(self.app.state());
        let editing = matches!(action, Action::StartEdit(_) | Action::EditLast);
        let effects = self.app.dispatch(action);
        // Muting and deafening answer with a short sound, as the official
        // clients do, in a call or not; and they're kept for next time.
        if toggles {
            use godwit_voice::sounds::Sound;
            let (now_muted, now_deafened) = flags(self.app.state());
            let cue = if now_deafened != deafened {
                Some(if now_deafened { Sound::Deafen } else { Sound::Undeafen })
            } else if now_muted != muted {
                Some(if now_muted { Sound::Mute } else { Sound::Unmute })
            } else {
                None
            };
            if let Some(cue) = cue {
                self.alerts.sound(&self.settings, cue);
            }
            self.settings.self_muted = now_muted;
            self.settings.self_deafened = now_deafened;
            self.save_settings();
        }
        // A room's messages were let go: give the memory back.
        if self.app.state().timelines.len() < loaded {
            crate::memory::settle();
        }
        self.run(effects);
        if self.app.state().selected != selected {
            self.close_fullscreen_player();
            self.inline_players.stop_all();
            self.presenter.images_changed = true;
        }
        self.save_cache();
        self.log_errors(ui);
        self.refresh(ui);
        // Editing starts with the cursor after the text, as officially, not
        // before it (offsets are clamped to the text's length).
        if editing {
            ui.set_composer_inserted_at(i32::MAX);
            ui.set_composer_insert_serial(ui.get_composer_insert_serial().wrapping_add(1));
        }
    }

    pub fn navigate_back(&mut self, ui: &AppWindow) {
        let Some(target) = self.navigation_back.pop() else {
            return;
        };
        let action = match target {
            NavigationTarget::Room(room) if self.app.state().room(&room).is_some() => Action::SelectRoom(room),
            NavigationTarget::Friends => Action::ShowFriends,
            NavigationTarget::People => Action::ShowPeople,
            NavigationTarget::Active => Action::ShowActive,
            NavigationTarget::Notifications => Action::ShowNotifications,
            NavigationTarget::Games(account) => Action::OpenGamesInCommon {
                account,
                with: Vec::new(),
            },
            NavigationTarget::Threads => Action::ShowThreads,
            NavigationTarget::Room(_) => return,
        };
        self.replaying_navigation = true;
        self.act(ui, action);
        self.replaying_navigation = false;
    }

    /// What the UI shows (for callbacks that look something up first).
    pub fn app_state(&self) -> &godwit_core::application::AppState {
        self.app.state()
    }

    fn apply_all(&mut self, events: Vec<Event>) {
        let has_events = !events.is_empty();
        let mut back_offline = Vec::new();
        for event in events {
            if matches!(
                &event,
                Event::CallEnded { .. } | Event::AccountLost { .. } | Event::RoomRemoved(_)
            ) {
                self.share_dialog = None;
            }
            self.observe_invite_result(&event);
            self.observe_alerts(&event);
            // Signed in again: no longer asking for it.
            if let Event::AccountReady { account, .. } = &event {
                self.record_ready(account);
                let key = settings::account_key(account);
                let before = self.settings.sign_in_needed.len();
                self.settings.sign_in_needed.retain(|n| n.account != key);
                if self.settings.sign_in_needed.len() != before {
                    self.save_settings();
                }
            }
            if let Event::AccountReady { account, .. } = &event
                && self.settings.is_offline(account)
            {
                back_offline.push(account.clone());
            }
            match &event {
                Event::AccountReady { account, .. } if self.cached_accounts.insert(account.clone()) => {
                    if let Some(cache) = &self.cache {
                        if cache.request_load(account).is_err() {
                            self.cache_loads_finished.insert(account.clone());
                            eprintln!("note: the encrypted message cache could not be read");
                        }
                    } else {
                        self.cache_loads_finished.insert(account.clone());
                    }
                }
                Event::AccountLost { account, reason } => {
                    // Signed out by the network (we sign out with no reason):
                    // kept in the rail as "sign in again".
                    if !reason.is_empty() {
                        let name = self
                            .app
                            .state()
                            .accounts
                            .iter()
                            .find(|a| a.id == *account)
                            .map_or_else(String::new, |a| a.display_name.to_string());
                        let key = settings::account_key(account);
                        self.settings.sign_in_needed.retain(|n| n.account != key);
                        self.settings.sign_in_needed.push(settings::SignInNeeded {
                            account: key,
                            network: account.network.key().to_owned(),
                            name,
                            reason: reason.to_string(),
                        });
                        self.save_settings();
                    }
                    if reason.is_empty() && self.demo.is_none() {
                        // A log-out: no profile keeps it, nothing to start again.
                        self.settings.forget_account(&settings::account_key(account));
                        self.save_settings();
                    }
                    self.cached_accounts.remove(account);
                    self.cache_loads_finished.remove(account);
                    self.saved_cache.remove(account);
                    if let Some(cache) = &self.cache {
                        cache.remove(account);
                    }
                }
                _ => {}
            }
            let effects = self.app.apply(event);
            self.run(effects);
        }
        // Taken offline earlier (a restart, or Steam reconnecting): down again
        // as soon as the account is known.
        for account in back_offline {
            let events = self
                .accounts()
                .find(|a| a.route_id() == Some(&account))
                .map(|a| a.set_offline(true))
                .unwrap_or_default();
            self.apply_all(events);
        }
        if has_events {
            self.save_cache();
        }
    }

    fn save_cache(&mut self) {
        if self.cache_cleared {
            return;
        }
        let Some(cache) = &self.cache else { return };
        for account in &self.app.state().accounts {
            if !self.cache_loads_finished.contains(&account.id) {
                continue;
            }
            let snapshot = self.app.cache_snapshot(&account.id);
            if self.saved_cache.get(&account.id) == Some(&snapshot) {
                continue;
            }
            cache.save(&snapshot);
            self.saved_cache.insert(account.id.clone(), snapshot);
        }
    }

    /// Whether anything came from the cache.
    fn apply_cache_loads(&mut self) -> bool {
        let loaded = self.cache.as_ref().map(Cache::take_loaded).unwrap_or_default();
        let any = !loaded.is_empty();
        for (account, snapshot) in loaded {
            if !self.cached_accounts.contains(&account) || !self.app.state().accounts.iter().any(|a| a.id == account) {
                continue;
            }
            self.cache_loads_finished.insert(account.clone());
            if let Some(snapshot) = snapshot {
                self.saved_cache.insert(account, snapshot.clone());
                self.app.restore_cache(snapshot);
            }
        }
        self.save_cache();
        any
    }

    /// Runs effects until none are left (a command can produce events,
    /// which can produce effects).
    fn run(&mut self, effects: Vec<Effect>) {
        let mut queue: VecDeque<Effect> = effects.into();
        while let Some(effect) = queue.pop_front() {
            match effect {
                Effect::Backend { account, command } => {
                    // Offline accounts get nothing: a send fails visibly (its text
                    // comes back), a load stops waiting, the rest waits for later.
                    if self.settings.is_offline(&account) {
                        let offline = || -> std::sync::Arc<str> { "This account is offline.".into() };
                        let event = match command {
                            Command::SendMessage { room, nonce, .. } => Some(Event::MessageFailed {
                                room,
                                nonce,
                                error: offline(),
                            }),
                            Command::LoadHistory { room, .. } => Some(Event::HistoryFailed { room }),
                            Command::JoinCall { room } => Some(Event::CallEnded {
                                room,
                                reason: Some(offline()),
                            }),
                            _ => None,
                        };
                        if let Some(event) = event {
                            queue.extend(self.app.apply(event));
                        }
                        continue;
                    }
                    let Some(a) = self.accounts().find(|a| a.route_id() == Some(&account)) else {
                        // Nobody to carry it out (signed out meanwhile): a send must
                        // fail visibly, so its text comes back to the composer.
                        if let Command::SendMessage { room, nonce, .. } = command {
                            queue.extend(self.app.apply(Event::MessageFailed {
                                room,
                                nonce,
                                error: "Not signed in to this account.".into(),
                            }));
                        }
                        continue;
                    };
                    for event in a.run(command) {
                        self.observe_invite_result(&event);
                        self.observe_alerts(&event);
                        queue.extend(self.app.apply(event));
                    }
                }
                Effect::WatchStream {
                    account,
                    room,
                    user,
                    watch,
                } => {
                    if self.settings.is_offline(&account) {
                        continue;
                    }
                    let Some(a) = self.accounts().find(|a| a.route_id() == Some(&account)) else {
                        continue;
                    };
                    for event in a.watch_stream(&room, &user, watch) {
                        queue.extend(self.app.apply(event));
                    }
                }
                Effect::SaveContacts => {
                    if let Some(path) = &self.contacts_path
                        && let Err(e) = contacts_store::save(path, &self.app.state().contacts)
                    {
                        eprintln!("error: saving contacts: {e}");
                    }
                }
            }
        }
    }

    pub fn refresh(&mut self, ui: &AppWindow) {
        let media_active =
            self.app.state().call.as_ref().is_some_and(|call| {
                call.camera || call.screen_share || call.streams.iter().any(|stream| stream.watching)
            });
        self.media_awake.set_active(media_active);
        // Streams start, stop and get watched: their sound follows.
        self.push_voice_settings();
        self.scale.apply(ui, self.settings.ui_scale);
        self.save_room_prefs();
        self.presenter.pending = self
            .accounts()
            .filter(|a| a.connecting())
            .map(|a| a.network())
            .collect();
        // A call or a video ended: what it decoded and buffered was freed, but
        // glibc keeps freed memory; hand it back (measured: 211 MB stayed
        // after a call with a stream, 2026-11-04).
        let videos = self
            .app
            .state()
            .call
            .as_ref()
            .map(|c| (c.videos.len(), c.camera, c.screen_share));
        let ended = video_ended(self.call_videos_seen, videos);
        self.call_videos_seen = videos;
        if ended {
            crate::memory::settle_after(std::time::Duration::from_secs(2));
        }
        if let Some(account) = self.autodelete_page.account.clone() {
            let view = self.app.state().autodelete.get(&account).cloned();
            // Named in full on the master switch, so it's clear whose
            // messages it's about: "Stoat · Name (@user)".
            let label = self
                .app
                .state()
                .accounts
                .iter()
                .find(|a| a.id == account)
                .map(|a| format!("{} · {} (@{})", account.network.name(), a.display_name, a.username))
                .unwrap_or_else(|| account.network.name().to_string());
            self.autodelete_page.show(ui, view.as_ref(), &label);
        }
        self.account_pages.show(ui, self.app.state());
        self.show_report(ui);
        self.show_report_entries(ui);
        self.show_devices(ui);
        let signed_in = !self.app.state().accounts.is_empty();
        let status = self.sign_in_status();
        // A sign-in that went through ends "Add an account", on the list of
        // accounts where the new one now is (just closing the form looked
        // like nothing happened).
        if self.login_network.is_some() && status == Status::Ready {
            self.adding = false;
            self.login_network = None;
            // The new account's own page (the newest one listed).
            let newest = self.app.state().accounts.len().saturating_sub(1);
            ui.set_settings_section(10);
            ui.set_settings_account(i32::try_from(newest).unwrap_or(0));
            ui.set_settings_open(true);
            self.settings_opened(ui);
        }
        match &status {
            Status::SignedOut { error } | Status::NeedsMfa { error } => {
                ui.set_login_error(error.clone().unwrap_or_default().into());
            }
            _ => ui.set_login_error("".into()),
        }
        if signed_in {
            self.empty_profile = false;
        }
        let screen = if signed_in && !self.adding {
            Screen::Main
        } else if self.empty_profile && !self.adding {
            Screen::Empty
        } else {
            match status {
                Status::NeedsMfa { .. } => {
                    self.mfa_form = true;
                    self.login_network.get_or_insert_with(registry::default_network);
                    Screen::Mfa
                }
                Status::LoggingIn if self.mfa_form => Screen::Mfa,
                Status::LoggingIn => Screen::Login,
                // Signing in with a saved session, or a sign-in that just went through.
                Status::Connecting if self.login_network.is_some() || !self.adding => Screen::Connecting,
                Status::SignedOut { .. } | Status::Connecting | Status::Ready => {
                    self.mfa_form = false;
                    Screen::Login
                }
            }
        };
        ui.set_screen(screen);
        // On the main screen a call's video always shows: in its room, or in
        // the pop-out over any other. The network fetches video only then.
        let call_shown = self
            .app
            .state()
            .call
            .as_ref()
            .map(|c| (c.room.account.clone(), screen == Screen::Main));
        if call_shown.is_some() && call_shown != self.call_shown {
            let shown = call_shown.as_ref().is_some_and(|(_, s)| *s);
            let effects = self.app.dispatch(Action::SetCallShown(shown));
            self.run(effects);
        }
        self.call_shown = call_shown;
        ui.set_login_busy(status == Status::LoggingIn);
        self.show_steam_login(ui);
        ui.set_login_network(
            self.login_network
                .and_then(|n| view::ACCOUNT_NETWORKS.iter().position(|x| *x == n))
                .and_then(|i| i32::try_from(i).ok())
                .unwrap_or(-1),
        );
        // From the empty state the picker can go back to it.
        ui.set_login_can_cancel(signed_in || self.empty_profile);
        let available: Vec<bool> = view::ACCOUNT_NETWORKS
            .iter()
            .map(|n| self.network_available(*n))
            .collect();
        if self.networks_signed_in.as_ref() != Some(&available) {
            let rows: Vec<NetworkItem> = view::ACCOUNT_NETWORKS
                .iter()
                .zip(&available)
                .map(|(n, a)| view::network(*n, *a))
                .collect();
            self.networks.set_vec(rows);
            self.networks_signed_in = Some(available);
        }
        // Each account's alert mode and whether it's offline.
        let ids: Vec<AccountId> = self.app.state().accounts.iter().map(|a| a.id.clone()).collect();
        self.presenter.account_alerts = ids
            .into_iter()
            .map(|id| {
                let choices = settings::alert_choices(id.network);
                let mode = self.settings.alert_mode(&id);
                let row = crate::present::AccountAlerts {
                    options: crate::present::alert_options(&choices),
                    index: choices
                        .iter()
                        .position(|c| *c == mode)
                        .and_then(|i| i32::try_from(i).ok())
                        .unwrap_or(0),
                    can_offline: self.account(&id).is_some_and(|a| a.can_go_offline()),
                    offline: self.settings.is_offline(&id),
                };
                (id, row)
            })
            .collect();
        // Steam's device names, to find (or revoke) a sign-in in its device list.
        self.presenter.account_details = self
            .live
            .iter()
            .filter_map(|a| Some((a.id()?.clone(), a.detail()?)))
            .collect();
        let state = self.app.state();
        self.presenter.stream_sound = self.settings.voice.clone();
        self.presenter.show_with_players(
            ui,
            state,
            screen == Screen::Main,
            &mut self.images,
            Some(&self.inline_players),
        );
        self.show_gifs(ui);
        self.show_stickers(ui);
        self.show_selection(ui);
        self.schedule_animation();
        self.show_join_server(ui);
        self.show_own_profile(ui);
        self.show_server_settings(ui);
        self.show_channel_settings(ui);
        self.show_share_dialog(ui);
        self.show_discord_privacy(ui);
        self.show_quiet_notice(ui);
        self.show_notifications(ui);
        self.remember_call(ui);
        self.show_incoming_call(ui);
        self.show_pins(ui);
        self.show_forum(ui);
        self.show_games(ui);
        self.show_search(ui);
        if self.viewer.is_some() || self.viewer_shown {
            self.show_viewer(ui);
        }
    }
}

/// Puts text on the clipboard. On Linux the clipboard needs its owner to stay
/// alive until someone else takes it, so a thread holds it for a while.
/// Copies `text` to the clipboard (the profile card's username).
pub fn copy_string(text: String) {
    copy_to_clipboard(text);
}

fn copy_to_clipboard(text: String) {
    let spawned = std::thread::Builder::new().name("clipboard".into()).spawn(move || {
        let mut clipboard = match arboard::Clipboard::new() {
            Ok(c) => c,
            Err(e) => return eprintln!("error: opening the clipboard: {e}"),
        };
        #[cfg(target_os = "linux")]
        {
            use arboard::SetExtLinux;
            let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
            if let Err(e) = clipboard.set().wait_until(until).text(text) {
                eprintln!("error: copying: {e}");
            }
        }
        #[cfg(not(target_os = "linux"))]
        if let Err(e) = clipboard.set_text(text) {
            eprintln!("error: copying: {e}");
        }
    });
    if let Err(e) = spawned {
        eprintln!("error: copying: {e}");
    }
}

/// A closure reading a test call's level, for the UI timer.
fn test_level_reader(test: &godwit_voice::mictest::MicTest) -> impl Fn() -> f32 + 'static {
    let level = test.level_handle();
    move || f32::from_bits(level.load(std::sync::atomic::Ordering::Relaxed)).clamp(0.0, 1.0)
}

/// A picture the viewer can show: its address, name and pixel size, and for
/// a GIF that is really a looping video (Discord's `gifv`), that video.
type Picture = (
    std::sync::Arc<str>,
    std::sync::Arc<str>,
    Option<(u32, u32)>,
    Option<std::sync::Arc<str>>,
);

/// A message's pictures as the viewer steps through them: its image
/// attachments, then its embeds' pictures (GIFs, link previews).
fn pictures(m: &godwit_core::message::Message) -> Vec<Picture> {
    let attachments = m
        .attachments
        .iter()
        .filter(|a| a.kind == AttachmentKind::Image)
        // The full file where the network serves a lighter preview (Stoat's
        // `originalUrl`, as its client's viewer).
        .map(|a| {
            let url = a.original_url.clone().unwrap_or_else(|| a.url.clone());
            (url, a.name.clone(), a.dimensions, None)
        });
    let embeds = m.embeds.iter().filter_map(|e| {
        let url = e.image_url.clone()?;
        let name = e
            .title
            .clone()
            .or_else(|| e.site_name.clone())
            .unwrap_or_else(|| "Image".into());
        Some((url, name, e.media_dimensions, e.video_url.clone()))
    });
    attachments.chain(embeds).collect()
}

/// Whether something in the call that holds video memory ended between two
/// refreshes (`(others' videos, our camera, our screen share)`, None: no
/// call): the call, one of the videos, or our own camera or share (its
/// capture and encoder buffers: 75 MB stayed after a share, 2026-11-04).
fn video_ended(before: Option<(usize, bool, bool)>, now: Option<(usize, bool, bool)>) -> bool {
    match (before, now) {
        (Some(_), None) => true,
        (Some((videos, camera, share)), Some((videos_now, camera_now, share_now))) => {
            videos_now < videos || (camera && !camera_now) || (share && !share_now)
        }
        _ => false,
    }
}

/// Saves a web picture into the Downloads folder, off the UI thread.
fn save_download(url: String, name: String) {
    let spawned = std::thread::Builder::new().name("download".into()).spawn(move || {
        let Some(dir) = directories::UserDirs::new().and_then(|d| d.download_dir().map(PathBuf::from)) else {
            return eprintln!("error: no Downloads folder");
        };
        let Some(bytes) = crate::images::download(&url) else {
            return eprintln!("error: downloading an image failed");
        };
        let path = unique_path(&dir, &safe_file_name(&name));
        match std::fs::write(&path, bytes) {
            Ok(()) => godwit_core::verbose!("saved an image to {}", path.display()),
            Err(e) => eprintln!("error: saving an image: {e}"),
        }
    });
    if let Err(e) = spawned {
        eprintln!("error: downloading: {e}");
    }
}

/// Downloads an attachment that isn't a web link (Matrix media) into the
/// Downloads folder, off the UI thread. Pictures, video and sound then open;
/// other files only show their folder, since opening a downloaded file
/// could run it.
fn save_and_open(fetch: MediaFetch, url: String, name: String, kind: AttachmentKind) {
    let spawned = std::thread::Builder::new().name("download".into()).spawn(move || {
        let Some(dir) = directories::UserDirs::new().and_then(|d| d.download_dir().map(PathBuf::from)) else {
            return eprintln!("error: no Downloads folder");
        };
        let Some(bytes) = fetch(&url) else {
            return eprintln!("error: downloading an attachment failed");
        };
        let path = unique_path(&dir, &safe_file_name(&name));
        if let Err(e) = std::fs::write(&path, bytes) {
            return eprintln!("error: saving an attachment: {e}");
        }
        let target = match kind {
            AttachmentKind::Image | AttachmentKind::Video | AttachmentKind::Audio => path.as_path(),
            AttachmentKind::File => dir.as_path(),
        };
        if let Err(e) = open::that_detached(target) {
            eprintln!("error: opening a download: {e}");
        }
    });
    if let Err(e) = spawned {
        eprintln!("error: downloading: {e}");
    }
}

/// A file name from a sender, made safe: no folders, no hidden files.
fn safe_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let clean = clean.trim().trim_start_matches('.').to_owned();
    if clean.is_empty() { "download".to_owned() } else { clean }
}

/// `dir/name`, or `dir/name (2).ext` and so on if that's taken.
fn unique_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
        _ => (name.to_owned(), String::new()),
    };
    (2..1000)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// Opens a web link in the system browser, off the UI thread.
/// Players keep running until their row is this far from the view, so a
/// little scrolling doesn't reload them.
const PLAYER_KEEP_ROWS: usize = 15;

fn open_external(url: &str) {
    if let Err(e) = open::that_detached(url) {
        eprintln!("error: opening a link: {e}");
    }
}

/// Slint's selection offsets are UTF-8 bytes. A stale/out-of-range offset
/// falls back to the end rather than splitting an encoded character.
fn insert_at_selection(text: &str, anchor: i32, cursor: i32, insertion: &str) -> (String, usize) {
    let offset = |value| {
        usize::try_from(value)
            .ok()
            .filter(|&value| value <= text.len() && text.is_char_boundary(value))
            .unwrap_or(text.len())
    };
    let (anchor, cursor) = (offset(anchor), offset(cursor));
    let (start, end) = (anchor.min(cursor), anchor.max(cursor));
    let mut result = String::with_capacity(text.len() - (end - start) + insertion.len());
    result.push_str(&text[..start]);
    result.push_str(insertion);
    result.push_str(&text[end..]);
    (result, start + insertion.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn picker_insertion_replaces_selections_without_splitting_utf8() {
        assert_eq!(insert_at_selection("A😀B", 1, 5, "👩‍💻"), ("A👩‍💻B".into(), 12));
        assert_eq!(insert_at_selection("A😀B", 5, 1, "🇫🇮"), ("A🇫🇮B".into(), 9));
        assert_eq!(insert_at_selection("ab", 1, 1, "😀"), ("a😀b".into(), 5));
        assert_eq!(insert_at_selection("😀", 2, 2, "x"), ("😀x".into(), 5));
    }

    #[test]
    fn sender_file_names_stay_in_the_folder_and_visible() {
        assert_eq!(safe_file_name("../../.bashrc"), "bashrc");
        assert_eq!(safe_file_name("C:\\Windows\\evil.exe"), "evil.exe");
        assert_eq!(safe_file_name("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(safe_file_name("..."), "download");
        assert_eq!(safe_file_name("photo.jpg"), "photo.jpg");
    }

    #[test]
    fn taken_names_get_a_number() {
        let dir = std::env::temp_dir().join(format!("godwit-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a.txt"));
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (2).txt"));
        std::fs::write(dir.join("README"), "x").unwrap();
        assert_eq!(unique_path(&dir, "README"), dir.join("README (2)"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod video_memory_tests {
    use super::video_ended;

    #[test]
    fn memory_goes_back_when_a_call_a_video_or_our_capture_ends() {
        assert!(video_ended(Some((1, false, false)), None), "the call ended");
        assert!(
            video_ended(Some((2, false, false)), Some((1, false, false))),
            "a video went"
        );
        assert!(
            video_ended(Some((0, false, true)), Some((0, false, false))),
            "our share stopped"
        );
        assert!(
            video_ended(Some((0, true, false)), Some((0, false, false))),
            "our camera stopped"
        );
        assert!(
            !video_ended(Some((1, false, false)), Some((2, true, true))),
            "things started"
        );
        assert!(!video_ended(None, Some((1, false, false))));
        assert!(!video_ended(None, None));
    }
}
