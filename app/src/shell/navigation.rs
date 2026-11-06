// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Moving around: rooms, servers and accounts on the rail, Home, people, profiles, invites and joining servers.

use super::*;

impl Shell {
    /// Fetches the invite cards on screen, once each (as the official
    /// element's query, which never refetches).
    pub(super) fn load_invites(&mut self, ui: &AppWindow) {
        let Some((top, bottom)) = self.presenter.window else {
            return;
        };
        let wanted: Vec<_> = self
            .presenter
            .rows
            .iter()
            .enumerate()
            .filter(|(i, _)| *i >= top && *i <= bottom)
            .flat_map(|(_, row)| row.invite_codes().map(|(a, c)| (a.clone(), c.clone())))
            .filter(|key| !self.app.state().invites.contains_key(key))
            .collect();
        for (account, code) in wanted {
            self.act(ui, Action::LoadInvite { account, code });
        }
    }

    /// An invite card's Join, for the account whose message showed it.
    pub fn join_invite(&mut self, ui: &AppWindow, code: &str) {
        let account = self
            .presenter
            .rows
            .iter()
            .flat_map(|row| row.invite_codes())
            .find(|(_, c)| c.as_ref() == code)
            .map(|(a, c)| (a.clone(), c.clone()));
        if let Some((account, code)) = account {
            self.act(ui, Action::JoinInvite { account, code });
        }
    }

    pub fn open_join_server(&mut self, ui: &AppWindow) {
        self.join_accounts = self
            .app
            .state()
            .accounts
            .iter()
            .filter(|a| self.account(&a.id).is_some_and(|a| a.supports_invites()))
            .map(|a| a.id.clone())
            .collect();
        let labels: Vec<SharedString> = self
            .join_accounts
            .iter()
            .filter_map(|id| {
                self.app
                    .state()
                    .accounts
                    .iter()
                    .find(|a| &a.id == id)
                    .map(|a| format!("{} — {}", a.display_name, a.id.network.name()).into())
            })
            .collect();
        ui.set_join_server_accounts(ModelRc::new(VecModel::from(labels)));
        self.reset_join_server(ui);
        if self.join_accounts.is_empty() {
            ui.set_join_server_status("Sign in to an account that supports server invites first.".into());
        }
        ui.set_join_server_open(true);
    }

    pub fn close_join_server(&mut self, ui: &AppWindow) {
        self.reset_join_server(ui);
        self.join_accounts.clear();
        ui.set_join_server_open(false);
    }

    pub fn reset_join_server(&mut self, ui: &AppWindow) {
        self.join_preview = None;
        self.join_submitted = false;
        self.join_finished = false;
        ui.set_join_server_name("".into());
        ui.set_join_server_status("Paste an invite to preview its server before joining.".into());
        ui.set_join_server_ready(false);
        ui.set_join_server_busy(false);
        ui.set_join_server_failed(false);
    }

    pub fn inspect_invite(&mut self, ui: &AppWindow, row: i32, input: &str) {
        let account = usize::try_from(row)
            .ok()
            .and_then(|i| self.join_accounts.get(i))
            .cloned();
        self.reset_join_server(ui);
        let Some(account) = account else { return };
        let code = self.account(&account).and_then(|a| a.parse_invite(input));
        let Some(code) = code else {
            ui.set_join_server_status("Enter a valid invite link or code for this account.".into());
            ui.set_join_server_failed(true);
            return;
        };
        let code: std::sync::Arc<str> = code.into();
        self.join_preview = Some((account.clone(), code.clone()));
        self.act(ui, Action::LoadInvite { account, code });
    }

    pub fn accept_server_invite(&mut self, ui: &AppWindow) {
        let Some((account, code)) = self.join_preview.clone() else {
            return;
        };
        if !ui.get_join_server_ready() || ui.get_join_server_busy() {
            return;
        }
        self.join_submitted = true;
        self.act(ui, Action::JoinInvite { account, code });
    }

