// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Settings → a Stoat account → Auto-delete: fills the page (the `AdState`
//! global) from the account's view, and turns the editor's form into a rule.
//! The account keeps the rules and does the work (godwit-stoat).

use std::rc::Rc;
use std::sync::Arc;

use godwit_core::application::Action;
use godwit_core::autodelete::{AutoDeleteCommand, AutoDeleteView, RuleDraft, ServerChoice};
use godwit_core::ids::AccountId;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::ui::{AdPlaceItem, AdRuleItem, AdState, AppWindow};

/// Below this a rolling rule (or a date this recent) gets the note first.
const ADVISED_MIN_DAYS: i64 = 30;

#[derive(Default)]
pub struct AutoDeletePage {
    /// The account whose page is open.
    pub account: Option<AccountId>,
    /// The rule being edited (its id; None for a new one), and the whole
    /// servers and single chats it covers.
    editing: Option<Option<u64>>,
    chosen: Vec<Arc<str>>,
    chats_chosen: Vec<Arc<str>>,
    /// Channels left out of the whole servers (server, channel), and the
    /// servers shown open.
    left_out: Vec<(Arc<str>, Arc<str>)>,
    expanded: Vec<Arc<str>>,
    /// The servers (with their channels) and the DMs and groups last
    /// offered, in the page's order, and the channels already indexed.
    servers: Vec<ServerChoice>,
    chats: Vec<(Arc<str>, Arc<str>)>,
    indexed: Vec<Arc<str>>,
    /// The "Where" list's search, and what each shown row is.
    query: String,
    rows: Vec<Place>,
}

/// A row of the "Where" list.
#[derive(Clone)]
enum Place {
    Heading,
    Chat(Arc<str>),
    Server(Arc<str>),
    /// A server's channel: (server, channel).
    Channel(Arc<str>, Arc<str>),
}

/// The lookbacks the editor offers (days; 0 is all history), as the
/// account's rules keep them.
const DEFAULT_LOOKBACK_DAYS: i32 = 365;

fn lookback_label(days: u32) -> String {
    match days {
        0 => "all history".into(),
        90 => "3 months".into(),
        365 => "1 year".into(),
        730 => "2 years".into(),
        d => format!("{d} days"),
    }
}

/// One page of history read per this many seconds by the indexer (godwit-stoat
/// `INDEX_GAP`), for the estimate's duration.
const SECONDS_PER_PAGE: usize = 4;

/// What first reading costs, from what's known locally: a page of 100 per
/// channel not indexed yet is the least; busy channels take more (message
/// counts aren't known without asking, so none are asked for).
fn estimate_text(unread: usize, lookback_days: u32) -> String {
    if unread == 0 {
        return "These channels are already indexed: nothing to read at first.".into();
    }
    let secs = unread * SECONDS_PER_PAGE;
    let time = if secs < 90 {
        format!("{secs} seconds")
    } else {
        format!("{} minutes", secs.div_ceil(60))
    };
    let more = if lookback_days == 0 {
        " Busy channels have a lot more history."
    } else {
        ""
    };
    format!(
        "About {unread} page{} to read at first, one per channel not indexed yet (at least {time} in the background); busy channels need more.{more}",
        if unread == 1 { "" } else { "s" },
    )
}

fn days_ms(days: i64) -> i64 {
    days * 86_400_000
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Local midnight at the start of a `YYYY-MM-DD` date, in ms.
fn parse_date(text: &str) -> Option<i64> {
    use chrono::TimeZone;
    let day = chrono::NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d").ok()?;
    chrono::Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0)?)
        .earliest()
        .map(|d| d.timestamp_millis())
}

