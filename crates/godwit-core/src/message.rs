// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Messages in a network-neutral shape.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::ids::{MessageId, UserId};

/// How a message's text is written. Each network crate converts its own
/// format (Stoat and Discord Markdown, Matrix HTML, …) to [`Markup::Rich`],
/// resolving mentions to names on the way, so every UI renders one format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Markup {
    #[default]
    Plain,
    /// Godwit rich text: the CommonMark subset of emphasis, strong,
    /// strikethrough (`~~`), inline code, links, lists and hard breaks
    /// (backslash-newline). Mentions, room links and spoilers are links with
    /// `godwit://` URLs: `user/{id}`, `room/{id}`, `role/{id}` and
    /// `spoiler/{percent-encoded text}` (shown as blocks until clicked).
    Rich,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AttachmentKind {
    Image,
    Video,
    Audio,
    File,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: Arc<str>,
    pub url: Arc<str>,
    pub kind: AttachmentKind,
    pub size: u64,
    /// Pixel size, for images and videos: lets the timeline reserve space
    /// before the image loads.
    pub dimensions: Option<(u32, u32)>,
    /// The full-resolution file where `url` is a network's lighter preview
    /// (Stoat's `/original`): what the image viewer shows.
    #[serde(default)]
    pub original_url: Option<Arc<str>>,
}

/// One emoji's reactions on a message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    /// The emoji itself, or the network's id for a custom emoji. This is
    /// what reacting again sends.
    pub emoji: Arc<str>,
    /// A custom emoji's picture.
    pub image_url: Option<Arc<str>>,
    pub count: u32,
    /// We reacted with it.
    pub me: bool,
}

/// How an embed is drawn (the official Stoat client's three looks).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmbedStyle {
    /// A card: site, title, description, and its media below or beside.
    #[default]
    Card,
    /// Bare media with no card: an image link, a video link.
    Media,
    /// A GIF provider's (Tenor, Giphy, …): the picture alone, looping.
    Gif,
    /// An invitation to something outside the chat (a game, as Steam's
    /// game invites): `site_name` says what kind ("Invited you to play"),
    /// `title` names it once known, `image_url` is its art, and `url` is the
    /// network's own link that joins it (handed to the system, which opens
    /// the network's app). No `url`: nothing to join from here.
    Activity,
}

/// A network-neutral rich preview. Network-specific extras can be added
/// without coupling the core to any protocol or UI toolkit.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Embed {
    pub title: Option<Arc<str>>,
    pub description: Option<Arc<str>>,
    pub url: Option<Arc<str>>,
    pub site_name: Option<Arc<str>>,
    pub image_url: Option<Arc<str>>,
    pub video_url: Option<Arc<str>>,
    /// A provider's embeddable player URL (YouTube, Twitch, Spotify, etc.).
    /// The network adapter constructs this from its embed metadata.
    pub player_url: Option<Arc<str>>,
    pub media_dimensions: Option<(u32, u32)>,
    /// Render the image beside the card text when the network marks it as a thumbnail.
    pub image_thumbnail: bool,
    /// RGB accent color, when supplied by the network.
    pub colour: Option<[u8; 3]>,
    /// The site's (or bot's) small icon, beside the site name.
    #[serde(default)]
    pub icon_url: Option<Arc<str>>,
    #[serde(default)]
    pub style: EmbedStyle,
    /// How the description is written: bots' embeds use rich text, link
    /// previews plain text.
    #[serde(default)]
    pub description_markup: Markup,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub author: UserId,
    /// The name to show: nickname or display name, resolved by the backend.
    pub author_name: Arc<str>,
    pub author_avatar: Option<Arc<str>>,
    /// The author's role colour in this room, if any.
    pub author_colour: Option<[u8; 3]>,
    /// Milliseconds since the Unix epoch.
    pub sent_ms: i64,
    pub edited: bool,
    /// Written by the network (someone joined, a call started, …), not a person.
    pub system: bool,
    pub text: Arc<str>,
    pub markup: Markup,
    /// The text as its author wrote it, in the network's own format: what
    /// editing starts from.
    pub source: Arc<str>,
    pub reply_to: Option<MessageId>,
    /// When the text is nothing but emoji: their pictures and names, shown
    /// large in place of the text.
    pub jumbo: Vec<(Arc<str>, Arc<str>)>,
    pub attachments: Vec<Attachment>,
    pub embeds: Vec<Embed>,
    /// Invite codes the message links to plainly (the official client shows
    /// a card with Join for each).
    #[serde(default)]
    pub invites: Vec<Arc<str>>,
    pub reactions: Vec<Reaction>,
    /// Sent by us and not yet confirmed by the server.
    pub pending: bool,
    /// The text in plain words for one-line previews (a reply's quote), when
    /// `source` doesn't read well as it is (Discord's `<@id>` mentions).
    #[serde(default)]
    pub snippet: Option<Arc<str>>,
    /// Pinned in its room (the timeline shows a pin by its time).
    #[serde(default)]
    pub pinned: bool,
    /// The network lets this message be forwarded (a plain one: Discord takes
    /// no system messages, polls, calls or components). Needs the room's
    /// `forward_from` too.
    #[serde(default)]
    pub forwardable: bool,
    /// An open poll of ours that can be ended now ("End Poll Now").
    #[serde(default)]
    pub poll_endable: bool,
}

/// What an invite leads to, as its card shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvitePreview {
    pub server: Option<crate::ids::ServerId>,
    pub name: Arc<str>,
    pub icon_url: Option<Arc<str>>,
    pub members: Option<u64>,
}

