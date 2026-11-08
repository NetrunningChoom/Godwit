// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Accounts: signing in and out, the sign-in forms and their statuses, pruning signed-out slots.
//! One list for every network; what differs is in `crate::registry`.

use super::*;

impl Shell {
    /// Every account's news. An account signed in to the same user as
    /// another on its network is let go before anything of it reaches the
    /// app (both would be one account there), with its saved login.
    pub(super) fn poll_accounts(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        let mut seen = HashSet::new();
        let mut twins = Vec::new();
        for (i, a) in self.live.iter_mut().enumerate() {
            let polled = a.poll();
            if let Some(id) = a.id()
                && !seen.insert(id.clone())
            {
                twins.push(i);
                continue;
            }
            events.extend(polled);
        }
        for i in twins.into_iter().rev() {
            let twin = self.live.remove(i);
            twin.forget_saved();
            eprintln!("error: that {} account is already signed in", twin.network().name());
        }
        if let Some(demo) = &mut self.demo {
            events.extend(demo.poll());
        }
        self.prune();
        events
    }

    /// Signed-out accounts go (a log-out, a lost session, a cancelled
    /// "Add an account"), keeping any still signing in, and one for the
    /// sign-in form while it's needed: the network being added, or the
    /// default network's while nothing is signed in (or one is being added).
    pub(super) fn prune(&mut self) {
        let signed_in = self.live.iter().any(|a| a.id().is_some());
        let (adding, login_network) = (self.adding, self.login_network);
        let needs_form = |n: Network| {
            if registry::entry(n).default {
                !signed_in || adding
            } else {
                login_network == Some(n)
            }
        };
        let gone = self.live.iter().map(|a| a.id().is_none() && a.signed_out());
        let mut keep = keep_signed_out(self.live.iter().map(|a| a.network()).zip(gone), needs_form).into_iter();
        self.live.retain(|_| keep.next().unwrap_or(true));
    }

    /// The account `network`'s sign-in form drives: one not signed in and not
    /// restoring a saved session, else one restoring.
    pub(super) fn form(&self, network: Network) -> Option<&dyn Account> {
        let mine = || {
            self.live
                .iter()
                .filter(move |a| a.network() == network && a.id().is_none())
        };
        mine()
            .find(|a| a.status() != Status::Connecting)
            .or_else(|| mine().next())
            .map(|a| &**a)
    }

    /// The account `network`'s sign-in form drives, made in a free slot if
    /// there's none.
    pub(super) fn signing(&mut self, network: Network) -> Option<&mut Box<dyn Account>> {
        let form = |a: &dyn Account| a.network() == network && a.id().is_none() && a.status() != Status::Connecting;
        if !self.live.iter().any(|a| form(&**a)) {
            let entry = registry::entry(network);
            let used: HashSet<u32> = self
                .live
                .iter()
                .filter(|a| a.network() == network)
                .map(|a| a.slot())
                .collect();
            // Session slots are persistent identifiers, not a cap on how
            // many accounts can be added. Once the scanned range is full,
            // keep allocating higher slots rather than silently stopping.
            let slot = (0..entry.max_slots)
                .find(|s| !used.contains(s))
                .or_else(|| used.iter().copied().max().and_then(|s| s.checked_add(1)).or(Some(0)))?;
            match (entry.start)(self.wake.clone(), slot) {
                Ok(mut account) => {
                    account.set_notifications(self.settings.notifications || self.settings.sounds);
                    account.set_voice_settings(self.settings.voice.clone());
                    self.pushed_voice = None;
                    self.add_live(account);
                }
                Err(e) => {
                    eprintln!("error: starting a {} account: {e}", network.name());
                    return None;
                }
            }
        }
        self.live.iter_mut().find(|a| form(&***a))
    }

    /// Adds `account` after its network's others, keeping the registry's order.
    pub(super) fn add_live(&mut self, account: Box<dyn Account>) {
        let rank = |n: Network| registry::NETWORKS.iter().position(|e| e.network == n);
        let at = self
            .live
            .iter()
            .position(|a| rank(a.network()) > rank(account.network()))
            .unwrap_or(self.live.len());
        self.live.insert(at, account);
    }

    /// The sign-in form was sent (`server` isn't asked for by any network now).
    pub fn log_in(&mut self, ui: &AppWindow, _server: String, user: String, password: String) {
        let network = self.login_network.unwrap_or_else(registry::default_network);
        if let Some(a) = self.signing(network) {
            a.log_in(user, password);
        }
        self.refresh(ui);
    }

    pub fn submit_mfa(&mut self, ui: &AppWindow, code: String) {
        let network = self.login_network.unwrap_or_else(registry::default_network);
        if let Some(a) = self.signing(network) {
            a.submit_code(code);
        }
        self.refresh(ui);
    }

    /// The profile menu's "Log out": the account it shows (the focused one,
    /// see `focus_account`); with one account, that's all of them.
    pub fn log_out(&mut self) {
        let Some(id) = self.focus_account() else { return };
        if let Some(a) = self.accounts().find(|a| a.id() == Some(&id)) {
            a.log_out();
        }
        self.presenter.focus = None;
    }

