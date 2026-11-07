// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! One Stoat account as Godwit's core sees it.
//!
//! [`StoatAccount`] owns the network thread ([`crate::backend`]) and the
//! state [`Store`]. It turns backend events into core [`Event`]s and core
//! [`Command`]s into backend requests. Signing in is Stoat-specific, so the UI
//! drives it through [`StoatAccount::log_in`] and [`StoatAccount::status`].

mod calls;
mod commands;
mod events;
mod messages;
mod people;
mod rooms;
mod server_admin;
mod sign_in;

use crate::backend::{
    ChannelCmd, MediaCmd, MemberCmd, MessageCmd, PeopleCmd, RoleCmd, RoleEditCmd, ServerCmd, SessionCmd, SettingsCmd,
    VoiceCmd as VoiceCommand,
};
use std::collections::HashMap;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use godwit_core::account::NetworkAccount;
use godwit_core::application::{AlertKind, Command, Event, Person};
use godwit_core::call::CallMember;
use godwit_core::ids::{AccountId, MessageId, RoomId, ServerId, UserId};
use godwit_core::members::{Member, MemberGroup, Presence, Profile, Relationship};
use godwit_core::message::{
    Attachment, AttachmentKind, CustomEmoji, EmojiGroup, Markup, Message as CoreMessage, Reaction,
};
use godwit_core::network::Network;
use godwit_core::own_profile::OwnProfile;
use godwit_core::room::{Capabilities, Room, RoomEncryption, RoomKind};
use godwit_core::server::{Category as CoreCategory, Server as CoreServer};
use godwit_core::server_settings::{ServerAdmin, ServerAdminResult, ServerRoles};
use godwit_voice::call::{AudioDevices, VoiceCmd, VoiceEvent, VoiceSettings};
use serde_json::Value;

use crate::api::{Channel, DEFAULT_API, File, MemberId, Message, ReplyIntent, RootConfig, ulid_timestamp_ms};
use crate::backend::{self, Cmd, Ev, Handle, RelationshipAction, Upload, UploadSource, Waker};
use crate::permissions::{
    CONNECT, MANAGE_MESSAGES, REACT, SEND_EMBEDS, SEND_MESSAGE, UPLOAD_FILES, USE_EXTERNAL_EMOJIS, VIEW_CHANNEL,
};
use crate::rich::{self, Resolve};
use crate::session_file;
use crate::store::{RailEntry, Store};

#[path = "account_autodelete.rs"]
mod autodelete_page;

/// Where signing in stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Show the login form; `error` says why the last attempt failed.
    SignedOut {
        error: Option<String>,
    },
    /// A login request is out.
    LoggingIn,
    /// The account has two-factor auth: ask for a code (or recovery code).
    NeedsMfa {
        error: Option<String>,
    },
    /// Signed in; (re)connecting the events socket.
    Connecting,
    Ready,
}

pub struct StoatAccount {
    /// Where its session is saved (`session_file`'s slot).
    slot: u32,
    /// Auto-delete's rules, index and run (a Godwit extra, from Godwit).
    autodelete: autodelete_page::AutoDeleteRun,
    net: Handle,
    events: mpsc::Receiver<Ev>,
    store: Store,
    config: Option<RootConfig>,
    status: Status,
    mfa_ticket: Option<String>,
    account: Option<AccountId>,
    /// The rooms, servers and friends last sent to the core, to send only changes.
    friends: Vec<Person>,
    /// Invite previews read this session, by code (None: not valid).
    invites: HashMap<String, Option<godwit_core::message::InvitePreview>>,
    /// Pending requests last sent to the core: incoming, outgoing.
    requests: (Vec<Person>, Vec<Person>),
    rooms: godwit_core::room_sync::RoomSync,
    servers: Vec<CoreServer>,
    /// The channel on screen (core `Focus`).
    viewing: Option<String>,
    /// Keep the session in `session.json` (off in tests).
    persist_session: bool,
    /// When we last told each channel we're typing.
    typing_sent: HashMap<String, Instant>,
    /// Desktop notifications for new messages (the official setting's default: on).
    pub notifications: bool,
    /// Taken offline by the user (signed in, live connection closed).
    offline: bool,
    /// Microphone processing, devices and volumes for calls.
    pub voice_settings: VoiceSettings,
    /// Our call as LiveKit reports it.
    call: Option<LiveCall>,
    next_call: u64,
    /// Who is in each channel's call, as last sent to the core.
    occupants: HashMap<String, Vec<CallMember>>,
    /// The microphones, speakers and cameras last listed.
    devices: Option<AudioDevices>,
    /// The channel whose member list is shown, and what was last sent for it.
    members_room: Option<String>,
    members_sent: Option<Vec<MemberGroup>>,
    /// The server whose Roles page is open, and the roles last sent for it:
    /// role changes arrive by websocket, so they're sent again as they do.
    roles_watch: Option<(String, ServerRoles)>,
    /// The server a requested invite or new channel is for: their answers
    /// don't say.
    invite_for: Option<String>,
    session_revoke_pending: bool,
    channel_for: Option<String>,
    /// The open profile card: the user and the server it's seen from.
    profile_for: Option<(String, Option<String>, Option<String>)>,
    group_member_pending: Option<GroupMemberAction>,
    /// Custom emoji as last sent to the core.
    /// Our presence as last sent to the core.
    own_presence: Option<Presence>,
    /// Our status text as last sent to the core.
    own_status: Option<Arc<str>>,
    emoji_sent: Option<Vec<EmojiGroup>>,
}

enum GroupMemberAction {
    Remove { channel: String, user: String },
    Transfer { channel: String, user: String },
}

/// Servers over this many members only fetch who's online, as officially.
const LARGE_SERVER: u64 = 1000;

/// The call we're in, as the voice backend reports it.
struct LiveCall {
    generation: u64,
    channel: String,
    participants: Vec<String>,
    speaking: Vec<String>,
    muted: HashMap<String, bool>,
    video_sids: Vec<String>,
    /// Every video in the call, as LiveKit announced them.
    videos: Vec<godwit_voice::call::VideoTrackInfo>,
    /// Whose screen shares we chose to watch: as Discord's Go Live, a
    /// screen share is fetched only after "Watch" (cameras come by themselves).
    watching: std::collections::HashSet<String>,
}

/// How often "typing" is sent while someone keeps typing.
const TYPING_EVERY: Duration = Duration::from_secs(3);

/// Names for the ids a message mentions, as seen from its server.
struct Names<'a> {
    store: &'a Store,
    server: Option<&'a str>,
    /// Where custom emoji pictures are (autumn), once the config is known.
    autumn: Option<&'a str>,
}

impl Resolve for Names<'_> {
    fn user(&self, id: &str) -> String {
        self.store.display_name(id, self.server)
    }

    fn channel(&self, id: &str) -> String {
        self.store
            .channels
            .get(id)
            .map_or_else(|| "unknown-channel".into(), |c| self.store.channel_title(c))
    }

    fn role(&self, id: &str) -> String {
        self.server
            .and_then(|s| self.store.servers.get(s))
            .and_then(|s| s.roles.get(id))
            .map_or_else(|| "unknown-role".into(), |r| r.name.clone())
    }

    fn emoji(&self, id: &str) -> String {
        self.store
            .emojis
            .get(id)
            .map_or_else(|| id.to_owned(), |e| e.name.clone())
    }

    fn emoji_url(&self, id: &str) -> Option<String> {
        Some(format!("{}/emojis/{id}", self.autumn?))
    }

    fn timestamp(&self, secs: i64, style: Option<char>) -> String {
        crate::timefmt::timestamp(secs, style)
    }
}

/// Websocket events that can't change any room or server.
const QUIET_EVENTS: &[&str] = &[
    "Pong",
    "Authenticated",
    "ChannelStartTyping",
    "ChannelStopTyping",
    "MessageReact",
    "MessageUnreact",
    "MessageRemoveReaction",
];

