// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Stickers a room can send: a network lists the ones the account may use
//! (Discord: the current server's own, as Nitro is what unlocks the rest);
//! the picker's Stickers tab shows them, and picking one sends it at once.

use std::sync::Arc;

use crate::ids::ServerId;

/// One sticker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sticker {
    /// What sending names (the network's own id).
    pub id: Arc<str>,
    pub name: Arc<str>,
    /// Search keywords besides the name (comma separated on Discord).
    pub tags: Arc<str>,
    /// A still or animated picture (an animated one shows its first frame).
    pub image_url: Arc<str>,
    /// False while the network refuses it (Discord: the server lost its boost).
    pub available: bool,
}

impl Sticker {
    /// Whether the picker's search (`query` lowercased, "" matches all) finds it.
    pub fn matches(&self, query: &str) -> bool {
        query.is_empty() || self.name.to_lowercase().contains(query) || self.tags.to_lowercase().contains(query)
    }
}

/// A server's stickers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StickerGroup {
    pub server: ServerId,
    pub title: Arc<str>,
    pub stickers: Vec<Sticker>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_finds_names_and_tags() {
        let s = Sticker {
            id: "1".into(),
            name: "Wave".into(),
            tags: "hello, 👋".into(),
            image_url: "".into(),
            available: true,
        };
        assert!(s.matches(""));
        assert!(s.matches("wav"));
        assert!(s.matches("hello"));
        assert!(!s.matches("bye"));
    }
}
