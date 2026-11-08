// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Stoat API types. Kept deliberately lenient: unknown fields are ignored and
//! most fields default, so server-side additions don't break deserialization.

use serde::{Deserialize, Serialize};

pub const DEFAULT_API: &str = "https://stoat.chat/api";

#[derive(Debug, Clone, Deserialize)]
pub struct RootConfig {
    pub ws: String,
    pub features: Features,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Features {
    pub autumn: Feature,
    /// Media proxy for embed images ("January").
    #[serde(default)]
    pub january: Option<Feature>,
    #[serde(default)]
    pub limits: Option<LimitsConfig>,
    #[serde(default)]
    pub livekit: Option<VoiceFeature>,
}

/// Voice servers; joining a server call has to name one of these nodes.
#[derive(Debug, Clone, Deserialize)]
pub struct VoiceFeature {
    #[serde(default)]
    pub nodes: Vec<VoiceNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VoiceNode {
    pub name: String,
    pub public_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LimitsConfig {
    pub default: UserLimits,
    #[serde(default)]
    pub global: Option<GlobalLimits>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GlobalLimits {
    /// Most custom emojis one server may have.
    #[serde(default)]
    pub server_emoji: Option<usize>,
    /// Most messages one message may reply to.
    #[serde(default)]
    pub message_replies: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserLimits {
    #[serde(default)]
    pub video: bool,
    /// Width/height, as used by the official RTC getEnabledScreenShareQualities.
    #[serde(default)]
    pub video_resolution: Option<[u32; 2]>,
    #[serde(default)]
    pub message_attachments: Option<usize>,
    /// Per upload tag ("attachments", "avatars", …), in bytes.
    #[serde(default)]
    pub file_upload_size_limits: std::collections::HashMap<String, u64>,
}

impl RootConfig {
    /// Most files one message may carry; 5 if the server doesn't say.
    pub fn max_attachments(&self) -> usize {
        self.features
            .limits
            .as_ref()
            .and_then(|l| l.default.message_attachments)
            .unwrap_or(5)
    }

    /// Largest attachment in bytes; 20 MB if the server doesn't say.
    pub fn max_attachment_size(&self) -> u64 {
        self.features
            .limits
            .as_ref()
            .and_then(|l| l.default.file_upload_size_limits.get("attachments").copied())
            .unwrap_or(20_000_000)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Feature {
    pub url: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl RootConfig {
    /// Same as the official SDK's `proxyFile`: route third-party media through
    /// the January proxy so the client never fetches it directly.
    pub fn proxy(&self, url: &str) -> String {
        match self.features.january.as_ref().filter(|j| j.enabled) {
            Some(j) => format!("{}/proxy?url={}", j.url, encode_uri_component(url)),
            None => url.to_string(),
        }
    }
}

/// JavaScript's `encodeURIComponent`.
pub use godwit_core::encoding::encode_uri_component;

/// Link previews and bot embeds.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum Embed {
    Website {
        #[serde(default)]
        url: Option<String>,
        /// The URL as posted, before redirects.
        #[serde(default)]
        original_url: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        image: Option<EmbedImage>,
        #[serde(default)]
        video: Option<EmbedVideo>,
        #[serde(default)]
        site_name: Option<String>,
        #[serde(default)]
        icon_url: Option<String>,
        #[serde(default)]
        colour: Option<String>,
        /// YouTube, Twitch, Spotify, … (`{ type, id, content_type, timestamp }`); see player.rs.
        #[serde(default)]
        special: Option<serde_json::Value>,
    },
    Image(EmbedImage),
    Video(EmbedVideo),
    Text {
        #[serde(default)]
        icon_url: Option<String>,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        media: Option<File>,
        #[serde(default)]
        colour: Option<String>,
    },
    #[serde(other)]
    None,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmbedImage {
    pub url: String,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    /// "Large" (below the card) or "Preview" (thumbnail beside it).
    #[serde(default)]
    pub size: Option<String>,
}

/// The official What's New source.
pub const CHANGELOG_URL: &str = "https://changelog.stoat.chat/v1/changelogs/latest";

/// The latest changelog (the official ChangelogResponse; fields we show).
#[derive(Debug, Clone, Deserialize)]
pub struct Changelog {
    pub id: String,
    pub markdown_content: String,
    /// ISO 8601.
    pub published_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmbedVideo {
    pub url: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    #[serde(rename = "_id")]
    pub id: String,
    pub user_id: String,
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "result")]
pub enum LoginResponse {
    Success(Session),
    #[serde(rename = "MFA")]
    Mfa {
        ticket: String,
        #[serde(default)]
        allowed_methods: Vec<String>,
    },
    Disabled,
}

#[derive(Debug, Clone, Deserialize)]
pub struct File {
    #[serde(rename = "_id")]
    pub id: String,
    pub tag: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub metadata: FileMetadata,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileMetadata {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    #[serde(rename = "_id")]
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub discriminator: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<File>,
    #[serde(default)]
    pub relationship: Option<String>,
    #[serde(default)]
    pub online: Option<bool>,
    #[serde(default)]
    pub status: Option<UserStatus>,
    /// Present (with the owner's id) when the user is a bot.
    #[serde(default)]
    pub bot: Option<serde_json::Value>,
    #[serde(default)]
    pub privileged: bool,
    #[serde(default)]
    pub pronouns: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserStatus {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub presence: Option<String>,
}

impl User {
    pub fn name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.username)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Category {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub channels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Server {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub categories: Option<Vec<Category>>,
    #[serde(default)]
    pub icon: Option<File>,
    #[serde(default)]
    pub roles: std::collections::HashMap<String, Role>,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub default_permissions: i64,
    #[serde(default)]
    pub approximate_member_count: Option<u64>,
    /// Listed on Discover.
    #[serde(default)]
    pub discoverable: bool,
    #[serde(default)]
    pub description: Option<String>,
    /// Channels for join/leave/kick/ban messages.
    #[serde(default)]
    pub system_messages: Option<serde_json::Value>,
    #[serde(default)]
    pub banner: Option<File>,
    /// 1 = official server, 2 = verified.
    #[serde(default)]
    pub flags: Option<i64>,
}

/// A permission override: bits to allow and bits to deny.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
pub struct Override {
    pub a: i64,
    pub d: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Role {
    pub name: String,
    /// Any CSS colour; only plain hex is rendered.
    #[serde(default)]
    pub colour: Option<String>,
    #[serde(default)]
    pub hoist: bool,
    /// Lower rank means higher in the list.
    #[serde(default)]
    pub rank: i64,
    #[serde(default)]
    pub permissions: Option<Override>,
    #[serde(default)]
    pub icon: Option<File>,
}

/// Parses `#rgb` / `#rrggbb`; gradients and named colours return None.
pub fn parse_hex_colour(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.trim().strip_prefix('#')?;
    let v = |r: &str| u8::from_str_radix(r, 16).ok();
    match h.len() {
        6 => Some((v(&h[0..2])?, v(&h[2..4])?, v(&h[4..6])?)),
        3 => {
            let d = |i: usize| v(&h[i..i + 1]).map(|x| x * 17);
            Some((d(0)?, d(1)?, d(2)?))
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    #[serde(rename = "_id")]
    pub id: String,
    pub channel_type: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub recipients: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub last_message_id: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
    /// Group or channel icon.
    #[serde(default)]
    pub icon: Option<File>,
    /// Age-restricted ("mature").
    #[serde(default)]
    pub nsfw: bool,
    /// Slow mode delay in seconds.
    #[serde(default)]
    pub slowmode: Option<u64>,
    /// Present on voice-enabled text channels (e.g. `{ "max_users": 10 }`).
    #[serde(default)]
    pub voice: Option<serde_json::Value>,
    /// Group owner.
    #[serde(default)]
    pub owner: Option<String>,
    /// Group permissions for members.
    #[serde(default)]
    pub permissions: Option<i64>,
    #[serde(default)]
    pub default_permissions: Option<Override>,
    #[serde(default)]
    pub role_permissions: std::collections::HashMap<String, Override>,
}

impl Channel {
    /// Group icon, if any.
    pub fn icon_url(&self) -> Option<File> {
        self.icon.clone()
    }

    /// Server text channels with a `voice` object double as voice channels;
    /// there is no separate voice channel type.
    pub fn is_voice(&self) -> bool {
        self.voice.is_some()
    }

    /// Whether a call can be started here: voice channels, DMs and groups.
    pub fn can_call(&self) -> bool {
        self.is_voice() || matches!(self.channel_type.as_str(), "DirectMessage" | "Group")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct MemberId {
    pub server: String,
    pub user: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Member {
    #[serde(rename = "_id")]
    pub id: MemberId,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub avatar: Option<File>,
    /// Pronouns on this server.
    #[serde(default)]
    pub pronouns: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    /// ISO 8601 time the member is timed out until.
    #[serde(default)]
    pub timeout: Option<String>,
}

/// Response of `GET /servers/{id}/members`.
#[derive(Debug, Clone, Deserialize)]
pub struct AllMembers {
    pub members: Vec<Member>,
    pub users: Vec<User>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Masquerade {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    #[serde(rename = "_id")]
    pub id: String,
    pub channel: String,
    pub author: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub attachments: Vec<File>,
    #[serde(default)]
    pub replies: Vec<String>,
    /// Users this message mentions.
    #[serde(default)]
    pub mentions: Vec<String>,
    /// Emoji (unicode, or a custom emoji id) to the users who reacted, in server order.
    #[serde(default)]
    pub reactions: indexmap::IndexMap<String, Vec<String>>,
    #[serde(default)]
    pub embeds: Vec<Embed>,
    #[serde(default)]
    pub edited: Option<String>,
    #[serde(default)]
    pub pinned: Option<bool>,
    #[serde(default)]
    pub masquerade: Option<Masquerade>,
    #[serde(default)]
    pub system: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChannelUnreadKey {
    pub channel: String,
}

/// One entry of `GET /sync/unreads`.
#[derive(Debug, Clone, Deserialize)]
pub struct ChannelUnread {
    #[serde(rename = "_id")]
    pub id: ChannelUnreadKey,
    #[serde(default)]
    pub last_id: Option<String>,
    #[serde(default)]
    pub mentions: Vec<String>,
}

/// `GET /users/{id}/profile`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserProfile {
    /// Bio (Markdown).
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub background: Option<File>,
}

/// Response of `GET /channels/{id}/messages?include_users=true`.
#[derive(Debug, Clone, Deserialize)]
pub struct MessagesWithUsers {
    pub messages: Vec<Message>,
    #[serde(default)]
    pub users: Vec<User>,
    #[serde(default)]
    pub members: Vec<Member>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Emoji {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub creator_id: Option<String>,
    /// Where it comes from (`{ "type": "Server", "id": … }`).
    #[serde(default)]
    pub parent: Option<serde_json::Value>,
}

impl Emoji {
    /// The server this emoji belongs to, if it's a server emoji.
    pub fn server(&self) -> Option<&str> {
        self.parent
            .as_ref()
            .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("Server"))?
            .get("id")?
            .as_str()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Ready {
    #[serde(default)]
    pub users: Vec<User>,
    #[serde(default)]
    pub servers: Vec<Server>,
    #[serde(default)]
    pub channels: Vec<Channel>,
    #[serde(default)]
    pub members: Vec<Member>,
    #[serde(default)]
    pub emojis: Vec<Emoji>,
    #[serde(default)]
    pub voice_states: Vec<ChannelVoiceState>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChannelVoiceState {
    /// Channel id.
    pub id: String,
    #[serde(default)]
    pub participants: Vec<UserVoiceState>,
}

/// A user in a call, as Stoat (not LiveKit) reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct UserVoiceState {
    /// User id.
    pub id: String,
    /// False when muted.
    #[serde(default = "yes")]
    pub is_publishing: bool,
    /// False when deafened.
    #[serde(default = "yes")]
    pub is_receiving: bool,
    #[serde(default)]
    pub camera: bool,
    #[serde(default)]
    pub screensharing: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyIntent {
    pub id: String,
    pub mention: bool,
}

/// A login session as listed by `GET /auth/session/all`. The id is a ULID, so
/// it also tells when the session was created.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionInfo {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
}

/// Proof of identity for `PUT /auth/mfa/ticket`; serialises to the spec's
/// `{"password": …}`, `{"totp_code": …}` or `{"recovery_code": …}`.
#[derive(Clone, Serialize)]
pub enum MfaResponse {
    #[serde(rename = "password")]
    Password(String),
    #[serde(rename = "totp_code")]
    Totp(String),
    #[serde(rename = "recovery_code")]
    Recovery(String),
}

/// `POST /servers/create` and joining a server invite both return the server with its channels.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerWithChannels {
    pub server: Server,
    #[serde(default)]
    pub channels: Vec<Channel>,
}

/// The parts of `GET /invites/{code}` the invite dialog shows (server invites).
#[derive(Debug, Clone, Deserialize)]
pub struct InvitePreview {
    #[serde(default)]
    pub server_id: Option<String>,
    #[serde(default)]
    pub server_name: Option<String>,
    #[serde(default)]
    pub server_icon: Option<File>,
    #[serde(default)]
    pub member_count: Option<u64>,
}

/// Only the code of a newly created invite is needed.
#[derive(Deserialize)]
pub struct CreatedInvite {
    #[serde(rename = "_id")]
    pub code: String,
}

#[derive(Deserialize)]
pub struct MfaTicket {
    pub token: String,
}

/// Milliseconds since the Unix epoch encoded in a ULID's first 10 characters.
pub fn ulid_timestamp_ms(id: &str) -> Option<i64> {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut ms: i64 = 0;
    for c in id.bytes().take(10) {
        let v = ALPHABET.iter().position(|&a| a == c.to_ascii_uppercase())?;
        ms = ms * 32 + v as i64;
    }
    Some(ms)
}

/// Stoat's GIF service (a Tenor proxy). The official client's default; the
/// backend doesn't announce it yet ("TODO Gifbox URL from backend" there).
pub const GIFBOX: &str = "https://api.gifbox.me";

/// A page of gifbox results (stoatchat `crates/services/gifbox/src/types.rs`).
#[derive(Debug, Deserialize)]
pub struct GifPage {
    pub results: Vec<GifResult>,
    #[serde(default)]
    pub next: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GifResult {
    /// The Tenor page: what the official picker sends.
    pub url: String,
    /// By format: gifbox asks Tenor for "webm" and "tinywebm" only.
    #[serde(default)]
    pub media_formats: std::collections::HashMap<String, GifMedia>,
}

#[derive(Debug, Deserialize)]
pub struct GifMedia {
    pub url: String,
    /// Width and height in pixels.
    #[serde(default)]
    pub dimensions: Vec<u64>,
}

#[derive(Debug, Deserialize)]
pub struct GifCategory {
    pub title: String,
    pub image: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_messages_channel_response_deserializes() {
        let channel: Channel = serde_json::from_value(serde_json::json!({
            "_id": "notes-id",
            "channel_type": "SavedMessages",
            "user": "ME"
        }))
        .unwrap();
        assert_eq!(channel.id, "notes-id");
        assert_eq!(channel.channel_type, "SavedMessages");
        assert_eq!(channel.server, None);
        assert!(channel.recipients.is_empty());
    }

    #[test]
    fn proxy_and_embeds() {
        assert_eq!(
            encode_uri_component("https://a.b/c?d=1&e=ü"),
            "https%3A%2F%2Fa.b%2Fc%3Fd%3D1%26e%3D%C3%BC"
        );
        let embeds: Vec<Embed> = serde_json::from_value(serde_json::json!([
            {"type": "Website", "title": "T", "image": {"url": "https://i", "width": 1, "height": 1, "size": "Large"}},
            {"type": "Image", "url": "https://x.png", "width": 10, "height": 20, "size": "Large"},
            {"type": "Text", "title": "Card", "media": {"_id": "file-id", "tag": "attachments", "filename": "preview.png", "size": 123, "metadata": {"type": "Image", "width": 320, "height": 180}}},
            {"type": "SomethingNew"},
        ]))
        .unwrap();
        assert!(matches!(&embeds[0], Embed::Website { title: Some(t), .. } if t == "T"));
        assert!(matches!(&embeds[1], Embed::Image(i) if i.height == 20));
        assert!(
            matches!(&embeds[2], Embed::Text { title: Some(t), media: Some(f), .. } if t == "Card" && f.metadata.width == Some(320))
        );
        assert!(
            matches!(embeds[3], Embed::None),
            "unknown embed types don't break parsing"
        );
    }

    #[test]
    fn upload_limits() {
        let with: RootConfig = serde_json::from_value(serde_json::json!({
            "ws": "wss://x", "features": { "autumn": { "url": "https://cdn" },
              "limits": { "default": { "message_attachments": 10, "file_upload_size_limits": { "attachments": 1234 } } } }
        }))
        .unwrap();
        assert_eq!((with.max_attachments(), with.max_attachment_size()), (10, 1234));
        let without: RootConfig = serde_json::from_value(
            serde_json::json!({ "ws": "wss://x", "features": { "autumn": { "url": "https://cdn" } } }),
        )
        .unwrap();
        assert_eq!(
            (without.max_attachments(), without.max_attachment_size()),
            (5, 20_000_000)
        );
    }

    #[test]
    fn hex_colours() {
        assert_eq!(parse_hex_colour("#ff8000"), Some((255, 128, 0)));
        assert_eq!(parse_hex_colour("#f80"), Some((255, 136, 0)));
        assert_eq!(parse_hex_colour("linear-gradient(red, blue)"), None);
    }

    #[test]
    fn ulid_time() {
        // Example ULID from the spec; 2016-07-30T23:54:10.259Z.
        assert_eq!(ulid_timestamp_ms("01ARZ3NDEKTSV4RRFFQ69G5FAV"), Some(1469922850259));
    }

    #[test]
    fn mfa_response_shape() {
        // The spec's MFAResponse is one of three single-key objects.
        let v = serde_json::to_value(MfaResponse::Totp("123456".into())).unwrap();
        assert_eq!(v, serde_json::json!({ "totp_code": "123456" }));
        let v = serde_json::to_value(MfaResponse::Recovery("abcd-efgh".into())).unwrap();
        assert_eq!(v, serde_json::json!({ "recovery_code": "abcd-efgh" }));
    }

    #[test]
    fn gifbox_pages_parse_with_and_without_a_next_page() {
        let page: GifPage = serde_json::from_value(serde_json::json!({
            "results": [{
                "id": "1",
                "url": "https://tenor.com/view/x-1",
                "media_formats": {
                    "tinywebm": { "url": "https://media.tenor.com/a/tiny.webm", "dimensions": [220, 124] },
                    "webm": { "url": "https://media.tenor.com/a/full.webm", "dimensions": [498, 280] }
                }
            }],
            "next": "CAgQ"
        }))
        .unwrap();
        assert_eq!(page.next.as_deref(), Some("CAgQ"));
        assert_eq!(page.results[0].media_formats["tinywebm"].dimensions, [220, 124]);
        let last: GifPage = serde_json::from_value(serde_json::json!({ "results": [] })).unwrap();
        assert!(last.next.is_none());
        let cats: Vec<GifCategory> =
            serde_json::from_value(serde_json::json!([{ "title": "wave", "image": "https://media.tenor.com/c.gif" }]))
                .unwrap();
        assert_eq!(cats[0].title, "wave");
    }
}
