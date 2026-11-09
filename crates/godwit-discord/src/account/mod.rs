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
mod notify;
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
use godwit_core::members::{
    Member, MemberGroup, Moderation, ModerationRole, MutualFriend, MutualServer, Presence, Profile, Relationship,
};
use godwit_core::message::{Attachment, AttachmentKind, Embed, EmbedStyle, InvitePreview, Markup, Message, Reaction};
use godwit_core::network::Network;
use godwit_core::room::{Capabilities, Room, RoomEncryption, RoomKind};
use godwit_core::server::{Category, Server as CoreServer, ServerRights};
use godwit_core::server_settings::{
    NewChannelKind, ServerAdmin, ServerAdminResult, ServerBan, ServerInvite, ServerOverview, ServerRole, ServerRoles,
};
use serde_json::{Value, json};

use crate::backend::{self, Admin, AdminAnswer, Answer, Cmd, Ev, Handle, Request, Secret, Waker};
use crate::permissions as p;
use crate::remote_auth::Step;
use crate::session;
use crate::store::{self, Store};
use crate::voice::CallCmd;
use crate::voice::states::VoiceStates;
use godwit_voice::call::VoiceSettings;

mod calls;
mod forum;

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
    NeedsCode {
        ticket: LoginTicket,
        totp: bool,
        sms: bool,
        backup: bool,
        webauthn: bool,
        hint: String,
        error: Option<String>,
    },
    LoggingIn,
    Connecting,
    Ready,
}

