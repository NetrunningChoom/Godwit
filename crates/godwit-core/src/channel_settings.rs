// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A server channel's settings (overview, mature flag, deleting it), kept
//! only while the page is open.
use crate::ids::RoomId;
use crate::own_profile::ProfileImage;
use crate::server_settings::PermissionInfo;
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
    /// A group conversation: only its permissions page is available here.
    pub is_group: bool,
    /// The network gives channels an icon (the picture editor shows).
    pub icons: bool,
    /// Seconds a person waits between messages (slowmode), or None where
    /// the network has no such setting (the field stays hidden).
    pub slowmode: Option<u32>,
    /// The longest description in characters (0: not limited here).
    pub description_max: u32,
    /// Per-role and per-member permission overrides, or None where the
    /// network has none (the Permissions section stays hidden).
    pub overrides: Option<ChannelOverrides>,
}

/// Who an override is for: a role or one member, by the network's id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OverrideTarget {
    pub id: Arc<str>,
    pub member: bool,
}

/// One override as the Permissions page lists it. A permission in neither
/// `allow` nor `deny` is inherited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelOverride {
    pub target: OverrideTarget,
    pub name: Arc<str>,
    /// `#rrggbb`, or None for no colour.
    pub colour: Option<Arc<str>>,
    /// The @everyone entry: always first, and it can't be removed.
    pub everyone: bool,
    pub allow: u64,
    pub deny: u64,
}

/// A role or member an override could be added for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverrideChoice {
    pub target: OverrideTarget,
    pub name: Arc<str>,
}

/// A channel's overrides and what the network lets this account do to them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChannelOverrides {
    /// @everyone first, then the others as the network orders them.
    pub entries: Vec<ChannelOverride>,
    /// The permissions that apply to this kind of channel, in the order
    /// the official client lists them.
    pub permissions: Arc<[PermissionInfo]>,
    /// Roles and members without an override yet.
    pub choices: Vec<OverrideChoice>,
    /// Whether this account may change overrides (Manage Permissions here).
    pub can_edit: bool,
    /// Whether entries can carry explicit denies. Group permissions are a
    /// single allow bitset in Stoat.
    pub supports_deny: bool,
    /// The permission bits this account may allow or deny: it can't grant
    /// what it doesn't have itself (administrators and owners have all).
    pub grantable: u64,
}

impl ChannelOverrides {
    /// Whether setting `target` to (allow, deny) is something this account
    /// may ask for: the bits are listed, don't clash, and every bit that
    /// changes is one it may grant.
    pub fn may_set(&self, target: &OverrideTarget, allow: u64, deny: u64) -> bool {
        let listed = self.permissions.iter().fold(0, |m, p| m | p.bit);
        let (old_allow, old_deny) = self
            .entries
            .iter()
            .find(|e| &e.target == target)
            .map_or((0, 0), |e| (e.allow, e.deny));
        let known =
            self.entries.iter().any(|e| &e.target == target) || self.choices.iter().any(|c| &c.target == target);
        self.can_edit
            && (self.supports_deny || deny == 0)
            && known
            && allow & deny == 0
            && ((allow ^ old_allow) | (deny ^ old_deny)) & !listed == 0
            && ((allow ^ old_allow) | (deny ^ old_deny)) & !self.grantable == 0
    }
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
