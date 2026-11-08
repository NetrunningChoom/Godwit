// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Auto-delete (a Godwit extra, not in the official client): rules that
//! delete your own messages, by where (DMs, groups, server channels), what
//! (text, links, media) and when (older than N days, or before a date).
//!
//! It works from a local **index** of our own messages: per channel, the ids
//! of the messages we sent (with a kind letter; never their text) and the id
//! ranges the index has seen in full, so it knows nothing of ours is missing
//! there. Rules delete only what the index knows. The index fills in from:
//! - the live connection, which brings every new message in every channel we
//!   can see (the websocket subscribes to all of them:
//!   `bonfire/src/events/impl.rs`), so while connected each channel's range
//!   grows (minus a margin at both ends for clock differences);
//! - pages of history Godwit loads anyway, as you scroll or jump;
//! - a background indexer that reads the rest slowly, on a timer, oldest
//!   first, and only the part old enough for a rule to delete.
//!
//! A re-check (on request, or every 90 days by default) clears the index of
//! a rule's channels, and the indexer reads them again.
//!
//! Only channels some rule covers are indexed, and only while a rule exists.
//! Before each deletion the message is fetched and checked to be ours, so a
//! damaged index can never delete someone else's message (moderators can
//! delete others', and the server would let them).
//!
//! A rule can instead be a moderator **purge** (`everyone`): everyone's
//! messages in server channels where we have Manage Messages, pinned ones
//! kept by default. Purges don't use the index (it only knows our messages):
//! they read history up to the cutoff and delete as they go, remembering per
//! channel how far they've swept.
//!
//! Checked against the server source (stoatchat/stoatchat, 2026-10-31):
//! - `POST /channels/{id}/search` only takes query/pinned/limit/before/after/
//!   sort/include_users; the database's `MessageFilter.author` isn't exposed
//!   (`crates/delta/src/routes/channels/message_search.rs`), so history has
//!   to be read to find our messages.
//! - `GET …/messages` with `before`, `after` and `sort=Oldest` returns ids
//!   with `after < id < before`, ascending (`models/messages/ops/mongodb.rs`).
//! - Reading and deleting messages share a per-channel bucket of 15 requests
//!   per 10 s; responses carry `X-RateLimit-Remaining` and
//!   `X-RateLimit-Reset-After` (ms), and a 429's body is `{"retry_after": ms}`
//!   (`crates/core/ratelimits`, `crates/delta/src/util/ratelimits.rs`).
//! - Bulk delete needs Manage Messages and only reaches back a week, so
//!   messages are deleted one by one.
//!
//! This file holds the rules, the index and the pure logic; the paced queue
//! runs in backend.rs and the settings page in app/autodelete_ui.rs.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::api::Message;

/// Messages read per request by the indexer (the API's maximum).
pub const PAGE: usize = 100;
/// Deleting: by default one request every 1.5 s.
pub const DEFAULT_GAP_MS: u64 = 1500;
/// Indexing goes slower still: one page every 4 s.
pub const INDEX_GAP: Duration = Duration::from_secs(4);
/// Pages the indexer reads per channel each time it runs.
pub const INDEX_PAGES: usize = 10;
/// Channels the indexer works on each time it runs.
pub const INDEX_CHANNELS: usize = 3;
/// How often, while idle, the queue is given new work.
pub const CHECK_EVERY: Duration = Duration::from_secs(2 * 60);
/// Pages history loads for display (app.rs fetches 50 at a time).
pub const DISPLAY_PAGE: usize = 50;
/// Trimmed off both ends of a connected span, for clock differences with the
/// server (message ids carry the server's time).
pub const SPAN_MARGIN_MS: i64 = 5 * 60_000;
/// The default for [`AccountRules::recheck_days`].
pub const DEFAULT_RECHECK_DAYS: u32 = 90;
/// A rolling rule's cutoff moves on every minute, so indexing exactly up to
/// it always left a fresh sliver unindexed and the page said "Indexing" for
/// good: the index reads this far ahead of it (never past what's settled),
/// staying complete for a day. Deleting still stops at the cutoff itself.
pub const INDEX_AHEAD_MS: i64 = 86_400_000;
/// A purge deletes as it reads, so it never reads ahead: its channel counts
/// as swept once it's this close behind the moving cutoff (each check sweeps
/// the rest).
pub const SWEPT_SLACK_MS: i64 = 10 * 60_000;

/// How far the index must reach for `rule`: its cutoff, a day ahead for a
/// rolling one (see [`INDEX_AHEAD_MS`]), never past `now` less the margin.
pub fn index_until(rule: &Rule, now_ms: i64) -> String {
    let cutoff = rule.cutoff.at(now_ms);
    match rule.cutoff {
        Cutoff::OlderThanDays(_) => ulid_at((cutoff + INDEX_AHEAD_MS).min(now_ms - SPAN_MARGIN_MS).max(cutoff)),
        Cutoff::Before(_) => ulid_at(cutoff),
    }
}

/// How far back a rule reads by default (days); see [`Rule::lookback_days`].
pub const DEFAULT_LOOKBACK_DAYS: u32 = 365;
/// The lookbacks the editor offers (days; 0 is all history).
pub const LOOKBACK_CHOICES: [u32; 4] = [90, 365, 730, 0];

/// Rolling windows shorter than this get the "are you sure" note.
pub const ADVISED_MIN_DAYS: u32 = 30;

/// What a rule deletes, by the kind of message (a message is one kind: media
/// if it has attachments, else a link if its text has one, else text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Kinds {
    pub text: bool,
    pub links: bool,
    pub media: bool,
}

impl Default for Kinds {
    fn default() -> Self {
        Self {
            text: true,
            links: true,
            media: true,
        }
    }
}

impl Kinds {
    pub fn any(&self) -> bool {
        self.text || self.links || self.media
    }

    pub fn all(&self) -> bool {
        self.text && self.links && self.media
    }

    pub fn covers(&self, kind: Kind) -> bool {
        match kind {
            Kind::Text => self.text,
            Kind::Link => self.links,
            Kind::Media => self.media,
        }
    }
}

/// A message's kind for [`Kinds`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Link,
    Media,
}

impl Kind {
    /// How the index stores it.
    pub fn letter(self) -> char {
        match self {
            Kind::Text => 't',
            Kind::Link => 'l',
            Kind::Media => 'm',
        }
    }

    pub fn from_letter(c: char) -> Option<Self> {
        match c {
            't' => Some(Kind::Text),
            'l' => Some(Kind::Link),
            'm' => Some(Kind::Media),
            _ => None,
        }
    }
}

