// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The startup registration: the one place the app names the network crates.
//! Each network's account type is registered here with how to start it and
//! its saved sign-ins, and [`Account`] puts its sign-in steps in the shell's
//! neutral terms, so the shell keeps one list of accounts and one sign-in flow.

use std::path::PathBuf;

use godwit_core::account::NetworkAccount;
use godwit_core::backend::Waker;
use godwit_core::ids::{RoomId, UserId};
use godwit_core::network::Network;
use godwit_voice::call::VoiceSettings;

use godwit_discord::account::{DiscordAccount, Status as DiscordStatus};
use godwit_fluxer::account::{FluxerAccount, Status as FluxerStatus};
use godwit_gchat::account::{GChatAccount, Status as GChatStatus};
use godwit_slack::account::{SlackAccount, Status as SlackStatus};
use godwit_steam::account::{Status as SteamStatus, SteamAccount};
use godwit_stoat::account::{Status as StoatStatus, StoatAccount};
use godwit_teams::account::{Status as TeamsStatus, TeamsAccount};

/// Where a sign-in stands, as the sign-in screens show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// The form (or QR code, or code to enter) shows; `error` says why the
    /// last attempt failed.
    SignedOut {
        error: Option<String>,
    },
    /// A request is out (or the network's app is asked to approve).
    LoggingIn,
    /// A code is asked for (two-factor, Steam Guard).
    NeedsMfa {
        error: Option<String>,
    },
    /// Signed in; connecting.
    Connecting,
    Ready,
}

/// What the sign-in screen shows besides the form.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoginView {
    /// A QR code's contents, to scan with the network's phone app.
    pub qr: Option<String>,
    pub scanned: bool,
    /// A line under the form (a code to enter, what to do next).
    pub hint: String,
    /// A page to open (Microsoft's device sign-in), as a link.
    pub link: String,
    /// A page the browser should open once (Discord's captcha).
    pub open_once: Option<String>,
    /// The form can accept an optional server endpoint.
    pub custom_server: bool,
}

/// An account as the shell drives it: the shared trait, plus signing in and
/// the settings the app pushes. Everything defaults to "not on this network".
pub trait Account: NetworkAccount {
    /// The saved sign-in's slot.
    fn slot(&self) -> u32;
    /// Forgets the saved sign-in in this account's slot.
    fn forget_saved(&self);
    fn status(&self) -> Status;
    /// Signed out for good (pruned unless the form needs it). Networks whose
    /// waiting steps also read as "signed out" leave this false.
    fn signed_out(&self) -> bool {
        false
    }
    fn login_view(&self) -> LoginView {
        LoginView::default()
    }
    /// The network was picked: start its sign-in (a QR code, a code, a
    /// window; `helper` is the sign-in window's binary where one is needed).
    fn begin_sign_in(&mut self, _helper: Option<PathBuf>) {}
    fn log_in(&mut self, _user: String, _password: String) {}
    /// The sign-in form's server field, where `LoginView::custom_server`.
    fn set_server(&mut self, _server: String) {}
    fn submit_code(&mut self, _code: String) {}
    fn cancel_sign_in(&mut self) {}
    fn set_voice_settings(&mut self, _settings: VoiceSettings) {}
    fn set_notifications(&mut self, _on: bool) {}
    /// `user` is one of ours (Slack: a workspace's own user ids).
    fn is_ours(&self, _user: &UserId) -> bool {
        false
    }
    fn notifications_muted(&self, _room: &RoomId) -> bool {
        false
    }
    /// Asks the first-run privacy question (Discord; never the demo).
    fn asks_privacy(&self) -> bool {
        false
    }
    fn tighten_privacy(&mut self) {}
    /// A line about the sign-in for its settings page.
    fn detail(&self) -> Option<String> {
        None
    }
}