fn date_text(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

impl AutoDeletePage {
    /// Opening the tab: ask the account for its page.
    pub fn open(&mut self, account: AccountId) -> Action {
        self.account = Some(account.clone());
        self.editing = None;
        Action::AutoDelete(account, AutoDeleteCommand::Show)
    }

    fn command(&self, command: AutoDeleteCommand) -> Option<Action> {
        Some(Action::AutoDelete(self.account.clone()?, command))
    }

    /// Fills the page from the account's latest view.
    pub fn show(&mut self, ui: &AppWindow, view: Option<&AutoDeleteView>, account: &str) {
        let state = ui.global::<AdState>();
        state.set_account_label(account.into());
        let Some(view) = view else {
            state.set_rules(ModelRc::default());
            return;
        };
        let rules: Vec<AdRuleItem> = view
            .rules
            .iter()
            .map(|r| AdRuleItem {
                name: r.draft.name.as_ref().into(),
                enabled: r.draft.enabled,
                everyone: r.draft.everyone,
                r#where: r.where_text.as_ref().into(),
                what: r.what_text.as_ref().into(),
                when: r.when_text.as_ref().into(),
                progress: r.progress.as_ref().into(),
                deleted: r.deleted.to_string().into(),
                error: r.last_error.as_deref().unwrap_or_default().into(),
            })
            .collect();
        if !same(&state.get_rules(), &rules) {
            state.set_rules(ModelRc::new(Rc::new(VecModel::from(rules))));
        }
        state.set_on(view.on);
        state.set_gentle(view.gap_ms >= 3000);
        state.set_recheck_days(i32::try_from(view.recheck_days).unwrap_or(0));
        state.set_working(view.working);
        state.set_index_error(view.index_error.as_deref().unwrap_or_default().into());
        self.servers = view.servers.clone();
        self.indexed = view.indexed.clone();
        self.chats = view.chats.iter().map(|s| (s.id.clone(), s.name.clone())).collect();
        if self.editing.is_some() {
            self.show_places(ui);
        }
    }

    /// The "Where" list: DMs and groups one by one (unless all of them are
    /// taken), then servers whole; filtered by the search.
    fn show_places(&mut self, ui: &AppWindow) {
        let state = ui.global::<AdState>();
        let query = self.query.trim().to_lowercase();
        let matches = |name: &str| query.is_empty() || name.to_lowercase().contains(&query);
        let mut items = Vec::new();
        let mut rows = Vec::new();
        let heading = |items: &mut Vec<AdPlaceItem>, rows: &mut Vec<Place>, name: &str| {
            items.push(AdPlaceItem {
                name: name.into(),
                chosen: false,
                header: true,
                child: false,
                expandable: false,
                expanded: false,
            });
            rows.push(Place::Heading);
        };
        if !state.get_ed_all_private() && !state.get_ed_everyone() {
            let chats: Vec<_> = self.chats.iter().filter(|(_, n)| matches(n)).collect();
            if !chats.is_empty() {
                heading(&mut items, &mut rows, "Direct messages and groups");
            }
            for (id, name) in chats {
                items.push(AdPlaceItem {
                    name: name.as_ref().into(),
                    chosen: self.chats_chosen.contains(id),
                    header: false,
                    child: false,
                    expandable: false,
                    expanded: false,
                });
                rows.push(Place::Chat(id.clone()));
            }
        }
        let servers: Vec<_> = self.servers.iter().filter(|s| matches(&s.name)).collect();
        if !servers.is_empty() {
            heading(&mut items, &mut rows, "Servers (whole, or pick channels)");
        }
        for server in servers {
            let open = self.expanded.contains(&server.id) && !server.channels.is_empty();
            items.push(AdPlaceItem {
                name: server.name.as_ref().into(),
                chosen: self.chosen.contains(&server.id),
                header: false,
                child: false,
                expandable: !server.channels.is_empty(),
                expanded: open,
            });
            rows.push(Place::Server(server.id.clone()));
            if open {
                for c in &server.channels {
                    items.push(AdPlaceItem {
                        name: c.name.as_ref().into(),
                        chosen: self.channel_chosen(&server.id, &c.id),
                        header: false,
                        child: true,
                        expandable: false,
                        expanded: false,
                    });
                    rows.push(Place::Channel(server.id.clone(), c.id.clone()));
                }
            }
        }
        self.rows = rows;
        state.set_places(ModelRc::new(Rc::new(VecModel::from(items))));
        let estimate = self.estimate(ui);
        state.set_ed_estimate(estimate.into());
    }

    /// Whether a server's channel is covered: with the server taken whole,
    /// unless left out; otherwise only if picked on its own.
    fn channel_chosen(&self, server: &Arc<str>, channel: &Arc<str>) -> bool {
        if self.chosen.contains(server) {
            !self.left_out.iter().any(|(s, c)| s == server && c == channel)
        } else {
            self.chats_chosen.contains(channel)
        }
    }

    /// The channels the form covers now (chats, whole servers minus the left
    /// out, single channels), for the estimate; a purge never takes DMs.
    fn covered(&self, ui: &AppWindow) -> Vec<Arc<str>> {
        let state = ui.global::<AdState>();
        let everyone = state.get_ed_everyone();
        let mut out: Vec<Arc<str>> = Vec::new();
        if state.get_ed_all_private() && !everyone {
            out.extend(self.chats.iter().map(|(id, _)| id.clone()));
        } else if !everyone {
            out.extend(self.chats_chosen.iter().filter(|c| self.is_chat(c)).cloned());
        }
        for server in &self.servers {
            for c in &server.channels {
                if self.channel_chosen(&server.id, &c.id) {
                    out.push(c.id.clone());
                }
            }
        }
        out
    }

    fn is_chat(&self, id: &Arc<str>) -> bool {
        self.chats.iter().any(|(c, _)| c == id)
    }

    /// The editor's "about N pages" line (nothing until a place is chosen).
    fn estimate(&self, ui: &AppWindow) -> String {
        let covered = self.covered(ui);
        if covered.is_empty() {
            return String::new();
        }
        let unread = covered.iter().filter(|c| !self.indexed.contains(c)).count();
        let lookback = u32::try_from(ui.global::<AdState>().get_ed_lookback()).unwrap_or(0);
        estimate_text(unread, lookback)
    }

    /// A server's channels opened or closed.
    pub fn toggle_expand(&mut self, ui: &AppWindow, index: usize) {
        let Some(Place::Server(id)) = self.rows.get(index) else {
            return;
        };
        if let Some(at) = self.expanded.iter().position(|e| e == id) {
            self.expanded.remove(at);
        } else {
            self.expanded.push(id.clone());
        }
        self.show_places(ui);
    }

    /// The search changed.
    pub fn search(&mut self, ui: &AppWindow, query: String) {
        self.query = query;
        self.show_places(ui);
    }

    /// "All DMs and groups" or "whose messages" changed: the list follows.
    pub fn refresh_places(&mut self, ui: &AppWindow) {
        if self.editing.is_some() {
            self.show_places(ui);
        }
    }

    /// "New rule" (`index` None) or a rule's "Edit": the form, filled in.
    pub fn edit(&mut self, ui: &AppWindow, view: Option<&AutoDeleteView>, index: Option<usize>) {
        let draft = index
            .and_then(|i| view?.rules.get(i))
            .map(|r| r.draft.clone())
            .unwrap_or_else(|| RuleDraft {
                enabled: true,
                keep_pinned: true,
                text: true,
                links: true,
                media: true,
                older_than_days: Some(90),
                lookback_days: DEFAULT_LOOKBACK_DAYS as u32,
                ..RuleDraft::default()
            });
        self.editing = Some(draft.id);
        self.chosen = draft.servers.clone();
        self.chats_chosen = draft.channels.clone();
        self.left_out = draft.left_out.clone();
        // Servers with a channel picked or left out show open.
        self.expanded = self
            .servers
            .iter()
            .filter(|s| {
                s.channels.iter().any(|c| {
                    draft.channels.contains(&c.id) || draft.left_out.iter().any(|(sv, ch)| *sv == s.id && *ch == c.id)
                })
            })
            .map(|s| s.id.clone())
            .collect();
        self.query.clear();
        ui.global::<AdState>().set_place_query("".into());
        let state = ui.global::<AdState>();
        state.set_ed_name(draft.name.as_ref().into());
        state.set_ed_enabled(draft.enabled);
        state.set_ed_everyone(draft.everyone);
        state.set_ed_keep_pinned(draft.keep_pinned);
        state.set_ed_all_private(draft.all_private);
        state.set_ed_text(draft.text);
        state.set_ed_links(draft.links);
        state.set_ed_media(draft.media);
        state.set_ed_rolling(draft.before_ms.is_none());
        state.set_ed_days(draft.older_than_days.unwrap_or(90).to_string().into());
        state.set_ed_date(draft.before_ms.map(date_text).unwrap_or_default().into());
        state.set_ed_lookback(i32::try_from(draft.lookback_days).unwrap_or(DEFAULT_LOOKBACK_DAYS));
        state.set_ed_error("".into());
        state.set_purge_warning(false);
        self.show_places(ui);
        state.set_step(1);
    }

    pub fn toggle_place(&mut self, ui: &AppWindow, index: usize) {
        let list = match self.rows.get(index) {
            Some(Place::Chat(id)) => Some((&mut self.chats_chosen, id.clone())),
            Some(Place::Server(id)) => {
                // Whole or not at all: single picks and left-out channels of it start over.
                let id = id.clone();
                let own: Vec<Arc<str>> = self
                    .servers
                    .iter()
                    .filter(|s| s.id == id)
                    .flat_map(|s| s.channels.iter().map(|c| c.id.clone()))
                    .collect();
                self.chats_chosen.retain(|c| !own.contains(c));
                self.left_out.retain(|(s, _)| *s != id);
                Some((&mut self.chosen, id))
            }
            Some(Place::Channel(server, channel)) => {
                let (server, channel) = (server.clone(), channel.clone());
                if self.chosen.contains(&server) {
                    if let Some(at) = self.left_out.iter().position(|(s, c)| *s == server && *c == channel) {
                        self.left_out.remove(at);
                    } else {
                        self.left_out.push((server, channel));
                    }
                    self.show_places(ui);
                    return;
                }
                Some((&mut self.chats_chosen, channel))
            }
            Some(Place::Heading) | None => None,
        };
        let Some((list, id)) = list else { return };
        if let Some(at) = list.iter().position(|c| *c == id) {
            list.remove(at);
        } else {
            list.push(id);
        }
        self.show_places(ui);
    }

    fn in_server(&self, server: &Arc<str>, channel: &Arc<str>) -> bool {
        self.servers
            .iter()
            .any(|s| s.id == *server && s.channels.iter().any(|c| c.id == *channel))
    }

    /// The form as a rule, or what's wrong with it.
    fn draft(&self, ui: &AppWindow) -> Result<RuleDraft, &'static str> {
        let state = ui.global::<AdState>();
        let everyone = state.get_ed_everyone();
        let (text, links, media) = (state.get_ed_text(), state.get_ed_links(), state.get_ed_media());
        if !(text || links || media) {
            return Err("Choose at least one kind of message.");
        }
        let all_private = state.get_ed_all_private() && !everyone;
        // Single DMs and groups only for your own messages, and not when all are taken.
        let channels: Vec<Arc<str>> = self
            .chats_chosen
            .iter()
            .filter(|c| !((everyone || all_private) && self.is_chat(c)))
            .filter(|c| !self.chosen.iter().any(|s| self.in_server(s, c)))
            .cloned()
            .collect();
        if !all_private && self.chosen.is_empty() && channels.is_empty() {
            return Err("Choose where the rule works.");
        }
        let (older_than_days, before_ms) = if state.get_ed_rolling() {
            let days: u32 = state
                .get_ed_days()
                .trim()
                .parse()
                .ok()
                .filter(|d| *d >= 1)
                .ok_or("Give a number of days (1 or more).")?;
            (Some(days), None)
        } else {
            let ms = parse_date(&state.get_ed_date()).ok_or("Give the date as YYYY-MM-DD.")?;
            if ms > now_ms() {
                return Err("The date is in the future.");
            }
            (None, Some(ms))
        };
        let lookback_days = u32::try_from(state.get_ed_lookback()).unwrap_or(0);
        // A rule whose age reaches past its lookback would never find anything.
        if beyond_lookback(older_than_days, before_ms, now_ms(), lookback_days) {
            return Err("Messages that old are beyond how far back the rule reads: pick a longer lookback.");
        }
        Ok(RuleDraft {
            id: self.editing.flatten(),
            name: state.get_ed_name().trim().into(),
            enabled: state.get_ed_enabled(),
            everyone,
            keep_pinned: state.get_ed_keep_pinned(),
            all_private,
            servers: self.chosen.clone(),
            channels,
            text,
            links,
            media,
            older_than_days,
            before_ms,
            left_out: self
                .left_out
                .iter()
                .filter(|(s, _)| self.chosen.contains(s))
                .cloned()
                .collect(),
            lookback_days,
        })
    }

    /// "Next": a short cutoff gets the note first; then the confirmation
    /// saying exactly what will be deleted.
    pub fn next(&mut self, ui: &AppWindow) {
        let state = ui.global::<AdState>();
        let draft = match self.draft(ui) {
            Ok(draft) => draft,
            Err(e) => {
                state.set_ed_error(e.into());
                return;
            }
        };
        state.set_ed_error("".into());
        let kinds: Vec<&str> = [(draft.text, "text"), (draft.links, "links"), (draft.media, "media")]
            .into_iter()
            .filter(|(on, _)| *on)
            .map(|(_, n)| n)
            .collect();
        let mut places = Vec::new();
        if draft.all_private {
            places.push("all your DMs and groups".to_owned());
        }
        let names: Vec<String> = self
            .chats
            .iter()
            .filter(|(id, _)| draft.channels.contains(id))
            .map(|(id, name)| (id, name))
            .chain(
                self.servers
                    .iter()
                    .filter(|s| self.chosen.contains(&s.id))
                    .map(|s| (&s.id, &s.name)),
            )
            .map(|(_, name)| name.to_string())
            .collect();
        let singles = draft.channels.iter().filter(|c| !self.is_chat(c)).count();
        let mut names = names;
        if singles > 0 {
            names.push(format!(
                "{singles} single channel{}",
                if singles == 1 { "" } else { "s" }
            ));
        }
        if !draft.left_out.is_empty() {
            names.push(format!("({} channels left out)", draft.left_out.len()));
        }
        if !names.is_empty() {
            places.push(names.join(", "));
        }
        let when = match (draft.older_than_days, draft.before_ms) {
            (Some(d), _) => format!("older than {d} day{}, from now on", if d == 1 { "" } else { "s" }),
            (None, Some(ms)) => format!("sent before {}", date_text(ms)),
            (None, None) => String::new(),
        };
        let whose = if draft.everyone { "everyone's" } else { "your" };
        state.set_ed_summary(
            format!(
                "{} will permanently delete {whose} messages ({}) {when}, in {}. It only reads back {}; older messages are left alone.",
                if draft.name.is_empty() {
                    "This rule"
                } else {
                    draft.name.as_ref()
                },
                kinds.join(", "),
                places.join(" and "),
                lookback_label(draft.lookback_days),
            )
            .into(),
        );
        let short = match (draft.older_than_days, draft.before_ms) {
            (Some(d), _) => i64::from(d) < ADVISED_MIN_DAYS,
            (None, Some(ms)) => ms > now_ms() - days_ms(ADVISED_MIN_DAYS),
            (None, None) => false,
        };
        state.set_step(if short && draft.enabled { 2 } else { 3 });
    }

    /// "Save" after the confirmation.
    pub fn save(&mut self, ui: &AppWindow) -> Option<Action> {
        let draft = self.draft(ui).ok()?;
        self.editing = None;
        ui.global::<AdState>().set_step(0);
        self.command(AutoDeleteCommand::Save(draft))
    }

    pub fn act(&self, command: AutoDeleteCommand) -> Option<Action> {
        self.command(command)
    }

    /// A rule's id by its row.
    pub fn rule_id(view: Option<&AutoDeleteView>, index: usize) -> Option<u64> {
        view?.rules.get(index)?.draft.id
    }
}