    /// The account in Settings' row `row`.
    pub fn account_at(&self, row: i32) -> Option<AccountId> {
        usize::try_from(row)
            .ok()
            .and_then(|i| self.app.state().accounts.get(i))
            .map(|a| a.id.clone())
    }

    /// Settings' "Log out" on the account in row `row`.
    pub fn log_out_account(&mut self, row: i32) {
        let id = usize::try_from(row)
            .ok()
            .and_then(|i| self.app.state().accounts.get(i))
            .map(|a| a.id.clone());
        if let Some(a) = id.and_then(|id| self.accounts().find(|a| a.id() == Some(&id))) {
            a.log_out();
        }
    }

    /// Every account, in the registry's order, then the demo's.
    pub(super) fn accounts(&mut self) -> impl Iterator<Item = &mut dyn NetworkAccount> {
        let demo = self.demo.as_mut().map(|d| d as &mut dyn NetworkAccount);
        // The demo's Steam stand-in answers only for game libraries.
        let demo_steam = self.demo_steam.as_mut().map(|d| d as &mut dyn NetworkAccount);
        self.live
            .iter_mut()
            .map(|a| &mut **a as &mut dyn NetworkAccount)
            .chain(demo)
            .chain(demo_steam)
    }

    /// Keep newly signed-in accounts' authenticated media available to the
    /// worker threads and attachment downloads, including additional slots.
    pub(super) fn sync_media(&mut self) {
        let media: Vec<_> = self.accounts().filter_map(|a| a.media()).collect();
        if self.media.iter().map(|(p, _)| p).ne(media.iter().map(|(p, _)| p)) {
            self.images.set_media(media.clone());
            self.media = media;
        }
    }

    /// The account signed in as `id`.
    pub(super) fn account(&self, id: &AccountId) -> Option<&dyn NetworkAccount> {
        let demo = self.demo.as_ref().map(|d| d as &dyn NetworkAccount);
        let demo_steam = self.demo_steam.as_ref().map(|d| d as &dyn NetworkAccount);
        self.live
            .iter()
            .map(|a| &**a as &dyn NetworkAccount)
            .chain(demo)
            .chain(demo_steam)
            .find(|a| a.id() == Some(id))
    }

    /// Where `network`'s sign-in stands: its form's account; with none, done
    /// if one went through. The default network's falls back to its first
    /// account (a signed-in one reconnecting shows as connecting).
    pub(super) fn network_status(&self, network: Network) -> Status {
        if let Some(a) = self.form(network) {
            return a.status();
        }
        let mut mine = self.live.iter().filter(|a| a.network() == network);
        if registry::entry(network).default {
            return mine.next().map_or(Status::SignedOut { error: None }, |a| a.status());
        }
        if mine.any(|a| a.id().is_some()) {
            Status::Ready
        } else {
            Status::SignedOut { error: None }
        }
    }

    /// Where signing in stands: the network being signed in to, else the
    /// default network's, or "connecting" while a kept sign-in reconnects
    /// with nothing else signed in (not the picker).
    pub(super) fn sign_in_status(&self) -> Status {
        if let Some(network) = self.login_network {
            return self.network_status(network);
        }
        let status = self.network_status(registry::default_network());
        if matches!(status, Status::SignedOut { .. })
            && self.app.state().accounts.is_empty()
            && self.live.iter().any(|a| a.status() == Status::Connecting)
        {
            return Status::Connecting;
        }
        status
    }

    /// What the sign-in screen shows besides the form: a QR code, hints, a link.
    pub(super) fn show_steam_login(&mut self, ui: &AppWindow) {
        let view = self
            .login_network
            .and_then(|n| self.form(n))
            .map(|a| a.login_view())
            .unwrap_or_default();
        // A page the browser opens once (it's on this machine).
        if let Some(page) = &view.open_once
            && self.discord_captcha.as_deref() != Some(page.as_str())
        {
            open_external(page);
            self.discord_captcha = Some(page.clone());
        }
        match view.qr {
            Some(url) if self.steam_qr.as_ref().is_none_or(|(u, _)| *u != url) => {
                let image = crate::qr::image(&url);
                ui.set_login_has_qr(image.is_some());
                if let Some(image) = image {
                    ui.set_login_qr(image.clone());
                    self.steam_qr = Some((url, image));
                }
            }
            Some(_) => {}
            None => {
                if self.steam_qr.take().is_some() {
                    ui.set_login_has_qr(false);
                }
            }
        }
        ui.set_login_qr_scanned(view.scanned);
        ui.set_login_mfa_hint(view.hint.into());
        ui.set_login_link(view.link.into());
    }

    /// A link on the sign-in screen was clicked (Microsoft's sign-in page).
    pub fn open_login_link(&self, url: &str) {
        if url.starts_with("https://") {
            open_external(url);
        }
    }

    /// The sign-in screen's "Copy" buttons.
    pub fn copy_login_text(&self, text: String) {
        copy_to_clipboard(text);
    }

