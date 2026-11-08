// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A server's settings: overview, invites, bans, roles, members and deleting it.

use super::*;

impl Shell {
    pub fn open_server_settings(&mut self, ui: &AppWindow) {
        if let Some(server) = self.app.state().selected_server.clone() {
            self.act(ui, Action::OpenServerSettings(server));
        }
    }
    pub fn reload_server_overview(&mut self, ui: &AppWindow) {
        if let Some(s) = &self.app.state().server_settings
            && !s.busy
        {
            self.act(ui, Action::OpenServerSettings(s.server.clone()));
        }
    }
    pub fn save_server_overview(&mut self, ui: &AppWindow) {
        if let Some(action) = self.server_editor.save(ui) {
            self.act(ui, action);
        }
    }
    pub fn reset_server_overview(&mut self, ui: &AppWindow) {
        self.server_editor.reset(ui, self.app.state());
        self.show_server_settings(ui);
    }
    pub fn server_overview_edited(&mut self, ui: &AppWindow) {
        self.server_editor.edited();
        self.show_server_settings(ui);
    }
    pub fn remove_server_image(&mut self, ui: &AppWindow, icon: bool) {
        self.server_editor.remove(ui, icon);
        self.show_server_settings(ui);
    }
    pub fn server_edit_token(&self) -> Option<(ServerId, u64)> {
        self.server_editor.token()
    }

    pub fn server_settings_page(&mut self, ui: &AppWindow, page: i32) {
        self.act(
            ui,
            Action::ServerSettingsPage(crate::server_settings::page_from_index(page)),
        );
    }

    /// An admin change on the open settings' server.
    pub(super) fn server_admin(&mut self, ui: &AppWindow, op: godwit_core::server_settings::ServerAdmin) {
        if let Some(server) = self.app.state().server_settings.as_ref().map(|s| s.server.clone()) {
            self.act(ui, Action::ServerAdmin { server, op });
        }
    }

    /// The invite or ban in row `row` of the open settings' list.
    pub(super) fn admin_row<T: Clone>(
        &self,
        row: i32,
        list: impl Fn(&godwit_core::server_settings::ServerSettingsState) -> Option<&Vec<T>>,
    ) -> Option<T> {
        let s = self.app.state().server_settings.as_ref()?;
        list(s)?.get(usize::try_from(row).ok()?).cloned()
    }

    pub fn delete_server_invite(&mut self, ui: &AppWindow, row: i32) {
        if let Some(invite) = self.admin_row(row, |s| s.invites.as_ref()) {
            self.server_admin(ui, godwit_core::server_settings::ServerAdmin::DeleteInvite(invite.code));
        }
    }

    pub fn copy_server_invite(&mut self, row: i32) {
        if let Some(invite) = self.admin_row(row, |s| s.invites.as_ref()) {
            copy_to_clipboard(invite.link.to_string());
        }
    }

    pub fn unban_server_user(&mut self, ui: &AppWindow, row: i32) {
        if let Some(ban) = self.admin_row(row, |s| s.bans.as_ref()) {
            self.server_admin(ui, godwit_core::server_settings::ServerAdmin::Unban(ban.user));
        }
    }

    pub fn select_server_role(&mut self, ui: &AppWindow, row: i32) {
        self.server_editor.select_role(ui, self.app.state(), row);
        self.show_server_settings(ui);
    }

    pub fn create_server_role(&mut self, ui: &AppWindow, name: &str) {
        if !name.trim().is_empty() {
            self.server_admin(
                ui,
                godwit_core::server_settings::ServerAdmin::CreateRole(name.trim().into()),
            );
        }
    }

    pub fn server_role_edited(&mut self, ui: &AppWindow) {
        self.server_editor.role_edited();
        self.show_server_settings(ui);
    }

    pub fn toggle_server_permission(&mut self, ui: &AppWindow, row: i32) {
        self.server_editor.toggle_permission(self.app.state(), row);
        self.show_server_settings(ui);
    }

    pub fn save_server_role(&mut self, ui: &AppWindow) {
        match self.server_editor.save_role(ui, self.app.state()) {
            Ok(Some(action)) => self.act(ui, action),
            Ok(None) => {}
            Err(problem) => {
                ui.set_server_edit_notice(problem.into());
                ui.set_server_edit_notice_failed(true);
            }
        }
    }

    pub fn delete_server_role(&mut self, ui: &AppWindow) {
        if let Some(action) = self.server_editor.delete_role(self.app.state()) {
            self.act(ui, action);
        }
    }

