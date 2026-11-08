// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! What Godwit knows of a Discord account: from READY (with the web client's
//! capabilities: deduplicated users, guilds with their channels and roles,
//! our own member objects in `merged_members`, read states, server settings)
//! and the events after it. Only what the views need is kept: messages
//! themselves live in the core's timelines.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::permissions::{self, Overwrite, bits, overwrites};

/// Channel types (<https://docs.discord.food/resources/channel>).
pub const GUILD_TEXT: u64 = 0;
pub const DM: u64 = 1;
pub const GUILD_VOICE: u64 = 2;
pub const GROUP_DM: u64 = 3;
pub const GUILD_CATEGORY: u64 = 4;
pub const GUILD_ANNOUNCEMENT: u64 = 5;
pub const GUILD_STAGE_VOICE: u64 = 13;
pub const GUILD_FORUM: u64 = 15;
/// A forum of media posts (shown as a forum).
pub const GUILD_MEDIA: u64 = 16;
/// Threads: in an announcement channel, public, private.
pub const ANNOUNCEMENT_THREAD: u64 = 10;
pub const PUBLIC_THREAD: u64 = 11;
pub const PRIVATE_THREAD: u64 = 12;

/// Relationship types.
pub const FRIEND: u64 = 1;
pub const BLOCKED: u64 = 2;
pub const INCOMING: u64 = 3;
pub const OUTGOING: u64 = 4;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct User {
    pub id: String,
    pub username: String,
    pub global_name: Option<String>,
    pub avatar: Option<String>,
    pub bot: bool,
}

impl User {
    pub fn from(v: &Value) -> Option<Self> {
        Some(Self {
            id: v["id"].as_str()?.to_owned(),
            username: v["username"].as_str().unwrap_or_default().to_owned(),
            global_name: v["global_name"].as_str().filter(|n| !n.is_empty()).map(str::to_owned),
            avatar: v["avatar"].as_str().map(str::to_owned),
            bot: v["bot"] == true,
        })
    }

