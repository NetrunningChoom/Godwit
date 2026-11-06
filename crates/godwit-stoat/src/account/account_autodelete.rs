// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Auto-delete in the account (ported from Godwit's `autodelete_ui.rs`, its
//! non-UI half): feeding the index (live messages, pages loaded for display),
//! handing the backend's paced queue its next work, and the page the UI
//! shows. The rules, the index and the logic live in `autodelete.rs`.
//!
//! Nothing runs without rules: the account asks the backend to wake it for
//! the next check only while it has some (no polling while idle).

use crate::backend::AutoDeleteCmd;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use godwit_core::application::Event;
use godwit_core::autodelete::{AutoDeleteCommand, AutoDeleteView, RuleDraft, RuleView, ServerChoice};
use serde_json::Value;

use super::StoatAccount;
use crate::api::Message;
use crate::autodelete::{self, AccountRules, ChannelRef, Cutoff, Kinds, Rule, Scope};
use crate::backend::{AutoDeleteEv, Cmd};
use crate::permissions;

/// Auto-delete's state in the account.
#[derive(Default)]
pub(super) struct AutoDeleteRun {
    /// This account's rules and index, as saved (loaded once signed in).
    rules: AccountRules,
    loaded: bool,
    /// Work is in the backend's queue.
    busy: bool,
    /// When the queue was last given work.
    checked: Option<Instant>,
    /// While connected: since when (ms) each covered channel has been
    /// receiving its messages live.
    live_since: BTreeMap<String, i64>,
    /// When the index was last brought up to date and saved.
    touched: Option<Instant>,
    /// The index changed since it was last saved.
    dirty: bool,
    /// The last indexing problem (not tied to a rule).
    index_error: Option<String>,
    /// The page is open: send it again after changes.
    shown: bool,
}

/// A channel for the planner: what the store knows.
struct Known {
    id: String,
    name: String,
    server: Option<String>,
    private: bool,
    readable: bool,
    manage: bool,
}

