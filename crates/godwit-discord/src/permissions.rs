// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Discord's permissions, computed as its clients compute them
//! (<https://docs.discord.food/topics/permissions>): @everyone's role, then
//! the member's roles, Administrator granting all; then in a channel the
//! @everyone overwrite, the roles' overwrites together, and the member's own.
//! Godwit uses them to show only what's allowed, so it never sends requests
//! Discord would refuse (refusals count against the account).

pub const CREATE_INSTANT_INVITE: u64 = 1 << 0;
pub const KICK_MEMBERS: u64 = 1 << 1;
pub const BAN_MEMBERS: u64 = 1 << 2;
pub const ADMINISTRATOR: u64 = 1 << 3;
pub const MANAGE_CHANNELS: u64 = 1 << 4;
pub const MANAGE_GUILD: u64 = 1 << 5;
pub const ADD_REACTIONS: u64 = 1 << 6;
pub const USE_EXTERNAL_EMOJIS: u64 = 1 << 18;
pub const VIEW_CHANNEL: u64 = 1 << 10;
pub const SEND_MESSAGES: u64 = 1 << 11;
/// Writing in threads (and forum posts) takes this instead of Send Messages.
pub const SEND_MESSAGES_IN_THREADS: u64 = 1 << 38;
pub const CREATE_PUBLIC_THREADS: u64 = 1 << 35;
pub const MANAGE_MESSAGES: u64 = 1 << 13;
pub const EMBED_LINKS: u64 = 1 << 14;
pub const ATTACH_FILES: u64 = 1 << 15;
pub const READ_MESSAGE_HISTORY: u64 = 1 << 16;
/// Pinning on its own (Discord split it from Manage Messages in 2025).
pub const PIN_MESSAGES: u64 = 1 << 51;
pub const MENTION_EVERYONE: u64 = 1 << 17;
/// "Video": cameras and Go Live in voice channels (Discord's STREAM).
pub const STREAM: u64 = 1 << 9;
pub const CONNECT: u64 = 1 << 20;
pub const SPEAK: u64 = 1 << 21;
pub const MANAGE_ROLES: u64 = 1 << 28;
pub const CHANGE_NICKNAME: u64 = 1 << 26;
pub const MANAGE_NICKNAMES: u64 = 1 << 27;
/// Timing members out (docs.discord.food/topics/permissions).
pub const MODERATE_MEMBERS: u64 = 1 << 40;
/// Manage Expressions: change anyone's emoji (docs.discord.food, Permissions).
pub const MANAGE_GUILD_EXPRESSIONS: u64 = 1 << 30;
/// Create Expressions: add emoji, and change or delete our own.
pub const CREATE_GUILD_EXPRESSIONS: u64 = 1 << 43;
pub const ALL: u64 = u64::MAX;

/// Channel kinds for [`CHANNEL_ROWS`]: text (also announcement, forum and
/// media), voice and stage.
pub const TEXT: u8 = 1;
pub const VOICE: u8 = 2;
pub const STAGE: u8 = 4;