    pub(super) fn show_join_server(&mut self, ui: &AppWindow) {
        use godwit_core::message::Invite;
        if !ui.get_join_server_open() {
            return;
        }
        if self.join_finished {
            self.close_join_server(ui);
            return;
        }
        let Some(key) = self.join_preview.as_ref() else { return };
        if !self.app.state().accounts.iter().any(|a| a.id == key.0) {
            self.close_join_server(ui);
            return;
        }
        let state = self.app.state();
        let invite = state.invites.get(key);
        let preview = match invite {
            Some(Invite::Ready(p) | Invite::Joining(p)) => Some(p),
            _ => None,
        };
        // Both a successful join and opening a server already joined go
        // through the core's server selection, including in demo mode.
        if self.join_submitted
            && let Some(server) = preview.and_then(|p| p.server.as_ref())
            && state.server(server).is_some()
            && state.selected_server.as_ref() == Some(server)
        {
            self.close_join_server(ui);
            return;
        }
        ui.set_join_server_name(preview.map(|p| SharedString::from(p.name.as_ref())).unwrap_or_default());
        ui.set_join_server_ready(matches!(invite, Some(Invite::Ready(_))));
        ui.set_join_server_busy(matches!(invite, Some(Invite::Loading | Invite::Joining(_))));
        ui.set_join_server_failed(matches!(invite, Some(Invite::Invalid)));
        let status = match invite {
            Some(Invite::Loading) | None => "Loading invite…".to_owned(),
            Some(Invite::Invalid) => "This invite is invalid, expired, or unavailable.".to_owned(),
            Some(Invite::Joining(_)) => "Joining server…".to_owned(),
            Some(Invite::Ready(p)) => {
                if self.join_submitted {
                    self.join_submitted = false;
                    "Could not join. See Settings → Problems for details; you can try again.".to_owned()
                } else {
                    p.members
                        .map_or_else(|| "Ready to join.".to_owned(), |n| format!("{n} members"))
                }
            }
        };
        ui.set_join_server_status(status.into());
    }

    pub fn send_friend_request(&mut self, ui: &AppWindow, key: &str, name: String) {
        let account = self
            .app
            .state()
            .accounts
            .iter()
            .find(|a| a.id.to_string() == key && a.id.network.has_friends())
            .map(|a| a.id.clone());
        if let Some(account) = account {
            self.act(ui, Action::SendFriendRequest { account, name });
        } else {
            self.apply_all(vec![Event::FriendRequestFormError(
                "That account is no longer available. Choose a signed-in account.".into(),
            )]);
            self.refresh(ui);
        }
    }

    /// The person whose chat is open, as a contact.
    pub fn open_contact(&self) -> Option<godwit_core::contacts::ContactId> {
        let state = self.app.state();
        let room = state.selected.as_ref().and_then(|r| state.room(r))?;
        match &room.kind {
            godwit_core::room::RoomKind::Direct { with } => state.contacts.of(with).map(|c| c.id),
            _ => None,
        }
    }

    /// The server in rail row `row` (None for an account's row).
    pub fn server_at(&self, row: usize) -> Option<ServerId> {
        match self.presenter.server_rows.get(row)? {
            view::RailRow::Server(id) => Some(id.clone()),
            view::RailRow::Account(_) | view::RailRow::Pending(_) | view::RailRow::SignIn(_) => None,
        }
    }

    /// An account's rail row was clicked: the "you" views (its profile,
    /// presence, status) act on it until another room or server is opened.
    pub fn open_account(&mut self, ui: &AppWindow, row: usize) {
        if let Some(view::RailRow::Account(id)) = self.presenter.server_rows.get(row) {
            self.presenter.focus = Some(id.clone());
            self.refresh(ui);
        }
    }

    /// The account the "you" views act on (see `present::focus_account`).
    pub fn focus_account(&self) -> Option<AccountId> {
        crate::present::focus_account(self.app.state(), self.presenter.focus.as_ref()).map(|a| a.id.clone())
    }

