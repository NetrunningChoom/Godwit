// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Search, pins and threads.

use super::*;

impl Shell {
    /// A message as the pins and search lists show it.
    pub(super) fn list_item(m: &godwit_core::message::Message) -> crate::ui::PinItem {
        crate::ui::PinItem {
            author: m.author_name.to_string().into(),
            text: m.snippet.as_deref().unwrap_or(&m.source).to_string().into(),
            time: chrono::DateTime::from_timestamp_millis(m.sent_ms)
                .map(|t| t.with_timezone(&chrono::Local).format("%-d %b %Y").to_string())
                .unwrap_or_default()
                .into(),
        }
    }

    /// The message search dialog, from the core's search.
    pub(super) fn show_search(&self, ui: &AppWindow) {
        let Some(s) = self.app.state().search.as_ref() else {
            ui.set_search_open(false);
            return;
        };
        let items: Vec<crate::ui::PinItem> = s.messages.iter().flatten().map(|m| Self::list_item(m)).collect();
        ui.set_search_results(ModelRc::new(VecModel::from(items)));
        ui.set_search_busy(s.messages.is_none());
        ui.set_search_failed(s.failed);
        ui.set_search_asked(!s.query.is_empty());
        ui.set_search_open(true);
    }

    /// "Jump" on a search result: the dialog closes, the message shows.
    pub fn search_jump(&mut self, ui: &AppWindow, index: i32) {
        let id = self.app.state().search.as_ref().and_then(|s| {
            s.messages
                .as_ref()?
                .get(usize::try_from(index).ok()?)
                .map(|m| m.id.clone())
        });
        if let Some(id) = id {
            self.act(ui, Action::CloseSearch);
            self.act(ui, Action::JumpTo(id));
        }
    }

    /// The pinned messages panel, from the core's list.
    pub(super) fn show_pins(&self, ui: &AppWindow) {
        let Some(p) = self.app.state().pins.as_ref() else {
            ui.set_pins_open(false);
            return;
        };
        let items: Vec<crate::ui::PinItem> = p.messages.iter().flatten().map(|m| Self::list_item(m)).collect();
        ui.set_pins(ModelRc::new(VecModel::from(items)));
        ui.set_pins_loading(p.messages.is_none() && !p.failed);
        ui.set_pins_failed(p.failed);
        ui.set_pins_open(true);
    }

    pub(super) fn pinned_at(&self, index: i32) -> Option<MessageId> {
        let p = self.app.state().pins.as_ref()?;
        p.messages
            .as_ref()?
            .get(usize::try_from(index).ok()?)
            .map(|m| m.id.clone())
    }

    /// "Jump" in the pins panel: the panel closes, the message shows.
    pub fn pin_jump(&mut self, ui: &AppWindow, index: i32) {
        let Some(id) = self.pinned_at(index) else { return };
        self.act(ui, Action::ClosePins);
        self.act(ui, Action::JumpTo(id));
    }

    pub fn pin_unpin(&mut self, ui: &AppWindow, index: i32) {
        if let Some(message) = self.pinned_at(index) {
            self.act(ui, Action::SetPinned { message, pinned: false });
        }
    }

    /// "Create thread" on a message: the dialog, named after the message.
    pub fn create_thread_from(&mut self, ui: &AppWindow, row: i32) {
        let Some(id) = self.message_at(row) else { return };
        let name: String = self
            .app
            .state()
            .selected_message(&id)
            .map(|m| m.snippet.as_deref().unwrap_or(&m.source).chars().take(40).collect())
            .unwrap_or_default();
        self.thread_from = Some(id);
        ui.set_thread_post(false);
        ui.set_thread_name(name.trim().to_owned().into());
        ui.set_thread_text("".into());
        ui.set_thread_open(true);
    }

    /// "New post" in a forum: the dialog, empty.
    pub fn new_post(&mut self, ui: &AppWindow) {
        self.thread_from = None;
        ui.set_thread_post(true);
        ui.set_thread_name("".into());
        ui.set_thread_text("".into());
        ui.set_thread_open(true);
    }

    pub fn thread_confirm(&mut self, ui: &AppWindow) {
        let (name, text) = (ui.get_thread_name().to_string(), ui.get_thread_text().to_string());
        ui.set_thread_open(false);
        let message = if ui.get_thread_post() {
            None
        } else {
            self.thread_from.take()
        };
        self.act(ui, Action::CreateThread { name, message, text });
    }

    /// "Pin message" in a message's menu.
    pub fn pin_row(&mut self, ui: &AppWindow, row: i32, pinned: bool) {
        if let Some(message) = self.message_at(row) {
            self.act(ui, Action::SetPinned { message, pinned });
        }
    }
}
