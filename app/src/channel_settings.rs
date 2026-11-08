// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A channel's settings page. Edits stay a local draft until Save; a refresh
//! or a failure keeps them (like `server_settings`).
use crate::{
    images::{Fit, Images},
    ui::AppWindow,
};
use godwit_core::application::{Action, AppState};
use godwit_core::channel_settings::{ChannelOverview, ChannelOverviewEdit, SLOWMODE_CHOICES};
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
        ui.set_channel_edit_slowmode_index(slowmode_index(overview.slowmode));
        // Set with each load (not each refresh), so an open menu isn't rebuilt under the pointer.
        ui.set_channel_edit_slowmode_choices(slint::ModelRc::new(slint::VecModel::from(
            SLOWMODE_CHOICES
                .iter()
                .map(|(label, _)| slint::SharedString::from(*label))
                .collect::<Vec<_>>(),
        )));
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
        // A value off the list (another client set it) stays unless a choice is made.
        let slowmode = usize::try_from(ui.get_channel_edit_slowmode_index())
            .ok()
            .and_then(|i| SLOWMODE_CHOICES.get(i))
            .map(|(_, seconds)| *seconds);
        let edit = ChannelOverviewEdit {
            name: (name.trim() != o.name.trim()).then(|| name.trim().into()),
            description: (description.trim() != o.description.trim()).then(|| description.trim().into()),
            icon: self.icon.clone(),
            mature: (mature != o.mature).then_some(mature),
            slowmode: slowmode.filter(|s| o.slowmode.is_some_and(|old| old != *s)),
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
        let overview = s.overview.as_ref();
        ui.set_channel_edit_has_icons(overview.is_some_and(|o| o.icons));
        ui.set_channel_edit_is_category(overview.is_some_and(|o| o.is_category));
        ui.set_channel_edit_has_slowmode(overview.is_some_and(|o| o.slowmode.is_some()));
        ui.set_channel_edit_description_hint(
            overview
                .filter(|o| o.description_max > 0)
                .map(|o| format!("Up to {} characters.", o.description_max))
                .unwrap_or_default()
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

/// The slowmode choice for `seconds` (-1 when it isn't one of the listed ones).
fn slowmode_index(seconds: Option<u32>) -> i32 {
    seconds
        .and_then(|s| SLOWMODE_CHOICES.iter().position(|(_, v)| *v == s))
        .and_then(|i| i32::try_from(i).ok())
        .unwrap_or(-1)
}