    /// The profile card's Kick or Ban, confirmed: the change arrives as the
    /// member leaving; a refusal shows in Problems.
    pub fn moderate_member(&mut self, ui: &AppWindow, ban: bool, reason: &str, delete_history: usize) {
        use godwit_core::server_settings::ServerAdmin;
        let Some((user, Some(profile))) = self.app.state().profile.clone() else {
            return;
        };
        let Some(m) = profile.moderation else {
            return;
        };
        let op = match ban {
            true if m.ban => ServerAdmin::Ban {
                user,
                reason: reason.trim().into(),
                delete_seconds: godwit_core::server_settings::BAN_DELETE_CHOICES
                    .get(delete_history)
                    .map_or(0, |c| c.1),
            },
            false if m.kick => ServerAdmin::Kick(user),
            _ => return,
        };
        self.act(ui, Action::ServerAdmin { server: m.server, op });
        self.act(ui, Action::CloseProfile);
    }

    /// The profile card's Roles checklist: one role given or taken. The
    /// card comes back updated with the member's change.
    pub fn toggle_member_role(&mut self, ui: &AppWindow, index: usize, on: bool) {
        use godwit_core::server_settings::ServerAdmin;
        self.member_change(ui, |user, m| {
            let role = m.roles.get(index)?.id.clone();
            Some(ServerAdmin::SetMemberRole { user, role, on })
        });
    }

    /// Lifts the member's timeout.
    pub fn lift_member_timeout(&mut self, ui: &AppWindow) {
        self.timeout_member(ui, None);
    }

    /// Times the member out for the dialog's choice (None lifts it; the
    /// dialog only ever passes a valid index).
    pub fn timeout_member(&mut self, ui: &AppWindow, choice: Option<usize>) {
        use godwit_core::server_settings::{ServerAdmin, TIMEOUT_CHOICES};
        self.member_change(ui, |user, m| {
            let duration = match choice {
                Some(i) => Some(std::time::Duration::from_secs(TIMEOUT_CHOICES.get(i)?.1)),
                None => None,
            };
            m.timeout.then_some(ServerAdmin::Timeout { user, duration })
        });
    }

    /// A server nickname for the card's member; empty resets it.
    pub fn set_member_nickname(&mut self, ui: &AppWindow, nick: &str) {
        use godwit_core::server_settings::ServerAdmin;
        let nick = nick.trim();
        self.member_change(ui, |user, m| {
            m.nickname.then(|| ServerAdmin::SetServerNickname {
                user,
                nick: (!nick.is_empty()).then(|| nick.into()),
            })
        });
    }

    fn member_change(
        &mut self,
        ui: &AppWindow,
        op: impl FnOnce(
            godwit_core::ids::UserId,
            &godwit_core::members::Moderation,
        ) -> Option<godwit_core::server_settings::ServerAdmin>,
    ) {
        let Some((user, Some(profile))) = self.app.state().profile.clone() else {
            return;
        };
        let Some(m) = profile.moderation else {
            return;
        };
        if let Some(op) = op(user, &m) {
            self.act(ui, Action::ServerAdmin { server: m.server, op });
        }
    }

    /// Remove the profile's member from the group after the confirmation dialog.
    pub fn remove_group_member(&mut self, ui: &AppWindow) {
        let Some((user, Some(profile))) = self.app.state().profile.clone() else {
            return;
        };
        let Some(moderation) = profile.group_moderation else {
            return;
        };
        if moderation.remove_member {
            self.act(
                ui,
                Action::RemoveGroupMember {
                    room: moderation.room,
                    user,
                },
            );
            self.act(ui, Action::CloseProfile);
        }
    }

    /// Transfer group ownership to the profile's member after confirmation.
    pub fn transfer_group_ownership(&mut self, ui: &AppWindow) {
        let Some((user, Some(profile))) = self.app.state().profile.clone() else {
            return;
        };
        let Some(moderation) = profile.group_moderation else {
            return;
        };
        if moderation.transfer_ownership {
            self.act(
                ui,
                Action::TransferGroupOwnership {
                    room: moderation.room,
                    user,
                },
            );
            self.act(ui, Action::CloseProfile);
        }
    }

    /// A server menu entry, for the selected server.
    pub fn server_menu(&mut self, ui: &AppWindow, op: godwit_core::server_settings::ServerAdmin) {
        if let Some(server) = self.app.state().selected_server.clone() {
            self.act(ui, Action::ServerAdmin { server, op });
        }
    }

