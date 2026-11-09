// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A Fluxer account as the core sees it: READY and the gateway's dispatches
//! into rooms, servers and messages (through `godwit-discord`'s store, as
//! Fluxer's objects have Discord's shapes), and the core's commands into
//! requests. First version: text chat; friends, member lists and voice
//! (LiveKit, as Stoat's) come later.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use godwit_core::account::NetworkAccount;
use godwit_core::application::{Command, Event};
use godwit_core::ids::{AccountId, MessageId, RoomId, ServerId, UserId};
use godwit_core::message::{Attachment, AttachmentKind, Markup, Message, Reaction};
use godwit_core::network::Network;
use godwit_core::room::{Capabilities, Room, RoomEncryption, RoomKind};
use godwit_core::server::{Category, Server, ServerRights};
use godwit_discord::store::{self, Store};
use serde_json::Value;

use crate::backend::{self, Answer, Cmd, Ev, Handle, Request};
use crate::session;

/// Where signing in stands, for the sign-in form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    SignedOut {
        error: Option<String>,
    },
    /// Waiting for the authenticator (or a backup) code.
    NeedsCode {
        error: Option<String>,
    },
    Connecting,
    Ready,
}

/// Typing notices go at most this often per channel.
const TYPING_EVERY: Duration = Duration::from_secs(8);
/// History pages are this long; a shorter one is the start.
const PAGE: usize = 50;
/// Fluxer's snowflakes count from 2015 (docs.fluxer.com, Snowflakes).
const EPOCH_MS: i64 = 1_420_070_400_000;

pub struct FluxerAccount {
    /// The rooms last sent to the app.
    room_sync: godwit_core::room_sync::RoomSync,
    slot: u32,
    net: Handle,
    events: mpsc::Receiver<Ev>,
    status: Status,
    account: Option<AccountId>,
    store: Store,
    /// The instance's media address (avatars, icons, emoji).
    media: String,
    typing_sent: HashMap<String, Instant>,
    /// Keep the sign-in in the keychain (off in tests).
    persist: bool,
    downloads: godwit_core::media::Media,
    /// Member lists as GUILD_MEMBER_LIST_UPDATE streams them (Discord's
    /// shared code; a Fluxer list's id is its channel's).
    member_lists: godwit_discord::member_list::MemberLists,
    /// Each guild's open channel, subscribed with Lazy Request (op 14).
    list_channel: HashMap<String, String>,
    /// The ranges last asked for, by guild (op 14 again only when they change).
    list_ranges: HashMap<String, Vec<[usize; 2]>>,
}

/// A snowflake's time.
pub fn snowflake_ms(id: &str) -> i64 {
    id.parse::<u64>()
        .ok()
        .and_then(|n| i64::try_from(n >> 22).ok())
        .map_or(0, |ms| ms + EPOCH_MS)
}

impl FluxerAccount {
    /// Starts the network thread; with a kept sign-in in `slot`, connects.
    pub fn new(slot: u32, wake: backend::Waker) -> std::io::Result<Self> {
        let (net, events) = backend::spawn(wake)?;
        let saved = session::load(slot);
        let status = if saved.is_some() {
            Status::Connecting
        } else {
            Status::SignedOut { error: None }
        };
        if let Some(saved) = saved {
            net.send(Cmd::UseSession(saved));
        }
        Ok(Self::with(slot, net, events, status, true))
    }

    fn with(slot: u32, net: Handle, events: mpsc::Receiver<Ev>, status: Status, persist: bool) -> Self {
        Self {
            room_sync: Default::default(),
            slot,
            net,
            events,
            status,
            account: None,
            store: Store::default(),
            media: String::new(),
            typing_sent: HashMap::new(),
            member_lists: godwit_discord::member_list::MemberLists::default(),
            list_channel: HashMap::new(),
            list_ranges: HashMap::new(),
            persist,
            downloads: {
                let media = godwit_core::media::Media::new("fluxer");
                media.set("", Vec::new());
                media
            },
        }
    }

