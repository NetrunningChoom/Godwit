// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Permission calculation, ported from the official SDK
//! (`stoat.js/src/permissions/calculator.ts` and `definitions.ts`). Only used
//! to decide which actions to offer; the server enforces the real rules.

use crate::api::{Channel, Member, Override, Server, User};

pub const MANAGE_CHANNEL: u64 = 1 << 0;
pub const MANAGE_SERVER: u64 = 1 << 1;
pub const MANAGE_PERMISSIONS: u64 = 1 << 2;
pub const MANAGE_ROLE: u64 = 1 << 3;
pub const MANAGE_CUSTOMISATION: u64 = 1 << 4;
pub const KICK_MEMBERS: u64 = 1 << 6;
pub const TIMEOUT_MEMBERS: u64 = 1 << 8;
pub const ASSIGN_ROLES: u64 = 1 << 9;
pub const CHANGE_NICKNAME: u64 = 1 << 10;
pub const MANAGE_NICKNAMES: u64 = 1 << 11;
pub const CHANGE_AVATAR: u64 = 1 << 12;
pub const REMOVE_AVATARS: u64 = 1 << 13;
pub const BAN_MEMBERS: u64 = 1 << 7;
pub const VIEW_CHANNEL: u64 = 1 << 20;
pub const READ_MESSAGE_HISTORY: u64 = 1 << 21;
pub const SEND_MESSAGE: u64 = 1 << 22;
pub const MANAGE_MESSAGES: u64 = 1 << 23;
pub const MANAGE_WEBHOOKS: u64 = 1 << 24;
pub const INVITE_OTHERS: u64 = 1 << 25;
pub const SEND_EMBEDS: u64 = 1 << 26;
pub const UPLOAD_FILES: u64 = 1 << 27;
pub const REACT: u64 = 1 << 29;
pub const CONNECT: u64 = 1 << 30;
pub const SPEAK: u64 = 1 << 31;
pub const VIDEO: u64 = 1 << 32;
pub const LISTEN: u64 = 1 << 36;
pub const USE_EXTERNAL_EMOJIS: u64 = 1 << 41;

/// Where a permission is being edited, for its description.
#[derive(Clone, Copy, PartialEq)]
pub enum EditContext {
    Server,
    TextChannel,
    Group,
}

/// One row of the official permissions editor
/// (`ChannelPermissionsEditor.tsx`): a heading before it, the bit, the title
/// and the description per context (`any` when the context has none).
pub struct Info {
    pub heading: Option<&'static str>,
    pub bit: u64,
    pub title: &'static str,
    server: Option<&'static str>,
    channel: Option<&'static str>,
    group: Option<&'static str>,
    any: Option<&'static str>,
}

impl Info {
    /// The description shown in `ctx`; None hides the row there.
    pub fn description(&self, ctx: EditContext) -> Option<&'static str> {
        match ctx {
            EditContext::Server => self.server,
            EditContext::TextChannel => self.channel,
            EditContext::Group => self.group,
        }
        .or(self.any)
    }
}

const fn info(
    heading: Option<&'static str>,
    shift: u32,
    title: &'static str,
    server: Option<&'static str>,
    channel: Option<&'static str>,
    group: Option<&'static str>,
    any: Option<&'static str>,
) -> Info {
    Info {
        heading,
        bit: 1 << shift,
        title,
        server,
        channel,
        group,
        any,
    }
}

/// The official permissions editor's rows for server roles, for the core.
pub fn server_list() -> std::sync::Arc<[godwit_core::server_settings::PermissionInfo]> {
    LIST.iter()
        .filter_map(|i| {
            Some(godwit_core::server_settings::PermissionInfo {
                bit: i.bit,
                heading: i.heading.map(Into::into),
                title: i.title.into(),
                description: i.description(EditContext::Server)?.into(),
            })
        })
        .collect()
}

