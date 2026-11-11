// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The Notifications page's list: every alert the app decided was worth
//! telling the user about, newest first, across every account and network.
//!
//! It's fed where `alerts` decides (see `shell/alerting.rs`), before Do Not
//! Disturb, an account's Muted mode or muzzle mode keep a banner or sound
//! from going out: catching up on what those kept quiet is the point.
//! Godwit's own (no network has it), so it only knows core types. Memory only
//! and bounded; deleted or since-read messages stay listed (it's a log of
//! alerts, not a view of the chats), and Clear empties it.

use std::collections::VecDeque;
use std::sync::Arc;

use godwit_core::application::{AlertKind, AppState};
use godwit_core::ids::{MessageId, RoomId, UserId};
use godwit_core::room::RoomKind;

/// How many are kept; the oldest go first. A fixed budget: a few hundred
/// bytes each, so well under 100 KB however long Godwit runs.
pub const KEEP: usize = 200;
/// Previews are one line: longer text is cut here.
const PREVIEW_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub room: RoomId,
    pub kind: AlertKind,
    /// Who it's from, when known (for the profile picture's fallback).
    pub from: Option<UserId>,
    /// The author's name, or the alert's title when nothing better is known.
    pub author: Arc<str>,
    pub avatar: Option<Arc<str>>,
    pub preview: Arc<str>,
    /// The message it's about, to jump to, when known.
    pub message: Option<MessageId>,
    /// When it came, in Unix milliseconds.
    pub at_ms: i64,
}

#[derive(Debug, Default)]
pub struct Notifications {
    /// Newest first.
    list: VecDeque<Notification>,
    /// How many of the newest haven't been seen (the badge).
    unseen: usize,
}

impl Notifications {
    /// A new alert, on top; the oldest drops past [`KEEP`].
    pub fn push(&mut self, mut n: Notification) {
        if n.preview.chars().count() > PREVIEW_CHARS {
            let mut cut: String = n.preview.chars().take(PREVIEW_CHARS).collect();
            cut.push('…');
            n.preview = cut.into();
        }
        self.list.push_front(n);
        self.list.truncate(KEEP);
        self.unseen = (self.unseen + 1).min(self.list.len());
    }

    /// Newest first.
    pub fn iter(&self) -> impl Iterator<Item = &Notification> {
        self.list.iter()
    }

    pub fn get(&self, index: usize) -> Option<&Notification> {
        self.list.get(index)
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn unseen(&self) -> usize {
        self.unseen
    }

    /// Whether the row at `index` came since the page was last looked at.
    pub fn is_unseen(&self, index: usize) -> bool {
        index < self.unseen
    }

    /// The page was looked at: the badge clears.
    pub fn mark_seen(&mut self) {
        self.unseen = 0;
    }

    pub fn clear(&mut self) {
        self.list = VecDeque::new();
        self.unseen = 0;
    }
}

/// The page's rows, newest first. `avatar` gives a picture when it's loaded
/// (and starts loading it); it's only asked while the page is open.
pub fn rows(
    list: &Notifications,
    state: &AppState,
    now_ms: i64,
    mut avatar: impl FnMut(Option<&str>) -> (slint::Image, bool),
) -> Vec<crate::ui::NotificationItem> {
    list.iter()
        .map(|n| {
            let place = match state.room(&n.room).map(|r| (&r.kind, &r.name)) {
                Some((RoomKind::Channel { server }, name)) => state
                    .server(server)
                    .map_or_else(|| name.to_string(), |s| format!("{} · {name}", s.name)),
                // The author already names a direct chat.
                Some((RoomKind::Direct { .. }, _)) => "Direct message".to_owned(),
                Some((_, name)) => name.to_string(),
                // A chat no longer listed (left, or its account signed out).
                None => n.room.account.network.name().to_owned(),
            };
            let (avatar, has_avatar) = avatar(n.avatar.as_deref());
            crate::ui::NotificationItem {
                network: n.room.account.network.key().into(),
                place: place.into(),
                author: n.author.as_ref().into(),
                initials: crate::view::initials(&n.author).into(),
                avatar,
                has_avatar,
                // One line: line breaks become spaces.
                preview: n.preview.replace(['\n', '\r'], " ").into(),
                when: ago(n.at_ms, now_ms).into(),
                call: n.kind == AlertKind::Call,
                // Filled by the caller, which has the emoji pictures.
                author_runs: Default::default(),
                preview_runs: Default::default(),
            }
        })
        .collect()
}

/// How long ago, as short as the row allows: "now", "5m", "3h", then the
/// day or date (as chat lists show it). Shown at each refresh; nothing ticks.
pub fn ago(at_ms: i64, now_ms: i64) -> String {
    let mins = (now_ms - at_ms).max(0) / 60_000;
    match mins {
        0 => "now".into(),
        1..=59 => format!("{mins}m"),
        60..=1439 => format!("{}h", mins / 60),
        _ => crate::view::last_talked(at_ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use godwit_core::ids::AccountId;
    use godwit_core::network::Network;

    fn note(i: usize) -> Notification {
        let account = AccountId::new(Network::Stoat, "me");
        Notification {
            room: RoomId::new(account, "r"),
            kind: AlertKind::Message,
            from: None,
            author: "Ann".into(),
            avatar: None,
            preview: format!("message {i}").into(),
            message: Some(MessageId(format!("m{i}").into())),
            at_ms: i as i64,
        }
    }

    fn previews(n: &Notifications) -> Vec<String> {
        n.iter().map(|n| n.preview.to_string()).collect()
    }

    #[test]
    fn newest_first_and_bounded() {
        let mut n = Notifications::default();
        for i in 0..3 {
            n.push(note(i));
        }
        assert_eq!(previews(&n), ["message 2", "message 1", "message 0"]);
        for i in 3..KEEP + 50 {
            n.push(note(i));
        }
        assert_eq!(n.len(), KEEP);
        assert_eq!(n.get(0).map(|n| n.at_ms), Some((KEEP + 49) as i64));
        assert_eq!(n.get(KEEP - 1).map(|n| n.at_ms), Some(50), "the oldest went");
    }

    #[test]
    fn counts_unseen_until_looked_at() {
        let mut n = Notifications::default();
        n.push(note(0));
        n.push(note(1));
        assert_eq!(n.unseen(), 2);
        assert!(n.is_unseen(0) && n.is_unseen(1) && !n.is_unseen(2));
        n.mark_seen();
        assert_eq!(n.unseen(), 0);
        n.push(note(2));
        assert_eq!(n.unseen(), 1);
        assert!(n.is_unseen(0) && !n.is_unseen(1));
        // Never more unseen than are kept.
        for i in 0..KEEP * 2 {
            n.push(note(i));
        }
        assert_eq!(n.unseen(), KEEP);
    }

    #[test]
    fn clear_empties_it() {
        let mut n = Notifications::default();
        n.push(note(0));
        n.clear();
        assert!(n.is_empty());
        assert_eq!(n.unseen(), 0);
    }

    #[test]
    fn long_previews_are_cut() {
        let mut n = Notifications::default();
        n.push(Notification {
            preview: "x".repeat(500).into(),
            ..note(0)
        });
        assert_eq!(n.get(0).map(|n| n.preview.chars().count()), Some(PREVIEW_CHARS + 1));
    }

    #[test]
    fn ago_is_short() {
        let min = 60_000;
        assert_eq!(ago(0, 30_000), "now");
        assert_eq!(ago(0, 5 * min), "5m");
        assert_eq!(ago(0, 3 * 60 * min), "3h");
        assert_eq!(ago(10 * min, 0), "now", "a clock running behind");
    }
}
