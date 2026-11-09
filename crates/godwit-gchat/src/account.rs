// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A Google Chat sign-in as the core sees it: DMs as direct chats, spaces
//! as group chats, each conversation's newest messages as plain text, and
//! sending. Rooms are maugclib's ids (`dm:…`, `space:…`), people their
//! Chat user ids. First version: no push yet (new messages show when a
//! chat is opened again), threads show flat, no files, edits or reactions.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;

use godwit_core::account::NetworkAccount;
use godwit_core::application::{Command, Event};
use godwit_core::ids::{AccountId, MessageId, RoomId, UserId};
use godwit_core::members::Presence;
use godwit_core::message::{Markup, Message};
use godwit_core::network::Network;
use godwit_core::room::{Capabilities, Room, RoomEncryption, RoomKind};

use crate::backend::{self, Change, Cmd, Ev, Handle};
use crate::{api, proto, session};

/// Where signing in stands, for the sign-in screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    SignedOut {
        error: Option<String>,
    },
    /// Google's sign-in window is open.
    Waiting,
    Connecting,
    Ready,
}

#[derive(Clone, Debug)]
struct Conversation {
    name: String,
    /// The other person, in a DM.
    with: Option<String>,
    unread: bool,
    /// Microseconds, as Chat keeps time.
    sort_us: i64,
    icon: Option<String>,
    parent: Option<String>,
    root_time: i64,
}

pub struct GChatAccount {
    slot: u32,
    net: Handle,
    events: mpsc::Receiver<Ev>,
    status: Status,
    account: Option<AccountId>,
    me: String,
    own_presence: Presence,
    own_status: Option<String>,
    statuses: HashMap<String, Option<String>>,
    conversations: HashMap<String, Conversation>,
    /// User id → (name, picture).
    users: HashMap<String, (String, Option<String>)>,
    asked: HashSet<String>,
    /// Loaded messages' topics and times, by message id: edits, deletes,
    /// reactions and replies name a message within its topic.
    messages: HashMap<(String, String), (String, i64)>,
    /// Bounded replay protection for the live stream, scoped by conversation.
    seen: HashSet<(String, String)>,
    seen_order: std::collections::VecDeque<(String, String)>,
    stream_connected: bool,
    world_pending: bool,
    catchup: HashMap<String, i64>,
    history_pending: HashSet<String>,
    presence_asked: HashMap<String, std::time::Instant>,
    read_pending: HashMap<String, i64>,
    read_wanted: HashMap<String, i64>,
    persist: bool,
    media: godwit_core::media::Media,
}

impl GChatAccount {
    pub fn new(slot: u32, wake: backend::Waker) -> std::io::Result<Self> {
        let (net, events) = backend::spawn(wake)?;
        let saved = session::load(slot);
        let status = if saved.is_some() {
            Status::Connecting
        } else {
            Status::SignedOut { error: None }
        };
        let account = Self::with(slot, net, events, status, true);
        if let Some(saved) = saved {
            crate::media::configure(&account.media, &saved);
            account.net.send(Cmd::UseSession(saved));
        }
        Ok(account)
    }

    fn with(slot: u32, net: Handle, events: mpsc::Receiver<Ev>, status: Status, persist: bool) -> Self {
        Self {
            slot,
            net,
            events,
            status,
            account: None,
            me: String::new(),
            own_presence: Presence::Unknown,
            own_status: None,
            statuses: HashMap::new(),
            conversations: HashMap::new(),
            users: HashMap::new(),
            asked: HashSet::new(),
            messages: HashMap::new(),
            seen: HashSet::new(),
            seen_order: std::collections::VecDeque::new(),
            stream_connected: false,
            world_pending: false,
            catchup: HashMap::new(),
            history_pending: HashSet::new(),
            presence_asked: HashMap::new(),
            read_pending: HashMap::new(),
            read_wanted: HashMap::new(),
            persist,
            media: godwit_core::media::Media::new("gchat"),
        }
    }