fn refs(known: &[Known]) -> Vec<ChannelRef<'_>> {
    known
        .iter()
        .map(|c| ChannelRef {
            id: &c.id,
            server: c.server.as_deref(),
            private: c.private,
            readable: c.readable,
            manage: c.manage,
        })
        .collect()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn date_text(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

impl StoatAccount {
    /// The channels auto-delete can work in: DMs and groups, then each
    /// server's text channels.
    fn autodelete_known(&self) -> Vec<Known> {
        let mut out: Vec<Known> = self
            .store
            .private_channels()
            .into_iter()
            .filter(|c| matches!(c.channel_type.as_str(), "DirectMessage" | "Group"))
            .map(|c| Known {
                id: c.id.clone(),
                name: self.store.channel_title(c),
                server: None,
                private: true,
                readable: true,
                manage: false,
            })
            .collect();
        let mut servers: Vec<_> = self.store.servers.values().collect();
        servers.sort_by_cached_key(|s| s.name.to_lowercase());
        for s in servers {
            for id in &s.channels {
                let Some(c) = self.store.channels.get(id).filter(|c| c.channel_type == "TextChannel") else {
                    continue;
                };
                let readable = self
                    .store
                    .can(id, permissions::VIEW_CHANNEL | permissions::READ_MESSAGE_HISTORY);
                let manage = readable && self.store.can(id, permissions::MANAGE_MESSAGES);
                out.push(Known {
                    id: id.clone(),
                    name: self.store.channel_title(c),
                    server: Some(s.id.clone()),
                    private: false,
                    readable,
                    manage,
                });
            }
        }
        out
    }

    /// Keeps this account's entry in the shared file (other accounts' kept).
    fn autodelete_save(&mut self) {
        self.autodelete.dirty = false;
        if !self.persist_session {
            return;
        }
        let Some(me) = self.store.me.clone() else { return };
        let mut all = autodelete::load();
        if self.autodelete.rules.rules.is_empty() && self.autodelete.rules.index.is_empty() {
            all.remove(&me);
        } else {
            all.insert(me, self.autodelete.rules.clone());
        }
        autodelete::save(&all);
    }

    /// Signed in (Ready): this account's rules, and live coverage starts again.
    pub(super) fn autodelete_connected(&mut self) {
        if !self.autodelete.loaded {
            self.autodelete.loaded = true;
            if self.persist_session
                && let Some(me) = self.store.me.as_deref()
            {
                self.autodelete.rules = autodelete::load().remove(me).unwrap_or_default();
            }
        }
        self.autodelete.live_since.clear();
        self.autodelete.touched = None;
        self.autodelete.checked = None;
        self.autodelete_tick();
    }

    /// Disconnected: live coverage reaches now, then stops.
    pub(super) fn autodelete_disconnected(&mut self) {
        if self.autodelete.rules.rules.is_empty() {
            return;
        }
        let since = std::mem::take(&mut self.autodelete.live_since);
        self.autodelete.rules.learn_live(&since, now_ms());
        self.autodelete.busy = false;
        self.autodelete_save();
    }

    /// Signed out: forget the run (the backend drops its queue too).
    pub(super) fn autodelete_logged_out(&mut self) {
        self.autodelete = AutoDeleteRun::default();
    }

    /// Brings the index up to date (once a minute) and, when the queue is
    /// idle and a check is due, hands it its next work; then asks to be woken
    /// for the next check while there are rules.
    pub(super) fn autodelete_tick(&mut self) {
        if self.status != super::Status::Ready || self.autodelete.rules.rules.is_empty() {
            return;
        }
        let Some(me) = self.store.me.clone() else { return };
        let known = self.autodelete_known();
        let refs = refs(&known);
        let now = now_ms();
        if self
            .autodelete
            .touched
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(60))
        {
            self.autodelete.touched = Some(Instant::now());
            let ad = &mut self.autodelete;
            let scoped = ad.rules.scoped(&refs);
            ad.rules.cover(&scoped);
            ad.rules.prune(&scoped);
            // Channels newly covered start receiving live from now.
            ad.live_since.retain(|c, _| scoped.contains(c.as_str()));
            for c in &scoped {
                ad.live_since.entry((*c).to_owned()).or_insert(now);
            }
            ad.rules.learn_live(&ad.live_since, now);
            self.autodelete_save();
        }
        if !self.autodelete.busy
            && self
                .autodelete
                .checked
                .is_none_or(|t| t.elapsed() >= autodelete::CHECK_EVERY)
        {
            self.autodelete.checked = Some(Instant::now());
            let account = &mut self.autodelete.rules;
            // Rules due a re-check: their channels' index starts over.
            for id in account.rechecks_due(now) {
                let channels = account
                    .rules
                    .iter()
                    .find(|r| r.id == id)
                    .map(|r| autodelete::resolve(&r.scope, &refs))
                    .unwrap_or_default();
                account.recheck(id, &channels, now);
                self.autodelete.dirty = true;
            }
            let jobs = autodelete::next_work(&self.autodelete.rules, &refs, &me, now);
            if !jobs.is_empty() {
                self.autodelete.busy = true;
                let gap = self.autodelete.rules.gap();
                self.net.send(Cmd::AutoDelete(AutoDeleteCmd::AutoDelete { jobs, gap }));
            }
        }
        self.net
            .send(Cmd::AutoDelete(AutoDeleteCmd::AutoDeleteWake(autodelete::CHECK_EVERY)));
    }

    /// Progress from the backend's queue, into the index.
    pub(super) fn autodelete_event(&mut self, ev: AutoDeleteEv, out: &mut Vec<Event>) {
        let account = &mut self.autodelete.rules;
        match ev {
            AutoDeleteEv::Indexed { channel, lo, hi, ours } => {
                account.learn_page(&channel, &lo, &hi, ours);
                // Progress shows as it's made (a page every few seconds),
                // not only once a whole batch is done.
                self.autodelete.dirty = true;
                self.autodelete_show(out);
                return;
            }
            AutoDeleteEv::Deleted { rule, channel, id } => {
                account.forget(&channel, &id);
                let state = account.state.entry(rule).or_default();
                state.deleted += 1;
                state.last_run = Some(now_ms());
                state.last_error = None;
            }
            AutoDeleteEv::NotOurs { channel, id } => account.forget(&channel, &id),
            AutoDeleteEv::Swept { rule, channel, hi } => {
                account.state.entry(rule).or_default().swept.insert(channel, hi);
            }
            AutoDeleteEv::Failed { rule, channel, error } => {
                let name = self
                    .store
                    .channels
                    .get(&channel)
                    .map_or(channel, |c| self.store.channel_title(c));
                let text = format!("{name}: {error}");
                match rule {
                    Some(rule) => account.state.entry(rule).or_default().last_error = Some(text),
                    None => self.autodelete.index_error = Some(text),
                }
            }
            AutoDeleteEv::Idle => {
                self.autodelete.busy = false;
                self.autodelete_save();
                self.autodelete_show(out);
                return;
            }
        }
        self.autodelete.dirty = true;
    }

    /// A websocket event, for the index: our new messages, and messages gone or edited.
    pub(super) fn autodelete_live(&mut self, v: &Value) {
        let Some(me) = self.store.me.clone() else { return };
        if self.autodelete.rules.index.is_empty() {
            return;
        }
        let field = |k: &str| v.get(k).and_then(Value::as_str);
        let account = &mut self.autodelete.rules;
        match field("type") {
            Some("Message") => {
                if let Ok(m) = serde_json::from_value::<Message>(v.clone())
                    && autodelete::is_ours(&m, &me)
                {
                    account.record(&m.channel, &m.id, autodelete::kind_of(&m));
                    self.autodelete.dirty = true;
                }
            }
            Some("MessageDelete") => {
                if let (Some(channel), Some(id)) = (field("channel"), field("id")) {
                    account.forget(channel, id);
                    self.autodelete.dirty = true;
                }
            }
            Some("BulkMessageDelete") => {
                if let (Some(channel), Some(ids)) = (field("channel"), v.get("ids").and_then(Value::as_array)) {
                    for id in ids.iter().filter_map(Value::as_str) {
                        account.forget(channel, id);
                    }
                    self.autodelete.dirty = true;
                }
            }
            // An edit can add or remove a link: the kind from the edited message.
            Some("MessageUpdate") => {
                if let (Some(channel), Some(id)) = (field("channel"), field("id"))
                    && account.index.get(channel).is_some_and(|ix| ix.ids.contains_key(id))
                    && let Some(m) = self.store.messages.get(channel).and_then(|c| c.messages.get(id))
                {
                    account.record(channel, id, autodelete::kind_of(m));
                    self.autodelete.dirty = true;
                }
            }
            _ => {}
        }
    }

    /// A page of history loaded for display proves its range, and our
    /// messages in it go into the index.
    pub(super) fn autodelete_seen_page(&mut self, channel: &str, before: Option<&str>, page: &[Message], around: bool) {
        let Some(me) = self.store.me.clone() else { return };
        let account = &mut self.autodelete.rules;
        if !account.index.contains_key(channel) {
            return;
        }
        let range = if around {
            let lo = page.iter().map(|m| m.id.as_str()).min();
            let hi = page.iter().map(|m| m.id.as_str()).max();
            lo.zip(hi).map(|(lo, hi)| (lo.to_owned(), hi.to_owned()))
        } else {
            autodelete::display_page_range(page, before)
        };
        let Some((lo, hi)) = range else { return };
        let ours = page
            .iter()
            .filter(|m| autodelete::is_ours(m, &me))
            .map(|m| (m.id.clone(), autodelete::kind_of(m).letter()));
        account.learn_page(channel, &lo, &hi, ours);
        self.autodelete.dirty = true;
    }

    /// The page's commands. Saving trusts the UI to have confirmed first.
    pub(super) fn autodelete_command(&mut self, command: AutoDeleteCommand) -> Vec<Event> {
        let mut out = Vec::new();
        let now = now_ms();
        let account = &mut self.autodelete.rules;
        let mut changed = true;
        match command {
            AutoDeleteCommand::Show => {
                self.autodelete.shown = true;
                changed = false;
            }
            AutoDeleteCommand::Save(draft) => {
                let id = draft.id.unwrap_or_else(|| account.next_id());
                account.upsert(rule_from(&draft, id), now);
            }
            AutoDeleteCommand::Remove(id) => account.remove(id),
            AutoDeleteCommand::SetEnabled(id, on) => {
                if let Some(rule) = account.rules.iter_mut().find(|r| r.id == id) {
                    rule.enabled = on;
                }
            }
            AutoDeleteCommand::Recheck(id) => {
                let known = self.autodelete_known();
                let refs = refs(&known);
                let account = &mut self.autodelete.rules;
                let channels = account
                    .rules
                    .iter()
                    .find(|r| r.id == id)
                    .map(|r| autodelete::resolve(&r.scope, &refs))
                    .unwrap_or_default();
                account.recheck(id, &channels, now);
            }
            AutoDeleteCommand::SetOn(on) => {
                account.on = Some(on);
                // Off means now: drop what's queued too.
                if !on {
                    self.net.send(Cmd::AutoDelete(AutoDeleteCmd::AutoDeleteCancel));
                    self.autodelete.busy = false;
                }
            }
            AutoDeleteCommand::SetGap(ms) => account.gap_ms = ms.max(autodelete::DEFAULT_GAP_MS),
            AutoDeleteCommand::SetRecheckDays(days) => account.recheck_days = Some(days),
            AutoDeleteCommand::Stop => {
                self.net.send(Cmd::AutoDelete(AutoDeleteCmd::AutoDeleteCancel));
                self.autodelete.busy = false;
                changed = false;
            }
        }
        if changed {
            self.autodelete_save();
            // Changed rules: look again now rather than at the next check.
            self.autodelete.checked = None;
            self.autodelete.touched = None;
            self.autodelete_tick();
        }
        self.autodelete_show(&mut out);
        out
    }

    /// Sends the page, if it's open.
    pub(super) fn autodelete_show(&self, out: &mut Vec<Event>) {
        if !self.autodelete.shown {
            return;
        }
        let Some(account) = self.account.clone() else { return };
        out.push(Event::AutoDelete {
            account,
            view: self.autodelete_view(),
        });
    }

    fn autodelete_view(&self) -> AutoDeleteView {
        let known = self.autodelete_known();
        let refs = refs(&known);
        let now = now_ms();
        let account = &self.autodelete.rules;
        let rules = account
            .rules
            .iter()
            .map(|rule| {
                let state = account.state.get(&rule.id).cloned().unwrap_or_default();
                let channels = rule.channels(&refs).len();
                let unindexed = account.unindexed(rule, &refs, now);
                let ready: usize = account
                    .deletable(rule, &refs, now)
                    .iter()
                    .map(|(_, ids)| ids.len())
                    .sum();
                let mut progress = if rule.everyone {
                    if unindexed == 0 {
                        format!("Purge · swept: all {channels} channels")
                    } else {
                        format!("Purge · sweeping: {} of {channels} channels done", channels - unindexed)
                    }
                } else if unindexed == 0 {
                    format!("Indexed: all {channels} channels · ready to delete now: {ready}")
                } else {
                    format!(
                        "Indexing: {} of {channels} channels done · ready to delete now: {ready}",
                        channels - unindexed
                    )
                };
                if let Some(t) = state.last_run {
                    progress += &format!(" · last deleted {}", date_text(t));
                }
                RuleView {
                    draft: draft_of(rule),
                    where_text: scope_summary(&rule.scope, &known).into(),
                    what_text: kinds_summary(rule.kinds).into(),
                    when_text: cutoff_summary(rule.cutoff).into(),
                    progress: progress.into(),
                    deleted: state.deleted,
                    last_error: state.last_error.map(Into::into),
                }
            })
            .collect();
        let mut servers: Vec<ServerChoice> = self
            .store
            .servers
            .values()
            .map(|s| ServerChoice {
                id: s.id.as_str().into(),
                name: s.name.as_str().into(),
            })
            .collect();
        servers.sort_by_cached_key(|s| s.name.to_lowercase());
        let chats = known
            .iter()
            .filter(|c| c.private)
            .map(|c| ServerChoice {
                id: c.id.as_str().into(),
                name: c.name.as_str().into(),
            })
            .collect();
        AutoDeleteView {
            on: account.is_on(),
            rules,
            gap_ms: account.gap_ms.max(autodelete::DEFAULT_GAP_MS),
            recheck_days: account.recheck_days.unwrap_or(autodelete::DEFAULT_RECHECK_DAYS),
            working: self.autodelete.busy,
            index_error: self.autodelete.index_error.as_deref().map(Into::into),
            servers,
            chats,
        }
    }
}