/// Whether `content` has a custom emoji (`:` + a 26-character id + `:`, as
/// Stoat writes them), or with `only`, that one.
fn has_custom_emoji(content: &str, only: Option<&str>) -> bool {
    let bytes = content.as_bytes();
    let mut i = 0;
    while let Some(at) = bytes.get(i..).and_then(|b| b.iter().position(|&c| c == b':')) {
        let start = i + at + 1;
        if let Some(id) = content.get(start..start + 26)
            && bytes.get(start + 26) == Some(&b':')
            && id.bytes().all(|c| c.is_ascii_alphanumeric())
            && only.is_none_or(|o| o == id)
        {
            return true;
        }
        i = start;
    }
    false
}

/// Notification bodies are cut to this many characters, as Godwit did.
const NOTIFY_CHARS: usize = 200;

fn parse_embed_colour(value: &str) -> Option<[u8; 3]> {
    let bytes = value.strip_prefix('#').unwrap_or(value).as_bytes();
    if bytes.len() != 6 {
        return None;
    }
    let byte = |i| (bytes[i] as char).to_digit(16).map(|v| v as u8);
    Some([
        byte(0)? * 16 + byte(1)?,
        byte(2)? * 16 + byte(3)?,
        byte(4)? * 16 + byte(5)?,
    ])
}

/// Converts Stoat's `special` provider descriptor to its official player URL.
/// Provider IDs are constrained before interpolation, so malformed payloads
/// cannot escape the provider URL or inject query parameters.
fn special_player(special: &Value, url: Option<&str>) -> Option<String> {
    let kind = special.get("type")?.as_str()?;
    let id = special.get("id").and_then(Value::as_str).unwrap_or_default();
    let valid_id =
        |s: &str| !s.is_empty() && s.len() <= 128 && s.chars().all(|c| c.is_ascii_alphanumeric() || "_-".contains(c));
    let content_type = special.get("content_type").and_then(Value::as_str).unwrap_or_default();
    Some(match kind {
        "YouTube" if valid_id(id) => {
            let timestamp = special
                .get("timestamp")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty() && s.len() <= 12 && s.chars().all(|c| c.is_ascii_digit()));
            format!(
                "https://www.youtube-nocookie.com/embed/{id}?modestbranding=1{}",
                timestamp.map_or_else(String::new, |t| format!("&start={t}"))
            )
        }
        "Twitch" if valid_id(id) && matches!(content_type, "Channel" | "Video" | "Clip") => {
            format!(
                "https://player.twitch.tv/?{}={id}&parent=stoat.chat&autoplay=false",
                content_type.to_ascii_lowercase()
            )
        }
        "Lightspeed" if valid_id(id) => format!("https://new.lightspeed.tv/embed/{id}/stream"),
        "Spotify" if valid_id(id) && valid_id(content_type) => {
            format!("https://open.spotify.com/embed/{content_type}/{id}")
        }
        "Streamable" if valid_id(id) => format!("https://streamable.com/e/{id}?loop=0"),
        "Soundcloud" => {
            let url = url?;
            let parsed = reqwest::Url::parse(url).ok()?;
            if parsed.scheme() != "https"
                || !matches!(parsed.host_str(), Some("soundcloud.com" | "www.soundcloud.com"))
                || url.len() > 2048
            {
                return None;
            }
            format!(
                "https://w.soundcloud.com/player/?url={}&color=%23FF7F50&auto_play=false&hide_related=false&show_comments=true&show_user=true&show_reposts=false&show_teaser=true&visual=true",
                crate::api::encode_uri_component(url)
            )
        }
        "Bandcamp" if valid_id(id) && valid_id(content_type) => format!(
            "https://bandcamp.com/EmbeddedPlayer/{}={id}/size=large/bgcol=181a1b/linkcol=056cc4/tracklist=false/transparent=true/",
            content_type.to_ascii_lowercase()
        ),
        _ => return None,
    })
}

/// Invite codes in a message's text, as the official client finds them
/// A ULID's time (its first ten Crockford base32 characters), in Unix
/// milliseconds; 0 for anything else.
fn ulid_ms(id: &str) -> i64 {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let Some(time) = id.get(..10) else { return 0 };
    time.bytes()
        .try_fold(0i64, |acc, b| {
            let digit = ALPHABET.iter().position(|&a| a == b.to_ascii_uppercase())?;
            Some(acc * 32 + i64::try_from(digit).ok()?)
        })
        .unwrap_or(0)
}

/// DataCreateRole/DataEditRole: a name of 1 to 32 characters.
fn role_name(name: &str) -> Result<(), &'static str> {
    match name.chars().count() {
        0 => Err("A role needs a name."),
        1..=32 => Ok(()),
        _ => Err("A role name can be at most 32 characters."),
    }
}

/// `BanListResult` as rows: user id, `name#discriminator` (the id if the
/// user object is missing), avatar and reason.
fn ban_list(data: &Value, avatar: impl Fn(&Value) -> Option<String>) -> Vec<(String, String, Option<String>, String)> {
    let users = data
        .get("users")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let bans = data
        .get("bans")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    bans.iter()
        .filter_map(|ban| {
            let id = ban.pointer("/_id/user")?.as_str()?;
            let user = users.iter().find(|u| u.get("_id").and_then(Value::as_str) == Some(id));
            let field = |k| user.and_then(|u| u.get(k)).and_then(Value::as_str).unwrap_or_default();
            let name = match user {
                Some(_) => format!("{}#{}", field("username"), field("discriminator")),
                None => id.to_owned(),
            };
            let reason = ban.get("reason").and_then(Value::as_str).unwrap_or_default();
            Some((id.to_owned(), name, user.and_then(&avatar), reason.to_owned()))
        })
        .collect()
}

/// (markdown/plugins/anchors.tsx): `stt.gg/CODE` and `rvlt.gg/CODE`, and
/// `/invite/CODE` on Stoat's (and Revolt's) own origins. Only plain links get
/// a card, not `[named](links)`. At most 5, each once.
fn invite_codes(content: &str) -> Vec<String> {
    const ORIGINS: [&str; 5] = [
        "https://stoat.chat",
        "https://beta.stoat.chat",
        "https://old.stoat.chat",
        "https://revolt.chat",
        "https://app.revolt.chat",
    ];
    let code_ok =
        |c: &str| !c.is_empty() && c.len() <= 64 && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    let mut codes = Vec::new();
    for word in content.split_whitespace() {
        // Named links (`[text](url)`) and code aren't plain links.
        if word.contains("](") || word.contains('`') {
            continue;
        }
        let word = word
            .trim_start_matches('<')
            .trim_end_matches(['>', '.', ',', ')', '!', '?']);
        let Ok(url) = reqwest::Url::parse(word) else { continue };
        let origin = url.origin().ascii_serialization();
        let path = url.path().trim_end_matches('/');
        let code = if origin == "https://stt.gg" || origin == "https://rvlt.gg" {
            path.strip_prefix('/').filter(|c| !c.contains('/'))
        } else if ORIGINS.contains(&origin.as_str()) {
            path.strip_prefix("/invite/")
        } else {
            None
        };
        if let Some(code) = code.filter(|c| code_ok(c))
            && !codes.iter().any(|c| c == code)
        {
            codes.push(code.to_owned());
        }
        if codes.len() == 5 {
            break;
        }
    }
    codes
}

/// A gifbox result as the picker shows it: the small looping preview
/// (`tinywebm`, as the official grid), else the full one; none without either.
fn gif(r: crate::api::GifResult) -> Option<godwit_core::gifs::Gif> {
    let media = r
        .media_formats
        .get("tinywebm")
        .or_else(|| r.media_formats.get("webm"))?;
    let size = |i: usize| {
        media
            .dimensions
            .get(i)
            .and_then(|v| u32::try_from(*v).ok())
            .unwrap_or(0)
    };
    Some(godwit_core::gifs::Gif {
        url: r.url.into(),
        preview: media.url.as_str().into(),
        width: size(0),
        height: size(1),
    })
}

