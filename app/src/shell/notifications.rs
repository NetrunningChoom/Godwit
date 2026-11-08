// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The Notifications page: its badge, its rows, opening one, and Clear.

use super::*;
use slint::Model as _;

impl Shell {
    /// The sidebar's badge, and the page's rows while it's open (only then:
    /// its pictures load and decode only for a page on screen).
    pub(super) fn show_notifications(&mut self, ui: &AppWindow) {
        let state = self.app.state();
        if !state.showing_notifications {
            ui.set_notifications_unseen(i32::try_from(self.notifications.unseen()).unwrap_or(i32::MAX));
            // Hidden: drop the rows and the pictures they held.
            if self.notification_items.row_count() > 0 {
                view::sync(&self.notification_items, Vec::new());
            }
            return;
        }
        // Looking at the page sees them all, including any that come while it's open.
        self.notifications.mark_seen();
        ui.set_notifications_unseen(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        let images = &mut self.images;
        let rows = crate::notifications::rows(&self.notifications, state, now, |url| view::avatar(images, url));
        view::sync(&self.notification_items, rows);
    }

    /// A row clicked: its chat opens, at the message when it's known.
    pub fn notification_open(&mut self, ui: &AppWindow, row: i32) {
        let Some(n) = usize::try_from(row).ok().and_then(|i| self.notifications.get(i)) else {
            return;
        };
        let (room, message) = (n.room.clone(), n.message.clone());
        if self.app.state().room(&room).is_none() {
            return;
        }
        self.act(ui, Action::SelectRoom(room));
        if let Some(message) = message {
            self.act(ui, Action::JumpTo(message));
        }
    }

    /// "Clear": the list empties.
    pub fn notifications_clear(&mut self, ui: &AppWindow) {
        self.notifications.clear();
        self.refresh(ui);
    }
}
