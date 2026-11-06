// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Signing in, MFA, devices and the event poll.

use super::*;

impl StoatAccount {
    pub fn log_in(&mut self, email: String, password: String) {
        self.status = Status::LoggingIn;
        self.net.send(Cmd::Session(SessionCmd::Login { email, password }));
    }

    /// Answers the two-factor prompt with an authenticator or recovery code.
    pub fn submit_mfa(&mut self, code: String) {
        let Some(ticket) = self.mfa_ticket.clone() else {
            return;
        };
        self.status = Status::LoggingIn;
        self.net.send(Cmd::Session(SessionCmd::Mfa { ticket, code }));
    }

    /// Asks for the audio devices; they arrive with a later [`Self::poll`].
    pub fn list_devices(&self) {
        self.net.send(Cmd::Voice(VoiceCommand::ListAudioDevices));
    }

    pub fn devices(&self) -> Option<&AudioDevices> {
        self.devices.as_ref()
    }

    /// New voice settings; a call in progress picks up volumes and devices now.
    pub fn set_voice_settings(&mut self, settings: VoiceSettings) {
        if self.call.is_some() {
            self.net.send(Cmd::Voice(VoiceCommand::Voice(VoiceCmd::Update(Box::new(
                settings.clone(),
            )))));
        }
        self.voice_settings = settings;
    }

    /// Handles everything the backend sent since the last call.
    pub(super) fn poll_events(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        godwit_core::backend::poll(self, |s| s.events.try_recv().ok(), Self::handle, &mut out);
        out
    }

    pub(super) fn forget_session(&self) {
        if self.persist_session {
            session_file::clear(self.slot);
        }
    }

    pub(super) fn lose(&mut self, reason: &str, out: &mut Vec<Event>) {
        self.store = Store::default();
        self.rooms.clear();
        self.servers.clear();
        self.friends.clear();
        self.occupants.clear();
        self.call = None;
        self.viewing = None;
        if let Some(account) = self.account.take() {
            out.push(Event::AccountLost {
                account,
                reason: reason.into(),
            });
        }
    }
}
