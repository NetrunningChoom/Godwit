// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Streamed member lists, shared by Discord and Fluxer: GUILD_MEMBER_LIST_UPDATE
//! keeps each list's rows (group headers and members, as the server numbers
//! them), the client asks for the ranges around what's on screen, and the
//! rows become the member panel's groups.
//!
//! Each update names its list (`id`): on Discord "everyone" or a hash of who
//! may see the channel (`Store::member_list_id`), on Fluxer the channel's id.
//! An update only touches the list it names. The list is as long as its
//! groups' counts say; rows not loaded yet stay `Null` and show as blank
//! placeholders, so scrolling to them knows which range to ask for, as the
//! official clients do.

use std::collections::HashMap;

use godwit_core::ids::{AccountId, UserId};
use godwit_core::members::{Member, MemberGroup, Presence};
use serde_json::Value;

use crate::store::Store;

/// Rows a range holds, as both servers count them.
pub const RANGE: usize = 100;
/// The most ranges asked for at once: the first, and two around the view
/// (Discord's client asks for three; Fluxer allows ten).
const RANGES: usize = 3;

/// Every list streamed, by guild and list id.
#[derive(Default)]
pub struct MemberLists {
    lists: HashMap<(String, String), Vec<Value>>,
    sized: std::collections::HashSet<(String, String)>,
}

impl MemberLists {
    /// Applies an update to the list it names (`fallback` when it names
    /// none): its length from its groups, then its ops. Returns the list's
    /// key, or None if the update is for no list.
    pub fn apply(&mut self, d: &Value, fallback: Option<String>) -> Option<(String, String)> {
        let guild = d["guild_id"].as_str()?.to_owned();
        let id = d["id"].as_str().map(str::to_owned).or(fallback)?;
        let key = (guild, id);
        let rows = self.lists.entry(key.clone()).or_default();
        // The whole list's length: a header for each group, then its members.
        if let Some(groups) = d["groups"].as_array() {
            let total: usize = groups
                .iter()
                .filter_map(|g| g["count"].as_u64())
                .map(|n| usize::try_from(n).unwrap_or(0) + 1)
                .sum();
            rows.resize(total, Value::Null);
            self.sized.insert(key.clone());
        }
        for op in d["ops"].as_array().into_iter().flatten() {
            apply_op(rows, op, self.sized.contains(&key));
        }
        Some(key)
    }

    pub fn rows(&self, guild: &str, id: &str) -> Option<&[Value]> {
        self.lists.get(&(guild.to_owned(), id.to_owned())).map(Vec::as_slice)
    }

    /// Forgets `guild`'s lists but `keep`.
    pub fn keep_only(&mut self, guild: &str, keep: &str) {
        self.lists.retain(|(g, l), _| g != guild || l == keep);
        self.sized.retain(|(g, l)| g != guild || l == keep);
    }

    /// Every member row, for the store (names, roles, presence).
    pub fn members<'a>(&'a self, key: &(String, String)) -> impl Iterator<Item = &'a Value> + 'a {
        self.lists
            .get(key)
            .into_iter()
            .flatten()
            .map(|row| &row["member"])
            .filter(|m| m.is_object())
    }
}

fn apply_op(rows: &mut Vec<Value>, op: &Value, sized: bool) {
    let index = op["index"].as_u64().and_then(|i| usize::try_from(i).ok());
    let at = |n: usize| {
        op["range"][n]
            .as_u64()
            .and_then(|i| usize::try_from(i).ok())
            .unwrap_or(0)
    };
    match op["op"].as_str() {
        // The rows of its range, in place; a short range means the list
        // ends within it.
        Some("SYNC") => {
            let (start, end) = (at(0), at(1));
            let items = op["items"].as_array().cloned().unwrap_or_default();
            if rows.len() < start {
                rows.resize(start, Value::Null);
            }
            let size = end.saturating_sub(start).saturating_add(1);
            let count = items.len();
            let needed = start.saturating_add(count);
            if rows.len() < needed {
                rows.resize(needed, Value::Null);
            }
            let clear_end = rows.len().min(end.saturating_add(1));
            for row in rows.iter_mut().take(clear_end).skip(start) {
                *row = Value::Null;
            }
            for (index, item) in items.into_iter().enumerate() {
                rows[start + index] = item;
            }
            // Group counts size a sparse list. A partial SYNC fills its range
            // without removing the placeholders belonging to later ranges.
            if !sized && count < size {
                rows.truncate(needed);
            }
        }
        Some("INSERT") => {
            if let Some(i) = index.filter(|&i| i <= rows.len()) {
                rows.insert(i, op["item"].clone());
            }
        }
        Some("UPDATE") => {
            if let Some(row) = index.and_then(|i| rows.get_mut(i)) {
                *row = op["item"].clone();
            }
        }
        Some("DELETE") => {
            if let Some(i) = index.filter(|&i| i < rows.len()) {
                rows.remove(i);
            }
        }
        Some("INVALIDATE") => {
            let end = (at(1) + 1).min(rows.len());
            let start = at(0);
            if start < end {
                rows.splice(start..end, std::iter::repeat_n(Value::Null, end - start));
            }
        }
        _ => {}
    }
}