/// The permissions a channel's Permissions page lists, in the official
/// client's order and groups: (group, bit, title, description, kinds).
/// Which apply where follows the Channel Type column of
/// docs.discord.food/topics/permissions.
pub const CHANNEL_ROWS: &[(&str, u64, &str, &str, u8)] = &[
    (
        "General Channel Permissions",
        1 << 10,
        "View Channel",
        "Allows members to view this channel. Turn it off for private channels.",
        7,
    ),
    (
        "General Channel Permissions",
        1 << 4,
        "Manage Channel",
        "Allows members to change this channel's name, description and settings, or delete it.",
        7,
    ),
    (
        "General Channel Permissions",
        1 << 28,
        "Manage Permissions",
        "Allows members to change this channel's permissions.",
        7,
    ),
    (
        "General Channel Permissions",
        1 << 29,
        "Manage Webhooks",
        "Allows members to create, edit and delete webhooks.",
        7,
    ),
    (
        "General Channel Permissions",
        1 << 0,
        "Create Invite",
        "Allows members to invite new people to this server through this channel.",
        7,
    ),
    (
        "Text Channel Permissions",
        1 << 11,
        "Send Messages",
        "Allows members to send messages in this channel.",
        7,
    ),
    (
        "Text Channel Permissions",
        1 << 38,
        "Send Messages in Threads",
        "Allows members to send messages in threads.",
        TEXT,
    ),
    (
        "Text Channel Permissions",
        1 << 35,
        "Create Public Threads",
        "Allows members to create threads that everyone in the channel can view.",
        TEXT,
    ),
    (
        "Text Channel Permissions",
        1 << 36,
        "Create Private Threads",
        "Allows members to create invite-only threads.",
        TEXT,
    ),
    (
        "Text Channel Permissions",
        1 << 14,
        "Embed Links",
        "Allows links that members share to show an embedded preview.",
        7,
    ),
    (
        "Text Channel Permissions",
        1 << 15,
        "Attach Files",
        "Allows members to upload files or media.",
        7,
    ),
    (
        "Text Channel Permissions",
        1 << 6,
        "Add Reactions",
        "Allows members to add new emoji reactions to a message.",
        7,
    ),
    (
        "Text Channel Permissions",
        1 << 18,
        "Use External Emoji",
        "Allows members to use emoji from other servers.",
        7,
    ),
    (
        "Text Channel Permissions",
        1 << 37,
        "Use External Stickers",
        "Allows members to use stickers from other servers.",
        7,
    ),
    (
        "Text Channel Permissions",
        1 << 17,
        "Mention @everyone, @here, and All Roles",
        "Allows members to use @everyone and @here, and to mention all roles.",
        7,
    ),
    (
        "Text Channel Permissions",
        1 << 13,
        "Manage Messages",
        "Allows members to delete messages by other members.",
        7,
    ),
    (
        "Text Channel Permissions",
        1 << 51,
        "Pin Messages",
        "Allows members to pin or unpin any message.",
        7,
    ),
    (
        "Text Channel Permissions",
        1 << 34,
        "Manage Threads",
        "Allows members to rename, delete, close and turn on slow mode for threads, and to view private threads.",
        TEXT,
    ),
    (
        "Text Channel Permissions",
        1 << 16,
        "Read Message History",
        "Allows members to read earlier messages sent in this channel.",
        7,
    ),
    (
        "Text Channel Permissions",
        1 << 12,
        "Send Text-to-Speech Messages",
        "Allows members to send text-to-speech messages by starting a message with /tts.",
        7,
    ),
    (
        "Text Channel Permissions",
        1 << 31,
        "Use Application Commands",
        "Allows members to use commands from applications, including slash commands and context menu commands.",
        7,
    ),
    (
        "Text Channel Permissions",
        1 << 46,
        "Send Voice Messages",
        "Allows members to send voice messages.",
        7,
    ),
    (
        "Text Channel Permissions",
        1 << 49,
        "Create Polls",
        "Allows members to create polls.",
        7,
    ),
    (
        "Text Channel Permissions",
        1 << 52,
        "Bypass Slowmode",
        "Allows members to send messages and create threads without waiting for slowmode.",
        7,
    ),
    (
        "Voice Channel Permissions",
        1 << 20,
        "Connect",
        "Allows members to join this voice channel and hear others.",
        VOICE | STAGE,
    ),
    (
        "Voice Channel Permissions",
        1 << 21,
        "Speak",
        "Allows members to talk in this voice channel.",
        VOICE,
    ),
    (
        "Voice Channel Permissions",
        1 << 9,
        "Video",
        "Allows members to share their video, screen share, or stream an application.",
        VOICE | STAGE,
    ),
    (
        "Voice Channel Permissions",
        1 << 42,
        "Use Soundboard",
        "Allows members to send sounds from the server soundboard.",
        VOICE,
    ),
    (
        "Voice Channel Permissions",
        1 << 45,
        "Use External Sounds",
        "Allows members to use sounds from other servers.",
        VOICE,
    ),
    (
        "Voice Channel Permissions",
        1 << 25,
        "Use Voice Activity",
        "Allows members to speak by just talking. When off, members need push-to-talk.",
        VOICE,
    ),
    (
        "Voice Channel Permissions",
        1 << 8,
        "Priority Speaker",
        "Allows members to be more easily heard: other people's volume goes down while they speak.",
        VOICE,
    ),
    (
        "Voice Channel Permissions",
        1 << 22,
        "Mute Members",
        "Allows members to mute others in this channel.",
        VOICE | STAGE,
    ),
    (
        "Voice Channel Permissions",
        1 << 23,
        "Deafen Members",
        "Allows members to deafen others in this channel.",
        VOICE,
    ),
    (
        "Voice Channel Permissions",
        1 << 24,
        "Move Members",
        "Allows members to move others out of this channel into another one they can connect to.",
        VOICE | STAGE,
    ),
    (
        "Voice Channel Permissions",
        1 << 48,
        "Set Voice Channel Status",
        "Allows members to create and edit voice channel status.",
        VOICE,
    ),
    (
        "Stage Channel Permissions",
        1 << 32,
        "Request to Speak",
        "Allows members to request to speak in stage channels.",
        STAGE,
    ),
];

