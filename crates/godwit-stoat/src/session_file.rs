// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Persists new logins in the OS keychain, with owner-only files as fallback.
//! Alpha sessions from the old plain JSON file are deliberately not migrated.

#[cfg(test)]
use godwit_secrets::{Secret, SecretStore};

use crate::api::Session;

const NAME: &str = "stoat-session";
/// Number of existing session slots to scan when restoring accounts.
const LEGACY_SCAN_SLOTS: u32 = 8;
/// Per-account storage uses numeric names, so new slots need no migration.
pub const MAX_SLOTS: u32 = u32::MAX;

static SESSIONS: godwit_core::sessions::Sessions<Session> = godwit_core::sessions::Sessions::new(
    NAME,
    LEGACY_SCAN_SLOTS,
    None,
    "Could not save the Stoat login; sign in again after restarting",
    "Could not remove the saved Stoat login",
);

fn discard_old_session() {
    if let Some(dir) = godwit_core::config_dir::get() {
        // Never restore a token from the pre-keychain alpha format.
        let path = dir.join("session.json");
        if let Err(e) = std::fs::remove_file(path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("Could not remove the obsolete Stoat login file");
        }
    }
}

pub fn slots() -> Vec<u32> {
    discard_old_session();
    SESSIONS.discover(LEGACY_SCAN_SLOTS, |slot| SESSIONS.load(slot).is_some())
}

pub fn load(slot: u32) -> Option<Session> {
    discard_old_session();
    SESSIONS.load(slot)
}

pub fn save(slot: u32, saved: &Session) {
    discard_old_session();
    SESSIONS.save(slot, saved)
}

pub fn clear(slot: u32) {
    discard_old_session();
    SESSIONS.clear(slot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    use godwit_secrets::testing::Memory;

    #[test]
    fn saved_bytes_and_slot_names_stay_compatible() {
        let bytes = r#"{"_id":"test-session","user_id":"test-user","token":"fake-test-token"}"#;
        let saved: Session = serde_json::from_str(bytes).unwrap();
        assert_eq!(serde_json::to_string(&saved).unwrap(), bytes);
        assert_eq!(SESSIONS.name(0), NAME);
        assert_eq!(SESSIONS.name(3), format!("{NAME}-3"));
    }

    #[test]
    fn session_round_trip_and_logout() {
        let store = Memory::default();
        assert!(SESSIONS.read_from(&store, 0).is_none());
        let session = Session {
            id: "test-session".into(),
            user_id: "test-user".into(),
            token: "fake-test-token".into(),
        };
        assert!(SESSIONS.write_to(&store, 0, &session));
        let restored = SESSIONS.read_from(&store, 0).unwrap();
        assert_eq!(restored.id, session.id);
        assert_eq!(restored.user_id, session.user_id);
        assert_eq!(restored.token, session.token);
        // Other slots are other accounts; slot 0 keeps the old name.
        assert!(SESSIONS.read_from(&store, 1).is_none());
        assert!(SESSIONS.write_to(&store, 3, &session));
        assert!(store.get("stoat-session-3").unwrap().is_some());
        store.delete(NAME).unwrap();
        assert!(SESSIONS.read_from(&store, 0).is_none());
        assert!(SESSIONS.read_from(&store, 3).is_some());
    }

    #[test]
    fn higher_account_slots_are_supported_and_discovered() {
        let store = Memory::default();
        let session = Session {
            id: "test-session".into(),
            user_id: "test-user".into(),
            token: "fake-test-token".into(),
        };
        assert!(SESSIONS.write_to(&store, 12, &session));
        assert!(SESSIONS.read_from(&store, 12).is_some());
        let discovered = SESSIONS.discover(LEGACY_SCAN_SLOTS, |slot| SESSIONS.read_from(&store, slot).is_some());
        assert_eq!(discovered, [12]);
    }

    struct Unavailable;

    impl SecretStore for Unavailable {
        fn get(&self, _: &str) -> io::Result<Option<Secret>> {
            Err(io::Error::other("test store unavailable"))
        }
        fn set(&self, _: &str, _: &Secret) -> io::Result<()> {
            Err(io::Error::other("test store unavailable"))
        }
        fn delete(&self, _: &str) -> io::Result<()> {
            Err(io::Error::other("test store unavailable"))
        }
    }

    #[test]
    fn unavailable_store_does_not_panic_or_claim_a_saved_login() {
        assert!(SESSIONS.read_from(&Unavailable, 0).is_none());
        assert!(!SESSIONS.write_to(
            &Unavailable,
            0,
            &Session {
                id: "test-session".into(),
                user_id: "test-user".into(),
                token: "fake-test-token".into(),
            }
        ));
    }

    #[test]
    fn invalid_saved_data_does_not_restore_a_login() {
        let store = Memory::default();
        store.set(NAME, &Secret::new("invalid fake data")).unwrap();
        assert!(SESSIONS.read_from(&store, 0).is_none());
    }
}
