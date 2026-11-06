// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Teams sign-ins kept in the secret store (OS keychain, or owner-only
//! files as fallback), one slot per account as the other networks': the
//! refresh token and whom it's for. The refresh token gives full access to
//! the account's Teams (and is long-lived), so it's never logged or printed.

#[cfg(test)]
use godwit_secrets::SecretStore;
use serde::{Deserialize, Serialize};

const NAME: &str = "teams-session";
/// Teams accounts signed in at once, at most.
pub const MAX_SLOTS: u32 = 8;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    /// The user's id (`8:orgid:…`), once known.
    #[serde(default)]
    pub user: String,
    pub refresh_token: String,
    /// The tenant refreshed at (`organizations` unless known better).
    #[serde(default = "tenant")]
    pub tenant: String,
    /// This sign-in's push endpoint, kept: Teams limits how many an account
    /// may have (purple-teams keeps its one for the same reason).
    #[serde(default)]
    pub endpoint: String,
}

fn tenant() -> String {
    crate::auth::TENANT.to_owned()
}

impl Saved {
    pub fn new(refresh_token: String) -> Self {
        Self {
            user: String::new(),
            refresh_token,
            tenant: tenant(),
            endpoint: String::new(),
        }
    }
}

impl std::fmt::Debug for Saved {
    // Never print the token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Saved")
            .field("user", &self.user)
            .field("tenant", &self.tenant)
            .finish_non_exhaustive()
    }
}

static SESSIONS: godwit_core::sessions::Sessions<Saved> = godwit_core::sessions::Sessions::new(
    NAME,
    MAX_SLOTS,
    Some(MAX_SLOTS),
    "Could not keep the Teams sign-in; sign in again after restarting",
    "Could not remove the kept Teams sign-in",
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
        let bytes = r#"{"user":"","refresh_token":"fake-test-token","tenant":"organizations","endpoint":""}"#; // not-a-secret: test fixture
        let saved: Saved = serde_json::from_str(bytes).unwrap();
        assert_eq!(serde_json::to_string(&saved).unwrap(), bytes);
        assert_eq!(SESSIONS.name(0), NAME);
        assert_eq!(SESSIONS.name(3), format!("{NAME}-3"));
    }

    #[test]
    fn sign_ins_round_trip_per_slot_without_showing_the_token() {
        let store = Memory::default();
        let saved = Saved::new("fake-test-token".into());
        assert!(SESSIONS.read_from(&store, 0).is_none());
        assert!(SESSIONS.write_to(&store, 2, &saved));
        assert_eq!(SESSIONS.read_from(&store, 2), Some(saved.clone()));
        assert!(store.get("teams-session-2").unwrap().is_some());
        assert!(!format!("{saved:?}").contains("fake-test-token"));
        assert_eq!(saved.tenant, "organizations");
    }
}
