// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Profiles: switching (only the profile's accounts stay connected), and the
//! calls the switcher and the Profiles page make. What is pure (which
//! accounts stop and start) lives in `crate::profiles`.

use super::*;
use crate::profiles::{Running, plan};

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

/// Panel widths as last dragged, within the handles' limits (0: never
/// dragged, left as the window has them).
pub(super) fn apply_widths(ui: &AppWindow, settings: &Settings) {
    if settings.sidebar_width > 0.0 {
        ui.set_sidebar_w(settings.sidebar_width.clamp(200.0, 420.0));
    }
    if settings.members_width > 0.0 {
        ui.set_members_w(settings.members_width.clamp(200.0, 400.0));
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
            })
            .collect()
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
    /// disconnect and leave memory, and this profile's connect.
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
}
