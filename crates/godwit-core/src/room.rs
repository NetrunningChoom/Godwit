// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Conversations as the UI sees them, whatever their network.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::ids::{RoomId, ServerId, UserId};
use crate::network::Network;

/// What can be done in a room. The UI follows these instead of asking which
/// network a room is on. Backends start from [`Capabilities::of`] and adjust
/// per room (permissions, room settings).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Capabilities {
    /// This group conversation can be left (the network removes it on confirmation).
    #[serde(default)]
    pub leave: bool,
    /// A direct conversation can be closed (removed from the network's DM list).
    #[serde(default)]
    pub close: bool,
    /// A member can be added to this group conversation.
    #[serde(default)]
    pub add_group_member: bool,
    pub send: bool,
    pub edit: bool,
    pub delete: bool,
    /// Delete others' messages, including system messages, as a moderator.
    #[serde(default)]
    pub delete_others: bool,
    /// Remove one or all reactions from a message as a moderator.
    #[serde(default)]
    pub moderate_reactions: bool,
    /// Load the people who used one reaction on a message.
    #[serde(default)]
    pub reaction_users: bool,
    pub react: bool,
    /// "Mark unread" can move the read marker back (the network takes it).
    #[serde(default)]
    pub mark_unread: bool,
    /// Messages here can be forwarded to another room of the account (the
    /// network's own rule: its channel permissions and server settings).
    #[serde(default)]
    pub forward_from: bool,
    /// A forwarded message can be sent here.
    #[serde(default)]
    pub forward_to: bool,
    /// A poll can be created here, with what the network takes in one.
    #[serde(default)]
    pub polls: Option<crate::poll::PollLimits>,
    pub reply: bool,
    pub attachments: bool,
    /// `||text||` hides text until clicked.
    pub spoilers: bool,
    pub pins: bool,
    pub search: bool,
    /// Which filters its search takes (`from:`, `has:`, ...); only with `search`.
    #[serde(default)]
    pub search_filters: crate::search::SearchFilters,
    /// Values available for the `has:` search filter, when supported.
    #[serde(default)]
    pub search_has: crate::search::SearchHasSet,
    /// Threads can be started here (from a message, or as a forum's post).
    pub threads: bool,
    /// A thread can be joined or left; its membership is shown on the room.
    #[serde(default)]
    pub join_thread: bool,
    /// A forum: no messages of its own, its posts are threads under it.
    #[serde(default)]
    pub forum: bool,
    pub voice: bool,
    /// Soundboard sounds can be played in this call.
    #[serde(default)]
    pub soundboard: bool,
    /// A voice channel's capacity: 0 means unlimited, None means unknown.
    #[serde(default)]
    pub voice_user_limit: Option<u32>,
    /// Sending a camera or screen, after instance limits and room permissions.
    #[serde(default)]
    pub video: bool,
    /// Maximum width/height; a zero side is unlimited.
    #[serde(default)]
    pub video_resolution: [u32; 2],
    /// The network has calls, but Godwit's don't work yet: the call button
    /// shows, disabled, with a note. Joining stays impossible (`voice` is false).
    #[serde(default)]
    pub voice_unavailable: bool,
    pub typing: bool,
    /// Custom emoji from other servers may be used here.
    pub external_emoji: bool,
    /// The GIF picker can send here (Stoat: the SendEmbeds permission).
    #[serde(default)]
    pub gifs: bool,
    /// The sticker picker can send stickers here (Discord: Send Messages in
    /// a server; the stickers are that server's own).
    #[serde(default)]
    pub stickers: bool,
    /// The largest file that can be attached here, in bytes (0: not known).
    #[serde(default)]
    pub max_upload: u64,
    /// Messages can be pinned and unpinned here (`pins`: the list can be seen).
    #[serde(default)]
    pub pin_messages: bool,
    /// The network takes reports about messages here (the menu still hides
    /// it for our own and system messages).
    #[serde(default)]
    pub report: bool,
    /// The network can look up members of this room's server by name when
    /// they aren't loaded (Discord's gateway search); the member list's
    /// search box and mention completion then ask it. Networks that load
    /// every member leave it off: the local filter already finds them.
    #[serde(default)]
    pub member_search: bool,
    /// The channel's settings page can be opened; its individual actions may
    /// require narrower permissions than this page-level capability.
    #[serde(default)]
    pub manage_channel: bool,
    /// Webhooks for this channel can be managed (Stoat: Manage Webhooks).
    #[serde(default)]
    pub manage_webhooks: bool,
    /// We can set how loudly this room notifies (a mute, or a level of its
    /// own): the menu offers it, [`Room::notify`] shows the state (Discord:
    /// channel overrides in the user's guild settings).
    #[serde(default)]
    pub notify_settings: bool,
}