pub fn kind_of(m: &Message) -> Kind {
    if !m.attachments.is_empty() {
        Kind::Media
    } else if m.content.as_deref().is_some_and(has_link) {
        Kind::Link
    } else {
        Kind::Text
    }
}

fn has_link(text: &str) -> bool {
    text.contains("https://") || text.contains("http://")
}

/// Whether the index takes this message: ours, and not a system message.
pub fn is_ours(m: &Message, me: &str) -> bool {
    m.author == me && m.system.is_none()
}

/// When a rule's messages are old enough.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cutoff {
    /// Rolling: messages older than this many days (keeps working as time passes).
    OlderThanDays(u32),
    /// One-off: messages sent before this moment (ms since the epoch).
    Before(i64),
}

impl Cutoff {
    /// The moment (ms) before which messages go.
    pub fn at(&self, now_ms: i64) -> i64 {
        match *self {
            Cutoff::OlderThanDays(days) => now_ms - days as i64 * 86_400_000,
            Cutoff::Before(ms) => ms,
        }
    }

    /// Whether it reaches messages less than 30 days old (the advice note).
    pub fn is_short(&self, now_ms: i64) -> bool {
        self.at(now_ms) > now_ms - ADVISED_MIN_DAYS as i64 * 86_400_000
    }
}

/// Where a rule works. Channels are listed by id; a server can be taken
/// whole (its channels now and later) minus the ones unticked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scope {
    /// Every DM and group (as they come and go).
    pub all_private: bool,
    /// Individual DMs, groups and server channels.
    pub channels: BTreeSet<String>,
    /// Whole servers: server id → its channels left out.
    pub servers: BTreeMap<String, BTreeSet<String>>,
}

impl Scope {
    pub fn is_empty(&self) -> bool {
        !self.all_private && self.channels.is_empty() && self.servers.is_empty()
    }
}

/// A channel as the scope sees it.
#[derive(Debug, Clone, Copy)]
pub struct ChannelRef<'a> {
    pub id: &'a str,
    /// The server, for server channels.
    pub server: Option<&'a str>,
    /// DMs and groups (not Saved Notes).
    pub private: bool,
    /// Has messages we may read (a text channel we can see and read history in).
    pub readable: bool,
    /// We may delete others' messages here (Manage Messages).
    pub manage: bool,
}

/// The channels a scope covers right now, from every channel we know, in the given order.
pub fn resolve<'a>(scope: &Scope, known: &[ChannelRef<'a>]) -> Vec<&'a str> {
    known
        .iter()
        .filter(|c| c.readable)
        .filter(|c| {
            scope.channels.contains(c.id)
                || (scope.all_private && c.private)
                || c.server
                    .and_then(|s| scope.servers.get(s))
                    .is_some_and(|excluded| !excluded.contains(c.id))
        })
        .map(|c| c.id)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Unique within the account's rules.
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    pub scope: Scope,
    pub kinds: Kinds,
    pub cutoff: Cutoff,
    /// A moderator purge: everyone's messages, not just ours.
    #[serde(default)]
    pub everyone: bool,
    /// Purges leave pinned messages alone.
    #[serde(default = "yes")]
    pub keep_pinned: bool,
    /// How far back the rule ever reads, in days (0: all history). Reading
    /// history is the costly part (a page of 100 every few seconds), so the
    /// indexer, purges and re-checks never go past it and older messages are
    /// left alone. Saved rules without it get a year.
    #[serde(default = "default_lookback")]
    pub lookback_days: u32,
}

fn default_lookback() -> u32 {
    DEFAULT_LOOKBACK_DAYS
}

fn yes() -> bool {
    true
}

impl Rule {
    /// The oldest moment (ms) the rule reads, or None for all history.
    pub fn floor_ms(&self, now_ms: i64) -> Option<i64> {
        (self.lookback_days > 0).then(|| now_ms - self.lookback_days as i64 * 86_400_000)
    }

    /// The same as an id to read after ("" is the start of history).
    pub fn floor(&self, now_ms: i64) -> String {
        self.floor_ms(now_ms).map(ulid_at).unwrap_or_default()
    }

    /// Whether its cutoff is newer than its lookback (else there is nothing
    /// to read or delete: everything it would delete is out of reach).
    pub fn in_reach(&self, now_ms: i64) -> bool {
        self.floor_ms(now_ms).is_none_or(|f| self.cutoff.at(now_ms) > f)
    }

    /// The channels it works in now: a purge only where we may delete others'
    /// messages, and never in DMs or groups.
    pub fn channels<'a>(&self, known: &[ChannelRef<'a>]) -> Vec<&'a str> {
        let all = resolve(&self.scope, known);
        if !self.everyone {
            return all;
        }
        all.into_iter()
            .filter(|id| known.iter().any(|c| c.id == *id && c.manage && !c.private))
            .collect()
    }
}

/// What a rule has done, kept with it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleState {
    pub deleted: u64,
    /// When it last deleted something (ms).
    pub last_run: Option<i64>,
    pub last_error: Option<String>,
    /// When its channels' index was last cleared to be read again (or the
    /// rule was made) (ms).
    pub last_recheck: Option<i64>,
    /// Purges: per channel, every message up to this id has been dealt with.
    pub swept: BTreeMap<String, String>,
}

/// One channel's part of the index.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelIndex {
    /// Our messages: id → kind letter.
    pub ids: BTreeMap<String, char>,
    /// Id ranges seen in full (inclusive, sorted, not overlapping): every
    /// message of ours in them is in `ids`.
    pub known: Vec<(String, String)>,
}

impl ChannelIndex {
    /// Notes that everything from `lo` to `hi` (inclusive; "" is the start)
    /// has been seen, merging with what's known.
    pub fn learn(&mut self, lo: &str, hi: &str) {
        if lo > hi {
            return;
        }
        let (mut lo, mut hi) = (lo.to_owned(), hi.to_owned());
        self.known.retain(|(a, b)| {
            let overlaps = a.as_str() <= hi.as_str() && b.as_str() >= lo.as_str();
            if overlaps {
                if *a < lo {
                    lo = a.clone();
                }
                if *b > hi {
                    hi = b.clone();
                }
            }
            !overlaps
        });
        let at = self.known.partition_point(|(a, _)| a.as_str() < lo.as_str());
        self.known.insert(at, (lo, hi));
    }

