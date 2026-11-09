// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Server drafts stay local until Save; refreshes and failures preserve edits.
use crate::{
    images::{AVATAR, Fit, Images},
    ui::{AdminRoleItem, AppWindow, BanItem, EmojiItem, PermissionItem, ServerInviteItem},
};
use godwit_core::application::{Action, AppState};
use godwit_core::ids::ServerId;
use godwit_core::own_profile::ProfileImage;
use godwit_core::server_settings::{
    EmojiFile, RoleEdit, ServerAdmin, ServerEmojis, ServerOverview, ServerOverviewEdit, ServerPage, ServerRoles,
    ServerSettingsState,
};
use slint::{ModelRc, VecModel};

#[derive(Default)]
pub struct ServerEditor {
    token: Option<(ServerId, u64)>,
    original: Option<ServerOverview>,
    icon: ProfileImage,
    banner: ProfileImage,
    saving: bool,
    dirty: bool,
    error: Option<String>,
    /// The Roles page: the row being edited (0 is Everyone), its permission
    /// draft, and the list it was taken from (a new list refreshes a clean draft).
    role_index: usize,
    role_allow: u64,
    role_deny: u64,
    role_dirty: bool,
    roles_seen: Option<ServerRoles>,
    /// The Emoji page: the picture chosen for the next emoji, a problem with
    /// picking it, and whether a create is waiting for its answer.
    emoji_file: Option<EmojiFile>,
    emoji_error: Option<String>,
    emoji_creating: bool,
}

/// The core's page as the view's index, and back.
fn page_index(page: ServerPage) -> i32 {
    match page {
        ServerPage::Overview => 0,
        ServerPage::Roles => 1,
        ServerPage::Emoji => 2,
        ServerPage::Invites => 3,
        ServerPage::Bans => 4,
    }
}

pub fn page_from_index(i: i32) -> ServerPage {
    match i {
        1 => ServerPage::Roles,
        2 => ServerPage::Emoji,
        3 => ServerPage::Invites,
        4 => ServerPage::Bans,
        _ => ServerPage::Overview,
    }
}

/// A role colour as typed: empty for none, else `#rgb` or `#rrggbb`. The
/// API takes any CSS colour, but the editor offers plain hex, which every
/// client can draw.
fn parse_colour(text: &str) -> Result<Option<&str>, &'static str> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let hex = t.strip_prefix('#').unwrap_or_default();
    if matches!(hex.len(), 3 | 6) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(Some(t))
    } else {
        Err("Colours are written as #rrggbb.")
    }
}

/// `#rrggbb` (or `#rgb`) as a Slint colour.
pub(crate) fn slint_colour(text: &str) -> Option<slint::Color> {
    let hex = text.trim().strip_prefix('#').filter(|h| h.is_ascii())?;
    let expand = |s: &str| u8::from_str_radix(s, 16).ok();
    let (r, g, b) = match hex.len() {
        6 => (expand(&hex[0..2])?, expand(&hex[2..4])?, expand(&hex[4..6])?),
        3 => {
            let d = |i: usize| expand(&hex[i..=i]).map(|v| v * 17);
            (d(0)?, d(1)?, d(2)?)
        }
        _ => return None,
    };
    Some(slint::Color::from_rgb_u8(r, g, b))
}

