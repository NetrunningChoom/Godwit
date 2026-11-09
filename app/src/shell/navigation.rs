// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Moving around: rooms, servers and accounts on the rail, Home, people, profiles, invites and joining servers.

use super::*;
use slint::Model as _;

impl Shell {
    pub fn select_group_account(&mut self, ui: &AppWindow, account_key: &str) {
        let supported = crate::settings::parse_account_key(account_key).is_some_and(|id| id.network.can_create_group());
        ui.set_friend_group_capable(supported);
    }

    pub fn toggle_group_person(&mut self, ui: &AppWindow, row: i32) {
        if let Some(count) = self.presenter.toggle_group_person(row) {
            ui.set_group_selected_count(i32::try_from(count).unwrap_or(i32::MAX));
        }
    }

    pub fn create_group(&mut self, ui: &AppWindow, account_key: &str, name: &str) {
        let Some(account) = crate::settings::parse_account_key(account_key) else {
            return;
        };
        let users = self.presenter.selected_group_users(&account);
        if name.trim().is_empty() || users.len() < 2 {
            return;
        }
        self.presenter.group_person_selected.clear();
        for i in 0..self.presenter.group_people.row_count() {
            if let Some(mut row) = self.presenter.group_people.row_data(i) {
                row.selected = false;
                self.presenter.group_people.set_row_data(i, row);
            }
        }
        ui.set_group_selected_count(0);
        ui.set_group_name(String::new().into());
        self.act(
            ui,
            Action::CreateGroup {
                account,
                name: name.to_owned(),
                users,
            },
        );
    }

    pub fn toggle_people_favourites(&mut self, ui: &AppWindow) {
        self.presenter.people_favourites_filter = !self.presenter.people_favourites_filter;
        self.refresh(ui);
    }

    /// Stars or unstars a person in Godwit, across all linked identities.
    pub fn toggle_person_favourite(&mut self, ui: &AppWindow, row: i32) {
        let Some((contact, _)) = self.person_at(row).cloned() else {
            return;
        };
        let favourite = self.app.state().contacts.get(contact).is_some_and(|c| !c.favourite);
        self.act(ui, Action::SetContactFavourite { contact, favourite });
    }

    pub fn contact_of(&self, user: &UserId) -> Option<ContactId> {
        self.app.state().contacts.of(user).map(|c| c.id)
    }

