// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Who is in a room: its member list, grouped the way the network shows it.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::ids::{ServerId, UserId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Presence {
    Online,
    Idle,
    /// Do-not-disturb but working ("Focus" on Stoat).
    Focus,
    Busy,
    Offline,
    /// Hidden: only ever our own (others who hide just look offline).
    Invisible,
    /// The network doesn't say (Matrix servers often don't share presence):
    /// no dot, rather than a wrong one.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub user: UserId,
    /// Nickname or display name, as shown in this room.
    pub name: Arc<str>,
    pub avatar_url: Option<Arc<str>>,
    /// Their role colour here, if any.
    pub colour: Option<[u8; 3]>,
    pub presence: Presence,
    /// Their custom status text.
    pub status: Option<Arc<str>>,
}

/// A heading ("Moderators — 3") and the members under it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberGroup {
    pub title: Arc<str>,
    pub members: Vec<Member>,
}

/// Where we stand with someone (the official profile's actions follow it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Relationship {
    #[default]
    None,
    Friend,
    /// They asked to be friends.
    Incoming,
    /// We asked.
    Outgoing,
    /// We blocked them.
    Blocked,
    /// They blocked us.
    BlockedOther,
}

/// Someone's profile card, as seen from a room (nickname and roles are
/// per server).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub user: UserId,
    pub name: Arc<str>,
    /// Their name on the network (e.g. `name#1234`).
    pub username: Arc<str>,
    pub avatar_url: Option<Arc<str>>,
    pub banner_url: Option<Arc<str>>,
    pub presence: Presence,
    pub status: Option<Arc<str>>,
    /// Rich text; None while it loads or if they have none.
    pub bio: Option<Arc<str>>,
    /// The network's account level, when its profile exposes one.
    #[serde(default)]
    pub profile_level: Option<u32>,
    /// Account creation date as a display-ready year, when exposed.
    #[serde(default)]
    pub member_since: Option<Arc<str>>,
    /// Their roles in the room's server, highest first, with colours.
    pub roles: Vec<(Arc<str>, Option<[u8; 3]>)>,
    /// It's us.
    pub me: bool,
    #[serde(default)]
    pub relationship: Relationship,
    /// A bot's account.
    #[serde(default)]
    pub bot: bool,
    /// The network has friends (and only messages them, as Stoat): the card
    /// offers friend requests, and Message only to friends.
    #[serde(default)]
    pub has_friends: bool,
    /// Seen in a server where we may kick or ban them (they rank below us).
    #[serde(default)]
    pub moderation: Option<Moderation>,
    /// A group conversation where this member may be removed.
    #[serde(default)]
    pub group_moderation: Option<GroupModeration>,
    /// The card may block and unblock this person (never ourselves).
    #[serde(default)]
    pub can_block: bool,
    /// The profile supports ignoring this person (never ourselves).
    #[serde(default)]
    pub can_ignore: bool,
    /// Their messages are ignored on this network.
    #[serde(default)]
    pub ignored: bool,
    /// The network takes reports about this person (never about us, nor
    /// about the network's own staff).
    #[serde(default)]
    pub can_report: bool,
    /// Servers we both are in (the official profile's Mutual Servers tab).
    #[serde(default)]
    pub mutual_servers: Vec<MutualServer>,
    /// Friends we have in common (Mutual Friends tab).
    #[serde(default)]
    pub mutual_friends: Vec<MutualFriend>,
    /// Our private note on them; None while it loads or if there is none.
    #[serde(default)]
    pub note: Option<Arc<str>>,
    /// The card has a note field for them (the network keeps private notes,
    /// never ours on ourselves).
    #[serde(default)]
    pub can_note: bool,
    /// The most characters a note may hold (the card's field stops there).
    #[serde(default)]
    pub note_limit: usize,
}

/// A server both we and a profile's owner are in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutualServer {
    pub server: ServerId,
    /// Their nickname there, if they set one.
    pub nick: Option<Arc<str>>,
}

/// A friend we share with a profile's owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutualFriend {
    pub user: UserId,
    pub name: Arc<str>,
    pub avatar_url: Option<Arc<str>>,
}

/// What a profile card may do to a group member, and where.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupModeration {
    pub room: crate::ids::RoomId,
    #[serde(default)]
    pub add_member: bool,
    pub remove_member: bool,
    pub transfer_ownership: bool,
}

