// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A server channel's settings (overview, mature flag, deleting it), kept
//! only while the page is open.
use crate::ids::RoomId;
use crate::own_profile::ProfileImage;
use std::sync::Arc;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChannelOverview {
    pub name: Arc<str>,
    pub description: Arc<str>,
    pub icon_url: Option<Arc<str>>,
    /// Age-restricted: people confirm their age before opening it.
    pub mature: bool,
    /// Whether this account may change it or delete it.
    pub can_manage: bool,
    /// A category: only its name (and deleting it) apply.
    pub is_category: bool,
    /// The network gives channels an icon (the picture editor shows).
    pub icons: bool,
    /// Seconds a person waits between messages (slowmode), or None where
    /// the network has no such setting (the field stays hidden).
    pub slowmode: Option<u32>,
    /// The longest description in characters (0: not limited here).
    pub description_max: u32,
}

/// The slowmode choices as the official Discord client lists them
/// (rate_limit_per_user: off, 5s, 10s, 15s, 30s, 1m, 2m, 5m, 10m, 15m, 30m,
/// 1h, 2h, 6h; docs.discord.food/resources/channel, max 21600).
pub const SLOWMODE_CHOICES: [(&str, u32); 14] = [
    ("Off", 0),
    ("5 seconds", 5),
    ("10 seconds", 10),
    ("15 seconds", 15),
    ("30 seconds", 30),
    ("1 minute", 60),
    ("2 minutes", 120),
    ("5 minutes", 300),
    ("10 minutes", 600),
    ("15 minutes", 900),
    ("30 minutes", 1_800),
    ("1 hour", 3_600),
    ("2 hours", 7_200),
    ("6 hours", 21_600),
];

/// A change to a channel; None / Keep leaves that part as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChannelOverviewEdit {
    pub name: Option<Arc<str>>,
    pub description: Option<Arc<str>>,
    pub icon: ProfileImage,
    pub mature: Option<bool>,
    pub slowmode: Option<u32>,
}

impl ChannelOverviewEdit {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.description.is_none()
            && self.icon == ProfileImage::Keep
            && self.mature.is_none()
            && self.slowmode.is_none()
    }
}

#[derive(Clone, Debug)]
pub struct ChannelSettingsState {
    pub room: RoomId,
    pub request: u64,
    pub overview: Option<ChannelOverview>,
    /// A load, save or delete is on its way.
    pub busy: bool,
    pub saved: bool,
    pub error: Option<Arc<str>>,
}

impl ChannelSettingsState {
    pub fn new(room: RoomId, request: u64) -> Self {
        Self {
            room,
            request,
            overview: None,
            busy: true,
            saved: false,
            error: None,
        }
    }
}
