// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A server channel's settings: overview, mature flag and deleting it.

use super::*;
use crate::backend::ChannelEditCmd;
use godwit_core::channel_settings::{
    ChannelOverride, ChannelOverrides, ChannelOverview, OverrideChoice, OverrideTarget,
};

impl StoatAccount {
    /// A server channel whose settings are visible through Manage Channel or
    /// Manage Permissions. The edit and delete paths check Manage Channel again.
    fn channel_settings_channel(&self, room: &RoomId) -> Result<&Channel, &'static str> {
        if self.account.as_ref() != Some(&room.account) {
            return Err("This channel belongs to another account.");
        }
        let Some(c) = self.store.channels.get(room.raw.as_ref()) else {
            return Err("This channel is no longer available.");
        };
        if c.server.is_none() && c.channel_type != "Group" {
            return Err("Only server channels and groups have these settings.");
        }
        let can_open = if c.channel_type == "Group" {
            self.store.can(&c.id, crate::permissions::MANAGE_PERMISSIONS)
        } else {
            self.store.can(&c.id, crate::permissions::MANAGE_CHANNEL)
                || self.store.can(&c.id, crate::permissions::MANAGE_PERMISSIONS)
        };
        if !can_open {
            return Err(if c.channel_type == "Group" {
                "You need Manage Permissions for this group."
            } else {
                "You need Manage Channels or Manage Permissions for this."
            });
        }
        Ok(c)
    }

    /// A channel this account may edit or delete.
    fn managed_channel(&self, room: &RoomId) -> Result<&Channel, &'static str> {
        let c = self.channel_settings_channel(room)?;
        if c.server.is_none() {
            return Err("Group details can't be changed from this page.");
        }
        if !self.store.can(&c.id, crate::permissions::MANAGE_CHANNEL) {
            return Err("You need the Manage Channels permission for this.");
        }
        Ok(c)
    }

    fn managed_category(&self, room: &RoomId) -> Result<(String, crate::api::Category), &'static str> {
        if self.account.as_ref() != Some(&room.account) {
            return Err("This category belongs to another account.");
        }
        let Some((server, category)) = self.store.servers.iter().find_map(|(sid, server)| {
            server
                .categories
                .as_ref()?
                .iter()
                .find(|c| c.id == room.raw.as_ref())
                .map(|c| (sid.clone(), c.clone()))
        }) else {
            return Err("This category is no longer available.");
        };
        if !self.store.can_in_server(&server, crate::permissions::MANAGE_CHANNEL) {
            return Err("You need the Manage Channels permission for this.");
        }
        Ok((server, category))
    }

    /// Values for the official ChannelPermissionsEditor, with @everyone
    /// first and (for server channels) role rows in server role priority order.
    fn channel_overrides(&self, channel: &Channel) -> Option<ChannelOverrides> {
        let is_group = channel.channel_type == "Group";
        if is_group {
            let permissions = crate::permissions::group_list();
            let allow = channel
                .permissions
                .map_or(crate::permissions::DEFAULT_DIRECT_MESSAGE, |p| p as u64);
            let grantable = self.store.channel_permissions(&channel.id);
            return Some(ChannelOverrides {
                entries: vec![ChannelOverride {
                    target: OverrideTarget {
                        id: "default".into(),
                        member: false,
                    },
                    name: "Everyone".into(),
                    colour: None,
                    everyone: true,
                    allow,
                    deny: 0,
                }],
                permissions,
                choices: Vec::new(),
                can_edit: self.store.can(&channel.id, crate::permissions::MANAGE_PERMISSIONS),
                supports_deny: false,
                grantable,
            });
        }
        let server_id = channel.server.as_deref()?;
        let server = self.store.servers.get(server_id)?;
        let permissions = crate::permissions::channel_list();
        let default = channel.default_permissions.unwrap_or_default();
        let mut entries = vec![ChannelOverride {
            target: OverrideTarget {
                id: "default".into(),
                member: false,
            },
            name: "Everyone".into(),
            colour: None,
            everyone: true,
            allow: u64::try_from(default.a).unwrap_or(0),
            deny: u64::try_from(default.d).unwrap_or(0),
        }];
        let mut roles: Vec<_> = server.roles.iter().collect();
        roles.sort_by(|a, b| a.1.rank.cmp(&b.1.rank).then_with(|| a.0.cmp(b.0)));
        let mut choices = Vec::new();
        for (id, role) in roles {
            let target = OverrideTarget {
                id: id.as_str().into(),
                member: false,
            };
            if let Some(value) = channel.role_permissions.get(id) {
                entries.push(ChannelOverride {
                    target,
                    name: role.name.as_str().into(),
                    colour: role.colour.as_deref().map(Into::into),
                    everyone: false,
                    allow: u64::try_from(value.a).unwrap_or(0),
                    deny: u64::try_from(value.d).unwrap_or(0),
                });
            } else {
                choices.push(OverrideChoice {
                    target,
                    name: role.name.as_str().into(),
                });
            }
        }
        let can_edit = self.store.can(&channel.id, crate::permissions::MANAGE_PERMISSIONS);
        let grantable = permissions
            .iter()
            .filter(|p| self.store.can(&channel.id, p.bit))
            .fold(0, |bits, p| bits | p.bit);
        Some(ChannelOverrides {
            entries,
            permissions,
            choices,
            can_edit,
            supports_deny: true,
            grantable,
        })
    }

    /// A channel as the Overview tab shows it; `None` once it is gone.
    pub(super) fn channel_overview(&self, room: &RoomId) -> Result<ChannelOverview, &'static str> {
        if let Ok((_server, category)) = self.managed_category(room) {
            return Ok(ChannelOverview {
                name: category.title.as_str().into(),
                can_manage: true,
                is_category: true,
                ..Default::default()
            });
        }
        let c = self.channel_settings_channel(room)?;
        let overrides = self.channel_overrides(c);
        Ok(ChannelOverview {
            name: c.name.as_deref().unwrap_or_default().into(),
            description: c.description.as_deref().unwrap_or_default().into(),
            icon_url: c.icon.as_ref().and_then(|f| self.file_url(f)).map(Into::into),
            mature: c.nsfw,
            can_manage: c.server.is_some() && self.store.can(&c.id, crate::permissions::MANAGE_CHANNEL),
            is_category: false,
            is_group: c.channel_type == "Group",
            icons: true,
            // Stoat has no slowmode.
            slowmode: None,
            description_max: 0,
            overrides,
        })
    }

    pub(super) fn channel_command(&mut self, command: Command) -> Vec<Event> {
        match command {
            Command::LoadChannelOverview { room, request } => {
                let result = self.channel_overview(&room).map(Some).map_err(Into::into);
                vec![Event::ChannelOverview { room, request, result }]
            }
            Command::SaveChannelOverview { room, request, edit } => {
                let failed = |room: RoomId, e: &str| {
                    vec![Event::ChannelOverview {
                        room,
                        request,
                        result: Err(e.into()),
                    }]
                };
                if let Ok((server, category)) = self.managed_category(&room) {
                    if edit.description.is_some()
                        || edit.mature.is_some()
                        || edit.slowmode.is_some()
                        || edit.icon != godwit_core::own_profile::ProfileImage::Keep
                    {
                        return failed(room, "Categories only have a name setting.");
                    }
                    let title = edit
                        .name
                        .as_deref()
                        .unwrap_or(category.title.as_str())
                        .trim()
                        .to_owned();
                    if title.is_empty() || title.chars().count() > 32 {
                        return failed(room, "A category name is 1 to 32 characters.");
                    }
                    let Some(body) = self.store.edit_categories(&server, |cats| {
                        if let Some(c) = cats.iter_mut().find(|c| c.id == room.raw.as_ref()) {
                            c.title = title.clone();
                        }
                    }) else {
                        return failed(room, "This category is no longer available.");
                    };
                    self.net.send(Cmd::Server(ServerCmd::EditCategories {
                        server,
                        body: serde_json::json!({"categories": body}),
                    }));
                    self.category_pending = Some((room.clone(), request, Some(title.into())));
                    return Vec::new();
                }
                if let Err(e) = self.managed_channel(&room) {
                    return failed(room, e);
                }
                let limits = self
                    .config
                    .as_ref()
                    .and_then(|c| c.features.limits.as_ref())
                    .map(|l| l.default.file_upload_size_limits.clone())
                    .unwrap_or_default();
                match crate::channel_edit::prepare(*edit, &limits) {
                    Ok(p) => {
                        self.net
                            .send(Cmd::Channel(ChannelCmd::Edit(ChannelEditCmd::EditChannel {
                                channel: room.raw.to_string(),
                                request,
                                body: p.body,
                                icon: p.icon,
                            })));
                        Vec::new()
                    }
                    Err(e) => failed(room, &e),
                }
            }
            Command::DeleteChannel { room, request } => {
                if let Ok((server, _category)) = self.managed_category(&room) {
                    let Some(body) = self
                        .store
                        .edit_categories(&server, |cats| cats.retain(|c| c.id != room.raw.as_ref()))
                    else {
                        return vec![Event::ChannelOverview {
                            room,
                            request,
                            result: Err("This category is no longer available.".into()),
                        }];
                    };
                    self.net.send(Cmd::Server(ServerCmd::EditCategories {
                        server,
                        body: serde_json::json!({"categories": body}),
                    }));
                    self.category_pending = Some((room.clone(), request, None));
                    return Vec::new();
                }
                if let Err(e) = self.managed_channel(&room) {
                    return vec![Event::ChannelOverview {
                        room,
                        request,
                        result: Err(e.into()),
                    }];
                }
                self.net.send(Cmd::Channel(ChannelCmd::DeleteChannel {
                    channel: room.raw.to_string(),
                    request,
                }));
                Vec::new()
            }
            Command::SetChannelOverride {
                room,
                request,
                target,
                change,
            } => {
                let failed = |error: &str| {
                    vec![Event::ChannelOverview {
                        room: room.clone(),
                        request,
                        result: Err(error.into()),
                    }]
                };
                let Ok(c) = self.channel_settings_channel(&room) else {
                    return failed("You no longer have permission to edit this channel's permissions.");
                };
                if !self.store.can(&c.id, crate::permissions::MANAGE_PERMISSIONS) {
                    return failed("You need Manage Permissions for this channel.");
                }
                if target.member {
                    return failed("Member-specific channel permissions aren't supported by Stoat.");
                }
                let Some(overrides) = self.channel_overrides(c) else {
                    return failed("This channel's permission settings are no longer available.");
                };
                let role = if target.id.as_ref() == "default" {
                    if change.is_none() {
                        return failed("The Everyone permissions can't be removed.");
                    }
                    None
                } else if let Some(entry) = overrides.entries.iter().find(|entry| entry.target == target) {
                    if entry.everyone {
                        return failed("That role can't be changed here.");
                    }
                    Some(target.id.to_string())
                } else if overrides.choices.iter().any(|choice| choice.target == target) {
                    // Adding a new role override starts with an empty value.
                    if change != Some((0, 0)) {
                        return failed("Add a role override before changing its permissions.");
                    }
                    Some(target.id.to_string())
                } else {
                    return failed("That role is no longer available in this channel.");
                };
                let (allow, deny) = change.unwrap_or((0, 0));
                if change.is_some() && !overrides.may_set(&target, allow, deny) {
                    return failed("You can't grant or deny those permissions here.");
                }
                let (Ok(allow), Ok(deny)) = (i64::try_from(allow), i64::try_from(deny)) else {
                    return failed("Those permission values aren't supported by Stoat.");
                };
                let permissions = if c.channel_type == "Group" {
                    if role.is_some() || deny != 0 {
                        return failed("Group permissions only support a default allow bitset.");
                    }
                    serde_json::json!(allow)
                } else {
                    serde_json::json!({ "a": allow, "d": deny })
                };
                self.net
                    .send(Cmd::Channel(ChannelCmd::Edit(ChannelEditCmd::SetChannelPermissions {
                        channel: room.raw.to_string(),
                        request,
                        role,
                        permissions,
                    })));
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The server's answer to a channel edit or delete.
    pub(super) fn channel_answer(&mut self, ev: Ev, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else { return };
        match ev {
            Ev::ChannelSaved {
                channel,
                request,
                result,
            } => {
                let room = RoomId::new(account, channel.as_str());
                let result = match result {
                    Ok(Some(updated)) if updated.id == channel => {
                        // Applied now; the ChannelUpdate event that follows repeats it.
                        if let Some(c) = self.store.channels.get_mut(&channel) {
                            c.name = updated.name;
                            c.description = updated.description;
                            c.icon = updated.icon;
                            c.nsfw = updated.nsfw;
                            c.default_permissions = updated.default_permissions;
                            c.role_permissions = updated.role_permissions;
                            c.permissions = updated.permissions;
                        }
                        self.sync_rooms(out);
                        self.channel_overview(&room).map(Some).map_err(Into::into)
                    }
                    Ok(_) => Err("The server returned an unexpected response.".into()),
                    Err(e) => Err(e.into()),
                };
                out.push(Event::ChannelOverview { room, request, result });
            }
            Ev::ChannelDeleted {
                channel,
                request,
                result,
            } => out.push(Event::ChannelOverview {
                room: RoomId::new(account, channel.as_str()),
                request,
                // The ChannelDelete event removes it from the lists.
                result: result.map(|()| None).map_err(Into::into),
            }),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{connected, room};
    use super::*;
    use godwit_core::channel_settings::{ChannelOverviewEdit, OverrideTarget};

    #[test]
    fn channel_permissions_show_everyone_roles_and_addable_role_choices() {
        let (mut a, _tx, _cmds, _) = connected();
        let role = crate::api::Role {
            name: "Moderators".into(),
            rank: 1,
            ..Default::default()
        };
        if let Some(server) = a.store.servers.get_mut("S") {
            server.roles.insert("R1".into(), role);
        }
        if let Some(channel) = a.store.channels.get_mut("C1") {
            channel.default_permissions = Some(crate::api::Override { a: 1 << 22, d: 0 });
            channel
                .role_permissions
                .insert("R1".into(), crate::api::Override { a: 1 << 23, d: 1 << 22 });
        }
        let overview = a
            .channel_overview(&room("C1"))
            .expect("owner can view channel settings");
        let overrides = overview
            .overrides
            .expect("Stoat server channels support permission overrides");
        assert_eq!(overrides.entries.len(), 2);
        assert!(overrides.entries[0].everyone);
        assert_eq!(&*overrides.entries[0].name, "Everyone");
        assert_eq!(overrides.entries[0].allow, 1 << 22);
        assert_eq!(&*overrides.entries[1].name, "Moderators");
        assert_eq!(overrides.entries[1].allow, 1 << 23);
        assert_eq!(overrides.entries[1].deny, 1 << 22);
        assert!(overrides.choices.is_empty());
        assert!(overrides.can_edit);
    }

    #[test]
    fn group_permissions_show_binary_everyone_bits_and_save_the_group_bitset() {
        let (mut a, _tx, mut cmds, _) = connected();
        let group: Channel = serde_json::from_value(serde_json::json!({
            "_id": "G1",
            "channel_type": "Group",
            "owner": "ME",
            "permissions": crate::permissions::SEND_MESSAGE | crate::permissions::CONNECT
        }))
        .expect("a group channel");
        a.store.channels.insert("G1".into(), group);
        while cmds.try_recv().is_ok() {}
        let group_room = a.room(&room("G1").account, a.store.channels.get("G1").expect("group"));
        assert!(group_room.capabilities.manage_channel);

        let loaded = a.run_command(Command::LoadChannelOverview {
            room: room("G1"),
            request: 1,
        });
        let [
            Event::ChannelOverview {
                result: Ok(Some(overview)),
                ..
            },
        ] = loaded.as_slice()
        else {
            panic!("group owner should be able to load group permissions");
        };
        assert!(overview.is_group);
        assert!(!overview.can_manage, "group controls are permission-only here");
        let overrides = overview.overrides.as_ref().expect("group permissions");
        assert!(!overrides.supports_deny);
        assert!(overrides.choices.is_empty());
        assert_eq!(overrides.entries.len(), 1);
        assert_eq!(
            overrides.entries[0].allow,
            crate::permissions::SEND_MESSAGE | crate::permissions::CONNECT
        );

        let target = OverrideTarget {
            id: "default".into(),
            member: false,
        };
        assert!(
            a.run_command(Command::SetChannelOverride {
                room: room("G1"),
                request: 2,
                target: target.clone(),
                change: Some((
                    crate::permissions::SEND_MESSAGE | crate::permissions::CONNECT | crate::permissions::UPLOAD_FILES,
                    0,
                )),
            })
            .is_empty()
        );
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Channel(ChannelCmd::Edit(ChannelEditCmd::SetChannelPermissions {
                channel,
                request: 2,
                role: None,
                permissions,
            }))) if channel == "G1" && permissions == serde_json::json!(
                crate::permissions::SEND_MESSAGE | crate::permissions::CONNECT | crate::permissions::UPLOAD_FILES
            )
        ));

        let updated: Channel = serde_json::from_value(serde_json::json!({
            "_id": "G1",
            "channel_type": "Group",
            "owner": "ME",
            "permissions": crate::permissions::SEND_MESSAGE | crate::permissions::CONNECT | crate::permissions::UPLOAD_FILES
        }))
        .expect("updated group");
        let mut events = Vec::new();
        a.channel_answer(
            Ev::ChannelSaved {
                channel: "G1".into(),
                request: 2,
                result: Ok(Some(updated)),
            },
            &mut events,
        );
        assert_eq!(
            a.store.channels.get("G1").and_then(|c| c.permissions),
            Some(
                (crate::permissions::SEND_MESSAGE | crate::permissions::CONNECT | crate::permissions::UPLOAD_FILES)
                    as i64
            )
        );
        assert!(events.iter().any(|event| matches!(
            event,
            Event::ChannelOverview { result: Ok(Some(overview)), .. }
                if overview.is_group
                    && overview.overrides.as_ref().is_some_and(|o| {
                        o.entries[0].allow
                            == (crate::permissions::SEND_MESSAGE
                                | crate::permissions::CONNECT
                                | crate::permissions::UPLOAD_FILES)
                    })
        )));

        let denied = a.run_command(Command::SetChannelOverride {
            room: room("G1"),
            request: 3,
            target,
            change: Some((crate::permissions::SEND_MESSAGE, crate::permissions::CONNECT)),
        });
        assert!(matches!(
            denied.as_slice(),
            [Event::ChannelOverview { result: Err(_), .. }]
        ));
        assert!(cmds.try_recv().is_err());
    }

    #[test]
    fn a_group_member_with_manage_permissions_can_edit_the_default_set() {
        let (mut a, _tx, mut cmds, _) = connected();
        let initial = crate::permissions::MANAGE_PERMISSIONS | crate::permissions::SEND_MESSAGE;
        let group: Channel = serde_json::from_value(serde_json::json!({
            "_id": "G2",
            "channel_type": "Group",
            "owner": "ANN",
            "permissions": initial
        }))
        .expect("a group channel");
        a.store.channels.insert("G2".into(), group);
        while cmds.try_recv().is_ok() {}

        let room_id = room("G2");
        let room = a.room(&room_id.account, a.store.channels.get("G2").expect("group"));
        assert!(room.capabilities.manage_channel);
        let loaded = a.run_command(Command::LoadChannelOverview {
            room: room_id.clone(),
            request: 1,
        });
        assert!(matches!(
            loaded.as_slice(),
            [Event::ChannelOverview { result: Ok(Some(o)), .. }]
                if o.overrides.as_ref().is_some_and(|o| o.can_edit)
        ));
        assert!(
            a.run_command(Command::SetChannelOverride {
                room: room_id,
                request: 2,
                target: OverrideTarget {
                    id: "default".into(),
                    member: false,
                },
                change: Some((0, 0)),
            })
            .is_empty()
        );
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Channel(ChannelCmd::Edit(ChannelEditCmd::SetChannelPermissions {
                role: None,
                permissions,
                ..
            }))) if permissions == serde_json::json!(0)
        ));
    }

    #[test]
    fn channel_permission_changes_use_stoats_default_and_role_routes() {
        let (mut a, _tx, mut cmds, _) = connected();
        if let Some(server) = a.store.servers.get_mut("S") {
            server.owner = "ANN".into();
            server.default_permissions = (crate::permissions::VIEW_CHANNEL
                | crate::permissions::MANAGE_PERMISSIONS
                | crate::permissions::SEND_MESSAGE
                | crate::permissions::MANAGE_MESSAGES) as i64;
        }
        while cmds.try_recv().is_ok() {}
        let loaded = a.run_command(Command::LoadChannelOverview {
            room: room("C1"),
            request: 9,
        });
        assert!(
            matches!(loaded.as_slice(), [Event::ChannelOverview { result: Ok(Some(o)), .. }]
            if !o.can_manage && o.overrides.as_ref().is_some_and(|o| o.can_edit))
        );
        let default = OverrideTarget {
            id: "default".into(),
            member: false,
        };
        assert!(
            a.run_command(Command::SetChannelOverride {
                room: room("C1"),
                request: 10,
                target: default,
                change: Some((1 << 22, 0)),
            })
            .is_empty()
        );
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Channel(ChannelCmd::Edit(ChannelEditCmd::SetChannelPermissions {
            channel, request: 10, role: None, permissions,
        }))) if channel == "C1" && permissions == serde_json::json!({"a": 1 << 22, "d": 0}))
        );

        if let Some(server) = a.store.servers.get_mut("S") {
            server.roles.insert(
                "R1".into(),
                crate::api::Role {
                    name: "Moderators".into(),
                    rank: 1,
                    ..Default::default()
                },
            );
        }
        if let Some(channel) = a.store.channels.get_mut("C1") {
            channel
                .role_permissions
                .insert("R1".into(), crate::api::Override { a: 1 << 23, d: 0 });
        }
        let target = OverrideTarget {
            id: "R1".into(),
            member: false,
        };
        assert!(
            a.run_command(Command::SetChannelOverride {
                room: room("C1"),
                request: 11,
                target,
                change: Some((1 << 23, 1 << 22)),
            })
            .is_empty()
        );
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Channel(ChannelCmd::Edit(ChannelEditCmd::SetChannelPermissions {
            channel, request: 11, role: Some(role), permissions,
        }))) if channel == "C1" && role == "R1" && permissions == serde_json::json!({"a": 1 << 23, "d": 1 << 22}))
        );
    }

    #[test]
    fn overview_loads_and_saves_go_to_the_network() {
        let (mut a, _tx, mut cmds, _events) = connected();
        while cmds.try_recv().is_ok() {}
        let out = a.run_command(Command::LoadChannelOverview {
            room: room("C1"),
            request: 1,
        });
        assert!(matches!(
            out.as_slice(),
            [Event::ChannelOverview { result: Ok(Some(o)), .. }] if &*o.name == "general" && !o.mature && o.can_manage
        ));
        let out = a.run_command(Command::SaveChannelOverview {
            room: room("C1"),
            request: 2,
            edit: Box::new(ChannelOverviewEdit {
                name: Some("lobby".into()),
                mature: Some(true),
                ..Default::default()
            }),
        });
        assert!(out.is_empty());
        let Ok(Cmd::Channel(ChannelCmd::Edit(ChannelEditCmd::EditChannel {
            channel, request, body, ..
        }))) = cmds.try_recv()
        else {
            panic!("expected an edit");
        };
        assert_eq!((channel.as_str(), request), ("C1", 2));
        assert_eq!(body, serde_json::json!({"name": "lobby", "nsfw": true}));
        // An invalid name never reaches the network.
        let out = a.run_command(Command::SaveChannelOverview {
            room: room("C1"),
            request: 3,
            edit: Box::new(ChannelOverviewEdit {
                name: Some(" ".into()),
                ..Default::default()
            }),
        });
        assert!(matches!(
            out.as_slice(),
            [Event::ChannelOverview { result: Err(_), .. }]
        ));
        assert!(cmds.try_recv().is_err());
    }

    #[test]
    fn only_server_channels_can_be_managed_or_deleted() {
        let (mut a, _tx, mut cmds, _events) = connected();
        while cmds.try_recv().is_ok() {}
        // A DM and saved notes have no channel settings here.
        for raw in ["D", "N", "MISSING"] {
            let out = a.run_command(Command::DeleteChannel {
                room: room(raw),
                request: 1,
            });
            assert!(matches!(
                out.as_slice(),
                [Event::ChannelOverview { result: Err(_), .. }]
            ));
        }
        assert!(cmds.try_recv().is_err());
        assert!(
            a.run_command(Command::DeleteChannel {
                room: room("C2"),
                request: 4
            })
            .is_empty()
        );
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Channel(ChannelCmd::DeleteChannel { request: 4, .. }))
        ));
    }

    #[test]
    fn answers_update_the_store_and_a_delete_closes_the_page() {
        let (mut a, _tx, _cmds, _events) = connected();
        let updated: Channel = serde_json::from_value(serde_json::json!({
            "_id": "C1", "channel_type": "TextChannel", "server": "S",
            "name": "lobby", "description": "Hello", "nsfw": true
        }))
        .unwrap();
        let mut out = Vec::new();
        a.channel_answer(
            Ev::ChannelSaved {
                channel: "C1".into(),
                request: 7,
                result: Ok(Some(updated)),
            },
            &mut out,
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::RoomUpdated(r) if &*r.name == "lobby"))
        );
        assert!(out.iter().any(|e| matches!(
            e,
            Event::ChannelOverview { request: 7, result: Ok(Some(o)), .. }
                if &*o.description == "Hello" && o.mature
        )));
        let mut out = Vec::new();
        a.channel_answer(
            Ev::ChannelDeleted {
                channel: "C1".into(),
                request: 8,
                result: Ok(()),
            },
            &mut out,
        );
        assert!(matches!(
            out.as_slice(),
            [Event::ChannelOverview {
                request: 8,
                result: Ok(None),
                ..
            }]
        ));
    }
}