/// The official list, in its order.
pub const LIST: &[Info] = &[
    info(
        Some("Admin"),
        0,
        "Manage Channel",
        None,
        None,
        Some("Edit group name and description"),
        Some("Edit and delete channel"),
    ),
    info(
        None,
        1,
        "Manage Server",
        Some("Edit the server's information and settings"),
        None,
        None,
        None,
    ),
    info(
        None,
        2,
        "Manage Permissions",
        Some("Edit any permissions on the server"),
        Some("Edit channel-specific role and default permissions"),
        Some("Whether other users can edit these settings"),
        None,
    ),
    info(
        None,
        3,
        "Manage Roles",
        Some("Create and edit server roles"),
        None,
        None,
        None,
    ),
    info(
        None,
        4,
        "Manage Customisation",
        Some("Create server emoji"),
        None,
        None,
        None,
    ),
    info(
        Some("Members"),
        6,
        "Kick Members",
        Some("Kick lower-ranking members from the server"),
        None,
        None,
        None,
    ),
    info(
        None,
        7,
        "Ban Members",
        Some("Ban lower-ranking members from the server"),
        None,
        None,
        None,
    ),
    info(
        None,
        8,
        "Timeout Members",
        Some("Temporarily prevent lower-ranking members from interacting"),
        None,
        None,
        None,
    ),
    info(
        None,
        9,
        "Assign Roles",
        Some("Assign lower-ranked roles to lower-ranking members"),
        None,
        None,
        None,
    ),
    info(
        None,
        10,
        "Change Nickname",
        Some("Change own nickname"),
        None,
        None,
        None,
    ),
    info(
        None,
        11,
        "Manage Nicknames",
        Some("Change other members' nicknames"),
        None,
        None,
        None,
    ),
    info(None, 12, "Change Avatar", Some("Change own avatar"), None, None, None),
    info(
        None,
        13,
        "Remove Avatars",
        Some("Remove other members' avatars"),
        None,
        None,
        None,
    ),
    info(
        Some("Channels"),
        20,
        "View Channel",
        Some("Able to access channels on this server"),
        Some("Able to access this channel"),
        None,
        None,
    ),
    info(
        None,
        21,
        "Read Message History",
        Some("Read past messages sent in channels"),
        Some("Read past messages sent in channel"),
        None,
        None,
    ),
    info(
        None,
        22,
        "Send Messages",
        Some("Send messages in channels"),
        Some("Send messages in channel"),
        Some("Send messages in channel"),
        None,
    ),
    info(
        None,
        23,
        "Manage Messages",
        Some("Delete and pin messages sent by other members"),
        Some("Delete and pin messages sent by other members"),
        Some("Delete and pin messages sent by other members"),
        None,
    ),
    info(
        None,
        24,
        "Manage Webhooks",
        Some("Create and edit webhooks"),
        Some("Create and edit webhooks"),
        Some("Create and edit webhooks"),
        None,
    ),
    info(
        None,
        25,
        "Invite Others",
        None,
        None,
        Some("Add new members to the group"),
        Some("Create invites for others to use"),
    ),
    info(
        Some("Messaging"),
        26,
        "Send Embeds",
        None,
        None,
        None,
        Some("Send embedded content such as link embeds or custom embeds"),
    ),
    info(
        None,
        27,
        "Upload Files",
        None,
        None,
        None,
        Some("Send attachments to chat"),
    ),
    info(
        None,
        28,
        "Masquerade",
        None,
        None,
        None,
        Some("Allow members to change name and avatar per-message"),
    ),
    info(
        None,
        29,
        "React",
        None,
        None,
        None,
        Some("React to messages with emoji"),
    ),
    info(
        None,
        41,
        "Use External Emojis",
        Some("Use emoji from other servers"),
        Some("Use emoji from other servers"),
        None,
        None,
    ),
    info(
        None,
        39,
        "Bypass Slowmode",
        Some("Bypasses slowmode in channels"),
        Some("Bypasses slowmode in channels"),
        None,
        None,
    ),
    info(
        Some("Voice"),
        30,
        "Connect",
        Some("Connect to voice channel"),
        Some("Connect to voice channel"),
        None,
        None,
    ),
    info(
        None,
        31,
        "Speak",
        Some("Able to speak in voice call"),
        Some("Able to speak in voice call"),
        None,
        None,
    ),
    info(
        None,
        32,
        "Video",
        Some("Share camera or screen in voice call"),
        Some("Share camera or screen in voice call"),
        None,
        None,
    ),
    info(
        None,
        33,
        "Mute Members",
        Some("Mute lower-ranking members in voice call"),
        Some("Mute lower-ranking members in voice call"),
        None,
        None,
    ),
    info(
        None,
        34,
        "Deafen Members",
        Some("Deafen lower-ranking members in voice call"),
        Some("Deafen lower-ranking members in voice call"),
        None,
        None,
    ),
    info(
        None,
        35,
        "Move Members",
        Some("Move members between voice channels"),
        Some("Move members between voice channels"),
        None,
        None,
    ),
    info(
        None,
        36,
        "Listen",
        Some("Hear other people and see their video"),
        Some("Hear other people and see their video"),
        None,
        None,
    ),
    info(
        Some("Mentions"),
        37,
        "Mention Everyone",
        None,
        None,
        None,
        Some("Mention everyone and online members inside the server"),
    ),
    info(
        None,
        38,
        "Mention Roles",
        None,
        None,
        None,
        Some("Mention specific roles"),
    ),
];

