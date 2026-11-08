// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The Games in common page: opening it from Home or a chat, and showing
//! the core's comparison (`crate::games`).

use super::*;

impl Shell {
    /// "Games in common…" in a chat's menu: us and the person (a direct
    /// chat) or its members as far as they're loaded (a group).
    pub(super) fn open_games_for(&mut self, ui: &AppWindow, room: &RoomId) {
        let state = self.app.state();
        let Some(r) = state.room(room) else { return };
        if !r.network().has_game_libraries() {
            return;
        }
        let with: Vec<UserId> = match &r.kind {
            godwit_core::room::RoomKind::Direct { with } => vec![with.clone()],
            godwit_core::room::RoomKind::Group => state
                .members
                .get(room)
                .map(|groups| {
                    groups
                        .iter()
                        .flat_map(|g| g.members.iter())
                        .map(|m| m.user.clone())
                        .filter(|u| u.raw != room.account.user)
                        .collect()
                })
                .unwrap_or_default(),
            _ => return,
        };
        self.games.clear();
        self.act(
            ui,
            Action::OpenGamesInCommon {
                account: room.account.clone(),
                with,
            },
        );
    }

    /// A toggle in the picker.
    pub fn games_pick_friend(&mut self, ui: &AppWindow, index: i32, picked: bool) {
        let Some(user) = usize::try_from(index)
            .ok()
            .and_then(|i| self.games.friends.get(i))
            .cloned()
        else {
            return;
        };
        // The diagram changes shape: back to everyone's games, at the top.
        self.games.region = 0;
        self.games.first_row = 0;
        self.act(ui, Action::PickForGames { user, picked });
    }

    pub fn games_pick_region(&mut self, ui: &AppWindow, region: i32) {
        self.games.region = u32::try_from(region).unwrap_or(0);
        self.games.first_row = 0;
        self.show_games(ui);
    }

    /// The list scrolled: pictures follow the view.
    pub fn games_scrolled(&mut self, ui: &AppWindow, first: i32) {
        let first = usize::try_from(first).unwrap_or(0);
        if first != self.games.first_row {
            self.games.first_row = first;
            self.show_games(ui);
        }
    }

    /// The accounts whose network has game libraries, in sign-in order.
    fn game_accounts(&self) -> Vec<AccountId> {
        self.app
            .state()
            .accounts
            .iter()
            .filter(|a| a.id.network.has_game_libraries())
            .map(|a| a.id.clone())
            .collect()
    }

    /// Home's "Games in common": the first account with game libraries.
    pub fn open_games(&mut self, ui: &AppWindow) {
        let Some(account) = self.game_accounts().into_iter().next() else {
            return;
        };
        self.games.clear();
        self.act(
            ui,
            Action::OpenGamesInCommon {
                account,
                with: Vec::new(),
            },
        );
    }

    /// The page's account selector (shown with more than one such account).
    pub fn games_pick_account(&mut self, ui: &AppWindow, index: i32) {
        let Some(account) = usize::try_from(index)
            .ok()
            .and_then(|i| self.game_accounts().into_iter().nth(i))
        else {
            return;
        };
        if self.app.state().games.as_ref().is_some_and(|g| g.account == account) {
            return;
        }
        self.games.clear();
        ui.set_games_query(SharedString::new());
        self.act(
            ui,
            Action::OpenGamesInCommon {
                account,
                with: Vec::new(),
            },
        );
    }

    /// The copy button: every list as Markdown on the clipboard.
    pub fn games_copy_list(&mut self) {
        let state = self.app.state();
        if let Some(text) = state.games.as_ref().and_then(|g| crate::games::markdown(state, g)) {
            copy_to_clipboard(text);
        }
    }

    /// The picker's search, as typed.
    pub fn games_search(&mut self, ui: &AppWindow, query: &str) {
        self.games.query = query.to_owned();
        self.show_games(ui);
    }

    pub(super) fn show_games(&mut self, ui: &AppWindow) {
        let state = self.app.state();
        let Some(g) = state.games.as_ref().filter(|_| state.showing_games) else {
            if ui.get_games_open() {
                ui.set_games_open(false);
                ui.set_games_query(SharedString::new());
                ui.set_games_accounts(ModelRc::default());
                self.games.clear();
            }
            return;
        };
        let accounts: Vec<AccountId> = self.game_accounts();
        let chosen = accounts.iter().position(|a| *a == g.account);
        let c = self.games.show(self.app.state(), g, &mut self.images);
        let notes: Vec<SharedString> = c.notes.iter().map(|n| n.as_str().into()).collect();
        let regions: Vec<i32> = c
            .regions
            .iter()
            .map(|n| i32::try_from(*n).unwrap_or(i32::MAX))
            .collect();
        let (venn, legend, region) = (
            i32::try_from(c.venn).unwrap_or(0),
            SharedString::from(c.legend.as_str()),
            i32::try_from(c.region).unwrap_or(0),
        );
        ui.set_games_notes(ModelRc::new(VecModel::from(notes)));
        ui.set_games_regions(ModelRc::new(VecModel::from(regions)));
        ui.set_games_venn(venn);
        ui.set_games_legend(legend);
        ui.set_games_region(region);
        ui.set_games_account(chosen.and_then(|i| i32::try_from(i).ok()).unwrap_or(0));
        if !ui.get_games_open() {
            let labels: Vec<SharedString> = accounts
                .iter()
                .filter_map(|id| self.app.state().accounts.iter().find(|a| a.id == *id))
                .map(|a| a.display_name.as_ref().into())
                .collect();
            ui.set_games_accounts(ModelRc::new(VecModel::from(labels)));
            ui.set_games_friends(self.games.friend_model());
            ui.set_games_rows(self.games.rows());
            ui.set_games_open(true);
        }
    }
}
