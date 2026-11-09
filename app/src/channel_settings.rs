// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A channel's settings page. Edits stay a local draft until Save; a refresh
//! or a failure keeps them (like `server_settings`).
use crate::{
    images::{Fit, Images},
    ui::{AdminRoleItem, AppWindow, OverridePermItem},
};
use godwit_core::application::{Action, AppState};
use godwit_core::channel_settings::{
    ChannelOverrides, ChannelOverview, ChannelOverviewEdit, OverrideTarget, SLOWMODE_CHOICES,
};
use godwit_core::ids::RoomId;
use godwit_core::own_profile::ProfileImage;
use slint::{Model, VecModel};
use std::rc::Rc;

#[derive(Default)]
pub struct ChannelEditor {
    token: Option<(RoomId, u64)>,
    original: Option<ChannelOverview>,
    icon: ProfileImage,
    saving: bool,
    dirty: bool,
    error: Option<String>,
    /// The Permissions page is open (else the overview).
    page: i32,
    /// The override selected there, by target so a refresh keeps it.
    selected: Option<OverrideTarget>,
    /// What the selected override's draft started from, and the draft
    /// (allow, deny); it restarts when the channel's own value changes.
    draft: Option<((u64, u64), (u64, u64))>,
    /// An override change is on its way (the request id moves on).
    acting: bool,
    /// The Permissions page's models, and what they were last filled from:
    /// the overrides, and the entry row and values the permission rows show.
    entries_model: Rc<VecModel<AdminRoleItem>>,
    perms_model: Rc<VecModel<OverridePermItem>>,
    choices_model: Rc<VecModel<slint::SharedString>>,
    shown: Option<ChannelOverrides>,
    rows: Option<(usize, (u64, u64))>,
}

impl ChannelEditor {
    pub fn load_webhooks(&self) -> Action {
        Action::LoadWebhooks
    }
    pub fn create_webhook(&self, name: &str) -> Option<Action> {
        let name = name.trim();
        (!name.is_empty() && name.chars().count() <= 32)
            .then(|| Action::ChangeWebhook(godwit_core::webhooks::WebhookChange::Create { name: name.into() }))
    }
    pub fn rename_webhook(&self, id: String, name: &str) -> Option<Action> {
        let name = name.trim();
        (!name.is_empty() && name.chars().count() <= 32).then(|| {
            Action::ChangeWebhook(godwit_core::webhooks::WebhookChange::Rename {
                id: id.into(),
                name: name.into(),
            })
        })
    }
    pub fn delete_webhook(&self, id: String) -> Action {
        Action::ChangeWebhook(godwit_core::webhooks::WebhookChange::Delete { id: id.into() })
    }
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