    pub fn slot(&self) -> u32 {
        self.slot
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Signs in on the official instance with an email (or username) and password.
    pub fn log_in(&mut self, login: String, password: String) {
        self.status = Status::Connecting;
        self.net.send(Cmd::Login { login, password });
    }

    pub fn submit_code(&mut self, code: String) {
        self.status = Status::Connecting;
        self.net.send(Cmd::Code(code));
    }

    fn me(&self) -> &str {
        self.store.me.as_ref().map_or("", |m| m.id.as_str())
    }

    fn room_id(&self, channel: &str) -> Option<RoomId> {
        Some(RoomId::new(self.account.clone()?, channel))
    }

    fn channel_of(&self, room: &RoomId) -> Option<String> {
        let id = room.raw.to_string();
        self.store.channels.contains_key(&id).then_some(id)
    }

    fn avatar(&self, user: &str) -> Option<String> {
        let hash = self.store.users.get(user)?.avatar.clone()?;
        (!self.media.is_empty()).then(|| format!("{}/avatars/{user}/{hash}.webp?size=80", self.media))
    }

    fn room(&self, channel: &str) -> Option<Room> {
        let account = self.account.clone()?;
        let c = self.store.channels.get(channel)?;
        if !c.has_messages() || (c.thread() && c.archived) {
            return None;
        }
        let names = |ids: &[String]| {
            ids.iter()
                .map(|u| self.store.display_name(u, None))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let (name, kind, icon) = match c.kind {
            store::DM => {
                let with = c.recipients.first().cloned().unwrap_or_default();
                (
                    self.store.display_name(&with, None),
                    RoomKind::Direct {
                        with: UserId::new(account.clone(), with.as_str()),
                    },
                    self.avatar(&with),
                )
            }
            store::GROUP_DM => (
                if c.name.is_empty() {
                    names(&c.recipients)
                } else {
                    c.name.clone()
                },
                RoomKind::Group,
                None,
            ),
            _ => (
                c.name.clone(),
                RoomKind::Channel {
                    server: ServerId::new(account.clone(), c.guild.clone().unwrap_or_default().as_str()),
                },
                None,
            ),
        };
        Some(Room {
            notify: Default::default(),
            id: RoomId::new(account, channel),
            name: name.into(),
            kind,
            icon_url: icon.map(Into::into),
            // READY lists only channels we can see; Fluxer refuses what we
            // may not do (its permission bits aren't documented as Discord's).
            capabilities: Capabilities::of(Network::Fluxer),
            encryption: RoomEncryption::Unencrypted,
            unread: self.store.unread(channel),
            mentions: self.store.mentions(channel),
            last_read: self
                .store
                .read
                .get(channel)
                .and_then(|r| r.last_message_id.clone())
                .map(|m| MessageId(m.into())),
            invited_by: None,
            presence: None,
            status: None,
            member_count: (c.kind == store::GROUP_DM)
                .then(|| u32::try_from(c.recipients.len() + 1).unwrap_or(u32::MAX)),
            last_activity: snowflake_ms(c.last_message_id.as_deref().unwrap_or(&c.id)),
            parent: c.thread().then(|| c.parent.clone()).flatten().map(Into::into),
            joined_thread: false,
        })
    }

    fn all_rooms(&self) -> Vec<Room> {
        let mut rooms: Vec<Room> = self
            .store
            .channels
            .values()
            .filter(|c| c.private())
            .filter_map(|c| self.room(&c.id))
            .collect();
        for g in &self.store.guild_order {
            if let Some(guild) = self.store.guilds.get(g) {
                rooms.extend(guild.channels.iter().filter_map(|c| self.room(c)));
            }
        }
        rooms
    }

    /// Servers in READY's order, channels under their categories by position.
    fn servers(&self) -> Vec<Server> {
        let Some(account) = self.account.clone() else {
            return Vec::new();
        };
        self.store
            .guild_order
            .iter()
            .filter_map(|id| self.store.guilds.get(id))
            .map(|g| {
                let channels: Vec<&store::Channel> =
                    g.channels.iter().filter_map(|c| self.store.channels.get(c)).collect();
                let mut cats: Vec<&store::Channel> = channels
                    .iter()
                    .copied()
                    .filter(|c| c.kind == store::GUILD_CATEGORY)
                    .collect();
                cats.sort_by_key(|c| c.position);
                let rooms_in = |parent: Option<&str>| {
                    let mut list: Vec<&store::Channel> = channels
                        .iter()
                        .copied()
                        .filter(|c| c.kind != store::GUILD_CATEGORY && !c.thread() && c.parent.as_deref() == parent)
                        .filter(|c| c.has_messages())
                        .collect();
                    list.sort_by_key(|c| c.position);
                    list.iter()
                        .map(|c| RoomId::new(account.clone(), c.id.as_str()))
                        .collect::<Vec<_>>()
                };
                let mut categories = vec![Category {
                    id: None,
                    can_manage: false,
                    title: "".into(),
                    rooms: rooms_in(None),
                }];
                categories.extend(cats.iter().map(|cat| Category {
                    id: None,
                    can_manage: false,
                    title: cat.name.as_str().into(),
                    rooms: rooms_in(Some(cat.id.as_str())),
                }));
                categories.retain(|c| !c.rooms.is_empty());
                Server {
                    id: ServerId::new(account.clone(), g.id.as_str()),
                    name: g.name.as_str().into(),
                    icon_url: g
                        .icon
                        .as_deref()
                        .filter(|_| !self.media.is_empty())
                        .map(|h| format!("{}/icons/{}/{h}.webp?size=96", self.media, g.id).into()),
                    banner_url: None,
                    scheduled_events: false,
                    can_view_moderation_log: false,
                    scheduled_event_count: 0,
                    edit_own_profile: false,
                    categories,
                    muted: false,
                    folder: None,
                    rights: ServerRights::default(),
                }
            })
            .collect()
    }

    fn message(&self, m: &Value) -> Option<Message> {
        let account = self.account.clone()?;
        let id = m["id"].as_str()?;
        let author = m["author"]["id"].as_str().unwrap_or_default();
        let guild = m["guild_id"].as_str().map(str::to_owned).or_else(|| {
            m["channel_id"]
                .as_str()
                .and_then(|c| self.store.channels.get(c))
                .and_then(|c| c.guild.clone())
        });
        let content = m["content"].as_str().unwrap_or_default();
        let kind = m["type"].as_u64().unwrap_or(0);
        // Ordinary messages and replies; the rest are the network's notices.
        let system = !matches!(kind, 0 | 19);
        let names = Names {
            account: self,
            guild: guild.as_deref(),
        };
        let text = if system {
            notice(kind, &self.store.display_name(author, guild.as_deref()))
        } else {
            godwit_discord::markdown::to_rich(content, &names)
        };
        let attachments = m["attachments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| {
                let url = a["url"].as_str().or_else(|| a["proxy_url"].as_str())?;
                let content_type = a["content_type"]
                    .as_str()
                    .unwrap_or_else(|| godwit_core::media::mime(a["filename"].as_str().unwrap_or_default()));
                let kind = if content_type.starts_with("image/") {
                    AttachmentKind::Image
                } else if content_type.starts_with("video/") {
                    AttachmentKind::Video
                } else if content_type.starts_with("audio/") {
                    AttachmentKind::Audio
                } else {
                    AttachmentKind::File
                };
                let dim = |k: &str| a[k].as_u64().and_then(|n| u32::try_from(n).ok());
                Some(Attachment {
                    name: a["filename"].as_str().unwrap_or("file").into(),
                    url: if matches!(kind, AttachmentKind::Video | AttachmentKind::Audio) {
                        godwit_core::media::https(url)?.to_string().into()
                    } else {
                        self.downloads.address("", url)?.into()
                    },
                    kind,
                    size: a["size"].as_u64().unwrap_or(0),
                    dimensions: dim("width").zip(dim("height")),
                    original_url: None,
                })
            })
            .collect();
        let reactions = m["reactions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                let e = &r["emoji"];
                let (emoji, image_url) = match e["id"].as_str() {
                    Some(id) => (
                        format!("{}:{id}", e["name"].as_str().unwrap_or("emoji")),
                        (!self.media.is_empty()).then(|| format!("{}/emojis/{id}.webp?size=48", self.media)),
                    ),
                    None => (e["name"].as_str()?.to_owned(), None),
                };
                Some(Reaction {
                    emoji: emoji.into(),
                    image_url: image_url.map(Into::into),
                    count: r["count"].as_u64().and_then(|c| u32::try_from(c).ok()).unwrap_or(1),
                    me: r["me"] == true,
                })
            })
            .collect();
        let sent_ms = m["timestamp"]
            .as_str()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map_or_else(|| snowflake_ms(id), |t| t.timestamp_millis());
        Some(Message {
            id: MessageId(id.into()),
            author: UserId::new(account, author),
            author_name: self.store.display_name(author, guild.as_deref()).into(),
            author_avatar: self.avatar(author).map(Into::into),
            author_colour: guild.as_deref().and_then(|g| self.store.colour(author, g)),
            sent_ms,
            edited: m["edited_timestamp"].is_string(),
            system,
            text: text.into(),
            markup: if system { Markup::Plain } else { Markup::Rich },
            source: content.into(),
            reply_to: m["message_reference"]["message_id"]
                .as_str()
                .filter(|_| kind == 19)
                .map(|r| MessageId(r.into())),
            jumbo: Vec::new(),
            attachments,
            components: None,
            embeds: Vec::new(),
            invites: Vec::new(),
            reactions,
            pending: false,
            snippet: (!system).then(|| godwit_discord::markdown::to_plain(content, &names).into()),
            pinned: m["pinned"] == true,
            forwardable: false,
            poll_endable: false,
        })
    }

