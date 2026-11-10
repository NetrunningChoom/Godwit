// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Client-side cache of everything the websocket and REST calls tell us.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::api::*;

/// The official client's notification preferences (synced settings key
/// `notifications`): per-server/per-channel levels and mutes.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct NotificationPrefs {
    #[serde(default)]
    pub server: HashMap<String, String>,
    #[serde(default)]
    pub channel: HashMap<String, String>,
    #[serde(default)]
    pub server_mutes: HashMap<String, Mute>,
    #[serde(default)]
    pub channel_mutes: HashMap<String, Mute>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Mute {
    /// Muted until this time (ms since the epoch); forever if absent.
    #[serde(default)]
    pub until: Option<i64>,
}

impl NotificationPrefs {
    fn muted(map: &HashMap<String, Mute>, levels: &HashMap<String, String>, id: &str, now_ms: i64) -> bool {
        // Older clients stored "muted" as a level.
        levels.get(id).is_some_and(|l| l == "muted") || map.get(id).is_some_and(|m| m.until.is_none_or(|u| u > now_ms))
    }

    /// Same rules as the official client: muted server or channel → never;
    /// otherwise the channel's level, else the server's, else the default
    /// ("all" for DMs, groups and Saved Notes, "mention" for server channels).
    pub fn should_notify(&self, channel: &Channel, mentions_me: bool, now_ms: i64) -> bool {
        if let Some(s) = &channel.server
            && Self::muted(&self.server_mutes, &self.server, s, now_ms)
        {
            return false;
        }
        if Self::muted(&self.channel_mutes, &self.channel, &channel.id, now_ms) {
            return false;
        }
        let level = self
            .channel
            .get(&channel.id)
            .or_else(|| channel.server.as_ref().and_then(|s| self.server.get(s)))
            .map(String::as_str)
            .unwrap_or(if channel.server.is_some() { "mention" } else { "all" });
        match level {
            "all" => true,
            "mention" => mentions_me,
            _ => false,
        }
    }

    /// The active mute for a server or channel, if any.
    pub fn mute(&self, server: bool, id: &str, now_ms: i64) -> Option<&Mute> {
        let (map, levels) = if server {
            (&self.server_mutes, &self.server)
        } else {
            (&self.channel_mutes, &self.channel)
        };
        static FOREVER: Mute = Mute { until: None };
        if levels.get(id).is_some_and(|l| l == "muted") {
            return Some(&FOREVER);
        }
        map.get(id).filter(|m| m.until.is_none_or(|u| u > now_ms))
    }

    /// Parses the value half of a synced-settings `[timestamp, json]` pair.
    pub fn from_synced(value: &Value) -> Option<Self> {
        serde_json::from_str(value.get(1)?.as_str()?).ok()
    }
}

/// A server folder from the official client's synced `server-folders` key.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ServerFolder {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub colour: Option<String>,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default)]
    pub servers: Vec<String>,
}

/// One entry of the server rail.
#[derive(Debug, Clone, PartialEq)]
pub enum RailEntry {
    Server(String),
    Folder { id: String, servers: Vec<String> },
}

#[derive(Default)]
pub struct ChannelMessages {
    /// Ordered by id; ULIDs sort chronologically.
    pub messages: BTreeMap<String, Message>,
    /// True once a fetch returned fewer messages than requested.
    pub reached_start: bool,
    pub loading: bool,
    pub loaded_once: bool,
    /// Showing older messages around a jump target, not the latest: new
    /// messages aren't appended (that would leave a gap) until "Jump to present".
    pub detached: bool,
    /// After a failed fetch, don't retry before this.
    pub retry_after: Option<Instant>,
}

#[derive(Default)]
pub struct Store {
    pub me: Option<String>,
    pub users: HashMap<String, User>,
    pub servers: HashMap<String, Server>,
    pub server_order: Vec<String>,
    pub channels: HashMap<String, Channel>,
    pub members: HashMap<MemberId, Member>,
    pub emojis: HashMap<String, Emoji>,
    pub notification_prefs: NotificationPrefs,
    /// Profiles fetched this session (None while loading).
    pub profiles: HashMap<String, Option<UserProfile>>,
    /// Mutual ids fetched only after opening that person's profile.
    pub mutuals: HashMap<String, Option<crate::api::UserMutual>>,
    /// Synced server order (`ordering.serverSidebar`, else `ordering.servers`).
    pub sidebar_order: Vec<String>,
    /// The whole synced `server-folders` object, so edits keep fields we don't use.
    pub folders_raw: Option<Value>,
    /// The whole synced `notifications` object, so edits keep fields we don't use.
    pub notifications_raw: Option<Value>,
    /// The whole synced `ordering` object, so writing the order back keeps
    /// fields we don't use.
    pub ordering_raw: Option<Value>,
    /// Synced settings have arrived; until then we must not write any back.
    pub synced_loaded: bool,
    pub folders: Vec<ServerFolder>,
    /// Who is in each channel's call, in join order.
    pub voice: HashMap<String, indexmap::IndexMap<String, UserVoiceState>>,
    /// Servers whose (online) member list has been requested this session.
    pub members_requested: HashSet<String>,
    pub messages: HashMap<String, ChannelMessages>,
    /// channel -> user -> when they started typing.
    pub typing: HashMap<String, HashMap<String, Instant>>,
    /// Last message id read per channel, from /sync/unreads and acks.
    pub last_read: HashMap<String, String>,
    /// Unacknowledged messages mentioning us, per channel.
    pub mentions: HashMap<String, HashSet<String>>,
    /// Until the first /sync/unreads arrives nothing counts as unread,
    /// so channels don't flash unread at startup.
    pub unreads_synced: bool,
    /// When each channel was last on screen, for trimming idle history.
    pub last_viewed: HashMap<String, Instant>,
}

impl Store {
    pub fn apply_ready(&mut self, ready: Ready) {
        *self = Store::default();
        for u in ready.users {
            if u.relationship.as_deref() == Some("User") {
                self.me = Some(u.id.clone());
            }
            self.users.insert(u.id.clone(), u);
        }
        for s in ready.servers {
            self.server_order.push(s.id.clone());
            self.servers.insert(s.id.clone(), s);
        }
        for c in ready.channels {
            self.channels.insert(c.id.clone(), c);
        }
        for m in ready.members {
            self.members.insert(m.id.clone(), m);
        }
        for e in ready.emojis {
            self.emojis.insert(e.id.clone(), e);
        }
        for vs in ready.voice_states {
            self.voice
                .insert(vs.id, vs.participants.into_iter().map(|p| (p.id.clone(), p)).collect());
        }
    }

    pub fn apply_unreads(&mut self, unreads: Vec<ChannelUnread>) {
        for u in unreads {
            if let Some(last) = u.last_id {
                self.last_read.insert(u.id.channel.clone(), last);
            }
            if !u.mentions.is_empty() {
                self.mentions.insert(u.id.channel, u.mentions.into_iter().collect());
            }
        }
        self.unreads_synced = true;
    }

    /// Same rule as the official client: newer messages than our last read,
    /// or outstanding mentions. Saved Notes never count.
    pub fn is_unread(&self, channel: &str) -> bool {
        if !self.unreads_synced {
            return false;
        }
        let Some(c) = self.channels.get(channel) else {
            return false;
        };
        if c.channel_type == "SavedMessages" {
            return false;
        }
        let newer = c
            .last_message_id
            .as_deref()
            .is_some_and(|last| self.last_read.get(channel).is_none_or(|read| read.as_str() < last));
        newer || self.mention_count(channel) > 0
    }

    pub fn mention_count(&self, channel: &str) -> usize {
        self.mentions.get(channel).map_or(0, HashSet::len)
    }

    /// Our permissions in a channel, as the official client calculates them.
    pub fn channel_permissions(&self, channel: &str) -> u64 {
        let (Some(c), Some(me)) = (self.channels.get(channel), self.me.as_deref()) else {
            return 0;
        };
        let server = c.server.as_deref().and_then(|s| self.servers.get(s));
        let member = c.server.as_ref().and_then(|s| {
            self.members.get(&MemberId {
                server: s.clone(),
                user: me.to_string(),
            })
        });
        let recipient = c.recipients.iter().find(|r| *r != me).and_then(|r| self.users.get(r));
        let who = crate::permissions::Me {
            user: self.users.get(me),
            id: me,
            member,
            now_ms: chrono::Utc::now().timestamp_millis(),
        };
        crate::permissions::channel_permissions(c, server, recipient, &who)
    }

    /// Whether `user` (a member of the channel's server) may see `channel`,
    /// for the member list (the SDK's `member.hasPermission(channel, ViewChannel)`).
    pub fn member_sees(&self, channel: &str, user: &str) -> bool {
        let Some(c) = self.channels.get(channel) else {
            return false;
        };
        let server = c.server.as_deref().and_then(|s| self.servers.get(s));
        let member = c.server.as_ref().and_then(|s| {
            self.members.get(&MemberId {
                server: s.clone(),
                user: user.to_string(),
            })
        });
        let who = crate::permissions::Me {
            user: self.users.get(user),
            id: user,
            member,
            now_ms: chrono::Utc::now().timestamp_millis(),
        };
        let view = crate::permissions::VIEW_CHANNEL;
        crate::permissions::channel_permissions(c, server, None, &who) & view == view
    }