/// A registered network.
pub struct Entry {
    pub network: Network,
    pub max_slots: u32,
    /// Slots with a saved sign-in, to start again.
    pub saved: fn() -> Vec<u32>,
    pub start: fn(Waker, u32) -> std::io::Result<Box<dyn Account>>,
    /// Signs in in the player helper's window (WebKitGTK, WebView2), so only
    /// where that runs.
    pub needs_helper: bool,
    /// The network signed in to by default: it keeps a spare account for
    /// the first form (Stoat, per the mission).
    pub default: bool,
    /// The demo's stand-in, when the demo has one.
    pub inert: Option<fn() -> Box<dyn Account>>,
}

impl Entry {
    /// Whether another sign-in can be added on this platform at all.
    pub fn supported(&self) -> bool {
        !self.needs_helper || cfg!(any(target_os = "linux", windows))
    }
}

fn boxed<A: Account + 'static>(a: std::io::Result<A>) -> std::io::Result<Box<dyn Account>> {
    a.map(|a| Box::new(a) as Box<dyn Account>)
}

/// Every network, in the order accounts are listed (Stoat's first).
pub const NETWORKS: [Entry; 7] = [
    Entry {
        network: Network::Stoat,
        max_slots: godwit_stoat::session_file::MAX_SLOTS,
        saved: godwit_stoat::session_file::slots,
        start: |wake, slot| boxed(StoatAccount::start(wake, slot)),
        needs_helper: false,
        default: true,
        inert: Some(|| Box::new(StoatAccount::inert())),
    },
    Entry {
        network: Network::Steam,
        max_slots: godwit_steam::session::MAX_SLOTS,
        saved: godwit_steam::session::slots,
        start: |wake, slot| boxed(SteamAccount::start(wake, slot)),
        needs_helper: false,
        default: false,
        inert: None,
    },
    Entry {
        network: Network::Fluxer,
        max_slots: godwit_fluxer::session::MAX_SLOTS,
        saved: godwit_fluxer::session::slots,
        start: |wake, slot| boxed(FluxerAccount::new(slot, wake)),
        needs_helper: false,
        default: false,
        inert: None,
    },
    Entry {
        network: Network::Teams,
        max_slots: godwit_teams::session::MAX_SLOTS,
        saved: godwit_teams::session::slots,
        start: |wake, slot| boxed(TeamsAccount::new(slot, wake)),
        needs_helper: false,
        default: false,
        inert: None,
    },
    Entry {
        network: Network::Slack,
        max_slots: godwit_slack::session::MAX_SLOTS,
        saved: godwit_slack::session::slots,
        start: |wake, slot| boxed(SlackAccount::new(slot, wake)),
        needs_helper: true,
        default: false,
        inert: None,
    },
    Entry {
        network: Network::GoogleChat,
        max_slots: godwit_gchat::session::MAX_SLOTS,
        saved: godwit_gchat::session::slots,
        start: |wake, slot| boxed(GChatAccount::new(slot, wake)),
        needs_helper: true,
        default: false,
        inert: None,
    },
    Entry {
        network: Network::Discord,
        max_slots: godwit_discord::session::MAX_SLOTS,
        saved: godwit_discord::session::slots,
        start: |wake, slot| boxed(DiscordAccount::start(wake, slot)),
        needs_helper: false,
        default: false,
        inert: None,
    },
];

/// The order the "Add an account" page offers networks in.
pub const PICKER: [Network; 7] = [
    Network::Stoat,
    Network::Steam,
    Network::Discord,
    Network::Fluxer,
    Network::Teams,
    Network::Slack,
    Network::GoogleChat,
];

pub fn entry(network: Network) -> &'static Entry {
    NETWORKS.iter().find(|e| e.network == network).unwrap_or(&NETWORKS[0])
}

/// The network signed in to when none was picked.
pub fn default_network() -> Network {
    NETWORKS
        .iter()
        .find(|e| e.default)
        .map_or(Network::Stoat, |e| e.network)
}

/// The demo's Discord account: its real bridge on made-up data.
pub fn demo_discord(
    ready: serde_json::Value,
    history: std::collections::HashMap<String, Vec<serde_json::Value>>,
) -> (Box<dyn Account>, Vec<godwit_core::application::Event>) {
    let (account, events) = DiscordAccount::demo(ready, history);
    (Box::new(account), events)
}

