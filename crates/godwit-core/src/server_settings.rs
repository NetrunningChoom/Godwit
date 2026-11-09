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
    /// Whether the Emoji page is offered (Stoat: Manage Customisation).
    pub can_manage_emoji: bool,
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
    Emoji,
    Invites,
    Bans,
    ModerationLog,
}

/// A network-neutral entry in a server's moderation log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModerationLogEntry {
    pub id: Arc<str>,
    /// Unix milliseconds, derived from the network's entry id.
    pub time_ms: i64,
    pub actor: Arc<str>,
    pub actor_avatar_url: Option<Arc<str>>,
    pub action: ModerationLogAction,
    /// Network-prepared text such as "banned Alex".
    pub text: Arc<str>,
    pub target: Option<Arc<str>>,
    pub reason: Option<Arc<str>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModerationLogAction {
    Member,
    Role,
    Channel,
    Message,
    Other,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModerationLog {
    pub entries: Vec<ModerationLogEntry>,
    pub loading: bool,
    pub has_more: bool,
    pub error: Option<Arc<str>>,
}

/// How a network spells a custom emoji's name; the pure check lives here so
/// every network's emoji manager reports the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmojiRules {
    pub min_len: usize,
    pub max_len: usize,
    /// Letters may be upper case (Discord); Stoat takes `[a-z0-9_]` only.
    pub uppercase: bool,
}

impl Default for EmojiRules {
    /// Stoat's: `[a-z0-9_]{1,32}` (for-web EmojiList.tsx).
    fn default() -> Self {
        Self {
            min_len: 1,
            max_len: 32,
            uppercase: false,
        }
    }
}

impl EmojiRules {
    /// Why `name` is not allowed, or Ok.
    pub fn validate_name(&self, name: &str) -> Result<(), String> {
        let count = name.chars().count();
        if count < self.min_len || count > self.max_len {
            return Err(format!(
                "An emoji name is {} to {} characters.",
                self.min_len, self.max_len
            ));
        }
        let ok = |c: char| {
            c.is_ascii_digit() || c == '_' || c.is_ascii_lowercase() || (self.uppercase && c.is_ascii_uppercase())
        };
        if !name.chars().all(ok) {
            return Err(if self.uppercase {
                "Use letters, numbers and underscores only.".into()
            } else {
                "Use lowercase letters, numbers and underscores only.".into()
            });
        }
        Ok(())
    }
}

/// An image picked to become an emoji.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmojiFile {
    pub path: std::path::PathBuf,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerEmoji {
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub url: Option<Arc<str>>,
    /// Who added it, as a display name (may be empty).
    pub creator: Arc<str>,
}

/// A server's custom emoji and the limits that apply to adding more.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerEmojis {
    /// Newest first, as the official list.
    pub emoji: Vec<ServerEmoji>,
    /// Most emoji a server may have, when the network says.
    pub limit: Option<usize>,
    /// Largest upload, in bytes, when the network says.
    pub max_file_bytes: Option<u64>,
    pub rules: EmojiRules,
}

impl ServerEmojis {
    pub fn slots_left(&self) -> Option<usize> {
        self.limit.map(|l| l.saturating_sub(self.emoji.len()))
    }
    /// What stops adding `name` with `file`, checked before anything is sent.
    pub fn check_new(&self, name: &str, file_size: u64) -> Result<(), String> {
        self.rules.validate_name(name)?;
        if self.slots_left() == Some(0) {
            return Err("This server has no emoji slots left.".into());
        }
        if file_size == 0 || self.max_file_bytes.is_some_and(|m| file_size > m) {
            return Err("The image is empty or larger than this network allows for emoji.".into());
        }
        Ok(())
    }
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
    /// This network lets managers change role priority.
    pub can_reorder: bool,
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
    /// Replaces role priority, most important first.
    SetRoleRanks(Vec<Arc<str>>),
    DeleteRole(Arc<str>),
    SetDefaultPermissions(u64),
    LoadEmoji,
    CreateEmoji {
        name: Arc<str>,
        file: EmojiFile,
    },
    RenameEmoji {
        id: Arc<str>,
        name: Arc<str>,
    },
    DeleteEmoji(Arc<str>),
    Kick(UserId),
    Ban {
        user: UserId,
        reason: Arc<str>,
        /// How much of their message history goes with them, in seconds
        /// (one of [`BAN_DELETE_CHOICES`]).
        delete_seconds: u32,
    },
    /// Gives or takes a role from a member (the profile card's Roles list).
    SetMemberRole {
        user: UserId,
        role: Arc<str>,
        on: bool,
    },
    /// Times a member out for that long, or lifts it with None.
    Timeout {
        user: UserId,
        duration: Option<std::time::Duration>,
    },
    /// A member's nickname in this server (ours included); None resets it.
    /// Not the local contact nickname of `Action::SetNickname`.
    SetServerNickname {
        user: UserId,
        nick: Option<Arc<str>>,
    },
    /// Server mute, server deafen or disconnect for a member in a call.
    VoiceMember {
        user: UserId,
        op: VoiceMemberOp,
    },
    DeleteServer,
    /// The server menu's entries.
    CreateInvite,
    MarkRead,
    /// The server's notification level (see [`crate::server::ServerRights::notify`]).
    SetNotify(u8),
    Leave,
    /// A new channel (or category) in the server, inside `category` (that
    /// category's id from [`crate::server::Category::id`]) for a channel.
    CreateChannel {
        name: Arc<str>,
        kind: NewChannelKind,
        category: Option<Arc<str>>,
    },
    /// Moves a channel or category (`id`: the room's raw id, or the
    /// category's) one place up or down in the server's list. A channel
    /// at the edge of its category crosses into the neighbouring one.
    MoveChannel {
        id: Arc<str>,
        category: bool,
        up: bool,
    },
}