    /// Whether some members may not see `channel`, so its member list must
    /// be filtered (the SDK's `potentiallyRestrictedChannel`): the channel or
    /// the server's default takes View Channel away, or some role's override
    /// or role permissions deny it.
    pub fn potentially_restricted(&self, channel: &str) -> bool {
        let Some(c) = self.channels.get(channel) else {
            return false;
        };
        let Some(server) = c.server.as_deref().and_then(|s| self.servers.get(s)) else {
            return false;
        };
        let view = crate::permissions::VIEW_CHANNEL;
        let denies = |bits: i64| u64::try_from(bits).unwrap_or(0) & view == view;
        c.default_permissions.as_ref().is_some_and(|o| denies(o.d))
            || u64::try_from(server.default_permissions).unwrap_or(0) & view != view
            || server.roles.keys().any(|role| {
                c.role_permissions.get(role).is_some_and(|o| denies(o.d))
                    || server
                        .roles
                        .get(role)
                        .and_then(|r| r.permissions.as_ref())
                        .is_some_and(|o| denies(o.d))
            })
    }

    /// Our server-wide permissions (the official `server.havePermission`).
    pub fn can_in_server(&self, server: &str, permission: u64) -> bool {
        let (Some(s), Some(me)) = (self.servers.get(server), self.me.as_deref()) else {
            return false;
        };
        let member = self.members.get(&MemberId {
            server: server.to_string(),
            user: me.to_string(),
        });
        let who = crate::permissions::Me {
            user: self.users.get(me),
            id: me,
            member,
            now_ms: chrono::Utc::now().timestamp_millis(),
        };
        crate::permissions::server_permissions(s, &who) & permission == permission
    }

    /// The SDK's member `ranking`: the owner is highest, then the member's
    /// best (lowest) role rank; no roles ranks last.
    pub fn member_ranking(&self, server: &str, user: &str) -> i64 {
        let Some(s) = self.servers.get(server) else {
            return i64::MAX;
        };
        if s.owner == user {
            return i64::MIN;
        }
        let member = self.members.get(&MemberId {
            server: server.to_string(),
            user: user.to_string(),
        });
        member
            .into_iter()
            .flat_map(|m| &m.roles)
            .filter_map(|r| s.roles.get(r))
            .map(|r| r.rank)
            .min()
            .unwrap_or(i64::MAX)
    }

    /// Whether we outrank `user` on the server (the SDK's `inferiorTo`).
    pub fn outranks(&self, server: &str, user: &str) -> bool {
        self.me
            .as_deref()
            .is_some_and(|me| self.member_ranking(server, me) < self.member_ranking(server, user))
    }

    /// The official `orderedCategories`: the server's categories in order, with
    /// a "default" category (first, unless the server places it) holding
    /// channels no category lists. Ids of channels we don't have are dropped.
    /// Adds a channel (and puts a server channel in its server's list, as
    /// the SDK does on ChannelCreate).
    pub fn add_channel(&mut self, c: Channel) {
        if let Some(s) = c.server.as_ref().and_then(|s| self.servers.get_mut(s))
            && !s.channels.contains(&c.id)
        {
            s.channels.push(c.id.clone());
        }
        self.channels.insert(c.id.clone(), c);
    }

    /// The server's categories as the `categories` value of a server edit,
    /// changed by `f` (the official category modals edit the whole list).
    pub fn edit_categories(&self, server: &str, f: impl FnOnce(&mut Vec<Category>)) -> Option<Value> {
        let mut cats = self.servers.get(server)?.categories.clone().unwrap_or_default();
        f(&mut cats);
        Some(Value::Array(
            cats.iter()
                .map(|c| serde_json::json!({ "id": c.id, "title": c.title, "channels": c.channels }))
                .collect(),
        ))
    }

    /// The ordered categories as a server edit's `categories` value (the
    /// official sidebar saves this whole list, "default" included).
    pub fn categories_value(cats: &[Category]) -> Value {
        Value::Array(
            cats.iter()
                .map(|c| serde_json::json!({ "id": c.id, "title": c.title, "channels": c.channels }))
                .collect(),
        )
    }

    pub fn ordered_categories(&self, server: &Server) -> Vec<Category> {
        let known: HashSet<&String> = server.channels.iter().collect();
        let mut uncategorised: indexmap::IndexSet<&String> = server.channels.iter().collect();
        let mut out: Vec<Category> = Vec::new();
        for c in server.categories.iter().flatten() {
            for id in &c.channels {
                uncategorised.shift_remove(id);
            }
            if c.id != "default" || c.channels.iter().any(|id| known.contains(id)) {
                out.push(c.clone());
            }
        }
        let rest = uncategorised.into_iter().cloned();
        match out.iter_mut().find(|c| c.id == "default") {
            Some(d) => d.channels.extend(rest),
            None => out.insert(
                0,
                Category {
                    id: "default".into(),
                    title: "Default".into(),
                    channels: rest.collect(),
                },
            ),
        }
        for c in &mut out {
            c.channels.retain(|id| self.channels.contains_key(id));
        }
        out
    }

    pub fn can(&self, channel: &str, permission: u64) -> bool {
        self.channel_permissions(channel) & permission == permission
    }

    /// Trims history of channels nobody is looking at, oldest messages first,
    /// so what's left is always the newest unbroken run: after 5 minutes
    /// unviewed a channel keeps one page (50), after 30 minutes nothing (it's
    /// fetched again when opened). Older pages load again on scrolling up.
    pub fn trim_idle(&mut self, open: Option<&str>, now: Instant) {
        if let Some(c) = open {
            self.last_viewed.insert(c.to_string(), now);
        }
        let idle = |id: &str| {
            self.last_viewed
                .get(id)
                .map_or(Duration::MAX, |t| now.saturating_duration_since(*t))
        };
        let mut drop = Vec::new();
        for (id, cm) in self.messages.iter_mut() {
            if Some(id.as_str()) == open || cm.loading {
                continue;
            }
            let idle = idle(id);
            if idle >= Duration::from_secs(30 * 60) {
                drop.push(id.clone());
            } else if idle >= Duration::from_secs(5 * 60)
                && cm.messages.len() > 50
                && let Some(cut) = cm.messages.keys().nth(cm.messages.len() - 50).cloned()
            {
                cm.messages = cm.messages.split_off(&cut);
                cm.reached_start = false;
            }
        }
        for id in drop {
            self.messages.remove(&id);
        }
    }

    /// Moves the read position back so everything after `message` is unread.
    pub fn mark_unread_after(&mut self, channel: &str, message: &str) {
        self.last_read.insert(channel.to_string(), message.to_string());
    }

    pub fn mark_read(&mut self, channel: &str, message: &str) {
        let entry = self.last_read.entry(channel.to_string()).or_default();
        if entry.as_str() < message {
            *entry = message.to_string();
        }
        self.mentions.remove(channel);
    }

    /// Applies synced settings (`key → [timestamp, json]`); keys we don't use are ignored.
    pub fn apply_synced(&mut self, v: &Value) {
        self.synced_loaded = true;
        let parsed = |key: &str| -> Option<Value> { serde_json::from_str(v.get(key)?.get(1)?.as_str()?).ok() };
        if let Some(prefs) = v.get("notifications").and_then(NotificationPrefs::from_synced) {
            self.notification_prefs = prefs;
        }
        if let Some(n) = parsed("notifications") {
            self.notifications_raw = Some(n);
        }
        if let Some(o) = parsed("ordering") {
            let list = o
                .get("serverSidebar")
                .filter(|s| s.is_array())
                .or_else(|| o.get("servers"));
            self.sidebar_order = clean_ids(list);
            self.ordering_raw = Some(o);
        }
        if let Some(f) = parsed("server-folders") {
            if let Some(folders) = f.get("folders").cloned().and_then(|x| serde_json::from_value(x).ok()) {
                self.folders = folders;
            }
            self.folders_raw = Some(f);
        }
    }
}

/// Takes `server` out of every folder (the official `removeServer`).
pub fn folder_remove_server(list: &mut [Value], server: &str) {
    for f in list.iter_mut() {
        if let Some(s) = f.get_mut("servers").and_then(Value::as_array_mut) {
            s.retain(|x| x != server);
        }
    }
}

/// Moves `server` into folder `id`, out of any other (the official `addServer`).
pub fn folder_add_server(list: &mut [Value], id: &str, server: &str) {
    for f in list.iter_mut() {
        let mine = f.get("id").and_then(Value::as_str) == Some(id);
        let Some(s) = f.get_mut("servers").and_then(Value::as_array_mut) else {
            continue;
        };
        if mine {
            if !s.iter().any(|x| x == server) {
                s.push(Value::String(server.to_string()));
            }
        } else {
            s.retain(|x| x != server);
        }
    }
}