/// The invite code in what someone pasted into "Join a server": an invite
/// link (as `invite_codes` reads them) or the bare code.
pub fn invite_code(pasted: &str) -> Option<String> {
    let pasted = pasted.trim();
    if let Some(code) = invite_codes(pasted).into_iter().next() {
        return Some(code);
    }
    (!pasted.is_empty() && pasted.len() <= 64 && pasted.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .then(|| pasted.to_owned())
}

/// gifbox's WebM for one of its MP4 videos (`rpc.gifbox.me/media/post/{id}/mp4`):
/// the same clip in VP9, which Godwit decodes itself (godwit-media).
fn gifbox_webm(video: &str) -> Option<String> {
    let base = video.strip_prefix("https://rpc.gifbox.me/media/post/")?;
    let id = base.strip_suffix("/mp4")?;
    (!id.is_empty() && !id.contains('/')).then(|| format!("https://rpc.gifbox.me/media/post/{id}/webm"))
}

/// Whether `url`'s origin is exactly `origin`.
fn has_origin(url: &str, origin: &str) -> bool {
    url.strip_prefix(origin)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '?', '#']))
}

/// The official `isGif`: GIF special content, or a GIF provider's page or
/// media. The official list (`GIF_PROVIDERS`: tenor.com, giphy.com,
/// gifbox.me, exact origins) misses their media hosts (media.tenor.com,
/// i.giphy.com) and other services; core's list covers those.
fn is_gif(embed: &crate::api::Embed) -> bool {
    use crate::api::Embed as StoatEmbed;
    let url = match embed {
        StoatEmbed::Website {
            special,
            original_url,
            url,
            ..
        } => {
            if special.as_ref().and_then(|s| s.get("type")).and_then(Value::as_str) == Some("GIF") {
                return true;
            }
            original_url.as_deref().or(url.as_deref())
        }
        StoatEmbed::Image(i) => Some(i.url.as_str()),
        StoatEmbed::Video(v) => Some(v.url.as_str()),
        _ => None,
    };
    url.is_some_and(godwit_core::gifs::is_gif_link)
}

impl StoatAccount {
    /// Starts the network thread and, if a session was saved in `slot`
    /// (see `session_file`), signs in with it.
    pub fn start(wake: Waker, slot: u32) -> std::io::Result<Self> {
        let (net, events) = backend::spawn(wake)?;
        let status = match session_file::load(slot) {
            Some(session) => {
                net.send(Cmd::Session(SessionCmd::UseSession(session)));
                Status::Connecting
            }
            None => Status::SignedOut { error: None },
        };
        Ok(Self {
            slot,
            net,
            events,
            store: Store::default(),
            config: None,
            status,
            mfa_ticket: None,
            account: None,
            friends: Vec::new(),
            requests: (Vec::new(), Vec::new()),
            invites: HashMap::new(),
            rooms: Default::default(),
            servers: Vec::new(),
            viewing: None,
            persist_session: true,
            autodelete: Default::default(),
            typing_sent: HashMap::new(),
            notifications: true,
            offline: false,
            voice_settings: VoiceSettings::default(),
            call: None,
            next_call: 0,
            occupants: HashMap::new(),
            devices: None,
            members_room: None,
            members_sent: None,
            roles_watch: None,
            invite_for: None,
            session_revoke_pending: false,
            channel_for: None,
            profile_for: None,
            group_member_pending: None,
            emoji_sent: None,
            own_presence: None,
            own_status: None,
        })
    }

    /// Where its session is saved.
    pub fn slot(&self) -> u32 {
        self.slot
    }

    /// An account that never connects (demo mode): no network thread, and
    /// the saved session isn't read. Signed out, and it stays so.
    pub fn inert() -> Self {
        let (net, _) = Handle::detached();
        let (_, events) = mpsc::channel();
        Self {
            slot: 0,
            net,
            events,
            store: Store::default(),
            config: None,
            status: Status::SignedOut { error: None },
            mfa_ticket: None,
            account: None,
            friends: Vec::new(),
            requests: (Vec::new(), Vec::new()),
            invites: HashMap::new(),
            rooms: Default::default(),
            servers: Vec::new(),
            viewing: None,
            persist_session: false,
            autodelete: Default::default(),
            typing_sent: HashMap::new(),
            notifications: false,
            offline: false,
            voice_settings: VoiceSettings::default(),
            call: None,
            next_call: 0,
            occupants: HashMap::new(),
            devices: None,
            members_room: None,
            members_sent: None,
            roles_watch: None,
            invite_for: None,
            session_revoke_pending: false,
            channel_for: None,
            profile_for: None,
            group_member_pending: None,
            emoji_sent: None,
            own_presence: None,
            own_status: None,
        }
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    fn room_id(&self, channel: &str) -> Option<RoomId> {
        Some(RoomId::new(self.account.clone()?, channel))
    }
}

impl NetworkAccount for StoatAccount {
    fn can_edit_profile(&self) -> bool {
        true
    }
    fn supports_invites(&self) -> bool {
        true
    }

    fn parse_invite(&self, input: &str) -> Option<String> {
        invite_code(input)
    }

    fn network(&self) -> Network {
        Network::Stoat
    }

    fn id(&self) -> Option<&AccountId> {
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
        self.net.send(Cmd::Session(SessionCmd::SetOffline(offline)));
        if offline && let Some(account) = self.account.clone() {
            out.push(Event::Connection { account, online: false });
        }
        out
    }

    fn connecting(&self) -> bool {
        self.id().is_none() && matches!(self.status(), Status::Connecting)
    }

    fn poll(&mut self) -> Vec<Event> {
        self.poll_events()
    }

    /// "Watch" on a screen share in our call, as Discord's Go Live: only
    /// then is it fetched and shown.
    fn watch_stream(&mut self, room: &RoomId, user: &UserId, watch: bool) -> Vec<Event> {
        let mut out = Vec::new();
        let Some(call) = self.call.as_mut().filter(|c| *c.channel == *room.raw) else {
            return out;
        };
        let who = user.raw.to_string();
        let changed = if watch {
            call.watching.insert(who)
        } else {
            call.watching.remove(&who)
        };
        if changed {
            self.sync_call_videos(&mut out);
        }
        out
    }

    fn run(&mut self, command: Command) -> Vec<Event> {
        self.run_command(command)
    }

    /// A message's link in the official web app.
    fn message_link(&self, room: &RoomId, message: &MessageId) -> Option<String> {
        let server = self.store.channels.get(&*room.raw).and_then(|c| c.server.as_deref());
        let server = server.map(|s| format!("/server/{s}")).unwrap_or_default();
        Some(format!(
            "https://stoat.chat/app{server}/channel/{}/{}",
            room.raw, message.0
        ))
    }

    fn log_out(&mut self) {
        self.net.send(Cmd::Session(SessionCmd::Logout));
    }
}

/// Separates a search request's key parts (kind, channel, query): never typed.
const KEY_SEP: char = '\u{1f}';

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::sync::mpsc as tmpsc;

    use super::*;
    use crate::backend::AutoDeleteCmd;

