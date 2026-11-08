// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Home's own arrangement: pinned chats (in the user's order) and hidden
//! ones. A person merged across networks is one chat in Home, so pinning,
//! hiding and moving act on all of their rooms at once.

use super::{AppState, Application};
use crate::ids::RoomId;
use crate::room::RoomKind;

impl AppState {
    /// The direct room Home should open for a person. Keep the room already
    /// selected in this merged chat for the session; otherwise honour Talk on.
    pub fn preferred_chat_room(&self, room: &RoomId) -> RoomId {
        let Some(RoomKind::Direct { with }) = self.room(room).map(|r| r.kind.clone()) else {
            return room.clone();
        };
        let Some(contact) = self.contacts.of(&with) else {
            return room.clone();
        };
        let rooms = self.chat_rooms(room);
        if let Some(selected) = self.selected.as_ref().filter(|selected| rooms.contains(selected)) {
            return selected.clone();
        }
        contact
            .best_identity(|user| self.accounts.iter().any(|account| account.id == user.account))
            .and_then(|user| {
                rooms.into_iter().find(|candidate| {
                    self.room(candidate)
                        .is_some_and(|r| matches!(&r.kind, RoomKind::Direct { with } if with == user))
                })
            })
            .unwrap_or_else(|| room.clone())
    }

    /// The rooms that make up `room`'s Home chat: every DM with a person
    /// merged across networks (signed in on more than one of them, as Home
    /// merges them), else the room alone.
    pub fn chat_rooms(&self, room: &RoomId) -> Vec<RoomId> {
        let alone = || vec![room.clone()];
        let Some(RoomKind::Direct { with }) = self.room(room).map(|r| r.kind.clone()) else {
            return alone();
        };
        let Some(contact) = self.contacts.of(&with) else {
            return alone();
        };
        let signed_in = contact
            .identities
            .iter()
            .filter(|u| self.accounts.iter().any(|a| a.id == u.account))
            .count();
        if signed_in < 2 {
            return alone();
        }
        let mut rooms: Vec<RoomId> = self
            .direct_rooms()
            .filter(|o| matches!(&o.kind, RoomKind::Direct { with } if contact.identities.contains(with)))
            .map(|o| o.id.clone())
            .collect();
        if !rooms.contains(room) {
            rooms.push(room.clone());
        }
        rooms
    }

    /// Whether `room`'s Home chat is pinned (through any of its rooms).
    pub fn is_pinned(&self, room: &RoomId) -> bool {
        self.chat_rooms(room).iter().any(|r| self.pinned_rooms.contains(r))
    }

    /// Whether `room`'s Home chat is in the hidden section.
    pub fn is_hidden(&self, room: &RoomId) -> bool {
        !self.is_pinned(room) && self.chat_rooms(room).iter().any(|r| self.hidden_rooms.contains(r))
    }

    /// The pinned chats Home shows, in order (by each chat's first pin),
    /// and the slots in `pinned_rooms` their pins take. Pins of rooms not
    /// loaded (an account still connecting) are left out and keep their slot.
    fn pinned_chats(&self) -> (Vec<usize>, Vec<Vec<RoomId>>) {
        let mut slots = Vec::new();
        let mut chats: Vec<Vec<RoomId>> = Vec::new();
        for (slot, id) in self.pinned_rooms.iter().enumerate() {
            if self.room(id).is_none() {
                continue;
            }
            slots.push(slot);
            match chats
                .iter_mut()
                .find(|c| c.first().is_some_and(|f| self.chat_rooms(f).contains(id)))
            {
                Some(chat) => chat.push(id.clone()),
                None => chats.push(vec![id.clone()]),
            }
        }
        (slots, chats)
    }

    /// How many pinned chats are above `room`'s, and below it: how far it
    /// can move (0, 0 when it isn't pinned).
    pub fn pin_moves(&self, room: &RoomId) -> (usize, usize) {
        let (_, chats) = self.pinned_chats();
        let mine = self.chat_rooms(room);
        match chats.iter().position(|c| c.iter().any(|r| mine.contains(r))) {
            Some(at) => (at, chats.len() - at - 1),
            None => (0, 0),
        }
    }
}

