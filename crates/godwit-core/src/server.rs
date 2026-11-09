// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Servers, guilds and spaces: groups of rooms with an order of their own.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::ids::{RoomId, ServerId};

/// A named group of a server's rooms, in the server's order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    /// Empty for rooms outside any category (listed first).
    pub title: Arc<str>,
    pub rooms: Vec<RoomId>,
    /// The network's id for a real category (None for the headingless
    /// group and for networks that can't manage categories). It lets the
    /// UI aim a rename, delete, move or "create channel here" at it.
    #[serde(default)]
    pub id: Option<Arc<str>>,
    /// This account may rename and delete the category.
    #[serde(default)]
    pub can_manage: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Server {
    pub id: ServerId,
    pub name: Arc<str>,
    pub icon_url: Option<Arc<str>>,
    /// Its banner, shown above its channel list (Stoat's server banner).
    #[serde(default)]
    pub banner_url: Option<Arc<str>>,
    pub categories: Vec<Category>,
    /// The user muted it: unread rooms don't light it up (mentions still do).
    pub muted: bool,
    /// What the server's menu may offer us here.
    #[serde(default)]
    pub rights: ServerRights,
    /// The folder the network's own list puts it in (Discord's server
    /// folders); None for a plain server.
    #[serde(default)]
    pub folder: Option<Arc<ServerFolder>>,
    /// This server can list scheduled events and change the current user's interest.
    #[serde(default)]
    pub scheduled_events: bool,
    /// Known scheduled and active events for the Events entry's badge.
    #[serde(default)]
    pub scheduled_event_count: u32,
}

/// A scheduled or active event hosted by a server, independent of its network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerEvent {
    pub id: Arc<str>,
    pub server: ServerId,
    pub name: Arc<str>,
    pub description: Option<Arc<str>>,
    /// Unix time in seconds.
    pub starts_at: i64,
    /// Unix time in seconds, when the event has an end time.
    pub ends_at: Option<i64>,
    /// A room name or an external location, prepared by the network adapter.
    pub location: Option<Arc<str>>,
    pub status: Arc<str>,
    pub interested_count: u32,
    pub interested: bool,
    pub cover_url: Option<Arc<str>>,
}

/// A folder of servers on the rail. Servers name it by sharing its `id`;
/// opening and closing it is local to the app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerFolder {
    /// Stable within the account.
    pub id: Arc<str>,
    /// Its own name, if the user gave one (else the app joins the servers').
    pub name: Option<Arc<str>>,
    /// Its colour as 0xRRGGBB.
    pub colour: Option<u32>,
}

/// A rail entry: a server by itself, or a folder with its servers.
#[derive(Debug, PartialEq, Eq)]
pub enum RailGroup<T> {
    Single(T),
    Folder { folder: Arc<ServerFolder>, members: Vec<T> },
}

/// Groups servers (in the order given) into rail entries. A folder sits where
/// its first server is and gathers its other servers there; a folder with a
/// single server is just that server (as the official client's lone guild).
pub fn group_by_folder<T>(items: Vec<T>, folder_of: impl Fn(&T) -> Option<Arc<ServerFolder>>) -> Vec<RailGroup<T>> {
    // (its folder, its servers); a plain server is a group of one with none.
    let mut groups: Vec<(Option<Arc<ServerFolder>>, Vec<T>)> = Vec::new();
    for item in items {
        let folder = folder_of(&item);
        let existing = folder.as_ref().and_then(|f| {
            groups
                .iter()
                .position(|(g, _)| g.as_ref().is_some_and(|g| g.id == f.id))
        });
        match existing {
            Some(i) => groups[i].1.push(item),
            None => groups.push((folder, vec![item])),
        }
    }
    groups
        .into_iter()
        .flat_map(|(folder, mut members)| match folder {
            Some(folder) if members.len() > 1 => vec![RailGroup::Folder { folder, members }],
            _ => members.drain(..).map(RailGroup::Single).collect(),
        })
        .collect()
}

#[cfg(test)]
mod folder_tests {
    use super::*;

    fn f(id: &str) -> Option<Arc<ServerFolder>> {
        Some(Arc::new(ServerFolder {
            id: id.into(),
            name: None,
            colour: Some(0xff0000),
        }))
    }

    #[test]
    fn folders_gather_at_their_first_server_and_lone_ones_unwrap() {
        let items = vec![("a", f("x")), ("b", None), ("c", f("x")), ("d", f("y"))];
        let groups = group_by_folder(items, |i| i.1.clone());
        assert_eq!(groups.len(), 3);
        assert!(
            matches!(&groups[0], RailGroup::Folder { members, .. } if members.iter().map(|m| m.0).collect::<Vec<_>>() == ["a", "c"])
        );
        assert!(matches!(&groups[1], RailGroup::Single(("b", _))));
        assert!(matches!(&groups[2], RailGroup::Single(("d", _))));
    }
}

/// Our standing in a server, for its menu (the official ServerContextMenu
/// shows each entry only with its permission).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRights {
    pub invite: bool,
    pub create_channels: bool,
    /// Categories can be created here too (and, per category, `can_manage`).
    #[serde(default)]
    pub manage_categories: bool,
    /// Channels and categories can be moved up and down.
    #[serde(default)]
    pub reorder_channels: bool,
    /// Any settings page is open to us.
    pub settings: bool,
    /// The owner can't leave, only delete it.
    pub owner: bool,
    /// The network keeps a notification level per server, changeable here.
    #[serde(default)]
    pub notify_levels: bool,
    /// The level now: 0 all messages, 1 only @mentions, 2 nothing.
    #[serde(default)]
    pub notify: u8,
    /// The network takes reports about this server.
    #[serde(default)]
    pub report: bool,
}

/// A room of a server we haven't joined, from its directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryRoom {
    pub id: RoomId,
    pub name: Arc<str>,
    pub topic: Option<Arc<str>>,
    pub members: u64,
}

impl Server {
    pub fn rooms(&self) -> impl Iterator<Item = &RoomId> {
        self.categories.iter().flat_map(|c| c.rooms.iter())
    }
}