pub const GRANT_ALL_SAFE: u64 = 0x000f_ffff_ffff_ffff;
const ALLOW_IN_TIMEOUT: u64 = VIEW_CHANNEL | READ_MESSAGE_HISTORY;
const VIEW_ONLY: u64 = VIEW_CHANNEL | READ_MESSAGE_HISTORY;
const DEFAULT: u64 =
    VIEW_ONLY | SEND_MESSAGE | INVITE_OTHERS | SEND_EMBEDS | UPLOAD_FILES | CONNECT | SPEAK | VIDEO | LISTEN;
pub const DEFAULT_DIRECT_MESSAGE: u64 = DEFAULT | REACT | MANAGE_CHANNEL;

/// What the calculation needs to know about us.
pub struct Me<'a> {
    pub user: Option<&'a User>,
    pub id: &'a str,
    /// Our membership in the channel's server, if loaded.
    pub member: Option<&'a Member>,
    /// Now, for timeouts (milliseconds since the epoch).
    pub now_ms: i64,
}

fn apply(perm: u64, o: &Override) -> u64 {
    (perm | o.a as u64) & !(o.d as u64)
}

/// Our roles, applied in the SDK's order: highest rank number first, so the
/// most important role (lowest rank) gets the last word.
fn ordered_roles<'a>(server: &'a Server, member: &'a Member) -> Vec<&'a str> {
    let mut roles: Vec<(&str, i64)> = member
        .roles
        .iter()
        .filter_map(|id| server.roles.get(id).map(|r| (id.as_str(), r.rank)))
        .collect();
    roles.sort_by_key(|&(_, rank)| std::cmp::Reverse(rank));
    roles.into_iter().map(|(id, _)| id).collect()
}

fn timed_out(member: Option<&Member>, now_ms: i64) -> bool {
    member
        .and_then(|m| m.timeout.as_deref())
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .is_some_and(|t| t.timestamp_millis() > now_ms)
}

pub fn server_permissions(server: &Server, me: &Me) -> u64 {
    if me.user.is_some_and(|u| u.privileged) || server.owner == me.id {
        return GRANT_ALL_SAFE;
    }
    let mut perm = server.default_permissions as u64;
    if let Some(member) = me.member {
        for id in ordered_roles(server, member) {
            if let Some(o) = &server.roles[id].permissions {
                perm = apply(perm, o);
            }
        }
    }
    if timed_out(me.member, me.now_ms) {
        perm &= ALLOW_IN_TIMEOUT;
    }
    perm
}