/// A rule from the editor's draft.
fn rule_from(d: &RuleDraft, id: u64) -> Rule {
    let scope = Scope {
        all_private: d.all_private,
        channels: d.channels.iter().map(|c| c.to_string()).collect(),
        servers: d.servers.iter().map(|s| (s.to_string(), BTreeSet::new())).collect(),
    };
    let cutoff = match (d.older_than_days, d.before_ms) {
        (Some(days), _) => Cutoff::OlderThanDays(days.max(1)),
        (None, Some(ms)) => Cutoff::Before(ms),
        (None, None) => Cutoff::OlderThanDays(autodelete::ADVISED_MIN_DAYS),
    };
    Rule {
        id,
        name: if d.name.trim().is_empty() {
            format!("Rule {id}")
        } else {
            d.name.trim().to_owned()
        },
        enabled: d.enabled,
        scope,
        kinds: Kinds {
            text: d.text,
            links: d.links,
            media: d.media,
        },
        cutoff,
        everyone: d.everyone,
        keep_pinned: d.keep_pinned,
    }
}

fn draft_of(r: &Rule) -> RuleDraft {
    RuleDraft {
        id: Some(r.id),
        name: r.name.as_str().into(),
        enabled: r.enabled,
        everyone: r.everyone,
        keep_pinned: r.keep_pinned,
        all_private: r.scope.all_private,
        servers: r.scope.servers.keys().map(|s| Arc::from(s.as_str())).collect(),
        channels: r.scope.channels.iter().map(|c| Arc::from(c.as_str())).collect(),
        text: r.kinds.text,
        links: r.kinds.links,
        media: r.kinds.media,
        older_than_days: match r.cutoff {
            Cutoff::OlderThanDays(d) => Some(d),
            Cutoff::Before(_) => None,
        },
        before_ms: match r.cutoff {
            Cutoff::Before(ms) => Some(ms),
            Cutoff::OlderThanDays(_) => None,
        },
    }
}

