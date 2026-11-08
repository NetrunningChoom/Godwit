// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! A Discord account as the core sees it: DMs and group DMs, servers with
//! their categories and channels (only those we may see), friends and
//! requests, and messages. Requests go out only for something the user did,
//! and only when Discord would allow them (see `permissions`).

mod channel_admin;
mod commands;
mod events;
mod messages;
mod people;
mod rooms;
mod server_admin;
mod sign_in;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

use godwit_core::account::NetworkAccount;
use godwit_core::application::{AlertKind, Command, Event, Person};
use godwit_core::ids::{AccountId, MessageId, RoomId, ServerId, UserId};
use godwit_core::members::{Member, MemberGroup, Moderation, ModerationRole, Presence, Profile, Relationship};
use godwit_core::message::{Attachment, AttachmentKind, Embed, EmbedStyle, InvitePreview, Markup, Message, Reaction};
use godwit_core::network::Network;
use godwit_core::room::{Capabilities, Room, RoomEncryption, RoomKind};
use godwit_core::server::{Category, Server as CoreServer, ServerRights};
use godwit_core::server_settings::{
    NewChannelKind, ServerAdmin, ServerAdminResult, ServerBan, ServerInvite, ServerOverview, ServerRole, ServerRoles,
};
use serde_json::{Value, json};

use crate::backend::{self, Admin, AdminAnswer, Answer, Cmd, Ev, Handle, Request, Waker};
use crate::permissions as p;
use crate::remote_auth::Step;
use crate::session;
use crate::store::{self, Store};
use crate::voice::CallCmd;
use crate::voice::states::VoiceStates;
use godwit_voice::call::VoiceSettings;

mod calls;

/// Where signing in stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    SignedOut {
        error: Option<String>,
    },
    /// Showing (or waiting for) the QR code; `scanned` names who scanned it.
    Qr {
        url: Option<String>,
        scanned: Option<String>,
        /// Discord wants a captcha solved: the page with it, opened in the browser.
        captcha: Option<String>,
    },
    Connecting,
    Ready,
}

/// A notification shows this much of a message.
const ALERT_CHARS: usize = 200;
/// Someone typing stops showing after this long without another notice.
const TYPING_FOR: std::time::Duration = std::time::Duration::from_secs(10);

pub struct DiscordAccount {
    slot: u32,
    net: Handle,
    events: mpsc::Receiver<Ev>,
    status: Status,
    account: Option<AccountId>,
    store: Store,
    /// Messages with a poll, as Discord last sent them (id → message), so a
    /// vote's counts can change without fetching it again. Filled as
    /// messages are shown (`message` takes `&self`).
    polls: RefCell<HashMap<String, Value>>,
    /// What the core was last sent, to send only changes.
    rooms: godwit_core::room_sync::RoomSync,
    servers: Vec<CoreServer>,
    friends: Vec<Person>,
    requests: (Vec<Person>, Vec<Person>),
    /// The users we blocked, as last sent to the core.
    blocked: Vec<UserId>,
    /// Friends' activities as last sent (Event::Activities), to send changes only.
    activities: Vec<(UserId, Arc<str>)>,
    /// The custom emoji groups as last sent (Event::CustomEmoji).
    emoji_sent: Option<Vec<godwit_core::message::EmojiGroup>>,
    /// The channel on screen.
    viewing: Option<String>,
    /// The call's video is on screen (its room, or the app's pop-out).
    call_shown: bool,
    /// Guilds whose typing we subscribed to this session.
    subscribed: std::collections::HashSet<String>,
    typing_sent: HashMap<String, Instant>,
    /// Who's typing where: channel → user → since.
    typing: HashMap<String, HashMap<String, Instant>>,
    /// Our nonce → the core's, for messages on their way.
    pending: HashMap<String, Arc<str>>,
    /// The profile card on screen, and what Discord's profile said.
    profile_for: Option<(String, Option<String>)>,
    profiles: HashMap<String, Value>,
    /// Our own profile asked for (Settings → Profile): loading, or reloading
    /// after a save (true).
    own_profile: Option<bool>,
    /// The data-use consents as Discord last said (key → on), once asked.
    consents: Option<Vec<(String, bool)>>,
    /// The server whose Roles page is open, and the roles last sent.
    roles_watch: Option<(String, ServerRoles)>,
    /// The server whose Emoji page is open, and the list last sent.
    emoji_page: Option<(String, godwit_core::server_settings::ServerEmojis)>,
    /// A DM being opened from a profile, to open once it exists.
    opening_dm: bool,
    /// New codes shown since the form opened: the login gateway closes a
    /// code after about two minutes and the web login shows another, a few
    /// times before it stops.
    renewals: u32,
    /// Member lists as GUILD_MEMBER_LIST_UPDATE streams them, by guild and
    /// the list id each update names (see `member_list`).
    member_lists: crate::member_list::MemberLists,
    /// The ranges last asked for, by guild (op 37 again only when they change).
    list_ranges: HashMap<String, Vec<[usize; 2]>>,
    /// Each guild's channel whose member list we subscribed to.
    list_channel: HashMap<String, String>,
    /// Reactions of messages shown, by message id, so a live change can send
    /// the whole list again (the core replaces it).
    reactions: HashMap<String, (String, Vec<Reaction>)>,
    offline: bool,
    /// Microphone, speakers and processing for calls (from Settings).
    voice_settings: VoiceSettings,
    /// Who's in which voice channel and DM call.
    voice: VoiceStates,
    /// Our main gateway session, which our voice state must name.
    session_id: Option<String>,
    /// Our call, and the tag the next one gets.
    call: Option<calls::OurCall>,
    next_call: u64,
    /// DM calls ringing: channel → who's being rung.
    ringing: HashMap<String, Vec<String>>,
    /// Demo mode: canned history by channel, answered here, never asked of
    /// Discord (see [`DiscordAccount::demo`]).
    demo: Option<HashMap<String, Vec<Value>>>,
}

impl DiscordAccount {
    /// Starts the network thread and connects with the sign-in kept in `slot`.
    pub fn start(wake: Waker, slot: u32) -> std::io::Result<Self> {
        let (net, events) = backend::spawn(wake)?;
        let status = match session::load(slot) {
            Some(saved) => {
                net.send(Cmd::UseSession { token: saved.token });
                Status::Connecting
            }
            None => Status::SignedOut { error: None },
        };
        Ok(Self::with(slot, net, events, status))
    }