    pub fn slot(&self) -> u32 {
        self.slot
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Opens Google's sign-in window (the player helper at `helper`).
    pub fn sign_in(&mut self, helper: PathBuf) {
        if matches!(self.status, Status::SignedOut { .. }) {
            self.status = Status::Waiting;
            self.net.send(Cmd::SignIn(helper));
        }
    }

    pub fn cancel_sign_in(&mut self) {
        if self.status == Status::Waiting {
            self.net.send(Cmd::CancelSignIn);
            self.status = Status::SignedOut { error: None };
        }
    }

    /// A loaded message as the API names it.
    fn target(&self, room: &RoomId, message: &MessageId) -> Option<crate::api::Target> {
        let group = api::room_parts(&room.raw).0;
        let (topic, created) = self.messages.get(&(group.to_owned(), message.0.to_string()))?;
        Some(crate::api::Target {
            group: group.to_owned(),
            topic: topic.clone(),
            message: message.0.to_string(),
            created: *created,
        })
    }

    /// Keeps where `m` is, for acting on it later.
    fn remember(&mut self, m: &proto::Message) {
        let topic = m.id.parent_id.topic_id().topic_id();
        if !topic.is_empty()
            && let Some(group) = api::group_raw(&m.id.parent_id.topic_id().group_id)
        {
            self.messages.insert(
                (group, m.id.message_id().to_owned()),
                (topic.to_owned(), m.create_time()),
            );
        }
    }

    /// The thread room `m` belongs to. Chat gives every message a topic,
    /// so a topic only becomes a thread once it has a reply: plain messages
    /// in flat spaces and DMs stay plain.
    fn note_thread(&mut self, m: &proto::Message) -> Option<String> {
        let topic = m.id.parent_id.topic_id();
        let group = api::group_raw(&topic.group_id)?;
        if topic.topic_id().is_empty() || !self.conversations.contains_key(&group) {
            return None;
        }
        let raw = format!("{group}#{}", topic.topic_id());
        // maugclib writes topic_id=thread_id or message_id: a root's topic
        // is itself, so a reply's topic differs.
        let is_reply = m.id.message_id() != topic.topic_id();
        if !is_reply && !self.conversations.contains_key(&raw) {
            return None;
        }
        let title: String = m
            .text_body()
            .lines()
            .next()
            .unwrap_or("Thread")
            .chars()
            .take(60)
            .collect();
        let c = self.conversations.entry(raw.clone()).or_insert_with(|| Conversation {
            name: title.clone(),
            with: None,
            unread: false,
            sort_us: m.create_time(),
            icon: None,
            parent: Some(group),
            root_time: m.create_time(),
        });
        if m.create_time() <= c.root_time {
            c.name = title;
            c.root_time = m.create_time();
        }
        c.sort_us = c.sort_us.max(m.create_time());
        Some(raw)
    }
    fn thread_rooms(&self, out: &mut Vec<Event>) {
        out.extend(
            self.conversations
                .iter()
                .filter(|(_, c)| c.parent.is_some())
                .filter_map(|(raw, _)| self.room(raw))
                .map(Event::RoomUpdated),
        );
    }
    fn message_in(&self, room: &str, m: &proto::Message) -> Option<Message> {
        let mut message = self.message(m)?;
        if api::room_parts(room).1.is_none() {
            let topic = m.id.parent_id.topic_id().topic_id();
            let thread = format!("{room}#{topic}");
            if !topic.is_empty() && self.conversations.contains_key(&thread) {
                message.text = format!(
                    "{}\n\n[Open thread](godwit://room/{})",
                    message.text,
                    godwit_core::encoding::encode_uri_component(&thread)
                )
                .into();
                message.markup = Markup::Rich;
            }
        }
        Some(message)
    }
    fn mark_read(&mut self, room: &str) {
        let Some(c) = self.conversations.get(room) else { return };
        let timestamp = c.sort_us;
        if timestamp <= 0 {
            return;
        }
        let group = api::room_parts(room).0.to_owned();
        self.read_wanted
            .entry(group.clone())
            .and_modify(|t| *t = (*t).max(timestamp))
            .or_insert(timestamp);
        if !self.read_pending.contains_key(&group) {
            self.read_pending.insert(group.clone(), timestamp);
            self.net.send(Cmd::MarkRead { group, timestamp });
        }
    }

    fn change(&self, room: &RoomId, message: &MessageId, change: Change) {
        if let Some(target) = self.target(room, message) {
            self.net.send(Cmd::Change {
                target,
                change,
                room: room.raw.to_string(),
            });
        }
    }

    fn user_name(&self, id: &str) -> String {
        self.users
            .get(id)
            .map_or_else(|| "Google Chat user".into(), |(n, _)| n.clone())
    }

    /// Asks for the names of `ids` not known or asked for yet.
    fn want_users(&mut self, ids: impl IntoIterator<Item = String>) {
        let new: Vec<String> = ids
            .into_iter()
            .filter(|id| !id.is_empty() && !self.users.contains_key(id) && self.asked.insert(id.clone()))
            .collect();
        if !new.is_empty() {
            self.net.send(Cmd::Users(new));
        }
    }

    fn room(&self, raw: &str) -> Option<Room> {
        let c = self.conversations.get(raw)?;
        let account = self.account.clone()?;
        let (name, kind, icon) = match &c.with {
            Some(with) => (
                self.user_name(with),
                RoomKind::Direct {
                    with: UserId::new(account.clone(), with.as_str()),
                },
                self.users.get(with).and_then(|(_, p)| p.clone()),
            ),
            None => (c.name.clone(), RoomKind::Group, c.icon.clone()),
        };
        Some(Room {
            notify: Default::default(),
            id: RoomId::new(account, raw),
            name: name.into(),
            kind,
            icon_url: icon.map(Into::into),
            capabilities: Capabilities {
                threads: c.parent.is_none(),
                ..Capabilities::of(Network::GoogleChat)
            },
            encryption: RoomEncryption::Unencrypted,
            unread: c.unread,
            mentions: 0,
            last_read: None,
            invited_by: None,
            presence: None,
            status: c
                .with
                .as_ref()
                .and_then(|id| self.statuses.get(id))
                .cloned()
                .flatten()
                .map(Into::into),
            member_count: None,
            last_activity: c.sort_us / 1000,
            parent: c.parent.as_deref().map(Into::into),
            joined_thread: false,
        })
    }

    fn message(&self, m: &proto::Message) -> Option<Message> {
        let account = self.account.clone()?;
        let author = m.creator.user_id.id().to_owned();
        let name = match m.creator.name() {
            "" => self.user_name(&author),
            n => n.to_owned(),
        };
        let text = m.text_body();
        Some(Message {
            id: MessageId(m.id.message_id().into()),
            author: UserId::new(account, author.as_str()),
            author_name: name.into(),
            author_avatar: self
                .users
                .get(&author)
                .and_then(|(_, p)| p.as_deref())
                .map(str::to_owned)
                .or_else(|| crate::media::avatar(m.creator.avatar_url()))
                .map(Into::into),
            author_colour: None,
            sent_ms: m.create_time() / 1000,
            edited: m.last_edit_time() > 0,
            system: false,
            text: crate::annotations::text(m).into(),
            markup: Markup::Rich,
            source: text.into(),
            reply_to: None,
            jumbo: Vec::new(),
            attachments: crate::media::attachments(m, &self.media),
            components: None,
            embeds: crate::media::embeds(m, &self.media),
            invites: Vec::new(),
            reactions: m
                .reactions
                .iter()
                .filter(|r| r.emoji.has_unicode() && r.count() > 0)
                .map(|r| godwit_core::message::Reaction {
                    emoji: r.emoji.unicode().into(),
                    image_url: None,
                    count: u32::try_from(r.count()).unwrap_or(1),
                    me: r.current_user_participated(),
                })
                .collect(),
            pending: false,
            snippet: None,
            pinned: false,
            forwardable: false,
            poll_endable: false,
        })
    }

    fn ready(&mut self, me: String, world: Vec<proto::WorldItemLite>, out: &mut Vec<Event>) {
        let was_ready = self.account.is_some();
        let account = AccountId::new(Network::GoogleChat, me.as_str());
        self.account = Some(account.clone());
        self.me = me.clone();
        self.status = Status::Ready;
        if !was_ready {
            self.conversations.clear();
        }
        let mut people = vec![me.clone()];
        for item in &world {
            let Some(raw) = api::group_raw(&item.group_id) else {
                continue;
            };
            let with = item
                .dm_members
                .members
                .iter()
                .map(|u| u.id().to_owned())
                .find(|id| *id != me);
            people.extend(with.clone());
            self.conversations.insert(
                raw,
                Conversation {
                    name: match item.room_name() {
                        "" => "Unnamed space".into(),
                        n => n.to_owned(),
                    },
                    with: if item.dm_members.is_some() { with } else { None },
                    unread: item.read_state.unread_message_count() > 0,
                    sort_us: item.sort_timestamp(),
                    icon: crate::media::avatar(item.avatar_url()),
                    parent: None,
                    root_time: 0,
                },
            );
        }
        // DM partners' presence once at sign-in (a snapshot; there's no push yet).
        let partners: Vec<String> = people.iter().filter(|p| **p != me).cloned().collect();
        if !partners.is_empty() {
            self.presence_now(partners);
        }
        self.want_users(people);
        let mut rooms: Vec<Room> = self.conversations.keys().filter_map(|raw| self.room(raw)).collect();
        rooms.sort_by_key(|r| std::cmp::Reverse(r.last_activity));
        if was_ready {
            out.extend(rooms.into_iter().map(Event::RoomUpdated));
            return;
        }
        out.push(Event::AccountReady {
            account,
            display_name: "Google Chat".into(),
            username: String::new().into(),
            avatar_url: None,
            rooms,
            servers: Vec::new(),
        });
    }

    fn refresh_world(&mut self) {
        if !self.world_pending {
            self.world_pending = true;
            self.net.send(Cmd::World);
        }
    }
    fn history(&mut self, group: String) {
        if self.history_pending.insert(group.clone()) {
            self.net.send(Cmd::History(group));
        }
    }
    /// Like `presence_now`, but for a stream event: the server only sends
    /// these when something actually changed, so the 30-second snapshot
    /// throttle must not swallow the update.
    fn presence_now_forced(&mut self, ids: Vec<String>) {
        let now = std::time::Instant::now();
        let ids: Vec<_> = ids
            .into_iter()
            .filter(|id| {
                if self.presence_asked.len() >= 4096 {
                    self.presence_asked
                        .retain(|_, at| now.duration_since(*at) < std::time::Duration::from_secs(30));
                }
                if self.presence_asked.len() >= 4096 {
                    return false;
                }
                self.presence_asked.insert(id.clone(), now);
                true
            })
            .collect();
        if !ids.is_empty() {
            self.net.send(Cmd::Presence(ids));
        }
    }

    fn presence_now(&mut self, ids: Vec<String>) {
        let now = std::time::Instant::now();
        let ids: Vec<_> = ids
            .into_iter()
            .filter(|id| {
                if self
                    .presence_asked
                    .get(id)
                    .is_some_and(|at| now.duration_since(*at) < std::time::Duration::from_secs(30))
                {
                    return false;
                }
                if self.presence_asked.len() >= 4096 {
                    self.presence_asked
                        .retain(|_, at| now.duration_since(*at) < std::time::Duration::from_secs(30));
                }
                if self.presence_asked.len() >= 4096 {
                    return false;
                }
                self.presence_asked.insert(id.clone(), now);
                true
            })
            .collect();
        if !ids.is_empty() {
            self.net.send(Cmd::Presence(ids));
        }
    }

    fn note_seen(&mut self, group: &str, message: &str) -> bool {
        let key = (api::room_parts(group).0.to_owned(), message.to_owned());
        if !self.seen.insert(key.clone()) {
            return false;
        }
        self.seen_order.push_back(key);
        while self.seen_order.len() > 4096 {
            if let Some(key) = self.seen_order.pop_front() {
                self.seen.remove(&key);
                self.messages.remove(&key);
            }
        }
        true
    }

    fn stream_event(&mut self, event: proto::Event, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else { return };
        let group = api::group_raw(&event.group_id);
        let kind = event.type_();
        let mut bodies = event.bodies;
        if let Some(body) = event.body.into_option() {
            bodies.insert(0, body);
        }
        for body in bodies {
            let kind = body.event_type.unwrap_or_else(|| kind.into()).enum_value_or_default();
            use proto::event::event_body::Type;
            match body.Type {
                Some(Type::MessagePosted(post)) => {
                    let Some(m) = post.message.into_option() else { continue };
                    let group = group
                        .clone()
                        .or_else(|| api::group_raw(&m.id.parent_id.topic_id().group_id));
                    let Some(group) = group else { continue };
                    if !self.conversations.contains_key(&group) {
                        self.conversations.insert(
                            group.clone(),
                            Conversation {
                                name: "Google Chat".into(),
                                with: group
                                    .starts_with("dm:")
                                    .then(|| m.creator.user_id.id().to_owned())
                                    .filter(|id| id != &self.me),
                                unread: false,
                                sort_us: 0,
                                icon: None,
                                parent: None,
                                root_time: 0,
                            },
                        );
                        out.extend(self.room(&group).map(Event::RoomUpdated));
                        self.refresh_world();
                    }
                    self.remember(&m);
                    let is_reply = m.id.message_id() != m.id.parent_id.topic_id().topic_id();
                    let was_thread = m.id.parent_id.topic_id().topic_id().is_empty()
                        || self
                            .conversations
                            .contains_key(&format!("{group}#{}", m.id.parent_id.topic_id().topic_id()));
                    let thread = self.note_thread(&m);
                    // A first reply turns a plain message into a thread: reload
                    // the conversation (coalesced) so the root shows its link.
                    if is_reply && !was_thread && thread.is_some() {
                        self.history(group.clone());
                    }
                    let raw = if is_reply {
                        thread.clone().unwrap_or_else(|| group.clone())
                    } else {
                        group.clone()
                    };
                    let room = RoomId::new(account.clone(), raw.as_str());
                    if let Some(thread) = thread.as_ref() {
                        if let Some(c) = self.conversations.get_mut(thread) {
                            c.unread |= m.creator.user_id.id() != self.me;
                        }
                        out.extend(self.room(thread).map(Event::RoomUpdated));
                    }
                    self.want_users([m.creator.user_id.id().to_owned()]);
                    let fresh = self.note_seen(&group, m.id.message_id());
                    if !fresh && kind != proto::event::EventType::MESSAGE_UPDATED {
                        continue;
                    }
                    if let Some(message) = self.message_in(&raw, &m) {
                        let edited = kind == proto::event::EventType::MESSAGE_UPDATED;
                        if edited {
                            out.push(Event::MessageUpdated {
                                room,
                                message: message.clone(),
                            });
                            // A root post shows in its thread view too: its
                            // edits must reach both timelines.
                            if !is_reply && let Some(thread) = thread.as_ref() {
                                out.push(Event::MessageUpdated {
                                    room: RoomId::new(account.clone(), thread.as_str()),
                                    message,
                                });
                            }
                        } else {
                            out.push(Event::MessageAdded { room, message });
                        }
                    }
                    if let Some(c) = self.conversations.get_mut(&group) {
                        c.unread |= m.creator.user_id.id() != self.me;
                        c.sort_us = c.sort_us.max(m.create_time());
                        out.extend(self.room(&group).map(Event::RoomUpdated));
                    } else {
                        self.refresh_world();
                    }
                }
                Some(Type::MessageDeleted(deleted)) => {
                    // The remembered message tells us where it lives: a
                    // thread reply's delete must reach the thread room (a
                    // root post shows in both).
                    let id = deleted.message_id.message_id().to_owned();
                    let remembered = group.as_ref().and_then(|g| self.messages.get(&(g.clone(), id.clone())));
                    let topic = remembered.map(|(topic, _)| topic.clone());
                    let is_reply = topic.as_ref().is_some_and(|t| *t != id);
                    let mut rooms: Vec<String> = Vec::new();
                    if let (Some(g), Some(t)) = (group.as_ref(), topic.as_ref()) {
                        let raw = format!("{g}#{t}");
                        if is_reply || self.conversations.contains_key(&raw) {
                            rooms.push(raw);
                        }
                    }
                    if !is_reply {
                        if let Some(g) = group.as_ref() {
                            rooms.push(g.clone());
                        }
                    } else if rooms.is_empty() {
                        // An unknown thread room: the parent still hears it.
                        if let Some(g) = group.as_ref() {
                            rooms.push(g.clone());
                        }
                    }
                    for raw in rooms {
                        out.push(Event::MessageDeleted {
                            room: RoomId::new(account.clone(), raw.as_str()),
                            id: MessageId(id.as_str().into()),
                        });
                    }
                }
                Some(Type::MessageReaction(reaction)) => {
                    // Reactions refresh the room the message lives in: a
                    // thread reply's room, not just the parent conversation.
                    let id = reaction.message_id.into_option().map(|id| id.message_id().to_owned());
                    let remembered = group
                        .as_ref()
                        .zip(id.as_ref())
                        .and_then(|(g, id)| self.messages.get(&(g.clone(), id.clone())));
                    let topic = remembered.map(|(topic, _)| topic.clone());
                    let is_reply = topic.as_ref().zip(id.as_ref()).is_some_and(|(t, id)| *t != *id);
                    let target = if is_reply {
                        group
                            .as_ref()
                            .zip(topic.as_ref())
                            .map(|(g, t)| format!("{g}#{t}"))
                            .filter(|raw| self.conversations.contains_key(raw))
                    } else {
                        group.clone()
                    };
                    if let Some(raw) = target {
                        self.history(raw);
                    }
                }
                Some(Type::UserStatusUpdated(status)) => {
                    let id = status.user_status.user_id.id();
                    if !id.is_empty() {
                        // The event carries the new custom status itself:
                        // show it now instead of waiting for the round trip.
                        let text = status.user_status.custom_status.status_text().to_owned();
                        self.statuses.insert(id.to_owned(), (!text.is_empty()).then_some(text));
                        self.presence_now_forced(vec![id.to_owned()]);
                    }
                }
                _ => {}
            }
        }
    }

    fn handle(&mut self, ev: Ev, out: &mut Vec<Event>) {
        match ev {
            Ev::Read {
                group,
                timestamp,
                result,
            } => {
                self.read_pending.remove(&group);
                let wanted = self.read_wanted.remove(&group).unwrap_or(timestamp);
                match result {
                    Ok(()) => {
                        for (id, c) in &mut self.conversations {
                            if (id == &group || c.parent.as_ref() == Some(&group)) && c.sort_us <= timestamp {
                                c.unread = false;
                            }
                        }
                        out.extend(
                            self.conversations
                                .keys()
                                .filter_map(|id| self.room(id))
                                .map(Event::RoomUpdated),
                        );
                        if wanted > timestamp {
                            self.read_pending.insert(group.clone(), wanted);
                            self.read_wanted.insert(group.clone(), wanted);
                            self.net.send(Cmd::MarkRead {
                                group,
                                timestamp: wanted,
                            });
                        }
                    }
                    Err(e) => {
                        // The server refused the watermark: keep the newest
                        // one we wanted, so the next mark coalesces with it,
                        // but don't loop retries at the failure.
                        if wanted > timestamp {
                            self.read_wanted
                                .entry(group)
                                .and_modify(|t| *t = (*t).max(wanted))
                                .or_insert(wanted);
                        }
                        out.push(Event::Error(format!("Google Chat, marking read: {e}").into()));
                    }
                }
            }
            Ev::OwnState(result) => {
                match result {
                    Ok(status) => {
                        self.own_presence =
                            if status.dnd_settings.dnd_state() == proto::dnd_settings::DndState_State::DND {
                                Presence::Focus
                            } else if status.has_presence_shared() && !status.presence_shared() {
                                Presence::Idle
                            } else {
                                Presence::Online
                            };
                        self.own_status = Some(status.custom_status.status_text().to_owned()).filter(|s| !s.is_empty());
                    }
                    Err(e) => out.push(Event::Error(format!("Google Chat, setting status: {e}").into())),
                }
                if let Some(account) = self.account.clone() {
                    out.push(Event::OwnPresence {
                        account,
                        presence: self.own_presence,
                        status: self.own_status.as_deref().map(Into::into),
                    });
                }
            }
            Ev::Stream(event) => self.stream_event(*event, out),
            Ev::StreamDisconnected => {
                if let Some(account) = self.account.clone() {
                    out.push(Event::Connection { account, online: false });
                }
            }
            Ev::StreamConnected => {
                // One event per new connection, not per HTTP stream rollover.
                if self.stream_connected {
                    self.refresh_world();
                }
                self.stream_connected = true;
                if let Some(account) = self.account.clone() {
                    out.push(Event::Connection { account, online: true });
                }
            }
            Ev::World(world) => {
                self.world_pending = false;
                let changed: Vec<_> = world
                    .iter()
                    .filter_map(|item| {
                        let group = api::group_raw(&item.group_id)?;
                        let old = self.conversations.get(&group)?;
                        (item.sort_timestamp() > old.sort_us).then_some((group, old.sort_us))
                    })
                    .collect();
                self.ready(self.me.clone(), world, out);
                for (group, since) in changed {
                    self.catchup.entry(group.clone()).or_insert(since);
                    self.history(group);
                }
            }
            Ev::Waiting => self.status = Status::Waiting,
            Ev::LoginFailed(error) => self.status = Status::SignedOut { error: Some(error) },
            Ev::SignedIn(saved) => {
                crate::media::configure(&self.media, &saved);
                if self.persist {
                    session::save(self.slot, &saved);
                }
                self.status = Status::Connecting;
            }
            Ev::Ready { me, world } => self.ready(me, world, out),
            Ev::History { group, result } => {
                self.history_pending.remove(&group);
                let Some(account) = self.account.clone() else { return };
                let room = RoomId::new(account, group.as_str());
                match result {
                    Ok(messages) => {
                        let catchup = self.catchup.remove(&group);
                        // Replies first: they make their topic a thread, so
                        // its root (earlier in the page) gets linked too.
                        for m in messages
                            .iter()
                            .filter(|m| m.id.message_id() != m.id.parent_id.topic_id().topic_id())
                        {
                            self.note_thread(m);
                        }
                        for m in &messages {
                            self.remember(m);
                            self.note_thread(m);
                            let fresh = self.note_seen(&group, m.id.message_id());
                            if fresh
                                && catchup.is_some_and(|since| m.create_time() > since)
                                && let Some(message) = self.message(m)
                            {
                                out.push(Event::MessageAdded {
                                    room: room.clone(),
                                    message,
                                });
                            }
                        }
                        self.want_users(messages.iter().map(|m| m.creator.user_id.id().to_owned()));
                        // Who's here now, each time the chat is opened (no push yet).
                        let mut authors: Vec<String> = messages
                            .iter()
                            .map(|m| m.creator.user_id.id().to_owned())
                            .filter(|id| !id.is_empty())
                            .collect();
                        authors.sort();
                        authors.dedup();
                        if !authors.is_empty() {
                            self.presence_now(authors);
                        }
                        self.thread_rooms(out);
                        out.push(Event::History {
                            room,
                            before: None,
                            messages: messages.iter().filter_map(|m| self.message_in(&group, m)).collect(),
                            // One page is all there is: the proto has no
                            // page_token, so LoadHistory can't page deeper
                            // (see the Command::LoadHistory arm).
                            reached_start: true,
                        });
                    }
                    Err(e) => {
                        eprintln!("error: Google Chat history: {e}");
                        out.push(Event::HistoryFailed { room });
                    }
                }
            }
            Ev::Sent { group, nonce, result } => {
                let Some(account) = self.account.clone() else { return };
                let room = RoomId::new(account, group.as_str());
                if let Ok(m) = &result {
                    self.remember(m);
                    self.note_thread(m);
                    self.thread_rooms(out);
                    self.note_seen(&group, m.id.message_id());
                }
                match result.map(|m| self.message_in(&group, &m)) {
                    Ok(Some(message)) => out.push(Event::MessageConfirmed {
                        room,
                        nonce: nonce.into(),
                        message,
                    }),
                    Ok(None) => {}
                    Err(error) => out.push(Event::MessageFailed {
                        room,
                        nonce: nonce.into(),
                        error: error.into(),
                    }),
                }
            }
            Ev::Presence(presences) => {
                let Some(account) = self.account.clone() else { return };
                let mut presence = Vec::new();
                for p in presences {
                    let id = p.user_id.id().to_owned();
                    let state = api::presence(&p);
                    self.statuses.insert(
                        id.clone(),
                        Some(p.user_status.custom_status.status_text().to_owned()).filter(|s| !s.is_empty()),
                    );
                    if id == self.me {
                        self.own_presence = state;
                        out.push(Event::OwnPresence {
                            account: account.clone(),
                            presence: state,
                            status: self.own_status.as_deref().map(Into::into),
                        });
                    }
                    presence.push((UserId::new(account.clone(), id.as_str()), state));
                }
                out.push(Event::Presences { account, presence });
                out.extend(
                    self.conversations
                        .keys()
                        .filter_map(|id| self.room(id))
                        .map(Event::RoomUpdated),
                );
            }
            Ev::Failed(error) => out.push(Event::Error(format!("Google Chat, {error}").into())),
            Ev::Users(users) => {
                let mut people = Vec::new();
                for u in users {
                    let id = u.user_id.id().to_owned();
                    let picture = crate::media::avatar(u.avatar_url());
                    if id == self.me
                        && let Some(account) = self.account.clone()
                    {
                        out.push(Event::AccountIdentity {
                            account,
                            display_name: u.name().into(),
                            username: u.email().into(),
                            avatar_url: picture.clone().map(Into::into),
                        });
                    }
                    // A lookup without a picture keeps the one we had (the
                    // message's creator often carries it): replacing it with
                    // none made pictures disappear.
                    let picture = picture.or_else(|| self.users.get(&id).and_then(|(_, p)| p.clone()));
                    let name = match u.name() {
                        "" => self.users.get(&id).map(|(n, _)| n.clone()).unwrap_or_default(),
                        n => n.to_owned(),
                    };
                    if let Some(account) = self.account.clone() {
                        people.push(godwit_core::application::Person {
                            id: UserId::new(account, id.as_str()),
                            name: if name.is_empty() {
                                "Google Chat user".into()
                            } else {
                                name.as_str().into()
                            },
                            avatar_url: picture.as_deref().map(Into::into),
                            online: false,
                        });
                    }
                    self.users.insert(id, (name, picture));
                }
                if let Some(account) = self.account.clone() {
                    out.push(Event::Authors { account, people });
                }
                let dms: Vec<String> = self
                    .conversations
                    .iter()
                    .filter(|(_, c)| c.with.is_some())
                    .map(|(raw, _)| raw.clone())
                    .collect();
                out.extend(dms.iter().filter_map(|raw| self.room(raw)).map(Event::RoomUpdated));
            }
            Ev::Lost { reason, signed_out } => {
                if signed_out {
                    self.media.clear();
                    if self.persist {
                        session::clear(self.slot);
                    }
                    self.status = Status::SignedOut {
                        error: Some("Google signed this sign-in out. Sign in again.".into()),
                    };
                    if let Some(account) = self.account.take() {
                        out.push(Event::AccountLost {
                            account,
                            reason: reason.into(),
                        });
                    }
                } else if self.account.is_some() {
                    out.push(Event::Error(format!("Google Chat: {reason}").into()));
                } else if self.status == Status::Connecting {
                    // Not connected yet and not signed out: the network
                    // thread keeps trying (`keep_starting`), so this stays
                    // "Connecting…" instead of asking to sign in again.
                    eprintln!("note: Google Chat can't connect yet ({reason}); trying again");
                } else {
                    self.status = Status::SignedOut { error: Some(reason) };
                }
            }
            Ev::LoggedOut => {
                self.media.clear();
                if self.persist {
                    session::clear(self.slot);
                }
                self.status = Status::SignedOut { error: None };
                if let Some(account) = self.account.take() {
                    out.push(Event::AccountLost {
                        account,
                        reason: "Signed out".into(),
                    });
                }
            }
        }
    }
}

impl NetworkAccount for GChatAccount {
    fn network(&self) -> Network {
        Network::GoogleChat
    }