    fn sync_rooms(&mut self, out: &mut Vec<Event>) {
        // Only what changed, and rooms that went (left, or removed from).
        let now = self.all_rooms();
        self.room_sync.sync(now, out);
        if let Some(account) = self.account.clone() {
            out.push(Event::ServersChanged {
                account,
                servers: self.servers(),
            });
        }
    }

    fn ready(&mut self, d: &Value, out: &mut Vec<Event>) {
        self.downloads.set("", Vec::new());
        // Fluxer names its read states `read_states`; the store reads
        // `read_state` (an array is taken as it is).
        let mut d = d.clone();
        if d.get("read_state").is_none() {
            d["read_state"] = d["read_states"].clone();
        }
        self.store.ready(&d);
        // DM recipients come as user objects: their names and avatars.
        for c in d["private_channels"].as_array().into_iter().flatten() {
            for u in c["recipients"].as_array().into_iter().flatten() {
                self.store.put_user(u);
            }
        }
        let Some(me) = self.store.me.clone() else { return };
        let account = AccountId::new(Network::Fluxer, me.id.as_str());
        self.account = Some(account.clone());
        self.status = Status::Ready;
        out.push(Event::AccountReady {
            account,
            display_name: self.store.display_name(&me.id, None).into(),
            username: me.username.as_str().into(),
            avatar_url: self.avatar(&me.id).map(Into::into),
            rooms: {
                let rooms = self.all_rooms();
                self.room_sync.reset(rooms.clone());
                rooms
            },
            servers: self.servers(),
        });
    }

