// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A server's settings: roles, invites, overview and admin commands.

use super::*;

impl StoatAccount {
    /// Server settings' pages and member moderation. Permissions are checked
    /// here first, so a stale button can't send a request the server would
    /// refuse; the server still decides.
    pub(super) fn server_admin(&mut self, server: ServerId, op: ServerAdmin) -> Vec<Event> {
        use crate::permissions as p;
        let fail = |server: ServerId, e: &str| {
            vec![Event::ServerAdmin {
                server,
                result: Err(e.into()),
            }]
        };
        if self.account.as_ref() != Some(&server.account) {
            return fail(server, "This server belongs to another account.");
        }
        let raw = server.raw.to_string();
        let Some(owner) = self.store.servers.get(&raw).map(|s| s.owner.clone()) else {
            return fail(server, "This server is no longer available.");
        };
        let needed = match &op {
            ServerAdmin::LoadInvites | ServerAdmin::DeleteInvite(_) => Some((p::MANAGE_SERVER, "Manage Server")),
            ServerAdmin::LoadBans | ServerAdmin::Unban(_) | ServerAdmin::Ban { .. } => {
                Some((p::BAN_MEMBERS, "Ban Members"))
            }
            ServerAdmin::Kick(_) => Some((p::KICK_MEMBERS, "Kick Members")),
            ServerAdmin::CreateRole(_)
            | ServerAdmin::EditRole(_)
            | ServerAdmin::DeleteRole(_)
            | ServerAdmin::SetDefaultPermissions(_) => Some((p::MANAGE_ROLE, "Manage Roles")),
            // for-web gates the Emoji page on Manage Customisation.
            ServerAdmin::LoadEmoji
            | ServerAdmin::CreateEmoji { .. }
            | ServerAdmin::RenameEmoji { .. }
            | ServerAdmin::DeleteEmoji(_) => Some((p::MANAGE_CUSTOMISATION, "Manage Customisation")),
            ServerAdmin::CreateInvite => Some((p::INVITE_OTHERS, "Invite Others")),
            ServerAdmin::CreateChannel { .. } => Some((p::MANAGE_CHANNEL, "Manage Channels")),
            // The card never offers these here (no capability is set).
            ServerAdmin::SetMemberRole { .. } | ServerAdmin::Timeout { .. } | ServerAdmin::SetServerNickname { .. } => {
                None
            }
            ServerAdmin::LoadRoles
            | ServerAdmin::DeleteServer
            | ServerAdmin::MarkRead
            | ServerAdmin::SetNotify(_)
            | ServerAdmin::Leave => None,
        };
        if let Some((bit, name)) = needed
            && !self.store.can_in_server(&raw, bit)
        {
            return fail(server, &format!("You need the {name} permission for this."));
        }
        let cmd = match op {
            ServerAdmin::SetMemberRole { .. } | ServerAdmin::Timeout { .. } | ServerAdmin::SetServerNickname { .. } => {
                return fail(server, "This isn't available here yet.");
            }
            ServerAdmin::LoadInvites => Cmd::Server(ServerCmd::FetchServerInvites { server: raw }),
            ServerAdmin::DeleteInvite(code) => Cmd::Server(ServerCmd::DeleteInvite {
                server: raw,
                code: code.to_string(),
            }),
            ServerAdmin::LoadBans => Cmd::Server(ServerCmd::FetchBans { server: raw }),
            ServerAdmin::Unban(user) => Cmd::Server(ServerCmd::Unban {
                server: raw,
                user: user.raw.to_string(),
            }),
            ServerAdmin::Kick(user) => Cmd::Member(MemberCmd::KickMember {
                server: raw,
                user: user.raw.to_string(),
            }),
            ServerAdmin::Ban {
                user,
                reason,
                delete_seconds,
            } => {
                // DataBanCreate: a reason of at most 1024 characters.
                if reason.chars().count() > 1024 {
                    return fail(server, "The reason can be at most 1024 characters.");
                }
                Cmd::Member(MemberCmd::BanUser {
                    server: raw,
                    user: user.raw.to_string(),
                    reason: reason.trim().to_owned(),
                    delete_message_seconds: delete_seconds.into(),
                })
            }
            ServerAdmin::LoadRoles => {
                // Roles come with the server: nothing to fetch.
                let roles = self.server_roles(&raw);
                self.roles_watch = Some((raw, roles.clone()));
                return vec![Event::ServerAdmin {
                    server,
                    result: Ok(ServerAdminResult::Roles(roles)),
                }];
            }
            ServerAdmin::CreateRole(name) => {
                let name = name.trim();
                if let Err(e) = role_name(name) {
                    return fail(server, e);
                }
                Cmd::Role(RoleCmd::CreateRole {
                    server: raw,
                    name: name.to_owned(),
                })
            }
            ServerAdmin::EditRole(edit) => {
                let mut body = serde_json::Map::new();
                if let Some(name) = &edit.name {
                    if let Err(e) = role_name(name.trim()) {
                        return fail(server, e);
                    }
                    body.insert("name".into(), serde_json::json!(name.trim()));
                }
                match &edit.colour {
                    Some(Some(c)) => {
                        body.insert("colour".into(), serde_json::json!(c.as_ref()));
                    }
                    // DataEditRole: removing is `remove: ["Colour"]`.
                    Some(None) => {
                        body.insert("remove".into(), serde_json::json!(["Colour"]));
                    }
                    None => {}
                }
                if let Some(hoist) = edit.hoist {
                    body.insert("hoist".into(), serde_json::json!(hoist));
                }
                Cmd::Role(RoleCmd::Edit(RoleEditCmd::EditRole {
                    server: raw,
                    role: edit.id.to_string(),
                    body: Value::Object(body),
                    icon: None,
                    permissions: edit.permissions,
                }))
            }
            ServerAdmin::DeleteRole(role) => Cmd::Role(RoleCmd::Edit(RoleEditCmd::DeleteRole {
                server: raw,
                role: role.to_string(),
            })),
            ServerAdmin::SetDefaultPermissions(permissions) => {
                Cmd::Role(RoleCmd::Edit(RoleEditCmd::SetDefaultPermissions {
                    server: raw,
                    permissions,
                }))
            }
            ServerAdmin::LoadEmoji => {
                // Emoji come with Ready and the EmojiCreate/Delete events: nothing to fetch.
                let emoji = self.server_emojis(&raw);
                self.emoji_watch = Some((raw, emoji.clone()));
                return vec![Event::ServerAdmin {
                    server,
                    result: Ok(ServerAdminResult::Emoji(emoji)),
                }];
            }
            ServerAdmin::CreateEmoji { name, file } => {
                let name = name.trim();
                let current = self.server_emojis(&raw);
                if let Err(e) = current.check_new(name, file.size) {
                    return fail(server, &e);
                }
                self.emoji_pending = Some(EmojiChange::Create { server: raw.clone() });
                Cmd::Emoji(EmojiCmd::CreateEmoji {
                    server: raw,
                    name: name.to_owned(),
                    file: Upload {
                        name: file.path.file_name().and_then(|s| s.to_str()).unwrap_or("emoji").into(),
                        size: file.size,
                        source: UploadSource::Path(file.path),
                    },
                })
            }
            ServerAdmin::RenameEmoji { id, name } => {
                let name = name.trim();
                if let Err(e) = self.server_emojis(&raw).rules.validate_name(name) {
                    return fail(server, &e);
                }
                if !self
                    .store
                    .emojis
                    .get(id.as_ref())
                    .is_some_and(|e| e.server() == Some(raw.as_str()))
                {
                    return fail(server, "That emoji is no longer here.");
                }
                self.emoji_pending = Some(EmojiChange::Rename {
                    server: raw,
                    id: id.to_string(),
                    name: name.to_owned(),
                });
                Cmd::Emoji(EmojiCmd::RenameEmoji {
                    id: id.to_string(),
                    name: name.to_owned(),
                })
            }
            ServerAdmin::DeleteEmoji(id) => {
                if !self
                    .store
                    .emojis
                    .get(id.as_ref())
                    .is_some_and(|e| e.server() == Some(raw.as_str()))
                {
                    return fail(server, "That emoji is no longer here.");
                }
                self.emoji_pending = Some(EmojiChange::Delete {
                    server: raw,
                    id: id.to_string(),
                });
                Cmd::Emoji(EmojiCmd::DeleteEmoji { id: id.to_string() })
            }
            ServerAdmin::DeleteServer => {
                if owner != *server.account.user {
                    return fail(server, "Only the owner can delete this server.");
                }
                Cmd::Server(ServerCmd::DeleteServer { server: raw })
            }
            ServerAdmin::CreateInvite => {
                // The open channel if it's this server's, else its first text
                // channel we may invite to (invites point at a channel).
                let channels = self
                    .store
                    .servers
                    .get(&raw)
                    .map(|s| s.channels.clone())
                    .unwrap_or_default();
                let invitable = |id: &String| {
                    self.store
                        .channels
                        .get(id)
                        .is_some_and(|c| c.channel_type == "TextChannel")
                        && self.store.can(id, p::INVITE_OTHERS)
                };
                let channel = self
                    .viewing
                    .clone()
                    .filter(|v| channels.contains(v) && invitable(v))
                    .or_else(|| channels.iter().find(|c| invitable(c)).cloned());
                let Some(channel) = channel else {
                    return fail(server, "There's no channel here you may invite people to.");
                };
                self.invite_for = Some(raw);
                Cmd::Server(ServerCmd::CreateInvite { channel })
            }
            // The server menu's levels, as the official client's server
            // context menu: all, mention or none, synced (`notifications`).
            // A level chosen also lifts a server mute, so "all" is heard.
            ServerAdmin::SetNotify(level) => {
                let state = match level {
                    0 => "all",
                    2 => "none",
                    _ => "mention",
                };
                let _ = self.store.set_notification("server_mutes", &raw, None);
                let Some(value) = self
                    .store
                    .set_notification("server", &raw, Some(serde_json::json!(state)))
                else {
                    return fail(server, "Your Stoat settings haven't loaded yet; try again in a moment.");
                };
                self.net.send(Cmd::Settings(SettingsCmd::SetSyncedSetting {
                    key: "notifications".into(),
                    value,
                }));
                let mut out = Vec::new();
                self.sync_servers(&mut out);
                return out;
            }
            ServerAdmin::MarkRead => {
                // Marked here at once, then one request for the whole server.
                let marks: Vec<(String, String)> = self
                    .store
                    .servers
                    .get(&raw)
                    .map(|s| s.channels.clone())
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|c| {
                        let last = self.store.channels.get(&c)?.last_message_id.clone()?;
                        Some((c, last))
                    })
                    .collect();
                for (channel, last) in marks {
                    self.store.mark_read(&channel, &last);
                }
                self.net.send(Cmd::Message(MessageCmd::AckServer { server: raw }));
                let mut out = Vec::new();
                self.sync_rooms(&mut out);
                return out;
            }
            ServerAdmin::Leave => {
                if owner == *server.account.user {
                    return fail(server, "The owner can't leave; delete the server instead.");
                }
                Cmd::Server(ServerCmd::LeaveServer {
                    server: raw,
                    silent: false,
                })
            }
            ServerAdmin::CreateChannel { name, voice } => {
                // DataCreateServerChannel: a name of 1 to 32 characters.
                let name = name.trim();
                if name.is_empty() || name.chars().count() > 32 {
                    return fail(server, "A channel name is 1 to 32 characters.");
                }
                self.channel_for = Some(raw.clone());
                Cmd::Channel(ChannelCmd::CreateChannel {
                    server: raw,
                    name: name.to_owned(),
                    voice,
                    category: None,
                })
            }
        };
        self.net.send(cmd);
        Vec::new()
    }

    /// A server's roles, most important first, with the official editor's
    /// permission list.
    pub(super) fn server_roles(&self, server: &str) -> ServerRoles {
        let Some(s) = self.store.servers.get(server) else {
            return ServerRoles::default();
        };
        let mut roles: Vec<(&String, &crate::api::Role)> = s.roles.iter().collect();
        // Lower rank is more important; ids break ties so the order is stable.
        roles.sort_by(|a, b| a.1.rank.cmp(&b.1.rank).then_with(|| a.0.cmp(b.0)));
        ServerRoles {
            roles: roles
                .into_iter()
                .map(|(id, r)| godwit_core::server_settings::ServerRole {
                    id: id.as_str().into(),
                    name: r.name.as_str().into(),
                    colour: r.colour.as_deref().map(Into::into),
                    hoist: r.hoist,
                    allow: r.permissions.as_ref().map_or(0, |o| o.a as u64),
                    deny: r.permissions.as_ref().map_or(0, |o| o.d as u64),
                })
                .collect(),
            default_allow: s.default_permissions as u64,
            permissions: crate::permissions::server_list(),
        }
    }

    /// A server's custom emoji for the Emoji page, newest first (ULIDs sort
    /// by age, as for-web's list), with Stoat's name rules and limits.
    pub(super) fn server_emojis(&self, server: &str) -> godwit_core::server_settings::ServerEmojis {
        let autumn = self.config.as_ref().map(|c| c.features.autumn.url.clone());
        let limits = self.config.as_ref().and_then(|c| c.features.limits.as_ref());
        let mut emoji: Vec<&crate::api::Emoji> = self
            .store
            .emojis
            .values()
            .filter(|e| e.server() == Some(server))
            .collect();
        emoji.sort_by(|a, b| b.id.cmp(&a.id));
        godwit_core::server_settings::ServerEmojis {
            emoji: emoji
                .into_iter()
                .map(|e| godwit_core::server_settings::ServerEmoji {
                    id: e.id.as_str().into(),
                    name: e.name.as_str().into(),
                    url: autumn.as_ref().map(|a| format!("{a}/emojis/{}", e.id).into()),
                    creator: e
                        .creator_id
                        .as_deref()
                        .map(|c| self.store.display_name(c, Some(server)))
                        .unwrap_or_default()
                        .into(),
                })
                .collect(),
            limit: limits.and_then(|l| l.global.as_ref()?.server_emoji),
            max_file_bytes: limits.and_then(|l| l.default.file_upload_size_limits.get("emojis").copied()),
            rules: godwit_core::server_settings::EmojiRules::default(),
        }
    }

    /// Sends the watched server's emoji again when an event or our own change altered them.
    pub(super) fn sync_emoji_page(&mut self, out: &mut Vec<Event>) {
        let Some((server, sent)) = &self.emoji_watch else {
            return;
        };
        let (Some(account), true) = (self.account.clone(), self.store.servers.contains_key(server)) else {
            self.emoji_watch = None;
            return;
        };
        let now = self.server_emojis(server);
        if now != *sent {
            let server = server.clone();
            out.push(Event::ServerAdmin {
                server: ServerId::new(account, server.as_str()),
                result: Ok(ServerAdminResult::Emoji(now.clone())),
            });
            self.emoji_watch = Some((server, now));
        }
    }

    /// The answer to an emoji request: the store follows it at once (the
    /// socket's EmojiCreate/Delete repeat it; there is no rename event).
    pub(super) fn emoji_saved(&mut self, result: Result<Value, String>, out: &mut Vec<Event>) {
        let Some(change) = self.emoji_pending.take() else {
            return;
        };
        let (server, message) = match &change {
            EmojiChange::Create { server } => (server.clone(), "Emoji created."),
            EmojiChange::Rename { server, .. } => (server.clone(), "Emoji renamed."),
            EmojiChange::Delete { server, .. } => (server.clone(), "Emoji deleted."),
        };
        let result = result.map(|value| {
            match change {
                EmojiChange::Create { .. } => {
                    if let Ok(e) = serde_json::from_value::<crate::api::Emoji>(value) {
                        self.store.emojis.insert(e.id.clone(), e);
                    }
                }
                EmojiChange::Rename { id, name, .. } => {
                    if let Some(e) = self.store.emojis.get_mut(&id) {
                        e.name = name;
                    }
                }
                EmojiChange::Delete { id, .. } => {
                    self.store.emojis.remove(&id);
                }
            }
            ServerAdminResult::Done(message.into())
        });
        // The refreshed list goes first so the notice lands on current rows.
        self.sync_emoji(out);
        self.sync_emoji_page(out);
        self.admin_answer(&server, result.map_err(Into::into), out);
    }

    /// Sends the watched server's roles again when a websocket event changed them.
    pub(super) fn sync_roles(&mut self, out: &mut Vec<Event>) {
        let Some((server, sent)) = &self.roles_watch else {
            return;
        };
        let (Some(account), true) = (self.account.clone(), self.store.servers.contains_key(server)) else {
            self.roles_watch = None;
            return;
        };
        let now = self.server_roles(server);
        if now != *sent {
            let server = server.clone();
            out.push(Event::ServerAdmin {
                server: ServerId::new(account, server.as_str()),
                result: Ok(ServerAdminResult::Roles(now.clone())),
            });
            self.roles_watch = Some((server, now));
        }
    }

    pub(super) fn admin_answer(&self, server: &str, result: Result<ServerAdminResult, Arc<str>>, out: &mut Vec<Event>) {
        if let Some(account) = self.account.clone() {
            out.push(Event::ServerAdmin {
                server: ServerId::new(account, server),
                result,
            });
        }
    }

    /// An invite object from `/servers/{id}/invites`, for the Invites page.
    pub(super) fn invite(&self, v: &Value) -> Option<godwit_core::server_settings::ServerInvite> {
        let code = v.get("_id")?.as_str()?;
        let creator = v.get("creator").and_then(Value::as_str).unwrap_or_default();
        let server = v.get("server").and_then(Value::as_str);
        let channel = v
            .get("channel")
            .and_then(Value::as_str)
            .and_then(|c| self.store.channels.get(c))
            .map(|c| self.store.channel_title(c))
            .unwrap_or_default();
        Some(godwit_core::server_settings::ServerInvite {
            code: code.into(),
            // The official client copies the short link.
            link: format!("https://stt.gg/{code}").into(),
            creator: self.store.display_name(creator, server).into(),
            creator_avatar: Some(self.avatar_url(creator, server).into()),
            channel: channel.into(),
        })
    }

    /// A server's editable overview, as the official settings' Overview tab
    /// shows it; `None` once the server is gone or isn't this account's.
    pub(super) fn server_overview(&self, id: &ServerId) -> Option<godwit_core::server_settings::ServerOverview> {
        if self.account.as_ref() != Some(&id.account) {
            return None;
        }
        let s = self.store.servers.get(id.raw.as_ref())?;
        Some(godwit_core::server_settings::ServerOverview {
            name: s.name.as_str().into(),
            description: s.description.as_deref().unwrap_or_default().into(),
            icon_url: s.icon.as_ref().and_then(|f| self.file_url(f)).map(Into::into),
            banner_url: s.banner.as_ref().and_then(|f| self.file_url(f)).map(Into::into),
            can_manage: self.store.can_in_server(&s.id, crate::permissions::MANAGE_SERVER),
            can_manage_roles: self.store.can_in_server(&s.id, crate::permissions::MANAGE_ROLE),
            can_ban: self.store.can_in_server(&s.id, crate::permissions::BAN_MEMBERS),
            can_manage_emoji: self
                .store
                .can_in_server(&s.id, crate::permissions::MANAGE_CUSTOMISATION),
            owner: s.owner == *id.account.user,
            channels: s
                .channels
                .iter()
                .filter_map(|id| self.store.channels.get(id))
                .map(|c| {
                    (
                        RoomId::new(id.account.clone(), c.id.as_str()),
                        self.store.channel_title(c).into(),
                    )
                })
                .collect(),
            system_channels: crate::server_edit::SYSTEM_FIELDS.map(|field| {
                s.system_messages
                    .as_ref()
                    .and_then(|v| v.get(field))
                    .and_then(Value::as_str)
                    .map(Into::into)
            }),
        })
    }
}