impl Application {
    pub(super) fn pin_room(&mut self, room: RoomId, pinned: bool) {
        let chat = self.state.chat_rooms(&room);
        if pinned {
            self.state.hidden_rooms.retain(|r| !chat.contains(r));
            // Already pinned through another of the person's rooms: it stays
            // where it is.
            if !chat.iter().any(|r| self.state.pinned_rooms.contains(r)) {
                self.state.pinned_rooms.push(room);
            }
        } else {
            self.state.pinned_rooms.retain(|r| !chat.contains(r));
        }
    }

    pub(super) fn hide_room(&mut self, room: RoomId, hidden: bool) {
        let chat = self.state.chat_rooms(&room);
        if hidden {
            self.state.pinned_rooms.retain(|r| !chat.contains(r));
            if !chat.iter().any(|r| self.state.hidden_rooms.contains(r)) {
                self.state.hidden_rooms.push(room);
            }
        } else {
            self.state.hidden_rooms.retain(|r| !chat.contains(r));
        }
    }

    /// Moves `room`'s pinned chat `by` places among the pins (negative: up),
    /// stopping at the ends.
    pub(super) fn move_pin(&mut self, room: &RoomId, by: i32) {
        let (slots, mut chats) = self.state.pinned_chats();
        let mine = self.state.chat_rooms(room);
        let Some(at) = chats.iter().position(|c| c.iter().any(|r| mine.contains(r))) else {
            return;
        };
        let last = chats.len() - 1;
        let steps = usize::try_from(by.unsigned_abs()).unwrap_or(usize::MAX);
        let to = if by < 0 {
            at.saturating_sub(steps)
        } else {
            at.saturating_add(steps).min(last)
        };
        if to == at {
            return;
        }
        let chat = chats.remove(at);
        chats.insert(to, chat);
        for (slot, id) in slots.into_iter().zip(chats.into_iter().flatten()) {
            if let Some(s) = self.state.pinned_rooms.get_mut(slot) {
                *s = id;
            }
        }
    }