    /// An account fed by hand: no network thread, no session file.
    fn offline() -> (StoatAccount, mpsc::Sender<Ev>, tmpsc::UnboundedReceiver<Cmd>) {
        let (net, cmds) = Handle::detached();
        let (tx, events) = mpsc::channel();
        let account = StoatAccount {
            slot: 0,
            net,
            events,
            store: Store::default(),
            config: serde_json::from_value(json!({
                "ws": "wss://example.invalid",
                "features": { "autumn": { "url": "https://files.example" } }
            }))
            .ok(),
            status: Status::SignedOut { error: None },
            mfa_ticket: None,
            account: None,
            friends: Vec::new(),
            requests: (Vec::new(), Vec::new()),
            invites: HashMap::new(),
            rooms: Default::default(),
            servers: Vec::new(),
            viewing: None,
            persist_session: false,
            autodelete: Default::default(),
            typing_sent: HashMap::new(),
            notifications: false,
            offline: false,
            voice_settings: VoiceSettings::default(),
            call: None,
            next_call: 0,
            occupants: HashMap::new(),
            devices: None,
            members_room: None,
            members_sent: None,
            roles_watch: None,
            invite_for: None,
            session_revoke_pending: false,
            channel_for: None,
            profile_for: None,
            group_member_pending: None,
            emoji_sent: None,
            own_presence: None,
            own_status: None,
        };
        (account, tx, cmds)
    }

    fn ready() -> Ev {
        let ready = json!({
            "users": [
                { "_id": "ME", "username": "me", "display_name": "Me Myself", "relationship": "User" },
                { "_id": "ANN", "username": "ann", "display_name": "Ann" }
            ],
            "servers": [{
                "_id": "S", "name": "Server", "owner": "ME", "channels": ["C1", "C2"],
                "categories": [{ "id": "cat", "title": "Talk", "channels": ["C2"] }]
            }],
            "channels": [
                { "_id": "N", "channel_type": "SavedMessages", "user": "ME" },
                { "_id": "D", "channel_type": "DirectMessage", "recipients": ["ME", "ANN"], "active": true },
                { "_id": "C1", "channel_type": "TextChannel", "server": "S", "name": "general" },
                { "_id": "C2", "channel_type": "TextChannel", "server": "S", "name": "chat" }
            ],
            "members": [{ "_id": { "server": "S", "user": "ME" } }]
        });
        Ev::Ready(serde_json::from_value(ready).unwrap())
    }

    fn message(id: &str, channel: &str, content: &str) -> Value {
        json!({ "_id": id, "channel": channel, "author": "ANN", "content": content })
    }

    fn connected() -> (
        StoatAccount,
        mpsc::Sender<Ev>,
        tmpsc::UnboundedReceiver<Cmd>,
        Vec<Event>,
    ) {
        let (mut a, tx, cmds) = offline();
        tx.send(ready()).unwrap();
        let events = a.poll();
        (a, tx, cmds, events)
    }

    #[test]
    fn stoat_sessions_load_from_the_official_session_endpoint() {
        let (mut a, _tx, mut cmds, _) = connected();
        while cmds.try_recv().is_ok() {}
        a.run(Command::LoadSessions);
        assert!(matches!(cmds.try_recv(), Ok(Cmd::Settings(SettingsCmd::FetchSessions))));
    }