/// What only a network's crate knows, for the shared code to ask by network.
pub fn register_hooks() {
    godwit_core::hooks::register(Network::Steam, godwit_steam::media::hooks());
    godwit_core::hooks::register(Network::Discord, godwit_discord::client::hooks());
    godwit_core::hooks::register(Network::Teams, godwit_teams::emoji::hooks());
}

impl Account for StoatAccount {
    fn slot(&self) -> u32 {
        StoatAccount::slot(self)
    }
    fn forget_saved(&self) {
        godwit_stoat::session_file::clear(StoatAccount::slot(self));
    }
    fn status(&self) -> Status {
        match StoatAccount::status(self) {
            StoatStatus::SignedOut { error } => Status::SignedOut { error: error.clone() },
            StoatStatus::LoggingIn => Status::LoggingIn,
            StoatStatus::NeedsMfa { error } => Status::NeedsMfa { error: error.clone() },
            StoatStatus::Connecting => Status::Connecting,
            StoatStatus::Ready => Status::Ready,
        }
    }
    fn signed_out(&self) -> bool {
        matches!(StoatAccount::status(self), StoatStatus::SignedOut { .. })
    }
    fn login_view(&self) -> LoginView {
        LoginView {
            custom_server: true,
            ..LoginView::default()
        }
    }
    fn detail(&self) -> Option<String> {
        StoatAccount::instance_host(self)
    }
    fn log_in(&mut self, user: String, password: String) {
        StoatAccount::log_in(self, user, password);
    }
    fn set_server(&mut self, server: String) {
        StoatAccount::set_server(self, server);
    }
    fn submit_code(&mut self, code: String) {
        self.submit_mfa(code);
    }
    fn set_voice_settings(&mut self, settings: VoiceSettings) {
        StoatAccount::set_voice_settings(self, settings);
    }
    fn set_notifications(&mut self, on: bool) {
        self.notifications = on;
    }
}

impl Account for SteamAccount {
    fn slot(&self) -> u32 {
        SteamAccount::slot(self)
    }
    fn forget_saved(&self) {
        godwit_steam::session::clear(SteamAccount::slot(self));
    }
    /// A QR code or the password form is "signed out" (the form shows), a
    /// code is MFA, and waiting for the app's approval is busy.
    fn status(&self) -> Status {
        match SteamAccount::status(self) {
            SteamStatus::SignedOut { error } => Status::SignedOut { error: error.clone() },
            SteamStatus::Qr { .. } => Status::SignedOut { error: None },
            SteamStatus::NeedsCode { error, .. } => Status::NeedsMfa { error: error.clone() },
            SteamStatus::NeedsApproval { .. } | SteamStatus::LoggingIn => Status::LoggingIn,
            SteamStatus::Connecting => Status::Connecting,
            SteamStatus::Ready => Status::Ready,
        }
    }
    fn signed_out(&self) -> bool {
        matches!(SteamAccount::status(self), SteamStatus::SignedOut { .. })
    }
    fn login_view(&self) -> LoginView {
        match SteamAccount::status(self) {
            SteamStatus::Qr { url, scanned } => LoginView {
                qr: url.clone(),
                scanned: *scanned,
                ..LoginView::default()
            },
            SteamStatus::NeedsCode { hint, .. } | SteamStatus::NeedsApproval { hint } => LoginView {
                hint: hint.clone(),
                ..LoginView::default()
            },
            _ => LoginView::default(),
        }
    }
    /// Steam starts with a QR code for its app (the password form is below it).
    fn begin_sign_in(&mut self, _helper: Option<PathBuf>) {
        self.begin_qr();
    }
    fn log_in(&mut self, user: String, password: String) {
        SteamAccount::log_in(self, user, password);
    }
    fn submit_code(&mut self, code: String) {
        SteamAccount::submit_code(self, code);
    }
    fn cancel_sign_in(&mut self) {
        self.cancel_login();
    }
    fn set_voice_settings(&mut self, settings: VoiceSettings) {
        SteamAccount::set_voice_settings(self, settings);
    }
    /// Its device name, to find (or revoke) the sign-in in Steam's list.
    fn detail(&self) -> Option<String> {
        let name = self.device_name();
        (!name.is_empty()).then(|| format!("Shows as \"{name}\" in Steam's list of signed-in devices"))
    }
}