    /// An account's notes chat (Stoat's Saved Notes and the like) starts
    /// pinned at the top, once per account: unpinned later, it stays so.
    pub(super) fn pin_new_notes(&mut self) {
        let fresh: Vec<RoomId> = self
            .state
            .rooms
            .iter()
            .filter(|r| matches!(r.kind, RoomKind::Notes) && !self.state.pins_seeded.contains(&r.id.account))
            .map(|r| r.id.clone())
            .collect();
        for id in fresh {
            if self.state.pins_seeded.contains(&id.account) {
                continue;
            }
            self.state.pins_seeded.push(id.account.clone());
            if !self.state.pinned_rooms.contains(&id) && !self.state.hidden_rooms.contains(&id) {
                self.state.pinned_rooms.insert(0, id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::{Action, Event};
    use crate::application::{Command, Effect};
    use crate::ids::{AccountId, UserId};
    use crate::network::Network;
    use crate::room::{Capabilities, Room, RoomEncryption};

    fn account(n: Network) -> AccountId {
        AccountId::new(n, "me")
    }

    fn room(n: Network, raw: &str, kind: RoomKind) -> Room {
        Room {
            notify: Default::default(),
            id: RoomId::new(account(n), raw),
            name: raw.into(),
            kind,
            icon_url: None,
            capabilities: Capabilities::of(n),
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

    fn dm(n: Network, raw: &str, with: &str) -> Room {
        room(
            n,
            raw,
            RoomKind::Direct {
                with: UserId::new(account(n), with),
            },
        )
    }

    fn ready(app: &mut Application, n: Network, rooms: Vec<Room>) {
        app.apply(Event::AccountReady {
            account: account(n),
            display_name: "Me".into(),
            username: "me".into(),
            avatar_url: None,
            rooms,
            servers: Vec::new(),
        });
    }

    fn id(n: Network, raw: &str) -> RoomId {
        RoomId::new(account(n), raw)
    }

    fn pin(app: &mut Application, n: Network, raw: &str) {
        app.dispatch(Action::PinRoom {
            room: id(n, raw),
            pinned: true,
        });
    }

    fn pins(app: &Application) -> Vec<&str> {
        app.state().pinned_rooms.iter().map(|r| r.raw.as_ref()).collect()
    }

    #[test]
    fn pins_move_up_and_down_and_stop_at_the_ends() {
        let mut app = Application::default();
        let s = Network::Stoat;
        ready(&mut app, s, vec![dm(s, "a", "x"), dm(s, "b", "y"), dm(s, "c", "z")]);
        for r in ["a", "b", "c"] {
            pin(&mut app, s, r);
        }
        assert_eq!(app.state().pin_moves(&id(s, "a")), (0, 2));
        assert_eq!(app.state().pin_moves(&id(s, "b")), (1, 1));
        app.dispatch(Action::MovePin {
            room: id(s, "c"),
            by: -1,
        });
        assert_eq!(pins(&app), ["a", "c", "b"]);
        app.dispatch(Action::MovePin {
            room: id(s, "a"),
            by: 1,
        });
        assert_eq!(pins(&app), ["c", "a", "b"]);
        // At the ends nothing moves.
        app.dispatch(Action::MovePin {
            room: id(s, "c"),
            by: -1,
        });
        app.dispatch(Action::MovePin {
            room: id(s, "b"),
            by: 1,
        });
        assert_eq!(pins(&app), ["c", "a", "b"]);
        assert_eq!(app.state().pin_moves(&id(s, "b")), (2, 0));
    }

    #[test]
    fn a_drag_moves_a_pin_several_places_and_clamps() {
        let mut app = Application::default();
        let s = Network::Stoat;
        ready(
            &mut app,
            s,
            vec![dm(s, "a", "x"), dm(s, "b", "y"), dm(s, "c", "z"), dm(s, "d", "w")],
        );
        for r in ["a", "b", "c", "d"] {
            pin(&mut app, s, r);
        }
        app.dispatch(Action::MovePin {
            room: id(s, "a"),
            by: 2,
        });
        assert_eq!(pins(&app), ["b", "c", "a", "d"]);
        app.dispatch(Action::MovePin {
            room: id(s, "d"),
            by: -9,
        });
        assert_eq!(pins(&app), ["d", "b", "c", "a"]);
        app.dispatch(Action::MovePin {
            room: id(s, "b"),
            by: 9,
        });
        assert_eq!(pins(&app), ["d", "c", "a", "b"]);
    }

    #[test]
    fn moving_skips_pins_of_rooms_not_loaded_and_keeps_their_place() {
        let mut app = Application::default();
        let s = Network::Stoat;
        ready(&mut app, s, vec![dm(s, "a", "x"), dm(s, "b", "y")]);
        app.dispatch(Action::RestoreRoomPrefs {
            pinned: vec![id(s, "a"), id(Network::Discord, "gone"), id(s, "b")],
            hidden: Vec::new(),
        });
        app.dispatch(Action::MovePin {
            room: id(s, "b"),
            by: -1,
        });
        assert_eq!(pins(&app), ["b", "gone", "a"]);
    }

    #[test]
    fn unpinning_a_merged_person_unpins_every_network() {
        let mut app = Application::default();
        let (s, d) = (Network::Stoat, Network::Discord);
        ready(&mut app, s, vec![dm(s, "a", "x")]);
        ready(&mut app, d, vec![dm(d, "b", "x2")]);
        let one = app.state.contacts.ensure(UserId::new(account(s), "x"), "X");
        let two = app.state.contacts.ensure(UserId::new(account(d), "x2"), "X");
        app.state.contacts.merge(one, two);
        assert_eq!(app.state().chat_rooms(&id(s, "a")).len(), 2);
        // Pinned through one network, then the row opens the other.
        pin(&mut app, s, "a");
        assert!(app.state().is_pinned(&id(d, "b")));
        pin(&mut app, d, "b");
        assert_eq!(pins(&app), ["a"], "already pinned: not twice");
        app.dispatch(Action::PinRoom {
            room: id(d, "b"),
            pinned: false,
        });
        assert!(pins(&app).is_empty());
        assert!(!app.state().is_pinned(&id(s, "a")));
    }

    #[test]
    fn same_network_accounts_share_a_chat_and_resolve_to_talk_on() {
        let mut app = Application::default();
        let first = account(Network::Stoat);
        let second = AccountId::new(Network::Stoat, "work");
        app.apply(Event::AccountReady {
            account: first.clone(),
            display_name: "Personal".into(),
            username: "me".into(),
            avatar_url: None,
            rooms: vec![dm(Network::Stoat, "personal-dm", "alex")],
            servers: Vec::new(),
        });
        let work_user = UserId::new(second.clone(), "alex");
        app.apply(Event::AccountReady {
            account: second.clone(),
            display_name: "Work".into(),
            username: "me".into(),
            avatar_url: None,
            rooms: vec![Room {
                notify: Default::default(),
                id: RoomId::new(second.clone(), "work-dm"),
                name: "Alex".into(),
                kind: RoomKind::Direct {
                    with: work_user.clone(),
                },
                icon_url: None,
                capabilities: Capabilities::of(Network::Stoat),
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
            }],
            servers: Vec::new(),
        });
        let contact = app.state.contacts.ensure(UserId::new(first.clone(), "alex"), "Alex");
        let other = app.state.contacts.ensure(work_user.clone(), "Alex");
        app.state.contacts.merge(contact, other);
        app.dispatch(Action::PreferIdentity {
            contact,
            user: work_user,
        });

        let personal = RoomId::new(first.clone(), "personal-dm");
        let work = RoomId::new(second.clone(), "work-dm");
        assert_eq!(app.state().chat_rooms(&personal).len(), 2);
        assert_eq!(app.state().preferred_chat_room(&personal), work);
        app.dispatch(Action::SelectRoom(personal));
        assert_eq!(app.state().chat_rooms(&work).len(), 2);
        app.dispatch(Action::SelectRoom(work.clone()));
        app.dispatch(Action::SetDraft("hello".into()));
        let effects = app.dispatch(Action::Send { now_ms: 1 });
        assert!(effects.iter().any(|effect| matches!(effect, Effect::Backend { account, command: Command::SendMessage { room, .. } } if account == &second && room == &work)));
    }

    #[test]
    fn notes_start_pinned_at_the_top_once_per_account() {
        let mut app = Application::default();
        let s = Network::Stoat;
        app.dispatch(Action::RestoreRoomPrefs {
            pinned: vec![id(s, "a")],
            hidden: Vec::new(),
        });
        ready(&mut app, s, vec![dm(s, "a", "x"), room(s, "notes", RoomKind::Notes)]);
        assert_eq!(pins(&app), ["notes", "a"]);
        assert_eq!(app.state().pins_seeded, vec![account(s)]);
        // Unpinned, it stays unpinned when the account comes back.
        app.dispatch(Action::PinRoom {
            room: id(s, "notes"),
            pinned: false,
        });
        ready(&mut app, s, vec![dm(s, "a", "x"), room(s, "notes", RoomKind::Notes)]);
        assert_eq!(pins(&app), ["a"]);
    }

    #[test]
    fn kept_seeds_stop_notes_from_pinning_again_after_a_restart() {
        let mut app = Application::default();
        let s = Network::Stoat;
        app.dispatch(Action::RestorePinSeeds(vec![account(s)]));
        ready(&mut app, s, vec![room(s, "notes", RoomKind::Notes)]);
        assert!(pins(&app).is_empty());
        // A notes chat that arrives later still pins on a new account.
        let d = Network::Discord;
        ready(&mut app, d, Vec::new());
        app.apply(Event::RoomUpdated(room(d, "n", RoomKind::Notes)));
        assert_eq!(pins(&app), ["n"]);
    }
}