    #[test]
    fn stoat_session_revocation_requires_and_forwards_the_selected_mfa_proof() {
        let (mut a, _tx, mut cmds, _) = connected();
        while cmds.try_recv().is_ok() {}
        a.run(Command::LogOutSessionWithProof {
            target: godwit_core::privacy::SessionTarget::One("01ARZ3NDEKTSV4RRFFQ69G5FAV".into()),
            proof: godwit_core::privacy::SessionProof::Authenticator(godwit_core::application::Secret("123456".into())),
        });
        let Ok(Cmd::Settings(SettingsCmd::RevokeSessions { target, response })) = cmds.try_recv() else {
            panic!("session revocation must use the MFA-gated API path");
        };
        assert!(matches!(target, crate::backend::RevokeTarget::One(id) if id == "01ARZ3NDEKTSV4RRFFQ69G5FAV"));
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({ "totp_code": "123456" })
        );
    }

    #[test]
    fn stoat_session_rename_uses_the_official_friendly_name_request() {
        let (mut a, _tx, mut cmds, _) = connected();
        while cmds.try_recv().is_ok() {}
        a.run(Command::RenameSession {
            session: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            name: "Work laptop".into(),
        });
        let Ok(Cmd::Settings(SettingsCmd::RenameSession { id, name })) = cmds.try_recv() else {
            panic!("session rename must use Stoat's official settings command");
        };
        assert_eq!(id, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
        assert_eq!(name, "Work laptop");
    }

    #[test]
    fn stoat_bulk_session_revocation_excludes_the_current_device() {
        let (mut a, _tx, mut cmds, _) = connected();
        while cmds.try_recv().is_ok() {}
        let events = a.run(Command::LogOutSessionWithProof {
            target: godwit_core::privacy::SessionTarget::Others,
            proof: godwit_core::privacy::SessionProof::Password(godwit_core::application::Secret("password".into())),
        });
        assert!(matches!(events.as_slice(), [Event::SessionActionPending { .. }]));
        let Ok(Cmd::Settings(SettingsCmd::RevokeSessions { target, response })) = cmds.try_recv() else {
            panic!("bulk revocation must use the MFA-gated API path");
        };
        assert!(matches!(target, crate::backend::RevokeTarget::Others));
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({ "password": "password" })
        );
    }

    fn room(raw: &str) -> RoomId {
        RoomId::new(AccountId::new(Network::Stoat, "ME"), raw)
    }

    #[test]
    fn pins_and_search_go_through_stoats_search() {
        let (mut a, tx, mut cmds, _) = connected();
        while cmds.try_recv().is_ok() {}
        a.run(Command::SetPinned {
            room: room("D"),
            message: MessageId("01M".into()),
            pinned: false,
        });
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Message(MessageCmd::Pin { channel, id, pin: false }))
            if channel == "D" && id == "01M")
        );
        a.run(Command::LoadPins { room: room("D") });
        let Ok(Cmd::Message(MessageCmd::SearchMessages { key, body, .. })) = cmds.try_recv() else {
            panic!("pins ask Stoat's search")
        };
        assert_eq!(body["pinned"], json!(true));
        let message = json!({ "_id": "01M", "channel": "D", "author": "ANN", "content": "pinned!" });
        tx.send(Ev::SearchResults {
            key,
            result: Ok(serde_json::from_value(json!({ "messages": [message], "users": [] })).unwrap()),
        })
        .unwrap();
        let events = a.poll();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Pins { messages: Some(m), .. } if m.len() == 1))
        );
        a.run(Command::SearchMessages {
            room: room("D"),
            query: "lunch".into(),
        });
        let Ok(Cmd::Message(MessageCmd::SearchMessages { key, body, .. })) = cmds.try_recv() else {
            panic!("a search")
        };
        assert_eq!(body["query"], json!("lunch"));
        tx.send(Ev::SearchResults {
            key,
            result: Ok(serde_json::from_value(json!({ "messages": [], "users": [] })).unwrap()),
        })
        .unwrap();
        let events = a.poll();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::SearchResults { query, .. } if query == "lunch"))
        );
    }

    #[test]
    fn reaction_moderation_routes_one_emoji_or_all_to_stoat_api_commands() {
        let (mut a, _tx, mut cmds, _) = connected();
        while cmds.try_recv().is_ok() {}
        a.run(Command::RemoveReactions {
            room: room("D"),
            message: MessageId("M".into()),
            emoji: Some("👍".into()),
        });
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Message(MessageCmd::RemoveReaction { channel, id, emoji }))
            if channel == "D" && id == "M" && emoji == "👍")
        );

        a.run(Command::RemoveReactions {
            room: room("D"),
            message: MessageId("M".into()),
            emoji: None,
        });
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Message(MessageCmd::ClearReactions { channel, id }))
            if channel == "D" && id == "M")
        );
    }

    #[test]
    fn ready_becomes_an_account_with_rooms_and_servers() {
        let (a, _tx, mut cmds, events) = connected();
        assert_eq!(a.status(), &Status::Ready);
        // The account, then our own presence (Online until we set one).
        let [
            Event::AccountReady {
                account,
                display_name,
                rooms,
                servers,
                ..
            },
            Event::OwnPresence {
                presence: Presence::Online,
                ..
            },
        ] = &events[..]
        else {
            panic!("expected AccountReady then OwnPresence, got {events:?}");
        };
        assert_eq!(&*account.user, "ME");
        assert_eq!(&**display_name, "Me Myself");
        let names: Vec<&str> = rooms.iter().map(|r| &*r.name).collect();
        assert_eq!(names, ["Saved Notes", "Ann", "general", "chat"]);
        assert!(matches!(&rooms[1].kind, RoomKind::Direct { with } if &*with.raw == "ANN"));
        assert_eq!(
            rooms[1].icon_url.as_deref(),
            Some("https://stoat.chat/api/users/ANN/default_avatar")
        );
        assert_eq!(rooms[2].encryption, RoomEncryption::Unencrypted);
        assert!(rooms[2].capabilities.send, "the owner can send");

        let [server] = &servers[..] else { panic!("one server") };
        let cats: Vec<(&str, Vec<&str>)> = server
            .categories
            .iter()
            .map(|c| (&*c.title, c.rooms.iter().map(|r| &*r.raw).collect()))
            .collect();
        assert_eq!(cats, [("", vec!["C1"]), ("Talk", vec!["C2"])]);

        // One unread sync and one settings fetch per connect, as officially.
        assert!(matches!(cmds.try_recv(), Ok(Cmd::Message(MessageCmd::FetchUnreads))));
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Settings(SettingsCmd::FetchSyncedSettings))
        ));
    }

    #[test]
    fn a_history_page_arrives_oldest_first() {
        let (mut a, tx, _cmds, _) = connected();
        let data = json!({ "messages": [message("02", "C1", "second"), message("01", "C1", "first")] });
        tx.send(Ev::Messages {
            channel: "C1".into(),
            before: None,
            data: serde_json::from_value(data).unwrap(),
        })
        .unwrap();
        let events = a.poll();
        let [
            Event::History {
                room: r,
                before: None,
                messages,
                reached_start: true,
            },
        ] = &events[..]
        else {
            panic!("expected a full first page, got {events:?}");
        };
        assert_eq!(r, &room("C1"));
        let texts: Vec<&str> = messages.iter().map(|m| &*m.text).collect();
        assert_eq!(texts, ["first", "second"]);
        assert_eq!(&*messages[0].author_name, "Ann");
        assert_eq!(messages[0].markup, Markup::Rich);
    }

    #[test]
    fn custom_emoji_are_found_in_message_text() {
        let id = "01ABCDEFGHJKMNPQRSTVWXYZ00";
        assert!(has_custom_emoji(&format!("hi :{id}: there"), None));
        assert!(has_custom_emoji(&format!(":{id}:"), Some(id)));
        assert!(!has_custom_emoji(&format!(":{id}:"), Some("01OTHER")));
        assert!(!has_custom_emoji("a :smile: b :: c", None));
        assert!(!has_custom_emoji("time 12:30:00", None));
    }

    #[test]
    fn messages_with_custom_emoji_convert_again_once_the_config_arrives() {
        let (mut a, tx, _cmds, _) = connected();
        let config = a.config.take();
        let id = "01ABCDEFGHJKMNPQRSTVWXYZ00";
        let data = json!({ "messages": [message("01", "C1", &format!("look :{id}:")), message("02", "C1", "plain")] });
        tx.send(Ev::Messages {
            channel: "C1".into(),
            before: None,
            data: serde_json::from_value(data).unwrap(),
        })
        .unwrap();
        let events = a.poll();
        let Some(Event::History { messages, .. }) = events.first() else {
            panic!("expected a page, got {events:?}");
        };
        assert!(!messages[0].text.contains(godwit_core::rich::EMOJI), "no pictures yet");

        tx.send(Ev::Config(config.unwrap())).unwrap();
        let events = a.poll();
        let mut updated: Vec<&CoreMessage> = Vec::new();
        for e in &events {
            if let Event::MessageUpdated { message, .. } = e {
                updated.push(message);
            }
        }
        assert_eq!(updated.len(), 1, "only the message with an emoji: {events:?}");
        assert!(updated[0].text.contains(godwit_core::rich::EMOJI));
    }

    #[test]
    fn appended_link_previews_reach_the_core() {
        let (mut a, tx, _cmds, _) = connected();
        // An open room: its page is loaded, so its messages are kept.
        let data = json!({ "messages": [message("04", "C1", "https://example.com/cat.gif")] });
        tx.send(Ev::Messages {
            channel: "C1".into(),
            before: None,
            data: serde_json::from_value(data).unwrap(),
        })
        .unwrap();
        a.poll();
        let embed = json!({ "type": "Image", "url": "https://example.com/cat.gif", "width": 10, "height": 10, "size": "Large" });
        tx.send(Ev::Event(json!({
            "type": "MessageAppend", "channel": "C1", "id": "04", "append": { "embeds": [embed] }
        })))
        .unwrap();
        let events = a.poll();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::MessageUpdated { message, .. } if !message.embeds.is_empty())),
            "the preview came after sending: {events:?}"
        );
    }

    #[test]
    fn live_messages_edits_and_deletes_reach_the_core() {
        let (mut a, tx, _cmds, _) = connected();
        let mut m = message("03", "C1", "hello");
        m["type"] = "Message".into();
        tx.send(Ev::Event(m)).unwrap();
        let events = a.poll();
        assert!(events.iter().any(
            |e| matches!(e, Event::MessageAdded { room: r, message } if r == &room("C1") && &*message.text == "hello")
        ));
        tx.send(Ev::Event(
            json!({ "type": "MessageDelete", "channel": "C1", "id": "03" }),
        ))
        .unwrap();
        let events = a.poll();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::MessageDeleted { id, .. } if &*id.0 == "03"))
        );
    }

    #[test]
    fn embeds_keep_card_fields_and_proxy_thumbnail_media() {
        let (a, _tx, _cmds, _) = connected();
        let mut a = a;
        a.config = serde_json::from_value(json!({
            "ws": "wss://example.invalid",
            "features": {
                "autumn": { "url": "https://files.example" },
                "january": { "url": "https://proxy.example", "enabled": true }
            }
        }))
        .ok();
        let mut m: Message = serde_json::from_value(message("08", "C1", "https://site.example/article")).unwrap();
        m.embeds = serde_json::from_value(json!([
            { "type": "Website", "title": "Article", "description": "Preview text",
              "url": "https://site.example/article", "site_name": "Example",
              "colour": "#12aBcD", "special": {"type": "YouTube", "id": "aB_123", "timestamp": "90"},
              "image": {"url": "https://cdn.example/preview.png", "width": 640, "height": 360, "size": "Preview"} },
            { "type": "Text", "title": "Attached image", "media": {
                "_id": "file123", "tag": "attachments", "filename": "card.png", "size": 1234,
                "metadata": {"type": "Image", "width": 320, "height": 180}
            } }
        ]))
        .unwrap();
        let core = a.message(&m);
        assert_eq!(core.embeds.len(), 2);
        let embed = &core.embeds[0];
        assert_eq!(embed.title.as_deref(), Some("Article"));
        assert_eq!(embed.description.as_deref(), Some("Preview text"));
        assert_eq!(embed.site_name.as_deref(), Some("Example"));
        assert_eq!(embed.url.as_deref(), Some("https://site.example/article"));
        assert_eq!(
            embed.player_url.as_deref(),
            Some("https://www.youtube-nocookie.com/embed/aB_123?modestbranding=1&start=90")
        );
        assert_eq!(embed.colour, Some([0x12, 0xab, 0xcd]));
        assert!(embed.image_thumbnail);
        assert_eq!(embed.media_dimensions, Some((640, 360)));
        assert_eq!(
            embed.image_url.as_deref(),
            Some("https://proxy.example/proxy?url=https%3A%2F%2Fcdn.example%2Fpreview.png")
        );
        let attached = &core.embeds[1];
        assert_eq!(attached.title.as_deref(), Some("Attached image"));
        assert_eq!(
            attached.image_url.as_deref(),
            Some("https://files.example/attachments/file123")
        );
        assert_eq!(attached.media_dimensions, Some((320, 180)));
    }

    #[test]
    fn emoji_only_messages_are_shown_large() {
        let (a, _tx, _cmds, _) = connected();
        let m: Message = serde_json::from_value(message("06", "C1", "👍 🎉")).unwrap();
        let jumbo = a.message(&m).jumbo;
        let names: Vec<&str> = jumbo.iter().map(|(_, n)| &**n).collect();
        assert_eq!(names, [":+1:", ":tada:"]);
        let m: Message = serde_json::from_value(message("07", "C1", "nice 👍")).unwrap();
        assert!(a.message(&m).jumbo.is_empty(), "text with emoji stays text");
    }

    #[test]
    fn system_messages_use_the_official_wording() {
        let (a, _tx, _cmds, _) = connected();
        let mut m: Message = serde_json::from_value(message("04", "C1", "")).unwrap();
        m.system = Some(json!({ "type": "user_joined", "id": "ANN" }));
        let core = a.message(&m);
        assert!(core.system);
        assert_eq!(&*core.text, "Ann joined the server");
    }

    #[test]
    fn sends_carry_their_nonce_both_ways() {
        let (mut a, tx, mut cmds, _) = connected();
        let _ = (cmds.try_recv(), cmds.try_recv());
        a.run(Command::SendMessage {
            room: room("C1"),
            text: "hi".into(),
            nonce: "n1".into(),
            reply_to: None,
            files: Vec::new(),
        });
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Message(MessageCmd::Send { channel, nonce, .. })) if channel == "C1" && nonce == "n1")
        );

        tx.send(Ev::Sent {
            channel: "C1".into(),
            nonce: "n1".into(),
            message: serde_json::from_value(message("05", "C1", "hi")).unwrap(),
        })
        .unwrap();
        tx.send(Ev::SendFailed {
            channel: "C1".into(),
            nonce: "n2".into(),
            error: "nope".into(),
        })
        .unwrap();
        let events = a.poll();
        assert!(
            matches!(&events[0], Event::MessageConfirmed { nonce, message, .. } if &**nonce == "n1" && &*message.id.0 == "05")
        );
        assert!(matches!(&events[1], Event::MessageFailed { nonce, .. } if &**nonce == "n2"));
    }

    #[test]
    fn marking_read_acks_and_updates_the_room() {
        let (mut a, _tx, mut cmds, _) = connected();
        let _ = (cmds.try_recv(), cmds.try_recv());
        a.run(Command::MarkRead {
            room: room("C1"),
            last: MessageId("09".into()),
        });
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Message(MessageCmd::Ack { channel, message })) if channel == "C1" && message == "09")
        );
    }

    #[test]
    fn auto_delete_rules_are_saved_shown_and_scheduled() {
        use godwit_core::autodelete::{AutoDeleteCommand, RuleDraft};
        let (mut a, _tx, mut cmds, _) = connected();
        while cmds.try_recv().is_ok() {}
        // Nothing runs, and nothing is sent, before the page opens or a rule exists.
        assert!(
            a.run(Command::AutoDelete(AutoDeleteCommand::Show))
                .iter()
                .any(|e| matches!(e, Event::AutoDelete { view, .. } if view.rules.is_empty()))
        );
        assert!(cmds.try_recv().is_err());
        let out = a.run(Command::AutoDelete(AutoDeleteCommand::Save(RuleDraft {
            name: "Old".into(),
            enabled: true,
            all_private: true,
            text: true,
            links: true,
            media: true,
            older_than_days: Some(90),
            ..RuleDraft::default()
        })));
        let Some(Event::AutoDelete { view, .. }) = out.last() else {
            panic!("no page: {out:?}")
        };
        assert_eq!(view.rules.len(), 1);
        assert_eq!(&*view.rules[0].when_text, "Older than 90 days (keeps going)");
        let mut woken = false;
        while let Ok(cmd) = cmds.try_recv() {
            woken |= matches!(cmd, Cmd::AutoDelete(AutoDeleteCmd::AutoDeleteWake(_)));
        }
        assert!(woken, "the next check is scheduled while a rule exists");
        let id = view.rules[0].draft.id.unwrap();
        let out = a.run(Command::AutoDelete(AutoDeleteCommand::Remove(id)));
        assert!(matches!(out.last(), Some(Event::AutoDelete { view, .. }) if view.rules.is_empty()));
    }

    #[test]
    fn marking_read_acks_a_newer_unshown_message() {
        // The channel's newest message was deleted or isn't shown: read
        // past it, or the chat stays unread for good.
        let (mut a, _tx, mut cmds, _) = connected();
        let _ = (cmds.try_recv(), cmds.try_recv());
        if let Some(c) = a.store.channels.get_mut("C1") {
            c.last_message_id = Some("12".into());
        }
        a.run(Command::MarkRead {
            room: room("C1"),
            last: MessageId("09".into()),
        });
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Message(MessageCmd::Ack { channel, message })) if channel == "C1" && message == "12")
        );
        assert!(!a.store.is_unread("C1"));
    }

    #[test]
    fn embeds_pick_the_official_layout() {
        use godwit_core::message::EmbedStyle;
        let (a, _tx, _cmds, _) = connected();
        let mut a = a;
        a.config = serde_json::from_value(json!({
            "ws": "wss://example.invalid",
            "features": {
                "autumn": { "url": "https://files.example" },
                "january": { "url": "https://proxy.example", "enabled": true }
            }
        }))
        .ok();
        let mut m: Message = serde_json::from_value(message("09", "C1", "links")).unwrap();
        m.embeds = serde_json::from_value(json!([
            { "type": "Website", "url": "https://pics.example/a.png",
              "image": {"url": "https://pics.example/a.png", "width": 10, "height": 10, "size": "Large"} },
            { "type": "Website", "original_url": "https://tenor.com/view/x", "url": "https://tenor.com/view/x",
              "title": "Tenor", "image": {"url": "https://media.tenor.com/x.gif", "width": 4, "height": 3, "size": "Preview"} },
            { "type": "Video", "url": "https://giphy.com/x.mp4", "width": 4, "height": 3 },
            { "type": "Video", "url": "https://tenorish.example/x.mp4" },
            { "type": "Website", "title": "Site", "site_name": "S", "icon_url": "https://s.example/i.png" },
            { "type": "Text", "description": "**bold**" }
        ]))
        .unwrap();
        let core = a.message(&m);
        let styles: Vec<_> = core.embeds.iter().map(|e| e.style).collect();
        assert_eq!(
            styles,
            [
                EmbedStyle::Media,
                EmbedStyle::Gif,
                EmbedStyle::Gif,
                EmbedStyle::Media,
                EmbedStyle::Card,
                EmbedStyle::Card
            ]
        );
        assert!(!core.embeds[1].image_thumbnail, "GIFs are never thumbnails");
        assert_eq!(
            core.embeds[2].video_url.as_deref(),
            Some("https://giphy.com/x.mp4"),
            "GIF videos come straight from the provider"
        );
        assert!(
            core.embeds[3]
                .video_url
                .as_deref()
                .is_some_and(|u| u.starts_with("https://proxy.example/"))
        );
        assert_eq!(
            core.embeds[4].icon_url.as_deref(),
            Some("https://proxy.example/proxy?url=https%3A%2F%2Fs.example%2Fi.png")
        );
        assert_eq!(core.embeds[5].description_markup, Markup::Rich);
        assert_eq!(core.embeds[4].description_markup, Markup::Plain);
    }

    #[test]
    fn custom_status_keeps_the_presence_and_clearing_removes_the_text() {
        let (mut a, _tx, mut cmds, _) = connected();
        while cmds.try_recv().is_ok() {}
        a.own_presence = Some(Presence::Focus);
        a.run(Command::SetStatusText(Some("away for lunch".into())));
        let Ok(Cmd::Settings(SettingsCmd::EditMe(body))) = cmds.try_recv() else {
            panic!("no status sent")
        };
        assert_eq!(
            body,
            json!({ "status": { "text": "away for lunch", "presence": "Focus" } })
        );
        a.run(Command::SetStatusText(None));
        let Ok(Cmd::Settings(SettingsCmd::EditMe(body))) = cmds.try_recv() else {
            panic!("no removal sent")
        };
        assert_eq!(body, json!({ "remove": ["StatusText"] }));
    }

    #[test]
    fn ban_lists_pair_bans_with_their_users() {
        let data = json!({
            "users": [{ "_id": "U1", "username": "quorp", "discriminator": "0420", "avatar": null }],
            "bans": [
                { "_id": { "server": "S", "user": "U1" }, "reason": "spam" },
                { "_id": { "server": "S", "user": "U2" } },
                { "reason": "no id" }
            ]
        });
        let rows = ban_list(&data, |_| None);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], ("U1".into(), "quorp#0420".into(), None, "spam".into()));
        assert_eq!(
            rows[1],
            ("U2".into(), "U2".into(), None, String::new()),
            "a missing user shows its id"
        );
    }

    #[test]
    fn ulids_carry_their_time() {
        // The ULID spec's example: 1469922850259 ms.
        assert_eq!(ulid_ms("01ARZ3NDEKTSV4RRFFQ69G5FAV"), 1_469_922_850_259);
        assert_eq!(ulid_ms("short"), 0);
        assert_eq!(ulid_ms("!!!!!!!!!!XXXX"), 0);
    }

    #[test]
    fn role_names_follow_the_spec() {
        assert!(role_name("").is_err());
        assert!(role_name("x").is_ok());
        assert!(role_name(&"y".repeat(32)).is_ok());
        assert!(role_name(&"z".repeat(33)).is_err());
        assert!(role_name(&"é".repeat(32)).is_ok(), "characters, not bytes");
    }

    #[test]
    fn invite_links_follow_the_official_rules() {
        assert_eq!(
            invite_codes("join https://stt.gg/Testers and <https://stoat.chat/invite/ABC123>!"),
            ["Testers", "ABC123"]
        );
        assert_eq!(
            invite_codes("https://rvlt.gg/Old https://app.revolt.chat/invite/X1/"),
            ["Old", "X1"]
        );
        assert!(
            invite_codes("[named](https://stt.gg/Testers)").is_empty(),
            "named links get no card"
        );
        assert!(invite_codes("https://stt.gg/discover/servers https://evil.example/invite/X").is_empty());
        assert!(invite_codes("https://stoat.chat.evil.example/invite/X http://stt.gg/X").is_empty());
        assert!(invite_codes("`https://stt.gg/Code`").is_empty(), "code isn't a link");
        assert_eq!(invite_codes("https://stt.gg/A https://stt.gg/A").len(), 1);
        assert_eq!(invite_code("  https://stt.gg/Testers "), Some("Testers".into()));
        assert_eq!(invite_code("Testers"), Some("Testers".into()));
        assert_eq!(invite_code("not a code"), None);
        assert_eq!(invite_code("https://example.com/x"), None);
    }

    #[test]
    fn gif_origins_must_match_exactly() {
        assert!(has_origin("https://tenor.com/view/x", "https://tenor.com"));
        assert_eq!(
            gifbox_webm("https://rpc.gifbox.me/media/post/abc/mp4").as_deref(),
            Some("https://rpc.gifbox.me/media/post/abc/webm")
        );
        assert!(gifbox_webm("https://rpc.gifbox.me/media/post/a/b/mp4").is_none());
        assert!(gifbox_webm("https://example.com/media/post/abc/mp4").is_none());
        assert!(has_origin("https://tenor.com", "https://tenor.com"));
        assert!(!has_origin("https://tenor.com.evil.example/x", "https://tenor.com"));
        assert!(!has_origin("https://tenor.community/x", "https://tenor.com"));
    }

    #[test]
    fn embed_colours_accept_hex_and_reject_malformed_values() {
        assert_eq!(parse_embed_colour("#12aBcD"), Some([0x12, 0xab, 0xcd]));
        assert_eq!(parse_embed_colour("ffffff"), Some([255, 255, 255]));
        assert_eq!(parse_embed_colour("#fff"), None);
        assert_eq!(parse_embed_colour("ééé"), None);
        assert_eq!(parse_embed_colour("zzzzzz"), None);
    }

    #[test]
    fn special_embed_players_use_provider_urls_and_reject_unsafe_values() {
        assert_eq!(
            special_player(
                &json!({"type":"Twitch", "id":"channel_1", "content_type":"Channel"}),
                None
            )
            .as_deref(),
            Some("https://player.twitch.tv/?channel=channel_1&parent=stoat.chat&autoplay=false")
        );
        assert_eq!(
            special_player(&json!({"type":"Spotify", "id":"abc123", "content_type":"track"}), None).as_deref(),
            Some("https://open.spotify.com/embed/track/abc123")
        );
        assert!(special_player(&json!({"type":"YouTube", "id":"id?x=1"}), None).is_none());
        assert!(special_player(&json!({"type":"Twitch", "id":"id", "content_type":"foo"}), None).is_none());
        assert!(special_player(&json!({"type":"Soundcloud"}), Some("https://evil.example/path")).is_none());
        assert!(
            special_player(
                &json!({"type":"Soundcloud"}),
                Some("https://soundcloud.com.evil.example/path")
            )
            .is_none()
        );
    }

    #[test]
    fn login_failures_and_mfa_show_on_the_form() {
        let (mut a, tx, _cmds) = offline();
        a.log_in("e@example.org".into(), "pw".into());
        assert_eq!(a.status(), &Status::LoggingIn);
        tx.send(Ev::MfaRequired { ticket: "t".into() }).unwrap();
        a.poll();
        assert_eq!(a.status(), &Status::NeedsMfa { error: None });
        a.submit_mfa("123456".into());
        tx.send(Ev::Error("HTTP 401: invalid code".into())).unwrap();
        a.poll();
        assert!(matches!(a.status(), Status::NeedsMfa { error: Some(_) }));

        let (mut b, tx, _cmds) = offline();
        b.log_in("e@example.org".into(), "wrong".into());
        tx.send(Ev::Error("HTTP 401: invalid credentials".into())).unwrap();
        b.poll();
        assert!(matches!(b.status(), Status::SignedOut { error: Some(_) }));
    }

    #[test]
    fn a_call_reports_connection_members_and_its_end() {
        let (mut a, tx, mut cmds, _) = connected();
        let voice = |event| Ev::Voice {
            generation: 1,
            channel: "D".into(),
            event,
        };
        let _ = (cmds.try_recv(), cmds.try_recv());
        a.run(Command::JoinCall { room: room("D") });
        assert!(matches!(cmds.try_recv(), Ok(Cmd::Voice(VoiceCommand::JoinVoice { channel, .. })) if channel == "D"));

        tx.send(voice(VoiceEvent::Connected { channel: "D".into() })).unwrap();
        tx.send(voice(VoiceEvent::Participants(vec!["ME".into(), "ANN".into()])))
            .unwrap();
        tx.send(voice(VoiceEvent::Speaking(vec!["ANN".into()]))).unwrap();
        let events = a.poll();
        assert!(matches!(&events[0], Event::CallConnected { room: r } if r == &room("D")));
        let Some(Event::CallMembers { members, .. }) = events.last() else {
            panic!("members: {events:?}")
        };
        let who: Vec<(&str, bool)> = members.iter().map(|m| (&*m.name, m.speaking)).collect();
        assert_eq!(who, [("Me Myself", false), ("Ann", true)]);

        tx.send(Ev::Voice {
            generation: 0,
            channel: "old-call".into(),
            event: VoiceEvent::Disconnected { reason: None },
        })
        .unwrap();
        assert!(a.poll().is_empty(), "an old call must not end the current one");
        tx.send(voice(VoiceEvent::Disconnected {
            reason: Some("lost".into()),
        }))
        .unwrap();
        let events = a.poll();
        assert!(matches!(&events[..], [Event::CallEnded { reason: Some(_), .. }]));
        a.run(Command::JoinCall { room: room("D") });
        let old_generation = a.call.as_ref().unwrap().generation;
        a.run(Command::JoinCall { room: room("D") });
        tx.send(Ev::Voice {
            channel: "D".into(),
            generation: old_generation,
            event: VoiceEvent::Disconnected { reason: None },
        })
        .unwrap();
        assert!(
            a.poll().is_empty(),
            "the previous join to the same room must not end the new call"
        );
        assert!(a.call.is_some());
    }

    #[test]
    fn voice_channel_occupants_follow_join_and_leave() {
        let (mut a, tx, _cmds, _) = connected();
        let state = json!({ "id": "ANN", "is_publishing": false, "is_receiving": true });
        tx.send(Ev::Event(
            json!({ "type": "VoiceChannelJoin", "id": "C1", "state": state }),
        ))
        .unwrap();
        let events = a.poll();
        let occupants: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, Event::VoiceOccupants { .. }))
            .collect();
        let [Event::VoiceOccupants { room: r, members }] = &occupants[..] else {
            panic!("{events:?}")
        };
        assert_eq!(r, &room("C1"));
        assert!(members[0].muted, "not publishing is muted");

        tx.send(Ev::Event(
            json!({ "type": "VoiceChannelLeave", "id": "C1", "user": "ANN" }),
        ))
        .unwrap();
        let events = a.poll();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::VoiceOccupants { members, .. } if members.is_empty()))
        );
    }

    #[test]
    fn member_lists_group_by_hoisted_role_then_online_then_offline() {
        let (mut a, tx, mut cmds, _) = connected();
        let _ = (cmds.try_recv(), cmds.try_recv());
        let ready = json!({
            "users": [
                { "_id": "ME", "username": "me", "relationship": "User", "online": true },
                { "_id": "MOD", "username": "mod", "online": true, "status": { "presence": "Idle", "text": "afk" } },
                { "_id": "ANN", "username": "ann", "online": true },
                { "_id": "OFF", "username": "off", "online": false }
            ],
            "servers": [{
                "_id": "S", "name": "Server", "owner": "ME", "channels": ["C1"],
                "default_permissions": crate::permissions::VIEW_CHANNEL,
                "roles": { "R": { "name": "Mods", "hoist": true, "rank": 1, "colour": "#ff0000" } }
            }],
            "channels": [{ "_id": "C1", "channel_type": "TextChannel", "server": "S", "name": "general" }],
            "members": [
                { "_id": { "server": "S", "user": "ME" } },
                { "_id": { "server": "S", "user": "MOD" }, "roles": ["R"] },
                { "_id": { "server": "S", "user": "ANN" } },
                { "_id": { "server": "S", "user": "OFF" } }
            ]
        });
        tx.send(Ev::Ready(serde_json::from_value(ready).unwrap())).unwrap();
        a.poll();
        let _ = (cmds.try_recv(), cmds.try_recv());
        let events = a.run(Command::LoadMembers { room: room("C1") });
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::People(PeopleCmd::FetchMembers { server, online_only: false })) if server == "S")
        );
        let [Event::Members { groups, .. }] = &events[..] else {
            panic!("{events:?}")
        };
        let titles: Vec<&str> = groups.iter().map(|g| &*g.title).collect();
        assert_eq!(titles, ["Mods — 1", "Online — 2", "Offline — 1"]);
        let mod_ = &groups[0].members[0];
        assert_eq!(
            (mod_.presence, mod_.colour, mod_.status.as_deref()),
            (Presence::Idle, Some([255, 0, 0]), Some("afk"))
        );
        assert!(
            a.run(Command::LoadMembers { room: room("C1") }).len() == 1,
            "asking again resends the list"
        );
        assert!(cmds.try_recv().is_err(), "but fetches once per server");
    }

    #[test]
    fn friend_requests_send_the_tag_and_the_card_follows_the_relationship() {
        use godwit_core::members::Relationship;
        let (mut a, tx, mut cmds, _) = connected();
        let _ = (cmds.try_recv(), cmds.try_recv());
        let ann = UserId::new(AccountId::new(Network::Stoat, "ME"), "ANN");
        let events = a.run(Command::LoadProfile {
            user: ann.clone(),
            room: Some(room("C1")),
        });
        let [Event::Profile(p)] = &events[..] else {
            panic!("{events:?}")
        };
        assert!(p.has_friends && !p.bot);
        let before = p.relationship;
        let _ = cmds.try_recv();
        a.run(Command::AddFriend { user: ann.clone() });
        let Ok(Cmd::People(PeopleCmd::FriendRequest { username, .. })) = cmds.try_recv() else {
            panic!("no friend request sent")
        };
        assert!(username.starts_with(&a.store.users["ANN"].username) && username.contains('#'));
        let mut user = a.store.users["ANN"].clone();
        user.relationship = Some("Outgoing".into());
        tx.send(Ev::UserChanged(user)).unwrap();
        let events = a.poll();
        let Some(Event::Profile(p)) = events.iter().find(|e| matches!(e, Event::Profile(_))) else {
            panic!("{events:?}")
        };
        assert_eq!(p.relationship, Relationship::Outgoing);
        assert_ne!(before, Relationship::Outgoing);
        a.run(Command::RemoveFriend { user: ann });
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::People(PeopleCmd::Relationship { user, action: RelationshipAction::RemoveFriend })) if user == "ANN"
        ));
    }

    #[test]
    fn profiles_show_at_once_then_fill_in_the_bio() {
        let (mut a, tx, mut cmds, _) = connected();
        let _ = (cmds.try_recv(), cmds.try_recv());
        let ann = UserId::new(AccountId::new(Network::Stoat, "ME"), "ANN");
        let events = a.run(Command::LoadProfile {
            user: ann.clone(),
            room: Some(room("C1")),
        });
        let [Event::Profile(p)] = &events[..] else {
            panic!("{events:?}")
        };
        assert_eq!((&*p.name, p.bio.is_none(), p.me), ("Ann", true, false));
        assert!(matches!(cmds.try_recv(), Ok(Cmd::People(PeopleCmd::FetchProfile { user })) if user == "ANN"));
        let profile = serde_json::from_value(json!({ "content": "**hi** there" })).unwrap();
        tx.send(Ev::Profile {
            user: "ANN".into(),
            profile,
        })
        .unwrap();
        let events = a.poll();
        let [Event::Profile(p)] = &events[..] else {
            panic!("{events:?}")
        };
        assert_eq!(p.bio.as_deref(), Some("**hi** there"));
        a.run(Command::LoadProfile { user: ann, room: None });
        assert!(cmds.try_recv().is_err(), "fetched once per person");
    }

    #[test]
    fn logging_out_loses_the_account() {
        let (mut a, tx, _cmds, _) = connected();
        tx.send(Ev::LoggedOut).unwrap();
        let events = a.poll();
        assert!(matches!(&events[..], [Event::AccountLost { .. }]));
        assert_eq!(a.id(), None);
    }
}