    /// The name Discord shows: their display name, else their username.
    pub fn name(&self) -> &str {
        self.global_name.as_deref().unwrap_or(&self.username)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Role {
    pub id: String,
    pub name: String,
    pub color: u32,
    pub position: i64,
    pub permissions: u64,
    pub hoist: bool,
    pub managed: bool,
}

impl Role {
    fn from(v: &Value) -> Option<Self> {
        Some(Self {
            id: v["id"].as_str()?.to_owned(),
            name: v["name"].as_str().unwrap_or_default().to_owned(),
            color: v["color"]
                .as_u64()
                .and_then(|c| u32::try_from(c).ok())
                .unwrap_or_default(),
            position: v["position"].as_i64().unwrap_or_default(),
            permissions: bits(&v["permissions"]),
            hoist: v["hoist"] == true,
            managed: v["managed"] == true,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Channel {
    pub id: String,
    pub kind: u64,
    pub guild: Option<String>,
    pub name: String,
    pub position: i64,
    pub parent: Option<String>,
    pub overwrites: Vec<Overwrite>,
    pub last_message_id: Option<String>,
    /// DMs and group DMs: everyone in it besides us.
    pub recipients: Vec<String>,
    pub icon: Option<String>,
    /// A thread that's archived (closed): it isn't listed.
    pub archived: bool,
    /// Discord channel.user_limit: 0 is unlimited, absent is unknown.
    pub user_limit: Option<u32>,
    /// A DM from someone who isn't a friend, waiting to be accepted (or
    /// flagged as spam): Discord's message requests.
    pub request: bool,
    /// Channel.topic (a forum's guidelines).
    pub topic: String,
    /// Channel.nsfw: age-restricted.
    pub nsfw: bool,
    /// Channel.rate_limit_per_user: slowmode seconds (0: off).
    pub slowmode: u32,
}

impl Channel {
    pub fn from(v: &Value, guild: Option<&str>) -> Option<Self> {
        let recipients = match v["recipient_ids"].as_array() {
            Some(ids) => ids.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            None => v["recipients"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|u| u["id"].as_str().map(str::to_owned))
                .collect(),
        };
        Some(Self {
            id: v["id"].as_str()?.to_owned(),
            kind: v["type"].as_u64().unwrap_or_default(),
            guild: v["guild_id"].as_str().or(guild).map(str::to_owned),
            name: v["name"].as_str().unwrap_or_default().to_owned(),
            position: v["position"].as_i64().unwrap_or_default(),
            parent: v["parent_id"].as_str().map(str::to_owned),
            overwrites: overwrites(v),
            last_message_id: v["last_message_id"].as_str().map(str::to_owned),
            recipients,
            icon: v["icon"].as_str().map(str::to_owned),
            archived: v["thread_metadata"]["archived"] == true,
            user_limit: v["user_limit"].as_u64().and_then(|n| n.try_into().ok()),
            request: v["is_message_request"] == true || v["is_spam"] == true,
            topic: v["topic"].as_str().unwrap_or_default().to_owned(),
            nsfw: v["nsfw"] == true,
            slowmode: v["rate_limit_per_user"]
                .as_u64()
                .and_then(|n| n.try_into().ok())
                .unwrap_or(0),
        })
    }

    /// A thread (or a forum's post): its `parent` is the channel it's in.
    pub fn thread(&self) -> bool {
        matches!(self.kind, ANNOUNCEMENT_THREAD | PUBLIC_THREAD | PRIVATE_THREAD)
    }

    /// A forum: no messages of its own, its posts are threads.
    pub fn forum(&self) -> bool {
        matches!(self.kind, GUILD_FORUM | GUILD_MEDIA)
    }

    pub fn private(&self) -> bool {
        matches!(self.kind, DM | GROUP_DM)
    }

    /// A channel messages are read and written in (text, announcements, and
    /// voice channels' chat).
    pub fn has_messages(&self) -> bool {
        matches!(
            self.kind,
            GUILD_TEXT
                | GUILD_ANNOUNCEMENT
                | GUILD_VOICE
                | GUILD_STAGE_VOICE
                | DM
                | GROUP_DM
                | ANNOUNCEMENT_THREAD
                | PUBLIC_THREAD
                | PRIVATE_THREAD
        )
    }

    pub fn voice(&self) -> bool {
        matches!(self.kind, GUILD_VOICE | GUILD_STAGE_VOICE)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Guild {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub banner: Option<String>,
    pub description: Option<String>,
    pub owner: String,
    pub roles: HashMap<String, Role>,
    pub channels: Vec<String>,
    /// Custom emoji: id → (name, animated).
    pub emojis: HashMap<String, (String, bool)>,
    /// Who may use each one (id → rule), for the emoji picker.
    pub emoji_rules: HashMap<String, EmojiRule>,
    pub system_channel: Option<String>,
    /// Its boost level (0–3); level 2 and 3 raise the upload limit.
    pub premium_tier: u8,
    /// What it notifies of when we haven't chosen: 0 all messages, 1 mentions.
    pub default_notify: u8,
}

/// What limits a custom emoji's use: `available` false when the server lost
/// the boost it needs; `roles` (when any) the only roles that may use it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmojiRule {
    pub available: bool,
    pub roles: Vec<String>,
    /// Who uploaded it (`user.id`, sent when we may manage expressions).
    pub creator: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Member {
    pub roles: Vec<String>,
    pub nick: Option<String>,
    pub avatar: Option<String>,
    /// `communication_disabled_until`, as unix seconds: timed out until then.
    pub timeout_until: Option<i64>,
}

impl Member {
    /// Whether the timeout is still running at `now` (unix seconds).
    pub fn timed_out(&self, now: i64) -> bool {
        self.timeout_until.is_some_and(|t| t > now)
    }

    fn from(v: &Value) -> Self {
        Self {
            roles: v["roles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            nick: v["nick"].as_str().filter(|n| !n.is_empty()).map(str::to_owned),
            avatar: v["avatar"].as_str().map(str::to_owned),
            timeout_until: v["communication_disabled_until"]
                .as_str()
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.timestamp()),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadState {
    pub last_message_id: Option<String>,
    pub mentions: u32,
}

#[derive(Default)]
pub struct Store {
    /// Whether the presence shares activities, as the settings last said.
    pub show_game: Option<bool>,
    pub me: Option<User>,
    pub users: HashMap<String, User>,
    /// In READY's order (the folder order from settings comes later).
    pub guild_order: Vec<String>,
    pub guilds: HashMap<String, Guild>,
    pub channels: HashMap<String, Channel>,
    /// Members as seen: (guild, user).
    pub members: HashMap<(String, String), Member>,
    pub read: HashMap<String, ReadState>,
    /// user → relationship type.
    pub relationships: HashMap<String, u64>,
    /// user → status ("online", "idle", "dnd", "offline").
    pub presences: HashMap<String, String>,
    /// user → what they're doing ("Playing …"), from their presence's
    /// activities; absent when nothing (or only a custom status).
    pub activities: HashMap<String, String>,
    pub muted_guilds: HashSet<String>,
    /// Our notification level per server, as `user_guild_settings` keeps it
    /// (0 all, 1 mentions, 2 nothing, 3 the server's default).
    pub guild_notify: HashMap<String, u8>,
    /// A channel's own level where we set one (0 all, 1 mentions, 2
    /// nothing; 3 or missing: the server's).
    pub channel_notify: HashMap<String, u8>,
    pub muted_channels: HashSet<String>,
    /// Our custom status text, as last set.
    pub custom_status: Option<String>,
    pub status: String,
    /// Our Nitro (`premium_type`: 0 none, 1 Classic, 2 Nitro, 3 Basic): it
    /// sets how large an upload may be.
    pub premium_type: u8,
}

impl Store {
    /// Our notification level in `guild`: 0 all messages, 1 only mentions,
    /// 2 nothing (our choice, else the server's default).
    /// Our level in a channel: its own override, else its parent's (a
    /// thread follows its channel), else the server's.
    pub fn channel_notify_level(&self, channel: &str) -> u8 {
        if let Some(level @ 0..=2) = self.channel_notify.get(channel).copied() {
            return level;
        }
        let Some(c) = self.channels.get(channel) else { return 1 };
        if let Some(parent) = c.parent.as_deref().filter(|p| *p != channel)
            && let Some(level @ 0..=2) = self.channel_notify.get(parent).copied()
        {
            return level;
        }
        c.guild.as_deref().map_or(0, |g| self.notify_level(g))
    }

    pub fn notify_level(&self, guild: &str) -> u8 {
        match self.guild_notify.get(guild).copied() {
            Some(level @ 0..=2) => level,
            _ => self.guilds.get(guild).map_or(1, |g| g.default_notify.min(1)),
        }
    }
}

/// Snowflakes compare by value: a longer one is newer.
pub fn newer(a: &str, b: &str) -> bool {
    (a.len(), a) > (b.len(), b)
}

impl Store {
    /// Takes READY's data.
    pub fn ready(&mut self, d: &Value) {
        *self = Self::default();
        self.me = User::from(&d["user"]);
        self.premium_type = d["user"]["premium_type"]
            .as_u64()
            .and_then(|p| u8::try_from(p).ok())
            .unwrap_or(0);
        for u in d["users"].as_array().into_iter().flatten() {
            self.put_user(u);
        }
        if let Some(me) = self.me.clone() {
            self.users.insert(me.id.clone(), me);
        }
        let merged = d["merged_members"].as_array();
        let me = self.me.as_ref().map(|u| u.id.clone()).unwrap_or_default();
        for (i, g) in d["guilds"].as_array().into_iter().flatten().enumerate() {
            self.put_guild(g);
            // merged_members[i] holds our member object in guild i.
            if let (Some(id), Some(members)) = (g["id"].as_str(), merged.and_then(|m| m.get(i))) {
                for m in members.as_array().into_iter().flatten() {
                    let user = m["user_id"].as_str().unwrap_or(&me).to_owned();
                    self.members.insert((id.to_owned(), user), Member::from(m));
                }
            }
        }
        for c in d["private_channels"].as_array().into_iter().flatten() {
            if let Some(c) = Channel::from(c, None) {
                self.channels.insert(c.id.clone(), c);
            }
        }
        for r in d["relationships"].as_array().into_iter().flatten() {
            if let (Some(id), Some(kind)) = (r["id"].as_str(), r["type"].as_u64()) {
                self.relationships.insert(id.to_owned(), kind);
            }
            if let Some(u) = r.get("user") {
                self.put_user(u);
            }
        }
        // read_state is { entries, version } with the capabilities sent.
        let reads = d["read_state"]["entries"]
            .as_array()
            .or_else(|| d["read_state"].as_array());
        for r in reads.into_iter().flatten() {
            // Entries without a type (or type 0) are channels'.
            if r["read_state_type"].as_u64().unwrap_or(0) != 0 {
                continue;
            }
            if let Some(id) = r["id"].as_str() {
                self.read.insert(
                    id.to_owned(),
                    ReadState {
                        last_message_id: r["last_message_id"].as_str().map(str::to_owned),
                        mentions: r["mention_count"]
                            .as_u64()
                            .and_then(|m| u32::try_from(m).ok())
                            .unwrap_or_default(),
                    },
                );
            }
        }
        let settings = d["user_guild_settings"]["entries"]
            .as_array()
            .or_else(|| d["user_guild_settings"].as_array());
        for s in settings.into_iter().flatten() {
            let guild = s["guild_id"].as_str();
            if let (Some(g), Some(level)) = (
                guild,
                s["message_notifications"].as_u64().and_then(|l| u8::try_from(l).ok()),
            ) {
                self.guild_notify.insert(g.to_owned(), level);
            }
            if s["muted"] == true
                && let Some(g) = guild
            {
                self.muted_guilds.insert(g.to_owned());
            }
            for o in s["channel_overrides"].as_array().into_iter().flatten() {
                if let (Some(c), Some(level)) = (
                    o["channel_id"].as_str(),
                    o["message_notifications"].as_u64().and_then(|l| u8::try_from(l).ok()),
                ) {
                    self.channel_notify.insert(c.to_owned(), level);
                }
                if o["muted"] == true
                    && let Some(c) = o["channel_id"].as_str()
                {
                    self.muted_channels.insert(c.to_owned());
                }
            }
        }
        self.status = d["sessions"][0]["status"].as_str().unwrap_or("online").to_owned();
        // Presences usually come in READY_SUPPLEMENTAL; take any READY has.
        self.supplemental(d);
        if let Some(proto) = d["user_settings_proto"].as_str() {
            self.apply_settings(proto);
        }
    }

    /// The account's settings (base64 protobuf): status, custom status and
    /// the server list's order (servers it doesn't place go first, as new
    /// ones do in Discord's list).
    pub fn apply_settings(&mut self, proto: &str) {
        let read = crate::settings::read(proto);
        if read.show_game.is_some() {
            self.show_game = read.show_game;
        }
        if let Some(status) = read.status.filter(|s| !s.is_empty()) {
            self.status = status;
        }
        if let Some(custom) = read.custom {
            self.custom_status = custom;
        }
        if let Some(order) = read.guild_order {
            let mut placed: Vec<String> = order.into_iter().filter(|g| self.guilds.contains_key(g)).collect();
            let loose: Vec<String> = self
                .guild_order
                .iter()
                .filter(|g| !placed.contains(g))
                .cloned()
                .collect();
            placed.splice(0..0, loose);
            self.guild_order = placed;
        }
    }

    /// READY_SUPPLEMENTAL: friends' and servers' presences.
    pub fn supplemental(&mut self, d: &Value) {
        // PRIORITIZED_READY_PAYLOAD moves other members here, while
        // DEDUPE_USER_OBJECTS can replace their embedded users with ids.
        // Consume the supplied objects; don't fetch each voice participant.
        for user in d["users"].as_array().into_iter().flatten() {
            self.put_user(user);
        }
        for (index, guild) in d["guilds"].as_array().into_iter().flatten().enumerate() {
            let Some(id) = guild["id"].as_str() else { continue };
            for member in d["merged_members"][index].as_array().into_iter().flatten() {
                self.put_member(id, member);
            }
            for state in guild["voice_states"].as_array().into_iter().flatten() {
                if let Some(member) = state.get("member") {
                    let mut member = member.clone();
                    member["user_id"] = state["user_id"].clone();
                    self.put_member(id, &member);
                }
            }
        }
        for p in d["merged_presences"]["friends"].as_array().into_iter().flatten() {
            self.put_presence(p);
        }
        for list in d["merged_presences"]["guilds"].as_array().into_iter().flatten() {
            for p in list.as_array().into_iter().flatten() {
                self.put_presence(p);
            }
        }
    }

    pub fn put_presence(&mut self, p: &Value) {
        let user = p["user_id"].as_str().or_else(|| p["user"]["id"].as_str());
        if let (Some(user), Some(status)) = (user, p["status"].as_str()) {
            self.presences.insert(user.to_owned(), status.to_owned());
            // A presence carries the whole list: none now means none.
            match activity(&p["activities"]).filter(|_| status != "offline") {
                Some(text) => self.activities.insert(user.to_owned(), text),
                None => self.activities.remove(user),
            };
        }
    }

    pub fn put_user(&mut self, v: &Value) {
        if let Some(u) = User::from(v) {
            self.users.insert(u.id.clone(), u);
        }
    }

    /// A guild object from READY or GUILD_CREATE (fields may sit under
    /// `properties` in READY's form).
    pub fn put_guild(&mut self, g: &Value) {
        let Some(id) = g["id"].as_str() else { return };
        if g["unavailable"] == true {
            return;
        }
        let p = if g["properties"].is_object() {
            &g["properties"]
        } else {
            g
        };
        let mut guild = Guild {
            id: id.to_owned(),
            name: p["name"].as_str().unwrap_or_default().to_owned(),
            icon: p["icon"].as_str().map(str::to_owned),
            banner: p["banner"].as_str().map(str::to_owned),
            description: p["description"].as_str().map(str::to_owned),
            owner: p["owner_id"].as_str().unwrap_or_default().to_owned(),
            system_channel: p["system_channel_id"].as_str().map(str::to_owned),
            default_notify: p["default_message_notifications"]
                .as_u64()
                .or_else(|| g["default_message_notifications"].as_u64())
                .and_then(|n| u8::try_from(n).ok())
                .unwrap_or(1),
            // In READY it can sit beside `properties` rather than in it.
            premium_tier: p["premium_tier"]
                .as_u64()
                .or_else(|| g["premium_tier"].as_u64())
                .and_then(|t| u8::try_from(t).ok())
                .unwrap_or(0),
            ..Default::default()
        };
        for r in g["roles"].as_array().into_iter().flatten() {
            if let Some(r) = Role::from(r) {
                guild.roles.insert(r.id.clone(), r);
            }
        }
        put_emojis(&mut guild, &g["emojis"]);
        // READY and GUILD_CREATE list the active threads we can see beside the channels.
        for c in g["channels"]
            .as_array()
            .into_iter()
            .chain(g["threads"].as_array())
            .flatten()
        {
            if let Some(c) = Channel::from(c, Some(id)) {
                guild.channels.push(c.id.clone());
                self.channels.insert(c.id.clone(), c);
            }
        }
        if !self.guild_order.iter().any(|g| g == id) {
            self.guild_order.push(id.to_owned());
        }
        self.guilds.insert(id.to_owned(), guild);
    }

    /// GUILD_UPDATE: the guild's own fields, keeping its channels.
    pub fn update_guild(&mut self, g: &Value) {
        let Some(id) = g["id"].as_str() else { return };
        let Some(guild) = self.guilds.get_mut(id) else { return };
        if let Some(name) = g["name"].as_str() {
            guild.name = name.to_owned();
        }
        guild.icon = g["icon"].as_str().map(str::to_owned);
        guild.banner = g["banner"].as_str().map(str::to_owned);
        guild.description = g["description"].as_str().map(str::to_owned);
        if let Some(owner) = g["owner_id"].as_str() {
            guild.owner = owner.to_owned();
        }
        guild.system_channel = g["system_channel_id"].as_str().map(str::to_owned);
        if let Some(tier) = g["premium_tier"].as_u64().and_then(|t| u8::try_from(t).ok()) {
            guild.premium_tier = tier;
        }
        if let Some(roles) = g["roles"].as_array() {
            guild.roles = roles.iter().filter_map(Role::from).map(|r| (r.id.clone(), r)).collect();
        }
    }

    pub fn remove_guild(&mut self, id: &str) {
        if let Some(g) = self.guilds.remove(id) {
            for c in g.channels {
                self.channels.remove(&c);
            }
        }
        self.guild_order.retain(|g| g != id);
        self.members.retain(|(g, _), _| g != id);
    }

    pub fn put_channel(&mut self, v: &Value) {
        let Some(c) = Channel::from(v, None) else { return };
        if let Some(g) = c.guild.as_ref().and_then(|g| self.guilds.get_mut(g))
            && !g.channels.contains(&c.id)
        {
            g.channels.push(c.id.clone());
        }
        // An update keeps the last message we know of.
        let last = self.channels.get(&c.id).and_then(|old| old.last_message_id.clone());
        let mut c = c;
        if c.last_message_id.is_none() {
            c.last_message_id = last;
        }
        self.channels.insert(c.id.clone(), c);
    }

    pub fn remove_channel(&mut self, id: &str) {
        if let Some(c) = self.channels.remove(id)
            && let Some(g) = c.guild.as_ref().and_then(|g| self.guilds.get_mut(g))
        {
            g.channels.retain(|x| x != id);
        }
    }

    pub fn put_role(&mut self, guild: &str, v: &Value) {
        if let (Some(g), Some(r)) = (self.guilds.get_mut(guild), Role::from(v)) {
            g.roles.insert(r.id.clone(), r);
        }
    }

    pub fn put_member(&mut self, guild: &str, v: &Value) {
        let user = v["user"]["id"].as_str().or_else(|| v["user_id"].as_str());
        if let Some(u) = v.get("user") {
            self.put_user(u);
        }
        if let Some(user) = user {
            self.members
                .insert((guild.to_owned(), user.to_owned()), Member::from(v));
        }
    }

    /// A new message: its channel's newest, and its author (and member) seen.
    /// Returns whether it mentions us.
    pub fn message_created(&mut self, m: &Value) -> bool {
        if let Some(author) = m.get("author") {
            self.put_user(author);
        }
        if let (Some(guild), Some(member), Some(author)) =
            (m["guild_id"].as_str(), m.get("member"), m["author"]["id"].as_str())
        {
            let mut member = member.clone();
            member["user_id"] = Value::String(author.to_owned());
            self.put_member(guild, &member);
        }
        let (Some(channel), Some(id)) = (m["channel_id"].as_str(), m["id"].as_str()) else {
            return false;
        };
        if let Some(c) = self.channels.get_mut(channel)
            && c.last_message_id.as_deref().is_none_or(|last| newer(id, last))
        {
            c.last_message_id = Some(id.to_owned());
        }
        let me = self.me.as_ref().map(|u| u.id.as_str()).unwrap_or_default();
        let mine = m["author"]["id"].as_str() == Some(me);
        if mine {
            // Our own messages are read.
            self.mark_read(channel, id);
            return false;
        }
        let mentioned = m["mention_everyone"] == true
            || m["mentions"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|u| u["id"].as_str() == Some(me))
            || self.channels.get(channel).is_some_and(|c| c.kind == DM);
        if mentioned {
            self.read.entry(channel.to_owned()).or_default().mentions += 1;
        }
        mentioned
    }

    pub fn mark_read(&mut self, channel: &str, message: &str) {
        let entry = self.read.entry(channel.to_owned()).or_default();
        if entry
            .last_message_id
            .as_deref()
            .is_none_or(|last| newer(message, last) || last == message)
        {
            entry.last_message_id = Some(message.to_owned());
            entry.mentions = 0;
        }
    }

    pub fn unread(&self, channel: &str) -> bool {
        let Some(last) = self.channels.get(channel).and_then(|c| c.last_message_id.as_deref()) else {
            return false;
        };
        match self.read.get(channel).and_then(|r| r.last_message_id.as_deref()) {
            Some(read) => newer(last, read),
            // Never read: unread only if it's a DM (servers start read).
            None => self.channels.get(channel).is_some_and(Channel::private),
        }
    }

    pub fn mentions(&self, channel: &str) -> u32 {
        self.read.get(channel).map_or(0, |r| r.mentions)
    }

    /// Our permissions in a server.
    pub fn guild_permissions(&self, guild: &str) -> u64 {
        let (Some(g), Some(me)) = (self.guilds.get(guild), self.me.as_ref()) else {
            return 0;
        };
        let member = self.members.get(&(guild.to_owned(), me.id.clone()));
        let everyone = g.roles.get(guild).map_or(0, |r| r.permissions);
        let roles = member
            .into_iter()
            .flat_map(|m| &m.roles)
            .filter_map(|r| g.roles.get(r))
            .map(|r| r.permissions);
        permissions::base(g.owner == me.id, everyone, roles)
    }

    /// Our permissions in a channel (everything in DMs).
    pub fn channel_permissions(&self, channel: &str) -> u64 {
        let Some(c) = self.channels.get(channel) else { return 0 };
        // A thread has no overwrites of its own: its parent channel's apply.
        if c.thread()
            && let Some(parent) = c.parent.as_deref().filter(|p| *p != channel)
        {
            return self.channel_permissions(parent);
        }
        let Some(guild) = c.guild.as_deref() else {
            return permissions::ALL;
        };
        let Some(me) = self.me.as_ref() else { return 0 };
        let base = self.guild_permissions(guild);
        let roles = self
            .members
            .get(&(guild.to_owned(), me.id.clone()))
            .map(|m| m.roles.clone())
            .unwrap_or_default();
        // A channel in a category takes the category's overwrites only when
        // it's synced; Discord sends each channel's own, which is what counts.
        permissions::channel(base, guild, &me.id, &roles, &c.overwrites)
    }

    /// Why video is or isn't allowed in `channel`, for the log: the
    /// channel's kind, our permission bits there and in the server, the
    /// @everyone role's, how many of our roles are known, and the channel's
    /// overwrites (whose STREAM bit they allow or deny).
    pub fn video_reason(&self, channel: &str) -> String {
        let Some(c) = self.channels.get(channel) else {
            return "unknown channel".into();
        };
        let Some(guild) = c.guild.as_deref() else {
            return "video allowed (a DM call)".into();
        };
        let stream = permissions::STREAM;
        let here = self.channel_permissions(channel);
        let server = self.guild_permissions(guild);
        let g = self.guilds.get(guild);
        let everyone = g.and_then(|g| g.roles.get(guild)).map(|r| r.permissions);
        let me = self.me.as_ref().map(|m| m.id.clone()).unwrap_or_default();
        let mine = self.members.get(&(guild.to_owned(), me));
        let (roles, found) = mine.map_or((0, 0), |m| {
            (
                m.roles.len(),
                m.roles
                    .iter()
                    .filter(|r| g.is_some_and(|g| g.roles.contains_key(*r)))
                    .count(),
            )
        });
        let overwrites: Vec<String> = c
            .overwrites
            .iter()
            .filter(|o| (o.allow | o.deny) & stream != 0)
            .map(|o| {
                let who = if o.member {
                    "member"
                } else if o.id == guild {
                    "@everyone"
                } else {
                    "role"
                };
                format!("{who} {}", if o.allow & stream != 0 { "allows" } else { "denies" })
            })
            .collect();
        format!(
            "video {} (kind {}, Video bit here {}, in the server {}, @everyone {:?}, our roles {found}/{roles} known, owner {}, overwrites on Video: {:?}; bits here {here:#x})",
            if here & stream != 0 { "allowed" } else { "not allowed" },
            c.kind,
            here & stream != 0,
            server & stream != 0,
            everyone.map(|e| e & stream != 0),
            g.is_some_and(|g| self.me.as_ref().is_some_and(|m| m.id == g.owner)),
            overwrites,
        )
    }

    /// The tiers Discord says this account and server have, for the log
    /// when our video starts: Godwit sends the free 720p30 whatever they
    /// are, and says so where they'd allow more (a tripwire: a line saying
    /// "Nitro" or "boosted" next to a size past 720p would be a bug).
    pub fn tier_tripwire(&self, guild: Option<&str>) -> String {
        let account = match self.premium_type {
            0 => "no Nitro",
            1 => "Nitro Classic",
            2 => "Nitro",
            3 => "Nitro Basic",
            _ => "an unknown Nitro tier",
        };
        let boost = guild.and_then(|g| self.guilds.get(g)).map_or(0, |g| g.premium_tier);
        let more = self.premium_type != 0 || boost >= 2;
        format!(
            "account has {account} (premium_type {}), server boost level {boost}; sending Discord's free 1280x720 at 30 fps{}",
            self.premium_type,
            if more { " anyway (higher tiers aren't used)" } else { "" }
        )
    }

    /// The id of the member list Discord streams for `channel`, as its
    /// GUILD_MEMBER_LIST_UPDATE names it: "everyone" where the whole server
    /// sees the channel, else the MurmurHash3 of who may and mayn't see it
    /// (Discord's client's rule, as Yapfast's `readers`). Channels with the
    /// same readers share a list; a thread lists its channel's.
    pub fn member_list_id(&self, channel: &str) -> String {
        let Some(mut c) = self.channels.get(channel) else {
            return format!("channel:{channel}");
        };
        if c.thread()
            && let Some(parent) = c.parent.as_deref().and_then(|p| self.channels.get(p))
        {
            c = parent;
        }
        let view = permissions::VIEW_CHANNEL;
        // Without the server's roles, @everyone is taken to see channels,
        // as it does on most servers.
        let everyone_sees = c
            .guild
            .as_deref()
            .and_then(|g| self.guilds.get(g).and_then(|guild| guild.roles.get(g)))
            .is_none_or(|r| r.permissions & view != 0);
        let hidden = c.overwrites.iter().any(|o| o.deny & view != 0);
        if everyone_sees && !hidden {
            return "everyone".into();
        }
        let mut readers: Vec<String> = c
            .overwrites
            .iter()
            .filter_map(|o| {
                if o.allow & view != 0 {
                    Some(format!("allow:{}", o.id))
                } else if o.deny & view != 0 {
                    Some(format!("deny:{}", o.id))
                } else {
                    None
                }
            })
            .collect();
        readers.sort();
        murmur3(readers.join(",").as_bytes()).to_string()
    }

    pub fn can(&self, channel: &str, permission: u64) -> bool {
        self.channel_permissions(channel) & permission == permission
    }

    /// Someone's name in a server: their nickname there, else their name.
    pub fn display_name(&self, user: &str, guild: Option<&str>) -> String {
        if let Some(nick) = guild
            .and_then(|g| self.members.get(&(g.to_owned(), user.to_owned())))
            .and_then(|m| m.nick.clone())
        {
            return nick;
        }
        self.users
            .get(user)
            .map_or_else(|| user.to_owned(), |u| u.name().to_owned())
    }

    /// A member's highest coloured role's colour.
    pub fn colour(&self, user: &str, guild: &str) -> Option<[u8; 3]> {
        let g = self.guilds.get(guild)?;
        let m = self.members.get(&(guild.to_owned(), user.to_owned()))?;
        let role = m
            .roles
            .iter()
            .filter_map(|r| g.roles.get(r))
            .filter(|r| r.color != 0)
            .max_by_key(|r| r.position)?;
        let c = role.color;
        Some([(c >> 16) as u8, (c >> 8) as u8, c as u8])
    }

    /// Whether one of their roles (or @everyone, whose id is the guild's)
    /// grants Administrator; Discord refuses to time such members out.
    pub fn is_admin(&self, guild: &str, user: &str) -> bool {
        let Some(g) = self.guilds.get(guild) else { return false };
        let member = self.members.get(&(guild.to_owned(), user.to_owned()));
        member
            .into_iter()
            .flat_map(|m| &m.roles)
            .chain(std::iter::once(&guild.to_owned()))
            .filter_map(|r| g.roles.get(r))
            .any(|r| r.permissions & permissions::ADMINISTRATOR != 0)
    }

    /// The highest position among a member's roles (0 for none; the owner highest).
    pub fn rank(&self, guild: &str, user: &str) -> i64 {
        let Some(g) = self.guilds.get(guild) else { return 0 };
        if g.owner == user {
            return i64::MAX;
        }
        self.members
            .get(&(guild.to_owned(), user.to_owned()))
            .into_iter()
            .flat_map(|m| &m.roles)
            .filter_map(|r| g.roles.get(r))
            .map(|r| r.position)
            .max()
            .unwrap_or(0)
    }
}

/// Discord's CDN.
pub const CDN: &str = "https://cdn.discordapp.com";
/// The size pictures are asked for.
const PICTURE: u32 = 128;

pub fn avatar_url(user: &str, hash: Option<&str>) -> String {
    match hash {
        Some(hash) => format!("{CDN}/avatars/{user}/{hash}.png?size={PICTURE}"),
        None => {
            // Discord's default avatars, picked from the account's id.
            let index = user.parse::<u64>().map_or(0, |id| (id >> 22) % 6);
            format!("{CDN}/embed/avatars/{index}.png")
        }
    }
}

pub fn guild_icon_url(guild: &str, hash: &str) -> String {
    format!("{CDN}/icons/{guild}/{hash}.png?size={PICTURE}")
}

pub fn guild_banner_url(guild: &str, hash: &str) -> String {
    format!("{CDN}/banners/{guild}/{hash}.png?size=600")
}

pub fn member_avatar_url(guild: &str, user: &str, hash: &str) -> String {
    format!("{CDN}/guilds/{guild}/users/{user}/avatars/{hash}.png?size={PICTURE}")
}

/// A server's custom emoji list (READY, GUILD_CREATE, GUILD_EMOJIS_UPDATE),
/// replacing what was known.
pub fn put_emojis(guild: &mut Guild, list: &Value) {
    // READY and gateway updates may lack `user`: keep who we knew.
    let known: HashMap<String, Option<String>> = std::mem::take(&mut guild.emoji_rules)
        .into_iter()
        .map(|(id, r)| (id, r.creator))
        .collect();
    guild.emojis.clear();
    for e in list.as_array().into_iter().flatten() {
        put_emoji(guild, e);
        if let Some(rule) = e["id"].as_str().and_then(|id| guild.emoji_rules.get_mut(id))
            && rule.creator.is_none()
        {
            rule.creator = e["id"].as_str().and_then(|id| known.get(id).cloned().flatten());
        }
    }
}

/// One custom emoji object (also Create Guild Emoji's answer), added or replaced.
pub fn put_emoji(guild: &mut Guild, e: &Value) {
    if let (Some(eid), Some(name)) = (e["id"].as_str(), e["name"].as_str()) {
        guild
            .emojis
            .insert(eid.to_owned(), (name.to_owned(), e["animated"] == true));
        let roles = e["roles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        guild.emoji_rules.insert(
            eid.to_owned(),
            EmojiRule {
                available: e["available"] != false,
                roles,
                creator: e["user"]["id"].as_str().map(str::to_owned),
            },
        );
    }
}

pub fn emoji_url(id: &str, animated: bool) -> String {
    let format = if animated { "gif" } else { "png" };
    format!("{CDN}/emojis/{id}.{format}?size=48")
}

pub fn group_icon_url(channel: &str, hash: &str) -> String {
    format!("{CDN}/channel-icons/{channel}/{hash}.png?size={PICTURE}")
}

/// A READY to test with: one server (with a hidden channel and a category),
/// a DM, a friend and read states. Made-up names only.
/// MurmurHash3's 32-bit hash with seed 0, which Discord's client hashes
/// member list ids with (Yapfast's `murmur3`, GPL-3.0-or-later).
fn murmur3(data: &[u8]) -> u32 {
    const C1: u32 = 0xcc9e_2d51;
    const C2: u32 = 0x1b87_3593;
    let scramble = |k: u32| k.wrapping_mul(C1).rotate_left(15).wrapping_mul(C2);
    let mut hash = 0u32;
    let (blocks, tail) = data.as_chunks::<4>();
    for block in blocks {
        let k = u32::from_le_bytes(*block);
        hash = (hash ^ scramble(k))
            .rotate_left(13)
            .wrapping_mul(5)
            .wrapping_add(0xe654_6b64);
    }
    if !tail.is_empty() {
        let k = tail.iter().rev().fold(0, |k, &byte| k << 8 | u32::from(byte));
        hash ^= scramble(k);
    }
    hash ^= u32::try_from(data.len()).unwrap_or(u32::MAX);
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x85eb_ca6b);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(0xc2b2_ae35);
    hash ^ hash >> 16
}

/// What a presence's activities say someone is doing, as Discord's client
/// words it ("Playing X", "Listening to Spotify"); the custom status (type
/// 4) is their status text, not an activity.
pub fn activity(activities: &Value) -> Option<String> {
    activities.as_array()?.iter().find_map(|a| {
        let name = a["name"].as_str().filter(|n| !n.is_empty())?;
        Some(match a["type"].as_u64()? {
            0 => format!("Playing {name}"),
            1 => format!("Streaming {name}"),
            2 => format!("Listening to {name}"),
            3 => format!("Watching {name}"),
            5 => format!("Competing in {name}"),
            _ => return None,
        })
    })
}

#[cfg(test)]
pub(crate) fn test_ready() -> Value {
    serde_json::json!({
        "user": {"id": "100", "username": "me_bloop", "global_name": "Bloop"},
        "users": [{"id": "200", "username": "zarg", "global_name": null, "avatar": "abc"}],
        "guilds": [{
            "id": "10",
            "properties": {"name": "Qux Hall", "owner_id": "200", "icon": "ic"},
            "roles": [
                {"id": "10", "name": "@everyone", "permissions": (permissions::VIEW_CHANNEL | permissions::SEND_MESSAGES).to_string(), "position": 0},
                {"id": "11", "name": "Mods", "permissions": (permissions::KICK_MEMBERS).to_string(), "position": 3, "color": 0x00ff00}
            ],
            "channels": [
                {"id": "12", "type": 4, "name": "Talk", "position": 0},
                {"id": "13", "type": 0, "name": "general", "position": 0, "parent_id": "12", "last_message_id": "500"},
                {"id": "14", "type": 0, "name": "secret", "position": 1, "permission_overwrites": [{"id": "10", "type": 0, "allow": "0", "deny": permissions::VIEW_CHANNEL.to_string()}]}
            ],
            "emojis": [{"id": "77", "name": "blob", "animated": false}]
        }],
        "merged_members": [[{"user_id": "100", "roles": ["11"], "nick": "Bloopy"}]],
        "private_channels": [{"id": "20", "type": 1, "recipient_ids": ["200"], "last_message_id": "600"}],
        "relationships": [{"id": "200", "type": 1}],
        "read_state": {"entries": [{"id": "13", "last_message_id": "499", "mention_count": 2}], "version": 1},
        "user_guild_settings": {"entries": [{"guild_id": "10", "muted": false, "channel_overrides": [{"channel_id": "13", "muted": true}]}]},
        "sessions": [{"status": "idle"}]
    })
}

#[cfg(test)]
mod tests {

    #[test]
    fn activities_read_as_discord_words_them() {
        let list = serde_json::json!([
            {"type": 4, "name": "Custom Status", "state": "brb"},
            {"type": 2, "name": "Spotify"},
            {"type": 0, "name": "Factorio"}
        ]);
        assert_eq!(activity(&list).as_deref(), Some("Listening to Spotify"));
        assert_eq!(
            activity(&serde_json::json!([{"type": 4, "name": "Custom Status"}])),
            None
        );
        assert_eq!(activity(&serde_json::Value::Null), None);
    }
    use super::*;

    #[test]
    fn member_list_ids_hash_like_discords_client() {
        // Discord's ids for these overwrites, as Yapfast checked them.
        assert_eq!(murmur3(b"allow:30,deny:10").to_string(), "3643570923");
        assert_eq!(murmur3(b"deny:10").to_string(), "3007946959");
        assert_eq!(Store::default().member_list_id("nope"), "channel:nope");
    }
    use serde_json::json;

    fn ready() -> Value {
        test_ready()
    }

    #[test]
    fn ready_fills_guilds_channels_reads_and_permissions() {
        let mut s = Store::default();
        s.ready(&ready());
        assert_eq!(s.me.as_ref().map(User::name), Some("Bloop"));
        assert_eq!(s.guilds["10"].name, "Qux Hall");
        assert_eq!(s.guilds["10"].channels.len(), 3);
        assert!(s.unread("13"));
        assert_eq!(s.mentions("13"), 2);
        assert!(s.unread("20"), "a never-read DM with messages");
        assert!(s.muted_channels.contains("13"));
        assert_eq!(s.relationships["200"], FRIEND);
        assert_eq!(s.display_name("100", Some("10")), "Bloopy");
        assert_eq!(s.display_name("200", None), "zarg");
        assert!(s.can("13", permissions::SEND_MESSAGES));
        assert!(!s.can("14", permissions::VIEW_CHANNEL), "hidden from everyone");
        assert!(s.guild_permissions("10") & permissions::KICK_MEMBERS != 0);
        assert_eq!(s.colour("100", "10"), Some([0, 255, 0]));
        assert_eq!(s.status, "idle");
    }

    #[test]
    fn messages_move_reads_and_mentions() {
        let mut s = Store::default();
        s.ready(&ready());
        s.mark_read("13", "500");
        assert!(!s.unread("13"));
        let mentioned = s.message_created(&json!({"id": "501", "channel_id": "13", "guild_id": "10", "author": {"id": "200", "username": "zarg"}, "mentions": [{"id": "100"}]}));
        assert!(mentioned && s.unread("13") && s.mentions("13") == 1);
        s.message_created(&json!({"id": "502", "channel_id": "13", "author": {"id": "100", "username": "me_bloop"}}));
        assert!(!s.unread("13"), "writing reads the channel");
        assert!(newer("1000", "999") && !newer("999", "1000"));
    }
}