/// A login ticket is deliberately omitted from diagnostics because it can
/// authorize completion of the pending sign-in.
pub struct LoginTicket(String);
impl std::fmt::Debug for LoginTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LoginTicket([redacted])")
    }
}
impl Clone for LoginTicket {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl PartialEq for LoginTicket {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl Eq for LoginTicket {}

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
    /// The channel whose settings page was loaded last: only its overrides
    /// are rebuilt when they change elsewhere (there is no close command).
    settings_channel: Option<String>,
    store: Store,
    /// Messages with a poll, as Discord last sent them (id → message), so a
    /// vote's counts can change without fetching it again. Filled as
    /// messages are shown (`message` takes `&self`).
    polls: RefCell<HashMap<String, Value>>,
    /// Discord-only metadata needed to invoke controls; stored only for messages with components.
    component_contexts: RefCell<HashMap<String, Value>>,
    /// What the core was last sent, to send only changes.
    rooms: godwit_core::room_sync::RoomSync,
    servers: Vec<CoreServer>,
    /// Scheduled events last reported by REST or the gateway, keyed by guild.
    scheduled_events: HashMap<String, Vec<godwit_core::server::ServerEvent>>,
    /// Current-user RSVP changes seen before an event's full details arrive.
    scheduled_event_membership: HashMap<(String, String), bool>,
    friends: Vec<Person>,
    requests: (Vec<Person>, Vec<Person>),
    /// The users we blocked, as last sent to the core.
    blocked: Vec<UserId>,
    ignored: Vec<UserId>,
    /// Friends' activities as last sent (Event::Activities), to send changes only.
    activities: Vec<(UserId, Arc<str>)>,
    /// The custom emoji groups as last sent (Event::CustomEmoji).
    emoji_sent: Option<Vec<godwit_core::message::EmojiGroup>>,
    /// The sticker groups last sent to core, so an unchanged list isn't sent
    /// again (as `emoji_sent`).
    stickers_sent: Option<Vec<godwit_core::stickers::StickerGroup>>,
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
    profiles: HashMap<(String, Option<String>), Value>,
    /// Our private notes on people, by user id (asked when a profile opens,
    /// kept current by USER_NOTE_UPDATE); a person without one is absent.
    notes: HashMap<String, String>,
    /// Our own profile asked for (Settings → Profile): loading, or reloading
    /// after a save (true).
    own_profile: Option<bool>,
    /// Guilds awaiting a post-save readback, so the core can mark the edit saved.
    server_profile_saved: std::collections::HashSet<String>,
    /// The data-use consents as Discord last said (key → on), once asked.
    consents: Option<Vec<(String, bool)>>,
    /// The server whose Roles page is open, and the roles last sent.
    roles_watch: Option<(String, ServerRoles)>,
    /// The member search in flight and the newest query waiting behind it.
    member_search: crate::member_search::SearchGate,
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
            component_contexts: RefCell::new(HashMap::new()),
            store: Store::default(),
            rooms: Default::default(),
            servers: Vec::new(),
            scheduled_events: HashMap::new(),
            scheduled_event_membership: HashMap::new(),
            friends: Vec::new(),
            requests: (Vec::new(), Vec::new()),
            blocked: Vec::new(),
            ignored: Vec::new(),
            activities: Vec::new(),
            emoji_sent: None,
            stickers_sent: None,
            viewing: None,
            call_shown: false,
            subscribed: Default::default(),
            typing_sent: HashMap::new(),
            typing: HashMap::new(),
            pending: HashMap::new(),
            profile_for: None,
            profiles: HashMap::new(),
            notes: HashMap::new(),
            own_profile: None,
            server_profile_saved: Default::default(),
            consents: None,
            roles_watch: None,
            member_search: Default::default(),
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
            settings_channel: None,
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
            // The picker's tiles use the same address (store::sticker_url).
            let url = crate::store::sticker_url(id, s["format_type"].as_u64().unwrap_or(1), 160)?;
            Some((url.into(), name.into()))
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
    fn autocomplete_text(&self, kind: godwit_core::autocomplete::TriggerKind, id: &str, name: &str) -> String {
        match kind {
            godwit_core::autocomplete::TriggerKind::Person => format!("<@{id}>"),
            godwit_core::autocomplete::TriggerKind::Channel => format!("<#{id}>"),
            godwit_core::autocomplete::TriggerKind::Emoji => format!("<:{name}:{id}>"),
        }
    }
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
            Command::LoadReactors { room, message, emoji } => {
                if !self.store.can(&room.raw, p::VIEW_CHANNEL | p::READ_MESSAGE_HISTORY) {
                    return vec![Event::Reactors {
                        room,
                        message,
                        emoji,
                        people: Vec::new(),
                    }];
                }
                if let Some(channel) = self.channel_of(&room) {
                    req(
                        &self.net,
                        Request::Reactors {
                            channel,
                            message: message.0.to_string(),
                            emoji: emoji.to_string(),
                        },
                    );
                }
            }
            // Discord contact nicknames stay in Godwit's local contact model.
            Command::SetNickname { .. } => {}
            Command::Focus { room } => {
                self.viewing = room.as_ref().and_then(|r| self.channel_of(r));
                // Leaving a forum (and its posts) lets go of its loaded
                // archived posts.
                if !self.store.opened_archived.is_empty() {
                    let in_forum = self
                        .viewing
                        .as_ref()
                        .and_then(|c| self.store.channels.get(c))
                        .is_some_and(|c| {
                            c.forum()
                                || c.parent
                                    .as_ref()
                                    .and_then(|p| self.store.channels.get(p))
                                    .is_some_and(|p| p.forum())
                        });
                    if !in_forum {
                        self.forget_archived();
                        self.sync_rooms(&mut out);
                    }
                }
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
                let limit = self.message_limit();
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
            Command::SendSticker {
                room,
                sticker,
                reply_to,
            } => {
                let Some(channel) = self.channel_of(&room) else {
                    return out;
                };
                // The dispatcher checked already; this guards a stale room.
                if !self.store.can(&channel, p::SEND_MESSAGES) {
                    out.push(Event::Error(
                        "Discord, sending a sticker: you can't send messages here.".into(),
                    ));
                    return out;
                }
                let guild = self.store.channels.get(&channel).and_then(|c| c.guild.clone());
                let message = reply_to.as_ref().map(|r| r.0.as_ref());
                let reply = message.map(|m| (guild.as_deref(), channel.as_str(), m));
                let body = crate::api::sticker_body(&sticker, &crate::api::nonce(), reply);
                req(&self.net, Request::SendSticker { channel, body });
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
            Command::ForwardMessage {
                room,
                message,
                to,
                comment,
            } => {
                let (Some(channel), Some(target)) = (self.channel_of(&room), self.channel_of(&to)) else {
                    return out;
                };
                let allowed = self.room(&channel).is_some_and(|r| r.capabilities.forward_from)
                    && self.room(&target).is_some_and(|r| r.capabilities.forward_to);
                if !allowed {
                    out.push(Event::Error(
                        "Discord, forwarding a message: this message can't be forwarded here.".into(),
                    ));
                    return out;
                }
                if comment.chars().count() > self.message_limit() {
                    out.push(Event::Error(
                        format!(
                            "Discord takes at most {} characters in a message.",
                            self.message_limit()
                        )
                        .into(),
                    ));
                    return out;
                }
                let guild_of = |c: &str| self.store.channels.get(c).and_then(|c| c.guild.clone());
                let (guild, target_guild) = (guild_of(&channel), guild_of(&target));
                req(
                    &self.net,
                    Request::Forward {
                        guild,
                        target_guild,
                        channel,
                        message: message.0.to_string(),
                        target,
                        comment,
                    },
                );
            }
            Command::ActivateMessageButton {
                room,
                message,
                custom_id,
            } => {
                let (Some(channel), Some(session), Some(context)) = (
                    self.channel_of(&room),
                    self.session_id.as_deref(),
                    self.component_contexts.borrow().get(message.0.as_ref()).cloned(),
                ) else {
                    return out;
                };
                let Some(application) = context["application_id"].as_str() else {
                    return out;
                };
                let nonce = crate::api::nonce();
                let body = crate::api::message_component_body(crate::api::MessageComponentInteraction {
                    application_id: application,
                    guild_id: context["guild_id"].as_str(),
                    channel_id: &channel,
                    message_id: &message.0,
                    message_flags: context["flags"].as_u64().unwrap_or_default(),
                    session_id: session,
                    nonce: &nonce,
                    custom_id: &custom_id,
                });
                req(&self.net, Request::MessageComponent(body));
            }
            Command::MarkUnread { room, last_read } => {
                if let Some(channel) = self.channel_of(&room) {
                    let after = last_read.as_ref().map(|m| m.0.as_ref());
                    let mention_count = self.store.mark_unread_after(&channel, after);
                    // With no message before it there is nothing to ack (the
                    // official client sends a synthetic id there); only the
                    // local marker moves until the next read.
                    if let Some(message) = last_read {
                        req(
                            &self.net,
                            Request::AckManual {
                                channel,
                                message: message.0.to_string(),
                                mention_count,
                            },
                        );
                    }
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
            Command::CreateGroup { name, users } => {
                let users: Vec<String> = users.into_iter().map(|u| u.raw.to_string()).collect();
                if users.len() < 2 || users.len() > 9 || name.trim().is_empty() {
                    return out;
                }
                req(
                    &self.net,
                    Request::CreateGroup {
                        name: name.trim().to_owned(),
                        users,
                    },
                );
            }
            Command::LeaveGroup { room } => {
                if self
                    .store
                    .channels
                    .get(&*room.raw)
                    .is_some_and(|c| c.kind == store::GROUP_DM)
                {
                    let channel = room.raw.to_string();
                    req(&self.net, Request::LeaveGroup { channel });
                }
            }
            Command::AddGroupMember { room, user } => {
                if self
                    .store
                    .channels
                    .get(&*room.raw)
                    .is_some_and(|c| c.kind == store::GROUP_DM)
                {
                    let channel = room.raw.to_string();
                    req(
                        &self.net,
                        Request::AddGroupRecipient {
                            channel,
                            user: user.raw.to_string(),
                        },
                    );
                }
            }
            Command::RemoveGroupMember { room, user } => {
                if self
                    .store
                    .channels
                    .get(&*room.raw)
                    .is_some_and(|c| c.kind == store::GROUP_DM)
                {
                    let channel = room.raw.to_string();
                    req(
                        &self.net,
                        Request::RemoveGroupRecipient {
                            channel,
                            user: user.raw.to_string(),
                        },
                    );
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
            Command::IgnoreUser { user } if user.raw.as_ref() != self.me() => req(
                &self.net,
                Request::IgnoreUser {
                    user: user.raw.to_string(),
                },
            ),
            Command::UnignoreUser { user } if user.raw.as_ref() != self.me() => req(
                &self.net,
                Request::UnignoreUser {
                    user: user.raw.to_string(),
                },
            ),
            // Our note on someone else (the card offers it for no one else).
            Command::SetNote { user, note } if user.raw.as_ref() != self.me() => {
                let (user, note) = (user.raw.to_string(), note.clone());
                let previous = self.notes.get(&user).cloned().unwrap_or_default();
                if self.set_note(&user, &note) {
                    self.sync_profile(&mut out);
                }
                req(&self.net, Request::SetNote { user, note, previous });
            }
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
            Command::LoadServerProfile { server } => req(
                &self.net,
                Request::LoadServerProfile {
                    user: self.me().to_owned(),
                    guild: server.raw.to_string(),
                },
            ),
            Command::SaveServerProfile { server, edit } => match server_profile_request(*edit) {
                Ok((body, avatar)) => req(
                    &self.net,
                    Request::SaveServerProfile {
                        guild: server.raw.to_string(),
                        body,
                        avatar,
                    },
                ),
                Err(error) => out.push(Event::ServerProfileSaved {
                    server,
                    result: Err(error.into()),
                }),
            },
            Command::LoadProfile { user, room } => {
                let guild = room
                    .as_ref()
                    .and_then(|r| self.store.channels.get(r.raw.as_ref()))
                    .and_then(|c| c.guild.clone());
                let raw = user.raw.to_string();
                self.profile_for = Some((raw.clone(), guild.clone()));
                self.sync_profile(&mut out);
                // The note comes with the profile (the web client fetches it when the popout opens).
                if !self.notes.contains_key(&raw) && raw != self.me() {
                    req(&self.net, Request::Note { user: raw.clone() });
                }
                if !self.profiles.contains_key(&(raw.clone(), guild.clone())) {
                    req(&self.net, Request::Profile { user: raw, guild });
                }
            }
            Command::InviteToActivity { .. } => {}
            Command::MemberRows { room, first, last } => {
                if let Some(channel) = self.channel_of(&room)
                    && let Some(guild) = self.store.channels.get(&channel).and_then(|c| c.guild.clone())
                    && self.list_channel.get(&guild) == Some(&channel)
                {
                    let (first, last) = (usize::try_from(first).unwrap_or(0), usize::try_from(last).unwrap_or(0));
                    self.ask_ranges(&guild, &channel, first, last);
                }
            }
            Command::SearchMembers { room, query } => self.search_members(&room, &query, &mut out),
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
                tags,
            } => {
                if let Some(channel) = self.channel_of(&room) {
                    self.net.send(Cmd::Request(Request::CreateThread {
                        channel,
                        message: message.map(|m| m.0.to_string()),
                        name,
                        text,
                        tags: tags.iter().map(|t| t.to_string()).collect(),
                    }));
                }
            }
            Command::JoinThread { room } => {
                if let Some(channel) = self.channel_of(&room)
                    && self.store.channels.get(&channel).is_some_and(|c| c.thread())
                {
                    self.net
                        .send(Cmd::Request(Request::ThreadMembership { channel, join: true }));
                }
            }
            Command::LeaveThread { room } => {
                if let Some(channel) = self.channel_of(&room)
                    && self.store.channels.get(&channel).is_some_and(|c| c.thread())
                {
                    self.net
                        .send(Cmd::Request(Request::ThreadMembership { channel, join: false }));
                }
            }
            Command::LoadForum {
                room,
                sort,
                tag,
                offset,
            } => {
                if let Some(channel) = self.channel_of(&room) {
                    match offset {
                        None => self.forum_open(&room, &channel, &mut out),
                        Some(offset) => self.net.send(Cmd::Request(Request::ForumThreads {
                            channel,
                            sort,
                            tag: tag.map(|t| t.to_string()),
                            offset,
                        })),
                    }
                }
            }
            Command::SearchMessages { room, query, offset } => {
                if let Some((guild, channel, params)) = self.search_request(&room, &query, offset) {
                    self.net.send(Cmd::Request(Request::Search {
                        guild,
                        channel,
                        query: Box::new(query),
                        params: Box::new(params),
                    }));
                }
            }
            Command::VotePoll { room, message, answer } => {
                if let Some(channel) = self.channel_of(&room) {
                    self.vote(&room, &channel, &message.0, answer, &mut out);
                }
            }
            Command::CreatePoll {
                room,
                question,
                answers,
                hours,
                multi,
            } => {
                let Some(channel) = self.channel_of(&room) else {
                    return out;
                };
                // The dispatcher checked already; this guards a stale room.
                let limits = self.room(&channel).and_then(|r| r.capabilities.polls);
                let checked = limits
                    .ok_or_else(|| "polls can't be created here.".to_owned())
                    .and_then(|l| l.check(&question, &answers));
                match checked {
                    Ok((question, answers)) => {
                        let body = crate::api::poll_body(&question, &answers, hours, multi, &crate::api::nonce());
                        req(&self.net, Request::CreatePoll { channel, body });
                    }
                    Err(why) => out.push(Event::Error(format!("Discord, creating a poll: {why}").into())),
                }
            }
            Command::EndPoll { room, message } => {
                if let Some(channel) = self.channel_of(&room) {
                    req(
                        &self.net,
                        Request::EndPoll {
                            channel,
                            message: message.0.to_string(),
                        },
                    );
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
            Command::LoadServerEvents { server } if self.store.guilds.contains_key(&server.raw.to_string()) => {
                req(
                    &self.net,
                    Request::ScheduledEvents {
                        guild: server.raw.to_string(),
                    },
                );
            }
            Command::LoadModerationLog { server, before }
                if self.store.guilds.contains_key(&server.raw.to_string())
                    && self.store.guild_permissions(&server.raw) & p::VIEW_AUDIT_LOG != 0 =>
            {
                req(
                    &self.net,
                    Request::AuditLog {
                        guild: server.raw.to_string(),
                        before: before.map(|id| id.to_string()),
                    },
                );
            }
            Command::SetServerEventInterest {
                server,
                event,
                interested,
            } if self.store.guilds.contains_key(&server.raw.to_string())
                && self
                    .scheduled_events
                    .get(server.raw.as_ref())
                    .is_some_and(|events| events.iter().any(|e| e.id == event)) =>
            {
                req(
                    &self.net,
                    Request::ScheduledEventInterest {
                        guild: server.raw.to_string(),
                        event: event.to_string(),
                        interested,
                    },
                );
            }
            Command::LoadServerEvents { .. }
            | Command::SetServerEventInterest { .. }
            | Command::LoadModerationLog { .. } => {}
            Command::ServerAdmin { server, op } => out.extend(self.server_admin(server, op)),
            Command::SetRoomNotify { room, change } => out.extend(self.set_room_notify(&room, change)),
            Command::LoadChannelOverview { .. }
            | Command::SaveChannelOverview { .. }
            | Command::SetChannelOverride { .. }
            | Command::DeleteChannel { .. } => out.extend(self.channel_command(command)),
            // Discord's webhook manager is not wired in this account yet.
            Command::LoadWebhooks { .. } | Command::ChangeWebhook { .. } => {}
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
            Command::CreateServer { .. } => {}
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

    fn report_notice(&self) -> Option<godwit_core::report::ReportNotice> {
        Some(crate::report_notice())
    }

    fn privacy_section(&self) -> Option<godwit_core::privacy_overview::PrivacySection> {
        Some(crate::privacy_section())
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
    fn login_ticket_debug_is_redacted_even_inside_status() {
        let status = Status::NeedsCode {
            ticket: LoginTicket("sensitive-ticket".into()),
            totp: true,
            sms: false,
            backup: false,
            webauthn: false,
            hint: String::new(),
            error: None,
        };
        let debug = format!("{status:?}");
        assert!(!debug.contains("sensitive-ticket"));
        assert!(debug.contains("redacted"));
    }

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
    fn scheduled_event_json_gateway_updates_and_rsvp_commands() {
        let (mut a, mut commands, _) = account();
        ready(&mut a);
        while commands.try_recv().is_ok() {}
        let raw = json!({
            "id": "900", "guild_id": "10", "name": "Town hall", "description": "Monthly update",
            "scheduled_start_time": "2026-12-01T18:00:00+00:00", "scheduled_end_time": "2026-12-01T19:00:00+00:00",
            "entity_type": 2, "channel_id": "13", "status": 1, "user_count": 7, "image": "cover-hash"
        });
        let server = ServerId::new(a.account.clone().expect("ready account"), "10");
        a.run(Command::LoadServerEvents { server: server.clone() });
        assert!(matches!(commands.try_recv(), Ok(Cmd::Request(Request::ScheduledEvents { guild })) if guild == "10"));
        let mut output = Vec::new();
        let mut before_details = raw.clone();
        before_details["id"] = json!("902");
        a.dispatch(
            "GUILD_SCHEDULED_EVENT_USER_ADD",
            &json!({
                "guild_id": "10", "guild_scheduled_event_id": "902", "user_id": "100"
            }),
            &mut output,
        );
        assert!(
            a.parse_server_event("10", &before_details)
                .expect("gateway RSVP before event details")
                .interested
        );
        a.answer(
            Answer::ScheduledEvents {
                guild: "10".into(),
                result: Ok(json!([raw.clone()])),
            },
            &mut output,
        );
        let parsed = a.scheduled_events["10"][0].clone();
        assert_eq!(parsed.id.as_ref(), "900");
        assert_eq!(parsed.name.as_ref(), "Town hall");
        assert_eq!(parsed.location.as_deref(), Some("general"));
        assert_eq!(parsed.interested_count, 7);
        assert!(parsed.ends_at.is_some());
        assert!(
            parsed
                .cover_url
                .as_deref()
                .is_some_and(|url| url.contains("cover-hash"))
        );

        let mut second = raw.clone();
        second["id"] = json!("901");
        a.dispatch("GUILD_SCHEDULED_EVENT_CREATE", &second, &mut output);
        assert!(a.scheduled_events["10"].iter().any(|entry| entry.id.as_ref() == "901"));
        second["name"] = json!("Updated event");
        second["status"] = json!(2);
        a.dispatch("GUILD_SCHEDULED_EVENT_UPDATE", &second, &mut output);
        assert!(a.scheduled_events["10"].iter().any(|entry| entry.id.as_ref() == "901"
            && entry.name.as_ref() == "Updated event"
            && entry.status.as_ref() == "Active"));
        a.dispatch(
            "GUILD_SCHEDULED_EVENT_DELETE",
            &json!({"guild_id": "10", "id": "901"}),
            &mut output,
        );
        assert!(!a.scheduled_events["10"].iter().any(|entry| entry.id.as_ref() == "901"));

        a.dispatch(
            "GUILD_SCHEDULED_EVENT_USER_ADD",
            &json!({
                "guild_id": "10", "guild_scheduled_event_id": "900", "user_id": "100"
            }),
            &mut output,
        );
        let event = &a.scheduled_events["10"][0];
        assert_eq!(event.interested_count, 8);
        assert!(event.interested, "our own gateway RSVP marks the toggle on");
        a.dispatch(
            "GUILD_SCHEDULED_EVENT_USER_REMOVE",
            &json!({
                "guild_id": "10", "guild_scheduled_event_id": "900", "user_id": "200"
            }),
            &mut output,
        );
        assert_eq!(a.scheduled_events["10"][0].interested_count, 7);
        assert!(
            a.scheduled_events["10"][0].interested,
            "another user's RSVP leaves ours on"
        );

        a.run(Command::SetServerEventInterest {
            server,
            event: "900".into(),
            interested: false,
        });
        assert!(
            matches!(commands.try_recv(), Ok(Cmd::Request(Request::ScheduledEventInterest { guild, event, interested: false })) if guild == "10" && event == "900")
        );
        a.answer(
            Answer::ScheduledEventInterest {
                guild: "10".into(),
                event: "900".into(),
                interested: false,
                result: Ok(()),
            },
            &mut output,
        );
        assert!(!a.scheduled_events["10"][0].interested);
        assert_eq!(a.scheduled_events["10"][0].interested_count, 6);
        a.dispatch(
            "GUILD_CREATE",
            &json!({"id": "10", "properties": {"name": "Qux Hall"}, "guild_scheduled_events": [raw]}),
            &mut output,
        );
        assert_eq!(
            a.scheduled_events["10"].len(),
            1,
            "GUILD_CREATE carries the event snapshot when present"
        );
    }

    #[test]
    fn leaving_group_dm_emits_channel_delete_and_demo_removes_room() {
        let ready = json!({
            "user": {"id": "100", "username": "me"},
            "private_channels": [{"id": "50", "type": 3, "name": "friends", "recipients": [{"id": "7", "username": "robin"}]}],
            "relationships": [], "merged_members": [],
        });
        let (mut account, mut commands, _) = account();
        let mut events = Vec::new();
        account.handle(Ev::Ready(Box::new(ready.clone())), &mut events);
        let room = account.room_id("50").expect("group exists");
        while commands.try_recv().is_ok() {}
        let output = account.run(Command::LeaveGroup { room });
        assert!(output.is_empty());
        assert!(matches!(commands.try_recv(), Ok(Cmd::Request(Request::LeaveGroup { channel })) if channel == "50"));

        let (mut demo, _) = DiscordAccount::demo(ready, HashMap::new());
        let room = demo.room_id("50").expect("demo group exists");
        let output = demo.run(Command::LeaveGroup { room: room.clone() });
        assert!(output.contains(&Event::RoomRemoved(room)));
    }

    #[test]
    fn thread_membership_gateway_events_and_demo_commands_update_joined_state() {
        let mut ready_payload = crate::store::test_ready();
        ready_payload["guilds"][0]["threads"] = json!([
            {"id":"15", "type":11, "guild_id":"10", "parent_id":"13", "name":"topic", "member":null},
            {"id":"16", "type":12, "guild_id":"10", "parent_id":"13", "name":"private", "member":null}
        ]);
        let (mut account, _, _) = account();
        let mut initial = Vec::new();
        account.handle(Ev::Ready(Box::new(ready_payload.clone())), &mut initial);
        let id = account.room_id("15").expect("thread room");
        assert!(
            !account
                .room("16")
                .expect("private thread room")
                .capabilities
                .join_thread
        );
        assert!(!account.room("15").expect("thread room").joined_thread);
        let mut events = Vec::new();
        account.dispatch(
            "THREAD_MEMBERS_UPDATE",
            &json!({"id":"15", "added_members":[{"user_id":"100"}]}),
            &mut events,
        );
        assert!(account.room("15").expect("thread room").joined_thread);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::RoomUpdated(r) if r.id == id && r.joined_thread))
        );
        events.clear();
        account.dispatch(
            "THREAD_MEMBERS_UPDATE",
            &json!({"id":"15", "removed_member_ids":["100"]}),
            &mut events,
        );
        assert!(!account.room("15").expect("thread room").joined_thread);

        let (mut demo, _) = DiscordAccount::demo(ready_payload, HashMap::new());
        let id = demo.room_id("15").expect("demo thread room");
        let joined = demo.run(Command::JoinThread { room: id.clone() });
        assert!(
            joined
                .iter()
                .any(|e| matches!(e, Event::RoomUpdated(r) if r.id == id && r.joined_thread))
        );
        let left = demo.run(Command::LeaveThread { room: id.clone() });
        assert!(
            left.iter()
                .any(|e| matches!(e, Event::RoomUpdated(r) if r.id == id && !r.joined_thread))
        );
        assert!(demo.room("15").is_some(), "leaving keeps the thread visible");
    }

    #[test]
    fn demo_stickers_are_listed_for_the_server_and_sending_one_arrives_as_a_message() {
        let ready = json!({
            "user": {"id": "100", "username": "me_bloop"},
            "guilds": [{
                "id": "10", "properties": {"name": "Qux Hall", "owner_id": "100"},
                "roles": [{"id": "10", "name": "@everyone", "permissions": (crate::permissions::VIEW_CHANNEL | crate::permissions::SEND_MESSAGES).to_string(), "position": 0}],
                "channels": [{"id": "13", "type": 0, "name": "general", "position": 0}],
                "stickers": [
                    {"id": "1", "name": "Wave", "tags": "hi", "format_type": 1},
                    {"id": "2", "name": "Fancy", "format_type": 3}
                ]
            }],
            "private_channels": [{"id": "50", "type": 1, "recipients": [{"id": "7", "username": "robin"}]}],
            "relationships": [], "merged_members": [],
        });
        let (mut a, events) = DiscordAccount::demo(ready, HashMap::new());
        let groups = events
            .iter()
            .find_map(|e| match e {
                Event::Stickers { groups, .. } => Some(groups.clone()),
                _ => None,
            })
            .expect("stickers from READY");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].stickers.len(), 1, "Lottie can't be drawn");
        assert!(groups[0].stickers[0].image_url.ends_with("/stickers/1.png?size=160"));
        let channel = a.room_id("13").expect("the channel");
        let dm = a.room_id("50").expect("the DM");
        let caps = |r: &godwit_core::ids::RoomId| {
            events.iter().find_map(|e| match e {
                Event::AccountReady { rooms, .. } => rooms.iter().find(|x| x.id == *r).map(|x| x.capabilities.stickers),
                _ => None,
            })
        };
        assert_eq!(caps(&channel), Some(true));
        assert_eq!(caps(&dm), Some(false), "server stickers stay in their server");
        let out = a.run(Command::SendSticker {
            room: channel,
            sticker: "1".into(),
            reply_to: None,
        });
        let sent = out.iter().find_map(|e| match e {
            Event::MessageAdded { message, .. } => Some(message.clone()),
            _ => None,
        });
        let sent = sent.expect("the sticker comes back as a message");
        assert!(
            sent.jumbo.iter().any(|(url, _)| url.contains("/stickers/1.png")),
            "shown large, with its picture"
        );
    }

    #[test]
    fn a_sticker_goes_out_as_sticker_ids_and_refreshes_with_the_server() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let channel = a.room_id("13").expect("general");
        a.run(Command::SendSticker {
            room: channel,
            sticker: "42".into(),
            reply_to: None,
        });
        let Ok(Cmd::Request(Request::SendSticker { channel, body })) = cmds.try_recv() else {
            panic!("a request")
        };
        assert_eq!(channel, "13");
        assert_eq!(body["sticker_ids"], json!(["42"]));
        let mut out = Vec::new();
        a.handle(
            Ev::Dispatch(
                "GUILD_STICKERS_UPDATE".into(),
                json!({"guild_id": "10", "stickers": [{"id": "9", "name": "New", "format_type": 2}]}),
            ),
            &mut out,
        );
        assert!(matches!(
            out.as_slice(),
            [Event::Stickers { groups, .. }] if groups[0].stickers[0].name.as_ref() == "New"
        ));
        // Unchanged lists aren't resent.
        let mut again = Vec::new();
        a.handle(
            Ev::Dispatch(
                "GUILD_STICKERS_UPDATE".into(),
                json!({"guild_id": "10", "stickers": [{"id": "9", "name": "New", "format_type": 2}]}),
            ),
            &mut again,
        );
        assert!(again.is_empty());
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
                query: Box::new(godwit_core::search::SearchQuery::text_only("lunch")),
                offset: 0,
                result: Ok(json!({"retry_after": 2, "documents_indexed": 0})),
            }),
            &mut out,
        );
        assert!(
            out.iter().any(|e| matches!(
                e,
                Event::SearchResults { notice: Some(t), messages: None, .. } if t == "Search is indexing, try again in 2s"
            )),
            "failed, so searching again asks again: {out:?}"
        );
    }