fn same(model: &ModelRc<AdRuleItem>, rules: &[AdRuleItem]) -> bool {
    model.row_count() == rules.len() && model.iter().zip(rules).all(|(a, b)| a == *b)
}

/// Whether a rule's age reaches past its lookback (0: all history), so it
/// would never find anything.
fn beyond_lookback(older_than_days: Option<u32>, before_ms: Option<i64>, now: i64, lookback_days: u32) -> bool {
    if lookback_days == 0 {
        return false;
    }
    let reach = match (older_than_days, before_ms) {
        (Some(d), _) => days_ms(i64::from(d)),
        (None, Some(ms)) => now - ms,
        (None, None) => 0,
    };
    reach >= days_ms(i64::from(lookback_days))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookback_must_reach_past_the_age() {
        let now = 1_700_000_000_000;
        assert!(!beyond_lookback(Some(90), None, now, 365));
        assert!(beyond_lookback(Some(400), None, now, 365));
        assert!(beyond_lookback(Some(365), None, now, 365));
        assert!(!beyond_lookback(Some(4000), None, now, 0));
        assert!(beyond_lookback(None, Some(now - days_ms(100)), now, 90));
        assert!(!beyond_lookback(None, Some(now - days_ms(100)), now, 365));
    }

    #[test]
    fn estimate_counts_unread_channels() {
        assert!(estimate_text(0, 365).contains("already indexed"));
        assert!(estimate_text(1, 365).starts_with("About 1 page to"));
        assert!(estimate_text(5, 365).contains("5 pages") && estimate_text(5, 365).contains("20 seconds"));
        assert!(estimate_text(60, 365).contains("4 minutes"));
        assert!(estimate_text(3, 0).contains("Busy channels have a lot more"));
        assert!(!estimate_text(3, 90).contains("Busy channels have a lot more"));
    }
}
