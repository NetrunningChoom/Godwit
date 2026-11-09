// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Teams' emoticon ids (`smile`, `like`, `1f3aa_circustent`) as emoji.
//! Teams names emoji by these ids in reactions and in its emoji images
//! (whose `alt` may be a text shortcut like `(smile)`), not by the emoji.
//! The table is data from ic3-teams (MIT; see the head of `emoji.tsv`).

use std::collections::HashMap;
use std::sync::OnceLock;

const TABLE: &str = include_str!("emoji.tsv");

fn table() -> &'static HashMap<&'static str, &'static str> {
    static T: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    T.get_or_init(|| {
        TABLE
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| {
                let mut parts = l.split('\t');
                Some((parts.next()?, parts.next()?))
            })
            .collect()
    })
}

/// Every Unicode emoji Teams has (its picker's set), without presentation
/// selectors: Teams has its own subset, so the picker offers only these.
pub fn all() -> &'static std::collections::HashSet<String> {
    static ALL: OnceLock<std::collections::HashSet<String>> = OnceLock::new();
    ALL.get_or_init(|| {
        table()
            .values()
            .filter(|e| !e.is_ascii())
            .map(|e| e.replace('\u{fe0f}', ""))
            .collect()
    })
}

/// The emoji for a Teams emoticon id. Ids not in the table but named after
/// their code points (`1f600_…`, `1f468-200d-1f4bb_…`) are read from the name.
pub fn for_key(key: &str) -> Option<String> {
    if let Some(e) = table().get(key) {
        // Teams-only emoticons (`oreo`) have just their text shortcut.
        return (!e.is_ascii()).then(|| (*e).to_owned());
    }
    let (points, _) = key.split_once('_')?;
    points
        .split('-')
        .map(|h| u32::from_str_radix(h, 16).ok().and_then(char::from_u32))
        .collect::<Option<String>>()
        .filter(|s| !s.is_ascii())
}

/// Teams emotion key for an emoji (standard keys are names such as `like`,
/// per msteams-mcp's API reference, Reactions & Emoji).
pub fn key_for(emoji: &str) -> Option<String> {
    let normalized = emoji.replace('\u{fe0f}', "");
    if normalized == "👍" {
        return Some("like".into());
    }
    let mut matches: Vec<(&str, usize)> = table()
        .iter()
        .filter_map(|(key, value)| (value.replace('\u{fe0f}', "") == normalized).then_some((*key, key.len())))
        .collect();
    matches.sort_by_key(|(key, len)| (*len, *key != "like", *key));
    // Only keys from the table: the reference names no key form for other
    // Unicode emoji, so those are not sent.
    matches.first().map(|(key, _)| (*key).to_owned())
}

/// Teams' hooks for godwit-core (registered at startup): its own emoji set.
pub fn hooks() -> godwit_core::hooks::Hooks {
    godwit_core::hooks::Hooks {
        emoji_set: || Some(all()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_map_to_emoji() {
        assert_eq!(for_key("like").as_deref(), Some("👍"));
        assert_eq!(for_key("smile").as_deref(), Some("🙂"));
        assert_eq!(for_key("1f3aa_circustent").as_deref(), Some("🎪"));
        // Named after its code points, though not in the table.
        assert_eq!(for_key("1f9ff_somethingnew").as_deref(), Some("🧿"));
        assert_eq!(for_key("nonsense"), None);
        assert_eq!(for_key("oreo"), None, "Teams-only: no emoji");
        assert_eq!(for_key("0041_letter"), None, "plain letters aren't emoji");
    }

    #[test]
    fn the_table_is_only_data() {
        for line in TABLE.lines().filter(|l| !l.starts_with('#')) {
            let parts: Vec<&str> = line.split('\t').collect();
            assert_eq!(parts.len(), 3, "{line}");
            assert!(
                parts[0]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            );
            // An emoji, or a Teams-only emoticon's `(shortcut)`.
            assert!(
                !parts[1].is_ascii() || (parts[1].starts_with('(') && parts[1].ends_with(')')),
                "{line}"
            );
            assert!(!parts[1].contains(['<', '>', '&', '"']), "{line}");
        }
    }
}