    /// Subscribes `guild`'s open channel (Lazy Request, op 14, as
    /// docs.fluxer.com describes it): the guild active, its typing, and the
    /// member list's ranges around rows `first` to `last`. Only when they
    /// changed; subscribing a channel drops the guild's other list.
    fn ask_ranges(&mut self, guild: &str, channel: &str, first: usize, last: usize) {
        let ranges = godwit_discord::member_list::ranges(first, last);
        if self.list_ranges.get(guild) == Some(&ranges) {
            return;
        }
        self.list_ranges.insert(guild.to_owned(), ranges.clone());
        self.net.send(Cmd::Gateway(
            crate::gateway::LAZY_REQUEST,
            serde_json::json!({"subscriptions": {guild: {
                "active": true,
                "typing": true,
                "member_list_channels": {channel: ranges},
            }}}),
        ));
    }

    fn presence(&self, user: &str) -> godwit_core::members::Presence {
        use godwit_core::members::Presence;
        match self.store.presences.get(user).map(String::as_str) {
            Some("online") => Presence::Online,
            Some("idle") => Presence::Idle,
            Some("dnd") => Presence::Busy,
            _ => Presence::Offline,
        }
    }

    /// The open channel's streamed member list as the panel's groups.
    fn list_groups(&self, channel: &str) -> Option<Vec<godwit_core::members::MemberGroup>> {
        let guild = self.store.channels.get(channel)?.guild.clone()?;
        let rows = self.member_lists.rows(&guild, channel)?;
        let account = self.account.clone()?;
        Some(godwit_discord::member_list::groups(
            rows,
            &self.store,
            &guild,
            &account,
            |user| self.avatar(user).unwrap_or_default(),
            |user| self.presence(user),
        ))
    }

    /// GUILD_MEMBER_LIST_UPDATE: the list it names (its channel), its people
    /// kept for names, roles and presence, and shown if it's the open one.
    fn member_list_update(&mut self, d: &Value, out: &mut Vec<Event>) {
        let fallback = d["channel_id"].as_str().map(str::to_owned);
        let Some(key) = self.member_lists.apply(d, fallback) else {
            return;
        };
        let members: Vec<Value> = self.member_lists.members(&key).cloned().collect();
        for m in &members {
            self.store.put_member(&key.0, m);
            if let Some(user) = m["user"]["id"].as_str() {
                self.store.put_user(&m["user"]);
                let mut presence = m["presence"].clone();
                if presence.is_object() {
                    presence["user_id"] = serde_json::json!(user);
                    self.store.put_presence(&presence);
                }
            }
        }
        if self.list_channel.get(&key.0) == Some(&key.1)
            && let (Some(room), Some(groups)) = (self.room_id(&key.1), self.list_groups(&key.1))
        {
            out.push(Event::Members { room, groups });
        }
    }