    pub fn show_page(&mut self, page: i32) {
        self.page = page.clamp(0, 2);
        self.draft = None;
    }
    pub fn select_override(&mut self, state: &AppState, row: i32) {
        if let Some(o) = overrides(state) {
            self.select_in(o, row);
        }
    }
    /// Switching entries drops the other one's unsaved draft, silently (the
    /// official client keeps one draft per page the same way).
    fn select_in(&mut self, o: &ChannelOverrides, row: i32) {
        let target = usize::try_from(row)
            .ok()
            .and_then(|r| o.entries.get(r))
            .map(|e| e.target.clone());
        if target.is_some() && target != self.selected {
            self.selected = target;
            self.draft = None;
        }
    }
    /// Sets one permission of the selected override's draft: 0 inherit, 1 allow, 2 deny.
    pub fn set_permission(&mut self, state: &AppState, row: i32, value: i32) {
        if let Some(o) = overrides(state) {
            self.set_permission_in(o, row, value);
        }
    }
    fn set_permission_in(&mut self, o: &ChannelOverrides, row: i32, value: i32) {
        if !o.can_edit {
            return;
        }
        let Some(bit) = usize::try_from(row)
            .ok()
            .and_then(|r| o.permissions.get(r))
            .map(|p| p.bit)
        else {
            return;
        };
        if bit & o.grantable == 0 {
            return;
        }
        let Some(entry) = self.selected_entry(o) else { return };
        let (base, (mut allow, mut deny)) = self.draft.unwrap_or((entry, entry));
        allow &= !bit;
        deny &= !bit;
        match value {
            1 => allow |= bit,
            2 => deny |= bit,
            _ => {}
        }
        self.draft = Some((base, (allow, deny)));
    }
    pub fn save_override(&mut self, state: &AppState) -> Option<Action> {
        let o = overrides(state).filter(|o| o.can_edit)?;
        let (_, (allow, deny)) = self.draft?;
        let target = self.selected.clone()?;
        o.entries.iter().any(|e| e.target == target).then_some(())?;
        self.acting = true;
        Some(Action::SetChannelOverride { target, allow, deny })
    }
    pub fn remove_override(&mut self, state: &AppState) -> Option<Action> {
        self.remove_in(overrides(state)?)
    }
    fn remove_in(&mut self, o: &ChannelOverrides) -> Option<Action> {
        let target = self.selected.clone()?;
        o.entries
            .iter()
            .any(|e| e.target == target && !e.everyone)
            .then_some(())?;
        // The selection stays: a refused removal leaves the entry in place,
        // and a done one drops it back to @everyone (see `reconcile`).
        self.acting = true;
        o.can_edit.then_some(Action::DeleteChannelOverride(target))
    }
    /// Adds an override for the role or member picked in the list: it starts
    /// as all inherit, and is selected once the network confirms it.
    pub fn add_override(&mut self, state: &AppState, choice: i32) -> Option<Action> {
        let o = overrides(state).filter(|o| o.can_edit)?;
        let target = o.choices.get(usize::try_from(choice).ok()?)?.target.clone();
        self.acting = true;
        self.selected = Some(target.clone());
        self.draft = None;
        Some(Action::SetChannelOverride {
            target,
            allow: 0,
            deny: 0,
        })
    }
    /// The selected entry's (allow, deny), or @everyone's when none is.
    fn selected_entry(&self, o: &ChannelOverrides) -> Option<(u64, u64)> {
        let e = self
            .selected
            .as_ref()
            .and_then(|t| o.entries.iter().find(|e| &e.target == t))
            .or_else(|| o.entries.first())?;
        Some((e.allow, e.deny))
    }

    /// Where the selection and draft stand against the channel's overrides:
    /// the entry's row and the (allow, deny) to show.
    fn reconcile(&mut self, o: &ChannelOverrides) -> Option<(usize, (u64, u64))> {
        let found = self
            .selected
            .as_ref()
            .and_then(|t| o.entries.iter().position(|e| &e.target == t));
        // A removed (or not yet listed) selection falls back to @everyone,
        // except one just added or being removed, which waits for the answer.
        let index = found.unwrap_or(0);
        let entry = o.entries.get(index)?;
        if found.is_none() && !self.acting {
            self.selected = Some(entry.target.clone());
            self.draft = None;
        }
        let shown = (entry.allow, entry.deny);
        // The draft restarts when the channel's value moved under it (not
        // when another entry's did).
        let value = match self.draft {
            Some((base, value)) if base == shown => value,
            _ => {
                self.draft = None;
                shown
            }
        };
        Some((index, value))
    }

