// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A network holds back a protected request (a moderation action in a server
//! that requires two-factor authentication, an owner's own actions) until the
//! person confirms with a code. The network raises a prompt
//! ([`Event::StepUpRequired`](crate::application::Event::StepUpRequired)),
//! the person answers it
//! ([`Action::StepUpAnswer`](crate::application::Action::StepUpAnswer)), and
//! the network sends the request again. Nothing the person typed is kept in
//! state: the code goes straight to the network.

use std::sync::Arc;

use crate::ids::AccountId;

/// A way to confirm, as the network offers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepUpMethod {
    /// A code from an authenticator app.
    Authenticator,
    /// One of the recovery (backup) codes.
    Recovery,
    /// The account's password.
    Password,
}

impl StepUpMethod {
    pub fn label(self) -> &'static str {
        match self {
            Self::Authenticator => "Authenticator code",
            Self::Recovery => "Backup code",
            Self::Password => "Password",
        }
    }

    /// Whether a code could be one this method takes, so an obviously wrong
    /// one isn't sent: six digits from an app, eight to eleven characters of
    /// a backup code (dashes aside), any password.
    pub fn plausible(self, code: &str) -> bool {
        match self {
            Self::Authenticator => {
                let digits: String = code.chars().filter(|c| !c.is_whitespace()).collect();
                digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_digit())
            }
            Self::Recovery => (8..=11).contains(&code.trim().replace('-', "").chars().count()),
            Self::Password => !code.is_empty(),
        }
    }
}

/// A prompt waiting for the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepUpPrompt {
    pub account: AccountId,
    /// The network's id for it, what an answer names.
    pub id: u64,
    /// The methods the network offers that Godwit can use, in its order.
    pub methods: Vec<StepUpMethod>,
    /// Waiting for the network to take the answer.
    pub busy: bool,
    /// Why the last answer didn't do.
    pub error: Option<Arc<str>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_checked_before_they_are_sent() {
        assert!(StepUpMethod::Authenticator.plausible("123 456"));
        assert!(!StepUpMethod::Authenticator.plausible("12345"));
        assert!(!StepUpMethod::Authenticator.plausible("12345a"));
        assert!(StepUpMethod::Recovery.plausible("abcd-efgh"));
        assert!(!StepUpMethod::Recovery.plausible("abc"));
        assert!(!StepUpMethod::Password.plausible(""));
    }
}
