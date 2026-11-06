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
}

/// Our standing in a server, for its menu (the official ServerContextMenu
/// shows each entry only with its permission).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRights {
    pub invite: bool,
    pub create_channels: bool,
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