    fn id(&self) -> Option<&AccountId> {
        self.account.as_ref()
    }

    fn connecting(&self) -> bool {
        self.id().is_none() && matches!(self.status(), Status::Connecting)
    }

    fn poll(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        godwit_core::backend::poll(self, |s| s.events.try_recv().ok(), Self::handle, &mut out);
        out
    }

    fn run(&mut self, command: Command) -> Vec<Event> {
        match command {
            Command::EditFolders(_) => {}
            // Google Chat interactive cards aren't mapped to message buttons.
            Command::ActivateMessageButton { .. } => {}
            // Google Chat contact nicknames stay in Godwit's local contact model.
            Command::SetNickname { .. } => {}
            // Google Chat rooms do not currently expose group leave or recipient editing.
            Command::LeaveGroup { .. }
            | Command::AddGroupMember { .. }
            | Command::RemoveGroupMember { .. }
            | Command::JoinThread { .. }
            | Command::LeaveThread { .. } => {}
            // Google Chat has no channel webhook manager.
            Command::LoadWebhooks { .. } | Command::ChangeWebhook { .. } => {}
            Command::MarkRead { room, .. } | Command::MarkRoomRead { room } => self.mark_read(&room.raw),
            Command::CreateThread {
                room,
                name,
                message: Some(root),
                text,
                ..
            } => {
                if let Some(target) = self.target(&room, &root) {
                    let raw = format!("{}#{}", target.group, target.topic);
                    if let Some(c) = self.conversations.get_mut(&raw)
                        && !name.trim().is_empty()
                    {
                        c.name = name.trim().chars().take(60).collect();
                    }
                    if let Some(thread) = self.room(&raw) {
                        let out = vec![Event::RoomUpdated(thread.clone()), Event::RoomOpened(thread.id)];
                        if !text.trim().is_empty() {
                            self.net.send(Cmd::Send {
                                group: raw,
                                text,
                                nonce: format!(
                                    "thread-{}",
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map_or(0, |d| d.as_nanos())
                                ),
                                files: Vec::new(),
                                reply_to: None,
                            });
                        }
                        return out;
                    }
                }
            }
            Command::SetPresence(p) => self.net.send(Cmd::SetPresence(p)),
            Command::SetStatusText(s) => self.net.send(Cmd::SetStatus(s)),
            Command::LoadHistory { room, before } => {
                if before.is_some() {
                    // No paging yet: the first page is all there is.
                    return vec![Event::History {
                        room,
                        before,
                        messages: Vec::new(),
                        reached_start: true,
                    }];
                }
                self.history(room.raw.to_string());
            }
            Command::SendMessage {
                room,
                text,
                nonce,
                files,
                reply_to,
            } => {
                let reply_to = reply_to.and_then(|m| self.target(&room, &m));
                self.net.send(Cmd::Send {
                    group: room.raw.to_string(),
                    text,
                    nonce: nonce.to_string(),
                    files,
                    reply_to,
                });
            }
            Command::EditMessage { room, message, text } => self.change(&room, &message, Change::Edit(text)),
            Command::DeleteMessage { room, message } => self.change(&room, &message, Change::Delete),
            Command::React {
                room,
                message,
                emoji,
                add,
            } => self.change(
                &room,
                &message,
                Change::React {
                    emoji: emoji.to_string(),
                    add,
                },
            ),
            // Google Chat has no separate ignore operation.
            Command::IgnoreUser { .. } | Command::UnignoreUser { .. } => {}
            Command::LoadServerEvents { .. }
            | Command::SetServerEventInterest { .. }
            | Command::LoadModerationLog { .. } => {}
            Command::LoadServerProfile { .. } | Command::SaveServerProfile { .. } => {}
            _ => {}
        }
        Vec::new()
    }

