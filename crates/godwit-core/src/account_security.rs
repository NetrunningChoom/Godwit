// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Network-neutral account credentials and second-factor controls.

use crate::application::Secret;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecurityAction {
    Load,
    ChangeUsername {
        username: String,
        password: Secret,
        proof: Option<(ProofKind, Secret)>,
    },
    ChangeEmail {
        email: String,
        current_password: Secret,
        proof: Option<(ProofKind, Secret)>,
    },
    ChangePassword {
        password: Secret,
        current_password: Secret,
        proof: Option<(ProofKind, Secret)>,
    },
    BeginTotp {
        proof: (ProofKind, Secret),
    },
    ConfirmTotp {
        code: Secret,
    },
    ViewRecovery {
        proof: (ProofKind, Secret),
    },
    RegenerateRecovery {
        proof: (ProofKind, Secret),
    },
    RemoveTotp {
        proof: (ProofKind, Secret),
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofKind {
    Password,
    Authenticator,
    Recovery,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actions_redact_passwords_and_codes_in_debug() {
        let action = SecurityAction::ChangePassword {
            password: Secret("new-pass-secret".into()),
            current_password: Secret("old-pass-secret".into()),
            proof: Some((ProofKind::Authenticator, Secret("totp-secret".into()))),
        };
        let rendered = format!("{action:?}");
        assert!(!rendered.contains("new-pass-secret"));
        assert!(!rendered.contains("old-pass-secret"));
        assert!(!rendered.contains("totp-secret"));
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecurityView {
    pub loading: bool,
    pub email: String,
    pub totp: bool,
    pub recovery: bool,
    pub pending_secret: Option<Secret>,
    /// The `otpauth://` address of the pending secret, for a QR code.
    pub pending_uri: Option<Secret>,
    pub recovery_codes: Vec<Secret>,
    pub error: Option<String>,
    pub message: Option<String>,
}