impl Account for DiscordAccount {
    fn slot(&self) -> u32 {
        DiscordAccount::slot(self)
    }
    /// Only the slot is forgotten; a twin's session is the same sign-in as
    /// the one kept.
    fn forget_saved(&self) {
        godwit_discord::session::clear(DiscordAccount::slot(self));
    }
    /// The QR code is the form ("signed out"), then connecting.
    fn status(&self) -> Status {
        match DiscordAccount::status(self) {
            DiscordStatus::Qr { .. } => Status::SignedOut { error: None },
            DiscordStatus::NeedsCode { error, .. } => Status::NeedsMfa { error: error.clone() },
            DiscordStatus::LoggingIn => Status::LoggingIn,
            DiscordStatus::SignedOut { error } => Status::SignedOut { error: error.clone() },
            DiscordStatus::Connecting => Status::Connecting,
            DiscordStatus::Ready => Status::Ready,
        }
    }
    fn signed_out(&self) -> bool {
        matches!(DiscordAccount::status(self), DiscordStatus::SignedOut { .. })
    }
    fn login_view(&self) -> LoginView {
        match DiscordAccount::status(self) {
            DiscordStatus::NeedsCode { hint, .. } => LoginView {
                hint: hint.clone(),
                ..LoginView::default()
            },
            DiscordStatus::Qr { url, scanned, captcha } => {
                let hint = match (captcha, scanned) {
                    (Some(page), _) => format!(
                        "Discord wants to check you're a person: solve the captcha that opened in your browser ({page})."
                    ),
                    (None, Some(name)) => format!("Scanned by {name}. Confirm the sign-in on your phone."),
                    (None, None) => String::new(),
                };
                LoginView {
                    qr: url.clone(),
                    scanned: !hint.is_empty(),
                    hint,
                    open_once: captcha.clone(),
                    ..LoginView::default()
                }
            }
            _ => LoginView::default(),
        }
    }
    /// Discord offers QR and password sign-in on the same form.
    fn begin_sign_in(&mut self, _helper: Option<PathBuf>) {
        self.link();
    }
    fn log_in(&mut self, user: String, password: String) {
        DiscordAccount::password_login(self, user, password);
    }
    fn submit_code(&mut self, code: String) {
        DiscordAccount::submit_login_code(self, code);
    }
    fn cancel_sign_in(&mut self) {
        self.cancel_link();
    }
    fn set_voice_settings(&mut self, settings: VoiceSettings) {
        DiscordAccount::set_voice_settings(self, settings);
    }
    fn asks_privacy(&self) -> bool {
        // The demo's account sits past the real slots.
        DiscordAccount::slot(self) < godwit_discord::session::MAX_SLOTS
    }
    fn tighten_privacy(&mut self) {
        DiscordAccount::tighten_privacy(self);
    }
}

impl Account for FluxerAccount {
    fn slot(&self) -> u32 {
        FluxerAccount::slot(self)
    }
    fn forget_saved(&self) {
        godwit_fluxer::session::clear(FluxerAccount::slot(self));
    }
    /// Stoat's terms: the same email and password form, then the code.
    fn status(&self) -> Status {
        match FluxerAccount::status(self) {
            FluxerStatus::SignedOut { error } => Status::SignedOut { error: error.clone() },
            FluxerStatus::NeedsCode { error } => Status::NeedsMfa { error: error.clone() },
            FluxerStatus::Connecting => Status::Connecting,
            FluxerStatus::Ready => Status::Ready,
        }
    }
    fn log_in(&mut self, user: String, password: String) {
        FluxerAccount::log_in(self, user, password);
    }
    fn submit_code(&mut self, code: String) {
        FluxerAccount::submit_code(self, code);
    }
}