/// Applies a network-neutral folder edit to the synced `folders` list. Only
/// the fields an edit owns are written, so a folder keeps any the official
/// client (or a newer one) added. `new_id` names a folder `Create` makes.
pub fn apply_folder_edit(list: &mut Vec<Value>, edit: &godwit_core::folder_edit::FolderEdit, new_id: String) {
    use godwit_core::folder_edit::FolderEdit as E;
    let find = |list: &mut [Value], id: &str| -> Option<usize> {
        list.iter()
            .position(|f| f.get("id").and_then(Value::as_str) == Some(id))
    };
    match edit {
        E::Create { servers } => list.push(serde_json::json!({
            "id": new_id, "name": "", "colour": null, "collapsed": false,
            "servers": servers.iter().map(|s| s.raw.to_string()).collect::<Vec<_>>(),
        })),
        E::AddServer { folder, server, at } => {
            folder_add_server(list, &folder.raw, &server.raw);
            if let Some(at) = at {
                folder_place_server(list, &folder.raw, &server.raw, *at);
            }
        }
        E::RemoveServer { server } => folder_remove_server(list, &server.raw),
        E::Ungroup { folder } => list.retain(|f| f.get("id").and_then(Value::as_str) != Some(&*folder.raw)),
        E::Rename { folder, name } => {
            if let Some(i) = find(list, &folder.raw) {
                list[i]["name"] = serde_json::json!(name.clone().unwrap_or_default());
            }
        }
        E::Recolour { folder, colour } => {
            if let Some(i) = find(list, &folder.raw) {
                list[i]["colour"] = serde_json::json!(colour.map(|c| format!("#{c:06x}")));
            }
        }
    }
}

/// Moves `server` to position `at` among the other servers of folder `id`
/// (the official `place`).
pub fn folder_place_server(list: &mut [Value], id: &str, server: &str, at: usize) {
    for f in list
        .iter_mut()
        .filter(|f| f.get("id").and_then(Value::as_str) == Some(id))
    {
        if let Some(s) = f.get_mut("servers").and_then(Value::as_array_mut) {
            s.retain(|x| x != server);
            let at = at.min(s.len());
            s.insert(at, Value::String(server.to_string()));
        }
    }
}

/// A new folder id: "folder-" plus a ULID, as the official client makes them.
/// Moves a category to position `to` among the others; "default" stays first.
pub fn move_category(cats: &mut Vec<Category>, id: &str, to: usize) {
    let Some(from) = cats.iter().position(|c| c.id == id && c.id != "default") else {
        return;
    };
    let cat = cats.remove(from);
    let offset = usize::from(cats.first().is_some_and(|c| c.id == "default"));
    let at = (to + offset).min(cats.len());
    cats.insert(at, cat);
}

/// Moves a channel into category `to`, before `before` (or last).
pub fn move_channel(cats: &mut [Category], id: &str, to: &str, before: Option<&str>) {
    for c in cats.iter_mut() {
        c.channels.retain(|x| x != id);
    }
    let Some(cat) = cats.iter_mut().find(|c| c.id == to) else {
        return;
    };
    let at = before
        .and_then(|b| cat.channels.iter().position(|x| x == b))
        .unwrap_or(cat.channels.len());
    cat.channels.insert(at, id.to_owned());
}

pub fn new_folder_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut ms = chrono::Utc::now().timestamp_millis() as u64;
    let mut time = [0u8; 10];
    for c in time.iter_mut().rev() {
        *c = ALPHABET[(ms % 32) as usize];
        ms /= 32;
    }
    // Randomness from the std hasher's per-process random keys.
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    let (mut r, mut r2) = (
        h.finish(),
        std::collections::hash_map::RandomState::new().build_hasher().finish(),
    );
    let rand: String = (0..16)
        .map(|i| {
            let src = if i < 8 { &mut r } else { &mut r2 };
            let c = ALPHABET[(*src % 32) as usize] as char;
            *src /= 32;
            c
        })
        .collect();
    format!("folder-{}{rand}", String::from_utf8_lossy(&time))
}

impl Store {
    /// Changes the synced notification options like the official store does
    /// (`kind` is "server", "channel", "server_mutes" or "channel_mutes";
    /// None removes the entry) and returns the JSON to save, or None if the
    /// synced settings haven't loaded yet.
    pub fn set_notification(&mut self, kind: &str, id: &str, value: Option<Value>) -> Option<String> {
        if !self.synced_loaded {
            return None;
        }
        let mut raw = self
            .notifications_raw
            .clone()
            .filter(Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        if !raw.get(kind).is_some_and(Value::is_object) {
            raw[kind] = serde_json::json!({});
        }
        let map = raw[kind].as_object_mut()?;
        match value {
            Some(v) => {
                map.insert(id.to_string(), v);
            }
            None => {
                map.remove(id);
            }
        }
        if let Ok(prefs) = serde_json::from_value(raw.clone()) {
            self.notification_prefs = prefs;
        }
        self.notifications_raw = Some(raw.clone());
        Some(raw.to_string())
    }

    /// Edits the synced server folders like the official store (each folder
    /// object keeps fields we don't use; folders left with fewer than two servers
    /// dissolve) and returns the JSON to save, or None before synced settings load.
    pub fn edit_folders(&mut self, f: impl FnOnce(&mut Vec<Value>)) -> Option<String> {
        if !self.synced_loaded {
            return None;
        }
        let mut raw = self
            .folders_raw
            .clone()
            .filter(Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        let mut list: Vec<Value> = raw
            .get("folders")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        f(&mut list);
        list.retain(|folder| {
            folder
                .get("servers")
                .and_then(Value::as_array)
                .is_some_and(|s| s.len() >= 2)
        });
        raw["folders"] = Value::Array(list);
        if let Some(folders) = raw.get("folders").cloned().and_then(|x| serde_json::from_value(x).ok()) {
            self.folders = folders;
        }
        self.folders_raw = Some(raw.clone());
        Some(raw.to_string())
    }

    /// Moves a top-level rail entry (server or folder) to `to` among the
    /// others, like dragging it in the official client. Returns the new synced
    /// `ordering` JSON to save.
    pub fn move_rail_entry(&mut self, id: &str, to: usize) -> Option<String> {
        let mut entries: Vec<String> = self
            .rail()
            .into_iter()
            .map(|e| match e {
                RailEntry::Server(id) => id,
                RailEntry::Folder { id, .. } => id,
            })
            .collect();
        let from = entries.iter().position(|e| e == id)?;
        let moved = entries.remove(from);
        entries.insert(to.min(entries.len()), moved);
        // Folder members trail the folder id, as the official setServerOrder writes them.
        let sidebar: Vec<String> = entries
            .iter()
            .flat_map(|e| {
                let members = self
                    .folders
                    .iter()
                    .find(|f| &f.id == e)
                    .map(|f| f.servers.clone())
                    .unwrap_or_default();
                std::iter::once(e.clone()).chain(members)
            })
            .collect();
        self.sidebar_order = sidebar.clone();
        let mut raw = self
            .ordering_raw
            .clone()
            .filter(Value::is_object)
            .unwrap_or_else(|| serde_json::json!({ "servers": [] }));
        raw["serverSidebar"] = serde_json::json!(sidebar);
        self.ordering_raw = Some(raw.clone());
        Some(raw.to_string())
    }

    /// The official client's `orderedEntries`: walk the synced order; a server
    /// inside a folder pulls in the whole folder at its first appearance; then
    /// append servers the order doesn't mention. Empty folders are dropped.
    pub fn rail(&self) -> Vec<RailEntry> {
        let mut known: indexmap::IndexSet<&String> = self
            .server_order
            .iter()
            .filter(|s| self.servers.contains_key(*s))
            .collect();
        let mut folder_of: HashMap<&str, &ServerFolder> = HashMap::new();
        for f in &self.folders {
            for s in &f.servers {
                folder_of.entry(s.as_str()).or_insert(f);
            }
        }
        let by_id: HashMap<&str, &ServerFolder> = self.folders.iter().map(|f| (f.id.as_str(), f)).collect();
        let mut out = Vec::new();
        let mut drawn: HashSet<&str> = HashSet::new();
        let ids: Vec<String> = self
            .sidebar_order
            .iter()
            .cloned()
            .chain(known.iter().map(|s| (*s).clone()))
            .collect();
        for id in &ids {
            if let Some(f) = by_id.get(id.as_str()).or_else(|| folder_of.get(id.as_str())) {
                if !drawn.insert(f.id.as_str()) {
                    continue;
                }
                let servers: Vec<String> = f.servers.iter().filter(|s| known.shift_remove(*s)).cloned().collect();
                if !servers.is_empty() {
                    out.push(RailEntry::Folder {
                        id: f.id.clone(),
                        servers,
                    });
                }
            } else if known.shift_remove(id) {
                out.push(RailEntry::Server(id.clone()));
            }
        }
        out
    }

    pub fn apply_members(&mut self, data: AllMembers) {
        for u in data.users {
            self.users.insert(u.id.clone(), u);
        }
        for m in data.members {
            self.members.insert(m.id.clone(), m);
        }
    }

    /// Colour of the member's highest-ranked coloured role.
    pub fn role_colour(&self, server: &str, user: &str) -> Option<(u8, u8, u8)> {
        let s = self.servers.get(server)?;
        let key = MemberId {
            server: server.to_string(),
            user: user.to_string(),
        };
        let m = self.members.get(&key)?;
        m.roles
            .iter()
            .filter_map(|r| s.roles.get(r))
            .filter_map(|r| Some((r.rank, parse_hex_colour(r.colour.as_deref()?)?)))
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, c)| c)
    }