    fn dispatch(&mut self, event: &str, d: &Value, out: &mut Vec<Event>) {
        match event {
            "READY" => self.ready(d, out),
            "GUILD_MEMBER_LIST_UPDATE" => self.member_list_update(d, out),
            "MESSAGE_CREATE" => {
                if let Some(u) = d.get("author") {
                    self.store.put_user(u);
                }
                self.store.message_created(d);
                let channel = d["channel_id"].as_str().unwrap_or_default();
                if let (Some(room), Some(message)) = (self.room_id(channel), self.message(d)) {
                    let nonce = d["nonce"]
                        .as_str()
                        .filter(|_| d["author"]["id"].as_str() == Some(self.me()));
                    match nonce {
                        Some(nonce) => out.push(Event::MessageConfirmed {
                            room: room.clone(),
                            nonce: nonce.into(),
                            message,
                        }),
                        None => out.push(Event::MessageAdded {
                            room: room.clone(),
                            message,
                        }),
                    }
                    if let Some(r) = self.room(channel) {
                        out.push(Event::RoomUpdated(r));
                    }
                }
            }
            "MESSAGE_UPDATE" => {
                let channel = d["channel_id"].as_str().unwrap_or_default();
                // Partial updates (an embed arriving) have no author: skip them.
                if d.get("author").is_some()
                    && let (Some(room), Some(message)) = (self.room_id(channel), self.message(d))
                {
                    out.push(Event::MessageUpdated { room, message });
                }
            }
            "MESSAGE_DELETE" => {
                if let (Some(room), Some(id)) = (
                    self.room_id(d["channel_id"].as_str().unwrap_or_default()),
                    d["id"].as_str(),
                ) {
                    out.push(Event::MessageDeleted {
                        room,
                        id: MessageId(id.into()),
                    });
                }
            }
            "CHANNEL_CREATE" | "CHANNEL_UPDATE" | "THREAD_CREATE" | "THREAD_UPDATE" => {
                self.store.put_channel(d);
                self.sync_rooms(out);
            }
            "CHANNEL_DELETE" | "THREAD_DELETE" => {
                if let Some(id) = d["id"].as_str() {
                    self.store.remove_channel(id);
                    if let Some(room) = self.room_id(id) {
                        out.push(Event::RoomRemoved(room));
                    }
                    self.sync_rooms(out);
                }
            }
            "GUILD_CREATE" | "GUILD_UPDATE" => {
                self.store.put_guild(d);
                self.sync_rooms(out);
            }
            "GUILD_DELETE" => {
                if let Some(id) = d["id"].as_str() {
                    self.store.remove_guild(id);
                    self.sync_rooms(out);
                }
            }
            "USER_UPDATE" => self.store.put_user(d),
            _ => {}
        }
    }

    fn handle(&mut self, ev: Ev, out: &mut Vec<Event>) {
        match ev {
            Ev::LoginFailed(error) => {
                self.status = match self.status {
                    Status::NeedsCode { .. } => Status::NeedsCode { error: Some(error) },
                    _ => Status::SignedOut { error: Some(error) },
                };
            }
            Ev::NeedsCode => self.status = Status::NeedsCode { error: None },
            Ev::SignedIn(saved) => {
                if self.persist {
                    session::save(self.slot, &saved);
                }
                self.status = Status::Connecting;
            }
            Ev::Media(media) => self.media = media,
            Ev::Dispatch(d) => self.dispatch(&d.event, &d.data, out),
            Ev::Lost { reason, signed_out } => {
                if signed_out {
                    self.downloads.clear();
                    if self.persist {
                        session::clear(self.slot);
                    }
                    self.status = Status::SignedOut {
                        error: Some("Fluxer no longer accepts this sign-in. Sign in again.".into()),
                    };
                    if let Some(account) = self.account.take() {
                        out.push(Event::AccountLost {
                            account,
                            reason: reason.into(),
                        });
                    }
                } else if let Some(account) = self.account.clone() {
                    out.push(Event::Connection { account, online: false });
                }
            }
            Ev::Answer(answer) => self.answer(answer, out),
            Ev::LoggedOut => {
                self.downloads.clear();
                if self.persist {
                    session::clear(self.slot);
                }
                self.status = Status::SignedOut { error: None };
                if let Some(account) = self.account.take() {
                    out.push(Event::AccountLost {
                        account,
                        reason: "".into(),
                    });
                }
            }
        }
    }

    fn answer(&mut self, answer: Answer, out: &mut Vec<Event>) {
        match answer {
            Answer::History {
                channel,
                before,
                result,
            } => {
                let Some(room) = self.room_id(&channel) else { return };
                match result {
                    Ok(v) => {
                        let page = v.as_array().cloned().unwrap_or_default();
                        for m in &page {
                            if let Some(u) = m.get("author") {
                                self.store.put_user(u);
                            }
                        }
                        let mut messages: Vec<Message> = page.iter().filter_map(|m| self.message(m)).collect();
                        // Fluxer answers newest first; the core wants oldest first.
                        messages.reverse();
                        out.push(Event::History {
                            room,
                            before: before.map(|b| MessageId(b.into())),
                            messages,
                            reached_start: page.len() < PAGE,
                        });
                    }
                    Err(e) => {
                        out.push(Event::Error(format!("Fluxer: {e}").into()));
                        out.push(Event::HistoryFailed { room });
                    }
                }
            }
            Answer::Sent { channel, nonce, result } => {
                let Some(room) = self.room_id(&channel) else { return };
                match result {
                    Ok(v) => {
                        if let Some(message) = self.message(&v) {
                            out.push(Event::MessageConfirmed {
                                room,
                                nonce: nonce.into(),
                                message,
                            });
                        }
                    }
                    Err(error) => out.push(Event::MessageFailed {
                        room,
                        nonce: nonce.into(),
                        error: error.into(),
                    }),
                }
            }
            Answer::Done { what, result } => {
                if let Err(e) = result {
                    out.push(Event::Error(format!("Fluxer, {what}: {e}").into()));
                }
            }
        }
    }

