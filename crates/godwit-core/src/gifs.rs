// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The GIF picker: categories, trending and search results from a network's
//! GIF service (Stoat's gifbox, a Tenor proxy; Discord has the same idea).
//! Picking one sends its page link, which the network turns into an embed.

use std::sync::Arc;

/// A GIF result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gif {
    /// What's sent: the GIF's page (as the official picker sends).
    pub url: Arc<str>,
    /// A small looping preview for the picker's grid.
    pub preview: Arc<str>,
    /// The preview's size in pixels, when the service says.
    pub width: u32,
    pub height: u32,
}

/// A category tile: searching its title shows its GIFs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GifCategory {
    pub title: Arc<str>,
    pub image: Arc<str>,
}

/// What the picker shows, for the account of the room it's open in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GifPicker {
    /// Categories, once loaded (per session: they rarely change).
    pub categories: Vec<GifCategory>,
    pub categories_asked: bool,
    /// The search shown ("" for the categories; [`TRENDING`] for trending).
    pub query: String,
    pub results: Vec<Gif>,
    /// Where the next page starts; None at the end.
    pub next: Option<String>,
    pub loading: bool,
    pub error: Option<Arc<str>>,
}

/// The query that lists trending GIFs, as the official picker's tile.
pub const TRENDING: &str = "trending";

/// GIF services, by domain: their pages and media (any subdomain, such as
/// `media.tenor.com` or `i.giphy.com`) show as GIFs, looping without a card.
/// Stoat's official list is tenor.com, giphy.com and gifbox.me (exact
/// origins); the rest are other common ones.
const GIF_SITES: [&str; 5] = ["tenor.com", "giphy.com", "gifbox.me", "klipy.com", "gfycat.com"];

/// Whether `link` is a GIF: a GIF service's page or media, Imgur's `.gif` and
/// `.gifv`, or any `.gif` file. Only HTTPS links count.
pub fn is_gif_link(link: &str) -> bool {
    let Ok(url) = url::Url::parse(link) else {
        return false;
    };
    let Some(host) = url.host_str().filter(|_| url.scheme() == "https") else {
        return false;
    };
    let on = |domain: &str| host == domain || host.strip_suffix(domain).is_some_and(|rest| rest.ends_with('.'));
    let path = url.path().to_ascii_lowercase();
    GIF_SITES.iter().any(|d| on(d))
        || (on("imgur.com") && (path.ends_with(".gif") || path.ends_with(".gifv")))
        || path.ends_with(".gif")
}

/// A GIF service's video that Godwit can't play itself (MP4) as one it can
/// (WebM), so the GIF animates natively on any network, without a player.
/// Tenor: one id, the format in its last letters (`…AAAPo/x.mp4`,
/// `…AAAPs/x.webm`, as its pages list them). Also found inside a network's
/// media proxy (`…/https/media.tenor.com/…`).
pub fn native_gif_video(video: &str) -> Option<String> {
    let at = video.find("media.tenor.com/")?;
    let rest = video.get(at + "media.tenor.com/".len()..)?;
    let rest = rest.split(['?', '#']).next()?;
    let (id, name) = rest.split_once('/')?;
    let stem = name.strip_suffix(".mp4")?;
    let base = id.strip_suffix("AAPo")?;
    let safe = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c));
    (safe(base) && safe(stem)).then(|| format!("https://media.tenor.com/{base}AAPs/{stem}.webm"))
}

/// Gives GIF embeds whose video only plays in a player (see
/// `native_gif_video`) a picture that animates natively.
pub fn play_gifs_natively(embeds: &mut [crate::message::Embed]) {
    for e in embeds {
        if e.style != crate::message::EmbedStyle::Gif {
            continue;
        }
        if let Some(webm) = e.video_url.as_deref().and_then(native_gif_video) {
            e.image_url = Some(webm.into());
            e.image_thumbnail = false;
        }
    }
}

#[cfg(test)]
mod gif_link_tests {
    #[test]
    fn tenor_mp4_becomes_its_webm_even_behind_a_proxy() {
        use super::native_gif_video;
        let webm = Some("https://media.tenor.com/cp4Ls9jCj6MAAAPs/destiny2-witness.webm".to_owned());
        assert_eq!(
            native_gif_video("https://media.tenor.com/cp4Ls9jCj6MAAAPo/destiny2-witness.mp4"),
            webm
        );
        assert_eq!(
            native_gif_video(
                "https://images-ext-1.example.net/external/abc/https/media.tenor.com/cp4Ls9jCj6MAAAPo/destiny2-witness.mp4?x=1"
            ),
            webm
        );
        assert_eq!(
            native_gif_video("https://media.tenor.com/cp4Ls9jCj6MAAAP1/destiny2-witness.mp4"),
            None,
            "tiny mp4: no known webm"
        );
        assert_eq!(native_gif_video("https://example.com/x.mp4"), None);
    }

    #[test]
    fn gif_embeds_with_a_player_only_video_get_a_native_picture() {
        use crate::message::{Embed, EmbedStyle};
        let gif = Embed {
            style: EmbedStyle::Gif,
            // A network's still thumbnail and its proxied MP4.
            image_url: Some("https://proxy.example/x.png".into()),
            image_thumbnail: true,
            video_url: Some("https://proxy.example/https/media.tenor.com/abcAAPo/cat.mp4".into()),
            ..Embed::default()
        };
        let card = Embed {
            style: EmbedStyle::Card,
            ..gif.clone()
        };
        let mut embeds = [gif, card];
        super::play_gifs_natively(&mut embeds);
        assert_eq!(
            embeds[0].image_url.as_deref(),
            Some("https://media.tenor.com/abcAAPs/cat.webm")
        );
        assert!(!embeds[0].image_thumbnail);
        assert_eq!(
            embeds[1].image_url.as_deref(),
            Some("https://proxy.example/x.png"),
            "only GIFs"
        );
    }

    #[test]
    fn gif_links_are_known_by_site_and_file() {
        use super::is_gif_link;
        assert!(is_gif_link("https://tenor.com/view/x-123"));
        assert!(is_gif_link("https://media.tenor.com/abc/x.mp4"));
        assert!(is_gif_link("https://media1.giphy.com/media/abc/giphy.webp"));
        assert!(is_gif_link("https://rpc.gifbox.me/media/post/abc/mp4"));
        assert!(is_gif_link("https://i.imgur.com/abc.gifv"));
        assert!(is_gif_link("https://example.com/funny.GIF?x=1"));
        assert!(!is_gif_link("https://i.imgur.com/abc.png"));
        assert!(!is_gif_link("https://nottenor.com/view/x"));
        assert!(!is_gif_link("https://tenor.com.evil.example/x"));
        assert!(!is_gif_link("http://tenor.com/view/x"));
    }
}