/// `server` is the channel's server (for server channels); `recipient` is the
/// other user in a DM.
pub fn channel_permissions(channel: &Channel, server: Option<&Server>, recipient: Option<&User>, me: &Me) -> u64 {
    if me.user.is_some_and(|u| u.privileged) {
        return GRANT_ALL_SAFE;
    }
    match channel.channel_type.as_str() {
        "SavedMessages" => GRANT_ALL_SAFE,
        // The SDK grants messaging to friends (and ourselves); others are view-only.
        "DirectMessage" => match recipient.and_then(|u| u.relationship.as_deref()) {
            Some("Friend" | "User") => DEFAULT_DIRECT_MESSAGE,
            _ if recipient.is_some_and(|u| u.bot.is_some()) => DEFAULT_DIRECT_MESSAGE,
            _ => VIEW_ONLY,
        },
        "Group" => {
            if channel.owner.as_deref() == Some(me.id) {
                GRANT_ALL_SAFE
            } else {
                channel.permissions.map_or(DEFAULT_DIRECT_MESSAGE, |p| p as u64)
            }
        }
        "TextChannel" => {
            let Some(server) = server else { return 0 };
            if server.owner == me.id {
                return GRANT_ALL_SAFE;
            }
            let mut perm = server_permissions(server, me);
            if let Some(o) = &channel.default_permissions {
                perm = apply(perm, o);
            }
            if let Some(member) = me.member {
                for id in ordered_roles(server, member) {
                    if let Some(o) = channel.role_permissions.get(id) {
                        perm = apply(perm, o);
                    }
                }
            }
            if timed_out(me.member, me.now_ms) {
                perm &= ALLOW_IN_TIMEOUT;
            }
            perm
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn server() -> Server {
        serde_json::from_value(json!({
            "_id": "S", "name": "s", "owner": "OWNER",
            "default_permissions": DEFAULT,
            "roles": {
                "MOD": { "name": "Mod", "rank": 1, "permissions": { "a": MANAGE_MESSAGES, "d": 0 } },
                "MUTED": { "name": "Muted", "rank": 5, "permissions": { "a": 0, "d": SEND_MESSAGE | MANAGE_MESSAGES } },
            },
        }))
        .unwrap()
    }

    fn member(roles: &[&str], timeout: Option<&str>) -> Member {
        serde_json::from_value(json!({ "_id": { "server": "S", "user": "ME" }, "roles": roles, "timeout": timeout }))
            .unwrap()
    }

    fn me<'a>(member: Option<&'a Member>) -> Me<'a> {
        Me {
            user: None,
            id: "ME",
            member,
            now_ms: 1_700_000_000_000,
        }
    }

    fn text_channel(extra: serde_json::Value) -> Channel {
        let mut v = json!({ "_id": "C", "channel_type": "TextChannel", "server": "S" });
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn roles_apply_lowest_rank_last() {
        let s = server();
        let plain = member(&[], None);
        assert_eq!(server_permissions(&s, &me(Some(&plain))) & MANAGE_MESSAGES, 0);
        let m = member(&["MOD"], None);
        assert_ne!(server_permissions(&s, &me(Some(&m))) & MANAGE_MESSAGES, 0);
        // MOD (rank 1) outranks MUTED (rank 5), so its allow wins.
        let both = member(&["MUTED", "MOD"], None);
        let p = server_permissions(&s, &me(Some(&both)));
        assert_ne!(p & MANAGE_MESSAGES, 0);
        assert_eq!(p & SEND_MESSAGE, 0, "MOD doesn't re-allow sending");
    }

    #[test]
    fn owner_and_timeouts() {
        let s = server();
        let owner = Me {
            id: "OWNER",
            ..me(None)
        };
        assert_eq!(server_permissions(&s, &owner), GRANT_ALL_SAFE);
        let m = member(&["MOD"], Some("2100-01-01T00:00:00Z"));
        assert_eq!(server_permissions(&s, &me(Some(&m))), ALLOW_IN_TIMEOUT);
        let expired = member(&["MOD"], Some("2000-01-01T00:00:00Z"));
        assert_ne!(server_permissions(&s, &me(Some(&expired))) & MANAGE_MESSAGES, 0);
    }

    #[test]
    fn channel_overrides() {
        let s = server();
        let m = member(&["MOD"], None);
        let c = text_channel(json!({
            "default_permissions": { "a": 0, "d": SEND_MESSAGE },
            "role_permissions": { "MOD": { "a": SEND_MESSAGE, "d": 0 } },
        }));
        assert_ne!(
            channel_permissions(&c, Some(&s), None, &me(Some(&m))) & SEND_MESSAGE,
            0,
            "role override re-allows"
        );
        let plain = member(&[], None);
        assert_eq!(
            channel_permissions(&c, Some(&s), None, &me(Some(&plain))) & SEND_MESSAGE,
            0,
            "channel default denies"
        );
    }

    #[test]
    fn editor_rows_per_context() {
        let shown = |ctx| LIST.iter().filter(|i| i.description(ctx).is_some()).count();
        assert_eq!(
            shown(EditContext::Server),
            LIST.len(),
            "every permission is a server permission"
        );
        assert!(shown(EditContext::Group) < shown(EditContext::TextChannel));
        let manage = &LIST[0];
        assert_eq!(
            manage.description(EditContext::Group),
            Some("Edit group name and description")
        );
        assert_eq!(manage.description(EditContext::Server), Some("Edit and delete channel"));
        assert_eq!(
            LIST.iter().find(|i| i.title == "Use External Emojis").unwrap().bit,
            USE_EXTERNAL_EMOJIS
        );
    }

    #[test]
    fn private_channels() {
        let dm: Channel = serde_json::from_value(json!({ "_id": "D", "channel_type": "DirectMessage" })).unwrap();
        let friend: User =
            serde_json::from_value(json!({ "_id": "F", "username": "f", "relationship": "Friend" })).unwrap();
        let stranger: User =
            serde_json::from_value(json!({ "_id": "X", "username": "x", "relationship": "None" })).unwrap();
        assert_ne!(
            channel_permissions(&dm, None, Some(&friend), &me(None)) & SEND_MESSAGE,
            0
        );
        assert_eq!(
            channel_permissions(&dm, None, Some(&stranger), &me(None)) & SEND_MESSAGE,
            0
        );
        let group: Channel =
            serde_json::from_value(json!({ "_id": "G", "channel_type": "Group", "owner": "ME" })).unwrap();
        assert_eq!(channel_permissions(&group, None, None, &me(None)), GRANT_ALL_SAFE);
    }
}
