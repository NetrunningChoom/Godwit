// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors

use super::*;

/// What an [`Event::Alert`] is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertKind {
    Message,
    /// Someone is calling us: rings until [`Event::AlertDone`].
    Call,
}

/// A file picked to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub path: PathBuf,
    pub name: Arc<str>,
    pub size: u64,
}

/// Someone the user knows on a network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    pub id: UserId,
    pub name: Arc<str>,
    pub avatar_url: Option<Arc<str>>,
    pub online: bool,
}
/// What the composer is doing besides writing a new message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Compose {
    Reply(MessageId),
    Edit(MessageId),
}

/// A conversation's loaded messages, oldest first. The list is shared
/// (`Arc`) so the UI can hold it without copying; it's replaced whenever it
/// changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timeline {
    pub messages: Arc<Vec<Arc<Message>>>,
    pub reached_start: bool,
    pub loading: bool,
    /// The last load failed (offline, server trouble): say so, offer a retry.
    pub failed: bool,
    /// Showing older messages around a jump, not the newest: live messages
    /// aren't added (that would leave a gap) until back to the present.
    pub detached: bool,
    /// Restored from disk and waiting for a fresh network history page.
    pub cached: bool,
}

/// The suggestion bar: `contact` is also on a better network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nudge {
    pub contact: ContactId,
    pub to: Network,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub id: AccountId,
    pub display_name: Arc<str>,
    pub username: Arc<str>,
    pub avatar_url: Option<Arc<str>>,
    /// This device isn't verified yet, so older encrypted messages can't be
    /// read: the network wants a recovery key (or another device's OK).
    pub unverified: bool,
    /// The connection dropped; the network is trying again.
    pub offline: bool,
    /// Our own presence there, if the network has presence.
    pub presence: Option<Presence>,
    /// Our custom status text there.
    pub status: Option<Arc<str>>,
}

/// Where verifying this device with another of ours stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verify {
    /// Another of our devices asks to verify this one: accept or decline.
    Incoming,
    /// Asked; waiting for the other device to accept.
    Waiting,
    /// Compare these (emoji, name) with the other device's screen.
    Compare(Vec<(Arc<str>, Arc<str>)>),
    /// We said they match; waiting for the other device to say so too.
    Confirmed,
    Done,
    Failed(Arc<str>),
}

/// Something secret the user typed (a recovery key). Its debug output is
/// redacted, so no log or error can print it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// A message search: where, what was asked, and what it found.
#[derive(Clone, Debug, PartialEq)]
pub struct Search {
    pub room: RoomId,
    pub query: String,
    /// None while searching (or before anything is asked).
    pub messages: Option<Vec<Arc<Message>>>,
    pub failed: bool,
}

/// The pinned messages panel: whose, and the list once loaded.
#[derive(Clone, Debug, PartialEq)]
pub struct Pins {
    pub room: RoomId,
    /// None while loading.
    pub messages: Option<Vec<Arc<Message>>>,
    pub failed: bool,
}

/// The "Games in common" dialog: whose libraries are compared.
#[derive(Clone, Debug, PartialEq)]
pub struct GamesInCommon {
    pub account: AccountId,
    /// Who is compared: us first, then the people picked, in picking order.
    pub people: Vec<UserId>,
    /// The libraries loaded so far (someone missing is still loading). Kept
    /// for people taken out again, so picking them back needs no request.
    pub libraries: HashMap<UserId, crate::games::Library>,
}