/// A member card's call control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VoiceMemberOp {
    Mute(bool),
    Deafen(bool),
    /// Takes them out of the call.
    Disconnect,
    /// Moves them to this voice channel (raw id) of the same server.
    Move(std::sync::Arc<str>),
}

/// What the Create channel dialog makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewChannelKind {
    Text,
    Voice,
    /// A group of channels (needs [`crate::server::ServerRights::manage_categories`]).
    Category,
}

/// The ban dialog's "delete message history" choices, as the official
/// client lists them (docs.discord.food/resources/guild, Create Guild Ban:
/// 0 to 604800 seconds).
pub const BAN_DELETE_CHOICES: [(&str, u32); 7] = [
    ("Don't delete any", 0),
    ("Previous hour", 3_600),
    ("Previous 6 hours", 21_600),
    ("Previous 12 hours", 43_200),
    ("Previous 24 hours", 86_400),
    ("Previous 3 days", 259_200),
    ("Previous 7 days", 604_800),
];

/// The member menu's timeout durations, as the official client lists them.
pub const TIMEOUT_CHOICES: [(&str, u64); 6] = [
    ("60 seconds", 60),
    ("5 minutes", 300),
    ("10 minutes", 600),
    ("1 hour", 3_600),
    ("1 day", 86_400),
    ("1 week", 604_800),
];

/// The longest timeout (28 days).
pub const TIMEOUT_MAX_SECS: u64 = 28 * 86_400;

/// The longest server nickname.
pub const NICKNAME_MAX: usize = 32;

impl ServerAdmin {
    /// The member a change on the profile card is aimed at.
    pub fn member_target(&self) -> Option<&UserId> {
        match self {
            Self::SetMemberRole { user, .. }
            | Self::Timeout { user, .. }
            | Self::SetServerNickname { user, .. }
            | Self::VoiceMember { user, .. } => Some(user),
            _ => None,
        }
    }

    /// Loads only read; everything else changes the server.
    pub fn is_load(&self) -> bool {
        matches!(
            self,
            Self::LoadInvites | Self::LoadBans | Self::LoadRoles | Self::LoadEmoji
        )
    }
}

/// What the network answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerAdminResult {
    Invites(Vec<ServerInvite>),
    Bans(Vec<ServerBan>),
    Roles(ServerRoles),
    Emoji(ServerEmojis),
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
    pub emoji: Option<ServerEmojis>,
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
            emoji: None,
            admin_busy: false,
            notice: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stoat_emoji_names_are_lowercase_words_up_to_32() {
        let r = EmojiRules::default();
        assert!(r.validate_name("party_parrot2").is_ok());
        assert!(r.validate_name("").is_err());
        assert!(r.validate_name(&"a".repeat(33)).is_err());
        assert!(r.validate_name(&"a".repeat(32)).is_ok());
        assert!(r.validate_name("Parrot").is_err());
        assert!(r.validate_name("a-b").is_err());
        assert!(r.validate_name("é").is_err());
        let d = EmojiRules {
            min_len: 2,
            max_len: 32,
            uppercase: true,
        };
        assert!(d.validate_name("Parrot").is_ok());
        assert!(d.validate_name("a").is_err());
    }

    #[test]
    fn new_emoji_respect_slots_and_size() {
        let mut e = ServerEmojis {
            limit: Some(1),
            max_file_bytes: Some(100),
            ..Default::default()
        };
        assert!(e.check_new("ok", 50).is_ok());
        assert!(e.check_new("ok", 0).is_err());
        assert!(e.check_new("ok", 101).is_err());
        e.emoji.push(ServerEmoji {
            id: "1".into(),
            name: "x".into(),
            url: None,
            creator: "".into(),
        });
        assert_eq!(e.slots_left(), Some(0));
        assert!(e.check_new("ok", 50).is_err());
    }
}
