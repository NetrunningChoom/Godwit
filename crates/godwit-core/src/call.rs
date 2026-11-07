// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Voice calls: the one we're in, and who is in each room's call.

use std::sync::Arc;

use crate::ids::{RoomId, UserId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallStatus {
    Connecting,
    Connected,
}

/// Someone in a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallMember {
    pub user: UserId,
    pub name: Arc<str>,
    pub avatar_url: Option<Arc<str>>,
    pub speaking: bool,
    pub muted: bool,
    /// Hears nothing (deafened themselves).
    pub deafened: bool,
}

/// The call we're in (one at a time, across every account).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub room: RoomId,
    pub status: CallStatus,
    /// Latest voice transport round trip (ms), when the network exposes it.
    pub latency_ms: Option<u16>,
    pub members: Vec<CallMember>,
    pub muted: bool,
    pub deafened: bool,
    pub camera: bool,
    pub screen_share: bool,
    pub videos: Vec<CallVideo>,
    /// Who shares their screen in the call for others to watch (Discord's
    /// Go Live), and whether we do. Networks that send every screen share
    /// to everyone (LiveKit) leave this empty and list them in `videos`.
    pub streams: Vec<CallStream>,
}

/// Someone sharing their screen in our call, which we may watch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallStream {
    pub user: UserId,
    pub watching: bool,
    /// Number of other call members watching this stream, when the network reports it.
    pub viewers: Option<u32>,
    /// Why watching it stopped or failed, shown beside it until it's
    /// watched again ("No video arrived.", "The stream ended.").
    pub note: Option<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallVideo {
    pub sid: String,
    pub user: UserId,
    pub screen_share: bool,
}

/// Common screen-share presets; adapters apply the network's allowed limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenShareQuality {
    Low,
    High,
    Text,
}
