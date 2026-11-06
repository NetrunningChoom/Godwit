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
pub const ALL: u64 = u64::MAX;

/// A channel's change to one role's or member's permissions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overwrite {
    pub id: String,
    /// A member's overwrite (type 1), else a role's (type 0).
    pub member: bool,
    pub allow: u64,
    pub deny: u64,
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