impl Capabilities {
    /// What a network's rooms normally allow a member to do.
    pub const fn of(network: Network) -> Self {
        let all = Self {
            leave: false,
            close: false,
            add_group_member: false,
            send: true,
            edit: true,
            delete: true,
            delete_others: false,
            moderate_reactions: false,
            reaction_users: false,
            react: true,
            mark_unread: false,
            forward_from: false,
            forward_to: false,
            polls: None,
            reply: true,
            attachments: true,
            spoilers: true,
            pins: true,
            search: true,
            search_filters: crate::search::SearchFilters::NONE,
            search_has: crate::search::SearchHasSet::NONE,
            threads: false,
            join_thread: false,
            voice: true,
            soundboard: false,
            voice_user_limit: None,
            video: false,
            video_resolution: [1280, 720],
            voice_unavailable: false,
            typing: true,
            external_emoji: true,
            gifs: true,
            stickers: false,
            max_upload: 0,
            pin_messages: false,
            manage_channel: false,
            manage_webhooks: false,
            notify_settings: false,
            forum: false,
            report: false,
            member_search: false,
        };
        match network {
            // Autumn takes attachments up to 20 MB (Stoat's own limit).
            Network::Stoat => Self {
                mark_unread: true,
                video: true,
                max_upload: 20 * 1024 * 1024,
                ..all
            },
            // Discord without Nitro or boosts (the account adjusts per room).
            Network::Discord => Self {
                mark_unread: true,
                threads: true,
                max_upload: 10 * 1024 * 1024,
                ..all
            },
            // First version: text, replies, edits, deletes and reactions.
            Network::Fluxer => Self {
                attachments: true,
                max_upload: crate::media::MAX_UPLOAD,
                pins: false,
                search: false,
                voice: false,
                gifs: false,
                ..all
            },
            // First version: text, deleting and read state. Editing, reactions
            // and files go through calls not built yet; calls come later.
            Network::Teams => Self {
                edit: false,
                react: false,
                reply: false,
                attachments: false,
                spoilers: false,
                pins: false,
                search: false,
                voice: false,
                voice_unavailable: true,
                gifs: false,
                external_emoji: false,
                ..all
            },
            // Text, files, edits, deletes, reactions, read state, and replies
            // as thread replies. Huddles come later.
            Network::Slack => Self {
                attachments: true,
                max_upload: crate::media::MAX_UPLOAD,
                spoilers: false,
                pins: false,
                search: false,
                voice: false,
                voice_unavailable: true,
                gifs: false,
                external_emoji: false,
                ..all
            },
            // Text, history and files; other operations remain disabled.
            // Text, files, edits, deletes, reactions and quote replies, as
            // maugclib sends them. Threads show flat for now.
            Network::GoogleChat => Self {
                // Our own messages only: space managers' deletes aren't sent.
                delete_others: false,
                threads: false,
                attachments: true,
                max_upload: crate::media::MAX_UPLOAD,
                spoilers: false,
                pins: false,
                search: false,
                voice: false,
                typing: false,
                gifs: false,
                external_emoji: false,
                ..all
            },
            Network::Steam => Self {
                edit: false,
                // ChatRoom.DeleteChatMessages is published for group rooms;
                // direct FriendMessages has no corresponding request.
                delete: false,
                // Steam's EMessageReactionType accepts emoticons/stickers, while the shared picker accepts arbitrary Unicode.
                // Keep it hidden until the core can restrict it to Steam's accepted names (steam-vent friendmessages proto).
                react: false,
                // Neither FriendMessages.SendMessage nor ChatRoom.SendChatMessage has a reply field (steam-vent protos).
                reply: false,
                attachments: true,
                max_upload: crate::media::MAX_UPLOAD,
                spoilers: false,
                pins: false,
                search: false,
                // 1:1 and group voice (docs/steam-voice.md); group rooms set
                // `voice` from Steam's `voice_allowed`.
                voice: true,
                voice_unavailable: false,
                gifs: false,
                ..all
            },
        }
    }
}

