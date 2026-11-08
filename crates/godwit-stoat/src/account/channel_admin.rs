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
        if c.server.is_none() {
            return Err("Only server channels have these settings.");
        }
        if !self.store.can(&c.id, crate::permissions::MANAGE_CHANNEL)
            && !self.store.can(&c.id, crate::permissions::MANAGE_PERMISSIONS)
        {
            return Err("You need Manage Channels or Manage Permissions for this.");
        }
        Ok(c)
    }

    /// A channel this account may edit or delete.
    fn managed_channel(&self, room: &RoomId) -> Result<&Channel, &'static str> {
        let c = self.channel_settings_channel(room)?;
        if !self.store.can(&c.id, crate::permissions::MANAGE_CHANNEL) {
            return Err("You need the Manage Channels permission for this.");
        }
        Ok(c)
    }

    /// Values for the official ChannelPermissionsEditor, with @everyone
    /// first and role rows in the server's role priority order.
    fn channel_overrides(&self, channel: &Channel) -> Option<ChannelOverrides> {
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
            grantable,
        })
    }

    /// A channel as the Overview tab shows it; `None` once it is gone.
    pub(super) fn channel_overview(&self, room: &RoomId) -> Result<ChannelOverview, &'static str> {
        let c = self.channel_settings_channel(room)?;
        let overrides = self.channel_overrides(c);
        Ok(ChannelOverview {
            name: c.name.as_deref().unwrap_or_default().into(),
            description: c.description.as_deref().unwrap_or_default().into(),
            icon_url: c.icon.as_ref().and_then(|f| self.file_url(f)).map(Into::into),
            mature: c.nsfw,
            can_manage: self.store.can(&c.id, crate::permissions::MANAGE_CHANNEL),
            is_category: false,
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
                self.net
                    .send(Cmd::Channel(ChannelCmd::Edit(ChannelEditCmd::SetChannelPermissions {
                        channel: room.raw.to_string(),
                        request,
                        role,
                        permissions: serde_json::json!({ "a": allow, "d": deny }),
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