    fn with(slot: u32, net: Handle, events: mpsc::Receiver<Ev>, status: Status) -> Self {
        Self {
            slot,
            net,
            events,
            status,
            account: None,
            polls: RefCell::new(HashMap::new()),
            store: Store::default(),
            rooms: Default::default(),
            servers: Vec::new(),
            friends: Vec::new(),
            requests: (Vec::new(), Vec::new()),
            blocked: Vec::new(),
            activities: Vec::new(),
            emoji_sent: None,
            viewing: None,
            call_shown: false,
            subscribed: Default::default(),
            typing_sent: HashMap::new(),
            typing: HashMap::new(),
            pending: HashMap::new(),
            profile_for: None,
            profiles: HashMap::new(),
            own_profile: None,
            consents: None,
            roles_watch: None,
            emoji_page: None,
            opening_dm: false,
            renewals: 0,
            member_lists: crate::member_list::MemberLists::default(),
            list_ranges: HashMap::new(),
            list_channel: HashMap::new(),
            reactions: HashMap::new(),
            offline: false,
            voice_settings: VoiceSettings::default(),
            voice: VoiceStates::default(),
            session_id: None,
            call: None,
            next_call: 0,
            ringing: HashMap::new(),
            demo: None,
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

    fn room_id(&self, channel: &str) -> Option<RoomId> {
        Some(RoomId::new(self.account.clone()?, channel))
    }

    fn user_id(&self, user: &str) -> Option<UserId> {
        Some(UserId::new(self.account.clone()?, user))
    }

    fn me(&self) -> &str {
        self.store.me.as_ref().map(|u| u.id.as_str()).unwrap_or_default()
    }
}

/// A reaction object as the core shows it; custom emoji are `name:id`.
fn reaction(r: &Value) -> Option<Reaction> {
    let emoji = &r["emoji"];
    let image_url = emoji["id"]
        .as_str()
        .map(|id| store::emoji_url(id, emoji["animated"] == true).into());
    Some(Reaction {
        emoji: emoji_key(emoji).into(),
        image_url,
        count: r["count"].as_u64().and_then(|c| u32::try_from(c).ok()).unwrap_or(1),
        me: r["me"] == true,
    })
}

/// What reacting sends: the emoji itself, or `name:id` for a custom one.
fn emoji_key(emoji: &Value) -> String {
    match emoji["id"].as_str() {
        Some(id) => format!("{}:{id}", emoji["name"].as_str().unwrap_or("emoji")),
        None => emoji["name"].as_str().unwrap_or_default().to_owned(),
    }
}

fn embed(e: &Value) -> Option<Embed> {
    let kind = e["type"].as_str().unwrap_or("rich");
    let image = e["image"]["url"].as_str().or_else(|| e["thumbnail"]["url"].as_str());
    let dims = |v: &Value| {
        v["width"]
            .as_u64()
            .and_then(|w| u32::try_from(w).ok())
            .zip(v["height"].as_u64().and_then(|h| u32::try_from(h).ok()))
    };
    let style = match kind {
        "image" => EmbedStyle::Media,
        "gifv" => EmbedStyle::Gif,
        _ => EmbedStyle::Card,
    };
    let colour = e["color"].as_u64().map(|c| [(c >> 16) as u8, (c >> 8) as u8, c as u8]);
    // Bots' fields and footer, below the description (Discord lays fields
    // out in columns; here they follow each other).
    let mut description = e["description"].as_str().unwrap_or_default().to_owned();
    for f in e["fields"].as_array().into_iter().flatten() {
        let (name, value) = (
            f["name"].as_str().unwrap_or_default(),
            f["value"].as_str().unwrap_or_default(),
        );
        if name.is_empty() && value.is_empty() {
            continue;
        }
        if !description.is_empty() {
            description.push_str("\n\n");
        }
        description.push_str(&format!("**{name}**\n{value}"));
    }
    if let Some(footer) = e["footer"]["text"].as_str().filter(|t| !t.is_empty()) {
        if !description.is_empty() {
            description.push_str("\n\n");
        }
        description.push_str(&format!("*{footer}*"));
    }
    let embed = Embed {
        title: e["title"].as_str().map(Into::into),
        description: Some(description).filter(|d| !d.is_empty()).map(Into::into),
        url: e["url"].as_str().map(Into::into),
        site_name: e["provider"]["name"]
            .as_str()
            .or_else(|| e["author"]["name"].as_str())
            .map(Into::into),
        image_url: image.map(Into::into),
        video_url: e["video"]["proxy_url"]
            .as_str()
            .filter(|_| kind == "gifv")
            .map(Into::into),
        // music.youtube.com and Spotify links come as plain cards: their page plays too.
        player_url: player(e)
            .or_else(|| e["url"].as_str().and_then(godwit_core::message::link_player))
            .map(Into::into),
        media_dimensions: dims(&e["image"]).or_else(|| dims(&e["thumbnail"])),
        image_thumbnail: e["image"].is_null() && !e["thumbnail"].is_null() && kind != "image",
        colour,
        icon_url: e["author"]["icon_url"].as_str().map(Into::into),
        style,
        description_markup: if kind == "rich" { Markup::Rich } else { Markup::Plain },
    };
    (embed.title.is_some() || embed.description.is_some() || embed.image_url.is_some()).then_some(embed)
}

/// The largest upload Discord takes: Nitro's own limit, or a boosted
/// server's, whichever is larger (Yapfast's `upload_limit`; 10 MB without
/// either since Discord lowered it).
fn upload_limit(premium_type: u8, premium_tier: Option<u8>) -> u64 {
    const MB: u64 = 1024 * 1024;
    let own = match premium_type {
        2 => 500 * MB,
        1 | 3 => 50 * MB,
        _ => 10 * MB,
    };
    let boosted = match premium_tier {
        Some(3) => 100 * MB,
        Some(2) => 50 * MB,
        _ => 0,
    };
    own.max(boosted)
}

/// A poll, as text under the message (Yapfast's `Poll::of`): the question,
/// each answer with its votes (✓ for ours), and whether it's over. Voting
/// from Godwit isn't built yet.
fn poll_text(poll: &Value, votable: bool) -> Option<String> {
    let question = poll["question"]["text"].as_str()?;
    let counts = poll["results"]["answer_counts"].as_array();
    let mut out = format!("📊 **{}**", godwit_core::rich::escape(question));
    for answer in poll["answers"].as_array().into_iter().flatten() {
        let id = answer["answer_id"].as_u64();
        let media = &answer["poll_media"];
        let emoji = media["emoji"]["name"]
            .as_str()
            .filter(|n| !n.is_empty() && media["emoji"]["id"].is_null());
        let text = media["text"].as_str().unwrap_or_default();
        let count = counts
            .into_iter()
            .flatten()
            .find(|c| c["id"].as_u64() == id && id.is_some());
        let votes = count.and_then(|c| c["count"].as_u64()).unwrap_or(0);
        let mine = count.and_then(|c| c["me_voted"].as_bool()) == Some(true);
        let finalized = poll["results"]["is_finalized"].as_bool() == Some(true);
        // An open poll's answers are links: clicking one votes (or takes it back).
        let label = match id.filter(|_| votable && !finalized) {
            Some(id) => format!("[{}](godwit://vote/{id})", godwit_core::rich::escape(text)),
            None => godwit_core::rich::escape(text),
        };
        out.push_str(&format!(
            "\n• {}{} — {} vote{}{}",
            emoji.map(|e| format!("{e} ")).unwrap_or_default(),
            label,
            votes,
            if votes == 1 { "" } else { "s" },
            if mine { " ✓" } else { "" }
        ));
    }
    let finalized = poll["results"]["is_finalized"].as_bool() == Some(true);
    out.push_str(if finalized {
        "\n*Poll ended*"
    } else if votable {
        "\n*Click an answer to vote*"
    } else {
        "\n*Vote on the original message*"
    });
    Some(out)
}

/// Stickers sent with a message, shown large (Discord's CDN; Lottie ones,
/// format 3, aren't pictures and show as their name).
fn stickers(m: &Value) -> Vec<(Arc<str>, Arc<str>)> {
    m["sticker_items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let id = s["id"].as_str()?;
            let name = s["name"].as_str().unwrap_or("Sticker");
            let ext = match s["format_type"].as_u64() {
                Some(3) => return None,
                Some(4) => "gif",
                _ => "png",
            };
            Some((
                format!("https://media.discordapp.net/stickers/{id}.{ext}?size=160").into(),
                name.into(),
            ))
        })
        .collect()
}

/// A video embed's player, from Discord's `video.url`: YouTube through its
/// no-cookie player (as Stoat's are shown), Twitch through its own. Ids are
/// checked before they go into an address.
fn player(e: &Value) -> Option<String> {
    let url = e["video"]["url"].as_str()?;
    let valid =
        |s: &str| !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || "_-".contains(c));
    if let Some(rest) = url
        .strip_prefix("https://www.youtube.com/embed/")
        .or_else(|| url.strip_prefix("https://youtube.com/embed/"))
    {
        let id = rest.split(['?', '&', '/']).next().unwrap_or_default();
        let start = rest
            .split(['?', '&'])
            .find_map(|p| p.strip_prefix("start="))
            .filter(|t| !t.is_empty() && t.len() <= 8 && t.chars().all(|c| c.is_ascii_digit()));
        return valid(id).then(|| {
            format!(
                "https://www.youtube-nocookie.com/embed/{id}?modestbranding=1{}",
                start.map_or_else(String::new, |t| format!("&start={t}"))
            )
        });
    }
    let rest = url.strip_prefix("https://player.twitch.tv/?")?;
    let (kind, id) = rest.split('&').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        matches!(k, "channel" | "video" | "clip").then_some((k, v))
    })?;
    valid(id).then(|| format!("https://player.twitch.tv/?{kind}={id}&parent=discord.com&autoplay=false"))
}

/// What a system message says (the common kinds; others show their text).
fn system_text(kind: u64, who: &str, content: &str) -> String {
    match kind {
        1 => format!("{who} added someone to the group."),
        2 => format!("{who} removed someone from the group."),
        3 => format!("{who} started a call."),
        4 => format!("{who} changed the group's name: {content}"),
        5 => format!("{who} changed the group's icon."),
        6 => format!("{who} pinned a message."),
        7 => format!("{who} joined the server."),
        8 => format!("{who} boosted the server."),
        9..=11 => format!("{who} boosted the server: it reached level {}.", kind - 8),
        12 => format!("{who} followed a channel into this one."),
        18 => format!("{who} started a thread: {content}"),
        24 => "AutoMod blocked a message.".to_owned(),
        27 => format!("{who} joined the stage to speak."),
        36 | 37 => format!("{who} changed a server setting."),
        46 => "A poll ended.".to_owned(),
        _ if !content.is_empty() => content.to_owned(),
        _ => format!("{who} did something Godwit can't show yet."),
    }
}

/// Invite codes a message links plainly: `discord.gg/CODE`,
/// `discord.com/invite/CODE`.
pub fn invite_codes(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || c == '<' || c == '>') {
        if let Some(code) = invite_code(word)
            && !out.contains(&code)
        {
            out.push(code);
        }
    }
    out
}

pub fn invite_code(input: &str) -> Option<String> {
    let s = input.trim();
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let code = rest
        .strip_prefix("discord.gg/")
        .or_else(|| rest.strip_prefix("discord.com/invite/"))
        .or_else(|| rest.strip_prefix("discordapp.com/invite/"))?;
    let code = code.split(['?', '#', '/']).next()?;
    (!code.is_empty() && code.len() <= 32 && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        .then(|| code.to_owned())
}

/// The permissions a role editor shows for Discord, in its client's order.
fn permission_list() -> Arc<[godwit_core::server_settings::PermissionInfo]> {
    use godwit_core::server_settings::PermissionInfo;
    let row = |heading: Option<&str>, bit: u64, title: &str, description: &str| PermissionInfo {
        bit,
        heading: heading.map(Into::into),
        title: title.into(),
        description: description.into(),
    };
    Arc::from(vec![
        row(
            Some("General Server Permissions"),
            p::VIEW_CHANNEL,
            "View Channels",
            "Members can view channels by default (excluding private channels).",
        ),
        row(
            None,
            p::MANAGE_CHANNELS,
            "Manage Channels",
            "Create, edit and delete channels.",
        ),
        row(
            None,
            p::MANAGE_ROLES,
            "Manage Roles",
            "Create new roles and edit or delete roles lower than their highest role.",
        ),
        row(
            None,
            p::MANAGE_GUILD,
            "Manage Server",
            "Change the server's name, region and other settings.",
        ),
        row(
            Some("Membership Permissions"),
            p::CREATE_INSTANT_INVITE,
            "Create Invite",
            "Invite new people to this server.",
        ),
        row(
            None,
            p::KICK_MEMBERS,
            "Kick Members",
            "Remove other members from this server.",
        ),
        row(
            None,
            p::BAN_MEMBERS,
            "Ban Members",
            "Permanently ban other members from this server.",
        ),
        row(
            Some("Text Channel Permissions"),
            p::SEND_MESSAGES,
            "Send Messages",
            "Send messages in text channels.",
        ),
        row(
            None,
            p::EMBED_LINKS,
            "Embed Links",
            "Links shared show embedded content.",
        ),
        row(
            None,
            p::ATTACH_FILES,
            "Attach Files",
            "Upload files or media in text channels.",
        ),
        row(
            None,
            p::ADD_REACTIONS,
            "Add Reactions",
            "Add new emoji reactions to messages.",
        ),
        row(
            None,
            p::MENTION_EVERYONE,
            "Mention @everyone, @here, and All Roles",
            "Use @everyone or @here to notify everyone.",
        ),
        row(
            None,
            p::MANAGE_MESSAGES,
            "Manage Messages",
            "Delete messages by other members or pin any message.",
        ),
        row(
            None,
            p::READ_MESSAGE_HISTORY,
            "Read Message History",
            "Read messages sent before they joined.",
        ),
        row(
            Some("Voice Channel Permissions"),
            p::CONNECT,
            "Connect",
            "Join voice channels and hear others.",
        ),
        row(None, p::SPEAK, "Speak", "Talk in voice channels."),
        row(
            Some("Advanced Permissions"),
            p::ADMINISTRATOR,
            "Administrator",
            "Every permission, and bypasses channel-specific permissions. Dangerous to grant.",
        ),
    ])
}

/// Names for mentions in one server's (or a DM's) messages.
struct Names<'a> {
    store: &'a Store,
    guild: Option<String>,
}

impl crate::markdown::Names for Names<'_> {
    // Bare names: the rich-text conversion adds the `@` and `#`.
    fn user(&self, id: &str) -> String {
        self.store.display_name(id, self.guild.as_deref())
    }
    fn channel(&self, id: &str) -> String {
        self.store
            .channels
            .get(id)
            .map_or_else(|| "unknown-channel".to_owned(), |c| c.name.clone())
    }
    fn role(&self, id: &str) -> String {
        let name = self
            .guild
            .as_deref()
            .and_then(|g| self.store.guilds.get(g))
            .and_then(|g| g.roles.get(id))
            .map_or("unknown-role", |r| r.name.as_str());
        name.to_owned()
    }
    fn emoji(&self, id: &str) -> String {
        self.store
            .guilds
            .values()
            .find_map(|g| g.emojis.get(id).map(|(n, _)| n.clone()))
            .unwrap_or_else(|| "emoji".into())
    }
    fn emoji_url(&self, id: &str) -> Option<String> {
        let animated = self
            .store
            .guilds
            .values()
            .find_map(|g| g.emojis.get(id).map(|(_, a)| *a))
            .unwrap_or(false);
        Some(store::emoji_url(id, animated))
    }
}

impl NetworkAccount for DiscordAccount {
    fn network(&self) -> Network {
        Network::Discord
    }

    fn can_edit_profile(&self) -> bool {
        true
    }

