// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Profiles: switching (only the profile's accounts stay connected, or with
//! soft lock the others keep running in the background, see
//! `crate::background`), and the calls the switcher and the Profiles page
//! make. What is pure (which accounts stop and start) lives in
//! `crate::profiles`.

use super::*;
use crate::profiles::{Running, background_plan, plan};
use crate::profiles_page::RowKey;
use crate::ui::ProfilesState;
use slint::ComponentHandle;

/// One profile as the switcher and the Profiles page list it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileInfo {
    pub id: u32,
    pub name: String,
    /// The letter on its tile.
    pub initial: String,
    pub current: bool,
    pub default: bool,
    /// How many accounts it has.
    pub accounts: usize,
    /// Running in the background and alerted since it was last in use.
    pub alert: bool,
}

/// The saved contacts at `path`; a damaged file is kept aside and the
/// contacts start empty (as for every start).
pub(super) fn load_contacts(path: Option<&std::path::Path>) -> Contacts {
    match path.map(contacts_store::load) {
        Some(Ok(c)) => c,
        Some(Err(e)) if e.kind() == std::io::ErrorKind::InvalidData => {
            match path.map(crate::local_recovery::preserve_corrupt) {
                Some(Ok(recovery)) => eprintln!(
                    "warning: contacts were reset; the damaged file was preserved at {}",
                    recovery.display()
                ),
                Some(Err(_)) => {
                    eprintln!("error: contacts were invalid and could not be preserved; using empty contacts")
                }
                None => eprintln!("error: contacts were invalid; using empty contacts"),
            }
            Contacts::default()
        }
        Some(Err(_)) => {
            eprintln!("error: could not read contacts; using empty contacts");
            Contacts::default()
        }
        None => Contacts::default(),
    }
}

/// The member list's drag limits; `Members`' handle in app.slint clamps to
/// the same 200 to 480 px.
pub(super) const MEMBERS_WIDTH: (f32, f32) = (200.0, 480.0);

/// The kept member-list width brought within its limits; none when never
/// dragged (or reset), so the window keeps its default.
pub(super) fn members_width(saved: f32) -> Option<f32> {
    (saved > 0.0).then(|| saved.clamp(MEMBERS_WIDTH.0, MEMBERS_WIDTH.1))
}

/// Panel widths as last dragged, within the handles' limits (0: never
/// dragged, left as the window has them).
pub(super) fn apply_widths(ui: &AppWindow, settings: &Settings) {
    if settings.sidebar_width > 0.0 {
        ui.set_sidebar_w(settings.sidebar_width.clamp(200.0, 420.0));
    }
    if let Some(width) = members_width(settings.members_width) {
        ui.set_members_w(width);
    }
}

impl Shell {
    /// Hands the current profile's view settings to the presenter (its rail
    /// order, collapsed accounts and categories, muzzles).
    pub(super) fn apply_view_settings(&mut self) {
        self.presenter.collapsed = self
            .settings
            .collapsed
            .iter()
            .filter_map(|k| parse_account_key(k))
            .collect();
        self.presenter.server_order = self.settings.server_order.clone();
        self.presenter.account_order = self.settings.account_order.clone();
        self.presenter.muzzle = self.settings.clone();
        self.presenter.collapsed_categories = self.settings.collapsed_categories.clone();
    }

    /// The profiles, in their order, for the switcher and the settings page.
    pub fn profile_list(&self) -> Vec<ProfileInfo> {
        let s = &self.settings;
        s.profiles
            .iter()
            .map(|p| ProfileInfo {
                id: p.id,
                name: p.name.clone(),
                initial: p
                    .name
                    .chars()
                    .next()
                    .map(|c| c.to_uppercase().collect())
                    .unwrap_or_default(),
                current: p.id == s.current_profile,
                default: p.id == s.default_profile,
                accounts: p.accounts.len(),
                alert: p.id != s.current_profile && self.shelves.get(&p.id).is_some_and(|b| b.unseen),
            })
            .collect()
    }

