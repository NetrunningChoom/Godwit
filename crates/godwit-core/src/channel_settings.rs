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
}

/// A change to a channel; None / Keep leaves that part as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChannelOverviewEdit {
    pub name: Option<Arc<str>>,
    pub description: Option<Arc<str>>,
    pub icon: ProfileImage,
    pub mature: Option<bool>,
}

impl ChannelOverviewEdit {
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.description.is_none() && self.icon == ProfileImage::Keep && self.mature.is_none()
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