/// An invite card's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invite {
    Loading,
    Ready(InvitePreview),
    Joining(InvitePreview),
    /// Expired, deleted, or not a server invite.
    Invalid,
}

/// A network's own emoji (a server's custom emoji on Stoat).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomEmoji {
    pub name: Arc<str>,
    pub image_url: Arc<str>,
    /// What reacting with it sends (see [`Reaction::emoji`]).
    pub reaction: Arc<str>,
    /// What writing it in a message looks like (`:id:` on Stoat).
    pub code: Arc<str>,
}

/// A server's custom emoji.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmojiGroup {
    pub server: crate::ids::ServerId,
    pub title: Arc<str>,
    pub emoji: Vec<CustomEmoji>,
}

/// YouTube's no-cookie player for a YouTube page link: `watch?v=` on
/// youtube.com, music.youtube.com and m.youtube.com, `youtu.be/<id>`,
/// `/shorts/<id>` and `/live/<id>`, with a `t=` or `start=` in seconds. For
/// links a network embeds without a player of its own (music.youtube.com
/// gets a plain card on Discord and Stoat). The id is checked before it goes
/// into the address.
pub fn youtube_player(page: &str) -> Option<String> {
    let url = url::Url::parse(page).ok()?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return None;
    }
    let host = url.host_str()?;
    let query = |key: &str| url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned());
    let mut path = url.path_segments()?;
    let id = match host {
        "youtu.be" => path.next().map(str::to_owned),
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" => match path.next()? {
            "watch" => query("v"),
            "shorts" | "live" => path.next().map(str::to_owned),
            _ => None,
        },
        _ => None,
    }?;
    let valid = !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || "_-".contains(c));
    if !valid {
        return None;
    }
    let start = query("t")
        .or_else(|| query("start"))
        .map(|t| t.trim_end_matches('s').to_owned())
        .filter(|t| !t.is_empty() && t.len() <= 8 && t.chars().all(|c| c.is_ascii_digit()));
    Some(format!(
        "https://www.youtube-nocookie.com/embed/{id}?modestbranding=1{}",
        start.map_or_else(String::new, |t| format!("&start={t}"))
    ))
}

/// Spotify's embed player for an open.spotify.com page (`/track/<id>`,
/// album, playlist, episode, show, artist; with or without an `intl-xx/`
/// locale), as Stoat's January builds it for its Spotify embeds.
pub fn spotify_player(page: &str) -> Option<String> {
    let url = url::Url::parse(page).ok()?;
    if url.scheme() != "https" || url.host_str()? != "open.spotify.com" {
        return None;
    }
    let mut path = url.path_segments()?.filter(|s| !s.is_empty()).peekable();
    if path.peek().is_some_and(|s| s.starts_with("intl-")) {
        path.next();
    }
    let kind = path.next()?;
    let id = path.next()?;
    let valid = matches!(kind, "track" | "album" | "playlist" | "episode" | "show" | "artist")
        && !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric());
    valid.then(|| format!("https://open.spotify.com/embed/{kind}/{id}"))
}

/// The player for a page link Godwit knows how to play (YouTube, Spotify).
pub fn link_player(page: &str) -> Option<String> {
    youtube_player(page).or_else(|| spotify_player(page))
}

#[cfg(test)]
mod youtube_tests {
    use super::youtube_player;

    #[test]
    fn youtube_page_links_get_the_no_cookie_player() {
        let player = |id: &str| format!("https://www.youtube-nocookie.com/embed/{id}?modestbranding=1");
        assert_eq!(
            youtube_player("https://music.youtube.com/watch?v=aB_1-x&si=zz").as_deref(),
            Some(player("aB_1-x").as_str())
        );
        assert_eq!(
            youtube_player("https://youtu.be/aB1?t=90").as_deref(),
            Some(format!("{}&start=90", player("aB1")).as_str())
        );
        assert_eq!(
            youtube_player("https://www.youtube.com/shorts/xyz").as_deref(),
            Some(player("xyz").as_str())
        );
        assert_eq!(
            youtube_player("https://m.youtube.com/watch?v=q&t=12s").as_deref(),
            Some(format!("{}&start=12", player("q")).as_str())
        );
    }

    #[test]
    fn other_links_and_odd_ids_get_none() {
        assert!(youtube_player("https://music.youtube.com/playlist?list=abc").is_none());
        assert!(youtube_player("https://example.com/watch?v=abc").is_none());
        assert!(youtube_player("https://www.youtube.com/watch?v=a%22onerror").is_none());
        assert!(youtube_player("https://youtube.com.evil.example/watch?v=abc").is_none());
        assert!(youtube_player("javascript:alert(1)").is_none());
    }

    #[test]
    fn spotify_pages_get_its_embed_player() {
        use super::{link_player, spotify_player};
        assert_eq!(
            spotify_player("https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC?si=x").as_deref(),
            Some("https://open.spotify.com/embed/track/4uLU6hMCjMI75M1A2tKUQC")
        );
        assert_eq!(
            spotify_player("https://open.spotify.com/intl-nl/album/abc123").as_deref(),
            Some("https://open.spotify.com/embed/album/abc123")
        );
        assert!(spotify_player("https://open.spotify.com/user/abc").is_none());
        assert!(spotify_player("https://open.spotify.com/track/a%22b").is_none());
        assert!(spotify_player("http://open.spotify.com/track/abc").is_none());
        assert!(link_player("https://music.youtube.com/watch?v=abc").is_some());
    }
}