    fn id(&self) -> Option<&AccountId> {
        self.account.as_ref()
    }

    fn can_go_offline(&self) -> bool {
        true
    }

    fn set_offline(&mut self, offline: bool) -> Vec<Event> {
        if offline == self.offline {
            return Vec::new();
        }
        self.offline = offline;
        self.net.send(Cmd::Offline(offline));
        Vec::new()
    }

    fn connecting(&self) -> bool {
        self.id().is_none() && matches!(self.status(), Status::Connecting)
    }

    fn poll(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        godwit_core::backend::poll(self, |s| s.events.try_recv().ok(), Self::handle, &mut out);
        // Typing notices that ran out.
        let now = Instant::now();
        let stale: Vec<String> = self
            .typing
            .iter()
            .filter(|(_, m)| m.values().any(|since| now.duration_since(*since) >= TYPING_FOR))
            .map(|(c, _)| c.clone())
            .collect();
        for c in stale {
            self.typing_event(&c, &mut out);
        }
        out
    }

    fn run(&mut self, command: Command) -> Vec<Event> {
        if let Some(out) = self.demo_answer(&command) {
            return out;
        }
        let mut out = Vec::new();
        let req = |net: &Handle, r: Request| net.send(Cmd::Request(r));
        match command {
            Command::Focus { room } => {
                self.viewing = room.as_ref().and_then(|r| self.channel_of(r));
                // Subscribing, as opening a channel in the web client does,
                // brings the server's typing notices and the channel's
                // member list (its first hundred rows).
                let opened = self
                    .viewing
                    .as_ref()
                    .and_then(|c| Some((self.store.channels.get(c)?.guild.clone()?, c.clone())));
                if let Some((guild, channel)) = opened {
                    let first = self.subscribed.insert(guild.clone());
                    // When its voice channels' sessions began, for the time
                    // beside them; VOICE_CHANNEL_START_TIME_UPDATE follows.
                    if first {
                        self.net.send(Cmd::Gateway {
                            op: crate::gateway::REQUEST_CHANNEL_INFO,
                            data: json!({"guild_id": guild, "fields": ["voice_start_time"]}),
                        });
                    }
                    let moved = self.list_channel.get(&guild) != Some(&channel);
                    if first || moved {
                        self.list_channel.insert(guild.clone(), channel.clone());
                        // Only the new channel's list is wanted (kept if it's
                        // the same list, as most channels share "everyone").
                        let id = self.store.member_list_id(&channel);
                        self.member_lists.keep_only(&guild, &id);
                        self.list_ranges.remove(&guild);
                        self.ask_ranges(&guild, &channel, 0, 0);
                    }
                }
            }
            Command::LoadHistory { room, before } => {
                if let Some(channel) = self.channel_of(&room) {
                    // Never ask for what Discord would refuse; a forum has no
                    // messages of its own (its posts are the threads under it).
                    let forum = self.store.channels.get(&channel).is_some_and(store::Channel::forum);
                    if forum || !self.store.can(&channel, p::VIEW_CHANNEL | p::READ_MESSAGE_HISTORY) {
                        out.push(Event::History {
                            room,
                            before,
                            messages: Vec::new(),
                            reached_start: true,
                        });
                    } else {
                        req(
                            &self.net,
                            Request::History {
                                channel,
                                before: before.map(|b| b.0.to_string()),
                                around: None,
                            },
                        );
                    }
                }
            }
            Command::LoadAround { room, message } => {
                if let Some(channel) = self.channel_of(&room) {
                    req(
                        &self.net,
                        Request::History {
                            channel,
                            before: None,
                            around: Some(message.0.to_string()),
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
                // Threads take their own permission to write in.
                let thread = self.store.channels.get(&channel).is_some_and(store::Channel::thread);
                let needed = if thread {
                    p::SEND_MESSAGES_IN_THREADS
                } else {
                    p::SEND_MESSAGES
                };
                if !self.store.can(&channel, needed) {
                    out.push(Event::MessageFailed {
                        room,
                        nonce,
                        error: "You can't send messages here.".into(),
                    });
                    return out;
                }
                // Discord's own limit: 2,000 characters, 4,000 with Nitro. Longer
                // is refused here rather than sent (nothing claims Nitro we don't have).
                let limit = if self.store.premium_type == 2 { 4000 } else { 2000 };
                if text.chars().count() > limit {
                    out.push(Event::MessageFailed {
                        room,
                        nonce,
                        error: format!("Discord takes at most {limit} characters in a message.").into(),
                    });
                    return out;
                }
                let ours = crate::api::nonce();
                self.pending.insert(ours.clone(), nonce);
                let guild = self.store.channels.get(&channel).and_then(|c| c.guild.clone());
                req(
                    &self.net,
                    Request::Send {
                        guild,
                        channel,
                        text,
                        nonce: ours,
                        reply: reply_to.map(|r| r.0.to_string()),
                        files: files.into_iter().map(|f| (f.path, f.name.to_string())).collect(),
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
                            text,
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
                            add,
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
                        .is_none_or(|t| now.duration_since(*t) >= backend::TYPING_EVERY)
                    {
                        self.typing_sent.insert(channel.clone(), now);
                        req(&self.net, Request::Typing { channel });
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
                    req(&self.net, Request::Ack { channel, message: last });
                    self.sync_rooms(&mut out);
                }
            }
            Command::MarkRead { room, last } => {
                if let Some(channel) = self.channel_of(&room) {
                    let was_unread = self.store.unread(&channel) || self.store.mentions(&channel) > 0;
                    // Read up to the channel's newest message, as the official
                    // client acks: a deleted last message (Discord keeps its id
                    // as `last_message_id`) or one we don't show would
                    // otherwise leave the chat unread for good.
                    let message = self
                        .store
                        .channels
                        .get(&channel)
                        .and_then(|c| c.last_message_id.clone())
                        .filter(|newest| store::newer(newest, &last.0))
                        .unwrap_or_else(|| last.0.to_string());
                    self.store.mark_read(&channel, &message);
                    // Only acknowledge what wasn't read: each ack is a request.
                    if was_unread {
                        req(&self.net, Request::Ack { channel, message });
                    }
                    self.sync_rooms(&mut out);
                }
            }
            Command::OpenDirect { user } => {
                let raw = user.raw.to_string();
                let existing = self
                    .store
                    .channels
                    .values()
                    .find(|c| c.kind == store::DM && c.recipients == [raw.clone()])
                    .map(|c| c.id.clone());
                match existing.and_then(|c| self.room_id(&c)) {
                    Some(room) => out.push(Event::RoomOpened(room)),
                    None => {
                        self.opening_dm = true;
                        req(&self.net, Request::OpenDm { user: raw });
                    }
                }
            }
            Command::AddFriend { user } => req(
                &self.net,
                Request::AddFriend {
                    user: user.raw.to_string(),
                },
            ),
            Command::RemoveFriend { user } => req(
                &self.net,
                Request::RemoveFriend {
                    user: user.raw.to_string(),
                },
            ),
            // The web client offers Block for anyone but yourself.
            Command::BlockUser { user } if user.raw.as_ref() != self.me() => req(
                &self.net,
                Request::BlockUser {
                    user: user.raw.to_string(),
                },
            ),
            Command::UnblockUser { user } if user.raw.as_ref() != self.me() => req(
                &self.net,
                Request::UnblockUser {
                    user: user.raw.to_string(),
                },
            ),
            Command::SendFriendRequest { request, name } => req(
                &self.net,
                Request::AddFriendByName {
                    request,
                    name: name.trim().to_owned(),
                },
            ),
            Command::LoadSessions => {
                req(&self.net, Request::Sessions);
                if let Some(account) = self.account.clone() {
                    out.push(Event::Sessions {
                        account,
                        view: godwit_core::privacy::SessionsView {
                            loading: true,
                            ..Default::default()
                        },
                    });
                }
            }
            Command::LogOutSession(id) => req(&self.net, Request::LogOutSession(id.to_string())),
            Command::LoadPrivacy => {
                req(&self.net, Request::Consents);
                self.send_privacy(true, None, &mut out);
            }
            Command::SetPrivacy { key, on } => {
                if &*key == SHOW_ACTIVITY {
                    self.store.show_game = Some(on);
                    req(&self.net, Request::ShowActivity(on));
                } else if let Some(consents) = self.consents.as_mut()
                    && let Some(entry) = consents.iter_mut().find(|(k, _)| *k == *key)
                {
                    entry.1 = on;
                    req(
                        &self.net,
                        Request::SetConsent {
                            key: key.to_string(),
                            grant: on,
                        },
                    );
                }
                self.send_privacy(false, None, &mut out);
            }
            Command::LoadOwnProfile => {
                if let Some(me) = self.store.me.as_ref().map(|m| m.id.clone()) {
                    self.own_profile = Some(false);
                    req(&self.net, Request::Profile { user: me, guild: None });
                }
            }
            Command::SaveOwnProfile(edit) => match own_profile_request(edit) {
                Ok(request) => req(&self.net, request),
                Err(error) => {
                    if let Some(account) = self.account.clone() {
                        out.push(Event::OwnProfileSaved {
                            account,
                            result: Err(error.into()),
                        });
                    }
                }
            },
            Command::LoadProfile { user, room } => {
                let guild = room
                    .as_ref()
                    .and_then(|r| self.store.channels.get(r.raw.as_ref()))
                    .and_then(|c| c.guild.clone());
                let raw = user.raw.to_string();
                self.profile_for = Some((raw.clone(), guild.clone()));
                self.sync_profile(&mut out);
                if !self.profiles.contains_key(&raw) {
                    req(&self.net, Request::Profile { user: raw, guild });
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
            Command::LoadMembers { room } => {
                let Some(channel) = self.channel_of(&room) else {
                    return out;
                };
                // A server channel's list streams in through the subscription.
                if let Some(groups) = self.list_groups(&channel) {
                    out.push(Event::Members { room, groups });
                    return out;
                }
                // A server channel's list streams in shortly: showing who's
                // known first, then swapping to the streamed list (by role),
                // flickered the panel each time a channel opened. The list
                // shown before stays until the stream's arrives.
                if self.store.channels.get(&channel).is_some_and(|c| !c.private()) {
                    return out;
                }
                // DMs and group DMs: their recipients, by presence.
                let Some(account) = self.account.clone() else {
                    return out;
                };
                let c = self.store.channels.get(&channel);
                let ids: Vec<String> = match c {
                    Some(c) if c.private() => c
                        .recipients
                        .iter()
                        .cloned()
                        .chain(std::iter::once(self.me().to_owned()))
                        .collect(),
                    _ => Vec::new(),
                };
                let guild = c.and_then(|c| c.guild.clone());
                let member = |u: &String| Member {
                    user: UserId::new(account.clone(), u.as_str()),
                    name: self.store.display_name(u, guild.as_deref()).into(),
                    avatar_url: Some(self.avatar(u, guild.as_deref()).into()),
                    colour: guild.as_deref().and_then(|g| self.store.colour(u, g)),
                    presence: self.presence(u),
                    status: None,
                };
                let (mut online, mut offline): (Vec<Member>, Vec<Member>) = ids
                    .iter()
                    .map(member)
                    .partition(|m| !matches!(m.presence, Presence::Offline));
                online.sort_by_key(|a| a.name.to_lowercase());
                offline.sort_by_key(|a| a.name.to_lowercase());
                let mut groups = Vec::new();
                if !online.is_empty() {
                    groups.push(MemberGroup {
                        title: format!("Online — {}", online.len()).into(),
                        members: online,
                    });
                }
                if !offline.is_empty() {
                    groups.push(MemberGroup {
                        title: format!("Offline — {}", offline.len()).into(),
                        members: offline,
                    });
                }
                out.push(Event::Members { room, groups });
            }
            Command::SetStatusText(text) => {
                let text = text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
                self.store.custom_status = text.clone();
                req(&self.net, Request::CustomStatus(text.clone()));
                self.net.send(Cmd::Gateway {
                    op: crate::gateway::PRESENCE_UPDATE,
                    data: crate::settings::presence(&self.store.status, text.as_deref()),
                });
                if let Some(account) = self.account.clone() {
                    let presence = match self.store.status.as_str() {
                        "idle" => Presence::Idle,
                        "dnd" => Presence::Busy,
                        "invisible" => Presence::Invisible,
                        _ => Presence::Online,
                    };
                    out.push(Event::OwnPresence {
                        account,
                        presence,
                        status: text.map(Into::into),
                    });
                }
            }
            Command::SetPresence(presence) => {
                let status = match presence {
                    Presence::Idle => "idle",
                    Presence::Busy | Presence::Focus => "dnd",
                    Presence::Invisible | Presence::Offline => "invisible",
                    _ => "online",
                };
                self.store.status = status.to_owned();
                req(&self.net, Request::Status(status.to_owned()));
                self.net.send(Cmd::Gateway {
                    op: crate::gateway::PRESENCE_UPDATE,
                    data: crate::settings::presence(status, self.store.custom_status.as_deref()),
                });
                if let Some(account) = self.account.clone() {
                    out.push(Event::OwnPresence {
                        account,
                        presence,
                        status: self.store.custom_status.clone().map(Into::into),
                    });
                }
            }
            Command::LoadInvite { code } => req(&self.net, Request::Invite { code: code.to_string() }),
            Command::LoadGifCategories => req(&self.net, Request::GifCategories),
            Command::AnswerInvite { room, accept } => {
                if let Some(channel) = self.channel_of(&room) {
                    self.net.send(Cmd::Request(Request::AnswerRequest { channel, accept }));
                }
            }
            Command::CreateThread {
                room,
                name,
                message,
                text,
            } => {
                if let Some(channel) = self.channel_of(&room) {
                    self.net.send(Cmd::Request(Request::CreateThread {
                        channel,
                        message: message.map(|m| m.0.to_string()),
                        name,
                        text,
                    }));
                }
            }
            Command::SearchMessages { room, query } => {
                if let Some(channel) = self.channel_of(&room) {
                    let guild = self.store.channels.get(&channel).and_then(|c| c.guild.clone());
                    self.net.send(Cmd::Request(Request::Search { guild, channel, query }));
                }
            }
            Command::VotePoll { room, message, answer } => {
                if let Some(channel) = self.channel_of(&room) {
                    self.vote(&room, &channel, &message.0, answer, &mut out);
                }
            }
            Command::LoadPins { room } => {
                if let Some(channel) = self.channel_of(&room) {
                    req(&self.net, Request::Pins { channel });
                }
            }
            Command::SetPinned { room, message, pinned } => {
                if let Some(channel) = self.channel_of(&room) {
                    req(
                        &self.net,
                        Request::Pin {
                            channel,
                            message: message.0.to_string(),
                            pinned,
                        },
                    );
                }
            }
            // Discord answers a search all at once: a "next page" is empty.
            Command::SearchGifs { query, position: None } => req(&self.net, Request::Gifs { query }),
            Command::SearchGifs {
                query,
                position: Some(_),
            } => {
                if let Some(account) = self.account.clone() {
                    out.push(Event::Gifs {
                        account,
                        query,
                        more: true,
                        results: Vec::new(),
                        next: None,
                    });
                }
            }
            Command::JoinInvite { code } => req(&self.net, Request::JoinInvite { code: code.to_string() }),
            Command::ServerAdmin { server, op } => out.extend(self.server_admin(server, op)),
            Command::LoadChannelOverview { .. }
            | Command::SaveChannelOverview { .. }
            | Command::DeleteChannel { .. } => out.extend(self.channel_command(command)),
            Command::LoadServerOverview { server, request } => {
                let result = self
                    .overview(&server.raw)
                    .ok_or_else(|| "This server is no longer available.".into());
                out.push(Event::ServerOverview {
                    server,
                    request,
                    result,
                });
            }
            Command::SaveServerOverview { server, request, edit } => {
                let mut body = serde_json::Map::new();
                if let Some(name) = &edit.name {
                    body.insert("name".into(), json!(name.trim()));
                }
                if let Some(d) = &edit.description {
                    body.insert("description".into(), json!(d));
                }
                if let Some(ch) = &edit.system_channels {
                    body.insert("system_channel_id".into(), json!(ch[0].as_deref()));
                }
                for (field, image) in [("icon", &edit.icon), ("banner", &edit.banner)] {
                    match image {
                        godwit_core::own_profile::ProfileImage::Keep => {}
                        godwit_core::own_profile::ProfileImage::Remove => {
                            body.insert(field.into(), Value::Null);
                        }
                        godwit_core::own_profile::ProfileImage::File { path, .. } => match data_uri(path) {
                            Ok(uri) => {
                                body.insert(field.into(), json!(uri));
                            }
                            Err(e) => {
                                out.push(Event::ServerOverview {
                                    server,
                                    request,
                                    result: Err(e.into()),
                                });
                                return out;
                            }
                        },
                    }
                }
                req(
                    &self.net,
                    Request::Admin {
                        guild: server.raw.to_string(),
                        op: Admin::ModifyGuild(Value::Object(body)),
                    },
                );
                // The change arrives as GUILD_UPDATE; the form shows it as saved.
                if let Some(mut overview) = self.overview(&server.raw) {
                    if let Some(name) = &edit.name {
                        overview.name = name.trim().into();
                    }
                    if let Some(d) = &edit.description {
                        overview.description = d.clone();
                    }
                    out.push(Event::ServerOverview {
                        server,
                        request,
                        result: Ok(overview),
                    });
                }
            }
            Command::JoinCall { room } => self.join_call(room, &mut out),
            Command::LeaveCall => self.leave_call(&mut out),
            Command::SetCallShown(shown) => {
                self.call_shown = shown;
                self.call_visibility();
            }
            Command::SetMuted(muted) => self.set_call_muted(Some(muted), None),
            Command::SetDeafened(deafened) => self.set_call_muted(None, Some(deafened)),
            Command::DeclineCall { room } => self.decline_call(room, &mut out),
            Command::SetCamera(on) => self.set_call_camera(on, &mut out),
            // Discord's free Go Live quality only (720p30): a higher choice
            // is never sent (no Nitro features).
            Command::SetScreenShare(on) => self.set_call_live(on, &mut out),
            Command::StartScreenShare { .. } => self.set_call_live(true, &mut out),
            _ => {}
        }
        out
    }

    fn message_link(&self, room: &RoomId, message: &MessageId) -> Option<String> {
        let c = self.store.channels.get(room.raw.as_ref())?;
        let guild = c.guild.as_deref().unwrap_or("@me");
        Some(format!("https://discord.com/channels/{guild}/{}/{}", c.id, message.0))
    }

    fn watch_stream(&mut self, room: &RoomId, user: &godwit_core::ids::UserId, watch: bool) -> Vec<Event> {
        let mut out = Vec::new();
        self.watch_call_stream(room, user, watch, &mut out);
        out
    }

    fn supports_invites(&self) -> bool {
        true
    }

    fn parse_invite(&self, input: &str) -> Option<String> {
        invite_code(input)
    }

    fn log_out(&mut self) {
        self.net.send(Cmd::Logout);
        if self.demo.is_none() {
            session::clear(self.slot);
        }
        self.status = Status::SignedOut { error: None };
    }
}

/// A picture as the data URI Discord takes for server icons and banners
/// (at most 10 MB, as its uploads allow).
fn data_uri(path: &std::path::Path) -> Result<String, String> {
    use base64::Engine;
    let bytes = std::fs::read(path).map_err(|e| format!("Couldn't read the picture: {e}"))?;
    if bytes.len() > 10 * 1024 * 1024 {
        return Err("The picture can be at most 10 MB.".into());
    }
    let mime = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => return Err("Use a PNG, JPEG, GIF or WebP picture.".into()),
    };
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// Discord's GIF list (search or trending): what's sent is the GIF's page
/// (`url`), as Discord's picker sends it; the grid shows `src` (Yapfast's `Gif::parse`).
fn gifs(v: &Value) -> Vec<godwit_core::gifs::Gif> {
    v.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|g| {
                    let preview = g["src"].as_str().filter(|s| s.starts_with("https://"))?;
                    Some(godwit_core::gifs::Gif {
                        url: g["url"].as_str()?.into(),
                        preview: preview.into(),
                        width: g["width"].as_u64().and_then(|w| u32::try_from(w).ok()).unwrap_or(0),
                        height: g["height"].as_u64().and_then(|h| u32::try_from(h).ok()).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polls_show_their_answers_and_votes() {
        let poll = serde_json::json!({
            "question": {"text": "Lunch?"},
            "answers": [
                {"answer_id": 1, "poll_media": {"text": "Pizza", "emoji": {"name": "🍕"}}},
                {"answer_id": 2, "poll_media": {"text": "Soup"}}
            ],
            "results": {"is_finalized": false, "answer_counts": [{"id": 1, "count": 2, "me_voted": true}]}
        });
        let text = poll_text(&poll, false).expect("a poll");
        assert!(text.contains("**Lunch\\?**"), "{text}");
        assert!(text.contains("• 🍕 Pizza — 2 votes ✓"));
        assert!(text.contains("• Soup — 0 votes"));
        assert!(poll_text(&serde_json::Value::Null, true).is_none());
        assert!(poll_text(&poll, true).expect("a poll").contains("](godwit://vote/2)"));
    }

    #[test]
    fn youtube_and_twitch_embeds_get_players() {
        let yt =
            serde_json::json!({"type": "video", "video": {"url": "https://www.youtube.com/embed/aB_123?start=90"}});
        assert_eq!(
            player(&yt).as_deref(),
            Some("https://www.youtube-nocookie.com/embed/aB_123?modestbranding=1&start=90")
        );
        let tw = serde_json::json!({"video": {"url": "https://player.twitch.tv/?channel=somebody&parent=meta.tag"}});
        assert!(player(&tw).is_some_and(|p| p.contains("channel=somebody")));
        let bad = serde_json::json!({"video": {"url": "https://www.youtube.com/embed/x\"onerror"}});
        assert_eq!(player(&bad), None);
    }

    #[test]
    fn upload_limits_follow_nitro_and_boosts() {
        const MB: u64 = 1024 * 1024;
        assert_eq!(upload_limit(0, None), 10 * MB);
        assert_eq!(upload_limit(3, None), 50 * MB);
        assert_eq!(upload_limit(2, Some(3)), 500 * MB);
        assert_eq!(upload_limit(0, Some(2)), 50 * MB);
    }

    #[test]
    fn stickers_show_large_and_embed_fields_follow_the_description() {
        let m = serde_json::json!({"sticker_items": [
            {"id": "1", "name": "Wave", "format_type": 1},
            {"id": "2", "name": "Dance", "format_type": 4},
            {"id": "3", "name": "Lottie", "format_type": 3}
        ]});
        let s = stickers(&m);
        assert_eq!(s.len(), 2, "Lottie stickers aren't pictures");
        assert!(s[1].0.ends_with("/stickers/2.gif?size=160"));
        let e = embed(&serde_json::json!({
            "type": "rich", "title": "Stats", "description": "Today",
            "fields": [{"name": "Wins", "value": "3"}], "footer": {"text": "bot"}
        }))
        .expect("an embed");
        assert_eq!(e.description.as_deref(), Some("Today\n\n**Wins**\n3\n\n*bot*"));
    }

    fn account() -> (
        DiscordAccount,
        tokio::sync::mpsc::UnboundedReceiver<Cmd>,
        mpsc::Sender<Ev>,
    ) {
        let (net, cmds) = Handle::detached();
        let (tx, events) = mpsc::channel();
        (
            DiscordAccount::with(u32::MAX, net, events, Status::Connecting),
            cmds,
            tx,
        )
    }

    fn ready(a: &mut DiscordAccount) -> Vec<Event> {
        let mut out = Vec::new();
        a.handle(Ev::Ready(Box::new(crate::store::test_ready())), &mut out);
        out
    }

    #[test]
    fn a_search_discord_is_still_indexing_says_so() {
        let (mut a, _cmds, _tx) = account();
        ready(&mut a);
        let channel = a
            .store
            .channels
            .values()
            .find(|c| c.private())
            .map(|c| c.id.clone())
            .expect("a DM");
        let mut out = Vec::new();
        a.handle(
            Ev::Answer(crate::backend::Answer::Search {
                channel,
                query: "lunch".into(),
                result: Ok(json!({"retry_after": 2, "documents_indexed": 0})),
            }),
            &mut out,
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Error(t) if t.contains("still preparing")))
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::SearchResults { messages: None, .. })),
            "failed, so searching again asks again"
        );
    }

    #[test]
    fn sessions_are_listed_and_one_signed_out_then_read_back() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let out = a.run(Command::LoadSessions);
        assert!(matches!(out.as_slice(), [Event::Sessions { view, .. }] if view.loading));
        assert!(matches!(cmds.try_recv(), Ok(Cmd::Request(Request::Sessions))));
        let mut out = Vec::new();
        a.handle(
            Ev::Answer(crate::backend::Answer::Sessions(Ok(json!({"user_sessions": [
                {"id_hash": "h1", "approx_last_used_time": "2026-11-04T10:00:00+00:00",
                 "client_info": {"os": "Linux", "platform": "Discord Client"}}
            ]})))),
            &mut out,
        );
        let Some(Event::Sessions { view, .. }) = out.last() else {
            panic!("{out:?}")
        };
        assert_eq!(view.sessions.len(), 1);
        assert!(!view.loading && view.error.is_none());
        a.run(Command::LogOutSession(view.sessions[0].id.clone()));
        assert!(matches!(cmds.try_recv(), Ok(Cmd::Request(Request::LogOutSession(id))) if id == "h1"));
        // A refusal shows as the page's error, not a silent empty list.
        let mut out = Vec::new();
        a.handle(
            Ev::Answer(crate::backend::Answer::Sessions(Err("nope".into()))),
            &mut out,
        );
        assert!(matches!(out.last(), Some(Event::Sessions { view, .. }) if view.error.as_deref() == Some("nope")));
    }

    #[test]
    fn privacy_switches_are_read_and_flipped_as_the_official_page() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let out = a.run(Command::LoadPrivacy);
        assert!(matches!(out.as_slice(), [Event::Privacy { view, .. }] if view.loading));
        assert!(matches!(cmds.try_recv(), Ok(Cmd::Request(Request::Consents))));
        let mut out = Vec::new();
        a.handle(
            Ev::Answer(crate::backend::Answer::Consents(Ok(json!({
                "usage_statistics": {"consented": true},
                "personalization": {"consented": false}
            })))),
            &mut out,
        );
        let Some(Event::Privacy { view, .. }) = out.last() else {
            panic!("{out:?}")
        };
        let on: Vec<(&str, bool)> = view.items.iter().map(|i| (&*i.key, i.on)).collect();
        assert_eq!(
            on,
            [
                ("usage_statistics", true),
                ("personalization", false),
                ("show_activity", true)
            ]
        );
        // A consent revoked by its own request; activity through the settings.
        a.run(Command::SetPrivacy {
            key: "usage_statistics".into(),
            on: false,
        });
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Request(Request::SetConsent { key, grant: false })) if key == "usage_statistics"
        ));
        a.run(Command::SetPrivacy {
            key: "show_activity".into(),
            on: false,
        });
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Request(Request::ShowActivity(false)))
        ));
        // Unknown keys send nothing.
        a.run(Command::SetPrivacy {
            key: "something_else".into(),
            on: true,
        });
        assert!(cmds.try_recv().is_err());
    }

    #[test]
    fn marking_read_acks_the_channels_newest_message() {
        // Discord keeps a deleted last message's id as `last_message_id`:
        // acking only what's shown would leave the chat unread for good.
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        let channel = a
            .store
            .channels
            .values()
            .find(|c| c.private())
            .map(|c| c.id.clone())
            .expect("a DM in the test READY");
        if let Some(c) = a.store.channels.get_mut(&channel) {
            c.last_message_id = Some("900000000000000000".into());
        }
        while cmds.try_recv().is_ok() {}
        let room = a.room_id(&channel).expect("its room");
        a.run(Command::MarkRead {
            room,
            last: godwit_core::ids::MessageId("100000000000000000".into()),
        });
        let mut acked = None;
        while let Ok(cmd) = cmds.try_recv() {
            if let Cmd::Request(Request::Ack { message, .. }) = cmd {
                acked = Some(message);
            }
        }
        assert_eq!(acked.as_deref(), Some("900000000000000000"));
        assert!(!a.store.unread(&channel));
    }

    #[test]
    fn ready_brings_rooms_servers_and_friends() {
        let (mut a, _cmds, _tx) = account();
        let out = ready(&mut a);
        let Some(Event::AccountReady {
            rooms,
            servers,
            display_name,
            ..
        }) = out.first()
        else {
            panic!("no AccountReady: {out:?}");
        };
        assert_eq!(&**display_name, "Bloop");
        let names: Vec<&str> = rooms.iter().map(|r| &*r.name).collect();
        assert!(names.contains(&"general") && names.contains(&"zarg"), "{names:?}");
        assert!(!names.contains(&"secret"), "hidden channels stay hidden");
        assert!(!names.contains(&"Talk"), "categories aren't rooms");
        assert_eq!(servers.len(), 1);
        assert_eq!(&*servers[0].categories[0].title, "Talk");
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Friends { people, .. } if people.len() == 1))
        );
        assert_eq!(a.status(), &Status::Ready);
    }

    #[test]
    fn blocking_sends_requests_never_for_ourselves_and_lists_the_blocked() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let zarg = a.user_id("200").unwrap();
        a.run(Command::BlockUser { user: zarg.clone() });
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Request(Request::BlockUser { user })) if user == "200"
        ));
        a.run(Command::UnblockUser { user: zarg.clone() });
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Request(Request::UnblockUser { user })) if user == "200"
        ));
        let me = a.user_id(a.me()).unwrap();
        a.run(Command::BlockUser { user: me });
        assert!(cmds.try_recv().is_err(), "no blocking ourselves");
        // The gateway's word is what the core hears.
        let mut out = Vec::new();
        a.dispatch("RELATIONSHIP_ADD", &json!({"id": "200", "type": 2}), &mut out);
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Blocked { users, .. } if users == std::slice::from_ref(&zarg)))
        );
        let mut out = Vec::new();
        a.dispatch("RELATIONSHIP_REMOVE", &json!({"id": "200"}), &mut out);
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Blocked { users, .. } if users.is_empty()))
        );
    }

    #[test]
    fn demo_blocks_and_unblocks_with_the_events_discord_would_send() {
        let (mut a, _) = DiscordAccount::demo(crate::store::test_ready(), HashMap::new());
        let zarg = a.user_id("200").unwrap();
        a.run(Command::LoadProfile {
            user: zarg.clone(),
            room: None,
        });
        let out = a.run(Command::BlockUser { user: zarg.clone() });
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Blocked { users, .. } if users.len() == 1))
        );
        let Some(Event::Profile(p)) = out.iter().find(|e| matches!(e, Event::Profile(_))) else {
            panic!("{out:?}")
        };
        assert_eq!(p.relationship, Relationship::Blocked);
        assert!(p.can_block);
        let out = a.run(Command::UnblockUser { user: zarg });
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Blocked { users, .. } if users.is_empty()))
        );
    }

    #[test]
    fn sending_confirms_by_nonce_and_refuses_where_not_allowed() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        // READY tells the gateway our presence first.
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Gateway {
                op: crate::gateway::PRESENCE_UPDATE,
                ..
            })
        ));
        let room = a.room_id("13").unwrap();
        a.run(Command::SendMessage {
            room: room.clone(),
            text: "hi".into(),
            nonce: "core-1".into(),
            reply_to: None,
            files: Vec::new(),
        });
        let Ok(Cmd::Request(Request::Send { nonce, .. })) = cmds.try_recv() else {
            panic!("no send")
        };
        let mut out = Vec::new();
        a.handle(
            Ev::Dispatch(
                "MESSAGE_CREATE".into(),
                json!({"id": "900", "channel_id": "13", "guild_id": "10", "nonce": nonce, "content": "hi", "author": {"id": "100", "username": "me_bloop"}, "type": 0}),
            ),
            &mut out,
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::MessageConfirmed { nonce, .. } if &**nonce == "core-1"))
        );
        let hidden = a.room_id("14").unwrap();
        let out = a.run(Command::SendMessage {
            room: hidden,
            text: "x".into(),
            nonce: "core-2".into(),
            reply_to: None,
            files: Vec::new(),
        });
        assert!(matches!(out.as_slice(), [Event::MessageFailed { .. }]));
        assert!(cmds.try_recv().is_err(), "nothing goes to Discord that it would refuse");
    }

    #[test]
    fn embeds_arriving_alone_reach_the_core() {
        let (mut a, _cmds, _tx) = account();
        ready(&mut a);
        let mut out = Vec::new();
        // A link's preview comes after sending, without the message's content.
        let gif = json!({"type": "gifv", "url": "https://example.com/cat", "thumbnail": {"url": "https://example.com/cat.png", "width": 10, "height": 10}, "video": {"proxy_url": "https://example.com/cat.mp4"}});
        a.handle(
            Ev::Dispatch(
                "MESSAGE_UPDATE".into(),
                json!({"id": "901", "channel_id": "13", "guild_id": "10", "embeds": [gif]}),
            ),
            &mut out,
        );
        assert!(
            matches!(out.as_slice(), [Event::EmbedsUpdated { id, embeds, .. }] if &*id.0 == "901" && embeds.len() == 1),
            "{out:?}"
        );
    }

    #[test]
    fn a_channels_own_level_wins_and_threads_follow_their_channel() {
        let (mut a, _cmds, _tx) = account();
        ready(&mut a);
        let g = a.store.guilds.keys().next().cloned().expect("a guild");
        a.store.guild_notify.insert(g, 2);
        a.store.channel_notify.insert("13".into(), 0);
        let mut out = Vec::new();
        a.handle(
            Ev::Dispatch(
                "THREAD_CREATE".into(),
                json!({"id": "600", "type": 11, "guild_id": "10", "parent_id": "13", "name": "T"}),
            ),
            &mut out,
        );
        assert_eq!(a.store.channel_notify_level("13"), 0, "the channel's own");
        assert_eq!(a.store.channel_notify_level("600"), 0, "a thread follows its channel");
        a.store.channel_notify.insert("13".into(), 3);
        assert_eq!(a.store.channel_notify_level("13"), 2, "3 means the server's");
    }

    #[test]
    fn server_notification_levels_follow_our_choice_then_the_servers_default() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let g = a.store.guilds.keys().next().cloned().expect("a guild");
        a.store.guild_notify.insert(g.clone(), 3);
        if let Some(guild) = a.store.guilds.get_mut(&g) {
            guild.default_notify = 0;
        }
        assert_eq!(a.store.notify_level(&g), 0, "3 means the server's default");
        let server = ServerId::new(a.account.clone().expect("signed in"), g.as_str());
        let out = a.run(Command::ServerAdmin {
            server,
            op: ServerAdmin::SetNotify(2),
        });
        assert_eq!(a.store.notify_level(&g), 2);
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Request(Request::GuildNotify { level: 2, .. }))
        ));
        assert!(out.iter().any(
            |e| matches!(e, Event::ServersChanged { servers, .. } if servers.iter().any(|s| s.rights.notify == 2))
        ));
    }

    #[test]
    fn threads_are_listed_under_their_channel_and_archived_ones_are_not() {
        let (mut a, _cmds, _tx) = account();
        ready(&mut a);
        let mut out = Vec::new();
        a.handle(
            Ev::Dispatch(
                "THREAD_CREATE".into(),
                json!({"id": "500", "type": 11, "guild_id": "10", "parent_id": "13", "name": "Plans", "thread_metadata": {"archived": false}}),
            ),
            &mut out,
        );
        a.handle(
            Ev::Dispatch(
                "THREAD_CREATE".into(),
                json!({"id": "501", "type": 11, "guild_id": "10", "parent_id": "13", "name": "Old", "thread_metadata": {"archived": true}}),
            ),
            &mut out,
        );
        let servers = a.build_servers();
        let rooms: Vec<&str> = servers[0]
            .categories
            .iter()
            .flat_map(|c| c.rooms.iter())
            .map(|r| &*r.raw)
            .collect();
        let at = rooms.iter().position(|r| *r == "13").expect("the channel");
        assert_eq!(rooms.get(at + 1), Some(&"500"), "the thread right under it: {rooms:?}");
        assert!(!rooms.contains(&"501"), "archived threads aren't listed");
        let thread = a.room("500").expect("a room");
        assert_eq!(thread.parent.as_deref(), Some("13"));
    }

    #[test]
    fn voting_moves_our_vote_and_takes_it_back() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let poll = json!({
            "id": "950", "channel_id": "13", "guild_id": "10", "type": 0, "content": "",
            "author": {"id": "200", "username": "zarg"},
            "poll": {
                "question": {"text": "Lunch"}, "allow_multiselect": false,
                "answers": [{"answer_id": 1, "poll_media": {"text": "Pizza"}}, {"answer_id": 2, "poll_media": {"text": "Soup"}}],
                "results": {"is_finalized": false, "answer_counts": [{"id": 1, "count": 1, "me_voted": true}]}
            }
        });
        let shown = a.message(&poll).expect("a message");
        assert!(shown.text.contains("godwit://vote/2"), "{}", shown.text);
        let room = a.room_id("13").unwrap();
        let out = a.run(Command::VotePoll {
            room: room.clone(),
            message: MessageId("950".into()),
            answer: 2,
        });
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Request(Request::Vote { answers, .. })) if answers == vec![2]
        ));
        let Some(Event::MessageUpdated { message, .. }) = out.first() else {
            panic!("the poll shows again: {out:?}")
        };
        assert!(
            message.text.contains("Soup") && message.text.contains("1 vote ✓"),
            "{}",
            message.text
        );
        assert!(
            message.text.contains("Pizza") && message.text.contains("0 votes"),
            "{}",
            message.text
        );
        a.run(Command::VotePoll {
            room,
            message: MessageId("950".into()),
            answer: 2,
        });
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Request(Request::Vote { answers, .. })) if answers.is_empty()
        ));
    }

    #[test]
    fn messages_convert_with_mentions_replies_and_reactions() {
        let (mut a, _cmds, _tx) = account();
        ready(&mut a);
        let m = a
            .message(&json!({
                "id": "901", "channel_id": "13", "guild_id": "10", "type": 19,
                "content": "hey <@200> **yo**", "author": {"id": "200", "username": "zarg"},
                "timestamp": "2026-11-02T10:00:00.000000+00:00", "edited_timestamp": null,
                "message_reference": {"message_id": "900"},
                "attachments": [{"filename": "a.png", "url": "https://cdn.discordapp.com/x/a.png", "content_type": "image/png", "size": 10, "width": 4, "height": 3}],
                "reactions": [{"emoji": {"id": null, "name": "👍"}, "count": 2, "me": true}, {"emoji": {"id": "77", "name": "blob"}, "count": 1, "me": false}]
            }))
            .unwrap();
        assert!(m.text.contains("@zarg"), "{}", m.text);
        assert_eq!(m.reply_to, Some(MessageId("900".into())));
        assert_eq!(m.attachments[0].kind, AttachmentKind::Image);
        assert_eq!(m.attachments[0].dimensions, Some((4, 3)));
        assert_eq!(&*m.reactions[1].emoji, "blob:77");
        assert!(m.reactions[0].me);
        assert!(!m.system && !m.edited);
        let join = a.message(&json!({"id": "902", "channel_id": "13", "type": 7, "content": "", "author": {"id": "200", "username": "zarg"}})).unwrap();
        assert!(join.system && join.text.contains("joined"));
    }

    #[test]
    fn invite_links_are_recognised() {
        assert_eq!(invite_code("https://discord.gg/abcDEF"), Some("abcDEF".into()));
        assert_eq!(invite_code("discord.com/invite/xyz-1?event=2"), Some("xyz-1".into()));
        assert_eq!(invite_code("https://evil.example/discord.gg/x"), None);
        assert_eq!(invite_codes("join https://discord.gg/a and discord.gg/a").len(), 1);
    }

    #[test]
    fn signing_in_shows_the_code_then_connects() {
        let (net, mut cmds) = Handle::detached();
        let (_tx, events) = mpsc::channel();
        let mut a = DiscordAccount::with(u32::MAX, net, events, Status::SignedOut { error: None });
        a.link();
        assert!(matches!(cmds.try_recv(), Ok(Cmd::Link)));
        let mut out = Vec::new();
        a.handle(Ev::LinkStep(Step::Code("https://discord.com/ra/abc".into())), &mut out);
        assert_eq!(
            a.status(),
            &Status::Qr {
                url: Some("https://discord.com/ra/abc".into()),
                scanned: None,
                captcha: None,
            }
        );
        a.handle(Ev::LinkStep(Step::Scanned { name: "zib".into() }), &mut out);
        assert!(matches!(a.status(), Status::Qr { scanned: Some(n), .. } if n == "zib"));
        a.handle(Ev::LinkFailed("The code expired.".into()), &mut out);
        assert!(
            matches!(a.status(), Status::SignedOut { error: Some(_) }),
            "a scanned code that expires stops"
        );
        a.link();
        let _ = cmds.try_recv();
        for _ in 0..5 {
            a.handle(Ev::LinkFailed("The code expired.".into()), &mut out);
            assert!(
                matches!(cmds.try_recv(), Ok(Cmd::Link)),
                "a new code, as the web login shows"
            );
        }
        a.handle(Ev::LinkFailed("The code expired.".into()), &mut out);
        assert!(matches!(a.status(), Status::SignedOut { .. }), "but not forever");
    }

    #[test]
    fn server_admin_checks_permissions_first() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        // READY tells the gateway our presence first.
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Gateway {
                op: crate::gateway::PRESENCE_UPDATE,
                ..
            })
        ));
        let server = ServerId::new(a.account.clone().unwrap(), "10");
        // Our role has Kick Members only.
        let out = a.run(Command::ServerAdmin {
            server: server.clone(),
            op: ServerAdmin::LoadBans,
        });
        assert!(matches!(out.as_slice(), [Event::ServerAdmin { result: Err(_), .. }]));
        assert!(cmds.try_recv().is_err());
        let out = a.run(Command::ServerAdmin {
            server: server.clone(),
            op: ServerAdmin::Kick(a.user_id("300").unwrap()),
        });
        assert!(out.is_empty());
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Request(Request::Admin {
                op: Admin::Kick { .. },
                ..
            }))
        ));
        let out = a.run(Command::ServerAdmin {
            server,
            op: ServerAdmin::LoadRoles,
        });
        assert!(
            matches!(out.as_slice(), [Event::ServerAdmin { result: Ok(ServerAdminResult::Roles(r)), .. }] if r.roles.len() == 1)
        );
    }
}

