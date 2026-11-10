// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Server drafts stay local until Save; refreshes and failures preserve edits.
use crate::{
    images::{AVATAR, Fit, Images},
    ui::{
        AdminRoleItem, AppWindow, BanItem, EmojiItem, ModerationLogItem, ModerationRuleItem, PermissionItem,
        RoleMemberItem, ServerInviteItem,
    },
};
use godwit_core::application::{Action, AppState};
use godwit_core::ids::{ServerId, UserId};
use godwit_core::own_profile::ProfileImage;
use godwit_core::server_settings::{
    EmojiFile, ExpressionKind, RoleEdit, RoleMember, RoleMembers, SafetyChoices, ServerAdmin, ServerEmojis,
    ServerOverview, ServerOverviewEdit, ServerPage, ServerRoles, ServerSettingsState,
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
    /// The role's Members tab: whether it is showing, the filter over the
    /// role's members, and the Add members picker (its filter and ticks).
    members_tab: bool,
    member_search: String,
    picking: bool,
    picker_search: String,
    picked: Vec<UserId>,
    /// The Emoji page: the picture chosen for the next emoji, a problem with
    /// picking it, and whether a create is waiting for its answer.
    emoji_file: Option<EmojiFile>,
    emoji_error: Option<String>,
    emoji_creating: bool,
    /// Which of the Emoji, Stickers and Sounds pages the form above belongs to.
    expression_page: Option<ServerPage>,
}

/// Which expression kind a page manages; None is the Emoji page (or no such page).
fn page_kind(page: ServerPage) -> Option<ExpressionKind> {
    match page {
        ServerPage::Stickers => Some(ExpressionKind::Sticker),
        ServerPage::Sounds => Some(ExpressionKind::Sound),
        _ => None,
    }
}

/// The loaded list the Emoji, Stickers or Sounds page shows.
fn page_list(s: &godwit_core::server_settings::ServerSettingsState) -> Option<&ServerEmojis> {
    match page_kind(s.page) {
        Some(kind) => s.expressions(kind),
        None => s.emoji.as_ref(),
    }
}

/// The core's page as the view's index, and back.
fn page_index(page: ServerPage) -> i32 {
    match page {
        ServerPage::Overview => 0,
        ServerPage::Roles => 1,
        ServerPage::Emoji => 2,
        ServerPage::Invites => 3,
        ServerPage::Bans => 4,
        ServerPage::ModerationLog => 5,
        ServerPage::Stickers => 6,
        ServerPage::Sounds => 7,
        ServerPage::AutoMod => 8,
    }
}

pub fn page_from_index(i: i32) -> ServerPage {
    match i {
        1 => ServerPage::Roles,
        2 => ServerPage::Emoji,
        3 => ServerPage::Invites,
        4 => ServerPage::Bans,
        5 => ServerPage::ModerationLog,
        6 => ServerPage::Stickers,
        7 => ServerPage::Sounds,
        8 => ServerPage::AutoMod,
        _ => ServerPage::Overview,
    }
}

/// Most candidates the Add members picker lists at once; typing narrows them.
const PICKER_LIMIT: usize = 100;