    #[test]
    fn demo_search_answers_filtered_pages_with_a_total() {
        let ready = json!({
            "user": {"id": "100", "username": "me_bloop"},
            "guilds": [],
            "private_channels": [{"id": "50", "type": 1, "recipients": [{"id": "7", "username": "robin"}]}],
            "relationships": [], "merged_members": [],
        });
        let msg = |id: u64, text: &str, author: &str| {
            json!({"id": id.to_string(), "content": text, "type": 0, "timestamp": "2026-11-01T10:00:00+00:00",
                   "author": {"id": author, "username": "u"}})
        };
        let history = HashMap::from([(
            "50".to_owned(),
            vec![
                msg(3, "Lunch?", "7"),
                msg(2, "lunch at noon", "100"),
                msg(1, "hello", "7"),
            ],
        )]);
        let (mut a, _) = DiscordAccount::demo(ready, history);
        let room = a.room_id("50").expect("the DM");
        let mut query = godwit_core::search::SearchQuery::text_only("lunch");
        let out = a.run(Command::SearchMessages {
            room: room.clone(),
            query: query.clone(),
            offset: 0,
        });
        let Some(Event::SearchResults {
            total,
            next_offset,
            messages: Some(m),
            ..
        }) = out.last()
        else {
            panic!("{out:?}")
        };
        assert_eq!((m.len(), *total, *next_offset), (2, Some(2), None));
        query.from = Some(godwit_core::ids::UserId::new(room.account.clone(), "100"));
        let out = a.run(Command::SearchMessages { room, query, offset: 0 });
        assert!(matches!(out.last(), Some(Event::SearchResults { messages: Some(m), .. }) if m.len() == 1));
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
        a.run(Command::IgnoreUser { user: zarg.clone() });
        assert!(matches!(cmds.try_recv(), Ok(Cmd::Request(Request::IgnoreUser { user })) if user == "200"));
        a.run(Command::UnignoreUser { user: zarg.clone() });
        assert!(matches!(cmds.try_recv(), Ok(Cmd::Request(Request::UnignoreUser { user })) if user == "200"));
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
    fn a_ready_with_guild_folders_puts_two_servers_in_a_folder_and_leaves_a_lone_guild_plain() {
        let mut ready = crate::store::test_ready();
        for id in ["12x", "13x"] {
            ready["guilds"].as_array_mut().unwrap().push(
                serde_json::json!({"id": id, "properties": {"name": format!("S{id}"), "owner_id": "200"}, "roles": [], "channels": []}),
            );
        }
        ready["user_settings"] = serde_json::json!({"guild_folders": [
            {"id": null, "name": null, "color": null, "guild_ids": ["10"]},
            {"id": 5, "name": "Pals", "color": 0x5865f2, "guild_ids": ["12x", "13x"]},
        ]});
        let (mut a, _cmds, _tx) = account();
        let mut events = Vec::new();
        a.handle(Ev::Ready(Box::new(ready)), &mut events);
        let servers = events
            .iter()
            .find_map(|e| match e {
                Event::AccountReady { servers, .. } => Some(servers),
                _ => None,
            })
            .unwrap();
        let folder = |raw: &str| servers.iter().find(|s| &*s.id.raw == raw).unwrap().folder.clone();
        assert_eq!(folder("10"), None);
        let f = folder("12x").unwrap();
        assert_eq!(
            (&*f.id, f.name.as_deref(), f.colour),
            ("5", Some("Pals"), Some(0x5865f2))
        );
        assert_eq!(folder("13x"), Some(f));
    }

    #[test]
    fn a_settings_update_changing_folders_sends_the_servers_again() {
        let mut ready = crate::store::test_ready();
        for id in ["21", "22"] {
            ready["guilds"].as_array_mut().unwrap().push(
                serde_json::json!({"id": id, "properties": {"name": id, "owner_id": "200"}, "roles": [], "channels": []}),
            );
        }
        let (mut a, _cmds, _tx) = account();
        let mut out = Vec::new();
        a.handle(Ev::Ready(Box::new(ready)), &mut out);
        let proto = crate::settings::test_folders_proto(&[(9, &[21, 22])]);
        let mut out = Vec::new();
        a.handle(
            Ev::Dispatch(
                "USER_SETTINGS_PROTO_UPDATE".into(),
                serde_json::json!({"settings": {"type": 1, "proto": proto}}),
            ),
            &mut out,
        );
        let folders: Vec<_> = out
            .iter()
            .find_map(|e| match e {
                Event::ServersChanged { servers, .. } => Some(servers.iter().map(|s| s.folder.is_some()).collect()),
                _ => None,
            })
            .expect("the servers are sent again");
        assert_eq!(folders.iter().filter(|f| **f).count(), 2);
    }

    #[test]
    fn demo_mark_unread_moves_the_marker_back_at_once() {
        let (mut a, _) = DiscordAccount::demo(crate::store::test_ready(), HashMap::new());
        let room = a.room_id("13").unwrap();
        let last = a.store.channels["13"].last_message_id.clone().unwrap_or_default();
        a.store.mark_read("13", &last);
        assert!(!a.store.unread("13"));
        a.run(Command::MarkUnread {
            room,
            last_read: Some(MessageId("1".into())),
        });
        assert!(a.store.unread("13"));
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
        let ignored = a.user_id("200").unwrap();
        let out = a.run(Command::IgnoreUser { user: ignored.clone() });
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Ignored { users, .. } if users == std::slice::from_ref(&ignored)))
        );
        let out = a.run(Command::UnignoreUser { user: ignored });
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Ignored { users, .. } if users.is_empty()))
        );
    }

    #[test]
    fn a_profile_answer_brings_mutual_servers_and_friends_and_the_note_follows() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let zarg = a.user_id("200").unwrap();
        a.run(Command::LoadProfile {
            user: zarg.clone(),
            room: None,
        });
        let mut asked = Vec::new();
        while let Ok(Cmd::Request(r)) = cmds.try_recv() {
            asked.push(r);
        }
        assert!(
            asked
                .iter()
                .any(|r| matches!(r, Request::Note { user } if user == "200"))
        );
        assert!(
            asked
                .iter()
                .any(|r| matches!(r, Request::Profile { user, .. } if user == "200"))
        );
        let mut out = Vec::new();
        a.answer(
            Answer::Profile {
                user: "200".into(),
                guild: None,
                result: Ok(json!({
                    "user": {"id": "200", "username": "zarg"},
                    "mutual_guilds": [{"id": "10", "nick": "Z"}, {"id": "999", "nick": null}],
                    "mutual_friends": [{"id": "300", "username": "kim", "global_name": "Kim"}],
                })),
            },
            &mut out,
        );
        assert!(
            matches!(out.last(), Some(Event::Profile(p)) if !p.can_note),
            "no field before the note arrives"
        );
        a.answer(
            Answer::Note {
                user: "200".into(),
                result: Ok("met at a con".into()),
            },
            &mut out,
        );
        let Some(Event::Profile(p)) = out.iter().rev().find(|e| matches!(e, Event::Profile(_))) else {
            panic!("{out:?}")
        };
        assert_eq!(p.mutual_servers.len(), 1, "servers we don't know are left out");
        assert_eq!(p.mutual_servers[0].server.raw.as_ref(), "10");
        assert_eq!(p.mutual_servers[0].nick.as_deref(), Some("Z"));
        assert_eq!(p.mutual_friends.len(), 1);
        assert_eq!(p.mutual_friends[0].name.as_ref(), "Kim");
        assert!(!a.store.users.contains_key("300"), "mutuals aren't kept as users");
        assert_eq!(p.note_limit, 256);
        assert_eq!(p.note.as_deref(), Some("met at a con"));
        assert!(p.can_note);
        // The gateway's update replaces it, and an empty one clears it.
        let mut out = Vec::new();
        a.dispatch("USER_NOTE_UPDATE", &json!({"id": "200", "note": "new"}), &mut out);
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Profile(p) if p.note.as_deref() == Some("new")))
        );
        let mut out = Vec::new();
        a.dispatch("USER_NOTE_UPDATE", &json!({"id": "200", "note": ""}), &mut out);
        assert!(out.iter().any(|e| matches!(e, Event::Profile(p) if p.note.is_none())));
        // A note already known isn't asked for again.
        while cmds.try_recv().is_ok() {}
        a.run(Command::LoadProfile {
            user: zarg.clone(),
            room: None,
        });
        assert!(!std::iter::from_fn(|| cmds.try_recv().ok()).any(|c| matches!(c, Cmd::Request(Request::Note { .. }))));
        // Saving sends the PUT and shows the note at once; never on ourselves.
        let out = a.run(Command::SetNote {
            user: zarg.clone(),
            note: "x".into(),
        });
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Profile(p) if p.note.as_deref() == Some("x")))
        );
        assert!(matches!(
            std::iter::from_fn(|| cmds.try_recv().ok()).find(|c| matches!(c, Cmd::Request(Request::SetNote { .. }))),
            Some(Cmd::Request(Request::SetNote { user, note, .. })) if user == "200" && note == "x"
        ));
        // A failed save puts the old note back and says so; a late fetch changes nothing.
        let mut out = Vec::new();
        a.answer(
            Answer::NoteSaved {
                user: "200".into(),
                previous: "old".into(),
                result: Err(crate::backend::RequestError::from(anyhow::anyhow!("no"))),
            },
            &mut out,
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Profile(p) if p.note.as_deref() == Some("old")))
        );
        assert!(out.iter().any(|e| matches!(e, Event::Error(_))));
        let mut out = Vec::new();
        a.answer(
            Answer::Note {
                user: "200".into(),
                result: Ok("stale".into()),
            },
            &mut out,
        );
        assert!(out.is_empty());
        let me = a.user_id(a.me()).unwrap();
        a.run(Command::SetNote {
            user: me,
            note: "x".into(),
        });
        assert!(cmds.try_recv().is_err());
    }

    #[test]
    fn demo_profiles_have_mutuals_and_a_note_that_saves_through_the_gateway_event() {
        let (mut a, _) = DiscordAccount::demo(crate::store::test_ready(), HashMap::new());
        let zarg = a.user_id("200").unwrap();
        let out = a.run(Command::LoadProfile {
            user: zarg.clone(),
            room: None,
        });
        let Some(Event::Profile(p)) = out.iter().find(|e| matches!(e, Event::Profile(_))) else {
            panic!("{out:?}")
        };
        assert!(!p.mutual_servers.is_empty());
        assert!(p.note.is_some());
        let out = a.run(Command::SetNote {
            user: zarg,
            note: "changed".into(),
        });
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Profile(p) if p.note.as_deref() == Some("changed")))
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
    fn forwarding_is_asked_only_where_discord_would_allow_it() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        let _ = cmds.try_recv(); // the presence update
        let (general, dm, hidden) = (
            a.room_id("13").unwrap(),
            a.room_id("20").unwrap(),
            a.room_id("14").unwrap(),
        );
        // Reading #general's history isn't granted in the test world, so its messages stay put.
        assert!(!a.room("13").unwrap().capabilities.forward_from);
        assert!(a.room("20").unwrap().capabilities.forward_from);
        assert!(a.room("13").unwrap().capabilities.forward_to);
        let forward = |room: &RoomId, to: &RoomId| Command::ForwardMessage {
            room: room.clone(),
            message: MessageId("600".into()),
            to: to.clone(),
            comment: "look".into(),
        };
        let out = a.run(forward(&general, &dm));
        assert!(matches!(out.as_slice(), [Event::Error(_)]), "{out:?}");
        assert!(cmds.try_recv().is_err());
        // Nor into a channel we can't see or write in.
        let out = a.run(forward(&dm, &hidden));
        assert!(matches!(out.as_slice(), [Event::Error(_)]), "{out:?}");
        assert!(cmds.try_recv().is_err());
        assert!(a.run(forward(&dm, &general)).is_empty());
        let Ok(Cmd::Request(Request::Forward {
            channel,
            message,
            target,
            comment,
            ..
        })) = cmds.try_recv()
        else {
            panic!("no forward")
        };
        assert_eq!((&*channel, &*message, &*target, &*comment), ("20", "600", "13", "look"));
    }

    #[test]
    fn demo_forwarding_answers_with_a_message_carrying_a_snapshot() {
        let history = HashMap::from([(
            "20".to_owned(),
            vec![
                json!({"id": "600", "channel_id": "20", "content": "see this", "type": 0, "timestamp": "2026-01-01T00:00:00+00:00", "attachments": [], "embeds": [], "author": {"id": "200", "username": "zarg"}}),
            ],
        )]);
        let (mut a, _) = DiscordAccount::demo(crate::store::test_ready(), history);
        let (dm, general) = (a.room_id("20").unwrap(), a.room_id("13").unwrap());
        let out = a.run(Command::ForwardMessage {
            room: dm,
            message: MessageId("600".into()),
            to: general.clone(),
            comment: "fyi".into(),
        });
        let sent: Vec<_> = out
            .iter()
            .filter_map(|e| match e {
                Event::MessageAdded { room, message } if *room == general => Some(message),
                _ => None,
            })
            .collect();
        assert_eq!(sent.len(), 2, "{out:?}");
        assert!(
            sent[0].text.contains("Forwarded") && sent[0].text.contains("see this"),
            "{:?}",
            sent[0].text
        );
        assert_eq!(&*sent[1].text, "fyi");
    }

    #[test]
    fn demo_polls_are_created_and_ended_like_discord_would() {
        let (mut a, _) = DiscordAccount::demo(crate::store::test_ready(), HashMap::new());
        let room = a.room_id("20").unwrap();
        let out = a.run(Command::CreatePoll {
            room: room.clone(),
            question: "Lunch?".into(),
            answers: vec!["Pizza".into(), "Soup".into()],
            hours: 24,
            multi: false,
        });
        let Some(Event::MessageAdded { message, .. }) = out.iter().find(|e| matches!(e, Event::MessageAdded { .. }))
        else {
            panic!("no poll: {out:?}")
        };
        assert!(
            message.text.contains("Lunch") && message.text.contains("](godwit://vote/2)"),
            "{:?}",
            message.text
        );
        assert!(message.poll_endable, "ours and open");
        let out = a.run(Command::EndPoll {
            room,
            message: message.id.clone(),
        });
        let Some(Event::MessageUpdated { message, .. }) =
            out.iter().find(|e| matches!(e, Event::MessageUpdated { .. }))
        else {
            panic!("no update: {out:?}")
        };
        assert!(message.text.contains("Poll ended") && !message.poll_endable);
    }

    #[test]
    fn creating_a_poll_asks_discord_for_the_message_it_documents() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        let room = a.room_id("20").unwrap();
        // Polls need Send Polls: a DM has it, the test server's @everyone doesn't.
        assert!(a.room("20").unwrap().capabilities.polls.is_some());
        assert!(a.room("13").unwrap().capabilities.polls.is_none());
        let run = |a: &mut DiscordAccount, question: &str, answers: &[&str]| {
            a.run(Command::CreatePoll {
                room: room.clone(),
                question: question.into(),
                answers: answers.iter().map(|s| (*s).to_owned()).collect(),
                hours: 72,
                multi: true,
            })
        };
        assert!(
            run(&mut a, "Lunch?", &["Pizza"])
                .iter()
                .any(|e| matches!(e, Event::Error(_)))
        );
        let out = run(&mut a, "Lunch?", &["Pizza", "Soup"]);
        assert!(out.is_empty(), "{out:?}");
        let mut found = None;
        while let Ok(c) = cmds.try_recv() {
            if let Cmd::Request(Request::CreatePoll { channel, body }) = c {
                found = Some((channel, body));
            }
        }
        let (channel, body) = found.expect("a request");
        assert_eq!(channel, "20");
        assert_eq!(body["poll"]["duration"], 72);
        assert_eq!(body["poll"]["allow_multiselect"], true);
        assert_eq!(body["poll"]["layout_type"], 1);
        assert_eq!(body["poll"]["question"]["text"], "Lunch?");
        assert_eq!(body["poll"]["answers"][1]["poll_media"]["text"], "Soup");
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
    fn channel_notification_settings_send_overrides_and_show_on_the_room() {
        use godwit_core::room::RoomNotifyChange as C;
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let room = RoomId::new(a.account.clone().expect("signed in"), "13");
        assert!(a.room("13").expect("room").capabilities.notify_settings);
        let out = a.run(Command::SetRoomNotify {
            room: room.clone(),
            change: C::Mute { seconds: Some(900) },
        });
        let Ok(Cmd::Request(Request::ChannelNotify {
            guild, channel, body, ..
        })) = cmds.try_recv()
        else {
            panic!("expected the override")
        };
        assert_eq!((guild.as_deref(), channel.as_str()), (Some("10"), "13"));
        assert_eq!(body["muted"], true);
        assert_eq!(body["mute_config"]["selected_time_window"], 900);
        assert!(body["mute_config"]["end_time"].is_string());
        assert!(out.iter().any(|e| matches!(e, Event::RoomUpdated(r) if r.notify.muted)));
        let out = a.run(Command::SetRoomNotify {
            room,
            change: C::Level(Some(1)),
        });
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::RoomUpdated(r) if r.notify.level == Some(1) && r.notify.muted))
        );
        // The whole override goes out, so the level change keeps the mute.
        let Ok(Cmd::Request(Request::ChannelNotify {
            guild, body, previous, ..
        })) = cmds.try_recv()
        else {
            panic!("expected the override")
        };
        assert_eq!(
            (body["message_notifications"].as_u64(), body["muted"].as_bool()),
            (Some(1), Some(true))
        );
        assert_eq!(previous["message_notifications"], 3);
        // A failed request puts the old override back.
        let mut out = Vec::new();
        a.answer(
            crate::backend::Answer::ChannelNotify {
                guild,
                previous,
                result: Err(anyhow::anyhow!("offline").into()),
            },
            &mut out,
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::RoomUpdated(r) if r.notify.level.is_none() && r.notify.muted))
        );
        assert!(out.iter().any(|e| matches!(e, Event::Error(_))));
    }

    #[test]
    fn settings_updates_read_overrides_with_expired_mutes_and_inherit() {
        let (mut a, _cmds, _tx) = account();
        ready(&mut a);
        let past = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let soon = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let mut out = Vec::new();
        a.dispatch(
            "USER_GUILD_SETTINGS_UPDATE",
            &json!({"guild_id": "10", "channel_overrides": [
                {"channel_id": "13", "muted": true, "mute_config": {"end_time": past, "selected_time_window": 900}, "message_notifications": 3},
                {"channel_id": "12", "muted": true, "mute_config": {"end_time": soon, "selected_time_window": 3600}, "message_notifications": 2},
                {"channel_id": "11", "muted": true, "mute_config": null, "message_notifications": 1},
            ]}),
            &mut out,
        );
        assert!(!a.store.channel_muted("13"), "an ended mute is off");
        assert!(a.store.channel_muted("12") && a.store.channel_muted("11"));
        assert_eq!(a.store.channel_notify.get("12"), Some(&2));
        assert!(!a.store.channel_notify.contains_key("13"), "3 inherits");
        // Resetting a channel clears it.
        a.dispatch(
            "USER_GUILD_SETTINGS_UPDATE",
            &json!({"guild_id": "10", "channel_overrides": [{"channel_id": "12", "muted": false, "mute_config": null, "message_notifications": 3}]}),
            &mut out,
        );
        assert!(!a.store.channel_muted("12") && !a.store.channel_notify.contains_key("12"));
    }

    #[test]
    fn demo_channel_notification_changes_come_back_as_room_updates() {
        use godwit_core::room::RoomNotifyChange as C;
        let (mut a, _) = DiscordAccount::demo(crate::store::test_ready(), HashMap::new());
        let room = RoomId::new(a.account.clone().expect("signed in"), "13");
        let out = a.run(Command::SetRoomNotify {
            room: room.clone(),
            change: C::Mute { seconds: None },
        });
        assert!(out.iter().any(|e| matches!(e, Event::RoomUpdated(r) if r.notify.muted)));
        let out = a.run(Command::SetRoomNotify {
            room,
            change: C::Unmute,
        });
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::RoomUpdated(r) if !r.notify.muted))
        );
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

