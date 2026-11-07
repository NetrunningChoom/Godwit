// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A channel's settings page. Edits stay a local draft until Save; a refresh
//! or a failure keeps them (like `server_settings`).
use crate::{
    images::{Fit, Images},
    ui::AppWindow,
};
use godwit_core::application::{Action, AppState};
use godwit_core::channel_settings::{ChannelOverview, ChannelOverviewEdit};
use godwit_core::ids::RoomId;
use godwit_core::own_profile::ProfileImage;

#[derive(Default)]
pub struct ChannelEditor {
    token: Option<(RoomId, u64)>,
    original: Option<ChannelOverview>,
    icon: ProfileImage,
    saving: bool,
    dirty: bool,
    error: Option<String>,
}

impl ChannelEditor {
    pub fn token(&self) -> Option<(RoomId, u64)> {
        self.token.clone()
    }
    pub fn edited(&mut self) {
        self.dirty = true;
        self.error = None;
    }
    fn copy(&mut self, ui: &AppWindow, overview: &ChannelOverview) {
        ui.set_channel_edit_name(overview.name.as_ref().into());
        ui.set_channel_edit_description(overview.description.as_ref().into());
        ui.set_channel_edit_mature(overview.mature);
        self.original = Some(overview.clone());
        self.icon = ProfileImage::Keep;
        self.dirty = false;
        self.saving = false;
        self.error = None;
    }
    pub fn reset(&mut self, ui: &AppWindow, state: &AppState) {
        if let Some(overview) = state.channel_settings.as_ref().and_then(|s| s.overview.as_ref()) {
            self.copy(ui, overview);
        }
    }
    pub fn remove_icon(&mut self, ui: &AppWindow) {
        if ui.get_channel_edit_busy() || !ui.get_channel_edit_allowed() {
            return;
        }
        self.icon = ProfileImage::Remove;
        self.edited();
    }
    pub fn picked(&mut self, ui: &AppWindow, token: &(RoomId, u64), result: Result<ProfileImage, String>) {
        if self.token.as_ref() != Some(token) || ui.get_channel_edit_busy() || !ui.get_channel_edit_allowed() {
            return;
        }
        match result {
            Ok(image) => {
                self.icon = image;
                self.edited();
            }
            Err(error) => self.error = Some(error),
        }
    }
    pub fn save(&mut self, ui: &AppWindow) -> Option<Action> {
        if ui.get_channel_edit_busy() || !ui.get_channel_edit_allowed() {
            return None;
        }
        let o = self.original.as_ref()?;
        let name = ui.get_channel_edit_name();
        let description = ui.get_channel_edit_description();
        let mature = ui.get_channel_edit_mature();
        let edit = ChannelOverviewEdit {
            name: (name.trim() != o.name.trim()).then(|| name.trim().into()),
            description: (description.trim() != o.description.trim()).then(|| description.trim().into()),
            icon: self.icon.clone(),
            mature: (mature != o.mature).then_some(mature),
        };
        self.saving = true;
        self.error = None;
        Some(Action::SaveChannelOverview(edit))
    }

    pub fn show(&mut self, ui: &AppWindow, state: &AppState, images: &mut Images) {
        let Some(s) = &state.channel_settings else {
            ui.set_channel_settings_open(false);
            *self = Self::default();
            return;
        };
        // A different channel, or one reloaded, starts from a clean draft.
        let identity_changed = self
            .token
            .as_ref()
            .is_none_or(|(id, request)| *id != s.room || (*request != s.request && !self.saving));
        if identity_changed || (s.overview.is_none() && self.original.is_some()) {
            *self = Self::default();
        }
        self.token = Some((s.room.clone(), s.request));
        if let Some(o) = &s.overview
            && (self.original.is_none() || (self.saving && s.saved && !s.busy))
        {
            self.copy(ui, o);
        }
        if !s.busy {
            self.saving = false;
        }
        ui.set_channel_settings_open(true);
        ui.set_channel_edit_loaded(s.overview.is_some());
        ui.set_channel_edit_busy(s.busy);
        ui.set_channel_edit_allowed(s.overview.as_ref().is_some_and(|o| o.can_manage));
        ui.set_channel_edit_dirty(self.dirty);
        ui.set_channel_edit_title(
            self.original
                .as_ref()
                .or(s.overview.as_ref())
                .map_or("", |o| o.name.as_ref())
                .into(),
        );
        let error = self.error.as_deref().or(s.error.as_deref());
        ui.set_channel_edit_failed(error.is_some());
        ui.set_channel_edit_status(
            error
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    if s.busy {
                        if s.overview.is_some() {
                            "Saving channel…"
                        } else {
                            "Loading channel…"
                        }
                    } else if s.saved && !self.dirty {
                        "Channel saved."
                    } else {
                        ""
                    }
                    .into()
                })
                .into(),
        );
        ui.set_channel_edit_icon_label(
            match self.icon {
                ProfileImage::Keep => "",
                ProfileImage::Remove => "Will be removed when you save.",
                ProfileImage::File { .. } => "New: saved when you save.",
            }
            .into(),
        );
        // A picked (cropped) picture shows at once, before it's uploaded.
        let icon = match &self.icon {
            ProfileImage::File { path, .. } => slint::Image::load_from_path(path).ok(),
            _ => None,
        }
        .or_else(|| {
            s.overview
                .as_ref()
                .and_then(|o| o.icon_url.as_deref())
                .filter(|_| self.icon != ProfileImage::Remove)
                .and_then(|u| images.get(u, Fit::Circle(160)))
        });
        ui.set_channel_edit_has_icon(icon.is_some());
        if let Some(image) = icon {
            ui.set_channel_edit_icon(image);
        }
    }
}
