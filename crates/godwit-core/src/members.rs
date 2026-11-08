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
    /// The network takes reports about this person (never about us, nor
    /// about the network's own staff).
    #[serde(default)]
    pub can_report: bool,
}

/// What a profile card may do to a group member, and where.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupModeration {
    pub room: crate::ids::RoomId,
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