fn server_profile_request(
    edit: godwit_core::own_profile::ProfileEdit,
) -> Result<(Value, Option<std::path::PathBuf>), String> {
    use godwit_core::own_profile::ProfileImage;
    const MAX_NICK: usize = 32;
    const MAX_PRONOUNS: usize = 40;
    const MAX_BIO: usize = 190;
    let mut body = serde_json::Map::new();
    let check = |s: &str, max: usize, field: &str| {
        if s.chars().count() > max {
            Err(format!("The {field} can be at most {max} characters on Discord."))
        } else {
            Ok(())
        }
    };
    if let Some(nick) = edit.display_name {
        let nick = nick.trim();
        check(nick, MAX_NICK, "nickname")?;
        body.insert("nick".into(), if nick.is_empty() { Value::Null } else { json!(nick) });
    }
    if let Some(bio) = edit.bio {
        check(&bio, MAX_BIO, "bio")?;
        body.insert("bio".into(), json!(bio.as_ref()));
    }
    if let Some(pronouns) = edit.pronouns {
        check(&pronouns, MAX_PRONOUNS, "pronouns")?;
        body.insert("pronouns".into(), json!(pronouns.as_ref()));
    }
    let mut image = None;
    for (field, change) in [("avatar", edit.avatar), ("banner", edit.banner)] {
        match change {
            ProfileImage::Keep => {}
            ProfileImage::Remove => {
                body.insert(field.into(), Value::Null);
            }
            ProfileImage::File { path, .. } => {
                if image.replace((field, path)).is_some() {
                    return Err("Choose either an avatar or a banner for each save.".into());
                }
            }
        }
    }
    let (field, path) = image.map_or((None, None), |(field, path)| (Some(field), Some(path)));
    if let Some(field) = field {
        body.insert("__image_field".into(), json!(field));
    }
    Ok((Value::Object(body), path))
}