/// Everything the UI shows. Plain data; the UI never mutates it directly.
#[derive(Clone, Debug, Default)]
pub struct AppState {
    /// Each account's privacy switches, as its network last sent them.
    pub privacy: HashMap<AccountId, crate::privacy::PrivacyView>,
    /// Each account's signed-in devices, as its network last listed them.
    pub sessions: HashMap<AccountId, crate::privacy::SessionsView>,
    /// Each account's auto-delete page, as its network last sent it.
    pub autodelete: HashMap<AccountId, crate::autodelete::AutoDeleteView>,
    pub server_settings: Option<crate::server_settings::ServerSettingsState>,
    pub channel_settings: Option<crate::channel_settings::ChannelSettingsState>,
    /// Home's pinned chats (at the top, in pin order) and hidden ones (in
    /// a collapsible section at the bottom; a new message brings one back).
    pub pinned_rooms: Vec<RoomId>,
    pub hidden_rooms: Vec<RoomId>,
    pub show_hidden: bool,
    /// Chats the user marked unread, shown so until opened.
    pub marked_unread: Vec<RoomId>,
    /// The server menu's Invite dialog: its server and, once made, the link.
    pub invite_link: Option<(ServerId, Option<Arc<str>>)>,
    pub(super) server_settings_sequence: u64,
    pub(super) channel_settings_sequence: u64,
    pub own_profiles: HashMap<AccountId, OwnProfileState>,
    pub accounts: Vec<Account>,
    /// Every account's rooms; direct messages in each account's order.
    pub rooms: Vec<Arc<Room>>,
    /// Every account's servers, each account's in its own order.
    pub servers: Vec<Arc<Server>>,
    /// The server whose rooms are listed; None lists direct messages.
    pub selected_server: Option<ServerId>,
    pub selected: Option<RoomId>,
    pub timelines: HashMap<RoomId, Timeline>,
    pub drafts: HashMap<RoomId, String>,
    pub contacts: Contacts,
    pub nudge: Option<Nudge>,
    /// Replies and edits in progress, per room.
    pub compose: HashMap<RoomId, Compose>,
    /// Who is typing, per room.
    pub typing: HashMap<RoomId, Vec<Arc<str>>>,
    /// Files waiting to go with the next message, per room.
    pub files: HashMap<RoomId, Vec<Outgoing>>,
    /// Every account's friends.
    pub friends: Vec<Person>,
    /// Directories of accounts on networks without friends (see
    /// [`Event::Directory`]), merged into `friends`.
    pub people_directory: HashMap<AccountId, Vec<Person>>,
    /// People's presence from [`Event::Presences`].
    pub presence: HashMap<UserId, Presence>,
    /// Invite cards' states, by account and code.
    pub invites: HashMap<(AccountId, Arc<str>), crate::message::Invite>,
    /// People we blocked: their messages collapse in the timeline.
    pub blocked: std::collections::HashSet<UserId>,
    /// Favourite people as the networks keep them (on those whose
    /// [`Network::keeps_favourites`](crate::network::Network::keeps_favourites)),
    /// so a star set in their own clients shows here too. Godwit's own,
    /// local stars are the app's.
    pub favourites: std::collections::HashSet<UserId>,
    /// Pending friend requests: who, and whether they asked (not us).
    pub friend_requests: Vec<(Person, bool)>,
    /// The last "Add friend" from the friends list: sending (None after
    /// sending, until the answer), sent, or why not.
    pub friend_request_status: Option<Result<(), Arc<str>>>,
    pub friend_request_pending: Option<(AccountId, u64)>,
    pub(super) friend_request_sequence: u64,
    /// The friends list is open (in place of a room).
    pub showing_friends: bool,
    /// People is open (in place of a room).
    pub showing_people: bool,
    /// Active now is open (in place of a room).
    pub showing_active: bool,
    /// Notifications is open (in place of a room). The list itself is the
    /// app's (it follows the app's alert settings), only the page is here.
    pub showing_notifications: bool,
    /// Games in common is open (in place of a room), comparing `games`.
    pub showing_games: bool,
    pub showing_threads: bool,
    /// What people are doing ("Playing …"), from [`Event::Activities`].
    pub activities: HashMap<UserId, Arc<str>>,
    /// Servers' directories: rooms we could join, as last loaded.
    pub directory: HashMap<ServerId, Vec<DirectoryRoom>>,
    /// Verifying this device with another of ours, while it's shown.
    pub verification: Option<(AccountId, Verify)>,
    /// The person People should start linking, numbered per request (the
    /// UI acts when the number changes).
    pub link_request: Option<(ContactId, u32)>,
    /// The call we're in.
    pub call: Option<Call>,
    /// Our own mute and deafen, kept between calls as Discord's are: the
    /// account card's buttons work outside a call, and a call starts so.
    pub self_muted: bool,
    pub self_deafened: bool,
    /// Who is in each room's call.
    pub voice: HashMap<RoomId, Vec<CallMember>>,
    /// When each voice channel's current session began (Unix ms), where
    /// the network says (Discord's `voice_start_time`): shown beside it.
    pub voice_since: HashMap<RoomId, i64>,
    /// Every account's custom emoji, by server.
    pub emoji: Vec<EmojiGroup>,
    /// Where the "New messages" divider goes in the open room: after this
    /// message (what was read when it was opened unread).
    pub unread_from: Option<MessageId>,
    /// The message to scroll to and flash, numbered per jump (the UI acts
    /// when the number changes).
    pub jump: Option<(MessageId, u32)>,
    /// A jump waiting for its messages to load.
    pub(super) jump_pending: Option<MessageId>,
    /// The open profile card: whose, and what's known so far.
    pub profile: Option<(UserId, Option<Profile>)>,
    /// Profiles kept for cache and quick reuse, keyed by network identity.
    pub profiles: HashMap<UserId, Profile>,
    /// The member list beside the room (the official default: shown).
    pub show_members: bool,
    pub members: HashMap<RoomId, Arc<Vec<MemberGroup>>>,
    /// Most recent last; the UI shows them until [`Action::ClearErrors`].
    pub errors: Vec<Arc<str>>,
    /// The last report's outcome (None while it is on its way, or none was
    /// sent since the last [`Action::Report`]); the report dialog shows it.
    pub report_outcome: Option<Result<(), Arc<str>>>,
    /// The pinned messages panel, for the selected room.
    pub pins: Option<Pins>,
    /// The "Games in common" dialog, while open.
    pub games: Option<GamesInCommon>,
    /// The message search, for the selected room.
    pub search: Option<Search>,
    /// The GIF picker's contents, per account.
    pub gifs: HashMap<AccountId, crate::gifs::GifPicker>,
}