#[cfg(test)]
mod member_admin_tests {
    use super::*;
    use godwit_core::server_settings::ServerAdmin;
    use std::time::Duration;

    /// Qux Hall with us (100) in "Mods" (position 3) holding Manage Roles,
    /// Moderate Members and the nickname permissions; the owner is 200 and
    /// 300 is a member with the lower role "20".
    fn staffed() -> (DiscordAccount, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
        let (net, mut cmds) = Handle::detached();
        let (_tx, events) = mpsc::channel();
        let mut a = DiscordAccount::with(u32::MAX, net, events, Status::Connecting);
        let mut ready = crate::store::test_ready();
        let mods = p::MANAGE_ROLES | p::MODERATE_MEMBERS | p::MANAGE_NICKNAMES | p::CHANGE_NICKNAME;
        let roles = ready["guilds"][0]["roles"].as_array_mut().unwrap();
        roles[1]["permissions"] = json!(mods.to_string());
        roles.push(json!({"id": "20", "name": "Helpers", "permissions": "0", "position": 1, "color": 0xff0000}));
        roles.push(json!({"id": "21", "name": "Bosses", "permissions": "0", "position": 5}));
        roles.push(json!({"id": "22", "name": "Bot", "permissions": "0", "position": 2, "managed": true}));
        a.handle(Ev::Ready(Box::new(ready)), &mut Vec::new());
        let mut out = Vec::new();
        let member = json!({"guild_id": "10", "user": {"id": "300", "username": "kim"}, "roles": ["20"]});
        a.dispatch("GUILD_MEMBER_ADD", &member, &mut out);
        while cmds.try_recv().is_ok() {}
        (a, cmds)
    }