fn server_profile_of(v: &Value, guild: &str, store: &store::Store) -> godwit_core::own_profile::OwnProfile {
    let member = &v["guild_member_profile"];
    let user = &v["user"];
    let name = member["nick"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| user["global_name"].as_str())
        .or_else(|| store.me.as_ref().map(|m| m.username.as_str()))
        .unwrap_or_default();
    let image_url = |field: &str| {
        member[field].as_str().filter(|s| !s.is_empty()).map(|hash| {
            store::guild_member_profile_avatar_url(
                guild,
                store.me.as_ref().map(|m| m.id.as_str()).unwrap_or_default(),
                hash,
            )
        })
    };
    godwit_core::own_profile::OwnProfile {
        name: name.into(),
        display_name: member["nick"].as_str().unwrap_or_default().into(),
        pronouns: member["pronouns"].as_str().unwrap_or_default().into(),
        bio: member["bio"].as_str().unwrap_or_default().into(),
        avatar_url: image_url("avatar").map(Into::into),
        banner_url: image_url("banner").map(Into::into),
    }
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

    #[test]
    fn server_profile_body_omits_keep_and_nulls_removals() {
        let edit = ProfileEdit {
            display_name: Some("  Nick  ".into()),
            bio: Some("hello".into()),
            avatar: ProfileImage::Remove,
            banner: ProfileImage::Remove,
            ..ProfileEdit::default()
        };
        let (body, avatar) = server_profile_request(edit).unwrap();
        assert_eq!(body["nick"], "Nick");
        assert_eq!(body["bio"], "hello");
        assert_eq!(body["avatar"], Value::Null);
        assert_eq!(body["banner"], Value::Null);
        assert!(body.get("pronouns").is_none());
        assert!(avatar.is_none());
        let (empty, _) = server_profile_request(ProfileEdit::default()).unwrap();
        assert!(empty.as_object().is_some_and(serde_json::Map::is_empty));
    }

    #[test]
    fn server_profile_images_choose_the_correct_patch_field() {
        let edit = ProfileEdit {
            avatar: ProfileImage::File {
                path: "/tmp/a.png".into(),
                size: 3,
            },
            ..ProfileEdit::default()
        };
        let (body, path) = server_profile_request(edit).unwrap();
        assert_eq!(body["__image_field"], "avatar");
        assert_eq!(path.as_deref(), Some(std::path::Path::new("/tmp/a.png")));
        let both = ProfileEdit {
            avatar: ProfileImage::File {
                path: "/tmp/a.png".into(),
                size: 3,
            },
            banner: ProfileImage::File {
                path: "/tmp/b.png".into(),
                size: 3,
            },
            ..ProfileEdit::default()
        };
        assert!(server_profile_request(both).is_err());
    }

    #[test]
    fn parses_guild_member_profile_and_uses_guild_cdn_paths() {
        let value = json!({
            "guild_member_profile": {"nick":"Local", "avatar":"hash-a", "banner":"hash-b", "bio":"Guild bio", "pronouns":"they/them"},
            "user": {"global_name":"Global", "username":"user"}
        });
        let parsed = server_profile_of(&value, "123", &store::Store::default());
        assert_eq!(parsed.display_name.as_ref(), "Local");
        assert_eq!(parsed.bio.as_ref(), "Guild bio");
        assert_eq!(parsed.pronouns.as_ref(), "they/them");
        assert!(
            parsed
                .avatar_url
                .as_deref()
                .is_some_and(|u| u.contains("/guilds/123/users/"))
        );
        assert!(
            parsed
                .banner_url
                .as_deref()
                .is_some_and(|u| u.contains("/guilds/123/users/"))
        );
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
