// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Discord sign-ins kept in the secret store (OS keychain, or owner-only
//! files as fallback), one slot per account as Stoat's and Steam's: the
//! user's id and the token. The token gives full control of the account, so
//! it's never logged or printed.

#[cfg(test)]
use godwit_secrets::SecretStore;
use serde::{Deserialize, Serialize};

const NAME: &str = "discord-session";
/// Discord accounts signed in at once, at most.
pub const MAX_SLOTS: u32 = 8;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    /// Their user id, once known (empty right after signing in).
    #[serde(default)]
    pub user: String,
    pub token: String,
}

impl std::fmt::Debug for Saved {
    // Never print the token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Saved")
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

static SESSIONS: godwit_core::sessions::Sessions<Saved> = godwit_core::sessions::Sessions::new(
    NAME,
    MAX_SLOTS,
    Some(MAX_SLOTS),
    "Could not keep the Discord sign-in; sign in again after restarting",
    "Could not remove the kept Discord sign-in",
);

pub fn slots() -> Vec<u32> {
    SESSIONS.slots()
}

pub fn load(slot: u32) -> Option<Saved> {
    SESSIONS.load(slot)
}

pub fn save(slot: u32, saved: &Saved) {
    SESSIONS.save(slot, saved)
}

pub fn clear(slot: u32) {
    SESSIONS.clear(slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    use godwit_secrets::testing::Memory;

    #[test]
    fn saved_bytes_and_slot_names_stay_compatible() {
        let bytes = r#"{"user":"123","token":"fake-test-token"}"#;
        let saved: Saved = serde_json::from_str(bytes).unwrap();
        assert_eq!(serde_json::to_string(&saved).unwrap(), bytes);
        assert_eq!(SESSIONS.name(0), NAME);
        assert_eq!(SESSIONS.name(3), format!("{NAME}-3"));
    }

    #[test]
    fn sign_ins_round_trip_per_slot_without_showing_the_token() {
        let store = Memory::default();
        let saved = Saved {
            user: "123".into(),
            token: "fake-test-token".into(),
        };
        assert!(SESSIONS.read_from(&store, 0).is_none());
        assert!(SESSIONS.write_to(&store, 3, &saved));
        assert_eq!(SESSIONS.read_from(&store, 3), Some(saved.clone()));
        assert!(store.get("discord-session-3").unwrap().is_some());
        assert!(!format!("{saved:?}").contains("fake-test-token"));
    }
}