    /// Whether the other profiles stay connected (never in the demo, whose
    /// accounts belong to every profile).
    fn soft_lock(&self) -> bool {
        self.settings.background_profiles && self.demo.is_none()
    }

    /// Starts and stops background accounts so every other profile's
    /// accounts run once (see `profiles::background_plan`); with soft lock
    /// off, the background empties.
    pub(super) fn keep_background(&mut self) {
        if !self.soft_lock() {
            for (_, mut shelf) in std::mem::take(&mut self.shelves) {
                for i in (0..shelf.live.len()).rev() {
                    shelf.remove(i);
                }
            }
            return;
        }
        let shelves: Vec<(u32, Vec<Running>)> = self.shelves.iter().map(|(id, s)| (*id, s.running())).collect();
        let p = background_plan(
            &self.running(),
            &shelves,
            &self.settings.profiles,
            self.settings.current_profile,
            &self.settings.known_accounts,
        );
        // Highest index first within each shelf (the plan lists them in order).
        for &(id, i) in p.stop.iter().rev() {
            if let Some(shelf) = self.shelves.get_mut(&id) {
                shelf.remove(i);
            }
        }
        for (id, network, slot) in p.start {
            let Some(entry) = registry::NETWORKS.iter().find(|e| e.network.key() == network) else {
                continue;
            };
            match (entry.start)(self.wake.clone(), slot) {
                Ok(mut account) => {
                    account.set_notifications(self.settings.notifications || self.settings.sounds);
                    self.shelves.entry(id).or_default().live.push(account);
                }
                Err(e) => eprintln!("error: starting a {} account: {e}", entry.network.name()),
            }
        }
        self.shelves.retain(|_, s| !s.live.is_empty());
    }

    /// An account the foreground is about to start leaves the background
    /// first, so it never runs twice (what it showed there goes with it).
    fn take_from_background(&mut self, network: &str, slot: u32) {
        for shelf in self.shelves.values_mut() {
            while let Some(i) = shelf
                .live
                .iter()
                .position(|a| a.network().key() == network && a.slot() == slot)
            {
                shelf.remove(i);
            }
        }
    }

    /// Polls the background profiles: their events update their own state,
    /// and their alerts become notifications naming the profile. Returns
    /// whether a profile's dot in the menu came on.
    pub(super) fn pump_background(&mut self) -> bool {
        if self.shelves.is_empty() {
            return false;
        }
        let mut shelves = std::mem::take(&mut self.shelves);
        let mut changed = false;
        for (id, shelf) in &mut shelves {
            let events = shelf.poll();
            if events.is_empty() {
                continue;
            }
            let name = self.settings.profile(*id).map(|p| p.name.clone()).unwrap_or_default();
            for event in events {
                // Judged against the state before the event, as in the foreground.
                if let Some(n) = shelf.notice(&event, &self.settings) {
                    let (title, body) =
                        crate::background::notification_text(&name, &n, self.settings.background_previews);
                    // Do Not Disturb, the account's alert mode and muzzle
                    // mode apply as to any alert. Never on the Notifications
                    // page: that belongs to the profile in use.
                    self.alerts
                        .alert(&self.settings, &n.room, n.kind, n.from.as_ref(), &title, &body);
                    changed |= !shelf.unseen;
                    shelf.unseen = true;
                }
                if let Event::AlertDone { room } = &event {
                    self.alerts.done(room);
                }
                shelf.apply(event, &|a| self.settings.is_offline(a));
            }
        }
        // Nothing may have been added meanwhile (the pump doesn't switch).
        self.shelves = shelves;
        changed
    }