    /// A Home row's menu: 0 pin/unpin, 1 mark read, 2 mark unread, 3 hide/unhide,
    /// 4 muzzle mode on/off for this chat, 5 its notifications (cycles).
    pub fn room_menu(&mut self, ui: &AppWindow, row: i32, choice: i32) {
        let Some(room) = usize::try_from(row).ok().and_then(|r| self.room_at(r)) else {
            return;
        };
        if choice == 5 {
            self.settings.cycle_room_alerts(&room);
            self.presenter.muzzle = self.settings.clone();
            self.save_settings();
            self.refresh(ui);
            return;
        }
        if choice == 4 {
            let key = settings::room_key(&room);
            let on = !self.settings.muzzled(&room);
            self.settings.muzzle_rooms.retain(|k| *k != key);
            self.settings.unmuzzle_rooms.retain(|k| *k != key);
            // Only a choice that differs from the default is kept.
            if on != self.settings.muzzle {
                if on {
                    self.settings.muzzle_rooms.push(key);
                } else {
                    self.settings.unmuzzle_rooms.push(key);
                }
            }
            self.presenter.muzzle = self.settings.clone();
            self.save_settings();
            self.refresh(ui);
            return;
        }
        let state = self.app.state();
        let action = match choice {
            0 => Action::PinRoom {
                pinned: !state.pinned_rooms.contains(&room),
                room,
            },
            1 => Action::MarkRoomRead(room),
            2 => Action::MarkRoomUnread(room),
            _ => Action::HideRoom {
                hidden: !state.hidden_rooms.contains(&room),
                room,
            },
        };
        self.act(ui, action);
        self.save_room_prefs();
    }

    /// A server moved on the rail: up (-1), down (1) or to the top (0) of
    /// its account's servers. The whole account's order is kept from then on.
    pub fn move_server(&mut self, ui: &AppWindow, row: i32, delta: i32) {
        let Some(view::RailRow::Server(id)) = usize::try_from(row)
            .ok()
            .and_then(|r| self.presenter.server_rows.get(r))
            .cloned()
        else {
            return;
        };
        // The account's servers as shown now.
        let shown: Vec<String> = self
            .presenter
            .server_rows
            .iter()
            .filter_map(|r| match r {
                view::RailRow::Server(s) if s.account == id.account => Some(settings::server_key(s)),
                _ => None,
            })
            .collect();
        let key = settings::server_key(&id);
        let Some(at) = shown.iter().position(|k| *k == key) else {
            return;
        };
        let to = match delta {
            0 => 0,
            d if d < 0 => at.saturating_sub(1),
            _ => (at + 1).min(shown.len() - 1),
        };
        self.place_server(ui, &id, shown, at, to);
    }

    /// A server dragged on the rail by `steps` rows (up negative), within
    /// its account's servers.
    pub fn move_server_by(&mut self, ui: &AppWindow, row: i32, steps: i32) {
        let Some(view::RailRow::Server(id)) = usize::try_from(row)
            .ok()
            .and_then(|r| self.presenter.server_rows.get(r))
            .cloned()
        else {
            return;
        };
        let shown: Vec<String> = self
            .presenter
            .server_rows
            .iter()
            .filter_map(|r| match r {
                view::RailRow::Server(s) if s.account == id.account => Some(settings::server_key(s)),
                _ => None,
            })
            .collect();
        let key = settings::server_key(&id);
        let Some(at) = shown.iter().position(|k| *k == key) else {
            return;
        };
        let last = i64::try_from(shown.len()).unwrap_or(1) - 1;
        let to = usize::try_from((i64::try_from(at).unwrap_or(0) + i64::from(steps)).clamp(0, last)).unwrap_or(at);
        if to != at {
            self.place_server(ui, &id, shown, at, to);
        }
    }

    /// An account dragged on the rail by `steps` rows (up negative): it goes
    /// where the account whose section it was dropped on is.
    pub fn move_account_by(&mut self, ui: &AppWindow, row: i32, steps: i32) {
        let rows = &self.presenter.server_rows;
        let Some(view::RailRow::Account(id)) = usize::try_from(row).ok().and_then(|r| rows.get(r)).cloned() else {
            return;
        };
        let last = i64::try_from(rows.len()).unwrap_or(1) - 1;
        let target = usize::try_from((i64::from(row) + i64::from(steps)).clamp(0, last)).unwrap_or(0);
        // The section the drop landed in: the nearest account row at or above it.
        let Some(onto) = rows[..=target].iter().rev().find_map(|r| match r {
            view::RailRow::Account(a) => Some(a.clone()),
            view::RailRow::Server(_) | view::RailRow::Pending(_) | view::RailRow::SignIn(_) => None,
        }) else {
            return;
        };
        let mut order: Vec<String> = view::ordered_accounts(self.app.state(), &self.settings.account_order)
            .iter()
            .map(|a| settings::account_key(&a.id))
            .collect();
        let (Some(at), Some(to)) = (
            order.iter().position(|k| *k == settings::account_key(&id)),
            order.iter().position(|k| *k == settings::account_key(&onto)),
        ) else {
            return;
        };
        if at == to {
            return;
        }
        let moved = order.remove(at);
        order.insert(to, moved);
        self.settings.account_order = order;
        self.presenter.account_order = self.settings.account_order.clone();
        self.save_settings();
        self.refresh(ui);
    }

