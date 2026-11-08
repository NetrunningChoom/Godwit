// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A server channel's settings: overview, mature flag and deleting it.

use super::*;
use crate::backend::ChannelEditCmd;
use godwit_core::channel_settings::ChannelOverview;

impl StoatAccount {
    /// The channel if it's one of this account's server channels we may
    /// manage (the official settings need Manage Channel; groups have their
    /// own page there, which isn't offered here).
    fn managed_channel(&self, room: &RoomId) -> Result<&Channel, &'static str> {
        if self.account.as_ref() != Some(&room.account) {
            return Err("This channel belongs to another account.");
        }
        let Some(c) = self.store.channels.get(room.raw.as_ref()) else {
            return Err("This channel is no longer available.");
        };
        if c.server.is_none() || !self.store.can(&c.id, crate::permissions::MANAGE_CHANNEL) {
            return Err("You need the Manage Channels permission for this.");
        }
        Ok(c)
    }

    /// A channel as the Overview tab shows it; `None` once it is gone.
    pub(super) fn channel_overview(&self, room: &RoomId) -> Result<ChannelOverview, &'static str> {
        let c = self.managed_channel(room)?;
        Ok(ChannelOverview {
            name: c.name.as_deref().unwrap_or_default().into(),
            description: c.description.as_deref().unwrap_or_default().into(),
            icon_url: c.icon.as_ref().and_then(|f| self.file_url(f)).map(Into::into),
            mature: c.nsfw,
            can_manage: true,
            is_category: false,
            icons: true,
            // Stoat has no slowmode.
            slowmode: None,
            description_max: 0,
            overrides: None,
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
    use godwit_core::channel_settings::ChannelOverviewEdit;

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