    fn run_command(&mut self, command: Command) -> Vec<Event> {
        let mut out = Vec::new();
        let req = |net: &Handle, r: Request| net.send(Cmd::Request(r));
        match command {
            // This account has no message component button implementation.
            Command::ActivateMessageButton { .. } => {}
            // Fluxer contact nicknames stay in Godwit's local contact model.
            Command::SetNickname { .. } => {}
            // Fluxer rooms do not currently expose group leave or recipient editing.
            Command::LeaveGroup { .. }
            | Command::AddGroupMember { .. }
            | Command::RemoveGroupMember { .. }
            | Command::JoinThread { .. }
            | Command::LeaveThread { .. } => {}
            // Fluxer has no channel webhook manager in this account.
            Command::LoadWebhooks { .. } | Command::ChangeWebhook { .. } => {}
            // A server channel opened: subscribed, as Fluxer's client does.
            Command::Focus { room } => {
                let opened = room
                    .as_ref()
                    .and_then(|r| self.channel_of(r))
                    .and_then(|c| Some((self.store.channels.get(&c)?.guild.clone()?, c)));
                if let Some((guild, channel)) = opened
                    && self.list_channel.get(&guild) != Some(&channel)
                {
                    self.list_channel.insert(guild.clone(), channel.clone());
                    self.member_lists.keep_only(&guild, &channel);
                    self.list_ranges.remove(&guild);
                    self.ask_ranges(&guild, &channel, 0, 0);
                }
            }
            Command::LoadMembers { room } => {
                if let Some(channel) = self.channel_of(&room)
                    && let Some(groups) = self.list_groups(&channel)
                {
                    out.push(Event::Members { room, groups });
                }
            }
            Command::MemberRows { room, first, last } => {
                if let Some(channel) = self.channel_of(&room)
                    && let Some(guild) = self.store.channels.get(&channel).and_then(|c| c.guild.clone())
                    && self.list_channel.get(&guild) == Some(&channel)
                {
                    let (first, last) = (usize::try_from(first).unwrap_or(0), usize::try_from(last).unwrap_or(0));
                    self.ask_ranges(&guild, &channel, first, last);
                }
            }
            Command::LoadHistory { room, before } => {
                if let Some(channel) = self.channel_of(&room) {
                    req(
                        &self.net,
                        Request::History {
                            channel,
                            before: before.map(|b| b.0.to_string()),
                        },
                    );
                }
            }
            Command::SendMessage {
                room,
                text,
                nonce,
                reply_to,
                files,
            } => {
                let Some(channel) = self.channel_of(&room) else {
                    return out;
                };
                req(
                    &self.net,
                    Request::Send {
                        channel,
                        content: text,
                        nonce: nonce.to_string(),
                        reply: reply_to.map(|r| r.0.to_string()),
                        files,
                    },
                );
            }
            Command::EditMessage { room, message, text } => {
                if let Some(channel) = self.channel_of(&room) {
                    req(
                        &self.net,
                        Request::Edit {
                            channel,
                            message: message.0.to_string(),
                            content: text,
                        },
                    );
                }
            }
            Command::DeleteMessage { room, message } => {
                if let Some(channel) = self.channel_of(&room) {
                    req(
                        &self.net,
                        Request::Delete {
                            channel,
                            message: message.0.to_string(),
                        },
                    );
                }
            }
            Command::React {
                room,
                message,
                emoji,
                add,
            } => {
                if let Some(channel) = self.channel_of(&room) {
                    req(
                        &self.net,
                        Request::React {
                            channel,
                            message: message.0.to_string(),
                            emoji: emoji.to_string(),
                            on: add,
                        },
                    );
                }
            }
            Command::Typing { room } => {
                if let Some(channel) = self.channel_of(&room) {
                    let now = Instant::now();
                    if self
                        .typing_sent
                        .get(&channel)
                        .is_none_or(|t| now.duration_since(*t) >= TYPING_EVERY)
                    {
                        self.typing_sent.insert(channel.clone(), now);
                        req(&self.net, Request::Typing { channel });
                    }
                }
            }
            Command::MarkRead { room, last } => {
                if let Some(channel) = self.channel_of(&room) {
                    let was_unread = self.store.unread(&channel) || self.store.mentions(&channel) > 0;
                    // Up to the channel's newest message, as the official client.
                    let message = self
                        .store
                        .channels
                        .get(&channel)
                        .and_then(|c| c.last_message_id.clone())
                        .filter(|newest| store::newer(newest, &last.0))
                        .unwrap_or_else(|| last.0.to_string());
                    self.store.mark_read(&channel, &message);
                    if was_unread {
                        req(
                            &self.net,
                            Request::Ack {
                                channel: channel.clone(),
                                message,
                            },
                        );
                    }
                    if let Some(r) = self.room(&channel) {
                        out.push(Event::RoomUpdated(r));
                    }
                }
            }
            Command::MarkRoomRead { room } => {
                if let Some(channel) = self.channel_of(&room)
                    && let Some(last) = self
                        .store
                        .channels
                        .get(&channel)
                        .and_then(|c| c.last_message_id.clone())
                {
                    self.store.mark_read(&channel, &last);
                    req(
                        &self.net,
                        Request::Ack {
                            channel: channel.clone(),
                            message: last,
                        },
                    );
                    if let Some(r) = self.room(&channel) {
                        out.push(Event::RoomUpdated(r));
                    }
                }
            }
            Command::LoadServerEvents { .. }
            | Command::SetServerEventInterest { .. }
            | Command::LoadModerationLog { .. } => {}
            _ => {}
        }
        out
    }
}