/// What a profile card may do to its member, and where.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moderation {
    pub server: ServerId,
    pub kick: bool,
    pub ban: bool,
    /// The ban dialog offers deleting message history.
    #[serde(default)]
    pub ban_history: bool,
    /// The roles we may toggle on them (already limited to what the
    /// network allows us to hand out); empty when we may not.
    #[serde(default)]
    pub roles: Vec<ModerationRole>,
    /// We may time them out (or lift it).
    #[serde(default)]
    pub timeout: bool,
    /// They are timed out right now, so the card offers lifting it.
    #[serde(default)]
    pub timed_out: bool,
    /// We may change their nickname in this server (our own, for ourselves).
    #[serde(default)]
    pub nickname: bool,
    /// Their current nickname in this server.
    #[serde(default)]
    pub nick: Option<std::sync::Arc<str>>,
}

impl Moderation {
    /// No powers over the member yet; the network switches on what we hold.
    pub fn new(server: ServerId) -> Self {
        Self {
            server,
            kick: false,
            ban: false,
            ban_history: false,
            roles: Vec::new(),
            timeout: false,
            timed_out: false,
            nickname: false,
            nick: None,
        }
    }

    /// Whether the card has anything to offer at all.
    pub fn any(&self) -> bool {
        self.kick || self.ban || !self.roles.is_empty() || self.timeout || self.nickname
    }
}

/// One role in the member's "Roles" checklist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModerationRole {
    pub id: std::sync::Arc<str>,
    pub name: std::sync::Arc<str>,
    pub colour: Option<[u8; 3]>,
    pub assigned: bool,
}

/// The heading of members found by a network search that the list doesn't show.
pub const FOUND_HEADING: &str = "Search results";

/// Whether `name` matches what was typed: a case-insensitive substring, as the
/// official clients filter what they hold (a network's own search matches
/// prefixes, which this includes).
fn name_matches(name: &str, needle: &str) -> bool {
    name.to_lowercase().contains(needle)
}

/// A heading like "Online — 3" with its count replaced (others are kept).
fn recount(title: &str, n: usize) -> Arc<str> {
    match title.rsplit_once(" — ") {
        Some((name, count)) if count.chars().all(|c| c.is_ascii_digit()) && !count.is_empty() => {
            format!("{name} — {n}").into()
        }
        _ => title.into(),
    }
}

/// The member list narrowed to `query`: the loaded members whose names match,
/// in their groups, then (under [`FOUND_HEADING`]) members the network found
/// that aren't loaded. An empty query leaves the list as it is. Shared by
/// every network, so the member list's search box and mention completion
/// merge local and remote matches the same way.
pub fn search_groups(groups: &[MemberGroup], found: &[Member], query: &str) -> Vec<MemberGroup> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return groups.to_vec();
    }
    let mut out: Vec<MemberGroup> = groups
        .iter()
        .filter_map(|g| {
            let members: Vec<Member> = g
                .members
                .iter()
                .filter(|m| name_matches(&m.name, &needle))
                .cloned()
                .collect();
            (!members.is_empty()).then(|| MemberGroup {
                title: recount(&g.title, members.len()),
                members,
            })
        })
        .collect();
    let new: Vec<Member> = found
        .iter()
        .filter(|m| !out.iter().any(|g| g.members.iter().any(|l| l.user == m.user)))
        .cloned()
        .collect();
    if !new.is_empty() {
        out.push(MemberGroup {
            title: FOUND_HEADING.into(),
            members: new,
        });
    }
    out
}

#[cfg(test)]
mod search_tests {
    use super::*;

    fn member(id: &str, name: &str) -> Member {
        Member {
            user: UserId::new(crate::ids::AccountId::new(crate::network::Network::Discord, "me"), id),
            name: name.into(),
            avatar_url: None,
            colour: None,
            presence: Presence::Offline,
            status: None,
        }
    }

    #[test]
    fn local_matches_keep_their_groups_and_remote_ones_follow_without_repeats() {
        let groups = vec![
            MemberGroup {
                title: "Mods".into(),
                members: vec![member("1", "Alice"), member("2", "Bob")],
            },
            MemberGroup {
                title: "Online — 9".into(),
                members: vec![member("3", "Malice")],
            },
        ];
        let found = vec![member("3", "Malice"), member("4", "alicia")];
        let out = search_groups(&groups, &found, " ALIC ");
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].members.len(), 1);
        assert_eq!(&*out[1].title, "Online — 1", "counts follow the matches");
        assert_eq!(&*out[2].title, FOUND_HEADING);
        assert_eq!(&*out[2].members[0].name, "alicia");
        assert_eq!(search_groups(&groups, &found, "  "), groups);
    }
}