impl AppState {
    pub fn room(&self, id: &RoomId) -> Option<&Arc<Room>> {
        self.rooms.iter().find(|r| &r.id == id)
    }

    pub fn server(&self, id: &ServerId) -> Option<&Arc<Server>> {
        self.servers.iter().find(|s| &s.id == id)
    }

    pub fn timeline(&self, id: &RoomId) -> Option<&Timeline> {
        self.timelines.get(id)
    }

    /// Whether we wrote `m` (with the account its room belongs to).
    pub fn is_mine(&self, m: &Message) -> bool {
        m.author.raw == m.author.account.user
    }

    /// A loaded message of the selected room.
    pub fn selected_message(&self, id: &MessageId) -> Option<&Arc<Message>> {
        let room = self.selected.as_ref()?;
        self.timelines.get(room)?.messages.iter().find(|m| &m.id == id)
    }

    /// The custom emoji usable in `room`: its own server's, plus everyone
    /// else's where external emoji are allowed.
    pub fn emoji_for(&self, room: &RoomId) -> Vec<&EmojiGroup> {
        let Some(r) = self.room(room) else { return Vec::new() };
        let here = match &r.kind {
            RoomKind::Channel { server } => Some(server),
            _ => None,
        };
        let mut groups: Vec<&EmojiGroup> = self
            .emoji
            .iter()
            .filter(|g| g.server.account == room.account)
            .filter(|g| r.capabilities.external_emoji || Some(&g.server) == here)
            .collect();
        // The room's own server first.
        groups.sort_by_key(|g| Some(&g.server) != here);
        groups
    }

    /// Rooms outside any server (DMs, groups, notes), in their order.
    pub fn direct_rooms(&self) -> impl Iterator<Item = &Arc<Room>> {
        self.rooms
            .iter()
            .filter(|r| !matches!(r.kind, RoomKind::Channel { .. }))
    }
}