impl Account for TeamsAccount {
    fn slot(&self) -> u32 {
        TeamsAccount::slot(self)
    }
    fn forget_saved(&self) {
        godwit_teams::session::clear(TeamsAccount::slot(self));
    }
    /// The code is the form ("signed out", as Discord's QR code), then connecting.
    fn status(&self) -> Status {
        match TeamsAccount::status(self) {
            TeamsStatus::Starting | TeamsStatus::Code { .. } => Status::SignedOut { error: None },
            TeamsStatus::SignedOut { error } => Status::SignedOut { error: error.clone() },
            TeamsStatus::Connecting => Status::Connecting,
            TeamsStatus::Ready => Status::Ready,
        }
    }
    /// No QR code (Microsoft's device sign-in has none): the code, and its
    /// page as a link to click or copy.
    fn login_view(&self) -> LoginView {
        match TeamsAccount::status(self) {
            TeamsStatus::Code { code, url } => LoginView {
                hint: code.clone(),
                link: if url.starts_with("https://") {
                    url.clone()
                } else {
                    String::new()
                },
                ..LoginView::default()
            },
            _ => LoginView::default(),
        }
    }
    /// Teams signs in with a code entered on Microsoft's page.
    fn begin_sign_in(&mut self, _helper: Option<PathBuf>) {
        self.sign_in();
    }
    fn cancel_sign_in(&mut self) {
        TeamsAccount::cancel_sign_in(self);
    }
}

impl Account for SlackAccount {
    fn slot(&self) -> u32 {
        SlackAccount::slot(self)
    }
    fn forget_saved(&self) {
        godwit_slack::session::clear(SlackAccount::slot(self));
    }
    /// Its window open is the form.
    fn status(&self) -> Status {
        match SlackAccount::status(self) {
            SlackStatus::Waiting => Status::SignedOut { error: None },
            SlackStatus::SignedOut { error } => Status::SignedOut { error: error.clone() },
            SlackStatus::Connecting => Status::Connecting,
            SlackStatus::Ready => Status::Ready,
        }
    }
    /// Slack signs in on its own page, in the helper's window.
    fn begin_sign_in(&mut self, helper: Option<PathBuf>) {
        if let Some(helper) = helper {
            self.sign_in(helper);
        }
    }
    fn cancel_sign_in(&mut self) {
        SlackAccount::cancel_sign_in(self);
    }
    fn is_ours(&self, user: &UserId) -> bool {
        SlackAccount::is_ours(self, user)
    }
    fn notifications_muted(&self, room: &RoomId) -> bool {
        SlackAccount::notifications_muted(self, room)
    }
}

impl Account for GChatAccount {
    fn slot(&self) -> u32 {
        GChatAccount::slot(self)
    }
    fn forget_saved(&self) {
        godwit_gchat::session::clear(GChatAccount::slot(self));
    }
    /// Its window open is the form.
    fn status(&self) -> Status {
        match GChatAccount::status(self) {
            GChatStatus::Waiting => Status::SignedOut { error: None },
            GChatStatus::SignedOut { error } => Status::SignedOut { error: error.clone() },
            GChatStatus::Connecting => Status::Connecting,
            GChatStatus::Ready => Status::Ready,
        }
    }
    /// Google Chat signs in on its own page, in the helper's window.
    fn begin_sign_in(&mut self, helper: Option<PathBuf>) {
        if let Some(helper) = helper {
            self.sign_in(helper);
        }
    }
    fn cancel_sign_in(&mut self) {
        GChatAccount::cancel_sign_in(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_network_is_registered_once_with_one_default() {
        for n in Network::ALL {
            assert_eq!(NETWORKS.iter().filter(|e| e.network == n).count(), 1, "{n:?}");
        }
        assert_eq!(NETWORKS.iter().filter(|e| e.default).count(), 1);
        assert_eq!(default_network(), Network::Stoat);
        assert!(entry(Network::Slack).needs_helper && entry(Network::GoogleChat).needs_helper);
    }
}