    fn send(
        a: &mut DiscordAccount,
        cmds: &mut tokio::sync::mpsc::UnboundedReceiver<Cmd>,
        op: ServerAdmin,
    ) -> Option<Admin> {
        let server = ServerId::new(a.account.clone().unwrap(), "10");
        a.run(Command::ServerAdmin { server, op });
        match cmds.try_recv() {
            Ok(Cmd::Request(Request::Admin { op, .. })) => Some(op),
            _ => None,
        }
    }

    #[test]
    fn the_card_offers_only_what_hierarchy_and_permissions_allow() {
        let (a, _cmds) = staffed();
        let m = a.moderation("10", "300").unwrap();
        // Not @everyone, not the managed role, nothing at or above our role.
        let ids: Vec<&str> = m.roles.iter().map(|r| &*r.id).collect();
        assert_eq!(ids, ["20"]);
        assert!(m.roles[0].assigned && m.roles[0].colour == Some([0xff, 0, 0]));
        assert!(m.timeout && !m.timed_out && m.nickname);
        assert!(a.moderation("10", "200").is_none(), "the owner is out of reach");
        let own = a.moderation("10", "100").unwrap();
        assert!(own.nickname && own.roles.is_empty() && !own.timeout && !own.kick);
        assert_eq!(own.nick.as_deref(), Some("Bloopy"));
    }

