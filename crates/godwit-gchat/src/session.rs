// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Google Chat sign-ins kept in the secret store, one slot per sign-in: the
//! Google cookies the web client sends. They give full access to the Google
//! account, not just Chat, so they're never logged or printed.

use serde::{Deserialize, Serialize};

const NAME: &str = "gchat-session";
/// Google Chat sign-ins at once, at most.
pub const MAX_SLOTS: u32 = 8;

/// The cookies the web client sends to chat.google.com; anything else the
/// sign-in page leaves behind isn't kept. `SAPISID` signs requests (see
/// `auth`); the rest authenticate them.
pub const KEPT: &[&str] = &[
    "SID",
    "HSID",
    "SSID",
    "APISID",
    "SAPISID",
    "__Secure-1PSID",
    "__Secure-3PSID",
    "__Secure-1PAPISID",
    "__Secure-3PAPISID",
    "COMPASS",
    "OSID",
];

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    /// `(name, value)` in the order they're sent.
    pub cookies: Vec<(String, String)>,
}

impl std::fmt::Debug for Saved {
    // Never print the values.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.cookies.iter().map(|(n, _)| n.as_str()).collect();
        f.debug_struct("Saved").field("cookies", &names).finish()
    }
}

impl Saved {
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.cookies
            .iter()
            .find(|(n, v)| n == name && !v.is_empty())
            .map(|(_, v)| v.as_str())
    }

    /// The sign-in window's line, `{"cookies": [[name, value], …]}`: only
    /// the kept cookies stay, and it's a sign-in only with `SID` and
    /// `SAPISID`.
    pub fn from_line(line: &str) -> Option<Self> {
        let mut saved: Saved = serde_json::from_str(line.trim()).ok()?;
        saved
            .cookies
            .retain(|(name, value)| KEPT.contains(&name.as_str()) && !value.is_empty() && valid_value(value));
        (saved.cookie("SID").is_some() && saved.cookie("SAPISID").is_some()).then_some(saved)
    }
}

/// A value that can go in a `Cookie` header as it is.
fn valid_value(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_graphic() && !matches!(b, b';' | b',' | b'"' | b'\\'))
}

static SESSIONS: godwit_core::sessions::Sessions<Saved> = godwit_core::sessions::Sessions::new(
    NAME,
    MAX_SLOTS,
    Some(MAX_SLOTS),
    "Could not keep the Google Chat sign-in; sign in again after restarting",
    "Could not remove the kept Google Chat sign-in",
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

    #[test]
    fn saved_bytes_and_slot_names_are_stable() {
        let bytes = r#"{"cookies":[["SID","fake-sid"],["SAPISID","fake-sapisid"]]}"#;
        let saved: Saved = serde_json::from_str(bytes).unwrap();
        assert_eq!(serde_json::to_string(&saved).unwrap(), bytes);
        assert_eq!(SESSIONS.name(0), NAME);
        assert_eq!(SESSIONS.name(2), format!("{NAME}-2"));
    }

    #[test]
    fn the_windows_line_keeps_only_chat_cookies() {
        let line = r#"{"cookies":[["SID","fake-sid"],["SAPISID","fake-sapisid"],
            ["NID","tracking"],["HSID","bad;value"]]}"#;
        let saved = Saved::from_line(line).unwrap();
        let names: Vec<&str> = saved.cookies.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["SID", "SAPISID"]);
        assert!(!format!("{saved:?}").contains("fake"));
        assert!(Saved::from_line(r#"{"cookies":[["SID","fake-sid"]]}"#).is_none());
        assert!(Saved::from_line("garbage").is_none());
    }
}