/// A channel's change to one role's or member's permissions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overwrite {
    pub id: String,
    /// A member's overwrite (type 1), else a role's (type 0).
    pub member: bool,
    pub allow: u64,
    pub deny: u64,
}

impl Overwrite {
    /// The Modify Channel Permissions body: bit fields as strings.
    pub fn body(&self) -> serde_json::Value {
        serde_json::json!({
            "type": u8::from(self.member),
            "allow": self.allow.to_string(),
            "deny": self.deny.to_string(),
        })
    }
}

/// A permission field: Discord sends them as strings of digits.
pub fn bits(value: &serde_json::Value) -> u64 {
    value
        .as_str()
        .and_then(|b| b.parse().ok())
        .or_else(|| value.as_u64())
        .unwrap_or_default()
}

pub fn overwrites(channel: &serde_json::Value) -> Vec<Overwrite> {
    channel["permission_overwrites"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            Some(Overwrite {
                id: o["id"].as_str()?.to_owned(),
                member: o["type"].as_u64() == Some(1) || o["type"].as_str() == Some("member"),
                allow: bits(&o["allow"]),
                deny: bits(&o["deny"]),
            })
        })
        .collect()
}

/// Server-wide permissions: `everyone` is @everyone's role's (its id is the
/// server's), `roles` the member's roles' bits.
pub fn base(owner: bool, everyone: u64, roles: impl IntoIterator<Item = u64>) -> u64 {
    if owner {
        return ALL;
    }
    let perms = roles.into_iter().fold(everyone, |p, r| p | r);
    if perms & ADMINISTRATOR != 0 { ALL } else { perms }
}

/// A channel's permissions from the server-wide ones.
pub fn channel(base: u64, guild: &str, member: &str, roles: &[String], overwrites: &[Overwrite]) -> u64 {
    if base & ADMINISTRATOR != 0 || base == ALL {
        return ALL;
    }
    let mut perms = base;
    if let Some(o) = overwrites.iter().find(|o| !o.member && o.id == guild) {
        perms = (perms & !o.deny) | o.allow;
    }
    let (mut allow, mut deny) = (0, 0);
    for o in overwrites.iter().filter(|o| !o.member && roles.contains(&o.id)) {
        allow |= o.allow;
        deny |= o.deny;
    }
    perms = (perms & !deny) | allow;
    if let Some(o) = overwrites.iter().find(|o| o.member && o.id == member) {
        perms = (perms & !o.deny) | o.allow;
    }
    // Without View Channel nothing else counts.
    if perms & VIEW_CHANNEL == 0 { 0 } else { perms }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn administrators_and_owners_have_everything() {
        assert_eq!(base(true, 0, []), ALL);
        assert_eq!(base(false, VIEW_CHANNEL, [ADMINISTRATOR]), ALL);
        assert_eq!(base(false, VIEW_CHANNEL, [SEND_MESSAGES]), VIEW_CHANNEL | SEND_MESSAGES);
    }

    #[test]
    fn overwrites_apply_everyone_then_roles_then_the_member() {
        let o = overwrites(&json!({"permission_overwrites": [
            {"id": "G", "type": 0, "allow": "0", "deny": (SEND_MESSAGES).to_string()},
            {"id": "R1", "type": 0, "allow": (SEND_MESSAGES).to_string(), "deny": "0"},
            {"id": "M", "type": 1, "allow": "0", "deny": (ADD_REACTIONS).to_string()},
        ]}));
        let b = VIEW_CHANNEL | SEND_MESSAGES | ADD_REACTIONS;
        assert_eq!(
            channel(b, "G", "X", &[], &o),
            VIEW_CHANNEL | ADD_REACTIONS,
            "everyone can't send"
        );
        assert_eq!(channel(b, "G", "X", &["R1".into()], &o), b, "the role lets them");
        assert_eq!(
            channel(b, "G", "M", &["R1".into()], &o),
            VIEW_CHANNEL | SEND_MESSAGES,
            "their own overwrite wins"
        );
        let hidden = overwrites(
            &json!({"permission_overwrites": [{"id": "G", "type": 0, "allow": "0", "deny": VIEW_CHANNEL.to_string()}]}),
        );
        assert_eq!(channel(b, "G", "X", &[], &hidden), 0, "no view, nothing");
    }
}