    #[test]
    fn the_owner_may_hand_out_any_role_and_admins_cannot_be_timed_out() {
        let (mut a, _cmds) = staffed();
        a.store.guilds.get_mut("10").unwrap().owner = "100".into();
        let ids = |a: &DiscordAccount| -> Vec<String> {
            a.moderation("10", "300")
                .unwrap()
                .roles
                .iter()
                .map(|r| r.id.to_string())
                .collect()
        };
        // Highest first; the managed "Bot" role and @everyone stay out.
        assert_eq!(ids(&a), ["21", "11", "20"]);
        a.store.guilds.get_mut("10").unwrap().owner = "200".into();
        a.store
            .guilds
            .get_mut("10")
            .unwrap()
            .roles
            .get_mut("20")
            .unwrap()
            .permissions = p::ADMINISTRATOR;
        assert!(
            !a.moderation("10", "300").unwrap().timeout,
            "Discord refuses to time admins out"
        );
    }

    #[test]
    fn role_timeout_and_nickname_requests_have_the_documented_shape() {
        let (mut a, mut cmds) = staffed();
        let user = a.user_id("300").unwrap();
        let op = send(
            &mut a,
            &mut cmds,
            ServerAdmin::SetMemberRole {
                user: user.clone(),
                role: "20".into(),
                on: false,
            },
        );
        assert!(matches!(op, Some(Admin::MemberRole { user, role, on: false }) if user == "300" && role == "20"));
        // Above our role: refused with no request.
        let refused = send(
            &mut a,
            &mut cmds,
            ServerAdmin::SetMemberRole {
                user: user.clone(),
                role: "21".into(),
                on: true,
            },
        );
        assert!(refused.is_none());
        let Some(Admin::ModifyMember { user: target, body }) = send(
            &mut a,
            &mut cmds,
            ServerAdmin::Timeout {
                user: user.clone(),
                duration: Some(Duration::from_secs(600)),
            },
        ) else {
            panic!("no timeout request")
        };
        assert_eq!(target, "300");
        let until =
            chrono::DateTime::parse_from_rfc3339(body["communication_disabled_until"].as_str().unwrap()).unwrap();
        let ahead = until.timestamp() - chrono::Utc::now().timestamp();
        assert!((595..=600).contains(&ahead), "{ahead}");
        let Some(Admin::ModifyMember { body, .. }) = send(
            &mut a,
            &mut cmds,
            ServerAdmin::Timeout {
                user: user.clone(),
                duration: None,
            },
        ) else {
            panic!("no lift request")
        };
        assert!(body["communication_disabled_until"].is_null());
        let Some(Admin::ModifyMember { user: target, body }) = send(
            &mut a,
            &mut cmds,
            ServerAdmin::SetServerNickname {
                user: user.clone(),
                nick: Some("Kimmy".into()),
            },
        ) else {
            panic!("no nickname request")
        };
        assert_eq!((target.as_str(), body["nick"].as_str()), ("300", Some("Kimmy")));
        // Ours goes to @me; empty resets with null.
        let me = a.user_id("100").unwrap();
        let Some(Admin::ModifyMember { user: target, body }) = send(
            &mut a,
            &mut cmds,
            ServerAdmin::SetServerNickname {
                user: me,
                nick: Some("  ".into()),
            },
        ) else {
            panic!("no own nickname request")
        };
        assert_eq!(target, "@me");
        assert!(body["nick"].is_null());
        // The owner and anyone ranked at or above us are refused.
        let owner = a.user_id("200").unwrap();
        let refused = send(
            &mut a,
            &mut cmds,
            ServerAdmin::Timeout {
                user: owner,
                duration: Some(Duration::from_secs(60)),
            },
        );
        assert!(refused.is_none());
    }

