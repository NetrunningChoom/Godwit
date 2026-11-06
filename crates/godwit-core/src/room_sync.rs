// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Keeping the app's rooms in step with a network's full list: what was
//! sent last, so only changes go out and rooms that went are removed.

use std::collections::HashMap;

use crate::application::Event;
use crate::ids::RoomId;
use crate::room::Room;

/// The rooms last sent to the app for one account.
#[derive(Debug, Default)]
pub struct RoomSync {
    sent: HashMap<RoomId, Room>,
}

impl RoomSync {
    /// The app now has exactly `rooms` (they went out with the account's
    /// `AccountReady`).
    pub fn reset(&mut self, rooms: impl IntoIterator<Item = Room>) {
        self.sent = rooms.into_iter().map(|r| (r.id.clone(), r)).collect();
    }

    /// Forgets what was sent (signed out).
    pub fn clear(&mut self) {
        self.sent.clear();
    }

    /// The room last sent with this raw id.
    pub fn room(&self, raw: &str) -> Option<&Room> {
        self.sent.values().find(|r| &*r.id.raw == raw)
    }

    /// `now` is every room the account has: new or changed ones go out as
    /// `RoomUpdated` (in `now`'s order), ones no longer there as `RoomRemoved`.
    pub fn sync(&mut self, now: impl IntoIterator<Item = Room>, out: &mut Vec<Event>) {
        let mut next = HashMap::new();
        for room in now {
            if self.sent.get(&room.id) != Some(&room) {
                out.push(Event::RoomUpdated(room.clone()));
            }
            next.insert(room.id.clone(), room);
        }
        let mut gone: Vec<RoomId> = self.sent.keys().filter(|id| !next.contains_key(*id)).cloned().collect();
        gone.sort_by(|a, b| a.raw.cmp(&b.raw));
        out.extend(gone.into_iter().map(Event::RoomRemoved));
        self.sent = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::AccountId;
    use crate::network::Network;
    use crate::room::{Capabilities, RoomEncryption, RoomKind};

    fn room(raw: &str, name: &str) -> Room {
        Room {
            id: RoomId::new(AccountId::new(Network::Slack, "me"), raw),
            name: name.into(),
            kind: RoomKind::Group,
            icon_url: None,
            capabilities: Capabilities::of(Network::Slack),
            encryption: RoomEncryption::Unencrypted,
            unread: false,
            mentions: 0,
            last_read: None,
            invited_by: None,
            presence: None,
            status: None,
            member_count: None,
            last_activity: 0,
            parent: None,
        }
    }

    fn kinds(out: &[Event]) -> Vec<String> {
        out.iter()
            .map(|e| match e {
                Event::RoomUpdated(r) => format!("+{}", r.id.raw),
                Event::RoomRemoved(id) => format!("-{}", id.raw),
                _ => "?".into(),
            })
            .collect()
    }

    #[test]
    fn sends_new_changed_and_removed_rooms_only() {
        let mut sync = RoomSync::default();
        let mut out = Vec::new();
        sync.sync([room("a", "A"), room("b", "B")], &mut out);
        assert_eq!(kinds(&out), ["+a", "+b"], "new");
        out.clear();
        sync.sync([room("a", "A"), room("b", "B")], &mut out);
        assert!(out.is_empty(), "unchanged");
        sync.sync([room("a", "A2"), room("b", "B")], &mut out);
        assert_eq!(kinds(&out), ["+a"], "changed");
        out.clear();
        sync.sync([room("b", "B")], &mut out);
        assert_eq!(kinds(&out), ["-a"], "removed");
        out.clear();
        sync.sync([], &mut out);
        assert_eq!(kinds(&out), ["-b"], "empty");
    }

    #[test]
    fn reset_sets_what_the_app_has() {
        let mut sync = RoomSync::default();
        sync.reset([room("a", "A"), room("b", "B")]);
        let mut out = Vec::new();
        sync.sync([room("a", "A")], &mut out);
        assert_eq!(kinds(&out), ["-b"]);
    }
}