    /// The parts of [start, `before`) not seen yet, oldest first, as
    /// (after, before) for reading: after None means from the start.
    pub fn gaps(&self, before: &str) -> Vec<(Option<String>, String)> {
        let mut out = Vec::new();
        let mut cursor: Option<&str> = None;
        for (a, b) in &self.known {
            if a.as_str() >= before {
                break;
            }
            let lo = cursor.unwrap_or("");
            if a.as_str() > lo && !(cursor.is_none() && a.is_empty()) {
                out.push((cursor.map(str::to_owned), a.clone()));
            }
            if cursor.is_none_or(|c| b.as_str() > c) {
                cursor = Some(b);
            }
        }
        match cursor {
            Some(c) if c >= before => {}
            _ => out.push((cursor.map(str::to_owned), before.to_owned())),
        }
        out
    }

    /// The parts of [`from`, `before`) not seen yet, as [`Self::gaps`] (a
    /// lookback's floor: older history doesn't count, and is never read).
    /// An empty `from` is the start of history.
    pub fn gaps_from(&self, from: &str, before: &str) -> Vec<(Option<String>, String)> {
        if from.is_empty() {
            return self.gaps(before);
        }
        let mut out = Vec::new();
        let mut cursor = from;
        for (a, b) in &self.known {
            if a.as_str() >= before {
                break;
            }
            if b.as_str() <= cursor {
                continue;
            }
            if a.as_str() > cursor {
                out.push((Some(cursor.to_owned()), a.clone()));
            }
            cursor = b;
        }
        if cursor < before {
            out.push((Some(cursor.to_owned()), before.to_owned()));
        }
        out
    }

    /// Whether everything from `from` to `before` has been seen.
    pub fn complete_between(&self, from: &str, before: &str) -> bool {
        self.gaps_from(from, before).is_empty()
    }

    /// Whether everything before `before` has been seen.
    pub fn complete_before(&self, before: &str) -> bool {
        self.gaps(before).is_empty()
    }
}

/// One account's rules and index (`autodelete.json` holds one per account).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountRules {
    /// The master switch. None (files from before it existed): on while any
    /// rule is enabled, so an update doesn't silently stop working rules.
    pub on: Option<bool>,
    pub rules: Vec<Rule>,
    pub state: BTreeMap<u64, RuleState>,
    /// Minimum gap between deletions (ms).
    pub gap_ms: u64,
    /// Re-check every rule this often (days); None means the default (90), Some(0) never.
    pub recheck_days: Option<u32>,
    /// Per channel (only channels a rule covers).
    pub index: BTreeMap<String, ChannelIndex>,
}

impl AccountRules {
    pub fn gap(&self) -> Duration {
        Duration::from_millis(if self.gap_ms == 0 {
            DEFAULT_GAP_MS
        } else {
            self.gap_ms.max(500)
        })
    }

    /// Whether anything may run at all.
    pub fn is_on(&self) -> bool {
        self.on.unwrap_or_else(|| self.rules.iter().any(|r| r.enabled))
    }

    pub fn next_id(&self) -> u64 {
        self.rules.iter().map(|r| r.id).max().unwrap_or(0) + 1
    }

    /// Saves a new or edited rule (a new one counts as freshly checked).
    pub fn upsert(&mut self, rule: Rule, now_ms: i64) {
        let id = rule.id;
        match self.rules.iter_mut().find(|r| r.id == id) {
            Some(old) => *old = rule,
            None => self.rules.push(rule),
        }
        self.state.entry(id).or_default().last_recheck.get_or_insert(now_ms);
    }

    pub fn remove(&mut self, id: u64) {
        self.rules.retain(|r| r.id != id);
        self.state.remove(&id);
    }

    /// Every channel some own-messages rule covers (enabled or not): what the index holds.
    pub fn scoped<'a>(&self, known: &[ChannelRef<'a>]) -> BTreeSet<&'a str> {
        self.rules
            .iter()
            .filter(|r| !r.everyone)
            .flat_map(|r| resolve(&r.scope, known))
            .collect()
    }

    /// Drops the index of channels no rule covers any more.
    pub fn prune(&mut self, scoped: &BTreeSet<&str>) {
        self.index.retain(|c, _| scoped.contains(c.as_str()));
    }

    /// A message of ours in a covered channel.
    pub fn record(&mut self, channel: &str, id: &str, kind: Kind) {
        if let Some(ix) = self.index.get_mut(channel) {
            ix.ids.insert(id.to_owned(), kind.letter());
        }
    }

    /// A message is gone (deleted here, elsewhere, or by us).
    pub fn forget(&mut self, channel: &str, id: &str) {
        if let Some(ix) = self.index.get_mut(channel) {
            ix.ids.remove(id);
        }
    }

    /// A page of history seen: our messages in it, and the range it proves.
    pub fn learn_page(&mut self, channel: &str, lo: &str, hi: &str, ours: impl IntoIterator<Item = (String, char)>) {
        if let Some(ix) = self.index.get_mut(channel) {
            ix.ids.extend(ours);
            ix.learn(lo, hi);
        }
    }

    /// Connected from `since_ms` until now: those channels have seen every new
    /// message in between (minus the margin at both ends).
    pub fn learn_live(&mut self, channels: &BTreeMap<String, i64>, now_ms: i64) {
        for (channel, since) in channels {
            let (lo, hi) = (since + SPAN_MARGIN_MS, now_ms - SPAN_MARGIN_MS);
            if lo < hi
                && let Some(ix) = self.index.get_mut(channel)
            {
                ix.learn(&ulid_at(lo), &ulid_at(hi));
            }
        }
    }

    /// Makes sure every covered channel has an index entry (new ones start unknown).
    pub fn cover(&mut self, scoped: &BTreeSet<&str>) {
        for c in scoped {
            self.index.entry((*c).to_owned()).or_default();
        }
    }

    /// Clears a rule's channels from the index, for the indexer to read again
    /// (a purge sweeps them again instead).
    pub fn recheck(&mut self, rule: u64, channels: &[&str], now_ms: i64) {
        let purge = self.rules.iter().any(|r| r.id == rule && r.everyone);
        if !purge {
            for c in channels {
                if let Some(ix) = self.index.get_mut(*c) {
                    *ix = ChannelIndex::default();
                }
            }
        }
        let state = self.state.entry(rule).or_default();
        if purge {
            state.swept.clear();
        }
        state.last_recheck = Some(now_ms);
    }

    /// The re-check interval in ms, if re-checks are on.
    pub fn recheck_every_ms(&self) -> Option<i64> {
        let days = self.recheck_days.unwrap_or(DEFAULT_RECHECK_DAYS);
        (days > 0).then(|| days as i64 * 86_400_000)
    }

    /// Enabled rules due a re-check.
    pub fn rechecks_due(&self, now_ms: i64) -> Vec<u64> {
        if !self.is_on() {
            return Vec::new();
        }
        let Some(every) = self.recheck_every_ms() else {
            return Vec::new();
        };
        self.rules
            .iter()
            .filter(|r| {
                r.enabled
                    && self
                        .state
                        .get(&r.id)
                        .and_then(|s| s.last_recheck)
                        .is_none_or(|t| now_ms - t >= every)
            })
            .map(|r| r.id)
            .collect()
    }

    /// What a rule would delete now: the index's messages of its kinds, sent
    /// before its cutoff, in its channels.
    pub fn deletable(&self, rule: &Rule, known: &[ChannelRef], now_ms: i64) -> Vec<(String, Vec<String>)> {
        if rule.everyone {
            return Vec::new();
        }
        let before = ulid_at(rule.cutoff.at(now_ms));
        let floor = rule.floor(now_ms);
        // Older than the lookback is left alone (and the range would be backwards).
        if floor >= before {
            return Vec::new();
        }
        resolve(&rule.scope, known)
            .into_iter()
            .filter_map(|c| {
                let ix = self.index.get(c)?;
                let ids: Vec<String> = ix
                    .ids
                    .range::<str, _>((
                        std::ops::Bound::Included(floor.as_str()),
                        std::ops::Bound::Excluded(before.as_str()),
                    ))
                    .filter(|(_, k)| Kind::from_letter(**k).is_some_and(|k| rule.kinds.covers(k)))
                    .map(|(id, _)| id.clone())
                    .collect();
                (!ids.is_empty()).then(|| (c.to_owned(), ids))
            })
            .collect()
    }

    /// A rule's channels whose history before its cutoff isn't fully indexed
    /// (for a purge: swept) yet.
    pub fn unindexed(&self, rule: &Rule, known: &[ChannelRef], now_ms: i64) -> usize {
        if !rule.in_reach(now_ms) {
            return 0;
        }
        let before = ulid_at(rule.cutoff.at(now_ms));
        let floor = rule.floor(now_ms);
        if rule.everyone {
            let before = ulid_at(rule.cutoff.at(now_ms) - SWEPT_SLACK_MS);
            let swept = self.state.get(&rule.id).map(|s| &s.swept);
            return rule
                .channels(known)
                .into_iter()
                .filter(|c| {
                    swept
                        .and_then(|s| s.get(*c))
                        .is_none_or(|until| until.as_str() < before.as_str())
                })
                .count();
        }
        resolve(&rule.scope, known)
            .into_iter()
            .filter(|c| {
                self.index
                    .get(*c)
                    .is_none_or(|ix| !ix.complete_between(&floor, &before))
            })
            .count()
    }
}