/// The ranges to subscribe to with rows `first` to `last` on screen: the
/// first range always, then the ones the view touches and the next.
pub fn ranges(first: usize, last: usize) -> Vec<[usize; 2]> {
    let (a, b) = (first / RANGE, last.max(first) / RANGE + 1);
    let mut chunks: Vec<usize> = std::iter::once(0).chain(a..=b).collect();
    chunks.dedup();
    chunks.sort_unstable();
    chunks.dedup();
    // The first, then the ones nearest the view.
    let mut kept: Vec<usize> = vec![0];
    kept.extend(chunks.into_iter().filter(|&c| c != 0).take(RANGES - 1));
    kept.into_iter().map(|c| [c * RANGE, c * RANGE + RANGE - 1]).collect()
}

/// A row not loaded yet: shown blank (its user id is empty).
pub fn placeholder(account: &AccountId) -> Member {
    Member {
        user: UserId::new(account.clone(), ""),
        name: "".into(),
        avatar_url: None,
        colour: None,
        presence: Presence::Unknown,
        status: None,
    }
}

/// The rows as the member panel's groups: a group per header, its members
/// (blank placeholders where not loaded), names and colours from the store.
pub fn groups(
    rows: &[Value],
    store: &Store,
    guild: &str,
    account: &AccountId,
    avatar: impl Fn(&str) -> String,
    presence: impl Fn(&str) -> Presence,
) -> Vec<MemberGroup> {
    let mut groups: Vec<MemberGroup> = Vec::new();
    let push = |groups: &mut Vec<MemberGroup>, member: Member| match groups.last_mut() {
        Some(g) => g.members.push(member),
        None => groups.push(MemberGroup {
            title: "Members".into(),
            members: vec![member],
        }),
    };
    for row in rows {
        if let Some(group) = row.get("group") {
            let id = group["id"].as_str().unwrap_or_default();
            let name = match id {
                "online" => "Online".to_owned(),
                "offline" => "Offline".to_owned(),
                role => store
                    .guilds
                    .get(guild)
                    .and_then(|g| g.roles.get(role))
                    .map_or_else(|| "Members".into(), |r| r.name.clone()),
            };
            let count = group["count"].as_u64().unwrap_or_default();
            groups.push(MemberGroup {
                title: format!("{name} — {count}").into(),
                members: Vec::new(),
            });
        } else if let Some(user) = row["member"]["user"]["id"].as_str() {
            let member = Member {
                user: UserId::new(account.clone(), user),
                name: store.display_name(user, Some(guild)).into(),
                avatar_url: Some(avatar(user).into()),
                colour: store.colour(user, guild),
                presence: presence(user),
                status: row["member"]["presence"]["activities"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|a| a["type"] == 4)
                    .and_then(|a| a["state"].as_str())
                    .map(Into::into),
            };
            push(&mut groups, member);
        } else {
            push(&mut groups, placeholder(account));
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ranges_cover_the_view_and_the_next() {
        assert_eq!(ranges(0, 30), [[0, 99], [100, 199]]);
        assert_eq!(ranges(150, 180), [[0, 99], [100, 199], [200, 299]]);
        assert_eq!(ranges(450, 520), [[0, 99], [400, 499], [500, 599]]);
    }

    #[test]
    fn groups_size_the_list_and_updates_touch_only_their_own() {
        let mut lists = MemberLists::default();
        let member = |id: &str| json!({"member": {"user": {"id": id}}});
        let key = lists
            .apply(
                &json!({"guild_id": "1", "id": "everyone",
                    "groups": [{"id": "online", "count": 2}, {"id": "offline", "count": 150}],
                    "ops": [{"op": "SYNC", "range": [0, 99],
                        "items": [{"group": {"id": "online", "count": 2}}, member("a"), member("b")]}]}),
                None,
            )
            .unwrap();
        // 2 headers + 152 members, the rest placeholders until asked for.
        assert_eq!(lists.rows("1", "everyone").unwrap().len(), 154);
        assert_eq!(lists.members(&key).count(), 2);
        // Another list of the same guild doesn't touch this one.
        lists.apply(
            &json!({"guild_id": "1", "id": "123", "ops": [{"op": "DELETE", "index": 1}]}),
            None,
        );
        assert_eq!(lists.members(&key).count(), 2);
        // A later range fills its rows in place.
        lists.apply(
            &json!({"guild_id": "1", "id": "everyone",
                "ops": [{"op": "SYNC", "range": [100, 199], "items": [member("c")]}]}),
            None,
        );
        let rows = lists.rows("1", "everyone").unwrap();
        assert_eq!(rows.len(), 154, "sized by the groups, not cut at the short range");
        assert_eq!(rows[100]["member"]["user"]["id"], "c");
        assert_eq!(rows[1]["member"]["user"]["id"], "a");
    }
}
