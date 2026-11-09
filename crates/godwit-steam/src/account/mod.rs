// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A Steam account as the core sees it: friends with their presence and
//! games, and a direct chat per friend talked with. Home lists the chats
//! Steam says have messages (as its own client does), plus any opened or
//! written in since; everyone else is under Friends.

mod calls;
mod chats;
mod events;
pub(crate) mod games;
mod group_chats;
mod login;
mod people;

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

use godwit_core::account::NetworkAccount;
use godwit_core::application::{AlertKind, Command, Event, Person};
use godwit_core::call::CallMember;
use godwit_core::ids::ServerId;
use godwit_core::ids::{AccountId, MessageId, RoomId, UserId};
use godwit_core::members::{Member as ListMember, MemberGroup, Presence, Profile, Relationship};
use godwit_core::message::{Attachment, AttachmentKind, CustomEmoji, EmojiGroup, InvitePreview, Markup, Message};
use godwit_core::network::Network;
use godwit_core::room::{Capabilities, Room, RoomEncryption, RoomKind};
use godwit_core::server::{Category, Server};

use godwit_voice::call::VoiceSettings;

use crate::auth::Guard;
use crate::backend::{self, Cmd, Ev, Handle, Line, Persona, Waker};
use crate::groups::GroupInfo;
use crate::session;
use crate::voice::session::{Member, SessionCmd, SessionEvent};

/// EFriendRelationship (Steam's values): we blocked them.
const BLOCKED: u32 = 1;
/// They asked to be friends.
const REQUEST_RECIPIENT: u32 = 2;
/// Friends.
const FRIEND: u32 = 3;
/// We asked them.
const REQUEST_INITIATOR: u32 = 4;

fn relationship(value: Option<u32>) -> Relationship {
    match value {
        Some(FRIEND) => Relationship::Friend,
        Some(REQUEST_RECIPIENT) => Relationship::Incoming,
        Some(REQUEST_INITIATOR) => Relationship::Outgoing,
        Some(BLOCKED) => Relationship::Blocked,
        _ => Relationship::None,
    }
}

/// A notification shows this much of a message.
const ALERT_CHARS: usize = 200;

fn shorten(text: &str) -> String {
    if text.chars().count() > ALERT_CHARS {
        text.chars().take(ALERT_CHARS).collect::<String>() + "…"
    } else {
        text.to_owned()
    }
}

/// The picture of someone without one (Steam's own default).
const DEFAULT_AVATAR: &str = "fef49e7fa7e1997310d705b2a6158ff8dc1cdfeb";

/// Where signing in stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    SignedOut {
        error: Option<String>,
    },
    /// Waiting for the QR code (or showing it): scan with the Steam app.
    Qr {
        url: Option<String>,
        scanned: bool,
    },
    /// The password went through; a Steam Guard code is wanted.
    NeedsCode {
        hint: String,
        error: Option<String>,
    },
    /// Waiting for approval in the Steam app or by email.
    NeedsApproval {
        hint: String,
    },
    LoggingIn,
    Connecting,
    Ready,
}

#[derive(Debug, Clone, Default)]
struct Friend {
    relationship: u32,
    persona: Persona,
}

/// What we know of a chat.
#[derive(Debug, Clone, Default)]
struct Chat {
    unread: bool,
    /// Group rooms: messages mentioning us since we last read. Friend
    /// chats: every unread message, counted as Steam's client counts them
    /// (and as Discord counts DMs), so DM badges show a number everywhere.
    mentions: u32,
    /// The newest message's time (orders Home).
    last: u32,
}

pub struct SteamAccount {
    slot: u32,
    /// The made-up device name of the kept sign-in ("" before one).
    device_name: String,
    net: Handle,
    events: mpsc::Receiver<Ev>,
    status: Status,
    account: Option<AccountId>,
    /// Our 64-bit id once connected (before AccountReady).
    me: Option<u64>,
    my_name: Option<String>,
    my_avatar: Option<String>,
    my_state: Option<u32>,
    /// Current activity from our own persona push, when another friend can join it.
    my_activity: Option<games::ActivityInvite>,
    /// Sent AccountReady already (for this connection).
    ready: bool,
    friends: HashMap<u64, Friend>,
    blocked: Vec<UserId>,
    /// Steam games' names by app id, looked up once each (personas name
    /// only non-Steam games), and the ones asked for.
    app_names: HashMap<u32, String>,
    asked_apps: HashSet<u32>,
    /// Messages whose game invite cards wait for a game's name.
    name_waits: Vec<games::NameWait>,
    /// Game libraries loaded for "Games in common", kept for the session
    /// (Steam is asked once per person), and the ones on their way.
    libraries: HashMap<u64, godwit_core::games::Library>,
    loading_libraries: HashSet<u64>,
    chats: HashMap<u64, Chat>,
    owned_emoticons: HashSet<String>,
    /// Reaction counts and own-reaction state by Steam message id.
    reactions: HashMap<(String, u32, u32), Vec<godwit_core::message::Reaction>>,
    /// The room on screen, if one of ours: its messages are read as they come.
    viewing: Option<u64>,
    profile_for: Option<u64>,
    profile_requested: HashSet<u64>,
    profile_details: HashMap<u64, people::ProfileDetails>,
    /// Latest typing notice per friend (stale stops are ignored).
    typing_seq: HashMap<u64, u64>,
    /// Throttles our own typing notices (Steam's client sends them seconds apart).
    typing_sent: HashMap<u64, std::time::Instant>,
    /// Microphone, speakers and processing for calls (from Settings).
    pub voice_settings: VoiceSettings,
    /// Our call: its tag and its room (a friend's DM or a group room).
    call: Option<(u64, RoomId)>,
    /// Group chats by id, as Steam last listed them.
    groups: HashMap<u64, GroupInfo>,
    /// What we know of each group room (group, chat).
    group_chats: HashMap<(u64, u64), Chat>,
    /// The group room on screen, if any.
    viewing_group: Option<(u64, u64)>,
    /// Group rooms with people in their voice chat (to clear when it empties).
    voiced: HashSet<(u64, u64)>,
    /// An invite we accepted, waiting for the group list to have its group:
    /// (code, group).
    joining: Option<(String, u64)>,
    /// The group room whose member list is open (sent again as names arrive).
    members_for: Option<(u64, u64)>,
    next_call: u64,
    /// Calls ringing us: Steam's voice chat id → the friend calling.
    ringing: HashMap<u64, u64>,
    /// Taken offline by the user (signed in, not connected).
    offline: bool,
    /// Started from a kept sign-in: show the account at once (offline, with
    /// its cached chats) rather than only once Steam answers, which can take
    /// a while when Steam resets the first connection.
    announce: bool,
}

impl SteamAccount {
    fn owned_emoticon(&self, name: &str) -> bool {
        self.owned_emoticons.contains(name)
    }

    /// The reactions to show on `line`. History and incoming lines carry
    /// Steam's current counts, so a non-empty list replaces what we kept;
    /// lines without any (live friend messages) fall back to the reaction
    /// notifications seen since, and nothing empty is kept.
    pub(super) fn reaction_list(&mut self, room: &RoomId, line: &Line) -> Vec<godwit_core::message::Reaction> {
        let key = (room.raw.to_string(), line.time, line.ordinal);
        if line.reactions.is_empty() {
            return self.reactions.get(&key).cloned().unwrap_or_default();
        }
        let list: Vec<_> = line
            .reactions
            .iter()
            .map(|r| godwit_core::message::Reaction {
                emoji: format!(":{}:", r.reaction).into(),
                image_url: Some(crate::bbcode::emoticon_url(&r.reaction).into()),
                count: r.count,
                me: r.me,
            })
            .collect();
        self.reactions.insert(key, list.clone());
        list
    }

    /// Applies one reaction notification to the message at `at`
    /// (Steam's `(server timestamp, ordinal)`) in `room`.
    fn update_reaction(
        &mut self,
        room: Option<RoomId>,
        at: (u32, u32),
        reactor: u64,
        name: String,
        add: bool,
        out: &mut Vec<Event>,
    ) {
        let Some(room) = room else { return };
        let (time, ordinal) = at;
        let key = (room.raw.to_string(), time, ordinal);
        let list = self.reactions.entry(key).or_default();
        let me = reactor == self.me.unwrap_or_default();
        if add {
            if let Some(existing) = list.iter_mut().find(|r| r.emoji.as_ref() == format!(":{name}:")) {
                existing.count = existing.count.saturating_add(1);
                existing.me |= me;
            } else {
                list.push(godwit_core::message::Reaction {
                    emoji: format!(":{name}:").into(),
                    image_url: Some(crate::bbcode::emoticon_url(&name).into()),
                    count: 1,
                    me,
                });
            }
        } else if let Some(existing) = list.iter_mut().find(|r| r.emoji.as_ref() == format!(":{name}:")) {
            existing.count = existing.count.saturating_sub(1);
            if me {
                existing.me = false;
            }
            list.retain(|r| r.count > 0);
        }
        let reactions = list.clone();
        if reactions.is_empty() {
            self.reactions.remove(&(room.raw.to_string(), time, ordinal));
        }
        out.push(Event::ReactionsChanged {
            room,
            id: message_id(time, ordinal),
            reactions,
        });
    }

