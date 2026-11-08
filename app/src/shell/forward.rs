// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The forward picker: "Forward" in a message's menu opens it; the core
//! lists the rooms that take forwards and gates the send.

use super::*;

impl Shell {
    pub fn open_forward(&mut self, ui: &AppWindow, row: i32) {
        let (Some(message), Some(room)) = (self.message_at(row), self.app.state().selected.clone()) else {
            return;
        };
        // Remembered with the room: switching chats while it's open can't redirect it.
        self.forward = Some((room, message));
        ui.set_forward_open(true);
        self.forward_search(ui, "");
    }

    pub fn forward_search(&mut self, ui: &AppWindow, query: &str) {
        let Some((from, _)) = self.forward.clone() else {
            return;
        };
        let (rows, rooms) = view::forward_results(self.app.state(), &from, query, &mut self.images);
        ui.set_forward_results(ModelRc::new(VecModel::from(rows)));
        self.forward_rows = rooms;
    }

    pub fn forward_send(&mut self, ui: &AppWindow, row: i32, note: &str) {
        let to = usize::try_from(row)
            .ok()
            .and_then(|r| self.forward_rows.get(r).cloned());
        if let (Some((room, message)), Some(to)) = (self.forward.take(), to) {
            self.act(
                ui,
                Action::ForwardMessage {
                    room,
                    message,
                    to,
                    comment: note.to_owned(),
                },
            );
        }
    }
}