/// Where a room sits.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RoomKind {
    /// One-to-one with another person.
    Direct {
        with: UserId,
    },
    Group,
    /// A channel inside a server, guild or space.
    Channel {
        server: ServerId,
    },
    /// Notes to yourself (Stoat's Saved Notes and the like).
    Notes,
}

/// Whether this room's messages are end-to-end encrypted *right now*. A room
/// can differ from its network's default (see [`crate::network::Security`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RoomEncryption {
    Unencrypted,
    Encrypted,
    /// Not known yet (still loading).
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Room {
    pub id: RoomId,
    pub name: Arc<str>,
    pub kind: RoomKind,
    pub icon_url: Option<Arc<str>>,
    pub capabilities: Capabilities,
    pub encryption: RoomEncryption,
    pub unread: bool,
    pub mentions: u32,
    /// The last message we've read here, if the network knows it.
    pub last_read: Option<crate::ids::MessageId>,
    /// We're invited, not in it yet: by whom (a name). Nothing loads until
    /// the invite is accepted.
    pub invited_by: Option<Arc<str>>,
    /// A direct chat: the other person's presence and custom status text.
    pub presence: Option<crate::members::Presence>,
    pub status: Option<Arc<str>>,
    /// A group: how many are in it.
    pub member_count: Option<u32>,
    /// When something last happened here (its newest message), in Unix
    /// milliseconds; 0 when not known. Home lists chats newest first.
    #[serde(default)]
    pub last_activity: i64,
    /// A thread (or a forum's post): the room it belongs to (its raw id),
    /// listed under it.
    #[serde(default)]
    pub parent: Option<Arc<str>>,
    /// Whether the current user is a member of this thread. Only meaningful
    /// when `capabilities.join_thread` is true.
    #[serde(default)]
    pub joined_thread: bool,
    /// Our own notification choices here (see `Capabilities::notify_settings`).
    #[serde(default)]
    pub notify: RoomNotify,
}

/// How we asked a room to notify: its own level, and a mute. Networks
/// without per-room settings leave it at the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomNotify {
    /// 0 all messages, 1 only mentions, 2 nothing; None follows the server.
    pub level: Option<u8>,
    /// Muted now (a mute that has run out counts as off).
    pub muted: bool,
}

/// A change to a room's [`RoomNotify`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomNotifyChange {
    /// Its own level (0 all, 1 mentions, 2 nothing); None: the server's.
    Level(Option<u8>),
    /// Mute for this many seconds; None: until we turn it back on.
    Mute {
        seconds: Option<u32>,
    },
    Unmute,
}

impl Room {
    pub fn network(&self) -> Network {
        self.id.network()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_chats_disable_unavailable_actions() {
        let c = Capabilities::of(Network::Steam);
        assert!(c.send && !c.edit && !c.delete && !c.react && !c.reply);
        assert!(c.attachments);
        assert_eq!(c.max_upload, crate::media::MAX_UPLOAD);
    }

    #[test]
    fn google_chat_supports_text_files_edits_reactions_and_replies() {
        let c = Capabilities::of(Network::GoogleChat);
        assert!(c.send && c.attachments && c.edit && c.delete && c.react && c.reply);
        assert_eq!(c.max_upload, crate::media::MAX_UPLOAD);
        assert!(
            [
                c.delete_others,
                c.spoilers,
                c.pins,
                c.search,
                c.threads,
                c.forum,
                c.voice,
                c.video,
                c.voice_unavailable,
                c.typing,
                c.external_emoji,
                c.gifs,
                c.stickers,
                c.pin_messages,
                c.manage_channel,
            ]
            .into_iter()
            .all(|enabled| !enabled)
        );
    }

    #[test]
    fn only_some_networks_thread() {
        assert!(Capabilities::of(Network::Discord).threads);
        assert!(Capabilities::of(Network::Discord).threads);
        assert!(!Capabilities::of(Network::Stoat).threads);
    }
}
