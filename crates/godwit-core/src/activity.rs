// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Whether the user is active in Godwit right now, and telling networks.
//!
//! The app reports what it already knows (the window gained or lost focus);
//! [`ActivityRelay`] passes it only to networks that declare they want it
//! ([`Network::presence_upkeep`](crate::network::Network::presence_upkeep)) and
//! never repeats a value an account already has. Nothing here wakes the app:
//! there is no timer.

use std::collections::HashMap;

use crate::ids::AccountId;
use crate::members::Presence;

/// How a network wants our presence kept up to date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresenceUpkeep {
    /// Our chosen status is sent when it changes and nothing else is needed
    /// (Stoat, Discord, Steam, ...): the network keeps it until told otherwise.
    OnChange,
    /// The network shows us active or away from what the client reports, so
    /// it is told once whenever that changes (and its own keep-alive, if the
    /// service needs one, stays inside its crate).
    FollowsActivity,
}

/// Active as the network should see it: the user is active in Godwit and
/// didn't choose a status that says otherwise (Idle, invisible).
pub fn effective(app_active: bool, chosen: Presence) -> bool {
    app_active && !matches!(chosen, Presence::Idle | Presence::Invisible | Presence::Offline)
}

/// The shared activity state and the last value each account was told.
#[derive(Debug)]
pub struct ActivityRelay {
    active: bool,
    sent: HashMap<AccountId, bool>,
}

impl Default for ActivityRelay {
    fn default() -> Self {
        // A window that is up is taken as active until it says otherwise.
        Self {
            active: true,
            sent: HashMap::new(),
        }
    }
}

impl ActivityRelay {
    /// The app's current activity.
    pub fn active(&self) -> bool {
        self.active
    }

    /// Records the app's activity; true if it changed.
    pub fn set_active(&mut self, active: bool) -> bool {
        let changed = self.active != active;
        self.active = active;
        changed
    }

    /// The value to send `account`, or None if it already has it.
    pub fn due(&mut self, account: &AccountId) -> Option<bool> {
        if self.sent.get(account) == Some(&self.active) {
            return None;
        }
        self.sent.insert(account.clone(), self.active);
        Some(self.active)
    }

    /// An account (re)connected or went away: whatever it knew is gone.
    pub fn forget(&mut self, account: &AccountId) {
        self.sent.remove(account);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::Network;

    fn id(n: &str) -> AccountId {
        AccountId::new(Network::GoogleChat, n)
    }

    #[test]
    fn unchanged_value_is_not_sent_again() {
        let mut r = ActivityRelay::default();
        let a = id("a");
        assert_eq!(r.due(&a), Some(true));
        assert_eq!(r.due(&a), None);
        assert!(r.set_active(false));
        assert!(!r.set_active(false));
        assert_eq!(r.due(&a), Some(false));
        assert_eq!(r.due(&a), None);
        assert!(r.set_active(true));
        assert_eq!(r.due(&a), Some(true));
    }

    #[test]
    fn reconnect_sends_once_more() {
        let mut r = ActivityRelay::default();
        let a = id("a");
        assert_eq!(r.due(&a), Some(true));
        r.forget(&a);
        assert_eq!(r.due(&a), Some(true));
        assert_eq!(r.due(&a), None);
    }

    #[test]
    fn accounts_are_tracked_apart() {
        let mut r = ActivityRelay::default();
        assert_eq!(r.due(&id("a")), Some(true));
        assert_eq!(r.due(&id("b")), Some(true));
        assert_eq!(r.due(&id("a")), None);
    }

    #[test]
    fn chosen_status_beats_activity() {
        assert!(effective(true, Presence::Online));
        assert!(effective(true, Presence::Focus));
        assert!(!effective(false, Presence::Online));
        assert!(!effective(true, Presence::Idle));
        assert!(!effective(true, Presence::Invisible));
    }
}