    /// A notification was clicked: its chat opens, in front. One from a
    /// background profile switches to that profile first (asking only
    /// when a call would end).
    pub(super) fn open_alert(&mut self, ui: &AppWindow, room: RoomId) {
        if self.app.state().room(&room).is_none() {
            let Some(id) = self
                .shelves
                .iter()
                .find(|(_, s)| s.app.state().room(&room).is_some())
                .map(|(id, _)| *id)
            else {
                return;
            };
            if self.app.state().call.is_some() {
                self.pending_switch_room = Some(room);
                self.open_switch_dialog(ui, id);
                show_window(ui);
                return;
            }
            self.switch_profile(ui, id);
            if self.app.state().room(&room).is_none() {
                return;
            }
        }
        self.act(ui, Action::SelectRoom(room));
        show_window(ui);
    }

    /// Requests the encrypted cache for accounts that came in with a
    /// background profile (as on `AccountReady`): it fills only what they
    /// haven't loaded.
    fn load_cache_for_restored(&mut self) {
        let ids: Vec<AccountId> = self.app.state().accounts.iter().map(|a| a.id.clone()).collect();
        for id in ids {
            if !self.cached_accounts.insert(id.clone()) {
                continue;
            }
            let loading = self.cache.as_ref().is_some_and(|c| c.request_load(&id).is_ok());
            if !loading {
                self.cache_loads_finished.insert(id);
            }
        }
    }

    /// Soft lock: the profile in use goes on its shelf, whole, and `to`'s
    /// comes off (an empty one if it has none yet), so its accounts are
    /// there at once with everything they showed.
    fn swap_shelf(&mut self, from: u32, to: u32) {
        let incoming = self.shelves.remove(&to).unwrap_or_default();
        // What the old profile's accounts showed stays in the message cache.
        self.save_cache();
        let app = std::mem::replace(&mut self.app, incoming.app);
        let live = std::mem::replace(&mut self.live, incoming.live);
        for a in &app.state().accounts {
            self.cached_accounts.remove(&a.id);
            self.cache_loads_finished.remove(&a.id);
            self.saved_cache.remove(&a.id);
        }
        self.shelves.insert(
            from,
            crate::background::Shelf {
                app,
                live,
                unseen: false,
            },
        );
        self.load_cache_for_restored();
    }