/// The people whose name contains `query` (ignoring case), at most `limit`.
fn matching<'a>(list: &'a [RoleMember], query: &str, limit: usize) -> Vec<&'a RoleMember> {
    let query = query.trim().to_lowercase();
    list.iter()
        .filter(|m| query.is_empty() || m.name.to_lowercase().contains(&query))
        .take(limit)
        .collect()
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
        let safety = overview.safety.as_ref();
        ui.set_server_edit_has_safety(safety.is_some());
        let choice = |c: Option<&SafetyChoices>| {
            let options: Vec<slint::SharedString> = c
                .map(|c| c.options.iter().map(|o| o.label.as_ref().into()).collect())
                .unwrap_or_default();
            let index = c.and_then(|c| c.options.iter().position(|o| o.value == c.selected));
            (ModelRc::new(VecModel::from(options)), index.map_or(0, |i| i as i32))
        };
        let (options, index) = choice(safety.map(|s| &s.verification));
        ui.set_server_edit_verification_options(options);
        ui.set_server_edit_verification(index);
        let (options, index) = choice(safety.map(|s| &s.content_filter));
        ui.set_server_edit_filter_options(options);
        ui.set_server_edit_filter(index);
        let mfa = safety.and_then(|s| s.mod_mfa.as_ref());
        ui.set_server_edit_has_mfa(mfa.is_some());
        ui.set_server_edit_mfa(mfa.is_some_and(|m| m.on));
        ui.set_server_edit_mfa_locked(mfa.and_then(|m| m.locked.as_deref()).unwrap_or_default().into());
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
        let chosen = |c: Option<&SafetyChoices>, index: i32| {
            let c = c?;
            let value = c.options.get(usize::try_from(index).ok()?)?.value;
            (value != c.selected).then_some(value)
        };
        let safety = o.safety.as_ref();
        let edit = ServerOverviewEdit {
            verification: chosen(safety.map(|s| &s.verification), ui.get_server_edit_verification()),
            content_filter: chosen(safety.map(|s| &s.content_filter), ui.get_server_edit_filter()),
            mod_mfa: safety
                .and_then(|s| s.mod_mfa.as_ref())
                .filter(|m| m.locked.is_none() && m.on != ui.get_server_edit_mfa())
                .map(|_| ui.get_server_edit_mfa()),
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
        self.close_picker();
        self.member_search.clear();
        ui.set_server_edit_role_member_search("".into());
    }

    fn selected_role_id(&self, state: &AppState) -> Option<std::sync::Arc<str>> {
        let roles = state.server_settings.as_ref()?.roles.as_ref()?;
        Some(roles.roles.get(self.role_index.checked_sub(1)?)?.id.clone())
    }

    /// The members of the selected role, once they have arrived for it.
    fn role_members<'a>(&self, state: &'a AppState) -> Option<&'a RoleMembers> {
        let id = self.selected_role_id(state)?;
        state
            .server_settings
            .as_ref()?
            .role_members
            .as_ref()
            .filter(|m| m.role == id)
    }

    /// Asks for the selected role's members when the tab shows them and they
    /// are not here yet.
    pub fn load_role_members(&self, state: &AppState) -> Option<Action> {
        let s = state.server_settings.as_ref()?;
        if !self.members_tab || !s.roles.as_ref()?.role_members || self.role_members(state).is_some() {
            return None;
        }
        Some(Action::ServerAdmin {
            server: s.server.clone(),
            op: ServerAdmin::LoadRoleMembers(self.selected_role_id(state)?),
        })
    }

    pub fn set_members_tab(&mut self, on: bool) {
        self.members_tab = on;
        self.close_picker();
    }

    pub fn member_search_edited(&mut self, text: &str) {
        self.member_search = text.to_owned();
    }

    pub fn picker_search_edited(&mut self, text: &str) {
        self.picker_search = text.to_owned();
    }

    pub fn open_picker(&mut self) {
        self.picking = true;
    }

    pub fn close_picker(&mut self) {
        self.picking = false;
        self.picker_search.clear();
        self.picked.clear();
    }

    /// Ticks or unticks the picker's row.
    pub fn toggle_picked(&mut self, state: &AppState, row: i32) {
        let Some(user) = self
            .role_members(state)
            .and_then(|m| {
                matching(&m.candidates, &self.picker_search, PICKER_LIMIT)
                    .get(usize::try_from(row).ok()?)
                    .copied()
            })
            .map(|m| m.user.clone())
        else {
            return;
        };
        match self.picked.iter().position(|u| *u == user) {
            Some(i) => {
                self.picked.remove(i);
            }
            None => self.picked.push(user),
        }
    }

    /// Gives the role to everyone ticked in the picker.
    pub fn add_picked(&mut self, state: &AppState) -> Option<Action> {
        let role = self.selected_role_id(state)?;
        let server = state.server_settings.as_ref()?.server.clone();
        let users = std::mem::take(&mut self.picked);
        self.close_picker();
        (!users.is_empty()).then_some(Action::ServerAdmin {
            server,
            op: ServerAdmin::SetRoleMembers { role, users, on: true },
        })
    }

    /// Takes the role from the member in row `row` of the filtered list.
    pub fn remove_role_member(&self, state: &AppState, row: i32) -> Option<Action> {
        let role = self.selected_role_id(state)?;
        let user = matching(&self.role_members(state)?.members, &self.member_search, usize::MAX)
            .get(usize::try_from(row).ok()?)?
            .user
            .clone();
        Some(Action::ServerAdmin {
            server: state.server_settings.as_ref()?.server.clone(),
            op: ServerAdmin::SetRoleMembers {
                role,
                users: vec![user],
                on: false,
            },
        })
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
        if !self.emoji_problem(page_list(s), &name).is_empty() || name.trim().is_empty() {
            return None;
        }
        self.emoji_creating = true;
        let name: std::sync::Arc<str> = name.trim().into();
        Some(Action::ServerAdmin {
            server: s.server.clone(),
            op: match page_kind(s.page) {
                Some(kind) => ServerAdmin::CreateExpression { kind, name, file },
                None => ServerAdmin::CreateEmoji { name, file },
            },
        })
    }

    pub fn rename_emoji(&self, state: &AppState, row: i32, name: &str) -> Option<Action> {
        let s = state.server_settings.as_ref()?;
        let emoji = page_list(s)?.emoji.get(usize::try_from(row).ok()?)?;
        let (id, name): (std::sync::Arc<str>, std::sync::Arc<str>) = (emoji.id.clone(), name.trim().into());
        (name.as_ref() != emoji.name.as_ref()).then(|| Action::ServerAdmin {
            server: s.server.clone(),
            op: match page_kind(s.page) {
                Some(kind) => ServerAdmin::RenameExpression { kind, id, name },
                None => ServerAdmin::RenameEmoji { id, name },
            },
        })
    }

    pub fn delete_emoji(&self, state: &AppState, row: i32) -> Option<Action> {
        let s = state.server_settings.as_ref()?;
        let id = page_list(s)?.emoji.get(usize::try_from(row).ok()?)?.id.clone();
        Some(Action::ServerAdmin {
            server: s.server.clone(),
            op: match page_kind(s.page) {
                Some(kind) => ServerAdmin::DeleteExpression { kind, id },
                None => ServerAdmin::DeleteEmoji(id),
            },
        })
    }

    /// The other pages' lists and the role editor.
    fn show_pages(&mut self, ui: &AppWindow, s: &ServerSettingsState, state: &AppState, images: &mut Images) {
        let o = s.overview.as_ref();
        ui.set_server_edit_server_name(
            o.map(|o| o.name.as_ref())
                .unwrap_or_default()
                .to_uppercase()
                .as_str()
                .into(),
        );
        ui.set_server_edit_page(page_index(s.page));
        let log = state.moderation_logs.get(&s.server);
        ui.set_server_edit_moderation_log_loading(log.is_some_and(|l| l.loading));
        ui.set_server_edit_moderation_log_has_more(log.is_some_and(|l| l.has_more));
        ui.set_server_edit_moderation_log_error(log.and_then(|l| l.error.as_deref()).unwrap_or_default().into());
        let rows = log
            .into_iter()
            .flat_map(|l| &l.entries)
            .map(|e| {
                let avatar = e.actor_avatar_url.as_deref().and_then(|url| images.get(url, AVATAR));
                let has_avatar = avatar.is_some();
                ModerationLogItem {
                    actor: e.actor.as_ref().into(),
                    avatar: avatar.unwrap_or_else(crate::view::no_image),
                    has_avatar,
                    text: e.text.as_ref().into(),
                    target: e.target.as_deref().unwrap_or_default().into(),
                    reason: e.reason.as_deref().unwrap_or_default().into(),
                    time: chrono::DateTime::from_timestamp_millis(e.time_ms)
                        .map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                        .unwrap_or_default()
                        .into(),
                }
            })
            .collect::<Vec<_>>();
        ui.set_server_edit_moderation_log(ModelRc::new(VecModel::from(rows)));
        ui.set_server_edit_show_roles(o.is_some_and(|o| o.can_manage_roles));
        ui.set_server_edit_show_emoji(o.is_some_and(|o| o.can_manage_emoji));
        ui.set_server_edit_show_stickers(o.is_some_and(|o| o.can_manage_stickers));
        ui.set_server_edit_show_sounds(o.is_some_and(|o| o.can_manage_sounds));
        if !matches!(s.page, ServerPage::Emoji | ServerPage::Stickers | ServerPage::Sounds) {
            self.expression_page = None;
        }
        ui.set_server_edit_show_invites(o.is_some_and(|o| o.can_manage));
        ui.set_server_edit_show_automod(o.is_some_and(|o| o.can_manage_automod));
        ui.set_server_edit_show_bans(o.is_some_and(|o| o.can_ban));
        ui.set_server_edit_show_moderation_log(
            state
                .server(&s.server)
                .is_some_and(|server| server.can_view_moderation_log),
        );
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
            ServerPage::Emoji | ServerPage::Stickers | ServerPage::Sounds => {
                // Each page starts with an empty form.
                if self.expression_page != Some(s.page) {
                    self.expression_page = Some(s.page);
                    self.emoji_file = None;
                    self.emoji_error = None;
                    self.emoji_creating = false;
                    ui.set_server_edit_emoji_new_name("".into());
                }
                let kind = page_kind(s.page);
                let list = page_list(s);
                let noun = kind.map_or("emoji", ExpressionKind::noun);
                ui.set_server_edit_expression_noun(noun.into());
                ui.set_server_edit_expression_hint(
                    match kind {
                        None => "Lowercase letters, numbers and underscores only.",
                        Some(ExpressionKind::Sticker) => "PNG, APNG or GIF, up to 500 KB.",
                        Some(ExpressionKind::Sound) => "MP3 or Ogg, up to 512 KB and 5.2 seconds.",
                    }
                    .into(),
                );
                // A finished create clears the form; a failed one keeps it to retry.
                if self.emoji_creating && !s.admin_busy {
                    self.emoji_creating = false;
                    if matches!(s.notice, Some(Ok(_))) {
                        self.emoji_file = None;
                        ui.set_server_edit_emoji_new_name("".into());
                    }
                }
                ui.set_server_edit_emoji_loaded(list.is_some());
                ui.set_server_edit_emoji_can_create(o.is_some_and(|o| match kind {
                    None => o.can_manage_emoji,
                    Some(ExpressionKind::Sticker) => o.can_manage_stickers,
                    Some(ExpressionKind::Sound) => o.can_manage_sounds,
                }));
                let name = ui.get_server_edit_emoji_new_name();
                ui.set_server_edit_emoji_new_error(self.emoji_problem(list, &name).into());
                ui.set_server_edit_emoji_file_label(
                    self.emoji_file
                        .as_ref()
                        .and_then(|f| f.path.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                        .into(),
                );
                ui.set_server_edit_emoji_slots(
                    list.and_then(ServerEmojis::slots_left)
                        .map(|n| format!("{n} {noun} slots remaining"))
                        .unwrap_or_default()
                        .into(),
                );
                let rows: Vec<EmojiItem> = list
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
            ServerPage::AutoMod => {
                ui.set_server_edit_automod_loaded(s.moderation_rules.is_some());
                let rows: Vec<ModerationRuleItem> = s
                    .moderation_rules
                    .iter()
                    .flatten()
                    .map(|r| ModerationRuleItem {
                        name: r.name.as_ref().into(),
                        kind: r.kind.as_ref().into(),
                        keywords: r.keywords.join(", ").into(),
                        enabled: r.enabled,
                    })
                    .collect();
                ui.set_server_edit_automod(ModelRc::new(VecModel::from(rows)));
            }
            ServerPage::ModerationLog => {}
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
                let tab = roles.role_members && self.role_index > 0 && self.members_tab;
                ui.set_server_edit_role_members_available(roles.role_members && self.role_index > 0);
                ui.set_server_edit_role_members_tab(tab);
                ui.set_server_edit_role_picking(tab && self.picking);
                let data = self.role_members(state).filter(|_| tab);
                ui.set_server_edit_role_members_loaded(data.is_some());
                let item =
                    |m: &RoleMember, checked: bool, avatar: &mut dyn FnMut(Option<&str>) -> Option<slint::Image>| {
                        let image = avatar(m.avatar_url.as_deref());
                        RoleMemberItem {
                            name: m.name.as_ref().into(),
                            has_avatar: image.is_some(),
                            avatar: image.unwrap_or_default(),
                            checked,
                        }
                    };
                let members: Vec<RoleMemberItem> = data
                    .iter()
                    .flat_map(|d| matching(&d.members, &self.member_search, usize::MAX))
                    .map(|m| item(m, false, &mut avatar))
                    .collect();
                ui.set_server_edit_role_members(ModelRc::new(VecModel::from(members)));
                let shown: Vec<&RoleMember> = data
                    .filter(|_| self.picking)
                    .map(|d| matching(&d.candidates, &self.picker_search, PICKER_LIMIT))
                    .unwrap_or_default();
                ui.set_server_edit_role_picker_more(
                    data.is_some_and(|d| shown.len() == PICKER_LIMIT && d.candidates.len() > PICKER_LIMIT),
                );
                let picker: Vec<RoleMemberItem> = shown
                    .into_iter()
                    .map(|m| item(m, self.picked.contains(&m.user), &mut avatar))
                    .collect();
                ui.set_server_edit_role_picker(ModelRc::new(VecModel::from(picker)));
                ui.set_server_edit_role_picked(i32::try_from(self.picked.len()).unwrap_or(i32::MAX));
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
        self.show_pages(ui, s, state, images);
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
    fn member_search_ignores_case_and_respects_the_limit() {
        use godwit_core::ids::AccountId;
        use godwit_core::network::Network;
        let account = AccountId::new(Network::Stoat, "me");
        let list: Vec<RoleMember> = ["Amy", "bob", "Abe"]
            .iter()
            .map(|n| RoleMember {
                user: UserId::new(account.clone(), *n),
                name: (*n).into(),
                avatar_url: None,
            })
            .collect();
        let names = |q: &str, l: usize| {
            matching(&list, q, l)
                .iter()
                .map(|m| m.name.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(names("", 10), ["Amy", "bob", "Abe"]);
        assert_eq!(names(" A", 10), ["Amy", "Abe"]);
        assert_eq!(names("a", 1), ["Amy"]);
        assert!(names("zz", 10).is_empty());
    }

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
            ServerPage::ModerationLog,
            ServerPage::Stickers,
            ServerPage::Sounds,
            ServerPage::AutoMod,
        ] {
            assert_eq!(page_from_index(page_index(page)), page);
        }
    }
}
