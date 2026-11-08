// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Unicode emoji as pictures: the one lookup every view uses (message text,
//! labels, reactions, the picker), so a network only has to hand over the
//! emoji itself. Networks still give their own custom emoji's pictures.

/// The emoji `g` (one grapheme) is, if it's one. Some arrive without (or with
/// a stray) presentation selector, U+FE0F: `❤` for `❤️`; both are found.
/// Plain ASCII never is: "4" + U+FE0F names the keycap's base, which turned
/// digits in names into pictures (real keycaps, 4️⃣, aren't ASCII).
pub fn canonical(g: &str) -> Option<&'static emojis::Emoji> {
    if g.is_ascii() {
        return None;
    }
    emojis::get(g)
        .or_else(|| emojis::get(&format!("{g}\u{fe0f}")))
        .or_else(|| emojis::get(&g.replace('\u{fe0f}', "")))
}

/// The pack every view draws from, as a row of `godwit_core::emoji_packs::PACKS`
/// (Settings › Appearance › Emoji). Godwit's default is Twemoji, flat
/// (Stoat's own default is Fluent 3D).
static PACK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(DEFAULT_PACK);
pub const DEFAULT_PACK: usize = 5;

/// Draws every emoji from `pack` (an id from `PACKS`; unknown ones: the
/// default). Rows already built keep their pictures until rebuilt.
pub fn set_pack(pack: &str) {
    let index = godwit_core::emoji_packs::PACKS
        .iter()
        .position(|(id, _)| *id == pack)
        .unwrap_or(DEFAULT_PACK);
    PACK.store(index, std::sync::atomic::Ordering::Relaxed);
}

/// The pack in use: its id.
pub fn pack() -> &'static str {
    let packs = &godwit_core::emoji_packs::PACKS;
    let index = PACK.load(std::sync::atomic::Ordering::Relaxed);
    packs
        .get(index)
        .or(packs.get(DEFAULT_PACK))
        .map_or(godwit_core::emoji_packs::PACK, |(id, _)| id)
}

/// The picture of a known Unicode emoji, in the chosen pack.
pub fn url_of(e: &emojis::Emoji) -> String {
    godwit_core::emoji_packs::unicode_url(pack(), e.as_str())
}

/// The pack has a picture for it (newer emoji don't, yet).
pub fn drawn(e: &emojis::Emoji) -> bool {
    let (major, minor) = godwit_core::emoji_packs::NEWEST;
    e.unicode_version() <= emojis::UnicodeVersion::new(major, minor)
}

/// The picture for `g`, if it's a Unicode emoji.
pub fn url(g: &str) -> Option<String> {
    canonical(g).map(url_of)
}

/// The emoji Settings › Appearance draws each pack with (the official
/// client's `EmojiPack` in its appearance settings shows these five).
pub const SAMPLES: [&str; 5] = ["😃", "😂", "😶\u{200d}🌫\u{fe0f}", "🤨", "🤔"];

/// `SAMPLES` as drawn in `pack`: each one's character and picture.
pub fn samples(pack: &str) -> Vec<(&'static str, String)> {
    SAMPLES
        .iter()
        .filter_map(|g| canonical(g))
        .map(|e| (e.as_str(), godwit_core::emoji_packs::unicode_url(pack, e.as_str())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_are_found_with_or_without_the_selector() {
        assert!(url("👍").is_some());
        assert_eq!(url("❤"), url("❤️"));
        assert!(url("4").is_none() && url("a").is_none());
    }

    #[test]
    fn every_sample_is_a_known_emoji_each_pack_draws() {
        for (pack, _) in godwit_core::emoji_packs::PACKS {
            let samples = samples(pack);
            assert_eq!(samples.len(), SAMPLES.len());
            assert!(
                samples
                    .iter()
                    .all(|(g, url)| url.contains(pack) && canonical(g).is_some_and(drawn))
            );
        }
        assert_eq!(
            samples("noto").get(2).map(|(_, url)| url.as_str()),
            Some("https://static.stoat.chat/emoji/noto/1f636-200d-1f32b-fe0f.svg?v=1")
        );
    }
}