    /// The network picker's row `row` was chosen: its form's account starts
    /// signing in (so the others' "signed in" doesn't end it before it starts).
    pub fn pick_network(&mut self, ui: &AppWindow, row: i32) {
        let network = usize::try_from(row)
            .ok()
            .and_then(|i| view::ACCOUNT_NETWORKS.get(i).copied());
        let Some(network) = network.filter(|n| self.network_available(*n)) else {
            return;
        };
        let helper = if registry::entry(network).needs_helper {
            match crate::inline_player::player_executable() {
                Ok(helper) => Some(Some(helper)),
                Err(e) => {
                    eprintln!("error: finding the sign-in helper: {e}");
                    None
                }
            }
        } else {
            Some(None)
        };
        if let Some(helper) = helper
            && let Some(a) = self.signing(network)
        {
            a.begin_sign_in(helper);
        }
        self.login_network = Some(network);
        self.refresh(ui);
    }

    /// Back from a sign-in form to the picker, or from the picker to the app.
    pub fn login_back(&mut self, ui: &AppWindow) {
        if let Some(network) = self.login_network
            && let Some(a) = self
                .live
                .iter_mut()
                .find(|a| a.network() == network && a.id().is_none() && a.status() != Status::Connecting)
        {
            a.cancel_sign_in();
        }
        if self.login_network.take().is_none() {
            self.adding = false;
            self.prune();
        }
        self.refresh(ui);
    }

    pub fn add_account(&mut self, ui: &AppWindow) {
        self.adding = true;
        self.login_network = None;
        self.refresh(ui);
    }

    /// Whether another sign-in fits this network's platform and saved slots.
    pub(super) fn network_available(&self, n: Network) -> bool {
        let entry = registry::entry(n);
        let mut mine = self.live.iter().filter(|a| a.network() == n);
        entry.supported()
            && (mine.clone().any(|a| a.id().is_none()) || mine.by_ref().count() < entry.max_slots as usize)
    }

    /// Takes `id`'s connection down or brings it back (remembered), staying
    /// signed in. Returns false if its network can't.
    pub fn set_account_offline(&mut self, id: &AccountId, offline: bool) -> bool {
        let Some(account) = self.accounts().find(|a| a.route_id() == Some(id)) else {
            return false;
        };
        if !account.can_go_offline() {
            return false;
        }
        let events = account.set_offline(offline);
        self.settings.set_offline(id, offline);
        self.save_settings();
        self.apply_all(events);
        true
    }

    /// Settings' row `row`'s Online switch.
    pub fn set_offline_row(&mut self, ui: &AppWindow, row: i32, offline: bool) {
        if let Some(id) = self.account_at(row) {
            self.set_account_offline(&id, offline);
        }
        self.refresh(ui);
    }

    /// An account's rail row was clicked: its section collapses (its
    /// servers hide, its badge stays) or expands again; remembered.
    pub fn toggle_account(&mut self, ui: &AppWindow, row: usize) {
        // An account its network signed out: straight to signing in again.
        if let Some(view::RailRow::SignIn(network)) = self.presenter.server_rows.get(row).cloned() {
            self.add_account(ui);
            if let Some(i) = view::ACCOUNT_NETWORKS.iter().position(|n| *n == network) {
                self.pick_network(ui, i32::try_from(i).unwrap_or(0));
            }
            return;
        }
        let Some(view::RailRow::Account(id)) = self.presenter.server_rows.get(row).cloned() else {
            return;
        };
        if !self.presenter.collapsed.remove(&id) {
            self.presenter.collapsed.insert(id);
        }
        let mut keys: Vec<String> = self.presenter.collapsed.iter().map(account_key).collect();
        keys.sort();
        self.settings.collapsed = keys;
        if let Some(path) = &self.settings_path
            && let Err(e) = settings::save(path, &self.settings)
        {
            eprintln!("error: saving settings: {e}");
        }
        self.refresh(ui);
    }

    /// One of our accounts is `account` and it's signed in.
    pub fn signed_in(&self, account: &AccountId) -> bool {
        self.app.state().accounts.iter().any(|a| a.id == *account)
    }
}

/// Which accounts stay, given each one's network and whether it's signed out
/// (and not signed in): every signed-out one goes except the first of each
/// network whose sign-in form needs it.
fn keep_signed_out(accounts: impl Iterator<Item = (Network, bool)>, needs_form: impl Fn(Network) -> bool) -> Vec<bool> {
    let mut formed = HashSet::new();
    accounts
        .map(|(network, gone)| !gone || (needs_form(network) && formed.insert(network)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_keeps_one_signed_out_account() {
        let stoat = |n: Network| n == Network::Stoat;
        // Adding a second Stoat account: the new form's account stays
        // (it was pruned, so the form fell back to the signed-in one).
        let kept = keep_signed_out(
            [(Network::Stoat, false), (Network::Stoat, true), (Network::Stoat, true)].into_iter(),
            stoat,
        );
        assert_eq!(kept, [true, true, false]);
        // A network without a form open keeps no signed-out account.
        let kept = keep_signed_out([(Network::Steam, true), (Network::Steam, false)].into_iter(), stoat);
        assert_eq!(kept, [false, true]);
    }
}