    #[test]
    fn member_updates_show_the_timeout_and_the_new_roles() {
        let (mut a, _cmds) = staffed();
        let until = (chrono::Utc::now() + Duration::from_secs(3_600)).to_rfc3339();
        let d = json!({"guild_id": "10", "user": {"id": "300", "username": "kim"}, "roles": [], "nick": "Kimmy", "communication_disabled_until": until});
        a.dispatch("GUILD_MEMBER_UPDATE", &d, &mut Vec::new());
        let m = a.moderation("10", "300").unwrap();
        assert!(m.timed_out && m.nick.as_deref() == Some("Kimmy") && !m.roles[0].assigned);
        let past = (chrono::Utc::now() - Duration::from_secs(60)).to_rfc3339();
        let d = json!({"guild_id": "10", "user": {"id": "300", "username": "kim"}, "roles": [], "communication_disabled_until": past});
        a.dispatch("GUILD_MEMBER_UPDATE", &d, &mut Vec::new());
        assert!(
            !a.moderation("10", "300").unwrap().timed_out,
            "an ended timeout is none"
        );
    }

    #[test]
    fn ban_sends_the_chosen_history_window() {
        let (mut a, mut cmds) = staffed();
        a.store
            .guilds
            .get_mut("10")
            .unwrap()
            .roles
            .get_mut("11")
            .unwrap()
            .permissions |= p::BAN_MEMBERS;
        let user = a.user_id("300").unwrap();
        let op = send(
            &mut a,
            &mut cmds,
            ServerAdmin::Ban {
                user,
                reason: "spam".into(),
                delete_seconds: 86_400,
            },
        );
        assert!(matches!(
            op,
            Some(Admin::Ban {
                delete_seconds: 86_400,
                ..
            })
        ));
    }

    #[test]
    fn demo_member_changes_come_back_as_gateway_updates() {
        let mut ready = crate::store::test_ready();
        let mods = p::MANAGE_ROLES | p::MODERATE_MEMBERS | p::MANAGE_NICKNAMES;
        let roles = ready["guilds"][0]["roles"].as_array_mut().unwrap();
        roles[1]["permissions"] = json!(mods.to_string());
        roles.push(json!({"id": "20", "name": "Helpers", "permissions": "0", "position": 1}));
        let (mut a, _) = DiscordAccount::demo(ready, HashMap::new());
        a.dispatch(
            "GUILD_MEMBER_ADD",
            &json!({"guild_id": "10", "user": {"id": "300", "username": "kim"}, "roles": []}),
            &mut Vec::new(),
        );
        let user = a.user_id("300").unwrap();
        let server = ServerId::new(a.account.clone().unwrap(), "10");
        a.run(Command::LoadProfile {
            user: user.clone(),
            room: None,
        });
        a.profile_for = Some(("300".into(), Some("10".into())));
        let mut change = |op| {
            a.run(Command::ServerAdmin {
                server: server.clone(),
                op,
            })
        };
        let out = change(ServerAdmin::SetMemberRole {
            user: user.clone(),
            role: "20".into(),
            on: true,
        });
        let Some(Event::Profile(p)) = out.iter().find(|e| matches!(e, Event::Profile(_))) else {
            panic!("{out:?}")
        };
        assert!(p.moderation.as_ref().unwrap().roles[0].assigned);
        let out = change(ServerAdmin::Timeout {
            user: user.clone(),
            duration: Some(Duration::from_secs(60)),
        });
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Profile(p) if p.moderation.as_ref().is_some_and(|m| m.timed_out)))
        );
        let out = change(ServerAdmin::Timeout {
            user: user.clone(),
            duration: None,
        });
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Profile(p) if p.moderation.as_ref().is_some_and(|m| !m.timed_out)))
        );
        let out = change(ServerAdmin::SetServerNickname {
            user,
            nick: Some("Kimmy".into()),
        });
        assert!(out.iter().any(|e| matches!(e, Event::Profile(p) if p.moderation.as_ref().is_some_and(|m| m.nick.as_deref() == Some("Kimmy")))));
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::ServerAdmin { result: Ok(_), .. }))
        );
    }
}

#[cfg(test)]
mod reaction_tests {
    use super::*;

    #[test]
    fn live_reactions_change_the_shown_list() {
        let (net, _cmds) = Handle::detached();
        let (_tx, events) = mpsc::channel();
        let mut a = DiscordAccount::with(u32::MAX, net, events, Status::Connecting);
        let mut out = Vec::new();
        a.handle(Ev::Ready(Box::new(crate::store::test_ready())), &mut out);
        out.clear();
        let add = |user: &str| json!({"channel_id": "13", "message_id": "900", "user_id": user, "emoji": {"id": null, "name": "🔥"}});
        a.dispatch("MESSAGE_REACTION_ADD", &add("200"), &mut out);
        a.dispatch("MESSAGE_REACTION_ADD", &add("100"), &mut out);
        let Some(Event::ReactionsChanged { reactions, .. }) = out.last() else {
            panic!("{out:?}")
        };
        assert_eq!((reactions[0].count, reactions[0].me), (2, true));
        a.dispatch("MESSAGE_REACTION_REMOVE", &add("100"), &mut out);
        a.dispatch("MESSAGE_REACTION_REMOVE", &add("200"), &mut out);
        let Some(Event::ReactionsChanged { reactions, .. }) = out.last() else {
            panic!()
        };
        assert!(reactions.is_empty());
    }
}

#[cfg(test)]
mod member_list_tests {
    use super::*;

