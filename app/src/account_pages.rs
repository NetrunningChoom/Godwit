// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Settings → a Discord account → Privacy and Sessions: fills the pages
//! (the `AccountPages` global) from what the account last said, and turns
//! their clicks into actions. The account talks to the network.

use std::rc::Rc;

use godwit_core::application::{Action, AppState};
use godwit_core::ids::AccountId;
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::ui::{AccountPages as Pages, AppWindow, PrivItem, SessItem};

/// The account whose pages are open.
#[derive(Default)]
pub struct AccountPages {
    pub account: Option<AccountId>,
}

impl AccountPages {
    pub fn open_privacy(&mut self) -> Option<Action> {
        Some(Action::LoadPrivacy(self.account.clone()?))
    }

    pub fn open_sessions(&mut self) -> Option<Action> {
        Some(Action::LoadSessions(self.account.clone()?))
    }

    /// A switch flipped: the opposite of what it shows.
    pub fn toggle(&self, state: &AppState, index: usize) -> Option<Action> {
        let account = self.account.clone()?;
        let item = state.privacy.get(&account)?.items.get(index)?;
        Some(Action::SetPrivacy {
            account,
            key: item.key.clone(),
            on: !item.on,
        })
    }

    pub fn log_out(&self, state: &AppState, index: usize) -> Option<Action> {
        let account = self.account.clone()?;
        let session = state.sessions.get(&account)?.sessions.get(index)?.id.clone();
        Some(Action::LogOutSession { account, session })
    }

    /// Fills both pages from the account's latest answers.
    pub fn show(&self, ui: &AppWindow, state: &AppState) {
        let Some(account) = &self.account else { return };
        let pages = ui.global::<Pages>();
        let privacy = state.privacy.get(account).cloned().unwrap_or_default();
        pages.set_privacy(ModelRc::new(Rc::new(VecModel::from(
            privacy
                .items
                .iter()
                .map(|i| PrivItem {
                    label: i.label.as_ref().into(),
                    detail: i.detail.as_ref().into(),
                    on: i.on,
                })
                .collect::<Vec<_>>(),
        ))));
        pages.set_privacy_loading(privacy.loading);
        pages.set_privacy_error(privacy.error.as_deref().unwrap_or_default().into());
        let sessions = state.sessions.get(account).cloned().unwrap_or_default();
        pages.set_sessions(ModelRc::new(Rc::new(VecModel::from(
            sessions
                .sessions
                .iter()
                .map(|s| {
                    let used = s.last_used_ms.and_then(|ms| {
                        use chrono::TimeZone;
                        chrono::Local
                            .timestamp_millis_opt(ms)
                            .single()
                            .map(|t| format!("last used {}", t.format("%b %-d")))
                    });
                    let detail: Vec<String> = [Some(s.location.to_string()).filter(|l| !l.is_empty()), used]
                        .into_iter()
                        .flatten()
                        .collect();
                    SessItem {
                        device: s.device.as_ref().into(),
                        detail: detail.join(" · ").into(),
                    }
                })
                .collect::<Vec<_>>(),
        ))));
        pages.set_sessions_loading(sessions.loading);
        pages.set_sessions_error(sessions.error.as_deref().unwrap_or_default().into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use godwit_core::network::Network;
    use godwit_core::privacy::{PrivacyItem, PrivacyView, Session, SessionsView};

    #[test]
    fn clicks_become_the_right_actions() {
        let account = AccountId::new(Network::Discord, "me");
        let mut state = AppState::default();
        state.privacy.insert(
            account.clone(),
            PrivacyView {
                items: vec![PrivacyItem {
                    key: "show_activity".into(),
                    label: "Share my activity".into(),
                    detail: "".into(),
                    on: true,
                }],
                ..PrivacyView::default()
            },
        );
        state.sessions.insert(
            account.clone(),
            SessionsView {
                sessions: vec![Session {
                    id: "h1".into(),
                    device: "Linux".into(),
                    location: "".into(),
                    last_used_ms: None,
                }],
                ..SessionsView::default()
            },
        );
        let mut pages = AccountPages::default();
        assert!(pages.open_privacy().is_none(), "no account open yet");
        pages.account = Some(account.clone());
        assert!(matches!(pages.open_privacy(), Some(Action::LoadPrivacy(a)) if a == account));
        assert!(matches!(pages.open_sessions(), Some(Action::LoadSessions(a)) if a == account));
        // A switch shown on asks for off.
        assert!(matches!(
            pages.toggle(&state, 0),
            Some(Action::SetPrivacy { key, on: false, .. }) if &*key == "show_activity"
        ));
        assert!(pages.toggle(&state, 5).is_none(), "no such row");
        assert!(matches!(
            pages.log_out(&state, 0),
            Some(Action::LogOutSession { session, .. }) if &*session == "h1"
        ));
    }
}
