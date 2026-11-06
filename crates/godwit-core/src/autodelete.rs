// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Auto-delete, as the UI sees it: an account's rules with their progress,
//! and the commands that change them. A Godwit extra (Godwit's), not part of
//! any official client; networks that support it (Stoat) keep the rules, the
//! index of our own messages and the paced queue themselves.

use std::sync::Arc;

/// A rule as the editor fills it in (new when `id` is None).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleDraft {
    pub id: Option<u64>,
    pub name: Arc<str>,
    pub enabled: bool,
    /// A moderator purge: everyone's messages (where we may), not just ours.
    pub everyone: bool,
    /// Purges leave pinned messages alone.
    pub keep_pinned: bool,
    /// Every DM and group.
    pub all_private: bool,
    /// Whole servers (their channels now and later), by id.
    pub servers: Vec<Arc<str>>,
    /// Single chats (DMs, groups, channels), by id.
    pub channels: Vec<Arc<str>>,
    pub text: bool,
    pub links: bool,
    pub media: bool,
    /// Rolling: older than this many days; else `before_ms`.
    pub older_than_days: Option<u32>,
    /// One-off: sent before this moment (ms since the epoch).
    pub before_ms: Option<i64>,
}

/// A rule in the list: what it does, in words, and how far it got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleView {
    pub draft: RuleDraft,
    pub where_text: Arc<str>,
    pub what_text: Arc<str>,
    pub when_text: Arc<str>,
    /// "Indexing: 3 of 5 channels done · ready to delete now: 12".
    pub progress: Arc<str>,
    pub deleted: u64,
    pub last_error: Option<Arc<str>>,
}

/// A server the editor can offer, with whether purging is possible there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerChoice {
    pub id: Arc<str>,
    pub name: Arc<str>,
}

/// An account's auto-delete page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutoDeleteView {
    /// The account's master switch: off, no rule deletes or indexes anything.
    pub on: bool,
    pub rules: Vec<RuleView>,
    /// Minimum gap between deletions (ms).
    pub gap_ms: u64,
    /// Re-check every this many days (0: never).
    pub recheck_days: u32,
    /// Work is queued or running.
    pub working: bool,
    pub index_error: Option<Arc<str>>,
    pub servers: Vec<ServerChoice>,
    /// DMs and groups, to pick one by one.
    pub chats: Vec<ServerChoice>,
}

/// What the page asks of the network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutoDeleteCommand {
    /// Send the page (opening it).
    Show,
    /// Save a rule (new or changed); the UI confirmed it first.
    Save(RuleDraft),
    Remove(u64),
    SetEnabled(u64, bool),
    /// The master switch for this account (the UI warned before turning it on).
    SetOn(bool),
    /// Forget a rule's channels from the index and read them again.
    Recheck(u64),
    SetGap(u64),
    SetRecheckDays(u32),
    /// Stop what's queued now (it carries on at the next check).
    Stop,
}