/// A notice's words, for message types that aren't text.
fn notice(kind: u64, who: &str) -> String {
    match kind {
        1 => format!("{who} added someone to the group"),
        2 => format!("{who} left or removed someone from the group"),
        3 => format!("{who} started a call"),
        4 => format!("{who} renamed the channel"),
        5 => format!("{who} changed the channel's icon"),
        6 => format!("{who} pinned a message"),
        7 => format!("{who} joined the server"),
        _ => format!("{who} did something Godwit can't show yet"),
    }
}

/// Names for mentions, from the store.
struct Names<'a> {
    account: &'a FluxerAccount,
    guild: Option<&'a str>,
}

impl godwit_discord::markdown::Names for Names<'_> {
    fn user(&self, id: &str) -> String {
        self.account.store.display_name(id, self.guild)
    }
    fn channel(&self, id: &str) -> String {
        self.account
            .store
            .channels
            .get(id)
            .map_or_else(|| "channel".into(), |c| c.name.clone())
    }
    fn role(&self, id: &str) -> String {
        self.guild
            .and_then(|g| self.account.store.guilds.get(g))
            .and_then(|g| g.roles.get(id))
            .map_or_else(|| "role".into(), |r| r.name.clone())
    }
    fn emoji(&self, id: &str) -> String {
        self.guild
            .and_then(|g| self.account.store.guilds.get(g))
            .and_then(|g| g.emojis.get(id))
            .map_or_else(|| "emoji".into(), |(name, _)| name.clone())
    }
    fn emoji_url(&self, id: &str) -> Option<String> {
        (!self.account.media.is_empty()).then(|| format!("{}/emojis/{id}.webp?size=48", self.account.media))
    }
}

impl NetworkAccount for FluxerAccount {
    fn network(&self) -> Network {
        Network::Fluxer
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
        self.run_command(command)
    }

    fn message_link(&self, room: &RoomId, message: &MessageId) -> Option<String> {
        let c = self.store.channels.get(room.raw.as_ref())?;
        let guild = c.guild.as_deref().unwrap_or("@me");
        Some(format!("https://fluxer.com/channels/{guild}/{}/{}", c.id, message.0))
    }

    fn media(&self) -> Option<(String, godwit_core::account::MediaFetch)> {
        Some(self.downloads.registration())
    }

    fn log_out(&mut self) {
        self.downloads.clear();
        self.net.send(Cmd::Logout);
    }
}