impl ServerEditor {
    pub fn token(&self) -> Option<(ServerId, u64)> {
        self.token.clone()
    }
    pub fn edited(&mut self) {
        self.dirty = true;
        self.error = None;
    }
    fn copy(&mut self, ui: &AppWindow, overview: &ServerOverview) {
        ui.set_server_edit_name(overview.name.as_ref().into());
        ui.set_server_edit_description(overview.description.as_ref().into());
        let mut labels = vec!["Disabled".into()];
        labels.extend(overview.channels.iter().map(|(_, name)| name.as_ref().into()));
        ui.set_server_edit_channels(ModelRc::new(VecModel::from(labels)));
        let indices = overview.system_channels.each_ref().map(|channel| {
            channel
                .as_ref()
                .and_then(|id| overview.channels.iter().position(|(room, _)| room.raw == *id))
                .map_or(0, |i| (i + 1) as i32)
        });
        ui.set_server_edit_joined(indices[0]);
        ui.set_server_edit_left(indices[1]);
        ui.set_server_edit_kicked(indices[2]);
        ui.set_server_edit_banned(indices[3]);
        self.original = Some(overview.clone());
        self.icon = ProfileImage::Keep;
        self.banner = ProfileImage::Keep;
        self.dirty = false;
        self.saving = false;
        self.error = None;
    }
    pub fn reset(&mut self, ui: &AppWindow, state: &AppState) {
        if let Some(overview) = state.server_settings.as_ref().and_then(|s| s.overview.as_ref()) {
            self.copy(ui, overview);
        }
    }
    pub fn remove(&mut self, ui: &AppWindow, icon: bool) {
        if ui.get_server_edit_busy() || !ui.get_server_edit_allowed() {
            return;
        }
        if icon {
            self.icon = ProfileImage::Remove
        } else {
            self.banner = ProfileImage::Remove
        }
        self.edited();
    }
    pub fn picked(
        &mut self,
        ui: &AppWindow,
        token: &(ServerId, u64),
        icon: bool,
        result: Result<ProfileImage, String>,
    ) {
        if self.token.as_ref() != Some(token) || ui.get_server_edit_busy() || !ui.get_server_edit_allowed() {
            return;
        }
        match result {
            Ok(image) => {
                if icon {
                    self.icon = image
                } else {
                    self.banner = image
                }
                self.edited();
            }
            Err(error) => self.error = Some(error),
        }
    }
    pub fn save(&mut self, ui: &AppWindow) -> Option<Action> {
        if ui.get_server_edit_busy() || !ui.get_server_edit_allowed() {
            return None;
        }
        let o = self.original.as_ref()?;
        let name = ui.get_server_edit_name();
        let description = ui.get_server_edit_description();
        let indices = [
            ui.get_server_edit_joined(),
            ui.get_server_edit_left(),
            ui.get_server_edit_kicked(),
            ui.get_server_edit_banned(),
        ];
        let system_channels = std::array::from_fn(|i| {
            let old = o.system_channels[i].clone();
            let old_index = old
                .as_ref()
                .and_then(|id| o.channels.iter().position(|(room, _)| room.raw == *id))
                .map_or(0, |i| (i + 1) as i32);
            // Preserve assignments to channels unavailable in the local store until explicitly changed.
            if indices[i] == old_index {
                old
            } else {
                usize::try_from(indices[i] - 1)
                    .ok()
                    .and_then(|i| o.channels.get(i))
                    .map(|(room, _)| room.raw.clone())
            }
        });
        let edit = ServerOverviewEdit {
            name: (name.trim() != o.name.trim()).then(|| name.trim().into()),
            description: (description.trim() != o.description.trim()).then(|| description.trim().into()),
            icon: self.icon.clone(),
            banner: self.banner.clone(),
            system_channels: (system_channels != o.system_channels).then_some(system_channels),
        };
        self.saving = true;
        self.error = None;
        Some(Action::SaveServerOverview(edit))
    }
    /// The role list row being edited picks its fields up from the list.
    fn load_role(&mut self, ui: &AppWindow, roles: &ServerRoles) {
        if self.role_index > roles.roles.len() {
            self.role_index = 0;
        }
        match self.role_index.checked_sub(1).and_then(|i| roles.roles.get(i)) {
            Some(role) => {
                ui.set_server_edit_role_name(role.name.as_ref().into());
                ui.set_server_edit_role_colour(role.colour.as_deref().unwrap_or_default().into());
                ui.set_server_edit_role_hoist(role.hoist);
                self.role_allow = role.allow;
                self.role_deny = role.deny;
            }
            None => {
                self.role_allow = roles.default_allow;
                self.role_deny = 0;
            }
        }
        self.role_dirty = false;
    }

    pub fn select_role(&mut self, ui: &AppWindow, state: &AppState, index: i32) {
        let Some(roles) = state.server_settings.as_ref().and_then(|s| s.roles.as_ref()) else {
            return;
        };
        self.role_index = usize::try_from(index).unwrap_or(0);
        self.load_role(ui, roles);
    }