    pub fn convert_emoticons(&self) -> bool {
        self.settings.convert_emoticons
    }

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
    /// "Join" on an activity invite: its link goes to the system, which
    /// opens the network's own app (Steam for a game invite). Only a link a
    /// shown message carries, so the UI can't open anything else.
    pub fn join_activity(&mut self, url: &str) {
        let known = self
            .presenter
            .rows
            .iter()
            .any(|row| row.activity_links().any(|u| u == url));
        // Never local files or programs, whatever a network crate builds.
        if known && !url.is_empty() && !url.starts_with("file:") {
            open_external(url);
        }
    }

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
        self.create_server_accounts = self
            .app
            .state()
            .accounts
            .iter()
            .filter(|a| self.account(&a.id).is_some_and(|a| a.supports_create_server()))
            .map(|a| a.id.clone())
            .collect();
        let create_labels: Vec<SharedString> = self
            .create_server_accounts
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
        ui.set_join_server_create_accounts(ModelRc::new(VecModel::from(create_labels)));
        ui.set_join_server_account_index(0);
        ui.set_join_server_create_account_index(0);
        ui.set_join_server_creating(false);
        ui.set_join_server_choosing(true);
        self.reset_join_server(ui);
        if self.join_accounts.is_empty() {
            ui.set_join_server_status("Sign in to an account that supports server invites first.".into());
        }
        ui.set_join_server_open(true);
    }

    pub fn close_join_server(&mut self, ui: &AppWindow) {
        self.reset_join_server(ui);
        self.join_accounts.clear();
        self.create_server_accounts.clear();
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

    pub fn create_server(&mut self, ui: &AppWindow, name: &str) {
        let Some(account) = usize::try_from(ui.get_join_server_create_account_index())
            .ok()
            .and_then(|i| self.create_server_accounts.get(i))
            .cloned()
        else {
            return;
        };
        if !self.account(&account).is_some_and(|a| a.supports_create_server()) {
            return;
        }
        self.act(
            ui,
            Action::CreateServer {
                account,
                name: name.to_owned(),
            },
        );
        self.close_join_server(ui);
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
            view::RailRow::Account(_)
            | view::RailRow::Folder(..)
            | view::RailRow::Pending(_)
            | view::RailRow::SignIn(_) => None,
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
    /// 4 muzzle mode on/off for this chat, 5 its notifications (cycles),
    /// 6 its channel settings, 7 "Games in common…", 10/11 move its pin up/down,
    /// 20..=33 its mute and level; 34/35 join or leave a thread.
    pub fn room_menu(&mut self, ui: &AppWindow, row: i32, choice: i32) {
        // A category heading's menu: 6 its settings, 8 move up, 9 move down.
        if let Some(category) = usize::try_from(row)
            .ok()
            .and_then(|r| self.presenter.category_ids.get(r).cloned().flatten())
            && let Some(server) = self.app.state().selected_server.clone()
        {
            match choice {
                6 => self.act(ui, Action::OpenCategorySettings { server, category }),
                8 | 9 => self.act(
                    ui,
                    Action::ServerAdmin {
                        server,
                        op: godwit_core::server_settings::ServerAdmin::MoveChannel {
                            id: category,
                            category: true,
                            up: choice == 8,
                        },
                    },
                ),
                _ => {}
            }
            return;
        }
        let Some(room) = usize::try_from(row).ok().and_then(|r| self.room_at(r)) else {
            return;
        };
        if choice == 34 {
            self.act(ui, Action::JoinThread(room));
            return;
        }
        if choice == 35 {
            self.act(ui, Action::LeaveThread(room));
            return;
        }
        if choice == 7 {
            self.open_games_for(ui, &room);
            return;
        }
        if choice == 12 {
            self.leave_group_pending = Some(room.clone());
            ui.set_leave_group_name(
                self.app
                    .state()
                    .room(&room)
                    .map_or_else(|| "this group".into(), |r| r.name.to_string())
                    .into(),
            );
            ui.set_confirm_leave_group(true);
            return;
        }
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
        if let Some(change) = notify_change(choice) {
            self.act(ui, Action::SetRoomNotify { room, change });
            return;
        }
        if choice == 6 {
            self.act(ui, Action::OpenChannelSettings(room));
            return;
        }
        // Move up / down in its server's list.
        if choice == 8 || choice == 9 {
            if let Some(godwit_core::room::RoomKind::Channel { server }) =
                self.app.state().room(&room).map(|r| r.kind.clone())
            {
                self.act(
                    ui,
                    Action::ServerAdmin {
                        server,
                        op: godwit_core::server_settings::ServerAdmin::MoveChannel {
                            id: room.raw.clone(),
                            category: false,
                            up: choice == 8,
                        },
                    },
                );
            }
            return;
        }
        let state = self.app.state();
        // A person merged across networks is one chat: core pins, hides
        // and moves all of their rooms together.
        let action = match choice {
            0 => Action::PinRoom {
                pinned: !state.is_pinned(&room),
                room,
            },
            1 => Action::MarkRoomRead(room),
            2 => Action::MarkRoomUnread(room),
            10 | 11 => Action::MovePin {
                room,
                by: if choice == 10 { -1 } else { 1 },
            },
            3 => Action::HideRoom {
                hidden: !state.is_hidden(&room),
                room,
            },
            _ => return,
        };
        self.act(ui, action);
        self.save_room_prefs();
    }

    /// Confirmed in the shared destructive-action dialog before the backend is asked.
    pub fn leave_group(&mut self, ui: &AppWindow) {
        if let Some(room) = self
            .leave_group_pending
            .take()
            .filter(|r| self.app.state().room(r).is_some_and(|room| room.capabilities.leave))
        {
            self.act(ui, Action::LeaveGroup(room));
        }
    }

    /// A pinned chat dragged `by` rows among Home's pins (negative: up).
    pub fn move_pin(&mut self, ui: &AppWindow, row: i32, by: i32) {
        let Some(room) = usize::try_from(row).ok().and_then(|r| self.room_at(r)) else {
            return;
        };
        self.act(ui, Action::MovePin { room, by });
        self.save_room_prefs();
    }

    /// A category heading's "Create channel": the dialog opens, aimed at it.
    pub fn create_channel_in(&mut self, ui: &AppWindow, row: i32) {
        let Some(category) = usize::try_from(row)
            .ok()
            .and_then(|r| self.presenter.category_ids.get(r).cloned().flatten())
        else {
            return;
        };
        let title = self
            .app
            .state()
            .selected_server
            .as_ref()
            .and_then(|s| self.app.state().server(s))
            .and_then(|s| s.categories.iter().find(|c| c.id.as_ref() == Some(&category)))
            .map(|c| c.title.to_string())
            .unwrap_or_default();
        ui.set_new_channel_category(category.as_ref().into());
        ui.set_new_channel_category_name(title.into());
        ui.set_creating_channel(true);
    }

    /// A server moved on the rail: up (-1), down (1) or to the top (0) of
    /// its account's servers. The whole account's order is kept from then on.
    pub fn move_server(&mut self, ui: &AppWindow, row: i32, delta: i32) {
        self.reorder_server(ui, row, i64::from(delta), delta == 0);
    }

    /// A server dragged on the rail by `steps` rows (up negative), within
    /// its account's servers.
    pub fn move_server_by(&mut self, ui: &AppWindow, row: i32, steps: i32) {
        self.reorder_server(ui, row, i64::from(steps), false);
    }

    fn reorder_server(&mut self, ui: &AppWindow, row: i32, steps: i64, top: bool) {
        let rows = &self.presenter.server_rows;
        let Some(r) = usize::try_from(row).ok() else { return };
        let Some(view::RailRow::Server(id)) = rows.get(r) else {
            return;
        };
        let all = view::ordered_servers(self.app.state(), &id.account, &self.presenter.server_order);
        let prefix = format!("{}|", settings::account_key(&id.account));
        let Some(keys) = view::reordered(&all, rows, r, steps, top) else {
            return;
        };
        self.settings.server_order.retain(|k| !k.starts_with(&prefix));
        self.settings.server_order.extend(keys);
        self.presenter.server_order = self.settings.server_order.clone();
        self.save_settings();
        self.refresh(ui);
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
            view::RailRow::Server(_)
            | view::RailRow::Folder(..)
            | view::RailRow::Pending(_)
            | view::RailRow::SignIn(_) => None,
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
        let seeded: Vec<String> = state.pins_seeded.iter().map(settings::account_key).collect();
        if pinned != self.settings.pinned_rooms
            || hidden != self.settings.hidden_rooms
            || seeded != self.settings.pins_seeded
        {
            self.settings.pinned_rooms = pinned;
            self.settings.hidden_rooms = hidden;
            self.settings.pins_seeded = seeded;
            self.save_settings();
        }
    }

    pub fn room_at(&self, row: usize) -> Option<RoomId> {
        self.presenter.room_rows.get(row).cloned().flatten().map(|room| {
            if room.raw.as_ref() == crate::view::NOTES_PLACEHOLDER {
                room
            } else {
                self.app.state().preferred_chat_room(&room)
            }
        })
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

    /// The card's Block user (after its confirmation) and Unblock user; the
    /// card stays open and shows the new relationship.
    pub fn block_profile(&mut self, ui: &AppWindow, block: bool) {
        if let Some((user, _)) = self.app.state().profile.clone() {
            self.act(
                ui,
                if block {
                    Action::BlockUser(user)
                } else {
                    Action::UnblockUser(user)
                },
            );
        }
    }

    /// The profile card's Ignore / Unignore action.
    pub fn ignore_profile(&mut self, ui: &AppWindow, ignore: bool) {
        if let Some((user, _)) = self.app.state().profile.clone() {
            self.act(
                ui,
                if ignore {
                    Action::IgnoreUser(user)
                } else {
                    Action::UnignoreUser(user)
                },
            );
        }
    }

    /// The card's note field committed (Enter or leaving it): one request
    /// per edit, none when it's what the network already has.
    pub fn save_note(&mut self, ui: &AppWindow, note: String) {
        if let Some((user, Some(p))) = self.app.state().profile.clone()
            && p.can_note
            && p.note.as_deref().unwrap_or_default() != note
        {
            self.act(ui, Action::SetNote { user, note });
        }
    }

    /// The note field changed: it stops at the network's limit (the text
    /// is cut rather than silently trimmed when saved).
    pub fn note_edited(&mut self, ui: &AppWindow, text: &str) {
        if let Some((_, Some(p))) = &self.app.state().profile
            && p.note_limit > 0
            && text.chars().count() > p.note_limit
        {
            ui.set_card_note(text.chars().take(p.note_limit).collect::<String>().into());
        }
    }

    /// A row of the card's mutual servers (open it) or friends (their card).
    pub fn open_mutual(&mut self, ui: &AppWindow, row: i32, server: bool) {
        let Some((_, Some(p))) = self.app.state().profile.clone() else {
            return;
        };
        let Ok(row) = usize::try_from(row) else { return };
        if server {
            if let Some(m) = p.mutual_servers.get(row) {
                self.act(ui, Action::CloseProfile);
                self.act(ui, Action::SelectServer(Some(m.server.clone())));
            }
        } else if let Some(f) = p.mutual_friends.get(row) {
            self.act(ui, Action::OpenProfile(f.user.clone()));
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

/// What a room menu's notification choices ask for (see `room_menu`), as
/// Discord's channel menu offers: mute for 15 minutes, 1, 3, 8, 24 hours or
/// until turned back on; unmute; or the channel's own level.
fn notify_change(choice: i32) -> Option<godwit_core::room::RoomNotifyChange> {
    use godwit_core::room::RoomNotifyChange as C;
    const MINUTE: u32 = 60;
    Some(match choice {
        20 => C::Mute {
            seconds: Some(15 * MINUTE),
        },
        21 => C::Mute {
            seconds: Some(60 * MINUTE),
        },
        22 => C::Mute {
            seconds: Some(3 * 60 * MINUTE),
        },
        23 => C::Mute {
            seconds: Some(8 * 60 * MINUTE),
        },
        24 => C::Mute {
            seconds: Some(24 * 60 * MINUTE),
        },
        26 => C::Mute { seconds: None },
        27 => C::Unmute,
        30 => C::Level(None),
        31..=33 => C::Level(Some(u8::try_from(choice - 31).ok()?)),
        _ => return None,
    })
}

#[cfg(test)]
mod notify_choice_tests {
    use super::notify_change;
    use godwit_core::room::RoomNotifyChange as C;

    #[test]
    fn menu_choices_map_to_changes() {
        assert_eq!(notify_change(21), Some(C::Mute { seconds: Some(3600) }));
        assert_eq!(notify_change(26), Some(C::Mute { seconds: None }));
        assert_eq!(notify_change(27), Some(C::Unmute));
        assert_eq!(notify_change(30), Some(C::Level(None)));
        assert_eq!(notify_change(32), Some(C::Level(Some(1))));
        assert_eq!(notify_change(6), None);
    }
}
