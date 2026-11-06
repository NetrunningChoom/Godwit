// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Slack sign-ins kept in the secret store, one slot per sign-in: the `d`
//! cookie and each workspace's client token. Together they give full access
//! to the user's Slack, so they're never logged or printed.

use serde::{Deserialize, Serialize};

const NAME: &str = "slack-session";
/// Slack sign-ins at once, at most.
pub const MAX_SLOTS: u32 = 8;

/// One workspace of a sign-in.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// `https://{domain}.slack.com/`: where its API is called.
    pub url: String,
    pub token: String,
    /// Our user id there.
    #[serde(default)]
    pub user: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    /// The `d` cookie every request carries.
    pub d: String,
    pub teams: Vec<Team>,
}

impl std::fmt::Debug for Saved {
    // Never print the cookie or the tokens.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let teams: Vec<&str> = self.teams.iter().map(|t| t.id.as_str()).collect();
        f.debug_struct("Saved").field("teams", &teams).finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Team {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Team")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// A workspace address Godwit calls: HTTPS on `*.slack.com`, nothing else
/// (the tokens and cookie go there).
pub fn valid_team_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest
        .split(['/', '?', '#', ':'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    host.ends_with(".slack.com") && host.len() > ".slack.com".len() && !host.contains('@')
}

impl Saved {
    /// The helper's line: `{"d": …, "teams": [{id, name, url, token, user}]}`;
    /// workspaces with an address that isn't Slack's are dropped.
    pub fn from_line(line: &str) -> Option<Self> {
        let mut saved: Saved = serde_json::from_str(line.trim()).ok()?;
        saved
            .teams
            .retain(|t| !t.id.is_empty() && t.token.starts_with("xoxc-") && valid_team_url(&t.url));
        (!saved.d.is_empty() && !saved.teams.is_empty()).then_some(saved)
    }
}

static SESSIONS: godwit_core::sessions::Sessions<Saved> = godwit_core::sessions::Sessions::new(
    NAME,
    MAX_SLOTS,
    Some(MAX_SLOTS),
    "Could not keep the Slack sign-in; sign in again after restarting",
    "Could not remove the kept Slack sign-in",
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
    fn saved_bytes_and_slot_names_stay_compatible() {
        let bytes = r#"{"d":"fake-d","teams":[{"id":"T1","name":"Acme","url":"https://acme.slack.com/","token":"xoxc-fake","user":"U1"}]}"#;
        let saved: Saved = serde_json::from_str(bytes).unwrap();
        assert_eq!(serde_json::to_string(&saved).unwrap(), bytes);
        assert_eq!(SESSIONS.name(0), NAME);
        assert_eq!(SESSIONS.name(3), format!("{NAME}-3"));
    }

    #[test]
    fn only_slack_addresses_are_called() {
        assert!(valid_team_url("https://acme.slack.com/"));
        assert!(valid_team_url("https://acme.enterprise.slack.com/"));
        assert!(!valid_team_url("http://acme.slack.com/"));
        assert!(!valid_team_url("https://slack.com.evil.example/"));
        assert!(!valid_team_url("https://evilslack.com/"));
        assert!(!valid_team_url("https://.slack.com/"));
    }

    #[test]
    fn the_helpers_line_is_checked() {
        let line = r#"{"d":"fake-d","teams":[
            {"id":"T1","name":"Acme","url":"https://acme.slack.com/","token":"xoxc-fake","user":"U1"},
            {"id":"T2","name":"Evil","url":"https://evil.example/","token":"xoxc-fake","user":"U2"}]}"#;
        let saved = Saved::from_line(line).unwrap();
        assert_eq!(saved.teams.len(), 1, "a workspace off slack.com is dropped");
        assert!(!format!("{saved:?}").contains("fake"));
        assert!(Saved::from_line(r#"{"d":"","teams":[]}"#).is_none());
        assert!(Saved::from_line("garbage").is_none());
    }
}
