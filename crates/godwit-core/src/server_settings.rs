// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Server settings: the editable overview and the admin pages (roles,
//! invites, bans), kept only while the settings are open.
use crate::ids::{RoomId, ServerId, UserId};
use crate::own_profile::ProfileImage;
use std::sync::Arc;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerOverview {
    pub name: Arc<str>,
    pub description: Arc<str>,
    pub icon_url: Option<Arc<str>>,
    pub banner_url: Option<Arc<str>>,
    pub can_manage: bool,
    /// Which other pages this account may use (the official settings hide
    /// the rest): Roles needs Manage Roles, Bans needs Ban Members, deleting
    /// is the owner's.
    pub can_manage_roles: bool,
    pub can_ban: bool,
    pub owner: bool,
    pub channels: Vec<(RoomId, Arc<str>)>,
    /// Joined, left, kicked, banned, in the official overview's order.
    pub system_channels: [Option<Arc<str>>; 4],
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerOverviewEdit {
    pub name: Option<Arc<str>>,
    pub description: Option<Arc<str>>,
    pub icon: ProfileImage,
    pub banner: ProfileImage,
    /// None preserves the configuration; Some replaces all four assignments.
    pub system_channels: Option<[Option<Arc<str>>; 4]>,
}
impl ServerOverviewEdit {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.description.is_none()
            && self.icon == ProfileImage::Keep
            && self.banner == ProfileImage::Keep
            && self.system_channels.is_none()
    }
}

/// The settings' pages, in the official sidebar's order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ServerPage {
    #[default]
    Overview,
    Roles,
    Invites,
    Bans,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerInvite {
    pub code: Arc<str>,
    /// The shareable link, in the network's own form.
    pub link: Arc<str>,
    pub creator: Arc<str>,
    pub creator_avatar: Option<Arc<str>>,
    pub channel: Arc<str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerBan {
    pub user: UserId,
    pub name: Arc<str>,
    pub avatar_url: Option<Arc<str>>,
    pub reason: Arc<str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerRole {
    pub id: Arc<str>,
    pub name: Arc<str>,
    /// `#rrggbb`, or None for no colour.
    pub colour: Option<Arc<str>>,
    pub hoist: bool,
    pub allow: u64,
    pub deny: u64,
}

/// One permission as the editor lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionInfo {
    pub bit: u64,
    pub heading: Option<Arc<str>>,
    pub title: Arc<str>,
    pub description: Arc<str>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerRoles {
    /// Most important first.
    pub roles: Vec<ServerRole>,
    /// What everyone has without a role.
    pub default_allow: u64,
    pub permissions: Arc<[PermissionInfo]>,
}

/// A change to a role; None leaves that part as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleEdit {
    pub id: Arc<str>,
    pub name: Option<Arc<str>>,
    /// Some(None) removes the colour.
    pub colour: Option<Option<Arc<str>>>,
    pub hoist: Option<bool>,
    /// (allow, deny).
    pub permissions: Option<(u64, u64)>,
}

/// Something server settings or a member's card asks the network to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerAdmin {
    LoadInvites,
    DeleteInvite(Arc<str>),
    LoadBans,
    Unban(UserId),
    LoadRoles,
    CreateRole(Arc<str>),
    EditRole(RoleEdit),
    DeleteRole(Arc<str>),
    SetDefaultPermissions(u64),
    Kick(UserId),
    Ban {
        user: UserId,
        reason: Arc<str>,
    },
    DeleteServer,
    /// The server menu's entries.
    CreateInvite,
    MarkRead,
    /// The server's notification level (see [`crate::server::ServerRights::notify`]).
    SetNotify(u8),
    Leave,
    CreateChannel {
        name: Arc<str>,
        voice: bool,
    },
}

impl ServerAdmin {
    /// Loads only read; everything else changes the server.
    pub fn is_load(&self) -> bool {
        matches!(self, Self::LoadInvites | Self::LoadBans | Self::LoadRoles)
    }
}

/// What the network answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerAdminResult {
    Invites(Vec<ServerInvite>),
    Bans(Vec<ServerBan>),
    Roles(ServerRoles),
    /// A change went through: what to tell the user.
    Done(Arc<str>),
    /// The server menu's Invite: the link to share.
    InviteCreated(Arc<str>),
}

#[derive(Clone, Debug)]
pub struct ServerSettingsState {
    pub server: ServerId,
    pub request: u64,
    pub overview: Option<ServerOverview>,
    pub busy: bool,
    pub saved: bool,
    pub error: Option<Arc<str>>,
    pub page: ServerPage,
    pub invites: Option<Vec<ServerInvite>>,
    pub bans: Option<Vec<ServerBan>>,
    pub roles: Option<ServerRoles>,
    /// A change is on its way; the page's buttons wait for it.
    pub admin_busy: bool,
    /// The last change's outcome: Ok is a confirmation, Err a failure.
    pub notice: Option<Result<Arc<str>, Arc<str>>>,
}

impl ServerSettingsState {
    pub fn new(server: ServerId, request: u64) -> Self {
        Self {
            server,
            request,
            overview: None,
            busy: true,
            saved: false,
            error: None,
            page: ServerPage::Overview,
            invites: None,
            bans: None,
            roles: None,
            admin_busy: false,
            notice: None,
        }
    }
}