    pub fn apply_messages(&mut self, channel: &str, before: Option<&str>, data: MessagesWithUsers) {
        for u in data.users {
            self.users.insert(u.id.clone(), u);
        }
        for m in data.members {
            self.members.insert(m.id.clone(), m);
        }
        let entry = self.messages.entry(channel.to_string()).or_default();
        if data.messages.len() < 50 {
            entry.reached_start = true;
        }
        if before.is_none() {
            // A fresh load replaces whatever partial window we had.
            entry.messages.clear();
        }
        for m in data.messages {
            entry.messages.insert(m.id.clone(), m);
        }
        entry.loading = false;
        entry.loaded_once = true;
    }

    /// Replaces a channel's window with the messages around a jump target.
    pub fn apply_nearby(&mut self, channel: &str, data: MessagesWithUsers) {
        self.messages.remove(channel);
        self.apply_messages(channel, Some(""), data);
        if let Some(cm) = self.messages.get_mut(channel) {
            // "nearby" returns both sides, so a short page doesn't mean the start.
            cm.reached_start = false;
            cm.detached = true;
        }
    }

    /// Applies a websocket event. Unknown events are ignored.
    pub fn apply_event(&mut self, v: Value, viewing: Option<&str>) {
        let Some(kind) = v.get("type").and_then(Value::as_str) else {
            return;
        };
        match kind {
            "Message" => {
                let Ok(msg) = serde_json::from_value::<Message>(v) else {
                    return;
                };
                if let Some(c) = self.channels.get_mut(&msg.channel) {
                    c.last_message_id = Some(msg.id.clone());
                }
                if let Some(t) = self.typing.get_mut(&msg.channel) {
                    t.remove(&msg.author);
                }
                let mine = self.me.as_deref() == Some(msg.author.as_str());
                if mine {
                    // Our own messages are read by definition.
                    self.last_read.insert(msg.channel.clone(), msg.id.clone());
                } else if viewing != Some(msg.channel.as_str())
                    && (self.me.as_ref().is_some_and(|me| msg.mentions.contains(me))
                        // In a DM or group every message counts, as
                        // Discord and Steam count them: DM badges show a
                        // number on every network.
                        || self
                            .channels
                            .get(&msg.channel)
                            .is_some_and(|c| matches!(c.channel_type.as_str(), "DirectMessage" | "Group")))
                {
                    self.mentions
                        .entry(msg.channel.clone())
                        .or_default()
                        .insert(msg.id.clone());
                }
                // Only append to channels we've loaded; others fetch fresh on open.
                if let Some(cm) = self.messages.get_mut(&msg.channel)
                    && cm.loaded_once
                    && !cm.detached
                {
                    cm.messages.insert(msg.id.clone(), msg);
                }
            }
            "MessageUpdate" => {
                let (Some(id), Some(channel)) = (str_field(&v, "id"), str_field(&v, "channel")) else {
                    return;
                };
                let Some(m) = self.messages.get_mut(channel).and_then(|c| c.messages.get_mut(id)) else {
                    return;
                };
                if let Some(data) = v.get("data") {
                    if let Some(content) = data.get("content").and_then(Value::as_str) {
                        m.content = Some(content.to_string());
                    }
                    if let Some(edited) = data.get("edited").and_then(Value::as_str) {
                        m.edited = Some(edited.to_string());
                    }
                    if let Some(embeds) = data.get("embeds").cloned().and_then(|e| serde_json::from_value(e).ok()) {
                        m.embeds = embeds;
                    }
                    if let Some(p) = data.get("pinned").and_then(Value::as_bool) {
                        m.pinned = Some(p);
                    }
                }
                if v.get("clear")
                    .and_then(Value::as_array)
                    .is_some_and(|c| c.iter().any(|f| f == "Pinned"))
                {
                    m.pinned = None;
                }
            }
            "MessageAppend" => {
                // Link previews are generated after sending and arrive this way.
                let (Some(id), Some(channel)) = (str_field(&v, "id"), str_field(&v, "channel")) else {
                    return;
                };
                let Some(m) = self.messages.get_mut(channel).and_then(|c| c.messages.get_mut(id)) else {
                    return;
                };
                if let Some(embeds) = v
                    .get("append")
                    .and_then(|a| a.get("embeds"))
                    .cloned()
                    .and_then(|e| serde_json::from_value::<Vec<Embed>>(e).ok())
                {
                    m.embeds.extend(embeds);
                }
            }
            "MessageDelete" => {
                let (Some(id), Some(channel)) = (str_field(&v, "id"), str_field(&v, "channel")) else {
                    return;
                };
                if let Some(c) = self.messages.get_mut(channel) {
                    c.messages.remove(id);
                }
                if let Some(m) = self.mentions.get_mut(channel) {
                    m.remove(id);
                }
            }
            "MessageReact" | "MessageUnreact" | "MessageRemoveReaction" => {
                let (Some(id), Some(channel), Some(emoji)) = (
                    str_field(&v, "id"),
                    str_field(&v, "channel_id"),
                    str_field(&v, "emoji_id"),
                ) else {
                    return;
                };
                let Some(m) = self.messages.get_mut(channel).and_then(|c| c.messages.get_mut(id)) else {
                    return;
                };
                match kind {
                    "MessageReact" => {
                        let Some(user) = str_field(&v, "user_id") else { return };
                        let users = m.reactions.entry(emoji.to_string()).or_default();
                        if !users.iter().any(|u| u == user) {
                            users.push(user.to_string());
                        }
                    }
                    "MessageUnreact" => {
                        let Some(user) = str_field(&v, "user_id") else { return };
                        if let Some(users) = m.reactions.get_mut(emoji) {
                            users.retain(|u| u != user);
                            if users.is_empty() {
                                m.reactions.shift_remove(emoji);
                            }
                        }
                    }
                    _ => {
                        m.reactions.shift_remove(emoji);
                    }
                }
            }
            "BulkMessageDelete" => {
                let Some(channel) = str_field(&v, "channel") else {
                    return;
                };
                let Some(c) = self.messages.get_mut(channel) else {
                    return;
                };
                for id in v.get("ids").and_then(Value::as_array).into_iter().flatten() {
                    if let Some(id) = id.as_str() {
                        c.messages.remove(id);
                    }
                }
            }
            "ChannelCreate" => {
                if let Ok(c) = serde_json::from_value::<Channel>(v) {
                    self.add_channel(c);
                }
            }
            "ChannelUpdate" => {
                let Some(id) = str_field(&v, "id") else { return };
                let Some(c) = self.channels.get_mut(id) else { return };
                if let Some(data) = v.get("data") {
                    if let Some(name) = data.get("name").and_then(Value::as_str) {
                        c.name = Some(name.to_string());
                    }
                    if let Some(desc) = data.get("description").and_then(Value::as_str) {
                        c.description = Some(desc.to_string());
                    }
                    if let Some(o) = data.get("owner").and_then(Value::as_str) {
                        c.owner = Some(o.to_string());
                    }
                    if let Some(p) = data.get("permissions").and_then(Value::as_i64) {
                        c.permissions = Some(p);
                    }
                    if let Some(o) = data
                        .get("default_permissions")
                        .cloned()
                        .and_then(|o| serde_json::from_value(o).ok())
                    {
                        c.default_permissions = Some(o);
                    }
                    if let Some(r) = data
                        .get("role_permissions")
                        .cloned()
                        .and_then(|r| serde_json::from_value(r).ok())
                    {
                        c.role_permissions = r;
                    }
                    if let Some(f) = data.get("icon").cloned().and_then(|f| serde_json::from_value(f).ok()) {
                        c.icon = Some(f);
                    }
                    if let Some(n) = data.get("nsfw").and_then(Value::as_bool) {
                        c.nsfw = n;
                    }
                    if let Some(s) = data.get("slowmode").and_then(Value::as_u64) {
                        c.slowmode = Some(s);
                    }
                    if let Some(v) = data.get("voice") {
                        c.voice = Some(v.clone());
                    }
                }
                for field in v.get("clear").and_then(Value::as_array).into_iter().flatten() {
                    match field.as_str() {
                        Some("Description") => c.description = None,
                        Some("Icon") => c.icon = None,
                        Some("DefaultPermissions") => c.default_permissions = None,
                        Some("Slowmode") => c.slowmode = None,
                        Some("Voice") => c.voice = None,
                        _ => {}
                    }
                }
            }
            "ChannelGroupJoin" | "ChannelGroupLeave" => {
                let (Some(id), Some(user)) = (str_field(&v, "id"), str_field(&v, "user")) else {
                    return;
                };
                if kind == "ChannelGroupLeave" && self.me.as_deref() == Some(user) {
                    // We left (from here or another device).
                    self.channels.remove(id);
                    self.messages.remove(id);
                    return;
                }
                let Some(c) = self.channels.get_mut(id) else { return };
                c.recipients.retain(|r| r != user);
                if kind == "ChannelGroupJoin" {
                    c.recipients.push(user.to_string());
                }
            }
            "ChannelDelete" => {
                if let Some(id) = str_field(&v, "id") {
                    // Server channels also leave their server's list, as the SDK does.
                    if let Some(s) = self
                        .channels
                        .remove(id)
                        .and_then(|c| c.server)
                        .and_then(|s| self.servers.get_mut(&s))
                    {
                        s.channels.retain(|c| c != id);
                    }
                    self.messages.remove(id);
                }
            }
            "ChannelStartTyping" | "ChannelStopTyping" => {
                let (Some(channel), Some(user)) = (str_field(&v, "id"), str_field(&v, "user")) else {
                    return;
                };
                let t = self.typing.entry(channel.to_string()).or_default();
                if kind == "ChannelStartTyping" {
                    t.insert(user.to_string(), Instant::now());
                } else {
                    t.remove(user);
                }
            }
            "ChannelAck" => {
                // Acks from our other sessions arrive here too.
                let (Some(channel), Some(user), Some(message)) =
                    (str_field(&v, "id"), str_field(&v, "user"), str_field(&v, "message_id"))
                else {
                    return;
                };
                if self.me.as_deref() == Some(user) {
                    self.mark_read(channel, message);
                }
            }
            "ServerCreate" => {
                if let Ok(s) = serde_json::from_value::<ServerWithChannels>(v) {
                    self.add_server(s);
                }
            }
            "ServerUpdate" => {
                let Some(id) = str_field(&v, "id") else { return };
                let Some(s) = self.servers.get_mut(id) else { return };
                if let Some(data) = v.get("data") {
                    if let Some(name) = data.get("name").and_then(Value::as_str) {
                        s.name = name.to_string();
                    }
                    if let Some(d) = data.get("description").and_then(Value::as_str) {
                        s.description = Some(d.to_string());
                    }
                    if let Some(chs) = data
                        .get("channels")
                        .cloned()
                        .and_then(|c| serde_json::from_value(c).ok())
                    {
                        s.channels = chs;
                    }
                    if let Some(cats) = data
                        .get("categories")
                        .cloned()
                        .and_then(|c| serde_json::from_value(c).ok())
                    {
                        s.categories = Some(cats);
                    }
                    if let Some(o) = data.get("owner").and_then(Value::as_str) {
                        s.owner = o.to_string();
                    }
                    if let Some(p) = data.get("default_permissions").and_then(Value::as_i64) {
                        s.default_permissions = p;
                    }
                    if let Some(f) = data.get("icon").cloned().and_then(|f| serde_json::from_value(f).ok()) {
                        s.icon = Some(f);
                    }
                    if let Some(f) = data.get("banner").cloned().and_then(|f| serde_json::from_value(f).ok()) {
                        s.banner = Some(f);
                    }
                    if let Some(m) = data.get("system_messages") {
                        s.system_messages = Some(m.clone());
                    }
                    if let Some(d) = data.get("discoverable").and_then(Value::as_bool) {
                        s.discoverable = d;
                    }
                }
                // FieldsServer the update removed.
                for f in v.get("clear").and_then(Value::as_array).into_iter().flatten() {
                    match f.as_str() {
                        Some("Description") => s.description = None,
                        Some("Icon") => s.icon = None,
                        Some("Banner") => s.banner = None,
                        Some("SystemMessages") => s.system_messages = None,
                        Some("Categories") => s.categories = None,
                        _ => {}
                    }
                }
            }
            "ServerDelete" => {
                if let Some(id) = str_field(&v, "id") {
                    self.servers.remove(id);
                    self.server_order.retain(|s| s != id);
                }
            }
            "ServerMemberLeave" => {
                // Leaving a server ourselves arrives as a member-leave for our id.
                let (Some(server), Some(user)) = (str_field(&v, "id"), str_field(&v, "user")) else {
                    return;
                };
                if self.me.as_deref() == Some(user) {
                    self.servers.remove(server);
                    self.server_order.retain(|s| s != server);
                }
                self.members.remove(&MemberId {
                    server: server.to_string(),
                    user: user.to_string(),
                });
            }
            "ServerRoleUpdate" => {
                let (Some(server), Some(role_id)) = (str_field(&v, "id"), str_field(&v, "role_id")) else {
                    return;
                };
                let Some(s) = self.servers.get_mut(server) else { return };
                let data = v.get("data").cloned().unwrap_or(Value::Null);
                let role = s.roles.entry(role_id.to_string()).or_default();
                if let Some(n) = data.get("name").and_then(Value::as_str) {
                    role.name = n.to_string();
                }
                if let Some(c) = data.get("colour").and_then(Value::as_str) {
                    role.colour = Some(c.to_string());
                }
                if let Some(h) = data.get("hoist").and_then(Value::as_bool) {
                    role.hoist = h;
                }
                if let Some(r) = data.get("rank").and_then(Value::as_i64) {
                    role.rank = r;
                }
                if let Some(p) = data
                    .get("permissions")
                    .cloned()
                    .and_then(|p| serde_json::from_value(p).ok())
                {
                    role.permissions = Some(p);
                }
                if let Some(i) = data.get("icon").cloned().and_then(|i| serde_json::from_value(i).ok()) {
                    role.icon = Some(i);
                }
                for f in v.get("clear").and_then(Value::as_array).into_iter().flatten() {
                    match f.as_str() {
                        Some("Colour") => role.colour = None,
                        Some("Icon") => role.icon = None,
                        _ => {}
                    }
                }
            }
            "ServerRoleRanksUpdate" => {
                // Ranks are the role ids in order, most important first.
                let Some(s) = str_field(&v, "id").and_then(|id| self.servers.get_mut(id)) else {
                    return;
                };
                for (i, id) in v
                    .get("ranks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    if let Some(r) = id.as_str().and_then(|id| s.roles.get_mut(id)) {
                        r.rank = i as i64;
                    }
                }
            }
            "ServerRoleDelete" => {
                let (Some(server), Some(role_id)) = (str_field(&v, "id"), str_field(&v, "role_id")) else {
                    return;
                };
                if let Some(s) = self.servers.get_mut(server) {
                    s.roles.remove(role_id);
                }
            }
            "VoiceChannelJoin" => {
                let Some(channel) = str_field(&v, "id") else { return };
                let Some(state) = v
                    .get("state")
                    .cloned()
                    .and_then(|s| serde_json::from_value::<UserVoiceState>(s).ok())
                else {
                    return;
                };
                self.voice
                    .entry(channel.to_string())
                    .or_default()
                    .insert(state.id.clone(), state);
            }
            "VoiceChannelLeave" => {
                let (Some(channel), Some(user)) = (str_field(&v, "id"), str_field(&v, "user")) else {
                    return;
                };
                if let Some(c) = self.voice.get_mut(channel) {
                    c.shift_remove(user);
                }
            }
            "VoiceChannelMove" => {
                let (Some(from), Some(to)) = (str_field(&v, "from"), str_field(&v, "to")) else {
                    return;
                };
                let Some(state) = v
                    .get("state")
                    .cloned()
                    .and_then(|s| serde_json::from_value::<UserVoiceState>(s).ok())
                else {
                    return;
                };
                if let Some(c) = self.voice.get_mut(from) {
                    c.shift_remove(&state.id);
                }
                self.voice
                    .entry(to.to_string())
                    .or_default()
                    .insert(state.id.clone(), state);
            }
            "UserVoiceStateUpdate" => {
                let (Some(user), Some(channel)) = (str_field(&v, "id"), str_field(&v, "channel_id")) else {
                    return;
                };
                let Some(p) = self.voice.get_mut(channel).and_then(|c| c.get_mut(user)) else {
                    return;
                };
                let data = v.get("data").cloned().unwrap_or(Value::Null);
                let flag = |k: &str| data.get(k).and_then(Value::as_bool);
                if let Some(b) = flag("is_publishing") {
                    p.is_publishing = b;
                }
                if let Some(b) = flag("is_receiving") {
                    p.is_receiving = b;
                }
                if let Some(b) = flag("camera") {
                    p.camera = b;
                }
                if let Some(b) = flag("screensharing") {
                    p.screensharing = b;
                }
            }
            "UserRelationship" => {
                if let Some(user) = v
                    .get("user")
                    .cloned()
                    .and_then(|u| serde_json::from_value::<User>(u).ok())
                {
                    self.upsert_relationship(user);
                }
            }
            "UserSettingsUpdate" => {
                // Our synced settings changed on another device.
                if let Some(update) = v.get("update") {
                    self.apply_synced(update);
                }
            }
            "EmojiCreate" => {
                if let Ok(e) = serde_json::from_value::<Emoji>(v) {
                    self.emojis.insert(e.id.clone(), e);
                }
            }
            "EmojiDelete" => {
                if let Some(id) = str_field(&v, "id") {
                    self.emojis.remove(id);
                }
            }
            "ServerMemberJoin" => {
                let (Some(server), Some(user)) = (str_field(&v, "id"), str_field(&v, "user")) else {
                    return;
                };
                let id = MemberId {
                    server: server.to_string(),
                    user: user.to_string(),
                };
                self.members.entry(id.clone()).or_insert(Member {
                    id,
                    nickname: None,
                    avatar: None,
                    pronouns: None,
                    roles: vec![],
                    timeout: None,
                });
            }
            "ServerMemberUpdate" => {
                let Some(id) = v
                    .get("id")
                    .cloned()
                    .and_then(|i| serde_json::from_value::<MemberId>(i).ok())
                else {
                    return;
                };
                let Some(m) = self.members.get_mut(&id) else { return };
                if let Some(data) = v.get("data") {
                    if let Some(n) = data.get("nickname").and_then(Value::as_str) {
                        m.nickname = Some(n.to_string());
                    }
                    if let Some(roles) = data.get("roles").cloned().and_then(|r| serde_json::from_value(r).ok()) {
                        m.roles = roles;
                    }
                    if let Some(a) = data.get("avatar").cloned().and_then(|a| serde_json::from_value(a).ok()) {
                        m.avatar = Some(a);
                    }
                    if let Some(t) = data.get("timeout").and_then(Value::as_str) {
                        m.timeout = Some(t.to_string());
                    }
                    if let Some(p) = data.get("pronouns").and_then(Value::as_str) {
                        m.pronouns = Some(p.to_string());
                    }
                }
                for field in v.get("clear").and_then(Value::as_array).into_iter().flatten() {
                    match field.as_str() {
                        Some("Nickname") => m.nickname = None,
                        Some("Avatar") => m.avatar = None,
                        Some("Roles") => m.roles.clear(),
                        Some("Timeout") => m.timeout = None,
                        Some("Pronouns") => m.pronouns = None,
                        _ => {}
                    }
                }
            }
            "UserUpdate" => {
                let Some(id) = str_field(&v, "id") else { return };
                let Some(u) = self.users.get_mut(id) else { return };
                if let Some(data) = v.get("data") {
                    if let Some(n) = data.get("display_name").and_then(Value::as_str) {
                        u.display_name = Some(n.to_string());
                    }
                    if let Some(a) = data.get("avatar").cloned().and_then(|a| serde_json::from_value(a).ok()) {
                        u.avatar = Some(a);
                    }
                    if let Some(o) = data.get("online").and_then(Value::as_bool) {
                        u.online = Some(o);
                    }
                    // Merge, so a presence-only change keeps the status text.
                    if let Some(st) = data
                        .get("status")
                        .cloned()
                        .and_then(|s| serde_json::from_value::<UserStatus>(s).ok())
                    {
                        let cur = u.status.get_or_insert_with(Default::default);
                        if st.text.is_some() {
                            cur.text = st.text;
                        }
                        if st.presence.is_some() {
                            cur.presence = st.presence;
                        }
                    }
                }
                for field in v.get("clear").and_then(Value::as_array).into_iter().flatten() {
                    match field.as_str() {
                        Some("StatusText") => u.status.get_or_insert_with(Default::default).text = None,
                        Some("StatusPresence") => u.status.get_or_insert_with(Default::default).presence = None,
                        Some("DisplayName") => u.display_name = None,
                        Some("Avatar") => u.avatar = None,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// Adds a server we created or joined (also sent as a ServerCreate event).
    pub fn add_server(&mut self, s: ServerWithChannels) {
        if !self.server_order.contains(&s.server.id) {
            self.server_order.push(s.server.id.clone());
        }
        self.servers.insert(s.server.id.clone(), s.server);
        for c in s.channels {
            self.channels.insert(c.id.clone(), c);
        }
    }

    /// Records a user's new relationship to us, adding them if we didn't know them.
    pub fn upsert_relationship(&mut self, user: User) {
        match self.users.get_mut(&user.id) {
            Some(u) => u.relationship = user.relationship,
            None => {
                self.users.insert(user.id.clone(), user);
            }
        }
    }

    pub fn saved_notes(&self) -> Option<&Channel> {
        self.channels.values().find(|c| c.channel_type == "SavedMessages")
    }

    /// Users with a given relationship, sorted by display name, as the official Friends lists are.
    pub fn users_with(&self, relationship: &str) -> Vec<&User> {
        let mut v: Vec<&User> = self
            .users
            .values()
            .filter(|u| u.relationship.as_deref() == Some(relationship))
            .collect();
        v.sort_by_cached_key(|u| u.name().to_lowercase());
        v
    }

    /// Display name for a user, preferring their nickname in `server`.
    pub fn display_name(&self, user: &str, server: Option<&str>) -> String {
        if let Some(server) = server {
            let key = MemberId {
                server: server.to_string(),
                user: user.to_string(),
            };
            if let Some(nick) = self.members.get(&key).and_then(|m| m.nickname.clone()) {
                return nick;
            }
        }
        match self.users.get(user) {
            Some(u) => u.name().to_string(),
            None => "Unknown user".to_string(),
        }
    }

    pub fn channel_title(&self, c: &Channel) -> String {
        match c.channel_type.as_str() {
            "SavedMessages" => "Saved Notes".to_string(),
            "DirectMessage" => {
                let other = c.recipients.iter().find(|r| Some(r.as_str()) != self.me.as_deref());
                other
                    .map(|o| self.display_name(o, None))
                    .unwrap_or_else(|| "Direct message".into())
            }
            _ => c.name.clone().unwrap_or_else(|| "unnamed".into()),
        }
    }

    /// DMs, groups and saved notes, most recently active first.
    pub fn private_channels(&self) -> Vec<&Channel> {
        let mut v: Vec<&Channel> = self
            .channels
            .values()
            // The official `orderedConversations`: active DMs and groups; Saved Notes has its own entry.
            .filter(|c| c.server.is_none())
            .filter(|c| c.channel_type == "Group" || (c.channel_type == "DirectMessage" && c.active.unwrap_or(false)))
            .collect();
        v.sort_by(|a, b| {
            let rank = |c: &Channel| (c.channel_type != "SavedMessages") as u8;
            rank(a)
                .cmp(&rank(b))
                .then(b.last_message_id.cmp(&a.last_message_id))
                .then(a.id.cmp(&b.id))
        });
        v
    }

    pub fn typing_in(&mut self, channel: &str) -> Vec<String> {
        let Some(t) = self.typing.get_mut(channel) else {
            return vec![];
        };
        // The server stops typing after a few seconds; expire stale entries ourselves too.
        t.retain(|_, since| since.elapsed().as_secs() < 8);
        let ids: Vec<String> = t
            .keys()
            .filter(|u| Some(u.as_str()) != self.me.as_deref())
            .cloned()
            .collect();
        let server = self.channels.get(channel).and_then(|c| c.server.clone());
        ids.iter().map(|u| self.display_name(u, server.as_deref())).collect()
    }
}

/// The official `cleanIds`: strings, or folder objects' `servers`, deduplicated.
fn clean_ids(input: Option<&Value>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for entry in input.and_then(Value::as_array).into_iter().flatten() {
        let ids: Vec<&str> = match entry {
            Value::String(s) => vec![s.as_str()],
            other => other
                .get("servers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect(),
        };
        for id in ids {
            if !id.is_empty() && seen.insert(id.to_string()) {
                out.push(id.to_string());
            }
        }
    }
    out
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(id: &str, last: Option<&str>, kind: &str) -> Channel {
        serde_json::from_value(serde_json::json!({
            "_id": id, "channel_type": kind, "last_message_id": last,
        }))
        .unwrap()
    }

    #[test]
    fn member_ranking_follows_the_sdk() {
        let mut store = Store::default();
        let server: Server = serde_json::from_value(serde_json::json!({
            "_id": "S", "owner": "O", "name": "s",
            "roles": { "A": { "name": "a", "rank": 1 }, "B": { "name": "b", "rank": 5 } },
        }))
        .unwrap();
        store.servers.insert("S".into(), server);
        for (user, roles) in [("ME", vec!["B"]), ("X", vec!["A", "B"]), ("Y", vec![])] {
            let m: Member =
                serde_json::from_value(serde_json::json!({ "_id": { "server": "S", "user": user }, "roles": roles }))
                    .unwrap();
            store.members.insert(m.id.clone(), m);
        }
        store.me = Some("ME".into());
        assert!(store.outranks("S", "Y"), "no roles ranks last");
        assert!(!store.outranks("S", "X"), "X's best role is higher");
        assert!(!store.outranks("S", "O"), "nobody outranks the owner");
    }

    #[test]
    fn sidebar_reordering() {
        let cat = |id: &str, ch: &[&str]| Category {
            id: id.into(),
            title: id.into(),
            channels: ch.iter().map(|s| s.to_string()).collect(),
        };
        let mut cats = vec![cat("default", &["a"]), cat("X", &["b", "c"]), cat("Y", &["d"])];
        move_category(&mut cats, "Y", 0);
        assert_eq!(
            cats.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["default", "Y", "X"]
        );
        move_category(&mut cats, "default", 2);
        assert_eq!(cats[0].id, "default", "default never moves");
        move_channel(&mut cats, "c", "Y", Some("d"));
        assert_eq!(cats[1].channels, ["c", "d"]);
        assert_eq!(cats[2].channels, ["b"]);
        move_channel(&mut cats, "a", "X", None);
        assert!(cats[0].channels.is_empty());
        assert_eq!(cats[2].channels, ["b", "a"]);
    }

    #[test]
    fn category_edits_and_new_channels() {
        let mut store = Store::default();
        let server: Server = serde_json::from_value(serde_json::json!({
            "_id": "S", "owner": "O", "name": "s", "channels": ["A"],
            "categories": [{ "id": "K", "title": "Talk", "channels": ["A"] }],
        }))
        .unwrap();
        store.servers.insert("S".into(), server);
        let renamed = store.edit_categories("S", |c| c[0].title = "Chat".into()).unwrap();
        assert_eq!(
            renamed,
            serde_json::json!([{ "id": "K", "title": "Chat", "channels": ["A"] }])
        );
        let c: Channel =
            serde_json::from_value(serde_json::json!({ "_id": "B", "channel_type": "TextChannel", "server": "S" }))
                .unwrap();
        store.add_channel(c.clone());
        store.add_channel(c);
        assert_eq!(
            store.servers["S"].channels,
            vec!["A".to_string(), "B".to_string()],
            "added once"
        );
    }

    #[test]
    fn unread_rules() {
        let mut s = Store {
            me: Some("ME".into()),
            ..Default::default()
        };
        s.channels.insert("a".into(), channel("a", Some("01B"), "TextChannel"));
        s.channels
            .insert("notes".into(), channel("notes", Some("01B"), "SavedMessages"));
        // Nothing is unread until the first sync arrives.
        assert!(!s.is_unread("a"));

        s.apply_unreads(vec![]);
        assert!(s.is_unread("a"), "never-read channel with messages is unread");
        assert!(!s.is_unread("notes"), "saved notes never count");

        s.mark_read("a", "01B");
        assert!(!s.is_unread("a"));
        // Marking an older message read must not move the marker back.
        s.mark_read("a", "01A");
        assert!(!s.is_unread("a"));

        s.mentions.entry("a".into()).or_default().insert("01B".into());
        assert!(s.is_unread("a") && s.mention_count("a") == 1);
        s.apply_event(
            serde_json::json!({"type": "ChannelAck", "id": "a", "user": "ME", "message_id": "01B"}),
            None,
        );
        assert_eq!(s.mention_count("a"), 0);
    }

    #[test]
    fn categories_follow_the_official_ordering() {
        use serde_json::json;
        let mut s = Store::default();
        for id in ["a", "b", "c", "d"] {
            s.channels.insert(id.into(), channel(id, None, "TextChannel"));
        }
        let server: Server = serde_json::from_value(json!({
            "_id": "S", "name": "s", "channels": ["a", "b", "c", "d"],
            "categories": [{"id": "X", "title": "Talk", "channels": ["c", "gone"]}],
        }))
        .unwrap();
        let cats = s.ordered_categories(&server);
        assert_eq!(cats[0].id, "default", "uncategorised channels go first");
        assert_eq!(cats[0].channels, ["a", "b", "d"]);
        assert_eq!(cats[1].channels, ["c"], "unknown channel ids are dropped");
    }

    #[test]
    fn relationships_update_and_sort() {
        use serde_json::json;
        let mut s = Store::default();
        s.users.insert(
            "A".into(),
            serde_json::from_value(json!({"_id": "A", "username": "zed", "online": true})).unwrap(),
        );
        s.apply_event(
            json!({"type": "UserRelationship", "id": "ME", "status": "Friend",
            "user": {"_id": "A", "username": "zed", "relationship": "Friend"}}),
            None,
        );
        s.apply_event(
            json!({"type": "UserRelationship", "id": "ME", "status": "Incoming",
            "user": {"_id": "B", "username": "amy", "relationship": "Incoming"}}),
            None,
        );
        assert_eq!(s.users["A"].online, Some(true), "known users keep their other fields");
        assert_eq!(s.users_with("Friend").len(), 1);
        assert_eq!(s.users_with("Incoming")[0].username, "amy", "unknown users are added");
    }

    #[test]
    fn folder_edits_follow_the_official_rules() {
        use serde_json::json;
        let mut s = Store::default();
        s.apply_synced(&json!({ "server-folders": [0, json!({"folders": [
            {"id": "folder-A", "name": "A", "servers": ["s1", "s2"], "extra": true},
            {"id": "folder-B", "name": "B", "servers": ["s3"]},
        ]}).to_string()] }));
        // Moving s3 into A empties B, which is dropped; A keeps its unknown field.
        let out: Value =
            serde_json::from_str(&s.edit_folders(|l| folder_add_server(l, "folder-A", "s3")).unwrap()).unwrap();
        assert_eq!(out["folders"].as_array().unwrap().len(), 1);
        assert_eq!(out["folders"][0]["servers"], json!(["s1", "s2", "s3"]));
        assert_eq!(out["folders"][0]["extra"], json!(true));
        assert_eq!(s.folders[0].servers.len(), 3);
        s.edit_folders(|l| folder_remove_server(l, "s1"));
        assert_eq!(s.folders[0].servers, ["s2", "s3"]);
        s.edit_folders(|l| folder_place_server(l, "folder-A", "s3", 0));
        assert_eq!(s.folders[0].servers, ["s3", "s2"], "reordered within the folder");
        let id = new_folder_id();
        assert!(id.starts_with("folder-") && id.len() == 7 + 26, "folder- plus a ULID");
    }

    #[test]
    fn neutral_folder_edits_write_only_their_fields() {
        use godwit_core::folder_edit::FolderEdit as E;
        use godwit_core::ids::{AccountId, ServerId};
        use godwit_core::network::Network;
        use serde_json::json;
        let acc = AccountId::new(Network::Stoat, "me");
        let id = |raw: &str| ServerId::new(acc.clone(), raw);
        let mut s = Store::default();
        s.apply_synced(&json!({ "server-folders": [0, json!({"folders": [
            {"id": "folder-A", "name": "A", "colour": "#112233", "servers": ["s1", "s2"], "extra": true},
        ], "other": 1}).to_string()] }));
        let mut edit = |e: E| -> Value {
            serde_json::from_str(
                &s.edit_folders(|l| apply_folder_edit(l, &e, "folder-NEW".into()))
                    .unwrap(),
            )
            .unwrap()
        };
        // Create: a new folder, the old one untouched.
        let out = edit(E::Create {
            servers: [id("s3"), id("s4")],
        });
        assert_eq!(out["folders"][1]["id"], json!("folder-NEW"));
        assert_eq!(out["folders"][1]["servers"], json!(["s3", "s4"]));
        assert_eq!(out["other"], json!(1), "unknown top-level fields stay");
        // Add: s3 joins A and leaves the new one, which dissolves with one left.
        let out = edit(E::AddServer {
            folder: id("folder-A"),
            server: id("s3"),
            at: None,
        });
        assert_eq!(out["folders"].as_array().unwrap().len(), 1);
        assert_eq!(out["folders"][0]["servers"], json!(["s1", "s2", "s3"]));
        assert_eq!(out["folders"][0]["extra"], json!(true), "unknown folder fields stay");
        // Rename and recolour change just their own key.
        let out = edit(E::Rename {
            folder: id("folder-A"),
            name: Some("Mates".into()),
        });
        assert_eq!(
            (&out["folders"][0]["name"], &out["folders"][0]["colour"]),
            (&json!("Mates"), &json!("#112233"))
        );
        let out = edit(E::Recolour {
            folder: id("folder-A"),
            colour: Some(0x5470ec),
        });
        assert_eq!(
            (&out["folders"][0]["name"], &out["folders"][0]["colour"]),
            (&json!("Mates"), &json!("#5470ec"))
        );
        let out = edit(E::Recolour {
            folder: id("folder-A"),
            colour: None,
        });
        assert_eq!(out["folders"][0]["colour"], Value::Null);
        // Remove: a folder of three keeps two; of two, it dissolves.
        let out = edit(E::RemoveServer { server: id("s3") });
        assert_eq!(out["folders"][0]["servers"], json!(["s1", "s2"]));
        let out = edit(E::RemoveServer { server: id("s1") });
        assert!(out["folders"].as_array().unwrap().is_empty());
        assert!(s.folders.is_empty());
    }

    #[test]
    fn notification_edits_keep_other_fields() {
        use serde_json::json;
        let mut s = Store::default();
        assert!(
            s.set_notification("server", "S", Some(json!("all"))).is_none(),
            "not before synced settings load"
        );
        s.apply_synced(&json!({ "notifications": [0, json!({"server": {"S": "none"}, "future": 1}).to_string()] }));
        let out: Value =
            serde_json::from_str(&s.set_notification("server_mutes", "S", Some(json!({}))).unwrap()).unwrap();
        assert_eq!(out["future"], json!(1));
        assert_eq!(out["server"]["S"], json!("none"));
        assert!(s.notification_prefs.server_mutes.contains_key("S"));
        let out: Value = serde_json::from_str(&s.set_notification("server", "S", None).unwrap()).unwrap();
        assert!(
            out["server"].get("S").is_none(),
            "None removes the entry, as the official store does"
        );
    }

    #[test]
    fn notification_edits_round_trip_unknown_fields_and_channel_precedence() {
        use serde_json::json;
        let mut s = Store::default();
        s.apply_synced(&json!({ "notifications": [0, json!({
            "server": {"S": "none"}, "channel": {}, "future": {"keep": true}
        }).to_string()] }));
        let edited = s.set_notification("channel", "C", Some(json!("all"))).unwrap();
        let raw: Value = serde_json::from_str(&edited).unwrap();
        assert_eq!(raw["future"]["keep"], json!(true));
        let reparsed: NotificationPrefs = serde_json::from_value(raw).unwrap();
        let channel: Channel = serde_json::from_value(json!({
            "_id": "C", "channel_type": "TextChannel", "server": "S"
        }))
        .unwrap();
        assert!(reparsed.should_notify(&channel, false, 0));
    }

    #[test]
    fn jumping_back_detaches_until_present() {
        use serde_json::json;
        let mut s = Store::default();
        let msg =
            |id: &str| serde_json::from_value::<Message>(json!({"_id": id, "channel": "c", "author": "u"})).unwrap();
        s.apply_nearby(
            "c",
            MessagesWithUsers {
                messages: vec![msg("010"), msg("011")],
                users: vec![],
                members: vec![],
            },
        );
        let cm = &s.messages["c"];
        assert!(cm.detached && cm.loaded_once && !cm.reached_start);
        // A new message isn't appended to the older window (it would leave a gap).
        s.apply_event(
            json!({"type": "Message", "_id": "099", "channel": "c", "author": "u"}),
            None,
        );
        assert_eq!(s.messages["c"].messages.len(), 2);
    }

    #[test]
    fn idle_channels_trim_oldest_first() {
        let mut s = Store::default();
        let msg = |id: &str| {
            serde_json::from_value::<Message>(serde_json::json!({"_id": id, "channel": "a", "author": "u"})).unwrap()
        };
        for c in ["recent", "idle", "gone", "open"] {
            let cm = s.messages.entry(c.into()).or_default();
            for i in 0..60 {
                cm.messages.insert(format!("{i:03}"), msg(&format!("{i:03}")));
            }
            cm.reached_start = true;
        }
        let now = Instant::now() + Duration::from_secs(3600);
        s.last_viewed.insert("recent".into(), now - Duration::from_secs(60));
        s.last_viewed.insert("idle".into(), now - Duration::from_secs(6 * 60));
        s.last_viewed.insert("gone".into(), now - Duration::from_secs(31 * 60));
        s.trim_idle(Some("open"), now);
        assert_eq!(s.messages["recent"].messages.len(), 60, "recently seen: untouched");
        let idle = &s.messages["idle"];
        assert_eq!(
            idle.messages.keys().next().map(String::as_str),
            Some("010"),
            "oldest go first"
        );
        assert_eq!((idle.messages.len(), idle.reached_start), (50, false));
        assert!(
            !s.messages.contains_key("gone"),
            "long unviewed: dropped, refetched on open"
        );
        assert_eq!(
            s.messages["open"].messages.len(),
            60,
            "the open channel is never trimmed"
        );
    }

    #[test]
    fn mark_unread_moves_the_marker_back() {
        let mut s = Store::default();
        s.channels.insert("a".into(), channel("a", Some("01C"), "TextChannel"));
        s.apply_unreads(vec![]);
        s.mark_read("a", "01C");
        assert!(!s.is_unread("a"));
        s.mark_unread_after("a", "01B");
        assert!(s.is_unread("a"), "messages after the chosen one are unread again");
    }

    #[test]
    fn moving_rail_entries_keeps_other_ordering_fields() {
        use serde_json::json;
        let mut s = Store::default();
        for id in ["A", "B", "C", "D"] {
            s.server_order.push(id.into());
            s.servers.insert(
                id.into(),
                serde_json::from_value(json!({"_id": id, "name": id})).unwrap(),
            );
        }
        let synced = json!({
            "ordering": [0, json!({"servers": ["A", "B", "C", "D"], "extra": 1}).to_string()],
            "server-folders": [0, json!({"folders": [{"id": "F", "servers": ["C", "D"]}]}).to_string()],
        });
        s.apply_synced(&synced);
        // Rail: A, B, folder F (C, D). Move the folder to the front.
        let written: Value = serde_json::from_str(&s.move_rail_entry("F", 0).unwrap()).unwrap();
        assert_eq!(written["serverSidebar"], json!(["F", "C", "D", "A", "B"]));
        assert_eq!(written["extra"], json!(1), "fields we don't use survive");
        assert_eq!(written["servers"], json!(["A", "B", "C", "D"]));
        assert!(matches!(&s.rail()[0], RailEntry::Folder { id, .. } if id == "F"));
        // Past the end clamps to the last place.
        s.move_rail_entry("A", 99);
        assert!(matches!(s.rail().last(), Some(RailEntry::Server(id)) if id == "A"));
    }

    #[test]
    fn rail_ordering_and_folders() {
        use serde_json::json;
        let mut s = Store::default();
        for id in ["A", "B", "C", "D", "E"] {
            s.server_order.push(id.into());
            s.servers.insert(
                id.into(),
                serde_json::from_value(json!({"_id": id, "name": id})).unwrap(),
            );
        }
        let synced = json!({
            "ordering": [1, json!({"serverSidebar": ["C", "folder-1", "A"]}).to_string()],
            "server-folders": [1, json!({"folders": [
                {"id": "folder-1", "name": "F", "servers": ["B", "D", "GONE"]},
                {"id": "folder-2", "name": "Empty", "servers": ["GONE"]},
            ]}).to_string()],
        });
        s.apply_synced(&synced);
        assert_eq!(
            s.rail(),
            vec![
                RailEntry::Server("C".into()),
                RailEntry::Folder {
                    id: "folder-1".into(),
                    servers: vec!["B".into(), "D".into()]
                },
                RailEntry::Server("A".into()),
                // Not in the synced order: appended in API order.
                RailEntry::Server("E".into()),
            ]
        );
    }

    #[test]
    fn notification_rules() {
        let ch = |id: &str, server: Option<&str>, kind: &str| -> Channel {
            serde_json::from_value(serde_json::json!({"_id": id, "channel_type": kind, "server": server})).unwrap()
        };
        let dm = ch("dm", None, "DirectMessage");
        let text = ch("t", Some("S"), "TextChannel");
        let mut p = NotificationPrefs::default();
        // Defaults: DMs notify for everything, server channels only on mention.
        assert!(p.should_notify(&dm, false, 0));
        assert!(!p.should_notify(&text, false, 0));
        assert!(p.should_notify(&text, true, 0));
        // Channel level beats server level.
        p.server.insert("S".into(), "none".into());
        assert!(!p.should_notify(&text, true, 0));
        p.channel.insert("t".into(), "all".into());
        assert!(p.should_notify(&text, false, 0));
        // A muted server silences its channels; timed mutes expire.
        p.server_mutes.insert("S".into(), Mute { until: Some(1000) });
        assert!(!p.should_notify(&text, true, 500));
        assert!(p.should_notify(&text, true, 2000));
        // Synced format: [timestamp, "json"].
        let synced = serde_json::json!([1, "{\"channel\":{\"dm\":\"none\"}}"]);
        assert!(
            !NotificationPrefs::from_synced(&synced)
                .unwrap()
                .should_notify(&dm, true, 0)
        );
    }

    #[test]
    fn voice_state_events() {
        use serde_json::json;
        let mut s = Store::default();
        let state = |u: &str| json!({"id": u, "joined_at": "2026-10-30T00:00:00Z", "is_publishing": true, "is_receiving": true});
        s.apply_event(
            json!({"type": "VoiceChannelJoin", "id": "VC", "state": state("A")}),
            None,
        );
        s.apply_event(
            json!({"type": "VoiceChannelJoin", "id": "VC", "state": state("B")}),
            None,
        );
        s.apply_event(
            json!({"type": "UserVoiceStateUpdate", "id": "A", "channel_id": "VC", "data": {"is_publishing": false}}),
            None,
        );
        assert!(!s.voice["VC"]["A"].is_publishing);
        s.apply_event(
            json!({"type": "VoiceChannelMove", "user": "B", "from": "VC", "to": "VC2", "state": state("B")}),
            None,
        );
        assert_eq!(s.voice["VC"].keys().collect::<Vec<_>>(), vec!["A"]);
        assert!(s.voice["VC2"].contains_key("B"));
        s.apply_event(json!({"type": "VoiceChannelLeave", "id": "VC", "user": "A"}), None);
        assert!(s.voice["VC"].is_empty());
    }

    #[test]
    fn reaction_events() {
        use serde_json::json;
        let mut s = Store::default();
        let msg: Message = serde_json::from_value(json!({"_id": "M", "channel": "C", "author": "A"})).unwrap();
        let cm = s.messages.entry("C".into()).or_default();
        cm.loaded_once = true;
        cm.messages.insert("M".into(), msg);
        let ev =
            |t: &str, user: &str| json!({"type": t, "id": "M", "channel_id": "C", "user_id": user, "emoji_id": "👍"});

        s.apply_event(ev("MessageReact", "U1"), None);
        s.apply_event(ev("MessageReact", "U2"), None);
        s.apply_event(ev("MessageReact", "U2"), None); // duplicate is ignored
        assert_eq!(s.messages["C"].messages["M"].reactions["👍"], vec!["U1", "U2"]);

        s.apply_event(ev("MessageUnreact", "U1"), None);
        s.apply_event(ev("MessageUnreact", "U2"), None);
        assert!(
            s.messages["C"].messages["M"].reactions.is_empty(),
            "empty reactions are dropped"
        );
    }
}