    /// Starts the network thread and connects with the sign-in kept in
    /// `slot`, if there is one.
    pub fn start(wake: Waker, slot: u32) -> std::io::Result<Self> {
        let (net, events) = backend::spawn(wake)?;
        let mut device_name = String::new();
        let mut known = None;
        let status = match session::load(slot) {
            Some(saved) => {
                device_name = saved.device_name.clone();
                known = Some((saved.steam_id, saved.account_name.clone()));
                net.send(Cmd::UseSession(saved));
                Status::Connecting
            }
            None => Status::SignedOut { error: None },
        };
        let mut account = Self::with(slot, net, events, status);
        account.device_name = device_name;
        if let Some((steam_id, name)) = known.filter(|(id, _)| *id != 0) {
            account.me = Some(steam_id);
            account.account = Some(AccountId::new(Network::Steam, steam_id.to_string()));
            // The sign-in name until Steam sends our persona name.
            account.my_name = Some(name).filter(|n| !n.is_empty());
            account.announce = true;
        }
        Ok(account)
    }

    fn with(slot: u32, net: Handle, events: mpsc::Receiver<Ev>, status: Status) -> Self {
        Self {
            slot,
            device_name: String::new(),
            net,
            events,
            status,
            account: None,
            me: None,
            my_name: None,
            my_avatar: None,
            my_state: None,
            my_activity: None,
            ready: false,
            friends: HashMap::new(),
            blocked: Vec::new(),
            app_names: HashMap::new(),
            asked_apps: HashSet::new(),
            name_waits: Vec::new(),
            libraries: HashMap::new(),
            loading_libraries: HashSet::new(),
            chats: HashMap::new(),
            owned_emoticons: HashSet::new(),
            reactions: HashMap::new(),
            viewing: None,
            profile_for: None,
            profile_requested: HashSet::new(),
            profile_details: HashMap::new(),
            typing_seq: HashMap::new(),
            typing_sent: HashMap::new(),
            voice_settings: VoiceSettings::default(),
            call: None,
            groups: HashMap::new(),
            group_chats: HashMap::new(),
            viewing_group: None,
            voiced: HashSet::new(),
            joining: None,
            members_for: None,
            next_call: 0,
            ringing: HashMap::new(),
            announce: false,
            offline: false,
        }
    }

    /// An account that never connects (demo mode).
    pub fn inert() -> Self {
        let (net, _) = Handle::detached();
        let (_, events) = mpsc::channel();
        Self::with(0, net, events, Status::SignedOut { error: None })
    }

    pub fn slot(&self) -> u32 {
        self.slot
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    fn room_id(&self, friend: u64) -> Option<RoomId> {
        Some(RoomId::new(self.account.clone()?, friend.to_string()))
    }
}

impl NetworkAccount for SteamAccount {
    fn network(&self) -> Network {
        Network::Steam
    }

    fn id(&self) -> Option<&AccountId> {
        self.account.as_ref().filter(|_| self.ready)
    }

    /// Commands still reach the backend while reconnecting or waiting for
    /// personas; it reports a send it can't make as failed.
    fn route_id(&self) -> Option<&AccountId> {
        self.account.as_ref()
    }

    fn can_go_offline(&self) -> bool {
        true
    }

    fn set_offline(&mut self, offline: bool) -> Vec<Event> {
        let mut out = Vec::new();
        if offline == self.offline {
            return out;
        }
        self.offline = offline;
        self.net.send(Cmd::SetOffline(offline));
        if offline {
            for (_, partner) in std::mem::take(&mut self.ringing) {
                if let Some(room) = self.room_id(partner) {
                    out.push(Event::VoiceOccupants {
                        room: room.clone(),
                        members: Vec::new(),
                    });
                    out.push(Event::AlertDone { room });
                }
            }
            if let Some(account) = self.account.clone() {
                out.push(Event::Connection { account, online: false });
            }
        }
        out
    }

    fn connecting(&self) -> bool {
        self.id().is_none() && matches!(self.status(), Status::Connecting)
    }

    fn poll(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        if std::mem::take(&mut self.announce) && !self.ready {
            self.make_ready(&mut out);
            // Shown, but not connected yet: connecting goes on.
            self.ready = false;
            self.status = Status::Connecting;
            if let Some(account) = self.account.clone() {
                out.push(Event::Connection { account, online: false });
            }
        }
        godwit_core::backend::poll(self, |s| s.events.try_recv().ok(), Self::handle, &mut out);
        out
    }

