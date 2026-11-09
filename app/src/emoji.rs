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

/// `g` as one regional indicator standing alone, without the ZWNJ that keeps
/// it from pairing into a flag (a grapheme carries it at either end). The
/// `emojis` table has no lone letters, but the packs draw each one (the
/// official client's shorthands show them: stoatchat/for-web a914137,
/// c3c43a4).
pub fn lone_indicator(g: &str) -> Option<&str> {
    let bare = g.trim_matches('\u{200c}');
    let mut chars = bare.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if (0x1f1e6..=0x1f1ff).contains(&u32::from(c)) => Some(bare),
        _ => None,
    }
}

/// Regional-indicator shorthand characters are consecutive Unicode codepoints.
pub fn regional_indicator(letter: char) -> Option<char> {
    letter
        .is_ascii_lowercase()
        .then(|| char::from_u32(0x1f1e6 + u32::from(letter) - u32::from('a')))
        .flatten()
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
    if let Some(letter) = lone_indicator(g) {
        return Some(godwit_core::emoji_packs::unicode_url(pack(), letter));
    }
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
        assert!(url("\u{200c}🇦").is_some());
        assert!(url("4").is_none() && url("a").is_none());
    }

    #[test]
    fn regional_indicator_shortcodes_cover_ascii_letters_only() {
        assert_eq!(regional_indicator('a'), Some('🇦'));
        assert_eq!(regional_indicator('z'), Some('🇿'));
        assert_eq!(regional_indicator('A'), None);
        assert_eq!(regional_indicator('1'), None);
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