    pub fn copy_invite_link(&mut self) {
        if let Some((_, Some(link))) = &self.app.state().invite_link {
            copy_to_clipboard(link.to_string());
        }
    }

    pub fn delete_server(&mut self, ui: &AppWindow) {
        self.server_admin(ui, godwit_core::server_settings::ServerAdmin::DeleteServer);
    }
    pub fn server_image_picked(
        &mut self,
        ui: &AppWindow,
        token: &(ServerId, u64),
        icon: bool,
        result: Result<godwit_core::own_profile::ProfileImage, String>,
    ) {
        self.server_editor.picked(ui, token, icon, result);
        self.show_server_settings(ui);
    }
    pub fn server_emoji_file_picked(
        &mut self,
        ui: &AppWindow,
        token: &(ServerId, u64),
        result: Result<godwit_core::server_settings::EmojiFile, String>,
    ) {
        self.server_editor.emoji_picked(token, result);
        self.show_server_settings(ui);
    }
    pub fn server_emoji_name_edited(&mut self, ui: &AppWindow) {
        self.show_server_settings(ui);
    }
    pub fn create_server_emoji(&mut self, ui: &AppWindow) {
        if let Some(action) = self.server_editor.create_emoji(ui, self.app.state()) {
            self.act(ui, action);
        }
        self.show_server_settings(ui);
    }
    pub fn rename_server_emoji(&mut self, ui: &AppWindow, row: i32, name: &str) {
        if let Some(action) = self.server_editor.rename_emoji(self.app.state(), row, name) {
            self.act(ui, action);
        }
    }
    pub fn delete_server_emoji(&mut self, ui: &AppWindow, row: i32) {
        if let Some(action) = self.server_editor.delete_emoji(self.app.state(), row) {
            self.act(ui, action);
        }
    }
    pub fn save_channel_overview(&mut self, ui: &AppWindow) {
        if let Some(action) = self.channel_editor.save(ui) {
            self.act(ui, action);
        }
    }
    pub fn reset_channel_overview(&mut self, ui: &AppWindow) {
        self.channel_editor.reset(ui, self.app.state());
        self.show_channel_settings(ui);
    }
    pub fn channel_overview_edited(&mut self, ui: &AppWindow) {
        self.channel_editor.edited();
        self.show_channel_settings(ui);
    }
    pub fn remove_channel_icon(&mut self, ui: &AppWindow) {
        self.channel_editor.remove_icon(ui);
        self.show_channel_settings(ui);
    }
    pub fn show_channel_edit_page(&mut self, ui: &AppWindow, page: i32) {
        self.channel_editor.show_page(page);
        self.show_channel_settings(ui);
    }
    pub fn select_channel_override(&mut self, ui: &AppWindow, row: i32) {
        self.channel_editor.select_override(self.app.state(), row);
        self.show_channel_settings(ui);
    }
    pub fn set_channel_override_permission(&mut self, ui: &AppWindow, row: i32, value: i32) {
        self.channel_editor.set_permission(self.app.state(), row, value);
        self.show_channel_settings(ui);
    }
    pub fn save_channel_override(&mut self, ui: &AppWindow) {
        if let Some(action) = self.channel_editor.save_override(self.app.state()) {
            self.act(ui, action);
        }
    }
    pub fn remove_channel_override(&mut self, ui: &AppWindow) {
        if let Some(action) = self.channel_editor.remove_override(self.app.state()) {
            self.act(ui, action);
        }
    }
    pub fn add_channel_override(&mut self, ui: &AppWindow, choice: i32) {
        if let Some(action) = self.channel_editor.add_override(self.app.state(), choice) {
            self.act(ui, action);
        }
    }
    pub fn channel_edit_token(&self) -> Option<(RoomId, u64)> {
        self.channel_editor.token()
    }
    pub fn channel_image_picked(
        &mut self,
        ui: &AppWindow,
        token: &(RoomId, u64),
        result: Result<godwit_core::own_profile::ProfileImage, String>,
    ) {
        self.channel_editor.picked(ui, token, result);
        self.show_channel_settings(ui);
    }
    pub(super) fn show_channel_settings(&mut self, ui: &AppWindow) {
        self.channel_editor.show(ui, self.app.state(), &mut self.images);
    }
    pub(super) fn show_server_settings(&mut self, ui: &AppWindow) {
        self.server_editor.show(ui, self.app.state(), &mut self.images);
    }
}
