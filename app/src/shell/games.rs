// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! "Games in common": opening it from a chat, and showing the core's
//! comparison (`crate::games`).

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

    pub fn close_games(&mut self, ui: &AppWindow) {
        self.act(ui, Action::CloseGamesInCommon);
    }

    pub(super) fn show_games(&mut self, ui: &AppWindow) {
        let Some(g) = self.app.state().games.as_ref() else {
            if ui.get_games_open() {
                ui.set_games_open(false);
                self.games.clear();
            }
            return;
        };
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
        if !ui.get_games_open() {
            ui.set_games_friends(self.games.friend_model());
            ui.set_games_rows(self.games.rows());
            ui.set_games_open(true);
        }
    }
}
