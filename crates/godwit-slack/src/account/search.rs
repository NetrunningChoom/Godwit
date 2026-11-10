// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Message search: Godwit's query as Slack's search modifiers, and the
//! matches back as messages (https://api.slack.com/methods/search.messages,
//! modifiers: https://slack.com/help/articles/202528808-Search-in-Slack).

use super::*;
use godwit_core::search::{SearchDate, SearchQuery, SearchScope};

fn day(d: SearchDate) -> String {
    format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)
}

impl SlackAccount {
    /// `query` in Slack's words, and the one chat matches must be in when
    /// no modifier can name it (a direct or group chat's own search).
    pub(super) fn search_text(&self, team: &str, channel: &str, query: &SearchQuery) -> (String, Option<String>) {
        let mut words: Vec<String> = query.text.split_whitespace().map(str::to_owned).collect();
        let conversation = |c: &str| self.workspaces.get(team).and_then(|w| w.conversations.get(c));
        let named = |c: &str| {
            conversation(c)
                .filter(|c| c.kind == Kind::Channel && !c.name.is_empty())
                .map(|c| format!("in:#{}", c.name))
        };
        let mut only = None;
        if let Some(target) = query.in_room.as_ref().and_then(|r| parts(&r.raw)).map(|(_, c, _)| c) {
            match named(target) {
                Some(word) => words.push(word),
                None => only = Some(target.to_owned()),
            }
        } else if query.scope == SearchScope::Channel {
            match named(channel) {
                Some(word) => words.push(word),
                None => only = Some(channel.to_owned()),
            }
        }
        if let Some((_, user)) = query.from.as_ref().and_then(|u| u.raw.split_once(':')) {
            words.push(format!("from:<@{user}>"));
        }
        if let Some(d) = query.before {
            words.push(format!("before:{}", day(d)));
        }
        if let Some(d) = query.after {
            words.push(format!("after:{}", day(d)));
        }
        if let Some(d) = query.during {
            words.push(format!("on:{}", day(d)));
        }
        if query.pinned {
            words.push("has:pin".into());
        }
        (words.join(" "), only)
    }

    pub(super) fn search_answer(&mut self, team: &str, result: Result<Value, String>, out: &mut Vec<Event>) {
        let Some(Search {
            room,
            query,
            offset,
            only,
        }) = self.searching.take()
        else {
            return;
        };
        let v = match result {
            Ok(v) => v,
            Err(e) => {
                out.push(Event::SearchResults {
                    room,
                    query,
                    offset,
                    next_offset: None,
                    total: None,
                    notice: Some(format!("Search failed: {e}")),
                    rooms: Vec::new(),
                    messages: None,
                });
                return;
            }
        };
        let found = &v["messages"];
        let mut rooms = Vec::new();
        let mut messages = Vec::new();
        for m in found["matches"].as_array().into_iter().flatten() {
            let Some(channel) = m["channel"]["id"].as_str() else {
                continue;
            };
            if only.as_deref().is_some_and(|c| c != channel) {
                continue;
            }
            let Some(at) = self.room_id(team, channel) else {
                continue;
            };
            if let Some(message) = self.message(team, channel, m, false) {
                rooms.push(at);
                messages.push(message);
            }
        }
        let total = found["paging"]["total"].as_u64().or_else(|| found["total"].as_u64());
        let next = offset + crate::api::SEARCH_PAGE;
        let next_offset = (next < usize::try_from(total.unwrap_or(0)).unwrap_or(usize::MAX)).then_some(next);
        let everywhere = query.scope == SearchScope::Server && query.in_room.is_none();
        out.push(Event::SearchResults {
            room,
            query,
            offset,
            next_offset,
            // A DM's matches are picked out of the workspace's: its count isn't Slack's.
            total: total.filter(|_| only.is_none()),
            notice: None,
            rooms: if everywhere { rooms } else { Vec::new() },
            messages: Some(messages),
        });
    }
}
