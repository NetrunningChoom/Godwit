// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Network-neutral snapshots for the encrypted local cache. These contain
//! only stable conversation data; transient UI and connection state stays out.

use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, RoomId, UserId};
use crate::members::Profile;
use crate::message::Message;
use crate::room::Room;
use crate::server::Server;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSnapshot {
    pub account: AccountId,
    pub rooms: Vec<Room>,
    pub servers: Vec<Server>,
    pub timelines: Vec<CachedTimeline>,
    pub profiles: Vec<Profile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedTimeline {
    pub room: RoomId,
    pub messages: Vec<Message>,
    pub reached_start: bool,
}

impl AccountSnapshot {
    pub fn for_account(mut self, account: &AccountId) -> Self {
        self.account = account.clone();
        self.rooms.retain(|room| &room.id.account == account);
        self.servers.retain(|server| &server.id.account == account);
        self.timelines.retain(|timeline| &timeline.room.account == account);
        self.profiles.retain(|profile| &profile.user.account == account);
        self
    }

    pub fn belongs_to(&self, account: &AccountId) -> bool {
        &self.account == account
    }

    pub fn profile(&self, user: &UserId) -> Option<&Profile> {
        self.profiles.iter().find(|profile| &profile.user == user)
    }
}
