// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Steam sign-ins kept in the secret store (OS keychain, or owner-only files
//! as fallback), one slot per account as Stoat's: the account name, its id
//! and the refresh token. Never the password.

#[cfg(test)]
use godwit_secrets::SecretStore;
use serde::{Deserialize, Serialize};

const NAME: &str = "steam-session";
/// Steam accounts signed in at once, at most.
pub const MAX_SLOTS: u32 = 8;

/// A kept sign-in.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub account_name: String,
    pub steam_id: u64,
    pub refresh_token: String,
    /// The made-up name this sign-in shows as in the account's list of
    /// signed-in devices (to find it there, or revoke it).
    #[serde(default)]
    pub device_name: String,
}

impl std::fmt::Debug for Saved {
    // Never print the token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Saved")
            .field("account_name", &self.account_name)
            .field("steam_id", &self.steam_id)
            .field("device_name", &self.device_name)
            .finish_non_exhaustive()
    }
}

impl Saved {
    /// A sign-in's tokens, and the device name it used.
    pub fn new(t: crate::auth::Tokens, device_name: String) -> Self {
        Self {
            account_name: t.account_name,
            steam_id: t.steam_id,
            refresh_token: t.refresh_token,
            device_name,
        }
    }
}

static SESSIONS: godwit_core::sessions::Sessions<Saved> = godwit_core::sessions::Sessions::new(
    NAME,
    MAX_SLOTS,
    None,
    "Could not keep the Steam sign-in; sign in again after restarting",
    "Could not remove the kept Steam sign-in",
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
        let bytes = r#"{"account_name":"someone","steam_id":7,"refresh_token":"fake-test-token","device_name":"DESKTOP-TEST123"}"#; // not-a-secret: test fixture
        let saved: Saved = serde_json::from_str(bytes).unwrap();
        assert_eq!(serde_json::to_string(&saved).unwrap(), bytes);
        assert_eq!(SESSIONS.name(0), NAME);
        assert_eq!(SESSIONS.name(3), format!("{NAME}-3"));
    }

    #[test]
    fn sign_ins_round_trip_per_slot_without_showing_the_token() {
        let store = Memory::default();
        let saved = Saved {
            account_name: "someone".into(),
            steam_id: 7,
            refresh_token: "fake-test-token".into(), // not-a-secret: test fixture
            device_name: "DESKTOP-TEST123".into(),
        };
        assert!(SESSIONS.read_from(&store, 0).is_none());
        assert!(SESSIONS.write_to(&store, 2, &saved));
        assert_eq!(SESSIONS.read_from(&store, 2), Some(saved.clone()));
        assert!(SESSIONS.read_from(&store, 0).is_none());
        assert!(store.get("steam-session-2").unwrap().is_some());
        assert!(!format!("{saved:?}").contains("fake-test-token"));
    }
}