/// Work for the backend's paced queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// Read history from `after` (None: the start) up to `before`, oldest
    /// first, at most `pages` pages, noting our messages.
    Index {
        channel: String,
        after: Option<String>,
        before: String,
        pages: usize,
        me: String,
    },
    /// Delete these messages (ours, from the index) for a rule; each is
    /// fetched first and only deleted if `me` wrote it.
    Delete {
        rule: u64,
        channel: String,
        ids: Vec<String>,
        me: String,
    },
    /// A purge: read history from `after` up to `before` (at most `pages`
    /// pages) and delete everyone's messages of these kinds as it goes.
    Purge {
        rule: u64,
        channel: String,
        after: Option<String>,
        before: String,
        kinds: Kinds,
        keep_pinned: bool,
        pages: usize,
    },
}

/// The next work, in order: deletions the index already allows (enabled
/// rules); if none, a little indexing of what enabled rules will need (each
/// channel up to the latest cutoff among the rules covering it).
pub fn next_work(account: &AccountRules, known: &[ChannelRef], me: &str, now_ms: i64) -> Vec<Job> {
    if !account.is_on() {
        return Vec::new();
    }
    let enabled: Vec<&Rule> = account.rules.iter().filter(|r| r.enabled && r.kinds.any()).collect();
    let mut deletes = Vec::new();
    let mut taken: BTreeSet<String> = BTreeSet::new();
    for rule in enabled.iter().filter(|r| !r.everyone) {
        for (channel, ids) in account.deletable(rule, known, now_ms) {
            // A message two rules want goes once.
            let ids: Vec<String> = ids.into_iter().filter(|id| taken.insert(id.clone())).collect();
            if !ids.is_empty() {
                deletes.push(Job::Delete {
                    rule: rule.id,
                    channel,
                    ids,
                    me: me.to_owned(),
                });
            }
        }
    }
    if !deletes.is_empty() {
        return deletes;
    }
    // Purges, a few channels at a time, from where each got to.
    let mut purges = Vec::new();
    for rule in enabled.iter().filter(|r| r.everyone) {
        let before = ulid_at(rule.cutoff.at(now_ms));
        for channel in rule.channels(known) {
            let swept = account.state.get(&rule.id).and_then(|s| s.swept.get(channel)).cloned();
            // Never from before the lookback.
            let floor = rule.floor(now_ms);
            let after = match swept {
                Some(s) if s >= floor => Some(s),
                _ if floor.is_empty() => None,
                _ => Some(floor),
            };
            if after.as_deref().is_some_and(|a| a >= before.as_str()) {
                continue;
            }
            purges.push(Job::Purge {
                rule: rule.id,
                channel: channel.to_owned(),
                after,
                before: before.clone(),
                kinds: rule.kinds,
                keep_pinned: rule.keep_pinned,
                pages: INDEX_PAGES,
            });
        }
    }
    if !purges.is_empty() {
        purges.truncate(INDEX_CHANNELS);
        return purges;
    }
    let enabled: Vec<&Rule> = enabled.into_iter().filter(|r| !r.everyone).collect();
    // How far back each channel needs to be known (the latest cutoff covering
    // it), and how far to read once it isn't: a day ahead of a rolling
    // cutoff, so it isn't read again for every minute the cutoff moves.
    // (cutoff, until, floor): the floor is the widest lookback among them.
    let mut need: BTreeMap<&str, (String, String, String)> = BTreeMap::new();
    for rule in enabled.iter().filter(|r| r.in_reach(now_ms)) {
        let cutoff = ulid_at(rule.cutoff.at(now_ms));
        let until = index_until(rule, now_ms);
        let floor = rule.floor(now_ms);
        for c in resolve(&rule.scope, known) {
            let e = need
                .entry(c)
                .or_insert_with(|| (cutoff.clone(), until.clone(), floor.clone()));
            if cutoff > e.0 {
                e.0 = cutoff.clone();
            }
            if until > e.1 {
                e.1 = until.clone();
            }
            if floor < e.2 {
                e.2 = floor.clone();
            }
        }
    }
    need.into_iter()
        .filter(|(c, (cutoff, _, floor))| {
            account
                .index
                .get(*c)
                .is_none_or(|ix| !ix.complete_between(floor, cutoff))
        })
        .filter_map(|(c, (_, until, floor))| {
            let (after, before) = account
                .index
                .get(c)
                .map_or_else(
                    || ChannelIndex::default().gaps_from(&floor, &until),
                    |ix| ix.gaps_from(&floor, &until),
                )
                .into_iter()
                .next()?;
            Some(Job::Index {
                channel: c.to_owned(),
                after,
                before,
                pages: INDEX_PAGES,
                me: me.to_owned(),
            })
        })
        .take(INDEX_CHANNELS)
        .collect()
}