fn scope_summary(scope: &Scope, known: &[Known]) -> String {
    let mut parts = Vec::new();
    if scope.all_private {
        parts.push("all DMs and groups".to_owned());
    }
    let servers = scope.servers.len();
    if servers > 0 {
        let left_out: usize = scope.servers.values().map(BTreeSet::len).sum();
        let mut s = format!("{servers} whole server{}", if servers == 1 { "" } else { "s" });
        if left_out > 0 {
            s += &format!(" ({left_out} channel{} left out)", if left_out == 1 { "" } else { "s" });
        }
        parts.push(s);
    }
    let channels = scope.channels.len();
    if channels > 0 {
        let names: Vec<String> = scope
            .channels
            .iter()
            .filter_map(|id| known.iter().find(|c| &c.id == id))
            .map(|c| c.name.clone())
            .take(2)
            .collect();
        let more = channels.saturating_sub(names.len());
        let mut s = names.join(", ");
        if more > 0 && names.is_empty() {
            s = format!("{more} channels");
        } else if more > 0 {
            s += &format!(" and {more} more");
        }
        parts.push(s);
    }
    if parts.is_empty() {
        "nowhere yet".into()
    } else {
        parts.join("; ")
    }
}

fn kinds_summary(k: Kinds) -> String {
    if k.all() {
        return "All messages".into();
    }
    let names: Vec<&str> = [(k.text, "text"), (k.links, "links"), (k.media, "media")]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, n)| n)
        .collect();
    let s = match names.as_slice() {
        [] => "nothing".to_owned(),
        [one] => format!("{one} only"),
        [a, b] => format!("{a} and {b}"),
        _ => names.join(", "),
    };
    let mut chars = s.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn cutoff_summary(c: Cutoff) -> String {
    match c {
        Cutoff::OlderThanDays(d) => format!("Older than {d} day{} (keeps going)", if d == 1 { "" } else { "s" }),
        Cutoff::Before(ms) => format!("Sent before {}", date_text(ms)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_read_well() {
        assert_eq!(kinds_summary(Kinds::default()), "All messages");
        assert_eq!(
            kinds_summary(Kinds {
                text: false,
                links: false,
                media: true
            }),
            "Media only"
        );
        assert_eq!(
            kinds_summary(Kinds {
                text: true,
                links: true,
                media: false
            }),
            "Text and links"
        );
        assert_eq!(
            cutoff_summary(Cutoff::OlderThanDays(1)),
            "Older than 1 day (keeps going)"
        );
        assert_eq!(scope_summary(&Scope::default(), &[]), "nowhere yet");
    }

    #[test]
    fn drafts_round_trip_and_never_go_below_a_day() {
        let draft = RuleDraft {
            name: "  Old DMs ".into(),
            enabled: true,
            all_private: true,
            servers: vec!["s1".into()],
            text: true,
            links: true,
            media: false,
            older_than_days: Some(0),
            keep_pinned: true,
            ..RuleDraft::default()
        };
        let rule = rule_from(&draft, 7);
        assert_eq!(rule.name, "Old DMs");
        assert_eq!(rule.cutoff, Cutoff::OlderThanDays(1));
        let back = draft_of(&rule);
        assert_eq!(back.id, Some(7));
        assert!(back.all_private && back.servers == vec![Arc::<str>::from("s1")]);
        assert!(back.text && back.links && !back.media);
    }

    #[test]
    fn single_dms_and_groups_are_kept_through_editing() {
        // Picked one by one (not "all DMs and groups"): they're the scope,
        // and editing the rule again gives them back (they were dropped).
        let draft = RuleDraft {
            name: "Old chat".into(),
            enabled: true,
            channels: vec!["dm1".into(), "group2".into()],
            text: true,
            older_than_days: Some(60),
            ..RuleDraft::default()
        };
        let rule = rule_from(&draft, 3);
        assert!(!rule.scope.all_private && rule.scope.servers.is_empty());
        assert_eq!(rule.scope.channels.len(), 2);
        let known = [
            Known {
                id: "dm1".into(),
                name: "Xaxais".into(),
                server: None,
                private: true,
                readable: true,
                manage: false,
            },
            Known {
                id: "elsewhere".into(),
                name: "Not this".into(),
                server: None,
                private: true,
                readable: true,
                manage: false,
            },
        ];
        // Only the picked chats are covered, never the others.
        assert_eq!(autodelete::resolve(&rule.scope, &refs(&known)), vec!["dm1"]);
        assert_eq!(scope_summary(&rule.scope, &known), "Xaxais and 1 more");
        let back = draft_of(&rule);
        assert_eq!(back.channels.len(), 2);
        assert!(back.channels.contains(&Arc::<str>::from("group2")));
    }
}