    /// Moves a server from `at` to `to` in its account's `shown` order, and keeps it.
    pub(super) fn place_server(&mut self, ui: &AppWindow, id: &ServerId, mut shown: Vec<String>, at: usize, to: usize) {
        let moved = shown.remove(at);
        shown.insert(to, moved);
        let prefix = format!("{}|", settings::account_key(&id.account));
        self.settings.server_order.retain(|k| !k.starts_with(&prefix));
        self.settings.server_order.extend(shown);
        self.presenter.server_order = self.settings.server_order.clone();
        self.save_settings();
        self.refresh(ui);
    }

    pub fn search_home(&mut self, ui: &AppWindow, query: String) {
        self.presenter.home_query = query;
        self.refresh(ui);
    }

    /// Home's network chip picked ("" for all).
    pub fn filter_home(&mut self, ui: &AppWindow, network: String) {
        self.presenter.home_network = network;
        self.refresh(ui);
    }

    /// A category heading clicked: fold or unfold it, kept between starts.
    pub fn toggle_category(&mut self, ui: &AppWindow, row: i32) {
        let Some(key) = usize::try_from(row)
            .ok()
            .and_then(|r| self.presenter.category_rows.get(r).cloned().flatten())
        else {
            return;
        };
        let list = &mut self.settings.collapsed_categories;
        if let Some(at) = list.iter().position(|k| *k == key) {
            list.remove(at);
        } else {
            list.push(key);
        }
        self.presenter.collapsed_categories = self.settings.collapsed_categories.clone();
        self.save_settings();
        self.refresh(ui);
    }

    pub fn toggle_hidden_rooms(&mut self, ui: &AppWindow) {
        self.act(ui, Action::ToggleHiddenRooms);
    }

    /// Keeps Home's pins and hidden chats (a new message unhides one, so
    /// this runs after every change, cheaply).
    pub(super) fn save_room_prefs(&mut self) {
        let state = self.app.state();
        let pinned: Vec<String> = state.pinned_rooms.iter().map(settings::room_key).collect();
        let hidden: Vec<String> = state.hidden_rooms.iter().map(settings::room_key).collect();
        if pinned != self.settings.pinned_rooms || hidden != self.settings.hidden_rooms {
            self.settings.pinned_rooms = pinned;
            self.settings.hidden_rooms = hidden;
            self.save_settings();
        }
    }

    pub fn room_at(&self, row: usize) -> Option<RoomId> {
        self.presenter.room_rows.get(row).cloned().flatten()
    }

    pub(super) fn observe_invite_result(&mut self, event: &Event) {
        if let Event::InviteJoined { account, code, .. } = event
            && self
                .join_preview
                .as_ref()
                .is_some_and(|(a, c)| a == account && c == code)
        {
            self.join_finished = true;
        }
    }

    /// The open room, to join its call.
    pub fn selected_room(&self) -> Option<RoomId> {
        self.app.state().selected.clone()
    }

    /// The author of the message at timeline row `row`.
    pub fn author_at(&self, row: i32) -> Option<UserId> {
        let id = self.message_at(row)?;
        self.app
            .state()
            .selected_message(&id)
            .filter(|m| !m.system)
            .map(|m| m.author.clone())
    }

    pub fn member_at(&self, row: i32) -> Option<UserId> {
        self.presenter
            .member_rows
            .get(usize::try_from(row).ok()?)
            .cloned()
            .flatten()
    }