    #[test]
    fn the_streamed_member_list_syncs_and_changes() {
        let (net, mut cmds) = Handle::detached();
        let (_tx, events) = mpsc::channel();
        let mut a = DiscordAccount::with(u32::MAX, net, events, Status::Connecting);
        let mut out = Vec::new();
        a.handle(Ev::Ready(Box::new(crate::store::test_ready())), &mut out);
        while cmds.try_recv().is_ok() {}
        let room = a.room_id("13").unwrap();
        a.run(Command::Focus {
            room: Some(room.clone()),
        });
        let data = std::iter::from_fn(|| cmds.try_recv().ok())
            .find_map(|cmd| match cmd {
                Cmd::Gateway { op, data } if op == crate::gateway::GUILD_SUBSCRIPTIONS_BULK => Some(data),
                _ => None,
            })
            .expect("member-list subscription");
        assert_eq!(
            data["subscriptions"]["10"]["channels"]["13"],
            json!([[0, 99], [100, 199]])
        );
        a.run(Command::Focus {
            room: Some(room.clone()),
        });
        assert!(cmds.try_recv().is_err(), "the same channel isn't subscribed twice");
        let member = |id: &str, status: &str| json!({"member": {"user": {"id": id, "username": format!("u{id}")}, "roles": [], "presence": {"status": status}}});
        let mut out = Vec::new();
        a.dispatch(
            "GUILD_MEMBER_LIST_UPDATE",
            &json!({"guild_id": "10", "ops": [
                {"op": "SYNC", "range": [0, 99], "items": [
                    {"group": {"id": "online", "count": 2}}, member("300", "online"), member("301", "idle"),
                    {"group": {"id": "offline", "count": 1}}, member("302", "offline")
                ]}
            ]}),
            &mut out,
        );
        let Some(Event::Members { groups, .. }) = out.last() else {
            panic!("{out:?}")
        };
        assert_eq!(groups.len(), 2);
        assert_eq!(&*groups[0].title, "Online — 2");
        assert_eq!(groups[0].members.len(), 2);
        assert_eq!(groups[0].members[1].presence, Presence::Idle);
        a.dispatch(
            "GUILD_MEMBER_LIST_UPDATE",
            &json!({"guild_id": "10", "ops": [{"op": "DELETE", "index": 2}]}),
            &mut out,
        );
        let Some(Event::Members { groups, .. }) = out.last() else {
            panic!()
        };
        assert_eq!(groups[0].members.len(), 1);
        // Another guild opened earlier keeps streaming its own list: that
        // mustn't touch this one's (it emptied the open list before).
        a.list_channel.insert("77".into(), "770".into());
        a.dispatch(
            "GUILD_MEMBER_LIST_UPDATE",
            &json!({"guild_id": "77", "ops": [{"op": "UPDATE", "index": 5, "item": {}}]}),
            &mut out,
        );
        let shown = a.run(Command::LoadMembers { room });
        assert!(matches!(shown.as_slice(), [Event::Members { groups, .. }] if groups.len() == 2));
        let [Event::Members { groups, .. }] = shown.as_slice() else {
            panic!()
        };
        assert_eq!(groups[0].members.len(), 1, "the open list kept its rows");
    }
}

#[cfg(test)]
mod sign_out_tests {
    use super::*;

    #[test]
    fn signing_out_loses_the_account_quietly() {
        let (net, _cmds) = Handle::detached();
        let (_tx, events) = mpsc::channel();
        let mut a = DiscordAccount::with(u32::MAX, net, events, Status::Connecting);
        let mut out = Vec::new();
        a.handle(Ev::Ready(Box::new(crate::store::test_ready())), &mut out);
        out.clear();
        a.handle(
            Ev::Lost {
                reason: String::new(),
                signed_out: true,
            },
            &mut out,
        );
        assert!(matches!(out.as_slice(), [Event::AccountLost { reason, .. }] if reason.is_empty()));
        assert_eq!(a.status(), &Status::SignedOut { error: None });
        assert!(a.id().is_none());
    }
}

/// Discord's limits for what Settings → Profile edits (the official client's).
const MAX_DISPLAY_NAME: usize = 32;
const MAX_PRONOUNS: usize = 40;
const MAX_BIO: usize = 190;

/// Our profile, as Settings shows it, from `GET /users/{me}/profile`.
fn own_profile_of(v: &Value, store: &store::Store, avatar: String) -> godwit_core::own_profile::OwnProfile {
    let user = &v["user"];
    let profile = &v["user_profile"];
    let display_name = user["global_name"].as_str().unwrap_or_default();
    let name = if display_name.is_empty() {
        store.me.as_ref().map(|m| m.username.clone()).unwrap_or_default()
    } else {
        display_name.to_owned()
    };
    godwit_core::own_profile::OwnProfile {
        name: name.into(),
        display_name: display_name.into(),
        pronouns: profile["pronouns"].as_str().unwrap_or_default().into(),
        bio: profile["bio"].as_str().unwrap_or_default().into(),
        avatar_url: Some(avatar.into()),
        // Profile banners are Nitro: never offered (no Nitro features).
        banner_url: None,
    }
}

/// A profile edit as Discord's requests, within its limits. Banners and
/// animated avatars are Nitro features, so they're refused here.
fn own_profile_request(edit: godwit_core::own_profile::ProfileEdit) -> Result<Request, String> {
    use godwit_core::own_profile::ProfileImage;
    let too_long = |text: &str, most: usize, what: &str| -> Result<(), String> {
        if text.chars().count() > most {
            Err(format!("The {what} can be at most {most} characters on Discord."))
        } else {
            Ok(())
        }
    };
    if edit.banner != ProfileImage::Keep {
        return Err("Profile banners are a Nitro feature on Discord.".into());
    }
    let mut me = serde_json::Map::new();
    if let Some(name) = &edit.display_name {
        let name = name.trim();
        too_long(name, MAX_DISPLAY_NAME, "display name")?;
        me.insert(
            "global_name".into(),
            if name.is_empty() { Value::Null } else { name.into() },
        );
    }
    let avatar = match edit.avatar {
        ProfileImage::Keep => None,
        ProfileImage::Remove => {
            me.insert("avatar".into(), Value::Null);
            None
        }
        ProfileImage::File { path, .. } => Some(path),
    };
    let mut profile = serde_json::Map::new();
    if let Some(pronouns) = &edit.pronouns {
        too_long(pronouns, MAX_PRONOUNS, "pronouns")?;
        profile.insert("pronouns".into(), pronouns.as_ref().into());
    }
    if let Some(bio) = &edit.bio {
        too_long(bio, MAX_BIO, "bio")?;
        profile.insert("bio".into(), bio.as_ref().into());
    }
    Ok(Request::SaveProfile {
        me: (!me.is_empty()).then_some(Value::Object(me)),
        avatar,
        profile: (!profile.is_empty()).then_some(Value::Object(profile)),
    })
}

#[cfg(test)]
mod own_profile_tests {
    use super::*;
    use godwit_core::own_profile::{ProfileEdit, ProfileImage};

    #[test]
    fn profile_edits_follow_discords_limits_and_never_nitro() {
        let banner = ProfileEdit {
            banner: ProfileImage::Remove,
            ..ProfileEdit::default()
        };
        assert!(own_profile_request(banner).unwrap_err().contains("Nitro"));
        let long = ProfileEdit {
            bio: Some("x".repeat(191).into()),
            ..ProfileEdit::default()
        };
        assert!(own_profile_request(long).is_err());
        let edit = ProfileEdit {
            display_name: Some("  ".into()),
            pronouns: Some("they/them".into()),
            avatar: ProfileImage::Remove,
            ..ProfileEdit::default()
        };
        let Ok(Request::SaveProfile { me, avatar, profile }) = own_profile_request(edit) else {
            panic!("a request")
        };
        let me = me.unwrap();
        assert!(me["global_name"].is_null(), "empty clears it");
        assert!(me["avatar"].is_null());
        assert!(avatar.is_none());
        assert_eq!(profile.unwrap()["pronouns"], "they/them");
    }
}

/// The data-use consents Settings → Privacy shows, as the official client's
/// Data & Privacy page words them.
const CONSENTS: [(&str, &str, &str); 2] = [
    (
        "usage_statistics",
        "Use data to improve Discord",
        "Lets Discord use how you use it to improve the service.",
    ),
    (
        "personalization",
        "Use data to personalize my Discord experience",
        "Lets Discord use what you do to tailor what it shows you.",
    ),
];
/// The activity-sharing switch's key.
const SHOW_ACTIVITY: &str = "show_activity";

impl DiscordAccount {
    /// Settings → Privacy: the consents (once Discord said them) and
    /// activity sharing.
    fn send_privacy(&self, loading: bool, error: Option<String>, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else { return };
        let mut items: Vec<godwit_core::privacy::PrivacyItem> = self
            .consents
            .iter()
            .flatten()
            .filter_map(|(key, on)| {
                let (_, label, detail) = CONSENTS.iter().find(|(k, _, _)| k == key)?;
                Some(godwit_core::privacy::PrivacyItem {
                    key: key.as_str().into(),
                    label: (*label).into(),
                    detail: (*detail).into(),
                    on: *on,
                })
            })
            .collect();
        items.push(godwit_core::privacy::PrivacyItem {
            key: SHOW_ACTIVITY.into(),
            label: "Share my activity".into(),
            detail: "Shows what you're playing in your status.".into(),
            // Discord's default is on.
            on: self.store.show_game.unwrap_or(true),
        });
        out.push(Event::Privacy {
            account,
            view: godwit_core::privacy::PrivacyView {
                items,
                loading: loading && self.consents.is_none(),
                error: error.map(Into::into),
            },
        });
    }
}

/// The signed-in devices from `GET /auth/sessions`, newest use first.
fn sessions_of(v: &Value) -> Vec<godwit_core::privacy::Session> {
    let mut sessions: Vec<godwit_core::privacy::Session> = v["user_sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let id = s["id_hash"].as_str()?;
            let info = &s["client_info"];
            let part = |k: &str| info[k].as_str().filter(|t| !t.is_empty());
            let device = match (part("os"), part("platform")) {
                (Some(os), Some(platform)) => format!("{os} · {platform}"),
                (Some(one), None) | (None, Some(one)) => one.to_owned(),
                (None, None) => "Unknown device".to_owned(),
            };
            Some(godwit_core::privacy::Session {
                id: id.into(),
                device: device.into(),
                location: part("location").unwrap_or_default().into(),
                created_ms: None,
                last_used_ms: s["approx_last_used_time"]
                    .as_str()
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .map(|t| t.timestamp_millis()),
            })
        })
        .collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.last_used_ms));
    sessions
}

#[cfg(test)]
mod sessions_tests {
    use serde_json::json;

    #[test]
    fn devices_read_as_the_official_page_lists_them() {
        let v = json!({"user_sessions": [
            {"id_hash": "a", "approx_last_used_time": "2026-11-01T10:00:00+00:00",
             "client_info": {"os": "Windows", "platform": "Discord Client", "location": "Amsterdam, NL"}},
            {"id_hash": "b", "approx_last_used_time": "2026-11-04T10:00:00+00:00",
             "client_info": {"os": "Android", "platform": ""}},
            {"client_info": {}}
        ]});
        let s = super::sessions_of(&v);
        assert_eq!(s.len(), 2, "one without an id is left out");
        assert_eq!(&*s[0].id, "b", "most recently used first");
        assert_eq!(&*s[0].device, "Android");
        assert_eq!(&*s[1].device, "Windows · Discord Client");
        assert_eq!(&*s[1].location, "Amsterdam, NL");
    }
}
