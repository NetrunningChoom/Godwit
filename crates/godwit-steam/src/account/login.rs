// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Signing in: QR, password and Steam Guard codes.

use super::*;

impl SteamAccount {
    /// The made-up device name this sign-in shows as in Steam's list of
    /// signed-in devices ("" if not known).
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Sign in with a QR code for the Steam mobile app.
    pub fn begin_qr(&mut self) {
        self.status = Status::Qr {
            url: None,
            scanned: false,
        };
        self.net.send(Cmd::BeginQr);
    }

    /// Sign in with an account name and password instead.
    pub fn log_in(&mut self, account: String, password: String) {
        self.status = Status::LoggingIn;
        self.net.send(Cmd::BeginPassword {
            account: account.trim().to_owned(),
            password,
        });
    }

    pub fn submit_code(&mut self, code: String) {
        self.status = Status::LoggingIn;
        self.net.send(Cmd::SubmitCode(code));
    }

    /// Leave the sign-in under way (back to choosing a network).
    pub fn cancel_login(&mut self) {
        self.net.send(Cmd::CancelLogin);
        if self.account.is_none() {
            self.status = Status::SignedOut { error: None };
        }
    }
}