    /// For the switch confirmation: what a switch costs beyond the accounts
    /// that disconnect (the demo's accounts never do).
    pub fn call_note(&self) -> Option<&'static str> {
        self.app
            .state()
            .call
            .is_some()
            .then_some("You are in a call; switching profiles ends it.")
    }

    /// Whether `key` (`network:user`) is in profile `id`, for the page's toggles.
    pub fn profile_has(&self, id: u32, key: &str) -> bool {
        self.settings
            .profile(id)
            .is_some_and(|p| p.accounts.iter().any(|k| k == key))
    }

    fn running(&self) -> Vec<Running> {
        self.live
            .iter()
            .map(|a| Running {
                key: a.id().map(account_key),
                network: a.network().key().to_owned(),
                slot: a.slot(),
            })
            .collect()
    }

    /// Stops the accounts the current profile doesn't have and starts the
    /// ones it has that aren't running. Dropping an account ends its network
    /// thread (it owns the only command sender); its state leaves the app.
    fn reconcile_accounts(&mut self) {
        if self.demo.is_some() {
            // Demo accounts belong to every profile.
            return;
        }
        let Some(profile) = self.settings.profile(self.settings.current_profile).cloned() else {
            return;
        };
        let p = plan(&self.running(), &profile, &self.settings.known_accounts);
        if !p.stop.is_empty() {
            // What each account last showed stays in the message cache.
            self.save_cache();
        }
        let mut gone = Vec::new();
        for &i in p.stop.iter().rev() {
            let mut account = self.live.remove(i);
            match account.id() {
                Some(id) => gone.push(id.clone()),
                // A sign-in form, or an account that never got ready: cancelled.
                None => account.cancel_sign_in(),
            }
        }
        for id in &gone {
            self.app.unload_account(id);
            // Not `cache.remove`: that is for a sign-out. Loaded afresh when it returns.
            self.cached_accounts.remove(id);
            self.cache_loads_finished.remove(id);
            self.saved_cache.remove(id);
            self.alerts.forget_account(id);
        }
        if self.rejoin.as_ref().is_some_and(|r| gone.contains(&r.account)) {
            self.rejoin = None;
        }
        for (network, slot) in p.start {
            let Some(entry) = registry::NETWORKS.iter().find(|e| e.network.key() == network) else {
                continue;
            };
            // Soft lock: it was in the background (another profile has it
            // too); its state there doesn't move, so it starts afresh here.
            self.take_from_background(&network, slot);
            match (entry.start)(self.wake.clone(), slot) {
                Ok(mut account) => {
                    account.set_notifications(self.settings.notifications || self.settings.sounds);
                    account.set_voice_settings(self.settings.voice.clone());
                    self.pushed_voice = None;
                    self.add_live(account);
                }
                Err(e) => eprintln!("error: starting a {} account: {e}", entry.network.name()),
            }
        }
        // Nothing running: a calm empty state, not a sign-in form (the
        // profile may just have no accounts; "Add an account" opens the picker).
        self.empty_profile = self.live.is_empty();
        // What left the foreground may belong in the background, and the
        // other profiles' lists may have changed.
        self.keep_background();
    }

    /// Closes Settings and every editor in it. They may refer to an account
    /// that was just unloaded, and a page of the old profile means nothing in
    /// the new one.
    fn close_settings(&mut self, ui: &AppWindow) {
        ui.set_settings_open(false);
        ui.set_settings_account(-1);
        // The server and channel editors close with the core's state
        // (`reset_view`) on the next refresh.
        self.profile_editor = Default::default();
        self.autodelete_page = Default::default();
        self.account_pages = Default::default();
        self.privacy_sources.clear();
        self.report_links.clear();
        self.close_join_server(ui);
    }

    /// Makes profile `id` the one in use (the UI has asked first, see
    /// `call_note`): an ongoing call ends, the other profiles' accounts
    /// disconnect and leave memory (or, with soft lock, go to the
    /// background), and this profile's connect.
    pub fn switch_profile(&mut self, ui: &AppWindow, id: u32) {
        if id == self.settings.current_profile || self.settings.profile(id).is_none() {
            return;
        }
        if self.app.state().call.is_some() {
            self.act(ui, Action::LeaveCall);
        }
        let old = self.settings.current_profile;
        // Drafts belong to their profile for this session.
        let drafts = self.app.take_drafts();
        self.stashed_drafts.insert(old, drafts);
        // The old profile's contacts are written before the new one's load.
        if let Some(path) = &self.contacts_path
            && let Err(e) = contacts_store::save(path, &self.app.state().contacts)
        {
            eprintln!("error: saving contacts: {e}");
        }
        self.settings.switch_view(id);
        if self.soft_lock() {
            self.swap_shelf(old, id);
        }
        if self.demo.is_none() {
            self.contacts_path = contacts_store::path_for(id);
        }
        // The demo's made-up contacts are in no file.
        if self.demo.is_none() {
            let contacts = load_contacts(self.contacts_path.as_deref());
            self.app.set_contacts(contacts);
        }
        // Sign-in forms are cancelled; a form's state goes with it.
        self.adding = false;
        self.login_network = None;
        self.mfa_form = false;
        self.steam_qr = None;
        self.discord_captcha = None;
        self.reconcile_accounts();
        // Nothing of the old profile stays open or pending.
        self.app.reset_view();
        self.notifications.clear();
        self.recent_alerts.clear();
        self.incoming_call = None;
        // Its ringtone too: a shared account's call would ring with no prompt.
        self.alerts.stop_ringing();
        self.close_settings(ui);
        self.quiet_notice = None;
        self.viewer = None;
        self.crop = None;
        self.thread_from = None;
        self.forward = None;
        self.share_dialog = None;
        self.picker_for = None;
        self.report = None;
        self.navigation_back.clear();
        self.navigation_forward.clear();
        self.games.clear();
        self.selection.clear();
        self.card_selection.clear();
        self.presenter.focus = None;
        self.presenter.open_folders.clear();
        if let Some(drafts) = self.stashed_drafts.remove(&id) {
            self.app.restore_drafts(drafts);
        }
        self.apply_view_settings();
        apply_widths(ui, &self.settings);
        let keys = |list: &[String]| {
            list.iter()
                .filter_map(|k| settings::parse_room_key(k))
                .collect::<Vec<_>>()
        };
        let (pinned, hidden) = (keys(&self.settings.pinned_rooms), keys(&self.settings.hidden_rooms));
        self.app.dispatch(Action::RestoreRoomPrefs { pinned, hidden });
        self.save_settings();
        self.refresh(ui);
    }

    /// A newly ready account: remembered with its slot, and in the current
    /// profile unless some profile has it (an old sign-in seen for the first
    /// time, or one just added).
    pub(super) fn record_ready(&mut self, account: &AccountId) {
        if self.demo.is_some() {
            return;
        }
        let Some(slot) = self.live.iter().find(|a| a.id() == Some(account)).map(|a| a.slot()) else {
            return;
        };
        let key = account_key(account);
        let before = (self.settings.known_accounts.clone(), self.settings.profiles.clone());
        self.settings.record_known(&key, account.network.key(), slot);
        self.settings.profile_of_new_account(&key);
        if (self.settings.known_accounts.clone(), self.settings.profiles.clone()) != before {
            self.save_settings();
        }
    }

    /// A new, empty profile; returns its id.
    pub fn create_profile(&mut self, name: &str) -> u32 {
        let id = self.settings.create_profile(name);
        self.save_settings();
        id
    }

    pub fn rename_profile(&mut self, id: u32, name: &str) {
        if self.settings.rename_profile(id, name) {
            self.save_settings();
        }
    }

    /// Deletes a profile and its contacts file (its accounts stay signed
    /// in, just in no profile). Deleting the profile in use switches first.
    pub fn delete_profile(&mut self, ui: &AppWindow, id: u32) {
        if self.settings.profiles.len() < 2 || self.settings.profile(id).is_none() {
            return;
        }
        if id == self.settings.current_profile {
            let to = if id != self.settings.default_profile {
                self.settings.default_profile
            } else {
                self.settings.profiles.iter().find(|p| p.id != id).map_or(id, |p| p.id)
            };
            self.switch_profile(ui, to);
        }
        if self.settings.delete_profile(id) {
            self.stashed_drafts.remove(&id);
            // A reused id would otherwise inherit these contacts; none is
            // fine (it was never written).
            if self.demo.is_none()
                && let Some(path) = contacts_store::path_for(id)
                && let Err(e) = std::fs::remove_file(&path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                eprintln!("error: removing a profile's contacts: {e}");
            }
            self.save_settings();
            self.refresh(ui);
        }
    }

    pub fn set_default_profile(&mut self, id: u32) {
        if self.settings.profile(id).is_some() {
            self.settings.default_profile = id;
            self.save_settings();
        }
    }

    /// Puts account `key` in (or takes it out of) profile `id`. In the
    /// profile in use that connects or disconnects it at once.
    pub fn set_account_in_profile(&mut self, ui: &AppWindow, id: u32, key: &str, member: bool) {
        if member {
            self.settings.assign_account(id, key);
        } else {
            self.settings.unassign_account(id, key);
        }
        if id == self.settings.current_profile {
            self.reconcile_accounts();
        }
        self.save_settings();
        self.refresh(ui);
    }

    /// The rail's tile and menu, and Settings → Profiles while it is open.
    pub(super) fn show_profiles(&mut self, ui: &AppWindow) {
        let infos = self.profile_list();
        self.profiles_view.show_rail(ui, &infos);
        // A pending confirmation follows its profile: renamed, or gone.
        let state = ui.global::<ProfilesState>();
        state.set_ask_switch(self.settings.ask_profile_switch);
        state.set_background(self.soft_lock());
        state.set_previews(self.settings.background_previews);
        if let Some(id) = self.pending_switch {
            match infos.iter().find(|p| p.id == id) {
                Some(p) if !p.current => {
                    ui.set_profile_switch_name(p.name.as_str().into());
                    ui.set_profile_switch_note(self.call_note().unwrap_or_default().into());
                }
                _ => self.cancel_profile_switch(ui),
            }
        }
        match self.pending_delete.and_then(|id| infos.iter().find(|p| p.id == id)) {
            Some(p) if ui.get_settings_open() && infos.len() > 1 => {
                state.set_removing_name(p.name.as_str().into());
                state.set_removing_note(delete_note(p.current).into());
            }
            _ => {
                self.pending_delete = None;
                state.set_removing_name("".into());
            }
        }
        let live: std::collections::HashMap<String, (String, String)> = self
            .app
            .state()
            .accounts
            .iter()
            .map(|a| {
                (
                    account_key(&a.id),
                    (
                        a.id.network.name().to_string(),
                        format!("{} (@{})", a.display_name, a.username),
                    ),
                )
            })
            .collect();
        let keys = self
            .settings
            .known_accounts
            .iter()
            .map(|k| k.key.clone())
            .chain(self.settings.profiles.iter().flat_map(|p| p.accounts.iter().cloned()))
            .chain(live.keys().cloned());
        let entries = crate::profiles_page::entries(keys, &live, |network| {
            registry::NETWORKS
                .iter()
                .find(|e| e.network.key() == network)
                .map(|e| e.network.name().to_string())
                .unwrap_or_default()
        });
        // Demo accounts belong to every profile (and are never unloaded).
        let demo = self.demo.is_some();
        let rows = crate::profiles_page::rows(&infos, &entries, |id, key| demo || self.profile_has(id, key));
        self.profiles_view.show_page(rows);
    }

    /// A menu row was chosen: ask before switching (it disconnects
    /// accounts), unless asking was turned off; a call ending always asks.
    pub fn ask_switch_profile(&mut self, ui: &AppWindow, row: i32) {
        let infos = self.profile_list();
        let Some(p) = usize::try_from(row).ok().and_then(|i| infos.get(i)) else {
            return;
        };
        if p.current {
            return;
        }
        if !needs_asking(self.settings.ask_profile_switch, self.call_note().is_some()) {
            self.switch_profile(ui, p.id);
            return;
        }
        self.open_switch_dialog(ui, p.id);
    }

    fn open_switch_dialog(&mut self, ui: &AppWindow, id: u32) {
        let Some(name) = self.settings.profile(id).map(|p| p.name.clone()) else {
            return;
        };
        self.pending_switch = Some(id);
        ui.set_profile_switch_name(name.as_str().into());
        ui.set_profile_switch_note(self.call_note().unwrap_or_default().into());
        let state = ui.global::<ProfilesState>();
        state.set_dont_ask(false);
        state.set_background(self.soft_lock());
        ui.set_profile_switch_open(true);
    }

    pub fn confirm_profile_switch(&mut self, ui: &AppWindow) {
        ui.set_profile_switch_open(false);
        // "Don't ask again": Settings → Profiles turns asking back on.
        if ui.global::<ProfilesState>().get_dont_ask() && self.settings.ask_profile_switch {
            self.settings.ask_profile_switch = false;
            self.save_settings();
        }
        let room = self.pending_switch_room.take();
        if let Some(id) = self.pending_switch.take() {
            self.switch_profile(ui, id);
        }
        if let Some(room) = room.filter(|r| self.app.state().room(r).is_some()) {
            self.act(ui, Action::SelectRoom(room));
        }
    }

    pub fn cancel_profile_switch(&mut self, ui: &AppWindow) {
        self.pending_switch = None;
        self.pending_switch_room = None;
        ui.set_profile_switch_open(false);
    }

    /// Settings → Profiles' switches.
    pub fn set_ask_profile_switch(&mut self, ui: &AppWindow, on: bool) {
        self.settings.ask_profile_switch = on;
        self.save_settings();
        self.refresh(ui);
    }

    /// Soft lock on: the other profiles' accounts connect in the
    /// background; off: they disconnect.
    pub fn set_background_profiles(&mut self, ui: &AppWindow, on: bool) {
        self.settings.background_profiles = on;
        self.save_settings();
        self.keep_background();
        self.refresh(ui);
    }

    pub fn set_background_previews(&mut self, ui: &AppWindow, on: bool) {
        self.settings.background_previews = on;
        self.save_settings();
        self.refresh(ui);
    }

    /// The Profiles page's calls, by the row they came from.
    pub fn profile_page_rename(&mut self, ui: &AppWindow, row: i32, name: &str) {
        if let Some(&RowKey::Profile(id)) = self.profiles_view.key(row)
            && self.settings.profile(id).is_some_and(|p| p.name != name.trim())
        {
            self.rename_profile(id, name);
        }
        // An empty or unchanged name goes back to what it was.
        self.refresh(ui);
    }

    pub fn profile_page_default(&mut self, ui: &AppWindow, row: i32) {
        if let Some(&RowKey::Profile(id)) = self.profiles_view.key(row) {
            self.set_default_profile(id);
            self.refresh(ui);
        }
    }

    pub fn profile_page_ask_delete(&mut self, ui: &AppWindow, row: i32) {
        if let Some(&RowKey::Profile(id)) = self.profiles_view.key(row) {
            self.pending_delete = Some(id);
            self.show_profiles(ui);
        }
    }

    pub fn profile_page_cancel_delete(&mut self, ui: &AppWindow) {
        self.pending_delete = None;
        self.show_profiles(ui);
    }

    /// Confirmed: deletes the profile that was asked about (still checked,
    /// the list may have changed since).
    pub fn profile_page_delete(&mut self, ui: &AppWindow) {
        if let Some(id) = self.pending_delete.take() {
            self.delete_profile(ui, id);
        }
        self.show_profiles(ui);
    }

    pub fn profile_page_create(&mut self, ui: &AppWindow, name: &str) {
        if name.trim().is_empty() {
            return;
        }
        self.create_profile(name);
        ui.global::<ProfilesState>().set_new_name("".into());
        self.refresh(ui);
    }

    pub fn profile_page_member(&mut self, ui: &AppWindow, row: i32, member: bool) {
        if let Some(RowKey::Account(id, key)) = self.profiles_view.key(row).cloned() {
            self.set_account_in_profile(ui, id, &key, member);
        }
    }
}

/// Whether a switch asks first: when asking is on, and always when it would
/// end a call.
fn needs_asking(ask: bool, in_call: bool) -> bool {
    ask || in_call
}

fn show_window(ui: &AppWindow) {
    if let Err(e) = slint::ComponentHandle::show(ui) {
        eprintln!("error: showing the window: {e}");
    }
}

/// What the delete confirmation adds for the profile in use.
fn delete_note(current: bool) -> &'static str {
    if current {
        "You are using this profile: Godwit switches to another first."
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_switch_asks_unless_turned_off_but_always_for_a_call() {
        assert!(needs_asking(true, false));
        assert!(!needs_asking(false, false));
        assert!(needs_asking(false, true));
    }

    #[test]
    fn the_member_width_is_kept_within_its_limits() {
        assert_eq!(members_width(0.0), None);
        assert_eq!(members_width(-5.0), None);
        assert_eq!(members_width(100.0), Some(200.0));
        assert_eq!(members_width(300.0), Some(300.0));
        assert_eq!(members_width(900.0), Some(480.0));
    }
}