impl Drop for FluxerAccount {
    fn drop(&mut self) {
        self.downloads.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn account() -> (
        FluxerAccount,
        tokio::sync::mpsc::UnboundedReceiver<Cmd>,
        mpsc::Sender<Ev>,
    ) {
        let (net, cmds) = Handle::detached();
        let (tx, events) = mpsc::channel();
        let mut a = FluxerAccount::with(u32::MAX, net, events, Status::Connecting, false);
        a.media = "https://media.example".into();
        (a, cmds, tx)
    }

    /// A READY as docs.fluxer.com describes it (guild fields under `properties`).
    fn ready() -> Value {
        json!({
            "session_id": "s", "version": 1,
            "user": {"id": "100", "username": "me", "avatar": null},
            "users": [{"id": "200", "username": "ada", "global_name": "Ada", "avatar": "abc"}],
            "guilds": [{
                "id": "10", "properties": {"id": "10", "name": "Lounge", "icon": "ic", "owner_id": "100"},
                "roles": [{"id": "10", "name": "@everyone", "permissions": "0"}],
                "channels": [
                    {"id": "11", "type": 4, "name": "Talk", "position": 0},
                    {"id": "12", "type": 0, "name": "general", "position": 1, "parent_id": "11", "last_message_id": "1501320000000000000"},
                    {"id": "13", "type": 0, "name": "rules", "position": 0}
                ],
                "members": [], "member_count": 2, "online_count": 1
            }],
            "private_channels": [{"id": "20", "type": 1, "recipients": [{"id": "200", "username": "ada", "global_name": "Ada", "avatar": "abc"}], "last_message_id": "1501320000000000001"}],
            "read_states": [{"id": "12", "last_message_id": "1501310000000000000", "mention_count": 1}],
            "relationships": []
        })
    }

    #[test]
    fn selected_files_reach_the_backend_without_reading_them_on_the_ui_thread() {
        let (mut a, mut cmds, _tx) = account();
        a.dispatch("READY", &ready(), &mut Vec::new());
        while cmds.try_recv().is_ok() {}
        let room = RoomId::new(a.id().unwrap().clone(), "20");
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
        let Ok(Cmd::Request(Request::Send { files, .. })) = cmds.try_recv() else {
            panic!("file send not routed")
        };
        assert_eq!(files, vec![selected]);
    }

    #[test]
    fn ready_brings_rooms_and_servers_with_categories() {
        let (mut a, _cmds, _tx) = account();
        let mut out = Vec::new();
        a.dispatch("READY", &ready(), &mut out);
        let Some(Event::AccountReady {
            rooms,
            servers,
            display_name,
            ..
        }) = out.first()
        else {
            panic!("{out:?}")
        };
        assert_eq!(&**display_name, "me");
        let dm = rooms.iter().find(|r| &*r.id.raw == "20").expect("the DM");
        assert_eq!(&*dm.name, "Ada");
        assert_eq!(
            dm.icon_url.as_deref(),
            Some("https://media.example/avatars/200/abc.webp?size=80")
        );
        let general = rooms.iter().find(|r| &*r.id.raw == "12").expect("general");
        assert!(general.unread && general.mentions == 1);
        assert_eq!(servers.len(), 1);
        assert_eq!(&*servers[0].name, "Lounge");
        let titles: Vec<&str> = servers[0].categories.iter().map(|c| &*c.title).collect();
        assert_eq!(titles, ["", "Talk"]);
        assert_eq!(&*servers[0].categories[0].rooms[0].raw, "13");
    }

    #[test]
    fn messages_convert_and_our_sends_confirm_by_nonce() {
        let (mut a, mut cmds, _tx) = account();
        let mut out = Vec::new();
        a.dispatch("READY", &ready(), &mut out);
        let room = RoomId::new(a.account.clone().unwrap(), "20");
        a.run(Command::SendMessage {
            room: room.clone(),
            text: "hi".into(),
            nonce: "n1".into(),
            reply_to: None,
            files: Vec::new(),
        });
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Request(Request::Send { channel, nonce, .. })) if channel == "20" && nonce == "n1")
        );
        let mut out = Vec::new();
        a.dispatch(
            "MESSAGE_CREATE",
            &json!({"id": "1501320000000000002", "channel_id": "20", "type": 0, "content": "hi <@200>",
                    "author": {"id": "100", "username": "me"}, "nonce": "n1",
                    "timestamp": "2026-08-31T09:14:02.000Z", "attachments": [
                        {"id": "a", "filename": "cat.png", "content_type": "image/png", "size": 10, "url": "https://m/x", "width": 4, "height": 3}
                    ]}),
            &mut out,
        );
        let Some(Event::MessageConfirmed { nonce, message, .. }) = out.first() else {
            panic!("{out:?}")
        };
        assert_eq!(&**nonce, "n1");
        assert!(message.text.contains("Ada"), "{}", message.text);
        assert_eq!(message.attachments[0].dimensions, Some((4, 3)));
        assert_eq!(message.attachments[0].kind, AttachmentKind::Image);
    }

    #[test]
    fn snowflakes_count_from_2015() {
        // docs.fluxer.com: timestamp_ms = (snowflake >> 22) + 1420070400000.
        assert_eq!(snowflake_ms(&((1_000u64) << 22).to_string()), EPOCH_MS + 1_000);
    }
}