    fn show_overrides(&mut self, ui: &AppWindow, o: &ChannelOverrides) {
        let Some((index, (allow, deny))) = self.reconcile(o) else {
            return;
        };
        let entry = &o.entries[index];
        ui.set_channel_edit_override_index(i32::try_from(index).unwrap_or(0));
        ui.set_channel_edit_override_dirty(self.draft.is_some_and(|(base, value)| base != value));
        ui.set_channel_edit_override_can_edit(o.can_edit);
        ui.set_channel_edit_override_removable(o.can_edit && !entry.everyone);
        // The models change only with the overrides (or the selection); a
        // toggle updates its own row.
        if self.shown.as_ref() != Some(o) {
            if self.shown.is_none() {
                ui.set_channel_edit_override_entries(self.entries_model.clone().into());
                ui.set_channel_edit_override_perms(self.perms_model.clone().into());
                ui.set_channel_edit_override_choices(self.choices_model.clone().into());
            }
            self.entries_model.set_vec(
                o.entries
                    .iter()
                    .map(|e| {
                        let colour = e.colour.as_deref().and_then(crate::server_settings::slint_colour);
                        AdminRoleItem {
                            name: e.name.as_ref().into(),
                            has_colour: colour.is_some(),
                            colour: colour.unwrap_or_default(),
                        }
                    })
                    .collect::<Vec<_>>(),
            );
            let last = i32::try_from(o.choices.len()).unwrap_or(0) - 1;
            let current = ui.get_channel_edit_override_choice_index();
            ui.set_channel_edit_override_choice_index(if last < 0 { -1 } else { current.clamp(0, last) });
            self.choices_model
                .set_vec(o.choices.iter().map(|c| c.name.as_ref().into()).collect::<Vec<_>>());
            self.shown = Some(o.clone());
            self.rows = None;
        }
        let item = |p: &godwit_core::server_settings::PermissionInfo| OverridePermItem {
            heading: p.heading.as_deref().unwrap_or_default().into(),
            title: p.title.as_ref().into(),
            description: p.description.as_ref().into(),
            state: if allow & p.bit != 0 {
                1
            } else if deny & p.bit != 0 {
                2
            } else {
                0
            },
            tri_state: o.supports_deny,
            locked: p.bit & o.grantable == 0,
        };
        match self.rows {
            Some((row, values)) if row == index => {
                if values != (allow, deny) {
                    for (i, p) in o.permissions.iter().enumerate() {
                        if (values.0 ^ allow | values.1 ^ deny) & p.bit != 0 {
                            self.perms_model.set_row_data(i, item(p));
                        }
                    }
                }
            }
            _ => self
                .perms_model
                .set_vec(o.permissions.iter().map(item).collect::<Vec<_>>()),
        }
        self.rows = Some((index, (allow, deny)));
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
            .is_none_or(|(id, request)| *id != s.room || (*request != s.request && !self.saving && !self.acting));
        if identity_changed || (s.overview.is_none() && self.original.is_some()) {
            *self = Self::default();
            // The webhook page's typed text is Slint state: clear it with the draft.
            ui.set_channel_edit_webhook_create_name("".into());
            ui.set_channel_edit_webhook_edit_name("".into());
            ui.set_channel_edit_webhook_edit_index(-1);
        }
        self.token = Some((s.room.clone(), s.request));
        if let Some(o) = &s.overview
            && (self.original.is_none() || (self.saving && s.saved && !s.busy))
        {
            self.copy(ui, o);
        }
        if !s.busy {
            self.saving = false;
            self.acting = false;
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
        let is_group = s.overview.as_ref().is_some_and(|o| o.is_group);
        ui.set_channel_edit_is_group(is_group);
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
        let overrides = overview.and_then(|o| o.overrides.as_ref());
        ui.set_channel_edit_has_overrides(overrides.is_some());
        if overrides.is_none() {
            self.page = 0;
        } else if is_group {
            self.page = 1;
        }
        if self.page == 2 && !state.room(&s.room).is_some_and(|r| r.capabilities.manage_webhooks) {
            self.page = 0;
        }
        ui.set_channel_edit_page(self.page);
        let hooks = state.webhooks.as_ref().filter(|w| w.room == s.room);
        ui.set_channel_edit_has_webhooks(state.room(&s.room).is_some_and(|r| r.capabilities.manage_webhooks));
        ui.set_channel_edit_webhooks_loading(hooks.is_none_or(|w| w.loading));
        ui.set_channel_edit_webhooks_busy(hooks.is_some_and(|w| w.busy));
        ui.set_channel_edit_webhook_status(
            hooks
                .and_then(|w| w.error.as_ref())
                .map_or_else(|| "".into(), |e| e.to_string().into()),
        );
        let items = hooks
            .map(|w| {
                w.entries
                    .iter()
                    .map(|e| {
                        let avatar = e.avatar_url.as_deref().and_then(|url| images.get(url, Fit::Circle(72)));
                        crate::ui::WebhookItem {
                            id: e.id.to_string().into(),
                            name: e.name.to_string().into(),
                            avatar: avatar.clone().unwrap_or_default(),
                            has_avatar: avatar.is_some(),
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        ui.set_channel_edit_webhooks(slint::ModelRc::new(slint::VecModel::from(items)));
        match overrides {
            // Rows only exist while the page is open.
            Some(o) if self.page == 1 => self.show_overrides(ui, o),
            _ => {
                if self.shown.take().is_some() || ui.get_channel_edit_override_perms().row_count() > 0 {
                    ui.set_channel_edit_override_entries(slint::ModelRc::default());
                    ui.set_channel_edit_override_perms(slint::ModelRc::default());
                    ui.set_channel_edit_override_choices(slint::ModelRc::default());
                    self.entries_model.set_vec(Vec::new());
                    self.perms_model.set_vec(Vec::new());
                    self.choices_model.set_vec(Vec::new());
                    self.rows = None;
                }
            }
        }
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

fn overrides(state: &AppState) -> Option<&ChannelOverrides> {
    state.channel_settings.as_ref()?.overview.as_ref()?.overrides.as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use godwit_core::channel_settings::ChannelOverride;
    use godwit_core::server_settings::PermissionInfo;

    fn target(id: &str) -> OverrideTarget {
        OverrideTarget {
            id: id.into(),
            member: false,
        }
    }

    fn overrides(everyone: (u64, u64), role: (u64, u64)) -> ChannelOverrides {
        let entry = |id: &str, e: bool, (allow, deny): (u64, u64)| ChannelOverride {
            target: target(id),
            name: id.into(),
            colour: None,
            everyone: e,
            allow,
            deny,
        };
        let perm = |bit| PermissionInfo {
            bit,
            heading: None,
            title: "p".into(),
            description: "".into(),
        };
        ChannelOverrides {
            entries: vec![entry("E", true, everyone), entry("R", false, role)],
            permissions: vec![perm(1), perm(2), perm(4)].into(),
            choices: Vec::new(),
            can_edit: true,
            supports_deny: true,
            grantable: u64::MAX,
        }
    }

    #[test]
    fn a_draft_survives_another_entrys_update_but_not_its_own() {
        let mut e = ChannelEditor::default();
        let o = overrides((0, 0), (1, 0));
        e.select_in(&o, 1);
        e.set_permission_in(&o, 1, 2);
        assert_eq!(e.reconcile(&o), Some((1, (1, 2))));
        // Someone else changes @everyone's entry: the draft stays.
        let other = overrides((4, 0), (1, 0));
        assert_eq!(e.reconcile(&other), Some((1, (1, 2))));
        // The selected entry itself changed elsewhere: back to its value.
        let mine = overrides((4, 0), (1, 4));
        assert_eq!(e.reconcile(&mine), Some((1, (1, 4))));
    }

    #[test]
    fn a_refused_removal_keeps_the_entry_selected_and_a_done_one_leaves_it() {
        let mut e = ChannelEditor::default();
        let o = overrides((0, 0), (1, 0));
        e.select_in(&o, 1);
        assert!(matches!(e.remove_in(&o), Some(Action::DeleteChannelOverride(_))));
        // The answer was an error: the entry is still there and still selected.
        e.acting = false;
        assert_eq!(e.reconcile(&o), Some((1, (1, 0))));
        // Removed for real: the page falls back to @everyone.
        e.remove_in(&o);
        e.acting = false;
        let mut gone = o.clone();
        gone.entries.pop();
        assert_eq!(e.reconcile(&gone), Some((0, (0, 0))));
        assert_eq!(e.selected, Some(target("E")));
        // @everyone's entry has no removal.
        assert!(e.remove_in(&gone).is_none());
    }

    #[test]
    fn locked_bits_stay_as_they_are() {
        let mut e = ChannelEditor::default();
        let mut o = overrides((0, 0), (0, 0));
        o.grantable = 1;
        e.reconcile(&o);
        e.set_permission_in(&o, 1, 1);
        assert!(e.draft.is_none());
        e.set_permission_in(&o, 0, 1);
        assert_eq!(e.reconcile(&o), Some((0, (1, 0))));
    }

    #[test]
    fn group_bitsets_preserve_unlisted_bits_and_reject_deny_values() {
        let mut o = overrides((1 << 60, 0), (0, 0));
        o.supports_deny = false;
        let everyone = target("E");
        assert!(o.may_set(&everyone, (1 << 60) | 1, 0));
        assert!(!o.may_set(&everyone, (1 << 60) | 1, 2));
    }
}
