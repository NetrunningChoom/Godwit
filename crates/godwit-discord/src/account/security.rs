// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The security page: change the password, set up or remove the
//! authenticator (see `crate::security` for the requests). Discord holds the
//! protected requests back for a confirmation, which the shared step-up
//! prompt asks for, so no proof is typed up front. The password typed for
//! authenticator setup is kept only until the setup ends.

use super::*;
use crate::backend::{Secret as NetSecret, SecurityDone, SecurityOp};
use godwit_core::account_security::{ProofKind, SecurityAction as A, SecurityView};
use godwit_core::application::Secret;

/// An authenticator setup waiting for its first code.
pub(super) struct TotpSetup {
    password: Secret,
    secret: Secret,
}

impl DiscordAccount {
    fn security_event(&self, view: &SecurityView) -> Vec<Event> {
        match self.account.clone() {
            Some(account) => vec![Event::AccountSecurity {
                account,
                view: view.clone(),
            }],
            None => Vec::new(),
        }
    }

    fn security_failed(&mut self, text: &str) -> Vec<Event> {
        self.security.loading = false;
        self.security.message = None;
        self.security.error = Some(text.to_owned());
        self.security_event(&self.security.clone())
    }

    /// Sends `op` and shows the page busy.
    fn security_send(&mut self, op: SecurityOp) -> Vec<Event> {
        self.net.send(Cmd::Security(op));
        self.security.loading = true;
        self.security.error = None;
        self.security.message = None;
        self.security.recovery_codes.clear();
        self.security_event(&self.security.clone())
    }

    pub(super) fn security_command(&mut self, action: A) -> Vec<Event> {
        match action {
            A::Load => {
                self.totp_setup = None;
                self.security = SecurityView {
                    email: crate::security::mask_email(&self.store.email),
                    totp: self.store.mfa_enabled,
                    ..Default::default()
                };
                self.security_event(&self.security.clone())
            }
            A::ChangePassword {
                password,
                current_password,
                ..
            } => {
                if !crate::security::PASSWORD_LEN.contains(&password.0.chars().count()) {
                    return self.security_failed("A password is 8 to 72 characters.");
                }
                self.security_send(SecurityOp::ChangePassword {
                    current: NetSecret(current_password.0),
                    new: NetSecret(password.0),
                })
            }
            A::BeginTotp {
                proof: (kind, password),
            } => {
                if kind != ProofKind::Password || password.0.is_empty() {
                    return self.security_failed("Discord asks for your password to set up an authenticator.");
                }
                self.security.pending_secret = None;
                self.security.pending_uri = None;
                self.totp_setup = Some(TotpSetup {
                    password: password.clone(),
                    secret: Secret(crate::security::totp_secret()),
                });
                self.security_send(SecurityOp::BeginTotp {
                    password: NetSecret(password.0),
                })
            }
            A::ConfirmTotp { code } => {
                use godwit_core::step_up::StepUpMethod;
                let code: String = code.0.chars().filter(|c| !c.is_whitespace()).collect();
                if !StepUpMethod::Authenticator.plausible(&code) {
                    return self.security_failed("An authenticator code is six digits.");
                }
                let Some(setup) = &self.totp_setup else {
                    return self.security_failed("Start the authenticator setup again.");
                };
                let op = SecurityOp::EnableTotp {
                    password: NetSecret(setup.password.0.clone()),
                    secret: NetSecret(setup.secret.0.clone()),
                    code: NetSecret(code),
                };
                self.security_send(op)
            }
            A::RemoveTotp { .. } => self.security_send(SecurityOp::DisableTotp),
            // Not offered on Discord's page here (the core only sends what
            // `AccountPage` allows, so this is a stray action).
            A::ChangeUsername { .. }
            | A::ChangeEmail { .. }
            | A::ViewRecovery { .. }
            | A::RegenerateRecovery { .. } => self.security_failed("Discord doesn't offer that here."),
        }
    }

    pub(super) fn security_done(&mut self, result: Result<SecurityDone, String>) -> Vec<Event> {
        let done = match result {
            Ok(done) => done,
            Err(text) => {
                // A failed first step ends the setup; a wrong code can be retried.
                if self.security.pending_secret.is_none() {
                    self.totp_setup = None;
                }
                return self.security_failed(&text);
            }
        };
        self.security.loading = false;
        self.security.error = None;
        self.security.message = None;
        match done {
            SecurityDone::PasswordChanged => self.security.message = Some("Password changed.".into()),
            SecurityDone::TotpBegun => {
                if let Some(setup) = &self.totp_setup {
                    self.security.pending_uri = Some(Secret(crate::security::otpauth_uri(&setup.secret.0)));
                    self.security.pending_secret = Some(setup.secret.clone());
                }
            }
            SecurityDone::TotpEnabled(codes) => {
                self.totp_setup = None;
                self.store.mfa_enabled = true;
                self.security.totp = true;
                self.security.pending_secret = None;
                self.security.pending_uri = None;
                self.security.recovery_codes = codes.into_iter().map(|c| Secret(c.0)).collect();
                self.security.message = Some("Authenticator turned on. Keep these backup codes somewhere safe.".into());
            }
            SecurityDone::TotpDisabled => {
                self.store.mfa_enabled = false;
                self.security.totp = false;
                self.security.recovery_codes.clear();
                self.security.message = Some("Authenticator turned off.".into());
            }
        }
        self.security_event(&self.security.clone())
    }
}