/// The range a page loaded for display proves (app.rs: newest first, 50 at a
/// time, before `before`): from its oldest message (or the start, if it came
/// back short) up to `before` (or its newest, for the latest page).
pub fn display_page_range(page: &[Message], before: Option<&str>) -> Option<(String, String)> {
    let oldest = page.iter().map(|m| m.id.as_str()).min()?;
    let newest = page.iter().map(|m| m.id.as_str()).max()?;
    let lo = if page.len() < DISPLAY_PAGE { "" } else { oldest };
    let hi = before.unwrap_or(newest);
    Some((lo.to_owned(), hi.max(newest).to_owned()))
}

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The smallest ULID made at `ms`: its 48-bit time in 10 Crockford base-32
/// characters, then zeros. Every message sent at or after `ms` sorts after it.
pub fn ulid_at(ms: i64) -> String {
    let mut t = ms.clamp(0, (1 << 48) - 1) as u64;
    let mut time = [b'0'; 10];
    for c in time.iter_mut().rev() {
        *c = CROCKFORD[(t & 31) as usize];
        t >>= 5;
    }
    // CROCKFORD is ASCII, so each byte is one char.
    let mut id: String = time.iter().map(|&b| char::from(b)).collect();
    id.push_str("0000000000000000");
    id
}

/// When a ULID was made (ms), if it is one.
#[cfg(test)]
pub fn ulid_ms(id: &str) -> Option<i64> {
    if id.len() != 26 {
        return None;
    }
    id.bytes().take(10).try_fold(0i64, |acc, b| {
        let v = CROCKFORD.iter().position(|&c| c == b.to_ascii_uppercase())? as i64;
        Some(acc * 32 + v)
    })
}

/// How the queue spaces its requests: never closer than `gap`, and when the
/// API says a bucket is empty or answers 429, waiting as long as it asks.
#[derive(Debug, Clone)]
pub struct Pacer {
    pub gap: Duration,
    /// Consecutive failures without an answer to go by, for the back-off.
    failures: u32,
}

/// What to do after a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Go on, after this wait.
    Next(Duration),
    /// Rate limited: send the same request again after this wait.
    Retry(Duration),
}

impl Pacer {
    pub fn new(gap: Duration) -> Self {
        Self { gap, failures: 0 }
    }

    /// After an answer: `status`, `X-RateLimit-Remaining`, `X-RateLimit-Reset-After`
    /// (ms) and, for a 429, the body's `retry_after` (ms).
    pub fn answered(
        &mut self,
        status: u16,
        remaining: Option<u32>,
        reset_after_ms: Option<u64>,
        retry_after_ms: Option<u64>,
    ) -> Verdict {
        if status == 429 {
            self.failures += 1;
            let asked = retry_after_ms.or(reset_after_ms).map(Duration::from_millis);
            return Verdict::Retry(asked.unwrap_or_else(|| self.backoff()).max(self.gap) + Duration::from_millis(250));
        }
        self.failures = 0;
        match (remaining, reset_after_ms) {
            // The bucket is empty: wait for it to refill rather than earn a 429.
            (Some(0), Some(ms)) => Verdict::Next(Duration::from_millis(ms).max(self.gap) + Duration::from_millis(250)),
            _ => Verdict::Next(self.gap),
        }
    }

    /// After a request that got no answer (network trouble): wait longer each time, up to 5 min.
    pub fn failed(&mut self) -> Duration {
        self.failures += 1;
        self.backoff()
    }

    fn backoff(&self) -> Duration {
        let secs = 5u64.saturating_mul(1 << self.failures.min(6));
        Duration::from_secs(secs.min(300))
    }

    /// Whether to give up on the current job (it keeps failing).
    pub fn exhausted(&self) -> bool {
        self.failures >= 8
    }
}

fn path() -> Option<std::path::PathBuf> {
    Some(godwit_core::config_dir::get()?.join("autodelete.json"))
}