    /// Chat's link to a conversation (messages have no public links).
    fn message_link(&self, room: &RoomId, _message: &MessageId) -> Option<String> {
        let (kind, id) = api::room_parts(&room.raw).0.split_once(':')?;
        Some(format!("https://chat.google.com/{kind}/{id}"))
    }

    fn media(&self) -> Option<(String, godwit_core::account::MediaFetch)> {
        Some(self.media.registration())
    }

    fn log_out(&mut self) {
        self.media.clear();
        self.net.send(Cmd::Logout);
    }
}

impl Drop for GChatAccount {
    fn drop(&mut self) {
        self.media.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> (GChatAccount, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
        let (net, cmds) = Handle::detached();
        let (_tx, events) = mpsc::channel();
        (
            GChatAccount::with(u32::MAX, net, events, Status::Connecting, false),
            cmds,
        )
    }

    fn world_item(raw: &str, name: &str, dm_with: Option<&str>) -> proto::WorldItemLite {
        let mut item = proto::WorldItemLite::new();
        item.group_id = Some(api::group_id(raw).unwrap()).into();
        item.set_room_name(name.into());
        if let Some(with) = dm_with {
            let mut members = proto::world_item_lite::DmMembers::new();
            for id in ["me", with] {
                let mut u = proto::UserId::new();
                u.set_id(id.into());
                members.members.push(u);
            }
            item.dm_members = Some(members).into();
        }
        item
    }

    fn stream_message(id: &str, text: &str) -> proto::Event {
        stream_message_at(id, text, 42)
    }

    /// Like `stream_message`, but with a chosen create time.
    fn stream_message_at(id: &str, text: &str, created: i64) -> proto::Event {
        let mut m = proto::Message::new();
        m.id = Some(
            api::Target {
                group: "space:2".into(),
                topic: "topic".into(),
                message: id.into(),
                created,
            }
            .id()
            .unwrap(),
        )
        .into();
        m.creator
            .mut_or_insert_default()
            .user_id
            .mut_or_insert_default()
            .set_id("ann".into());
        m.set_text_body(text.into());
        m.set_create_time(created);
        let mut post = proto::MessageEvent::new();
        post.message = Some(m).into();
        let mut body = proto::event::EventBody::new();
        body.set_message_posted(post);
        let mut event = proto::Event::new();
        event.group_id = Some(api::group_id("space:2").unwrap()).into();
        event.set_type(proto::event::EventType::MESSAGE_POSTED);
        event.body = Some(body).into();
        event
    }

    #[test]
    fn own_native_status_and_peer_custom_text_are_reflected() {
        let (mut a, mut cmds) = account();
        a.handle(
            Ev::Ready {
                me: "me".into(),
                world: vec![world_item("dm:1", "", Some("ann"))],
            },
            &mut Vec::new(),
        );
        while cmds.try_recv().is_ok() {}
        a.run(Command::SetPresence(Presence::Idle));
        assert!(matches!(cmds.try_recv(), Ok(Cmd::SetPresence(Presence::Idle))));
        a.run(Command::SetStatusText(Some("Reviewing".into())));
        assert!(matches!(cmds.try_recv(), Ok(Cmd::SetStatus(Some(s))) if s == "Reviewing"));
        let mut status = proto::UserStatus::new();
        status.set_presence_shared(false);
        status
            .custom_status
            .mut_or_insert_default()
            .set_status_text("Reviewing".into());
        let mut out = Vec::new();
        a.handle(Ev::OwnState(Ok(Box::new(status))), &mut out);
        assert!(out.iter().any(
            |e| matches!(e, Event::OwnPresence { presence:Presence::Idle, status:Some(s), .. } if &**s == "Reviewing")
        ));
        out.clear();
        a.handle(Ev::OwnState(Err("unreachable".into())), &mut out);
        assert!(out.iter().any(|e| matches!(
            e,
            Event::OwnPresence {
                presence: Presence::Idle,
                ..
            }
        )));
        let mut p = proto::UserPresence::new();
        p.user_id.mut_or_insert_default().set_id("ann".into());
        p.set_presence(proto::Presence::ACTIVE);
        p.user_status
            .mut_or_insert_default()
            .custom_status
            .mut_or_insert_default()
            .set_status_text("Lunch".into());
        a.handle(Ev::Presence(vec![p]), &mut out);
        assert_eq!(a.room("dm:1").unwrap().status.as_deref(), Some("Lunch"));
    }

    #[test]
    fn live_messages_update_unopened_rooms_once_and_embedded_edits_survive_replay() {
        let (mut a, mut cmds) = account();
        a.handle(
            Ev::Ready {
                me: "me".into(),
                world: vec![world_item("space:2", "Team", None)],
            },
            &mut Vec::new(),
        );
        while cmds.try_recv().is_ok() {}
        let event = stream_message("one", "hello");
        let mut out = Vec::new();
        a.handle(Ev::Stream(Box::new(event.clone())), &mut out);
        assert_eq!(
            out.iter().filter(|e| matches!(e, Event::MessageAdded { .. })).count(),
            1
        );
        assert!(a.room("space:2").unwrap().unread);
        out.clear();
        a.handle(Ev::Stream(Box::new(event)), &mut out);
        assert!(!out.iter().any(|e| matches!(e, Event::MessageAdded { .. })));
        let mut edit = stream_message("one", "edited");
        let mut body = edit.body.take().unwrap();
        body.set_event_type(proto::event::EventType::MESSAGE_UPDATED);
        edit.bodies.push(body);
        a.handle(Ev::Stream(Box::new(edit)), &mut out);
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::MessageUpdated { message, .. } if &*message.source == "edited"))
        );
    }

    #[test]
    fn bursts_coalesce_history_world_and_presence_requests() {
        let (mut a, mut cmds) = account();
        for _ in 0..10 {
            a.history("space:2".into());
            a.refresh_world();
            a.presence_now(vec!["ann".into()]);
        }
        let sent: Vec<_> = std::iter::from_fn(|| cmds.try_recv().ok()).collect();
        assert_eq!(sent.len(), 3);
        assert_eq!(sent.iter().filter(|c| matches!(c, Cmd::History(_))).count(), 1);
        assert_eq!(sent.iter().filter(|c| matches!(c, Cmd::World)).count(), 1);
        assert_eq!(sent.iter().filter(|c| matches!(c, Cmd::Presence(_))).count(), 1);
    }

    #[test]
    fn selected_files_reach_the_backend_without_reading_them_on_the_ui_thread() {
        let (mut a, mut cmds) = account();
        a.handle(
            Ev::Ready {
                me: "me".into(),
                world: Vec::new(),
            },
            &mut Vec::new(),
        );
        while cmds.try_recv().is_ok() {}
        let room = RoomId::new(a.id().unwrap().clone(), "space:2");
        let selected = godwit_core::application::Outgoing {
            path: "never-open-this-test-path.png".into(),
            name: "image.png".into(),
            size: 42,
        };
        let events = a.run(Command::SendMessage {
            room,
            text: "caption".into(),
            nonce: "file-nonce".into(),
            reply_to: None,
            files: vec![selected.clone()],
        });
        assert!(events.is_empty());
        let Ok(Cmd::Send { files, .. }) = cmds.try_recv() else {
            panic!("file send not routed")
        };
        assert_eq!(files, vec![selected]);
    }

    #[test]
    fn the_world_becomes_dms_and_spaces_and_names_are_asked_for() {
        let (mut a, mut cmds) = account();
        let mut out = Vec::new();
        a.handle(
            Ev::Ready {
                me: "me".into(),
                world: vec![world_item("dm:1", "", Some("ann")), world_item("space:2", "Team", None)],
            },
            &mut out,
        );
        let Some(Event::AccountReady { rooms, .. }) = out.first() else {
            panic!("{out:?}")
        };
        assert_eq!(rooms.len(), 2);
        let space = rooms.iter().find(|r| &*r.id.raw == "space:2").unwrap();
        assert_eq!(&*space.name, "Team");
        assert!(matches!(space.kind, RoomKind::Group));
        let dm = rooms.iter().find(|r| &*r.id.raw == "dm:1").unwrap();
        assert!(matches!(&dm.kind, RoomKind::Direct { with } if &*with.raw == "ann"));
        let sent: Vec<Cmd> = std::iter::from_fn(|| cmds.try_recv().ok()).collect();
        assert!(
            sent.iter().any(|c| matches!(c, Cmd::Presence(ids) if ids == &["ann"])),
            "the DM partner's presence is asked for"
        );
        assert!(
            sent.iter()
                .any(|c| matches!(c, Cmd::Users(ids) if ids == &["me", "ann"])),
            "names are asked for"
        );

        let mut ann = proto::User::new();
        ann.user_id = Some({
            let mut u = proto::UserId::new();
            u.set_id("ann".into());
            u
        })
        .into();
        ann.set_name("Ann".into());
        let mut out = Vec::new();
        a.handle(Ev::Users(vec![ann]), &mut out);
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::RoomUpdated(r) if &*r.name == "Ann"))
        );
    }

    /// A stream reaction on the message `id`, in the same group as
    /// `stream_message`.
    /// A MESSAGE_UPDATED event for a known message.
    fn stream_edit(id: &str, text: &str) -> proto::Event {
        let mut event = stream_message(id, text);
        event.set_type(proto::event::EventType::MESSAGE_UPDATED);
        event
    }

    /// A MESSAGE_DELETED event for a known message.
    fn stream_delete(id: &str) -> proto::Event {
        let mut message_id = proto::MessageId::new();
        message_id.set_message_id(id.into());
        let mut deleted = proto::MessageDeletedEvent::new();
        deleted.message_id = Some(message_id).into();
        let mut body = proto::event::EventBody::new();
        body.set_message_deleted(deleted);
        let mut event = proto::Event::new();
        event.group_id = Some(api::group_id("space:2").unwrap()).into();
        event.set_type(proto::event::EventType::MESSAGE_DELETED);
        event.body = Some(body).into();
        event
    }

    fn stream_reaction(id: &str) -> proto::Event {
        let mut message_id = proto::MessageId::new();
        message_id.set_message_id(id.into());
        let mut reaction = proto::MessageReactionEvent::new();
        reaction.message_id = Some(message_id).into();
        let mut body = proto::event::EventBody::new();
        body.set_message_reaction(reaction);
        let mut event = proto::Event::new();
        event.group_id = Some(api::group_id("space:2").unwrap()).into();
        event.set_type(proto::event::EventType::MESSAGE_REACTED);
        event.body = Some(body).into();
        event
    }

    #[test]
    fn thread_replies_edit_delete_and_react_in_their_rooms() {
        let (mut a, mut cmds) = account();
        a.handle(
            Ev::Ready {
                me: "me".into(),
                world: vec![world_item("space:2", "Team", None)],
            },
            &mut Vec::new(),
        );
        let _ = cmds.try_recv();
        let mut out = Vec::new();

        // A root arrives: it goes to the conversation, and on its own is
        // just a message, not a thread.
        a.handle(Ev::Stream(Box::new(stream_message("topic", "The plan"))), &mut out);
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::MessageAdded { room, .. } if &*room.raw == "space:2")),
            "{out:?}"
        );
        assert!(a.room("space:2#topic").is_none(), "a lone message is no thread");
        assert!(
            !out.iter()
                .any(|e| matches!(e, Event::MessageAdded { message, .. } if message.text.contains("Open thread"))),
            "a lone message has no thread link"
        );

        // A reply lands in the thread room, not the conversation.
        out.clear();
        a.handle(Ev::Stream(Box::new(stream_message("m2", "me too"))), &mut out);
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::MessageAdded { room, .. } if &*room.raw == "space:2#topic")),
            "{out:?}"
        );
        assert!(
            !out.iter()
                .any(|e| matches!(e, Event::MessageAdded { room, .. } if &*room.raw == "space:2")),
            "the reply must not fall into the conversation: {out:?}"
        );
        assert!(a.room("space:2#topic").is_some(), "the first reply makes the thread");
        assert!(
            std::iter::from_fn(|| cmds.try_recv().ok()).any(|c| matches!(c, Cmd::History(raw) if &*raw == "space:2")),
            "the conversation reloads once so the root shows its link"
        );

        // An edited reply stays in the thread room; an edited root reaches
        // both views it shows in.
        out.clear();
        a.handle(Ev::Stream(Box::new(stream_edit("m2", "me too!"))), &mut out);
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::MessageUpdated { room, .. } if &*room.raw == "space:2#topic")),
            "{out:?}"
        );
        out.clear();
        a.handle(Ev::Stream(Box::new(stream_edit("topic", "The plan!"))), &mut out);
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::MessageUpdated { room, .. } if &*room.raw == "space:2")),
            "{out:?}"
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::MessageUpdated { room, .. } if &*room.raw == "space:2#topic")),
            "{out:?}"
        );

        // Deleting the reply empties the thread view; deleting the root
        // empties both views.
        out.clear();
        a.handle(Ev::Stream(Box::new(stream_delete("m2"))), &mut out);
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::MessageDeleted { room, .. } if &*room.raw == "space:2#topic")),
            "{out:?}"
        );
        out.clear();
        a.handle(Ev::Stream(Box::new(stream_delete("topic"))), &mut out);
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::MessageDeleted { room, .. } if &*room.raw == "space:2")),
            "{out:?}"
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::MessageDeleted { room, .. } if &*room.raw == "space:2#topic")),
            "{out:?}"
        );

        // Reactions refresh the room the reacted message lives in.
        out.clear();
        a.handle(Ev::Stream(Box::new(stream_reaction("m2"))), &mut out);
        let sent: Vec<_> = std::iter::from_fn(|| cmds.try_recv().ok()).collect();
        assert!(
            sent.iter()
                .any(|c| matches!(c, Cmd::History(raw) if &**raw == "space:2#topic")),
            "the reaction's history reload should hit the thread:"
        );
    }

    #[test]
    fn a_refused_read_keeps_a_newer_watermark_for_the_next_mark() {
        let (mut a, mut cmds) = account();
        a.handle(
            Ev::Ready {
                me: "me".into(),
                world: vec![world_item("space:2", "Team", None)],
            },
            &mut Vec::new(),
        );
        let mut out = Vec::new();
        a.handle(Ev::Stream(Box::new(stream_message("topic", "The plan"))), &mut out);
        a.mark_read("space:2");
        let first = std::iter::from_fn(|| cmds.try_recv().ok())
            .find_map(|c| match c {
                Cmd::MarkRead { timestamp, .. } => Some(timestamp),
                _ => None,
            })
            .expect("not marked");
        assert_eq!(first, 42);
        // A newer message raises the wanted watermark while the first mark
        // is still in flight, so it coalesces instead of sending again.
        a.handle(Ev::Stream(Box::new(stream_message_at("m3", "two", 200))), &mut out);
        a.mark_read("space:2");
        let sent: Vec<_> = std::iter::from_fn(|| cmds.try_recv().ok()).collect();
        assert!(
            !sent.iter().any(|c| matches!(c, Cmd::MarkRead { .. })),
            "the newer mark must coalesce:"
        );
        // The first mark is refused: the newer watermark stays armed for
        // whatever mark comes next, instead of being dropped.
        a.handle(
            Ev::Read {
                group: "space:2".into(),
                timestamp: 42,
                result: Err("declined".into()),
            },
            &mut out,
        );
        assert_eq!(a.read_wanted.get("space:2"), Some(&200));
    }

    #[test]
    fn sending_goes_to_the_conversation() {
        let (mut a, mut cmds) = account();
        a.handle(
            Ev::Ready {
                me: "me".into(),
                world: Vec::new(),
            },
            &mut Vec::new(),
        );
        let _ = cmds.try_recv();
        let room = RoomId::new(a.id().unwrap().clone(), "space:2");
        a.run(Command::SendMessage {
            room,
            text: "hi".into(),
            nonce: "n1".into(),
            reply_to: None,
            files: Vec::new(),
        });
        let Ok(Cmd::Send { group, text, nonce, .. }) = cmds.try_recv() else {
            panic!("not sent")
        };
        assert_eq!((group.as_str(), text.as_str(), nonce.as_str()), ("space:2", "hi", "n1"));
    }
}
