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
use godwit_core::application::{Command, Event, Person};
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
    /// Friends, requests and blocked users as last sent to the core.
    relationships: godwit_core::relationships::RelationshipLists,
    /// The channel whose pins were last asked for: only its pin updates are
    /// fetched again, not every channel's.
    pins_shown: Option<String>,
    /// The instance's origin: the kept sign-in's, or the official one.
    origin: String,
    /// The sign-in form's server field, sent with the credentials.
    server_input: String,
    /// Our channel overrides as Fluxer sent them, by guild id (`@me`: the
    /// DMs). A change sends them all back, since the map replaces the stored one.
    overrides: HashMap<String, serde_json::Map<String, Value>>,
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
        let origin = saved.as_ref().map(|s| s.origin.clone());
        let status = if saved.is_some() {
            Status::Connecting
        } else {
            Status::SignedOut { error: None }
        };
        if let Some(saved) = saved {
            net.send(Cmd::UseSession(saved));
        }
        let mut account = Self::with(slot, net, events, status, true);
        if let Some(origin) = origin {
            account.origin = origin;
        }
        Ok(account)
    }

    fn with(slot: u32, net: Handle, events: mpsc::Receiver<Ev>, status: Status, persist: bool) -> Self {
        Self {
            room_sync: Default::default(),
            pins_shown: None,
            origin: crate::discovery::OFFICIAL.to_owned(),
            server_input: String::new(),
            overrides: HashMap::new(),
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
            relationships: Default::default(),
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

    /// Signs in with an email (or username) and password, on the instance in
    /// the form's server field (empty: the official one).
    pub fn log_in(&mut self, login: String, password: String) {
        let origin = match crate::discovery::origin(&self.server_input) {
            Ok(origin) => origin,
            Err(error) => {
                self.status = Status::SignedOut { error: Some(error) };
                return;
            }
        };
        self.status = Status::Connecting;
        self.net.send(Cmd::Login {
            login,
            password,
            origin,
        });
    }

    /// The sign-in form's server field.
    pub fn set_server(&mut self, server: String) {
        self.server_input = server;
    }

    /// Another instance's host, for the account list (none for the official one).
    pub fn instance_host(&self) -> Option<String> {
        if self.origin == crate::discovery::OFFICIAL {
            return None;
        }
        let host = self.origin.strip_prefix("https://").unwrap_or(&self.origin);
        Some(host.to_owned())
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
            notify: godwit_core::room::RoomNotify {
                level: self.store.channel_notify.get(channel).copied().filter(|l| *l <= 2),
                muted: self.store.channel_muted(channel),
            },
            id: RoomId::new(account, channel),
            name: name.into(),
            kind,
            icon_url: icon.map(Into::into),
            // READY lists only channels we can see; Fluxer refuses what we
            // may not do (its permission bits aren't documented as Discord's).
            capabilities: Capabilities::of(Network::Fluxer),
            encryption: RoomEncryption::Unencrypted,
            unread: self.store.unread(channel) && !self.store.channel_muted(channel),
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
                    muted: self.store.muted_guilds.contains(&g.id),
                    folder: self.store.folder_of.get(&g.id).cloned(),
                    rights: ServerRights {
                        notify_levels: true,
                        notify: self.store.notify_level(&g.id),
                        ..ServerRights::default()
                    },
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
                    spoiler: false,
                    voice: None,
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
            // Fluxer's embeds are Discord-shaped, as its gateway events show them
            // (https://docs.fluxer.com/gateway/events, MESSAGE_UPDATE).
            embeds: m["embeds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(godwit_discord::account::embed)
                .collect(),
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
        self.overrides.clear();
        if let Some(list) = d["user_guild_settings"].as_array_mut() {
            for s in list {
                self.remember_overrides(s);
                *s = crate::settings::normalize_guild_settings(s);
            }
        }
        self.store.ready(&d);
        self.read_user_settings(&d["user_settings"]);
        // Fluxer READY carries relationship records and any initial presence
        // data; these share the documented gateway object shapes.
        self.store.supplemental(&d);
        for presence in d["presences"].as_array().into_iter().flatten() {
            self.store.put_presence(presence);
        }
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
        self.sync_people(out);
        self.sync_presences(self.store.presences.keys().cloned().collect::<Vec<_>>(), out);
        out.extend(self.own_presence());
    }

    /// Keeps the overrides of a user guild settings object as sent.
    fn remember_overrides(&mut self, s: &Value) {
        let key = crate::settings::key(s["guild_id"].as_str());
        let map = s["channel_overrides"].as_object().cloned().unwrap_or_default();
        self.overrides.insert(key, map);
    }

    /// One user guild settings object (READY's, or the update's), in full:
    /// the channels it no longer lists have no override any more.
    fn apply_guild_settings(&mut self, s: &Value) {
        let before: Vec<String> = s
            .get("channel_overrides")
            .is_some()
            .then(|| {
                self.overrides
                    .get(&crate::settings::key(s["guild_id"].as_str()))
                    .map(|m| m.keys().cloned().collect())
            })
            .flatten()
            .unwrap_or_default();
        self.remember_overrides(s);
        for channel in before {
            if s["channel_overrides"].get(&channel).is_none() {
                self.store.channel_notify.remove(&channel);
                self.store.muted_channels.remove(&channel);
            }
        }
        self.store
            .apply_guild_settings(&crate::settings::normalize_guild_settings(s));
    }

    /// Our status and custom status from a user settings object, and the
    /// server list's folders and order (`guild_folders`, whose id -1 is the
    /// uncategorized group).
    fn read_user_settings(&mut self, s: &Value) {
        let read = crate::settings::read_status(s, chrono::Utc::now().timestamp());
        if let Some(status) = read.status {
            self.store.status = status;
        }
        if let Some(custom) = read.custom {
            self.store.custom_status = custom;
        }
        if s["guild_folders"].is_array() {
            let folders = godwit_discord::settings::folders_from_json(&s["guild_folders"]);
            self.store.set_folders(&folders);
            let place: HashMap<&String, usize> = folders
                .iter()
                .flat_map(|f| &f.guilds)
                .enumerate()
                .map(|(i, g)| (g, i + 1))
                .collect();
            self.store
                .guild_order
                .sort_by_key(|g| place.get(g).copied().unwrap_or(0));
        }
    }

    /// Our presence and custom status as the core shows them.
    fn own_presence(&self) -> Option<Event> {
        use godwit_core::members::Presence;
        let presence = match self.store.status.as_str() {
            "idle" => Presence::Idle,
            "dnd" => Presence::Busy,
            "invisible" => Presence::Invisible,
            _ => Presence::Online,
        };
        Some(Event::OwnPresence {
            account: self.account.clone()?,
            presence,
            status: self.store.custom_status.clone().map(Into::into),
        })
    }

    /// A room's own level or mute: shown at once, sent whole, and confirmed
    /// by USER_GUILD_SETTINGS_UPDATE.
    fn set_room_notify(&mut self, room: &RoomId, change: godwit_core::room::RoomNotifyChange) -> Vec<Event> {
        let Some(channel) = self.channel_of(room) else {
            return Vec::new();
        };
        let guild = self.store.channels.get(&channel).and_then(|c| c.guild.clone());
        let key = crate::settings::key(guild.as_deref());
        let patch = crate::settings::channel_patch(change, chrono::Utc::now());
        let existing = self.overrides.get(&key).cloned().unwrap_or_default();
        let body = crate::settings::overrides_body(&existing, &channel, patch);
        self.apply_guild_settings(&serde_json::json!({
            "guild_id": guild,
            "channel_overrides": body["channel_overrides"],
        }));
        self.net.send(Cmd::Request(Request::GuildSettings { guild, body }));
        self.room(&channel).map(Event::RoomUpdated).into_iter().collect()
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

    fn sync_people(&mut self, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else { return };
        let people = |kind| -> Vec<Person> {
            self.store
                .relationships
                .iter()
                .filter(|(_, relationship)| **relationship == kind)
                .map(|(user, _)| Person {
                    id: UserId::new(account.clone(), user.as_str()),
                    name: self.store.display_name(user, None).into(),
                    avatar_url: self.avatar(user).map(Into::into),
                    online: !matches!(self.presence(user), godwit_core::members::Presence::Offline),
                })
                .collect()
        };
        let blocked = self
            .store
            .relationships
            .iter()
            .filter(|(_, relationship)| **relationship == store::BLOCKED)
            .map(|(user, _)| UserId::new(account.clone(), user.as_str()))
            .collect();
        let (friends, incoming, outgoing) = (people(store::FRIEND), people(store::INCOMING), people(store::OUTGOING));
        self.relationships
            .sync(&account, friends, incoming, outgoing, blocked, out);
    }

    fn sync_presences(&self, users: impl IntoIterator<Item = String>, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else { return };
        let presence = users
            .into_iter()
            .filter_map(|user| {
                self.store.presences.get(&user)?;
                Some((UserId::new(account.clone(), user.as_str()), self.presence(&user)))
            })
            .collect::<Vec<_>>();
        if !presence.is_empty() {
            out.push(Event::Presences { account, presence });
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
            // Fluxer's published Gateway / relationship events use these
            // records: https://docs.fluxer.com.
            "RELATIONSHIP_ADD" | "RELATIONSHIP_UPDATE" | "RELATIONSHIP_REMOVE" => {
                let relationship = d.get("relationship").unwrap_or(d);
                let id = relationship["id"].as_str().or_else(|| relationship["user_id"].as_str());
                if let Some(id) = id {
                    if event == "RELATIONSHIP_REMOVE" {
                        self.store.relationships.remove(id);
                    } else if let Some(kind) = relationship["type"].as_u64() {
                        self.store.relationships.insert(id.to_owned(), kind);
                    }
                    if let Some(user) = relationship.get("user") {
                        self.store.put_user(user);
                    }
                    self.sync_people(out);
                }
            }
            // Presence is pushed as a gateway event, so friends stay current
            // without polling: https://docs.fluxer.com.
            "PRESENCE_UPDATE" => {
                self.store.put_presence(d);
                if let Some(user) = d["user_id"].as_str().or_else(|| d["user"]["id"].as_str()) {
                    self.sync_presences([user.to_owned()], out);
                    self.sync_people(out);
                }
            }
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
                if let (Some(room), Some(id)) = (self.room_id(channel), d["id"].as_str()) {
                    if d.get("author").is_some() {
                        if let Some(message) = self.message(d) {
                            out.push(Event::MessageUpdated { room, message });
                        }
                    } else {
                        // Fluxer documents partial MESSAGE_UPDATE payloads in its
                        // Gateway events reference; preserve fields the event supplies
                        // because late link previews omit the author and message body.
                        let id = MessageId(id.into());
                        if let Some(content) = d["content"].as_str() {
                            let guild = d["guild_id"]
                                .as_str()
                                .map(str::to_owned)
                                .or_else(|| self.store.channels.get(channel).and_then(|c| c.guild.clone()));
                            let names = Names {
                                account: self,
                                guild: guild.as_deref(),
                            };
                            out.push(Event::MessageEdited {
                                room: room.clone(),
                                id: id.clone(),
                                text: godwit_discord::markdown::to_rich(content, &names).into(),
                                markup: Markup::Rich,
                                source: content.into(),
                            });
                        }
                        if let Some(embeds) = d["embeds"].as_array() {
                            out.push(Event::EmbedsUpdated {
                                room,
                                id,
                                embeds: embeds.iter().filter_map(godwit_discord::account::embed).collect(),
                            });
                        }
                    }
                }
            }
            // Fluxer sends this after pin and unpin operations; refresh the visible pin list.
            // https://docs.fluxer.com/gateway/events (CHANNEL_PINS_UPDATE).
            "CHANNEL_PINS_UPDATE" => {
                if let Some(channel) = d["channel_id"].as_str()
                    && self.pins_shown.as_deref() == Some(channel)
                {
                    self.net.send(Cmd::Request(Request::Pins {
                        channel: channel.to_owned(),
                    }));
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
            "MESSAGE_DELETE_BULK" => {
                if let Some(room) = self.room_id(d["channel_id"].as_str().unwrap_or_default()) {
                    for id in d["ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        out.push(Event::MessageDeleted {
                            room: room.clone(),
                            id: MessageId(id.into()),
                        });
                    }
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
            // https://docs.fluxer.com/gateway/events/: the full user settings,
            // and the user guild settings of the one guild that changed.
            "USER_SETTINGS_UPDATE" => {
                self.read_user_settings(d);
                out.extend(self.own_presence());
                self.sync_rooms(out);
            }
            "USER_GUILD_SETTINGS_UPDATE" => {
                self.apply_guild_settings(d);
                self.sync_rooms(out);
            }
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
                self.origin = saved.origin.clone();
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
            Ev::Answer(answer) => self.answer(*answer, out),
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
            Answer::Search {
                channel,
                query,
                offset,
                result,
            } => {
                let Some(room) = self.room_id(&channel) else { return };
                match result {
                    Ok(v) if v["indexing"] == true => out.push(Event::SearchResults {
                        room,
                        query,
                        offset,
                        next_offset: None,
                        total: None,
                        notice: Some("Search index is being prepared".into()),
                        rooms: Vec::new(),
                        messages: None,
                    }),
                    Ok(v) => {
                        let page = v["messages"].as_array().cloned().unwrap_or_default();
                        for m in &page {
                            if let Some(u) = m.get("author") {
                                self.store.put_user(u);
                            }
                        }
                        let mut messages: Vec<Message> = page.iter().filter_map(|m| self.message(m)).collect();
                        messages.reverse();
                        let total = v["total"].as_u64();
                        // Pages are 25 hits (see Api::search), whatever we could show of them.
                        let next = offset + 25;
                        let next_offset =
                            (next < usize::try_from(total.unwrap_or(0)).unwrap_or(usize::MAX)).then_some(next);
                        out.push(Event::SearchResults {
                            room,
                            query,
                            offset,
                            next_offset,
                            total,
                            notice: None,
                            rooms: Vec::new(),
                            messages: Some(messages),
                        });
                    }
                    Err(e) => out.push(Event::SearchResults {
                        room,
                        query,
                        offset,
                        next_offset: None,
                        total: None,
                        notice: Some(format!("Search failed: {e}")),
                        rooms: Vec::new(),
                        messages: None,
                    }),
                }
            }
            Answer::Pins { channel, result } => {
                let Some(room) = self.room_id(&channel) else { return };
                let messages = result.ok().map(|v| {
                    let page = v["items"].as_array().or_else(|| v.as_array());
                    let values = page.cloned().unwrap_or_default();
                    for m in &values {
                        if let Some(u) = m.get("author") {
                            self.store.put_user(u);
                        }
                    }
                    values.iter().filter_map(|m| self.message(m)).collect()
                });
                out.push(Event::Pins { room, messages });
            }
            Answer::Gifs { query, result } => match result {
                Ok(v) if query.is_empty() => {
                    let categories = v["categories"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| {
                            Some(godwit_core::gifs::GifCategory {
                                title: c["name"].as_str()?.into(),
                                image: c["proxy_src"]
                                    .as_str()
                                    .filter(|s| !s.is_empty())
                                    .or_else(|| c["src"].as_str())?
                                    .into(),
                            })
                        })
                        .collect();
                    if let Some(account) = self.account.clone() {
                        out.push(Event::GifCategories {
                            account: account.clone(),
                            categories,
                        });
                        out.push(Event::Gifs {
                            account,
                            query: godwit_core::gifs::TRENDING.into(),
                            more: false,
                            results: fluxer_gifs(&v["gifs"]),
                            next: None,
                        });
                    }
                }
                Ok(v) => {
                    if let Some(account) = self.account.clone() {
                        out.push(Event::Gifs {
                            account,
                            query,
                            more: false,
                            results: fluxer_gifs(&v),
                            next: None,
                        });
                    }
                }
                Err(error) => {
                    if let Some(account) = self.account.clone() {
                        out.push(Event::GifsFailed {
                            account,
                            query,
                            error: error.into(),
                        });
                    }
                }
            },
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
            Command::LoadPins { room } => {
                if let Some(channel) = self.channel_of(&room) {
                    self.pins_shown = Some(channel.clone());
                    req(&self.net, Request::Pins { channel });
                }
            }
            Command::SetPinned { room, message, pinned } => {
                if let Some(channel) = self.channel_of(&room) {
                    req(
                        &self.net,
                        Request::SetPinned {
                            channel,
                            message: message.0.to_string(),
                            pinned,
                        },
                    );
                }
            }
            Command::SearchMessages { room, query, offset } => {
                if let Some(channel) = self.channel_of(&room) {
                    let guild = self.store.channels.get(&channel).and_then(|c| c.guild.clone());
                    req(
                        &self.net,
                        Request::Search {
                            channel,
                            guild,
                            query,
                            offset,
                        },
                    );
                }
            }
            Command::LoadGifCategories => req(&self.net, Request::GifCategories),
            Command::SearchGifs { query, position: None } => req(&self.net, Request::Gifs { query }),
            Command::SearchGifs {
                query,
                position: Some(_),
            } => {
                if let Some(account) = self.account.clone() {
                    out.push(Event::Gifs {
                        account,
                        query,
                        more: false,
                        results: Vec::new(),
                        next: None,
                    });
                }
            }
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
                if let Some(channel) = self.channel_of(&room) {
                    if let Some(groups) = self.list_groups(&channel) {
                        out.push(Event::Members {
                            room: room.clone(),
                            groups,
                        });
                    }
                    if let Some(guild) = self.store.channels.get(&channel).and_then(|c| c.guild.clone()) {
                        self.list_channel.insert(guild.clone(), channel.clone());
                        self.ask_ranges(&guild, &channel, 0, 0);
                    }
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
            Command::CancelSend { nonce, .. } => {
                self.net.send(Cmd::CancelSend(nonce.to_string()));
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
            Command::SetRoomNotify { room, change } => out.extend(self.set_room_notify(&room, change)),
            Command::ServerAdmin {
                server,
                op: godwit_core::server_settings::ServerAdmin::SetNotify(level),
            } => {
                let guild = server.raw.to_string();
                let level = level.min(2);
                self.store.guild_notify.insert(guild.clone(), level);
                req(
                    &self.net,
                    Request::GuildSettings {
                        guild: Some(guild),
                        body: crate::settings::guild_level_body(level),
                    },
                );
                self.sync_rooms(&mut out);
            }
            Command::SetPresence { presence, .. } => {
                use godwit_core::members::Presence;
                let status = match presence {
                    Presence::Idle => "idle",
                    Presence::Busy | Presence::Focus => "dnd",
                    Presence::Invisible | Presence::Offline => "invisible",
                    _ => "online",
                };
                self.store.status = status.to_owned();
                req(&self.net, Request::UserSettings(crate::settings::status_body(status)));
                out.extend(self.own_presence());
            }
            Command::SetStatusText(text) => {
                let text = text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
                self.store.custom_status = text.clone();
                req(
                    &self.net,
                    Request::UserSettings(crate::settings::custom_status_body(text.as_deref())),
                );
                out.extend(self.own_presence());
            }
            Command::LoadServerEvents { .. }
            | Command::SetServerEventInterest { .. }
            | Command::LoadModerationLog { .. } => {}
            _ => {}
        }
        out
    }
}

/// Maps Fluxer's provider objects into the shared picker's page/media pair.
fn fluxer_gifs(v: &Value) -> Vec<godwit_core::gifs::Gif> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| {
            let media = &g["media"]["tinygif"];
            let preview = media["proxy_src"]
                .as_str()
                .or_else(|| media["src"].as_str())
                .or_else(|| g["proxy_src"].as_str())
                .or_else(|| g["src"].as_str())?;
            let width = media["width"]
                .as_u64()
                .or_else(|| g["width"].as_u64())
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0);
            let height = media["height"]
                .as_u64()
                .or_else(|| g["height"].as_u64())
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0);
            Some(godwit_core::gifs::Gif {
                url: g["url"].as_str()?.into(),
                preview: preview.into(),
                width,
                height,
            })
        })
        .collect()
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
    // Fluxer documents Discord-compatible message formatting and mention metadata at https://docs.fluxer.com/http-api/messages/.
    fn autocomplete_text(&self, kind: godwit_core::autocomplete::TriggerKind, id: &str, name: &str) -> String {
        match kind {
            godwit_core::autocomplete::TriggerKind::Person => format!("<@{id}>"),
            godwit_core::autocomplete::TriggerKind::Channel => format!("<#{id}>"),
            godwit_core::autocomplete::TriggerKind::Emoji => format!("<:{name}:{id}>"),
            godwit_core::autocomplete::TriggerKind::MentionSpecial => match id {
                "everyone" => "@everyone".into(),
                "here" => "@here".into(),
                _ => format!("<@&{id}>"),
            },
        }
    }

    fn mention_specials(&self, room: &RoomId) -> Vec<godwit_core::autocomplete::MentionSpecial> {
        let Some(channel) = self.store.channels.get(room.raw.as_ref()) else {
            return Vec::new();
        };
        let Some(guild_id) = channel.guild.as_deref() else {
            return Vec::new();
        };
        let Some(guild) = self.store.guilds.get(guild_id) else {
            return Vec::new();
        };
        let allowed = self
            .store
            .can(&channel.id, godwit_discord::permissions::MENTION_EVERYONE);
        let mut out = Vec::new();
        if allowed {
            out.extend(
                [
                    ("everyone", "@everyone", "Everyone"),
                    ("here", "@here", "Online members"),
                ]
                .into_iter()
                .map(|(id, label, detail)| godwit_core::autocomplete::MentionSpecial {
                    id: id.into(),
                    label: label.into(),
                    detail: detail.into(),
                }),
            );
        }
        let mut roles: Vec<_> = guild
            .roles
            .values()
            .filter(|role| role.id != guild_id && (role.mentionable || allowed))
            .collect();
        roles.sort_by_key(|role| std::cmp::Reverse(role.position));
        out.extend(roles.into_iter().map(|role| godwit_core::autocomplete::MentionSpecial {
            id: role.id.clone(),
            label: format!("@{}", role.name),
            detail: "Role".into(),
        }));
        out
    }

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

    #[test]
    fn special_mention_wire_text_is_network_syntax() {
        let (a, _, _) = account();
        use godwit_core::autocomplete::TriggerKind;
        assert_eq!(
            a.autocomplete_text(TriggerKind::MentionSpecial, "here", "@here"),
            "@here"
        );
        assert_eq!(
            a.autocomplete_text(TriggerKind::MentionSpecial, "55", "@Team"),
            "<@&55>"
        );
        assert_eq!(a.autocomplete_text(TriggerKind::Person, "55", "Ada"), "<@55>");
        assert_eq!(a.autocomplete_text(TriggerKind::Channel, "55", "chat"), "<#55>");
        assert_eq!(a.autocomplete_text(TriggerKind::Emoji, "55", "wave"), "<:wave:55>");
    }

    #[test]
    fn received_mentions_render_and_drive_the_unread_mention_count() {
        let (mut a, _, _) = account();
        a.dispatch("READY", &ready(), &mut Vec::new());
        let mut out = Vec::new();
        a.dispatch(
            "MESSAGE_CREATE",
            &json!({
                "id":"1501320000000000003", "channel_id":"20", "type":0,
                "content":"<@200> <@&55> <:wave:66>",
                "author":{"id":"200", "username":"Ada"},
                "mentions":[{"id":"100"}], "mention_roles":["55"],
                "timestamp":"2026-08-31T09:14:02.000Z", "attachments":[]
            }),
            &mut out,
        );
        let Some(Event::MessageAdded { message, .. }) = out.first() else {
            panic!("message event missing")
        };
        assert!(message.text.contains("godwit://user/"), "{}", message.text);
        assert!(message.text.contains("godwit://role/"), "{}", message.text);
        assert!(message.text.contains("godwit://emoji/"), "{}", message.text);
        assert_eq!(a.room("20").map(|r| r.mentions), Some(1));
    }

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

    #[test]
    fn sign_in_goes_to_the_instance_in_the_server_field_and_keeps_it() {
        let (mut a, mut cmds, _tx) = account();
        assert_eq!(a.instance_host(), None);
        a.log_in("ada".into(), "pw".into());
        let Ok(Cmd::Login { origin, .. }) = cmds.try_recv() else {
            panic!("no sign-in sent");
        };
        assert_eq!(origin, crate::discovery::OFFICIAL);

        a.set_server("chat.example/app".into());
        a.log_in("ada".into(), "pw".into());
        let Ok(Cmd::Login { origin, .. }) = cmds.try_recv() else {
            panic!("no sign-in sent");
        };
        assert_eq!(origin, "https://chat.example");
        // Only a finished sign-in moves the account to the instance.
        assert_eq!(a.instance_host(), None);
        a.handle(
            Ev::SignedIn(session::Saved {
                user: "1".into(),
                token: "fake-test-token".into(),
                origin,
            }),
            &mut Vec::new(),
        );
        assert_eq!(a.instance_host().as_deref(), Some("chat.example"));

        a.set_server("http://chat.example".into());
        a.log_in("ada".into(), "pw".into());
        assert!(cmds.try_recv().is_err());
        assert!(matches!(a.status(), Status::SignedOut { error: Some(_) }));
    }

    #[test]
    fn documented_pin_page_emits_shared_pins_event_and_live_update_refetches() {
        let (mut a, mut cmds, _tx) = account();
        a.dispatch("READY", &ready(), &mut Vec::new());
        let channel = "12";
        a.answer(
            Answer::Pins {
                channel: channel.into(),
                result: Ok(json!({"items":[{
                    "id":"1501320000000000002", "channel_id":channel,
                    "author":{"id":"200","username":"ada"}, "content":"pinned", "type":0, "pinned":true,
                    "attachments":[], "embeds":[], "reactions":[]
                }]})),
            },
            &mut Vec::new(),
        );
        // Not shown: no fetch.
        a.dispatch("CHANNEL_PINS_UPDATE", &json!({"channel_id":channel}), &mut Vec::new());
        assert!(cmds.try_recv().is_err());
        a.run(Command::LoadPins {
            room: RoomId::new(a.id().unwrap().clone(), channel),
        });
        while cmds.try_recv().is_ok() {}
        a.dispatch("CHANNEL_PINS_UPDATE", &json!({"channel_id":channel}), &mut Vec::new());
        assert!(matches!(cmds.try_recv(), Ok(Cmd::Request(Request::Pins { channel: c })) if c == channel));
    }

    #[test]
    fn fluxer_gif_objects_map_page_media_and_dimensions() {
        let gifs = fluxer_gifs(
            &json!([{"url":"https://provider/gif/id","src":"fallback", "width":10,"height":11,
            "media":{"tinygif":{"proxy_src":"https://proxy/tiny.gif","width":20,"height":21}}}]),
        );
        assert_eq!(
            gifs,
            [godwit_core::gifs::Gif {
                url: "https://provider/gif/id".into(),
                preview: "https://proxy/tiny.gif".into(),
                width: 20,
                height: 21
            }]
        );
    }

    /// A READY as docs.fluxer.com describes it (guild fields under `properties`).
    fn ready() -> Value {
        json!({
            "session_id": "s", "version": 1,
            "user": {"id": "100", "username": "me", "avatar": null},
            "users": [{"id": "200", "username": "ada", "global_name": "Ada", "avatar": "abc"}],
            "guilds": [{
                "id": "10", "properties": {"id": "10", "name": "Lounge", "icon": "ic", "owner_id": "100"},
                "roles": [{"id": "10", "name": "@everyone", "permissions": "0"}, {"id": "55", "name": "Team", "permissions": "0"}],
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
            spoiler: false,
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

    fn next_request(cmds: &mut tokio::sync::mpsc::UnboundedReceiver<Cmd>) -> Option<Request> {
        while let Ok(c) = cmds.try_recv() {
            if let Cmd::Request(r) = c {
                return Some(r);
            }
        }
        None
    }

    #[test]
    fn settings_in_ready_drive_notify_status_and_folders_and_changes_are_patched() {
        use godwit_core::room::RoomNotifyChange as C;
        let (mut a, mut cmds, _tx) = account();
        let mut d = ready();
        d["user_settings"] = json!({
            "status": "dnd",
            "custom_status": {"text": "busy", "expires_at": null},
            "guild_folders": [{"id": -1, "name": null, "color": 0, "guild_ids": ["10"]}]
        });
        d["user_guild_settings"] = json!([
            {"guild_id": "10", "message_notifications": 1, "muted": false,
             "channel_overrides": {"13": {"muted": true, "message_notifications": 2,
                "mute_config": {"end_time": null, "selected_time_window": 0}},
                "11": {"collapsed": true, "message_notifications": 3}}}
        ]);
        let mut out = Vec::new();
        a.dispatch("READY", &d, &mut out);
        assert!(out.iter().any(|e| matches!(e,
            Event::OwnPresence { presence: godwit_core::members::Presence::Busy, status: Some(t), .. } if &**t == "busy")));
        let Some(Event::AccountReady { rooms, servers, .. }) = out.first() else {
            panic!("{out:?}")
        };
        let rules = rooms.iter().find(|r| &*r.id.raw == "13").expect("rules");
        assert!(rules.notify.muted && rules.notify.level == Some(2) && rules.capabilities.notify_settings);
        assert!(servers[0].rights.notify_levels && servers[0].rights.notify == 1 && servers[0].folder.is_none());

        let room = rules.id.clone();
        let out = a.run(Command::SetRoomNotify {
            room,
            change: C::Unmute,
        });
        assert!(matches!(&out[..], [Event::RoomUpdated(r)] if !r.notify.muted));
        let Some(Request::GuildSettings { guild, body }) = next_request(&mut cmds) else {
            panic!("no request")
        };
        assert_eq!(guild.as_deref(), Some("10"));
        assert_eq!(
            body,
            json!({"channel_overrides": {
                "11": {"collapsed": true, "message_notifications": 3},
                "13": {"muted": false, "message_notifications": 2,
                       "mute_config": {"end_time": null, "selected_time_window": 0}}
            }})
        );

        // An update that drops the override clears it.
        let mut out = Vec::new();
        a.dispatch(
            "USER_GUILD_SETTINGS_UPDATE",
            &json!({"guild_id": "10", "message_notifications": 0, "channel_overrides": null}),
            &mut out,
        );
        assert_eq!(a.store.channel_notify.get("13"), None);
        assert_eq!(a.store.notify_level("10"), 0);

        a.run(Command::SetPresence {
            presence: godwit_core::members::Presence::Idle,
            choice: 1,
        });
        assert!(matches!(next_request(&mut cmds), Some(Request::UserSettings(b)) if b["status"] == "idle"));
        a.run(Command::SetStatusText(Some(" hi ".into())));
        assert!(
            matches!(next_request(&mut cmds), Some(Request::UserSettings(b)) if b["custom_status"]["text"] == "hi")
        );
        a.run(Command::ServerAdmin {
            server: ServerId::new(a.account.clone().expect("account"), "10"),
            op: godwit_core::server_settings::ServerAdmin::SetNotify(2),
        });
        assert!(matches!(next_request(&mut cmds),
            Some(Request::GuildSettings { guild: Some(g), body }) if g == "10" && body["message_notifications"] == 2));
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
        assert!(
            rooms
                .iter()
                .all(|r| r.capabilities.pins && r.capabilities.search && r.capabilities.gifs)
        );
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
    fn ready_and_relationship_dispatches_fill_the_neutral_friend_lists() {
        let (mut a, _, _) = account();
        let mut payload = ready();
        payload["relationships"] = json!([
            {"id":"200", "type":store::FRIEND, "user":{"id":"200", "username":"ada", "global_name":"Ada", "avatar":"abc"}},
            {"id":"201", "type":store::INCOMING, "user":{"id":"201", "username":"bea"}}
        ]);
        payload["presences"] = json!([{ "user_id":"200", "status":"offline" }]);
        let mut out = Vec::new();
        a.dispatch("READY", &payload, &mut out);
        assert!(out.iter().any(|event| matches!(event, Event::Friends { people, .. } if people.len() == 1 && people[0].name.as_ref() == "Ada")));
        assert!(out.iter().any(|event| matches!(event, Event::FriendRequests { incoming, outgoing, .. } if incoming.len() == 1 && outgoing.is_empty())));
        assert!(out.iter().any(|event| matches!(event, Event::Presences { presence, .. } if presence.iter().any(|(user, status)| user.raw.as_ref() == "200" && *status == godwit_core::members::Presence::Offline))));

        out.clear();
        a.dispatch("RELATIONSHIP_REMOVE", &json!({"id":"201"}), &mut out);
        assert!(
            out.iter()
                .any(|event| matches!(event, Event::FriendRequests { incoming, .. } if incoming.is_empty()))
        );
        out.clear();
        a.dispatch("PRESENCE_UPDATE", &json!({"user_id":"200", "status":"idle"}), &mut out);
        assert!(out.iter().any(|event| matches!(event, Event::Presences { presence, .. } if presence == &vec![(UserId::new(a.id().unwrap().clone(), "200"), godwit_core::members::Presence::Idle)])));
        assert!(
            out.iter()
                .any(|event| matches!(event, Event::Friends { people, .. } if people[0].online))
        );
    }

    #[test]
    fn loading_members_requests_the_first_lazy_range() {
        let (mut a, mut cmds, _) = account();
        a.dispatch("READY", &ready(), &mut Vec::new());
        while cmds.try_recv().is_ok() {}
        let room = RoomId::new(a.id().unwrap().clone(), "12");
        a.run(Command::LoadMembers { room });
        let Ok(Cmd::Gateway(op, payload)) = cmds.try_recv() else {
            panic!("member range was not requested")
        };
        assert_eq!(op, crate::gateway::LAZY_REQUEST);
        assert_eq!(
            payload["subscriptions"]["10"]["member_list_channels"]["12"][0],
            json!([0, 99])
        );
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
        a.run(Command::CancelSend {
            room: room.clone(),
            nonce: "n1".into(),
        });
        assert!(matches!(cmds.try_recv(), Ok(Cmd::CancelSend(n)) if n == "n1"));
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
    fn partial_message_update_applies_content_and_embeds_without_author() {
        let (mut a, _cmds, _tx) = account();
        a.dispatch("READY", &ready(), &mut Vec::new());
        let mut out = Vec::new();
        a.dispatch(
            "MESSAGE_UPDATE",
            &json!({
                "id": "m1", "channel_id": "20", "content": "edited",
                "edited_timestamp": "2026-11-10T10:00:00.000Z",
                "embeds": [{"type": "rich", "title": "Preview", "url": "https://example.test"}]
            }),
            &mut out,
        );
        assert!(matches!(&out[0], Event::MessageEdited { id, source, .. } if &*id.0 == "m1" && &**source == "edited"));
        assert!(
            matches!(&out[1], Event::EmbedsUpdated { id, embeds, .. } if &*id.0 == "m1" && embeds[0].title.as_deref() == Some("Preview"))
        );
    }

    #[test]
    fn single_and_bulk_deletes_emit_each_id() {
        let (mut a, _cmds, _tx) = account();
        a.dispatch("READY", &ready(), &mut Vec::new());
        let mut out = Vec::new();
        a.dispatch("MESSAGE_DELETE", &json!({"channel_id":"20", "id":"one"}), &mut out);
        a.dispatch(
            "MESSAGE_DELETE_BULK",
            &json!({"channel_id":"20", "ids":["two", "three"]}),
            &mut out,
        );
        let ids: Vec<&str> = out
            .iter()
            .filter_map(|event| match event {
                Event::MessageDeleted { id, .. } => Some(id.0.as_ref()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["one", "two", "three"]);
    }

    #[test]
    fn history_messages_are_parsed_oldest_first() {
        let (mut a, _cmds, _tx) = account();
        a.dispatch("READY", &ready(), &mut Vec::new());
        let mut out = Vec::new();
        a.answer(Answer::History {
            channel: "20".into(), before: None,
            result: Ok(json!([
                {"id":"1501320000000000002", "channel_id":"20", "type":0, "content":"new", "author":{"id":"200","username":"ada"}},
                {"id":"1501320000000000001", "channel_id":"20", "type":0, "content":"old", "author":{"id":"200","username":"ada"}}
            ])),
        }, &mut out);
        let Some(Event::History { messages, .. }) = out.first() else {
            panic!("{out:?}")
        };
        assert_eq!(
            messages.iter().map(|m| m.source.as_ref()).collect::<Vec<_>>(),
            ["old", "new"]
        );
    }

    #[test]
    fn snowflakes_count_from_2015() {
        // docs.fluxer.com: timestamp_ms = (snowflake >> 22) + 1420070400000.
        assert_eq!(snowflake_ms(&((1_000u64) << 22).to_string()), EPOCH_MS + 1_000);
    }
}
