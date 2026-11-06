// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Unicode emoji drawn from packs on Stoat's CDN (as Stoat's official client
//! draws them), used for every network so emoji look the same everywhere.

/// The official default emoji pack.
pub const PACK: &str = "fluent-3d";

/// The packs the official client offers (`UNICODE_EMOJI_PACKS`; openmoji is
/// off there as incomplete): the id in the address, and its name.
pub const PACKS: [(&str, &str); 6] = [
    ("fluent-3d", "Fluent 3D"),
    ("fluent-color", "Fluent Color"),
    ("fluent-flat", "Fluent Flat"),
    ("mutant", "Mutant Remix"),
    ("noto", "Noto"),
    ("twemoji", "Twemoji"),
];

/// The newest Emoji version every pack draws: checked against
/// static.stoat.chat on 2026-11-04 (17.0's 1faea is there in all six, 18.0's
/// 1faeb in none). Newer emoji would only show as a missing picture.
pub const NEWEST: (u32, u32) = (17, 0);

/// A Unicode emoji's image in `pack` (the official `unicodeEmojiUrl`):
/// every codepoint in lowercase hex, joined by "-".
pub fn unicode_url(pack: &str, emoji: &str) -> String {
    let code: Vec<String> = emoji.chars().map(|c| format!("{:x}", u32::from(c))).collect();
    format!("https://static.stoat.chat/emoji/{pack}/{}.svg?v=1", code.join("-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_match_the_official_codepoints() {
        assert_eq!(
            unicode_url("fluent-3d", "😀"),
            "https://static.stoat.chat/emoji/fluent-3d/1f600.svg?v=1"
        );
        assert_eq!(
            unicode_url("twemoji", "❤️"),
            "https://static.stoat.chat/emoji/twemoji/2764-fe0f.svg?v=1"
        );
    }
}