    pub fn role_edited(&mut self) {
        self.role_dirty = true;
    }

    fn apply_permission_state(&mut self, bit: u64, value: i32) {
        self.role_allow &= !bit;
        self.role_deny &= !bit;
        match value {
            1 => self.role_allow |= bit,
            2 => self.role_deny |= bit,
            _ => {}
        }
        self.role_dirty = true;
    }

    pub fn set_permission_state(&mut self, state: &AppState, row: i32, value: i32) {
        let Some(p) = state
            .server_settings
            .as_ref()
            .and_then(|s| s.roles.as_ref())
            .and_then(|r| r.permissions.get(usize::try_from(row).ok()?))
        else {
            return;
        };
        self.apply_permission_state(p.bit, value);
    }

    /// Save on the Roles page: Everyone's default permissions, or the role's
    /// changed fields. A bad colour is reported instead of sent.
    pub fn save_role(&mut self, ui: &AppWindow, state: &AppState) -> Result<Option<Action>, &'static str> {
        let Some(s) = &state.server_settings else {
            return Ok(None);
        };
        let Some(roles) = &s.roles else {
            return Ok(None);
        };
        let server = s.server.clone();
        let Some(role) = self.role_index.checked_sub(1).and_then(|i| roles.roles.get(i)) else {
            return Ok(Some(Action::ServerAdmin {
                server,
                op: ServerAdmin::SetDefaultPermissions(self.role_allow),
            }));
        };
        let name = ui.get_server_edit_role_name();
        let colour_text = ui.get_server_edit_role_colour();
        let colour = parse_colour(&colour_text)?;
        let hoist = ui.get_server_edit_role_hoist();
        let edit = RoleEdit {
            id: role.id.clone(),
            name: (name.trim() != role.name.as_ref()).then(|| name.trim().into()),
            colour: (colour != role.colour.as_deref()).then(|| colour.map(Into::into)),
            hoist: (hoist != role.hoist).then_some(hoist),
            // Send both bitsets so neutral, allow and deny remain distinct.
            permissions: ((self.role_allow, self.role_deny) != (role.allow, role.deny))
                .then_some((self.role_allow, self.role_deny)),
        };
        Ok(Some(Action::ServerAdmin {
            server,
            op: ServerAdmin::EditRole(edit),
        }))
    }

    pub fn delete_role(&self, state: &AppState) -> Option<Action> {
        let s = state.server_settings.as_ref()?;
        let role = s.roles.as_ref()?.roles.get(self.role_index.checked_sub(1)?)?;
        Some(Action::ServerAdmin {
            server: s.server.clone(),
            op: ServerAdmin::DeleteRole(role.id.clone()),
        })
    }

    /// Move the selected role one place in the network's most-important-first order.
    pub fn move_role(&self, state: &AppState, up: bool) -> Option<Action> {
        let settings = state.server_settings.as_ref()?;
        let roles = settings.roles.as_ref()?;
        if !roles.can_reorder {
            return None;
        }
        let index = self.role_index.checked_sub(1)?;
        let ranks = moved_role_ranks(&roles.roles, index, up)?;
        Some(Action::ServerAdmin {
            server: settings.server.clone(),
            op: ServerAdmin::SetRoleRanks(ranks),
        })
    }

    /// A picture was chosen for the next emoji (or picking failed).
    pub fn emoji_picked(&mut self, token: &(ServerId, u64), result: Result<EmojiFile, String>) {
        if self.token.as_ref() != Some(token) {
            return;
        }
        match result {
            Ok(file) => {
                self.emoji_file = Some(file);
                self.emoji_error = None;
            }
            Err(e) => self.emoji_error = Some(e),
        }
    }

    /// Why the typed name or chosen picture can't make an emoji yet.
    fn emoji_problem(&self, emoji: Option<&ServerEmojis>, name: &str) -> String {
        if let Some(e) = &self.emoji_error {
            return e.clone();
        }
        let (Some(emoji), name) = (emoji, name.trim()) else {
            return String::new();
        };
        if name.is_empty() && self.emoji_file.is_none() {
            return String::new();
        }
        let checked = match &self.emoji_file {
            Some(f) => emoji.check_new(name, f.size),
            None => emoji.rules.validate_name(name),
        };
        // An untouched name shouldn't scold before anything is typed.
        match checked {
            Err(_) if name.is_empty() => String::new(),
            Err(e) => e,
            Ok(()) => String::new(),
        }
    }

    pub fn create_emoji(&mut self, ui: &AppWindow, state: &AppState) -> Option<Action> {
        let s = state.server_settings.as_ref()?;
        let file = self.emoji_file.clone()?;
        let name = ui.get_server_edit_emoji_new_name();
        if !self.emoji_problem(s.emoji.as_ref(), &name).is_empty() || name.trim().is_empty() {
            return None;
        }
        self.emoji_creating = true;
        Some(Action::ServerAdmin {
            server: s.server.clone(),
            op: ServerAdmin::CreateEmoji {
                name: name.trim().into(),
                file,
            },
        })
    }

    pub fn rename_emoji(&self, state: &AppState, row: i32, name: &str) -> Option<Action> {
        let s = state.server_settings.as_ref()?;
        let emoji = s.emoji.as_ref()?.emoji.get(usize::try_from(row).ok()?)?;
        (name.trim() != emoji.name.as_ref()).then(|| Action::ServerAdmin {
            server: s.server.clone(),
            op: ServerAdmin::RenameEmoji {
                id: emoji.id.clone(),
                name: name.trim().into(),
            },
        })
    }

    pub fn delete_emoji(&self, state: &AppState, row: i32) -> Option<Action> {
        let s = state.server_settings.as_ref()?;
        let emoji = s.emoji.as_ref()?.emoji.get(usize::try_from(row).ok()?)?;
        Some(Action::ServerAdmin {
            server: s.server.clone(),
            op: ServerAdmin::DeleteEmoji(emoji.id.clone()),
        })
    }

    /// The other pages' lists and the role editor.
    fn show_pages(&mut self, ui: &AppWindow, s: &ServerSettingsState, images: &mut Images) {
        let o = s.overview.as_ref();
        ui.set_server_edit_server_name(
            o.map(|o| o.name.as_ref())
                .unwrap_or_default()
                .to_uppercase()
                .as_str()
                .into(),
        );
        ui.set_server_edit_page(page_index(s.page));
        ui.set_server_edit_show_roles(o.is_some_and(|o| o.can_manage_roles));
        ui.set_server_edit_show_emoji(o.is_some_and(|o| o.can_manage_emoji));
        ui.set_server_edit_show_invites(o.is_some_and(|o| o.can_manage));
        ui.set_server_edit_show_bans(o.is_some_and(|o| o.can_ban));
        ui.set_server_edit_can_delete(o.is_some_and(|o| o.owner));
        ui.set_server_edit_admin_busy(s.admin_busy);
        let (notice, failed) = match &s.notice {
            Some(Ok(m)) => (m.as_ref(), false),
            Some(Err(e)) => (e.as_ref(), true),
            None => ("", false),
        };
        ui.set_server_edit_notice(notice.into());
        ui.set_server_edit_notice_failed(failed);
        let mut avatar = |url: Option<&str>| url.and_then(|u| images.get(u, AVATAR));
        match s.page {
            ServerPage::Overview => {}
            ServerPage::Emoji => {
                // A finished create clears the form; a failed one keeps it to retry.
                if self.emoji_creating && !s.admin_busy {
                    self.emoji_creating = false;
                    if matches!(s.notice, Some(Ok(_))) {
                        self.emoji_file = None;
                        ui.set_server_edit_emoji_new_name("".into());
                    }
                }
                ui.set_server_edit_emoji_loaded(s.emoji.is_some());
                ui.set_server_edit_emoji_can_create(o.is_some_and(|o| o.can_manage_emoji));
                let name = ui.get_server_edit_emoji_new_name();
                ui.set_server_edit_emoji_new_error(self.emoji_problem(s.emoji.as_ref(), &name).into());
                ui.set_server_edit_emoji_file_label(
                    self.emoji_file
                        .as_ref()
                        .and_then(|f| f.path.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                        .into(),
                );
                ui.set_server_edit_emoji_slots(
                    s.emoji
                        .as_ref()
                        .and_then(ServerEmojis::slots_left)
                        .map(|n| format!("{n} emoji slots remaining"))
                        .unwrap_or_default()
                        .into(),
                );
                let rows: Vec<EmojiItem> = s
                    .emoji
                    .iter()
                    .flat_map(|e| &e.emoji)
                    .map(|e| {
                        let image = e.url.as_deref().and_then(|u| images.get(u, crate::images::EMOJI));
                        EmojiItem {
                            name: e.name.as_ref().into(),
                            creator: e.creator.as_ref().into(),
                            has_image: image.is_some(),
                            image: image.unwrap_or_default(),
                        }
                    })
                    .collect();
                ui.set_server_edit_emoji(ModelRc::new(VecModel::from(rows)));
            }
            ServerPage::Invites => {
                ui.set_server_edit_invites_loaded(s.invites.is_some());
                let rows: Vec<ServerInviteItem> = s
                    .invites
                    .iter()
                    .flatten()
                    .map(|i| {
                        let image = avatar(i.creator_avatar.as_deref());
                        ServerInviteItem {
                            code: i.code.as_ref().into(),
                            creator: i.creator.as_ref().into(),
                            channel: i.channel.as_ref().into(),
                            has_avatar: image.is_some(),
                            avatar: image.unwrap_or_default(),
                        }
                    })
                    .collect();
                ui.set_server_edit_invites(ModelRc::new(VecModel::from(rows)));
            }
            ServerPage::Bans => {
                ui.set_server_edit_bans_loaded(s.bans.is_some());
                let rows: Vec<BanItem> = s
                    .bans
                    .iter()
                    .flatten()
                    .map(|b| {
                        let image = avatar(b.avatar_url.as_deref());
                        BanItem {
                            name: b.name.as_ref().into(),
                            reason: b.reason.as_ref().into(),
                            has_avatar: image.is_some(),
                            avatar: image.unwrap_or_default(),
                        }
                    })
                    .collect();
                ui.set_server_edit_bans(ModelRc::new(VecModel::from(rows)));
            }
            ServerPage::Roles => {
                ui.set_server_edit_roles_loaded(s.roles.is_some());
                let Some(roles) = &s.roles else {
                    return;
                };
                ui.set_server_edit_roles_reorderable(roles.can_reorder);
                if self.roles_seen.as_ref() != Some(roles) {
                    // Keep the same role selected across a refreshed list.
                    let id = self
                        .role_index
                        .checked_sub(1)
                        .and_then(|i| self.roles_seen.as_ref()?.roles.get(i))
                        .map(|r| r.id.clone());
                    if let Some(id) = id {
                        self.role_index = roles.roles.iter().position(|r| r.id == id).map_or(0, |i| i + 1);
                    }
                    if !self.role_dirty || s.notice.as_ref().is_some_and(Result::is_ok) {
                        self.load_role(ui, roles);
                    }
                    self.roles_seen = Some(roles.clone());
                }
                let mut rows = vec![AdminRoleItem {
                    name: "Everyone".into(),
                    colour: slint::Color::default(),
                    has_colour: false,
                }];
                rows.extend(roles.roles.iter().map(|r| {
                    let colour = r.colour.as_deref().and_then(slint_colour);
                    AdminRoleItem {
                        name: r.name.as_ref().into(),
                        has_colour: colour.is_some(),
                        colour: colour.unwrap_or_default(),
                    }
                }));
                ui.set_server_edit_roles(ModelRc::new(VecModel::from(rows)));
                ui.set_server_edit_role_index(i32::try_from(self.role_index).unwrap_or(0));
                ui.set_server_edit_role_dirty(self.role_dirty);
                let perms: Vec<PermissionItem> = roles
                    .permissions
                    .iter()
                    .map(|p| PermissionItem {
                        heading: p.heading.as_deref().unwrap_or_default().into(),
                        title: p.title.as_ref().into(),
                        description: p.description.as_ref().into(),
                        state: if self.role_allow & p.bit != 0 {
                            1
                        } else if self.role_deny & p.bit != 0 {
                            2
                        } else {
                            0
                        },
                    })
                    .collect();
                ui.set_server_edit_permissions(ModelRc::new(VecModel::from(perms)));
            }
        }
    }

    pub fn show(&mut self, ui: &AppWindow, state: &AppState, images: &mut Images) {
        // The selected server's menu, with what we may do there.
        let rights = state
            .selected_server
            .as_ref()
            .and_then(|id| state.server(id))
            .map(|s| s.rights);
        ui.set_server_menu(rights.is_some());
        let rights = rights.unwrap_or_default();
        ui.set_server_settings_available(rights.settings);
        ui.set_can_edit_server_profile(
            state
                .selected_server
                .as_ref()
                .and_then(|id| state.server(id))
                .is_some_and(|s| s.edit_own_profile),
        );
        ui.set_server_can_invite(rights.invite);
        ui.set_server_notify_levels(rights.notify_levels);
        ui.set_server_notify(i32::from(rights.notify));
        ui.set_server_can_create_channel(rights.create_channels);
        ui.set_server_can_create_category(rights.manage_categories);
        ui.set_server_can_reorder(rights.reorder_channels);
        ui.set_server_can_leave(!rights.owner);
        ui.set_server_can_report(rights.report);
        ui.set_invite_open(state.invite_link.is_some());
        ui.set_invite_link(
            state
                .invite_link
                .as_ref()
                .and_then(|(_, l)| l.as_deref())
                .unwrap_or_default()
                .into(),
        );
        let Some(s) = &state.server_settings else {
            ui.set_server_settings_open(false);
            *self = Self::default();
            return;
        };
        let identity_changed = self
            .token
            .as_ref()
            .is_none_or(|(id, request)| *id != s.server || (*request != s.request && !self.saving));
        // Reopening the same server starts with no overview; it must discard the old draft too.
        if identity_changed || (s.overview.is_none() && self.original.is_some()) {
            *self = Self::default();
            // The emoji name field lives on the window; don't carry it over.
            ui.set_server_edit_emoji_new_name("".into());
        }
        self.token = Some((s.server.clone(), s.request));
        if let Some(o) = &s.overview
            && (self.original.is_none() || (self.saving && s.saved && !s.busy))
        {
            self.copy(ui, o);
        }
        if !s.busy {
            self.saving = false;
        }
        ui.set_server_settings_open(true);
        ui.set_server_edit_loaded(s.overview.is_some());
        ui.set_server_edit_busy(s.busy);
        ui.set_server_edit_allowed(s.overview.as_ref().is_some_and(|o| o.can_manage));
        ui.set_server_edit_dirty(self.dirty);
        let error = self.error.as_deref().or(s.error.as_deref());
        ui.set_server_edit_failed(error.is_some());
        ui.set_server_edit_status(
            error
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    if s.busy {
                        if s.overview.is_some() {
                            "Saving server…"
                        } else {
                            "Loading server…"
                        }
                    } else if s.saved && !self.dirty {
                        "Server saved."
                    } else {
                        ""
                    }
                    .into()
                })
                .into(),
        );
        let label = |image: &ProfileImage| match image {
            ProfileImage::Keep => "".to_owned(),
            ProfileImage::Remove => "Will be removed when you save.".into(),
            ProfileImage::File { .. } => "New: saved when you save.".into(),
        };
        // A picked (cropped) picture shows at once, before it's uploaded.
        let picked = |image: &ProfileImage| match image {
            ProfileImage::File { path, .. } => slint::Image::load_from_path(path).ok(),
            _ => None,
        };
        ui.set_server_edit_icon_label(label(&self.icon).into());
        ui.set_server_edit_banner_label(label(&self.banner).into());
        let o = s.overview.as_ref();
        let icon = picked(&self.icon).or_else(|| {
            o.and_then(|o| o.icon_url.as_deref())
                .filter(|_| self.icon != ProfileImage::Remove)
                .and_then(|u| images.get(u, Fit::Circle(160)))
        });
        let banner = picked(&self.banner).or_else(|| {
            o.and_then(|o| o.banner_url.as_deref())
                .filter(|_| self.banner != ProfileImage::Remove)
                .and_then(|u| images.get(u, crate::images::EDIT_BANNER))
        });
        ui.set_server_edit_has_icon(icon.is_some());
        ui.set_server_edit_has_banner(banner.is_some());
        if let Some(image) = icon {
            ui.set_server_edit_icon(image);
        }
        if let Some(image) = banner {
            ui.set_server_edit_banner(image);
        }
        self.show_pages(ui, s, images);
    }
}