    /// Opens the room `delta` rows from the open one in the sidebar (headings skipped).
    pub fn adjacent_room(&mut self, ui: &AppWindow, delta: i32) {
        let rooms: Vec<RoomId> = self.presenter.room_rows.iter().flatten().cloned().collect();
        let at = self
            .app
            .state()
            .selected
            .as_ref()
            .and_then(|s| rooms.iter().position(|r| r == s));
        let next = match at {
            Some(i) => i.checked_add_signed(isize::try_from(delta).unwrap_or(0)),
            None => Some(0),
        };
        if let Some(room) = next.and_then(|i| rooms.get(i)).cloned() {
            self.act(ui, Action::SelectRoom(room));
        }
    }

    pub fn switcher_search(&mut self, ui: &AppWindow, query: &str) {
        let (rows, rooms) = view::switch_results(self.app.state(), query, &mut self.images);
        ui.set_switch_results(ModelRc::new(VecModel::from(rows)));
        self.switch_rows = rooms;
    }

    pub fn switcher_picked(&mut self, ui: &AppWindow, row: i32) {
        if let Some(room) = usize::try_from(row).ok().and_then(|r| self.switch_rows.get(r).cloned()) {
            self.act(ui, Action::SelectRoom(room));
        }
    }

    /// The reply preview at timeline row `row` was clicked: jump to the
    /// message it answers (loading the messages around it if needed).
    pub fn jump_to_reply(&mut self, ui: &AppWindow, row: i32) {
        let Some(id) = self.message_at(row) else { return };
        if let Some(target) = self.app.state().selected_message(&id).and_then(|m| m.reply_to.clone()) {
            self.act(ui, Action::JumpTo(target));
        }
    }

    /// "Message" on the open profile card.
    /// The card's friend buttons: request or accept (`add`), or cancel,
    /// decline or unfriend.
    pub fn befriend_profile(&mut self, ui: &AppWindow, add: bool) {
        if let Some((user, _)) = self.app.state().profile.clone() {
            self.act(
                ui,
                if add {
                    Action::AddFriend(user)
                } else {
                    Action::RemoveFriend(user)
                },
            );
        }
    }

    pub fn message_profile(&mut self, ui: &AppWindow) {
        if let Some((user, _)) = self.app.state().profile.clone() {
            self.act(ui, Action::CloseProfile);
            self.act(ui, Action::OpenDirect(user));
        }
    }

    /// The room in the directory's row `row`.
    pub fn directory_at(&self, row: i32) -> Option<RoomId> {
        self.presenter.directory_rows.get(usize::try_from(row).ok()?).cloned()
    }

    /// The room that's open.
    pub fn open_room(&self) -> Option<RoomId> {
        self.app.state().selected.clone()
    }

    /// Whose profile card is open.
    pub fn profile_user(&self) -> Option<UserId> {
        self.app.state().profile.as_ref().map(|(u, _)| u.clone())
    }

    /// The contact on People row `row`, and its accounts in chip order.
    pub fn person_at(&self, row: i32) -> Option<&(ContactId, Vec<UserId>)> {
        self.presenter.people_rows.get(usize::try_from(row).ok()?)
    }

    pub fn contact(&self, id: ContactId) -> Option<&Metacontact> {
        self.app.state().contacts.get(id)
    }

    /// The pending friend request on row `row`.
    pub fn request_at(&self, row: i32) -> Option<UserId> {
        self.presenter.request_rows.get(usize::try_from(row).ok()?).cloned()
    }

    pub fn friend_at(&self, row: i32) -> Option<UserId> {
        self.presenter.friend_rows.get(usize::try_from(row).ok()?).cloned()
    }

    /// What Active now's row `row` opens.
    pub fn thread_at(&self, row: i32) -> Option<RoomId> {
        self.presenter.thread_rows.get(usize::try_from(row).ok()?).cloned()
    }

    pub fn active_at(&self, row: i32) -> Option<crate::view::ActiveTarget> {
        self.presenter.active_rows.get(usize::try_from(row).ok()?).cloned()
    }

    /// People's search box and network filter changed.
    pub fn people_search(&mut self, ui: &AppWindow, query: String, network: Option<String>) {
        match network {
            Some(n) => self.presenter.people_network = n,
            None => self.presenter.people_query = query,
        }
        self.refresh(ui);
    }
}