    fn run(&mut self, command: Command) -> Vec<Event> {
        let mut out = Vec::new();
        match command {
            // Steam messages have no interactive component controls.
            Command::ActivateMessageButton { .. } => {}
            Command::CreateGroup { name, users } => {
                let friends: Vec<u64> = users.into_iter().filter_map(|user| user.raw.parse().ok()).collect();
                if friends.len() >= 2 {
                    self.net.send(Cmd::GroupCreate { name, friends });
                }
            }
            Command::LeaveGroup { room } => {
                if let Some((group, _)) = parse_group_room(&room.raw) {
                    self.net.send(Cmd::GroupLeave { group });
                }
            }
            Command::AddGroupMember { room, user } => {
                if let (Some((group, _)), Ok(friend)) = (parse_group_room(&room.raw), user.raw.parse::<u64>()) {
                    self.net.send(Cmd::GroupInvite { group, friend });
                }
            }
            Command::SaveServerOverview { server, edit, .. } => {
                if let (Some(name), Some(group)) =
                    (edit.name, server.raw.strip_prefix('g').and_then(|v| v.parse().ok()))
                {
                    self.net.send(Cmd::GroupRename {
                        group,
                        name: name.to_string(),
                    });
                }
            }
            Command::LoadServerOverview { server, request } => {
                if let (Some(account), Some(group)) = (
                    self.account.clone(),
                    server.raw.strip_prefix('g').and_then(|v| v.parse::<u64>().ok()),
                ) && let Some(info) = self.groups.get(&group)
                {
                    let owner = info.ranks.get(&crate::backend::account_id(self.me.unwrap_or_default())) == Some(&50);
                    out.push(Event::ServerOverview {
                        server,
                        request,
                        result: Ok(godwit_core::server_settings::ServerOverview {
                            name: self.group_title(group).into(),
                            can_manage: owner,
                            owner,
                            channels: info
                                .rooms
                                .iter()
                                .map(|r| {
                                    (
                                        RoomId::new(account.clone(), group_room_raw(group, r.chat)),
                                        r.name.clone().into(),
                                    )
                                })
                                .collect(),
                            ..Default::default()
                        }),
                    });
                }
            }
            Command::ServerAdmin {
                server,
                op: godwit_core::server_settings::ServerAdmin::Leave,
            } => {
                if let Some(group) = server.raw.strip_prefix('g').and_then(|v| v.parse().ok()) {
                    self.net.send(Cmd::GroupLeave { group });
                }
            }
            // Steam has no channel webhook manager.
            Command::LoadWebhooks { .. } | Command::ChangeWebhook { .. } => {}
            Command::SendFriendRequest { request, name } => self.net.send(Cmd::AddFriend {
                request: Some(request),
                name,
            }),
            Command::AddFriend { user } => self.net.send(Cmd::AddFriend {
                request: None,
                name: user.raw.to_string(),
            }),
            Command::SetFavourite { user, favourite } => {
                if let Ok(friend) = user.raw.parse::<u64>() {
                    self.net.send(Cmd::SetFavourite {
                        friend: crate::backend::account_id(friend),
                        favourite,
                    });
                }
            }
            Command::SetNickname { user, nickname } => {
                if let Ok(friend) = user.raw.parse::<u64>() {
                    self.net.send(Cmd::SetNickname { friend, nickname });
                }
            }
            Command::RemoveFriend { user } => {
                if let Ok(friend) = user.raw.parse::<u64>() {
                    self.net.send(Cmd::RemoveFriend(friend));
                }
            }
            Command::BlockUser { user } if self.me != user.raw.parse::<u64>().ok() => {
                if let Ok(steamid) = user.raw.parse::<u64>() {
                    self.net.send(Cmd::IgnoreFriend {
                        steamid,
                        unignore: false,
                    });
                }
            }
            Command::UnblockUser { user } if self.me != user.raw.parse::<u64>().ok() => {
                if let Ok(steamid) = user.raw.parse::<u64>() {
                    self.net.send(Cmd::IgnoreFriend {
                        steamid,
                        unignore: true,
                    });
                }
            }
            Command::LoadGameLibraries { users } => {
                for user in users {
                    let Ok(id) = user.raw.parse::<u64>() else { continue };
                    if let Some(library) = self.libraries.get(&id) {
                        out.push(Event::GameLibrary {
                            user,
                            library: library.clone(),
                        });
                    } else if self.loading_libraries.insert(id) {
                        self.net.send(Cmd::OwnedGames(id));
                    }
                }
            }
            Command::LoadProfile { user, .. } => {
                if let Ok(id) = user.raw.parse::<u64>() {
                    self.profile_for = Some(id);
                    if let Some(profile) = self.profile(id) {
                        out.push(Event::Profile(profile));
                    }
                    self.net.send(Cmd::RequestPersonas(vec![id]));
                    if self.profile_requested.insert(id) {
                        self.net.send(Cmd::ProfileDetails(id));
                    }
                }
            }
            Command::InviteToActivity { user } => {
                if self.account.as_ref() == Some(&user.account)
                    && let Ok(friend) = user.raw.parse::<u64>()
                    && let Some((app, connect, lobby)) = &self.my_activity
                    && let Some(text) = games::invite_message(*app, connect.as_deref(), *lobby)
                {
                    self.net.send(Cmd::SendInvite { friend, text });
                }
            }
            Command::Focus { room } => {
                self.viewing_group = room.as_ref().and_then(group_of);
                if let Some(key) = self.viewing_group
                    && let Some(chat) = self.group_chats.get_mut(&key)
                    && (chat.unread || chat.mentions > 0)
                {
                    chat.unread = false;
                    chat.mentions = 0;
                    let time = chat.last;
                    self.net.send(Cmd::GroupAck {
                        group: key.0,
                        chat: key.1,
                        time,
                    });
                    if let Some(r) = self.group_room(key.0, key.1) {
                        out.push(Event::RoomUpdated(r));
                    }
                }
                self.viewing = room.as_ref().and_then(Self::friend_of);
                if let Some(friend) = self.viewing
                    && let Some(chat) = self.chats.get_mut(&friend)
                    && (chat.unread || chat.mentions > 0)
                {
                    chat.unread = false;
                    chat.mentions = 0;
                    let last = chat.last;
                    self.net.send(Cmd::Ack { friend, time: last });
                    if let Some(r) = self.room(friend) {
                        out.push(Event::RoomUpdated(r));
                    }
                }
            }
            Command::SendMessage {
                room,
                text,
                nonce,
                files,
                ..
            } => {
                if text.trim().is_empty() && files.is_empty() {
                    return out;
                }
                if let Some((group, chat)) = group_of(&room) {
                    self.net.send(Cmd::GroupSend {
                        group,
                        chat,
                        text,
                        files,
                        nonce: nonce.to_string(),
                    });
                } else if let Some(friend) = Self::friend_of(&room) {
                    self.net.send(Cmd::Send {
                        friend,
                        text,
                        files,
                        nonce: nonce.to_string(),
                    });
                }
            }
            Command::React {
                room,
                message,
                emoji,
                add,
            } => {
                if self.account.as_ref() != Some(&room.account) {
                    return out;
                }
                let Some((time, ordinal)) = parse_message_id(&message) else {
                    return out;
                };
                let Some(name) = emoji.strip_prefix(':').and_then(|s| s.strip_suffix(':')) else {
                    return out;
                };
                if name.is_empty() || !self.owned_emoticon(name) {
                    return out;
                }
                if let Some((group, chat)) = group_of(&room) {
                    self.net.send(Cmd::GroupReact {
                        group,
                        chat,
                        time,
                        ordinal,
                        reaction: name.to_owned(),
                        add,
                    });
                } else if let Some(friend) = Self::friend_of(&room) {
                    self.net.send(Cmd::React {
                        friend,
                        time,
                        ordinal,
                        reaction: name.to_owned(),
                        add,
                    });
                }
                return out;
            }
            Command::OpenDirect { user } => {
                if let Ok(friend) = user.raw.parse::<u64>() {
                    self.chats.entry(friend).or_default();
                    if let Some(room) = self.room(friend) {
                        let id = room.id.clone();
                        out.push(Event::RoomUpdated(room));
                        out.push(Event::RoomOpened(id));
                    }
                }
            }
            Command::Typing { room } => {
                if let Some(friend) = Self::friend_of(&room) {
                    let now = std::time::Instant::now();
                    let due = self
                        .typing_sent
                        .get(&friend)
                        .is_none_or(|at| now.duration_since(*at) > std::time::Duration::from_secs(5));
                    if due {
                        self.typing_sent.insert(friend, now);
                        self.net.send(Cmd::Typing { friend });
                    }
                }
            }
            Command::LoadHistory { room, before } => {
                if let Some((group, chat)) = group_of(&room) {
                    let before = before.as_ref().and_then(parse_message_id);
                    self.net.send(Cmd::GroupHistory { group, chat, before });
                } else if let Some(friend) = Self::friend_of(&room) {
                    let before = before.as_ref().and_then(parse_message_id);
                    self.net.send(Cmd::History { friend, before });
                }
            }
            Command::MarkRead { room, last } => {
                if let Some((group, chat)) = group_of(&room)
                    && let Some((time, _)) = parse_message_id(&last)
                {
                    if let Some(c) = self.group_chats.get_mut(&(group, chat)) {
                        c.unread = false;
                        c.mentions = 0;
                    }
                    self.net.send(Cmd::GroupAck { group, chat, time });
                } else if let Some(friend) = Self::friend_of(&room)
                    && let Some((time, _)) = parse_message_id(&last)
                {
                    if let Some(chat) = self.chats.get_mut(&friend) {
                        chat.unread = false;
                        chat.mentions = 0;
                    }
                    self.net.send(Cmd::Ack { friend, time });
                }
            }
            Command::MarkRoomRead { room } => {
                if let Some((group, chat)) = group_of(&room) {
                    if let Some(c) = self.group_chats.get_mut(&(group, chat)) {
                        let time = c.last;
                        c.unread = false;
                        c.mentions = 0;
                        if time > 0 {
                            self.net.send(Cmd::GroupAck { group, chat, time });
                        }
                    }
                } else if let Some(friend) = Self::friend_of(&room)
                    && let Some(c) = self.chats.get_mut(&friend)
                {
                    let time = c.last;
                    c.unread = false;
                    c.mentions = 0;
                    if time > 0 {
                        self.net.send(Cmd::Ack { friend, time });
                    }
                }
            }
            Command::SetPresence(p) => {
                let state = match p {
                    Presence::Idle => 3,
                    Presence::Busy | Presence::Focus => 2,
                    Presence::Invisible => 7,
                    Presence::Offline => 0,
                    Presence::Online | Presence::Unknown => 1,
                };
                self.net.send(Cmd::SetPersona(state));
                if let Some(account) = self.account.clone() {
                    out.push(Event::OwnPresence {
                        account,
                        presence: p,
                        status: None,
                    });
                }
            }
            Command::JoinCall { room } => {
                if let Some((group, chat)) = group_of(&room) {
                    self.next_call = self.next_call.wrapping_add(1);
                    self.call = Some((self.next_call, room));
                    self.net.send(Cmd::GroupCall {
                        call: self.next_call,
                        group,
                        chat,
                        settings: Box::new(self.voice_settings.clone()),
                    });
                    return out;
                }
                let Some(friend) = Self::friend_of(&room) else {
                    return out;
                };
                // Their call ringing us: joining answers it.
                let incoming = self
                    .ringing
                    .iter()
                    .find(|(_, partner)| **partner == friend)
                    .map(|(voice_chat, _)| *voice_chat);
                if let Some(voice_chat) = incoming {
                    self.ringing.remove(&voice_chat);
                    out.push(Event::VoiceOccupants {
                        room: room.clone(),
                        members: Vec::new(),
                    });
                    out.push(Event::AlertDone { room: room.clone() });
                }
                self.next_call = self.next_call.wrapping_add(1);
                if let Some(room) = self.room_id(friend) {
                    self.call = Some((self.next_call, room));
                }
                self.net.send(Cmd::Call {
                    call: self.next_call,
                    partner: friend,
                    incoming,
                    settings: Box::new(self.voice_settings.clone()),
                });
            }
            Command::DeclineCall { room } => {
                let Some(friend) = Self::friend_of(&room) else {
                    return out;
                };
                let calls: Vec<u64> = self
                    .ringing
                    .iter()
                    .filter(|(_, partner)| **partner == friend)
                    .map(|(voice_chat, _)| *voice_chat)
                    .collect();
                for voice_chat in calls {
                    self.ringing.remove(&voice_chat);
                    self.net.send(Cmd::DeclineCall {
                        voice_chat,
                        partner: friend,
                    });
                }
                out.push(Event::VoiceOccupants {
                    room: room.clone(),
                    members: Vec::new(),
                });
                out.push(Event::AlertDone { room });
            }
            Command::LoadMembers { room } => {
                self.members_for = group_of(&room);
                if let Some((group, chat)) = self.members_for {
                    let unknown: Vec<u64> = self
                        .groups
                        .get(&group)
                        .map(|g| {
                            g.members
                                .iter()
                                .map(|a| backend::steam_id(*a))
                                .filter(|id| {
                                    Some(*id) != self.me
                                        && !self.friends.get(id).is_some_and(|f| f.persona.name.is_some())
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    if !unknown.is_empty() {
                        self.net.send(Cmd::RequestPersonas(unknown));
                    }
                    if let Some(event) = self.group_members(group, chat) {
                        out.push(event);
                    }
                }
            }
            Command::LoadInvite { code } => match invite_target(&code) {
                Some((group, _)) => self.net.send(Cmd::InviteSummary {
                    code: code.to_string(),
                    group,
                }),
                None => {
                    if let Some(account) = self.account.clone() {
                        out.push(Event::InviteLoaded {
                            account,
                            code,
                            preview: None,
                        });
                    }
                }
            },
            Command::JoinInvite { code } => {
                if let Some((group, chat)) = invite_target(&code) {
                    self.net.send(Cmd::JoinGroup {
                        code: code.to_string(),
                        group,
                        chat,
                    });
                }
            }
            Command::LeaveCall => self.net.send(Cmd::CallCmd(SessionCmd::Leave)),
            Command::RemoveGroupMember { .. } | Command::JoinThread { .. } | Command::LeaveThread { .. } => {}
            Command::SetMuted(muted) => self.net.send(Cmd::CallCmd(SessionCmd::Muted(muted))),
            Command::SetDeafened(deafened) => self.net.send(Cmd::CallCmd(SessionCmd::Deafened(deafened))),
            // Steam's calls are voice only.
            Command::SetCamera(_) => {
                if let Some((_, room)) = self.call.clone() {
                    out.push(Event::CallCamera { room, active: false });
                }
            }
            Command::SetScreenShare(_) => {
                if let Some((_, room)) = self.call.clone() {
                    out.push(Event::CallScreenShare { room, active: false });
                }
            }
            // Other commands are not implemented by this backend yet.
            Command::CreateServer { .. }
            | Command::LoadServerEvents { .. }
            | Command::SetServerEventInterest { .. } => {}
            Command::LoadServerProfile { .. } | Command::SaveServerProfile { .. } => {}
            _ => {}
        }
        out
    }

    fn message_link(&self, _room: &RoomId, _message: &MessageId) -> Option<String> {
        None
    }

    fn log_out(&mut self) {
        self.net.send(Cmd::Logout);
    }
}

/// A message's id: its time and ordinal (how Steam pages history).
fn message_id(time: u32, ordinal: u32) -> MessageId {
    MessageId(format!("{time}-{ordinal}").into())
}

fn parse_message_id(id: &MessageId) -> Option<(u32, u32)> {
    let (time, ordinal) = id.0.split_once('-')?;
    Some((time.parse().ok()?, ordinal.parse().ok()?))
}

/// The picture for an avatar hash (Steam's CDN; its default without one).
fn avatar_url(hash: &[u8]) -> String {
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    let hex = if hex.is_empty() || hex.bytes().all(|b| b == b'0') {
        DEFAULT_AVATAR.to_owned()
    } else {
        hex
    };
    format!("https://avatars.steamstatic.com/{hex}_medium.jpg")
}

/// Steam's persona state as presence.
fn presence(state: u32) -> Presence {
    match state {
        0 => Presence::Offline,
        2 => Presence::Busy,
        3 | 4 => Presence::Idle,
        7 => Presence::Invisible,
        _ => Presence::Online,
    }
}

/// "Playing …", as Steam's friends list says it.
fn playing(p: &Persona) -> Option<String> {
    match (&p.game_name, p.game_app) {
        (Some(name), _) => Some(format!("Playing {name}")),
        (None, Some(_)) => Some("In a game".into()),
        _ => None,
    }
}

/// Persona updates only carry what changed: keep the rest.
fn merge(into: &mut Persona, new: Persona) {
    into.steam_id = new.steam_id;
    if new.name.is_some() {
        into.name = new.name;
    }
    if new.state.is_some() {
        into.state = new.state;
        // A state without a game means the game ended.
        into.game_name = new.game_name;
        into.game_app = new.game_app;
        into.activity_join = new.activity_join;
        into.activity_invite = new.activity_invite;
    }
    if new.avatar_hash.is_some() {
        into.avatar_hash = new.avatar_hash;
    }
}

fn guard_hint(guard: &Guard) -> String {
    match guard {
        Guard::EmailCode { hint } if !hint.is_empty() => {
            format!("Enter the code Steam emailed to your address at {hint}.")
        }
        Guard::EmailCode { .. } => "Enter the code Steam emailed you.".into(),
        Guard::DeviceCode => "Enter the code from the Steam mobile app.".into(),
        Guard::DeviceConfirmation => "Approve the sign-in in the Steam mobile app.".into(),
        Guard::EmailConfirmation => "Approve the sign-in with the link Steam emailed you.".into(),
        Guard::None => String::new(),
    }
}

/// A group room's id in Godwit: `g<group>-<chat>` (friends' are their Steam id).
fn group_room_raw(group: u64, chat: u64) -> String {
    format!("g{group}-{chat}")
}

fn group_server_raw(group: u64) -> String {
    format!("g{group}")
}

fn parse_group_room(raw: &str) -> Option<(u64, u64)> {
    let (group, chat) = raw.strip_prefix('g')?.split_once('-')?;
    Some((group.parse().ok()?, chat.parse().ok()?))
}

/// The (group, chat) an invite code names (`g<group>-<chat>`, chat 0: the default).
fn invite_target(code: &str) -> Option<(u64, u64)> {
    let (group, chat) = code.strip_prefix('g')?.split_once('-')?;
    Some((group.parse().ok()?, chat.parse().ok()?))
}

/// Logs the shape (tag and attribute names, no content) of markup Godwit
/// doesn't show yet, so new kinds of Steam messages can be added.
fn log_unknown_markup(text: &str) {
    for tag in crate::bbcode::convert(text).unknown {
        crate::backend::diag(&format!("markup Godwit doesn't know: {tag}"));
    }
}

/// The (group, chat) of a group room.
fn group_of(room: &RoomId) -> Option<(u64, u64)> {
    let (group, chat) = room.raw.strip_prefix('g')?.split_once('-')?;
    Some((group.parse().ok()?, chat.parse().ok()?))
}

/// Pictures posted into a chat, as image attachments.
fn images(list: Vec<(String, Option<(u32, u32)>)>) -> Vec<Attachment> {
    list.into_iter()
        .map(|(url, dimensions)| Attachment {
            // Steam's uploads end in `/`: the last named part, else a plain name.
            name: url
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|n| n.contains('.'))
                .unwrap_or("image")
                .into(),
            url: url.into(),
            kind: AttachmentKind::Image,
            size: 0,
            dimensions,
            original_url: None,
        })
        .collect()
}

/// The picker's Steam groups: emoticons write `:name:` (Steam turns it into
/// the emoticon when we own it), stickers write `/sticker <name>`, which
/// Steam sends as the sticker when it's the whole message (as its clients'
/// `/sticker` command; vaporchat's `sendMessage`). Steam has no servers: the
/// groups hang off made-up ones that no room belongs to.
fn emoji_groups(account: &AccountId, emoticons: &[String], stickers: &[String]) -> Vec<EmojiGroup> {
    let mut groups = Vec::new();
    if !emoticons.is_empty() {
        groups.push(EmojiGroup {
            server: ServerId::new(account.clone(), "steam-emoticons"),
            title: "Steam emoticons".into(),
            emoji: emoticons
                .iter()
                .map(|name| CustomEmoji {
                    name: name.as_str().into(),
                    image_url: crate::bbcode::emoticon_url(name).into(),
                    reaction: format!(":{name}:").into(),
                    code: format!(":{name}:").into(),
                })
                .collect(),
        });
    }
    if !stickers.is_empty() {
        groups.push(EmojiGroup {
            server: ServerId::new(account.clone(), "steam-stickers"),
            title: "Steam stickers".into(),
            emoji: stickers
                .iter()
                .map(|name| CustomEmoji {
                    name: name.as_str().into(),
                    image_url: crate::bbcode::sticker_url(name).into(),
                    reaction: name.as_str().into(),
                    code: format!("/sticker {name}").into(),
                })
                .collect(),
        });
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected() -> (SteamAccount, mpsc::Sender<Ev>) {
        let (net, _cmds) = Handle::detached();
        let (tx, events) = mpsc::channel();
        let mut a = SteamAccount::with(0, net, events, Status::Connecting);
        tx.send(Ev::Connected {
            me: backend::steam_id(1),
        })
        .unwrap();
        a.poll();
        (a, tx)
    }

    fn ready_with_commands() -> (
        SteamAccount,
        mpsc::Sender<Ev>,
        tokio::sync::mpsc::UnboundedReceiver<Cmd>,
    ) {
        let (net, commands) = Handle::detached();
        let (tx, events) = mpsc::channel();
        let mut a = SteamAccount::with(0, net, events, Status::Connecting);
        tx.send(Ev::Connected {
            me: backend::steam_id(1),
        })
        .unwrap();
        tx.send(Ev::ReadyDeadline).unwrap();
        a.poll();
        (a, tx, commands)
    }

    fn group(id: u64, clan: Option<u32>, rooms: &[(u64, &str, bool)]) -> GroupInfo {
        GroupInfo {
            id,
            name: if clan.is_some() { "Clan".into() } else { String::new() },
            clan,
            avatar_url: None,
            members: vec![1, 2, 3],
            ranks: std::collections::BTreeMap::new(),
            default_chat: rooms[0].0,
            rooms: rooms
                .iter()
                .map(|(chat, name, voice)| crate::groups::GroupRoom {
                    chat: *chat,
                    name: (*name).into(),
                    voice_allowed: *voice,
                    in_voice: if *voice { vec![2] } else { Vec::new() },
                    ..Default::default()
                })
                .collect(),
        }
    }

    #[test]
    fn groups_become_servers_or_group_dms_and_route_commands() {
        let (mut a, tx, mut commands) = ready_with_commands();
        tx.send(Ev::Groups(vec![
            group(10, Some(5), &[(100, "General", false), (101, "Voice", true)]),
            group(20, None, &[(200, "General", true)]),
        ]))
        .unwrap();
        let events = a.poll();
        let servers = events.iter().find_map(|e| match e {
            Event::ServersChanged { servers, .. } => Some(servers.clone()),
            _ => None,
        });
        let servers = servers.expect("servers sent");
        assert_eq!(servers.len(), 1, "only the community group is a server");
        assert_eq!(&*servers[0].name, "Clan");
        assert_eq!(servers[0].categories[0].rooms.len(), 2);
        let rooms: Vec<&Room> = events
            .iter()
            .filter_map(|e| match e {
                Event::RoomUpdated(r) => Some(r),
                _ => None,
            })
            .collect();
        let adhoc = rooms.iter().find(|r| &*r.id.raw == "g20-200").expect("group dm");
        assert_eq!(adhoc.kind, RoomKind::Group);
        assert!(adhoc.capabilities.leave);
        assert!(adhoc.capabilities.voice);
        let voice = rooms.iter().find(|r| &*r.id.raw == "g10-101").expect("channel");
        assert!(matches!(voice.kind, RoomKind::Channel { .. }));
        assert!(!voice.capabilities.leave);
        assert!(events.iter().any(
            |e| matches!(e, Event::VoiceOccupants { room, members } if &*room.raw == "g10-101" && members.len() == 1)
        ));

        while commands.try_recv().is_ok() {}
        let room = adhoc.id.clone();
        a.run(Command::SendMessage {
            room: room.clone(),
            text: "hi all".into(),
            nonce: "n".into(),
            reply_to: None,
            files: Vec::new(),
        });
        assert!(matches!(
            commands.try_recv(),
            Ok(Cmd::GroupSend {
                group: 20,
                chat: 200,
                ..
            })
        ));
        a.run(Command::JoinCall { room: room.clone() });
        assert!(matches!(
            commands.try_recv(),
            Ok(Cmd::GroupCall {
                group: 20,
                chat: 200,
                ..
            })
        ));
        a.run(Command::LeaveGroup {
            room: RoomId::new(room.account.clone(), "g20-200"),
        });
        assert!(matches!(commands.try_recv(), Ok(Cmd::GroupLeave { group: 20 })));
        a.run(Command::AddGroupMember {
            room: RoomId::new(room.account.clone(), "g20-200"),
            user: UserId::new(room.account.clone(), backend::steam_id(4).to_string()),
        });
        assert!(
            matches!(commands.try_recv(), Ok(Cmd::GroupInvite { group: 20, friend }) if friend == backend::steam_id(4))
        );
    }

    #[test]
    fn group_refresh_notification_updates_the_displayed_name() {
        let (mut a, tx, _) = ready_with_commands();
        tx.send(Ev::Groups(vec![group(20, None, &[(200, "General", false)])]))
            .unwrap();
        a.poll();
        let mut renamed = group(20, None, &[(200, "General", false)]);
        renamed.name = "Renamed group".into();
        tx.send(Ev::Groups(vec![renamed])).unwrap();
        let events = a.poll();
        assert!(events.iter().any(|e| matches!(e, Event::RoomUpdated(room)
            if &*room.id.raw == "g20-200" && &*room.name == "Renamed group")));
    }

    #[test]
    fn steam_nickname_list_maps_to_core_event_and_set_command() {
        let (mut account, tx, mut commands) = ready_with_commands();
        let friend = backend::steam_id(7);
        tx.send(Ev::Nicknames(vec![(backend::account_id(friend), "Alex".into())]))
            .unwrap();
        let events = account.poll();
        let self_id = AccountId::new(Network::Steam, backend::steam_id(1).to_string());
        assert!(events.iter().any(|event| matches!(event,
            Event::Nicknames { account, nicknames }
                if account == &self_id
                    && nicknames == &vec![(UserId::new(self_id.clone(), friend.to_string()), "Alex".into())]
        )));
        account.run(Command::SetNickname {
            user: UserId::new(self_id, friend.to_string()),
            nickname: "Ace".into(),
        });
        assert!(
            matches!(commands.try_recv(), Ok(Cmd::SetNickname { friend: id, nickname }) if id == friend && nickname == "Ace")
        );
    }

    #[test]
    fn group_messages_count_unread_and_mentions() {
        let (net, _commands) = Handle::detached();
        let (tx, events) = mpsc::channel();
        let mut a = SteamAccount::with(0, net, events, Status::Connecting);
        tx.send(Ev::Connected {
            me: backend::steam_id(1),
        })
        .unwrap();
        tx.send(Ev::ReadyDeadline).unwrap();
        tx.send(Ev::Groups(vec![group(10, Some(5), &[(100, "General", false)])]))
            .unwrap();
        a.poll();
        tx.send(Ev::GroupIncoming {
            group: 10,
            chat: 100,
            line: Line {
                from: 2,
                time: 50,
                ordinal: 0,
                text: "hey @you".into(),
                reactions: Vec::new(),
            },
            mentioned: true,
        })
        .unwrap();
        let events = a.poll();
        assert!(events.iter().any(|e| matches!(e, Event::MessageAdded { .. })));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::RoomUpdated(r) if r.unread && r.mentions == 1))
        );
        assert!(
            events.iter().any(|e| matches!(e, Event::Alert { .. })),
            "a mention alerts, even in a server"
        );
    }

    #[test]
    fn a_game_invite_shows_a_card_named_once_the_game_is_known() {
        use godwit_core::message::EmbedStyle;
        let (mut a, tx, mut commands) = ready_with_commands();
        let friend = backend::steam_id(2);
        tx.send(Ev::Incoming {
            friend,
            mine: false,
            line: Line {
                from: 2,
                time: 10,
                ordinal: 0,
                text: r#"[lobbyinvite appid="440" lobbyid="9"][/lobbyinvite]"#.into(),
                reactions: Vec::new(),
            },
        })
        .unwrap();
        let events = a.poll();
        let card = events.iter().find_map(|e| match e {
            Event::MessageAdded { message, .. } => message.embeds.first().cloned(),
            _ => None,
        });
        let card = card.expect("a card");
        assert_eq!(card.style, EmbedStyle::Activity);
        assert_eq!(card.title.as_deref(), Some("A Steam game"));
        assert_eq!(
            card.url.as_deref(),
            Some(format!("steam://joinlobby/440/9/{friend}").as_str())
        );
        let mut asked = false;
        while let Ok(c) = commands.try_recv() {
            asked |= matches!(c, Cmd::AppNames(ref apps) if apps.contains(&440));
        }
        assert!(asked, "the game's name is looked up");
        tx.send(Ev::AppNames(vec![(440, "A Game".into())])).unwrap();
        let events = a.poll();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::EmbedsUpdated { embeds, .. } if embeds[0].title.as_deref() == Some("A Game")
        )));
        assert!(a.name_waits.is_empty());
    }

    #[test]
    fn a_group_chat_invite_shows_a_card_and_joining_opens_it() {
        let (mut a, tx, mut commands) = ready_with_commands();
        let friend = backend::steam_id(2);
        tx.send(Ev::Incoming {
            friend,
            mine: false,
            line: Line {
                from: 2,
                time: 10,
                ordinal: 0,
                text: r#"[invite chatgroupid="20" chatid="200"][/invite]"#.into(),
                reactions: Vec::new(),
            },
        })
        .unwrap();
        let events = a.poll();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::MessageAdded { message, .. } if message.invites.iter().any(|c| &**c == "g20-200")
        )));

        while commands.try_recv().is_ok() {}
        a.run(Command::LoadInvite { code: "g20-200".into() });
        assert!(matches!(commands.try_recv(), Ok(Cmd::InviteSummary { group: 20, .. })));
        a.run(Command::JoinInvite { code: "g20-200".into() });
        assert!(matches!(
            commands.try_recv(),
            Ok(Cmd::JoinGroup {
                group: 20,
                chat: 200,
                ..
            })
        ));
        tx.send(Ev::Groups(vec![group(20, None, &[(200, "General", false)])]))
            .unwrap();
        tx.send(Ev::GroupJoined {
            code: "g20-200".into(),
            group: 20,
            result: Ok(()),
        })
        .unwrap();
        let events = a.poll();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::RoomOpened(r) if &*r.raw == "g20-200")),
            "joining an ad-hoc group chat opens it"
        );
    }

    #[test]
    fn steam_reaction_notification_updates_core_reaction_state() {
        let (mut a, tx, _) = ready_with_commands();
        let friend = backend::steam_id(2);
        tx.send(Ev::Reaction {
            friend,
            time: 41,
            ordinal: 3,
            reactor: backend::steam_id(2),
            reaction: "steamhappy".into(),
            add: true,
        })
        .unwrap();
        let events = a.poll();
        assert!(events.iter().any(|e| matches!(e, Event::ReactionsChanged {
            id, reactions, ..
        } if id == &message_id(41, 3) && reactions.len() == 1 && reactions[0].count == 1 && !reactions[0].me)));
    }

    #[test]
    fn steam_room_capabilities_disable_unrepresentable_reactions_and_replies() {
        let caps = Capabilities::of(Network::Steam);
        assert!(!caps.react);
        assert!(!caps.reply);
    }

    #[test]
    fn steam_reaction_command_requires_owned_emoticon_and_maps_timestamp_ordinal() {
        let (mut a, tx, mut commands) = ready_with_commands();
        tx.send(Ev::Emoticons {
            emoticons: vec!["steamhappy".into()],
            stickers: Vec::new(),
        })
        .unwrap();
        a.poll();
        let account = a.account.clone().unwrap();
        let room = RoomId::new(account, backend::steam_id(2).to_string());
        a.run(Command::React {
            room: room.clone(),
            message: message_id(42, 7),
            emoji: ":steamhappy:".into(),
            add: true,
        });
        assert!(
            matches!(commands.try_recv(), Ok(Cmd::React { friend, time: 42, ordinal: 7, reaction, add: true }) if friend == backend::steam_id(2) && reaction == "steamhappy")
        );
        a.run(Command::React {
            room,
            message: message_id(42, 7),
            emoji: "🙂".into(),
            add: true,
        });
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn a_group_rooms_member_list_lists_its_members() {
        let (net, _commands) = Handle::detached();
        let (tx, events) = mpsc::channel();
        let mut a = SteamAccount::with(0, net, events, Status::Connecting);
        tx.send(Ev::Connected {
            me: backend::steam_id(1),
        })
        .unwrap();
        tx.send(Ev::ReadyDeadline).unwrap();
        tx.send(Ev::Groups(vec![group(10, Some(5), &[(100, "General", false)])]))
            .unwrap();
        a.poll();
        let room = RoomId::new(AccountId::new(Network::Steam, "76561197960265729"), "g10-100");
        let room = RoomId::new(a.account.clone().unwrap_or(room.account.clone()), room.raw.as_ref());
        let events = a.run(Command::LoadMembers { room });
        let Some(Event::Members { groups, .. }) = events.first() else {
            panic!("expected members, got {events:?}")
        };
        let total: usize = groups.iter().map(|g| g.members.len()).sum();
        assert_eq!(total, 3, "every member the group listed");
    }

    #[test]
    fn group_room_ids_round_trip() {
        let room = RoomId::new(AccountId::new(Network::Steam, "1"), group_room_raw(7, 9));
        assert_eq!(group_of(&room), Some((7, 9)));
        assert_eq!(group_of(&RoomId::new(AccountId::new(Network::Steam, "1"), "123")), None);
    }

    #[test]
    fn owned_emoticons_and_stickers_fill_the_picker() {
        let account = AccountId::new(Network::Steam, "1");
        let groups = emoji_groups(&account, &["steamhappy".to_owned()], &["Goose Chase".to_owned()]);
        assert_eq!(groups.len(), 2);
        assert_eq!(&*groups[0].emoji[0].code, ":steamhappy:");
        assert!(groups[0].emoji[0].image_url.ends_with("/economy/emoticon/steamhappy"));
        assert_eq!(&*groups[1].emoji[0].code, "/sticker Goose Chase");
        assert!(groups[1].emoji[0].image_url.ends_with("/economy/sticker/Goose%20Chase"));
        assert!(emoji_groups(&account, &[], &[]).is_empty());
    }

    #[test]
    fn declining_turns_the_ringing_call_down_on_steam() {
        let (mut a, tx, mut commands) = ready_with_commands();
        let friend = backend::steam_id(2);
        let room = a.room_id(friend).unwrap();
        tx.send(Ev::IncomingCall {
            voice_chat: 77,
            partner: friend,
        })
        .unwrap();
        a.poll();
        while commands.try_recv().is_ok() {}

        let events = a.run(Command::DeclineCall { room: room.clone() });
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::AlertDone { room: r } if *r == room))
        );
        assert!(matches!(
            commands.try_recv(),
            Ok(Cmd::DeclineCall { voice_chat: 77, partner }) if partner == friend
        ));
        assert!(a.ringing.is_empty());
    }

    #[test]
    fn a_ringing_call_shows_in_the_dm_and_joining_answers_it() {
        let (mut a, tx, mut commands) = ready_with_commands();
        let friend = backend::steam_id(2);
        let room = a.room_id(friend).unwrap();

        tx.send(Ev::IncomingCall {
            voice_chat: 77,
            partner: friend,
        })
        .unwrap();
        let events = a.poll();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::VoiceOccupants { room: r, members } if *r == room && members.len() == 1))
        );

        while commands.try_recv().is_ok() {}
        let events = a.run(Command::JoinCall { room: room.clone() });
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::VoiceOccupants { members, .. } if members.is_empty()))
        );
        let Ok(Cmd::Call {
            call,
            partner,
            incoming,
            ..
        }) = commands.try_recv()
        else {
            panic!("expected a call command")
        };
        assert_eq!((partner, incoming), (friend, Some(77)), "joining answers their call");

        tx.send(Ev::Call {
            call: call.wrapping_add(5),
            event: SessionEvent::Connected,
        })
        .unwrap();
        assert!(a.poll().is_empty(), "another call's events are ignored");
        tx.send(Ev::Call {
            call,
            event: SessionEvent::Connected,
        })
        .unwrap();
        tx.send(Ev::Call {
            call,
            event: SessionEvent::Members(vec![Member {
                steam_id: friend,
                speaking: true,
                muted: false,
                deafened: false,
            }]),
        })
        .unwrap();
        tx.send(Ev::Call {
            call,
            event: SessionEvent::Ended(None),
        })
        .unwrap();
        let events = a.poll();
        assert!(matches!(
            &events[..],
            [
                Event::CallConnected { .. },
                Event::CallMembers { members, .. },
                Event::CallEnded { reason: None, .. }
            ] if members[0].speaking
        ));
    }

    #[test]
    fn calling_without_a_ringing_call_rings_them() {
        let (net, mut commands) = Handle::detached();
        let (_tx, events) = mpsc::channel();
        let mut a = SteamAccount::with(0, net, events, Status::Connecting);
        a.account = Some(AccountId::new(Network::Steam, backend::steam_id(1).to_string()));
        let friend = backend::steam_id(2);
        a.run(Command::JoinCall {
            room: a.room_id(friend).unwrap(),
        });
        assert!(matches!(commands.try_recv(), Ok(Cmd::Call { incoming: None, partner, .. }) if partner == friend));
        a.run(Command::SetMuted(true));
        assert!(matches!(commands.try_recv(), Ok(Cmd::CallCmd(SessionCmd::Muted(true)))));
        a.run(Command::LeaveCall);
        assert!(matches!(commands.try_recv(), Ok(Cmd::CallCmd(SessionCmd::Leave))));
    }

    #[test]
    fn blank_sends_never_reach_steam() {
        let (net, mut commands) = Handle::detached();
        let (_tx, events) = mpsc::channel();
        let mut a = SteamAccount::with(0, net, events, Status::Connecting);
        a.account = Some(AccountId::new(Network::Steam, backend::steam_id(1).to_string()));
        let room = a.room_id(backend::steam_id(2)).unwrap();
        a.run(Command::SendMessage {
            room: room.clone(),
            text: "  ".into(),
            nonce: "n1".into(),
            reply_to: None,
            files: Vec::new(),
        });
        assert!(commands.try_recv().is_err(), "a blank message isn't sent");
        a.run(Command::SendMessage {
            room,
            text: "hi".into(),
            nonce: "n2".into(),
            reply_to: None,
            files: Vec::new(),
        });
        assert!(matches!(commands.try_recv(), Ok(Cmd::Send { text, .. }) if text == "hi"));
        a.run(Command::SendMessage {
            room: a.room_id(backend::steam_id(2)).unwrap(),
            text: "".into(),
            nonce: "n3".into(),
            reply_to: None,
            files: vec![godwit_core::application::Outgoing {
                path: std::path::PathBuf::from("image.png"),
                name: "image.png".into(),
                size: 4,
            }],
        });
        assert!(
            matches!(commands.try_recv(), Ok(Cmd::Send { text, files, .. }) if text.is_empty() && files.len() == 1)
        );
    }

    #[test]
    fn adding_a_friend_reaches_the_backend_and_reports_the_result() {
        let (net, mut commands) = Handle::detached();
        let (_tx, events) = mpsc::channel();
        let mut account = SteamAccount::with(0, net, events, Status::Connecting);
        account.account = Some(AccountId::new(Network::Steam, "me"));
        account.run(Command::SendFriendRequest {
            request: 7,
            name: "friend".into(),
        });
        assert!(matches!(commands.try_recv(), Ok(Cmd::AddFriend { request: Some(7), name }) if name == "friend"));
        let mut out = Vec::new();
        account.handle(
            Ev::FriendRequest {
                request: Some(7),
                result: Ok(()),
            },
            &mut out,
        );
        assert_eq!(
            out,
            vec![Event::FriendRequestSent {
                account: AccountId::new(Network::Steam, "me"),
                request: 7,
                result: Ok(()),
            }]
        );
    }

    #[test]
    fn profile_cards_show_known_personas_and_refresh_when_details_arrive() {
        let (mut account, tx) = connected();
        let friend = backend::steam_id(2);
        tx.send(Ev::ReadyDeadline).unwrap();
        tx.send(Ev::FriendsList {
            friends: vec![(friend, FRIEND)],
            incremental: false,
        })
        .unwrap();
        account.poll();
        let user = UserId::new(account.account.clone().unwrap(), friend.to_string());
        let events = account.run(Command::LoadProfile {
            user: user.clone(),
            room: None,
        });
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Profile(p) if p.user == user && p.relationship == Relationship::Friend))
        );
        tx.send(Ev::Personas(vec![Persona {
            steam_id: friend,
            name: Some("Updated friend".into()),
            avatar_hash: Some(vec![0xab; 20]),
            ..Persona::default()
        }]))
        .unwrap();
        assert!(account.poll().iter().any(|e| matches!(e, Event::Profile(p)
            if &*p.name == "Updated friend" && p.avatar_url.is_some())));
        tx.send(Ev::ProfileSummary(
            steam_vent_proto_steam::steammessages_clientserver_friends::CMsgClientFriendProfileInfoResponse {
                steamid_friend: Some(friend),
                time_created: Some(1_000_000_000),
                summary: Some("Profile summary".into()),
                ..Default::default()
            },
        ))
        .unwrap();
        tx.send(Ev::ProfileLevels(steam_vent_proto_steam::steammessages_clientserver_2::CMsgClientFSGetFriendsSteamLevelsResponse {
            friends: vec![steam_vent_proto_steam::steammessages_clientserver_2::cmsg_client_fsget_friends_steam_levels_response::Friend {
                accountid: Some(2), level: Some(42), ..Default::default()
            }],
            ..Default::default()
        })).unwrap();
        let enriched = account.poll();
        assert!(enriched.iter().any(|e| matches!(e, Event::Profile(p)
            if p.bio.as_deref() == Some("Profile summary")
                && p.profile_level == Some(42)
                && p.member_since.as_deref() == Some("2001"))));
    }

    #[test]
    fn blocked_relationships_leave_friends_and_update_blocked_list() {
        let (mut account, tx) = connected();
        tx.send(Ev::ReadyDeadline).unwrap();
        account.poll();
        let (friend, stranger) = (backend::steam_id(2), backend::steam_id(3));
        tx.send(Ev::FriendsList {
            friends: vec![(friend, FRIEND), (stranger, BLOCKED)],
            incremental: false,
        })
        .unwrap();
        let events = account.poll();
        assert!(events.iter().any(|e| matches!(e, Event::Friends { people, .. }
            if people.len() == 1 && people[0].id.raw.as_ref() == friend.to_string())));
        assert!(events.iter().any(|e| matches!(e, Event::Blocked { users, .. }
            if users.len() == 1 && users[0].raw.as_ref() == stranger.to_string())));
        assert!(
            account
                .profile(stranger)
                .is_some_and(|p| p.relationship == Relationship::Blocked && p.can_block)
        );
        tx.send(Ev::FriendsList {
            friends: vec![(stranger, 0)],
            incremental: true,
        })
        .unwrap();
        assert!(
            account
                .poll()
                .iter()
                .any(|e| matches!(e, Event::Blocked { users, .. } if users.is_empty()))
        );
    }

    #[test]
    fn block_commands_map_to_ignore_and_never_target_self() {
        let (net, mut commands) = Handle::detached();
        let (_tx, events) = mpsc::channel();
        let mut account = SteamAccount::with(0, net, events, Status::Connecting);
        account.me = Some(backend::steam_id(1));
        let user = |id: u64| UserId::new(AccountId::new(Network::Steam, "me"), id.to_string());
        account.run(Command::BlockUser {
            user: user(backend::steam_id(2)),
        });
        assert!(
            matches!(commands.try_recv(), Ok(Cmd::IgnoreFriend { steamid, unignore: false }) if steamid == backend::steam_id(2))
        );
        account.run(Command::UnblockUser {
            user: user(backend::steam_id(2)),
        });
        assert!(
            matches!(commands.try_recv(), Ok(Cmd::IgnoreFriend { steamid, unignore: true }) if steamid == backend::steam_id(2))
        );
        account.run(Command::BlockUser {
            user: user(backend::steam_id(1)),
        });
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn own_persona_after_ready_deadline_updates_identity_without_resyncing() {
        let (mut account, tx) = connected();
        tx.send(Ev::ReadyDeadline).unwrap();
        account.poll();
        tx.send(Ev::Personas(vec![Persona {
            steam_id: backend::steam_id(1),
            name: Some("Late Name".into()),
            avatar_hash: Some(vec![0xab; 20]),
            ..Persona::default()
        }]))
        .unwrap();
        let events = account.poll();
        assert!(events.iter().any(|event| matches!(event,
            Event::AccountIdentity { display_name, avatar_url: Some(_), .. } if &**display_name == "Late Name")));
        assert!(!events.iter().any(|event| matches!(event, Event::AccountReady { .. })));
    }

    #[test]
    fn lag_recovery_requests_named_friends_with_missing_avatars() {
        let (net, mut commands) = Handle::detached();
        let (_tx, events) = mpsc::channel();
        let mut account = SteamAccount::with(0, net, events, Status::Connecting);
        let friend = backend::steam_id(2);
        account.friends.insert(
            friend,
            Friend {
                relationship: FRIEND,
                persona: Persona {
                    steam_id: friend,
                    name: Some("Friend".into()),
                    ..Persona::default()
                },
            },
        );
        account.handle(Ev::NeedPersonas, &mut Vec::new());
        assert!(matches!(commands.try_recv(), Ok(Cmd::RequestPersonas(ids)) if ids == vec![friend]));
    }

    #[test]
    fn game_libraries_are_asked_once_and_kept() {
        use godwit_core::games::Library;
        let (mut a, _tx, mut commands) = ready_with_commands();
        while commands.try_recv().is_ok() {}
        let account = a.id().cloned().unwrap();
        let friend = backend::steam_id(2);
        let user = UserId::new(account, friend.to_string());
        let load = || Command::LoadGameLibraries {
            users: vec![user.clone()],
        };
        assert!(a.run(load()).is_empty());
        assert!(matches!(commands.try_recv(), Ok(Cmd::OwnedGames(id)) if id == friend));
        // Still loading: not asked again.
        a.run(load());
        assert!(commands.try_recv().is_err());
        let mut out = Vec::new();
        a.handle(
            Ev::Library {
                steam_id: friend,
                library: Library::Private,
            },
            &mut out,
        );
        assert_eq!(
            out,
            [Event::GameLibrary {
                user: user.clone(),
                library: Library::Private
            }]
        );
        // Known for the session: answered without a request.
        assert_eq!(a.run(load()).len(), 1);
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn steam_games_are_named_from_one_lookup() {
        let (net, mut commands) = Handle::detached();
        let (_tx, events) = mpsc::channel();
        let mut account = SteamAccount::with(0, net, events, Status::Connecting);
        let playing = |id: u64| Persona {
            steam_id: id,
            state: Some(1),
            game_app: Some(440),
            ..Persona::default()
        };
        let (a, b) = (backend::steam_id(2), backend::steam_id(3));
        account.handle(Ev::Personas(vec![playing(a), playing(b)]), &mut Vec::new());
        assert!(matches!(commands.try_recv(), Ok(Cmd::AppNames(apps)) if apps == vec![440]));
        account.handle(Ev::Personas(vec![playing(a)]), &mut Vec::new());
        assert!(commands.try_recv().is_err(), "asked once");
        account.handle(Ev::AppNames(vec![(440, "A Game".into())]), &mut Vec::new());
        for id in [a, b] {
            let persona = &account.friends[&id].persona;
            assert_eq!(super::playing(persona).as_deref(), Some("Playing A Game"));
        }
        // Known now: a later persona gets it at once.
        account.handle(Ev::Personas(vec![playing(a)]), &mut Vec::new());
        assert_eq!(account.friends[&a].persona.game_name.as_deref(), Some("A Game"));
    }

    #[test]
    fn friends_and_chats_reach_the_core_once_ready() {
        let (mut a, tx) = connected();
        let friend = backend::steam_id(2);
        tx.send(Ev::FriendsList {
            friends: vec![(friend, FRIEND)],
            incremental: false,
        })
        .unwrap();
        tx.send(Ev::Sessions(vec![(2, 100, 50, 3)])).unwrap();
        tx.send(Ev::Personas(vec![
            Persona {
                steam_id: friend,
                name: Some("Friend".into()),
                state: Some(1),
                game_name: Some("A Game".into()),
                ..Persona::default()
            },
            Persona {
                steam_id: backend::steam_id(1),
                name: Some("Me".into()),
                state: Some(1),
                ..Persona::default()
            },
        ]))
        .unwrap();
        let events = a.poll();
        let ready = events.iter().find_map(|e| match e {
            Event::AccountReady {
                rooms, display_name, ..
            } => Some((rooms, display_name)),
            _ => None,
        });
        let (rooms, name) = ready.expect("ready once our own persona came");
        assert_eq!(&**name, "Me");
        assert_eq!(rooms.len(), 1, "one chat with messages");
        assert!(rooms[0].unread);
        assert_eq!(rooms[0].status.as_deref(), Some("Playing A Game"));
        assert_eq!(rooms[0].presence, Some(Presence::Online));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Friends { people, .. } if people.len() == 1))
        );
        assert!(a.id().is_some());
    }

    #[test]
    fn pending_requests_reach_the_core_and_profiles() {
        let (mut a, tx) = connected();
        tx.send(Ev::ReadyDeadline).unwrap();
        a.poll();
        let (theirs, ours, friend) = (backend::steam_id(2), backend::steam_id(3), backend::steam_id(4));
        tx.send(Ev::FriendsList {
            friends: vec![(theirs, REQUEST_RECIPIENT), (ours, REQUEST_INITIATOR), (friend, FRIEND)],
            incremental: false,
        })
        .unwrap();
        let events = a.poll();
        let requests = events.iter().find_map(|e| match e {
            Event::FriendRequests { incoming, outgoing, .. } => Some((incoming, outgoing)),
            _ => None,
        });
        let (incoming, outgoing) = requests.expect("requests sent");
        assert_eq!(
            incoming.iter().map(|p| &*p.id.raw).collect::<Vec<_>>(),
            [theirs.to_string()]
        );
        assert_eq!(
            outgoing.iter().map(|p| &*p.id.raw).collect::<Vec<_>>(),
            [ours.to_string()]
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Friends { people, .. } if people.len() == 1))
        );
        assert_eq!(a.profile(theirs).unwrap().relationship, Relationship::Incoming);
        assert_eq!(a.profile(ours).unwrap().relationship, Relationship::Outgoing);
    }

    #[test]
    fn messages_typing_and_history_convert() {
        let (mut a, tx) = connected();
        tx.send(Ev::ReadyDeadline).unwrap();
        a.poll();
        let friend = backend::steam_id(2);
        tx.send(Ev::Typing {
            friend,
            typing: true,
            seq: 2,
        })
        .unwrap();
        // A stale stop (from an older notice) is ignored.
        tx.send(Ev::Typing {
            friend,
            typing: false,
            seq: 1,
        })
        .unwrap();
        tx.send(Ev::Incoming {
            friend,
            mine: false,
            line: Line {
                from: 2,
                time: 1_700_000_000,
                ordinal: 0,
                text: "hi".into(),
                reactions: Vec::new(),
            },
        })
        .unwrap();
        let events = a.poll();
        let typing: Vec<usize> = events
            .iter()
            .filter_map(|e| match e {
                Event::Typing { names, .. } => Some(names.len()),
                _ => None,
            })
            .collect();
        assert_eq!(typing, [1]);
        let msg = events.iter().find_map(|e| match e {
            Event::MessageAdded { message, .. } => Some(message),
            _ => None,
        });
        let msg = msg.expect("the message");
        assert_eq!(&*msg.text, "hi");
        assert_eq!(msg.id, message_id(1_700_000_000, 0));
        assert_eq!(parse_message_id(&msg.id), Some((1_700_000_000, 0)));
        assert!(
            events.iter().any(|e| matches!(e, Event::RoomUpdated(r) if r.unread)),
            "a new chat, unread"
        );

        tx.send(Ev::History {
            friend,
            before: None,
            lines: vec![
                Line {
                    from: 1,
                    time: 20,
                    ordinal: 0,
                    text: "second".into(),
                    reactions: vec![backend::LineReaction {
                        reaction: "steamhappy".into(),
                        count: 2,
                        me: true,
                    }],
                },
                Line {
                    from: 2,
                    time: 10,
                    ordinal: 0,
                    text: "first".into(),
                    reactions: Vec::new(),
                },
            ],
            more: false,
        })
        .unwrap();
        let events = a.poll();
        let Some(Event::History {
            messages,
            reached_start: true,
            ..
        }) = events.first()
        else {
            panic!("a history page, got {events:?}");
        };
        let texts: Vec<&str> = messages.iter().map(|m| &*m.text).collect();
        assert_eq!(texts, ["first", "second"], "oldest first");
        assert_eq!(messages[1].reactions[0].count, 2);
        assert!(messages[1].reactions[0].me);
        assert_eq!(&*messages[1].author.raw, backend::steam_id(1).to_string(), "ours");
    }

    #[test]
    fn avatars_and_presence_follow_steam() {
        assert!(avatar_url(&[0; 20]).contains(DEFAULT_AVATAR));
        assert!(avatar_url(&[0xab, 0x01]).ends_with("ab01_medium.jpg"));
        assert_eq!(presence(3), Presence::Idle);
        assert_eq!(presence(0), Presence::Offline);
        let mut p = Persona {
            name: Some("Old".into()),
            state: Some(1),
            game_name: Some("G".into()),
            ..Persona::default()
        };
        merge(
            &mut p,
            Persona {
                state: Some(1),
                ..Persona::default()
            },
        );
        assert_eq!(p.name.as_deref(), Some("Old"), "names stay");
        assert_eq!(p.game_name, None, "the game ended");
    }
}