fn moved_role_ranks(
    roles: &[godwit_core::server_settings::ServerRole],
    index: usize,
    up: bool,
) -> Option<Vec<std::sync::Arc<str>>> {
    let target = if up {
        index.checked_sub(1)?
    } else {
        index.checked_add(1)?
    };
    if index >= roles.len() || target >= roles.len() {
        return None;
    }
    let mut ranks: Vec<_> = roles.iter().map(|role| role.id.clone()).collect();
    ranks.swap(index, target);
    Some(ranks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_rank_moves_swap_neighbors_and_stop_at_the_edges() {
        use godwit_core::server_settings::ServerRole;
        let roles = ["a", "b", "c"].map(|id| ServerRole {
            id: id.into(),
            name: id.into(),
            colour: None,
            hoist: false,
            allow: 0,
            deny: 0,
        });
        assert_eq!(
            moved_role_ranks(&roles, 1, true),
            Some(vec!["b".into(), "a".into(), "c".into()])
        );
        assert_eq!(
            moved_role_ranks(&roles, 1, false),
            Some(vec!["a".into(), "c".into(), "b".into()])
        );
        assert_eq!(moved_role_ranks(&roles, 0, true), None);
        assert_eq!(moved_role_ranks(&roles, 2, false), None);
    }

    #[test]
    fn role_permission_draft_keeps_explicit_deny_and_can_return_to_neutral() {
        use godwit_core::server_settings::{PermissionInfo, ServerRole};
        let bit = 1_u64 << 12;
        let role = ServerRole {
            id: "r".into(),
            name: "Role".into(),
            colour: None,
            hoist: false,
            allow: 0,
            deny: bit,
        };
        let permission = PermissionInfo {
            bit,
            heading: None,
            title: "Permission".into(),
            description: "A permission".into(),
        };
        let mut editor = ServerEditor {
            role_allow: role.allow,
            role_deny: role.deny,
            ..Default::default()
        };
        editor.apply_permission_state(permission.bit, 2);
        assert_eq!(editor.role_deny, bit);
        editor.apply_permission_state(permission.bit, 0);
        assert_eq!(editor.role_allow | editor.role_deny, 0);
        editor.apply_permission_state(permission.bit, 1);
        assert_eq!(editor.role_allow, bit);
        assert_eq!(editor.role_deny, 0);
    }

    #[test]
    fn role_colours_are_plain_hex_or_none() {
        assert_eq!(parse_colour("  "), Ok(None));
        assert_eq!(parse_colour("#a1B2c3"), Ok(Some("#a1B2c3")));
        assert_eq!(parse_colour("#abc"), Ok(Some("#abc")));
        assert!(parse_colour("red").is_err());
        assert!(parse_colour("#abcd").is_err());
        assert_eq!(slint_colour("#fff"), Some(slint::Color::from_rgb_u8(255, 255, 255)));
        assert_eq!(slint_colour("#102030"), Some(slint::Color::from_rgb_u8(16, 32, 48)));
        assert_eq!(slint_colour("var(--x)"), None);
        assert_eq!(slint_colour("#ééé"), None, "no byte slicing inside a character");
    }

    #[test]
    fn page_indices_round_trip() {
        for page in [
            ServerPage::Overview,
            ServerPage::Roles,
            ServerPage::Emoji,
            ServerPage::Invites,
            ServerPage::Bans,
        ] {
            assert_eq!(page_from_index(page_index(page)), page);
        }
    }
}