/// Every account's rules (keyed by user id).
pub fn load() -> BTreeMap<String, AccountRules> {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Writes every account's rules, off the UI thread. Saves can come in quick
/// succession: they're written one at a time, and one overtaken by a newer
/// save is skipped, so an older state never lands last.
pub fn save(all: &BTreeMap<String, AccountRules>) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LATEST: AtomicU64 = AtomicU64::new(0);
    static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let Some(path) = path() else { return };
    let Ok(text) = serde_json::to_string(all) else { return };
    let seq = LATEST.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        let _turn = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        if LATEST.load(Ordering::SeqCst) != seq {
            return;
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Written beside, then moved over: a crash mid-write can't lose the rules.
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, author: &str, content: Option<&str>, attachments: usize, system: bool) -> Message {
        let mut v = serde_json::json!({ "_id": id, "channel": "C", "author": author });
        if let Some(c) = content {
            v["content"] = c.into();
        }
        if attachments > 0 {
            let file = serde_json::json!({ "_id": "F", "tag": "attachments", "filename": "a.png", "metadata": { "type": "Image", "width": 1, "height": 1 }, "content_type": "image/png", "size": 1 });
            v["attachments"] = serde_json::json!(vec![file; attachments]);
        }
        if system {
            v["system"] = serde_json::json!({ "type": "text", "content": "x" });
        }
        serde_json::from_value(v).unwrap()
    }

    /// An id made at `ms` (the rest filled with `fill`).
    fn id(ms: i64, fill: char) -> String {
        format!("{}{}", &ulid_at(ms)[..10], fill.to_string().repeat(16))
    }

    #[test]
    fn ulids_round_trip_and_sort_by_time() {
        let ms = ulid_ms("01HZ8Y7Q2K0000000000000000").unwrap();
        assert_eq!(ulid_at(ms), "01HZ8Y7Q2K0000000000000000");
        assert_eq!(ulid_ms(&ulid_at(1_700_000_000_123)), Some(1_700_000_000_123));
        assert!(ulid_at(1_700_000_000_000) < ulid_at(1_700_000_000_001));
        assert_eq!(ulid_at(0), "00000000000000000000000000");
        assert_eq!(ulid_ms("short"), None);
        assert_eq!(ulid_ms("UUUUUUUUUU0000000000000000"), None);
    }

    #[test]
    fn kinds_and_ownership() {
        assert!(is_ours(&msg("1", "me", Some("hi"), 0, false), "me"));
        assert!(!is_ours(&msg("1", "you", Some("hi"), 0, false), "me"));
        assert!(!is_ours(&msg("1", "me", Some("hi"), 0, true), "me"));
        // One kind each: media wins over links, links over text.
        assert_eq!(kind_of(&msg("1", "me", Some("see https://x.y"), 2, false)), Kind::Media);
        assert_eq!(kind_of(&msg("1", "me", Some("see http://x.y"), 0, false)), Kind::Link);
        assert_eq!(kind_of(&msg("1", "me", None, 0, false)), Kind::Text);
        for k in [Kind::Text, Kind::Link, Kind::Media] {
            assert_eq!(Kind::from_letter(k.letter()), Some(k));
        }
        let media = Kinds {
            text: false,
            links: false,
            media: true,
        };
        assert!(media.covers(Kind::Media) && !media.covers(Kind::Link));
    }

    fn known() -> Vec<ChannelRef<'static>> {
        vec![
            ChannelRef {
                id: "dm",
                server: None,
                private: true,
                readable: true,
                manage: false,
            },
            ChannelRef {
                id: "group",
                server: None,
                private: true,
                readable: true,
                manage: false,
            },
            ChannelRef {
                id: "a1",
                server: Some("A"),
                private: false,
                readable: true,
                manage: false,
            },
            ChannelRef {
                id: "a2",
                server: Some("A"),
                private: false,
                readable: true,
                manage: false,
            },
            ChannelRef {
                id: "a3",
                server: Some("A"),
                private: false,
                readable: false,
                manage: false,
            },
            ChannelRef {
                id: "b1",
                server: Some("B"),
                private: false,
                readable: true,
                manage: false,
            },
        ]
    }

    #[test]
    fn scopes_resolve_with_whole_servers_and_exclusions() {
        let mut scope = Scope {
            all_private: true,
            ..Default::default()
        };
        assert_eq!(resolve(&scope, &known()), vec!["dm", "group"]);
        scope.all_private = false;
        scope.servers.insert("A".into(), ["a2".to_string()].into());
        assert_eq!(resolve(&scope, &known()), vec!["a1"]);
        scope.channels.insert("b1".into());
        scope.channels.insert("group".into());
        assert_eq!(resolve(&scope, &known()), vec!["group", "a1", "b1"]);
        assert!(Scope::default().is_empty());
    }

    #[test]
    fn known_ranges_merge_and_leave_gaps() {
        let mut ix = ChannelIndex::default();
        assert_eq!(ix.gaps("M"), vec![(None, "M".to_string())]);
        ix.learn("C", "E");
        ix.learn("H", "J");
        assert_eq!(
            ix.gaps("M"),
            vec![
                (None, "C".into()),
                (Some("E".into()), "H".into()),
                (Some("J".into()), "M".into())
            ]
        );
        // Overlaps join; a range from the start removes the first gap.
        ix.learn("D", "I");
        assert_eq!(ix.known, vec![("C".to_string(), "J".to_string())]);
        ix.learn("", "C");
        assert_eq!(ix.known, vec![("".to_string(), "J".to_string())]);
        assert_eq!(ix.gaps("M"), vec![(Some("J".into()), "M".into())]);
        // Known past the cutoff: complete.
        assert!(ix.complete_before("G"));
        ix.learn("J", "Z");
        assert!(ix.complete_before("M"));
        // Backwards ranges are ignored.
        ix.learn("Z", "A");
        assert_eq!(ix.known.len(), 1);
    }

    fn rule(id: u64, cutoff: Cutoff) -> Rule {
        let mut scope = Scope::default();
        scope.channels.insert("dm".into());
        scope.channels.insert("a1".into());
        Rule {
            id,
            name: "r".into(),
            enabled: true,
            scope,
            kinds: Kinds::default(),
            cutoff,
            everyone: false,
            keep_pinned: true,
            // These tests read from the start of history.
            lookback_days: 0,
        }
    }

    #[test]
    fn deletes_what_the_index_knows_and_indexes_the_rest() {
        let day = 86_400_000;
        let now = 1_700_000_000_000;
        let mut account = AccountRules::default();
        account.upsert(rule(1, Cutoff::OlderThanDays(30)), now);
        let scoped = account.scoped(&known());
        assert_eq!(scoped, ["a1", "dm"].into());
        account.cover(&scoped);
        // Nothing known yet: index the start of each channel, up to a day
        // past the (rolling) cutoff.
        let cutoff = ulid_at(now - 29 * day);
        let work = next_work(&account, &known(), "me", now);
        assert_eq!(work.len(), 2);
        assert!(
            work.iter()
                .all(|j| matches!(j, Job::Index { after: None, before, pages: INDEX_PAGES, .. } if *before == cutoff))
        );
        // A page indexed: our old message (and a recent one) noted.
        let (old, recent) = (id(now - 40 * day, 'A'), id(now - day, 'B'));
        account.learn_page(
            "dm",
            "",
            &id(now - 35 * day, 'Z'),
            [(old.clone(), 't'), (recent.clone(), 'm')],
        );
        // The old one is deletable now; the recent one isn't old enough.
        let work = next_work(&account, &known(), "me", now);
        assert_eq!(
            work,
            vec![Job::Delete {
                rule: 1,
                channel: "dm".into(),
                ids: vec![old.clone()],
                me: "me".into()
            }]
        );
        account.forget("dm", &old);
        // Then indexing goes on from where it got to.
        let work = next_work(&account, &known(), "me", now);
        assert!(work.contains(&Job::Index {
            channel: "dm".into(),
            after: Some(id(now - 35 * day, 'Z')),
            before: cutoff.clone(),
            pages: INDEX_PAGES,
            me: "me".into()
        }));
        // Only its kinds: a media-only rule leaves text alone.
        let text = id(now - 50 * day, 'C');
        account.record("dm", &text, Kind::Text);
        account.rules[0].kinds = Kinds {
            text: false,
            links: false,
            media: true,
        };
        assert!(account.deletable(&account.rules[0], &known(), now).is_empty());
        // The master switch off: nothing, even with the rule enabled.
        account.on = Some(false);
        assert!(next_work(&account, &known(), "me", now).is_empty());
        assert!(account.rechecks_due(now).is_empty());
        // Older files have no switch: on while a rule is enabled.
        account.on = None;
        assert!(account.is_on());
        // Disabled rules do nothing.
        account.rules[0].enabled = false;
        assert!(!account.is_on());
        assert!(next_work(&account, &known(), "me", now).is_empty());
    }

    #[test]
    fn a_rolling_rule_finishes_indexing_though_its_cutoff_moves() {
        // It said "Indexing" for good: the cutoff moved on between indexing
        // and counting, always leaving a fresh sliver.
        let (day, hour) = (86_400_000, 3_600_000);
        let now = 1_700_000_000_000;
        let mut account = AccountRules::default();
        account.upsert(rule(1, Cutoff::OlderThanDays(30)), now);
        let scoped = account.scoped(&known());
        account.cover(&scoped);
        for job in next_work(&account, &known(), "me", now) {
            let Job::Index { channel, before, .. } = job else {
                panic!("indexing first")
            };
            account.learn_page(&channel, "", &before, std::iter::empty());
        }
        let rule = account.rules[0].clone();
        for later in [now, now + hour, now + 23 * hour] {
            assert_eq!(
                account.unindexed(&rule, &known(), later),
                0,
                "complete at +{}h",
                (later - now) / hour
            );
            assert!(
                next_work(&account, &known(), "me", later).is_empty(),
                "nothing to do yet"
            );
        }
        // A day on, the next stretch is read (a little, not everything again).
        assert_eq!(next_work(&account, &known(), "me", now + day + hour).len(), 2);
        // Never past what's settled: a short rolling rule reads only to now less the margin.
        let short = rule_with(Cutoff::OlderThanDays(1));
        assert_eq!(index_until(&short, now), ulid_at(now - SPAN_MARGIN_MS));
    }

    fn rule_with(cutoff: Cutoff) -> Rule {
        rule(9, cutoff)
    }

    #[test]
    fn live_spans_and_display_pages_count_as_seen() {
        let hour = 3_600_000;
        let t0 = 1_700_000_000_000;
        let mut account = AccountRules::default();
        account.upsert(rule(1, Cutoff::OlderThanDays(1)), t0);
        account.cover(&account.scoped(&known()));
        account.learn_live(&[("dm".to_string(), t0)].into(), t0 + 2 * hour);
        assert_eq!(
            account.index["dm"].known,
            vec![(ulid_at(t0 + SPAN_MARGIN_MS), ulid_at(t0 + 2 * hour - SPAN_MARGIN_MS))]
        );
        // Too short for the margins: nothing.
        account.learn_live(&[("a1".to_string(), t0)].into(), t0 + 60_000);
        assert!(account.index["a1"].known.is_empty());
        // Channels no rule covers aren't indexed.
        account.record("b1", &id(t0, 'A'), Kind::Text);
        assert!(!account.index.contains_key("b1"));
        // A full display page proves from its oldest to `before`; a short one from the start.
        let page: Vec<Message> = (0..DISPLAY_PAGE)
            .map(|i| msg(&id(t0 + i as i64, 'A'), "x", None, 0, false))
            .collect();
        assert_eq!(
            display_page_range(&page, Some("ZZZ")),
            Some((id(t0, 'A'), "ZZZ".into()))
        );
        assert_eq!(display_page_range(&page[..3], None), Some(("".into(), id(t0 + 2, 'A'))));
        assert_eq!(display_page_range(&[], None), None);
    }

    #[test]
    fn rechecks_clear_the_index_to_be_read_again() {
        let day = 86_400_000;
        let t0 = 1_700_000_000_000;
        let mut account = AccountRules::default();
        account.upsert(rule(1, Cutoff::OlderThanDays(1)), t0);
        assert!(account.rechecks_due(t0 + 89 * day).is_empty());
        assert_eq!(account.rechecks_due(t0 + 90 * day), vec![1]);
        account.cover(&account.scoped(&known()));
        account.learn_page("dm", "", "Z", [("X".to_string(), 't')]);
        account.recheck(1, &["dm"], t0 + 90 * day);
        assert_eq!(account.index["dm"], ChannelIndex::default());
        assert!(account.rechecks_due(t0 + 100 * day).is_empty());
        assert_eq!(account.unindexed(&account.rules[0], &known(), t0 + 100 * day), 2);
        account.recheck_days = Some(0);
        assert!(account.rechecks_due(t0 + 1000 * day).is_empty());
        // Removing a rule's channels from every scope drops their index.
        account.rules[0].scope.channels.remove("dm");
        let scoped = account.scoped(&known());
        account.prune(&scoped);
        assert!(!account.index.contains_key("dm"));
    }

    #[test]
    fn short_cutoffs_get_the_note() {
        let now = 1_700_000_000_000;
        assert!(Cutoff::OlderThanDays(7).is_short(now));
        assert!(!Cutoff::OlderThanDays(30).is_short(now));
        assert!(Cutoff::Before(now - 86_400_000).is_short(now));
        assert!(!Cutoff::Before(now - 60 * 86_400_000).is_short(now));
    }

    #[test]
    fn pacing_follows_the_rate_limits() {
        let gap = Duration::from_millis(1500);
        let mut p = Pacer::new(gap);
        assert_eq!(p.answered(200, Some(5), Some(9000), None), Verdict::Next(gap));
        assert_eq!(
            p.answered(204, Some(0), Some(4000), None),
            Verdict::Next(Duration::from_millis(4250))
        );
        assert_eq!(
            p.answered(429, None, None, Some(800)),
            Verdict::Retry(Duration::from_millis(1750))
        );
        assert_eq!(
            p.answered(429, Some(0), Some(6000), None),
            Verdict::Retry(Duration::from_millis(6250))
        );
        let mut p = Pacer::new(gap);
        let waits: Vec<_> = (0..8).map(|_| p.failed()).collect();
        assert!(waits.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(*waits.last().unwrap(), Duration::from_secs(300));
        assert!(p.exhausted());
        assert_eq!(AccountRules::default().gap(), Duration::from_millis(DEFAULT_GAP_MS));
        assert_eq!(
            AccountRules {
                gap_ms: 100,
                ..Default::default()
            }
            .gap(),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn purges_need_manage_messages_and_sweep_from_where_they_got_to() {
        let day = 86_400_000;
        let now = 1_700_000_000_000;
        let known = vec![
            ChannelRef {
                id: "dm",
                server: None,
                private: true,
                readable: true,
                manage: true,
            },
            ChannelRef {
                id: "mod",
                server: Some("A"),
                private: false,
                readable: true,
                manage: true,
            },
            ChannelRef {
                id: "plain",
                server: Some("A"),
                private: false,
                readable: true,
                manage: false,
            },
        ];
        let mut purge = rule(1, Cutoff::OlderThanDays(30));
        purge.everyone = true;
        purge.scope = Scope {
            all_private: true,
            servers: [("A".to_string(), BTreeSet::new())].into(),
            ..Default::default()
        };
        // Only server channels we can moderate; never DMs.
        assert_eq!(purge.channels(&known), vec!["mod"]);
        let mut account = AccountRules::default();
        account.upsert(purge, now);
        // Purges don't put channels in the index.
        assert!(account.scoped(&known).is_empty());
        let cutoff = ulid_at(now - 30 * day);
        let work = next_work(&account, &known, "me", now);
        assert_eq!(
            work,
            vec![Job::Purge {
                rule: 1,
                channel: "mod".into(),
                after: None,
                before: cutoff.clone(),
                kinds: Kinds::default(),
                keep_pinned: true,
                pages: INDEX_PAGES
            }]
        );
        assert_eq!(account.unindexed(&account.rules[0], &known, now), 1);
        // Swept to the cutoff: done until time moves on; a re-check sweeps again.
        account
            .state
            .get_mut(&1)
            .unwrap()
            .swept
            .insert("mod".into(), cutoff.clone());
        assert!(next_work(&account, &known, "me", now).is_empty());
        assert_eq!(account.unindexed(&account.rules[0], &known, now), 0);
        account.recheck(1, &["mod"], now);
        assert_eq!(next_work(&account, &known, "me", now).len(), 1);
    }

    #[test]
    fn lookback_bounds_reading_and_deleting() {
        let day = 86_400_000;
        let now = 1_700_000_000_000;
        let mut r = rule(1, Cutoff::OlderThanDays(30));
        r.lookback_days = 90;
        assert_eq!(r.floor_ms(now), Some(now - 90 * day));
        assert!(r.in_reach(now));
        // A cutoff older than the lookback leaves nothing in reach.
        r.cutoff = Cutoff::Before(now - 100 * day);
        assert!(!r.in_reach(now));
        r.lookback_days = 0;
        assert!(r.in_reach(now) && r.floor(now).is_empty());

        // Indexing starts at the floor, never at the start of history.
        r.cutoff = Cutoff::OlderThanDays(30);
        r.lookback_days = 90;
        let mut account = AccountRules::default();
        account.upsert(r.clone(), now);
        let jobs = next_work(&account, &known(), "me", now);
        assert!(!jobs.is_empty());
        for j in &jobs {
            let Job::Index { after, .. } = j else { panic!("{j:?}") };
            assert_eq!(after.as_deref(), Some(r.floor(now).as_str()));
        }
        // Once floor..cutoff is known the channel is done, though history
        // before the floor was never read.
        let (lo, hi) = (r.floor(now), ulid_at(r.cutoff.at(now) + day));
        for c in ["dm", "a1"] {
            account.index.entry(c.into()).or_default().learn(&lo, &hi);
        }
        assert_eq!(account.unindexed(&r, &known(), now), 0);
        assert!(next_work(&account, &known(), "me", now).is_empty());
        // Deleting ignores what the index holds from before the floor.
        let ix = account.index.get_mut("dm").unwrap();
        ix.ids.insert(id(now - 200 * day, 'A'), 't');
        ix.ids.insert(id(now - 60 * day, 'A'), 't');
        let del = account.deletable(&r, &known(), now);
        assert_eq!(del.len(), 1);
        assert_eq!(del[0].1.len(), 1);
    }

    #[test]
    fn purges_start_at_the_floor() {
        let now = 1_700_000_000_000;
        let mut r = rule(2, Cutoff::OlderThanDays(30));
        r.everyone = true;
        r.lookback_days = 90;
        r.scope = Scope::default();
        r.scope.channels.insert("mod".into());
        let known = [ChannelRef {
            id: "mod",
            server: Some("A"),
            private: false,
            readable: true,
            manage: true,
        }];
        let mut account = AccountRules::default();
        account.upsert(r.clone(), now);
        let jobs = next_work(&account, &known, "me", now);
        let [Job::Purge { after, .. }] = jobs.as_slice() else {
            panic!("{jobs:?}")
        };
        assert_eq!(after.as_deref(), Some(r.floor(now).as_str()));
        // A sweep from before the floor (an older lookback) restarts at it.
        account
            .state
            .entry(2)
            .or_default()
            .swept
            .insert("mod".into(), "0".into());
        let jobs = next_work(&account, &known, "me", now);
        assert!(matches!(&jobs[0], Job::Purge { after, .. } if after.as_deref() == Some(r.floor(now).as_str())));
    }

    #[test]
    fn gaps_start_at_the_floor() {
        let mut ix = ChannelIndex::default();
        assert_eq!(ix.gaps_from("D", "M"), vec![(Some("D".into()), "M".into())]);
        ix.learn("F", "H");
        assert_eq!(
            ix.gaps_from("D", "M"),
            vec![(Some("D".into()), "F".into()), (Some("H".into()), "M".into())]
        );
        // A floor inside a known range starts after it.
        assert_eq!(ix.gaps_from("G", "M"), vec![(Some("H".into()), "M".into())]);
        assert!(ix.complete_between("F", "H"));
        assert!(ix.gaps_from("M", "D").is_empty());
        assert_eq!(ix.gaps_from("", "M"), ix.gaps("M"));
    }

    #[test]
    fn old_rules_get_a_year() {
        let json = r#"{"id":1,"name":"x","enabled":true,"scope":{},"kinds":{},"cutoff":{"OlderThanDays":90}}"#;
        let r: Rule = serde_json::from_str(json).unwrap();
        assert_eq!(r.lookback_days, DEFAULT_LOOKBACK_DAYS);
    }

    #[test]
    fn a_server_minus_channels_plus_single_channels() {
        let mut scope = Scope::default();
        scope.servers.insert("A".into(), ["a2".to_string()].into());
        assert_eq!(resolve(&scope, &known()), vec!["a1"]);
        // A channel of another server, picked on its own, adds to it.
        scope.channels.insert("b1".into());
        assert_eq!(resolve(&scope, &known()), vec!["a1", "b1"]);
        // Picking an excluded channel singly brings it back.
        scope.channels.insert("a2".into());
        assert_eq!(resolve(&scope, &known()), vec!["a1", "a2", "b1"]);
    }
}
