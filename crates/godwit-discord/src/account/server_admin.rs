// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! A server's settings: overview, roles and admin commands.

use super::*;

impl DiscordAccount {
    pub(super) fn overview(&self, guild: &str) -> Option<ServerOverview> {
        let g = self.store.guilds.get(guild)?;
        let perms = self.store.guild_permissions(guild);
        let account = self.account.clone()?;
        Some(ServerOverview {
            name: g.name.as_str().into(),
            description: g.description.clone().unwrap_or_default().into(),
            icon_url: g.icon.as_deref().map(|h| store::guild_icon_url(guild, h).into()),
            banner_url: g.banner.as_deref().map(|h| store::guild_banner_url(guild, h).into()),
            can_manage: perms & p::MANAGE_GUILD != 0,
            can_manage_roles: perms & p::MANAGE_ROLES != 0,
            can_ban: perms & p::BAN_MEMBERS != 0,
            // Discord's emoji manager isn't wired yet; the core model is ready for it.
            can_manage_emoji: false,
            owner: g.owner == self.me(),
            channels: g
                .channels
                .iter()
                .filter_map(|c| self.store.channels.get(c))
                .filter(|c| c.kind == store::GUILD_TEXT)
                .map(|c| (RoomId::new(account.clone(), c.id.as_str()), Arc::from(c.name.as_str())))
                .collect(),
            // Discord has one system channel, for joins (and boosts).
            system_channels: [g.system_channel.as_deref().map(Into::into), None, None, None],
        })
    }

    pub(super) fn roles(&self, guild: &str) -> ServerRoles {
        let Some(g) = self.store.guilds.get(guild) else {
            return ServerRoles::default();
        };
        let mut roles: Vec<&store::Role> = g.roles.values().filter(|r| r.id != guild && !r.managed).collect();
        roles.sort_by(|a, b| b.position.cmp(&a.position).then_with(|| a.id.cmp(&b.id)));
        ServerRoles {
            roles: roles
                .into_iter()
                .map(|r| ServerRole {
                    id: r.id.as_str().into(),
                    name: r.name.as_str().into(),
                    colour: (r.color != 0).then(|| format!("#{:06x}", r.color).into()),
                    hoist: r.hoist,
                    allow: r.permissions,
                    deny: 0,
                })
                .collect(),
            default_allow: g.roles.get(guild).map_or(0, |r| r.permissions),
            permissions: permission_list(),
        }
    }

    pub(super) fn sync_roles(&mut self, out: &mut Vec<Event>) {
        let Some((guild, sent)) = &self.roles_watch else { return };
        let now = self.roles(guild);
        if now != *sent {
            let guild = guild.clone();
            if let Some(account) = self.account.clone() {
                out.push(Event::ServerAdmin {
                    server: ServerId::new(account, guild.as_str()),
                    result: Ok(ServerAdminResult::Roles(now.clone())),
                });
            }
            self.roles_watch = Some((guild, now));
        }
    }

    /// For a role, timeout or nickname change: the member it is aimed at, if
    /// our powers over them (permission and hierarchy, as the card shows
    /// them) allow it; None for other changes.
    pub(super) fn member_change(&self, guild: &str, op: &ServerAdmin) -> Result<Option<String>, &'static str> {
        let (ServerAdmin::SetMemberRole { user, .. }
        | ServerAdmin::Timeout { user, .. }
        | ServerAdmin::SetServerNickname { user, .. }) = op
        else {
            return Ok(None);
        };
        let user = user.raw.to_string();
        let allowed = self.moderation(guild, &user).is_some_and(|m| match op {
            ServerAdmin::SetMemberRole { role, .. } => m.roles.iter().any(|r| r.id == *role),
            ServerAdmin::Timeout { .. } => m.timeout,
            _ => m.nickname,
        });
        if allowed {
            Ok(Some(user))
        } else {
            Err("You can't change this member.")
        }
    }

    pub(super) fn server_admin(&mut self, server: ServerId, op: ServerAdmin) -> Vec<Event> {
        let fail = |server: ServerId, e: &str| {
            vec![Event::ServerAdmin {
                server,
                result: Err(e.into()),
            }]
        };
        let guild = server.raw.to_string();
        let Some(g) = self.store.guilds.get(&guild) else {
            return fail(server, "This server is no longer available.");
        };
        let owner = g.owner == self.me();
        let perms = self.store.guild_permissions(&guild);
        let needed = match &op {
            ServerAdmin::LoadInvites | ServerAdmin::DeleteInvite(_) => Some((p::MANAGE_GUILD, "Manage Server")),
            ServerAdmin::LoadBans | ServerAdmin::Unban(_) | ServerAdmin::Ban { .. } => {
                Some((p::BAN_MEMBERS, "Ban Members"))
            }
            ServerAdmin::Kick(_) => Some((p::KICK_MEMBERS, "Kick Members")),
            ServerAdmin::CreateRole(_)
            | ServerAdmin::EditRole(_)
            | ServerAdmin::DeleteRole(_)
            | ServerAdmin::SetDefaultPermissions(_) => Some((p::MANAGE_ROLES, "Manage Roles")),
            ServerAdmin::CreateInvite => Some((p::CREATE_INSTANT_INVITE, "Create Invite")),
            ServerAdmin::CreateChannel { .. } => Some((p::MANAGE_CHANNELS, "Manage Channels")),
            ServerAdmin::LoadEmoji
            | ServerAdmin::CreateEmoji { .. }
            | ServerAdmin::RenameEmoji { .. }
            | ServerAdmin::DeleteEmoji(_) => return fail(server, "Emoji can't be managed here yet."),
            // Checked against the member's own powers below (hierarchy included).
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
            && perms & bit != bit
        {
            return fail(server, &format!("You need the {name} permission for this."));
        }
        let target = match self.member_change(&guild, &op) {
            Ok(user) => user,
            Err(e) => {
                // The card comes back too, so its toggles show the true state.
                let mut out = fail(server, e);
                self.sync_profile(&mut out);
                return out;
            }
        };
        let admin = match op {
            ServerAdmin::LoadInvites => Admin::Invites,
            ServerAdmin::DeleteInvite(code) => Admin::RevokeInvite(code.to_string()),
            // Refused above until Discord's emoji endpoints are wired.
            ServerAdmin::LoadEmoji
            | ServerAdmin::CreateEmoji { .. }
            | ServerAdmin::RenameEmoji { .. }
            | ServerAdmin::DeleteEmoji(_) => return Vec::new(),
            ServerAdmin::LoadBans => Admin::Bans,
            ServerAdmin::Unban(user) => Admin::Unban(user.raw.to_string()),
            ServerAdmin::Kick(user) => Admin::Kick {
                user: user.raw.to_string(),
                reason: String::new(),
            },
            ServerAdmin::Ban {
                user,
                reason,
                delete_seconds,
            } => Admin::Ban {
                user: user.raw.to_string(),
                reason: reason.to_string(),
                delete_seconds,
            },
            ServerAdmin::SetMemberRole { role, on, .. } => Admin::MemberRole {
                user: target.unwrap_or_default(),
                role: role.to_string(),
                on,
            },
            ServerAdmin::Timeout { duration, .. } => Admin::ModifyMember {
                user: target.unwrap_or_default(),
                // Null lifts it; otherwise an ISO8601 time (docs.discord.food, Modify Guild Member).
                body: json!({"communication_disabled_until": duration.map(|d| {
                    (chrono::Utc::now() + d).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                })}),
            },
            ServerAdmin::SetServerNickname { user, nick } => Admin::ModifyMember {
                // Our own nickname has its own route.
                user: if &*user.raw == self.me() {
                    "@me".into()
                } else {
                    user.raw.to_string()
                },
                body: json!({"nick": nick.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty())}),
            },
            ServerAdmin::LoadRoles => {
                let roles = self.roles(&guild);
                self.roles_watch = Some((guild, roles.clone()));
                return vec![Event::ServerAdmin {
                    server,
                    result: Ok(ServerAdminResult::Roles(roles)),
                }];
            }
            ServerAdmin::CreateRole(name) => {
                let name = name.trim();
                if name.is_empty() || name.chars().count() > 100 {
                    return fail(server, "A role name is 1 to 100 characters.");
                }
                Admin::CreateRole(name.to_owned())
            }
            ServerAdmin::EditRole(edit) => {
                let mut body = serde_json::Map::new();
                if let Some(name) = &edit.name {
                    body.insert("name".into(), json!(name.trim()));
                }
                if let Some(colour) = &edit.colour {
                    let value = colour
                        .as_deref()
                        .and_then(|c| u32::from_str_radix(c.trim_start_matches('#'), 16).ok())
                        .unwrap_or(0);
                    body.insert("color".into(), json!(value));
                }
                if let Some(hoist) = edit.hoist {
                    body.insert("hoist".into(), json!(hoist));
                }
                if let Some((allow, _)) = edit.permissions {
                    body.insert("permissions".into(), json!(allow.to_string()));
                }
                Admin::ModifyRole {
                    role: edit.id.to_string(),
                    body: Value::Object(body),
                }
            }
            ServerAdmin::DeleteRole(role) => Admin::DeleteRole(role.to_string()),
            ServerAdmin::SetDefaultPermissions(allow) => Admin::ModifyRole {
                role: guild.clone(),
                body: json!({"permissions": allow.to_string()}),
            },
            ServerAdmin::DeleteServer => {
                if !owner {
                    return fail(server, "Only the owner can delete this server.");
                }
                Admin::Delete
            }
            ServerAdmin::Leave => {
                if owner {
                    return fail(
                        server,
                        "The owner can't leave; delete the server or give it away first.",
                    );
                }
                Admin::Leave
            }
            ServerAdmin::CreateInvite => {
                let channels = g.channels.clone();
                let invitable = |c: &String| {
                    self.store
                        .channels
                        .get(c)
                        .is_some_and(|ch| ch.kind == store::GUILD_TEXT)
                        && self.store.can(c, p::CREATE_INSTANT_INVITE | p::VIEW_CHANNEL)
                };
                let channel = self
                    .viewing
                    .clone()
                    .filter(|v| channels.contains(v) && invitable(v))
                    .or_else(|| channels.iter().find(|c| invitable(c)).cloned());
                let Some(channel) = channel else {
                    return fail(server, "There's no channel here you may invite people to.");
                };
                Admin::CreateInvite { channel }
            }
            ServerAdmin::SetNotify(level) => {
                let level = level.min(2);
                self.store.guild_notify.insert(g.id.clone(), level);
                self.net.send(Cmd::Request(Request::GuildNotify {
                    guild: g.id.clone(),
                    level,
                }));
                let mut out = Vec::new();
                self.sync_servers(&mut out);
                return out;
            }
            ServerAdmin::MarkRead => {
                let reads: Vec<(String, String)> = g
                    .channels
                    .iter()
                    .filter(|c| self.store.unread(c))
                    .filter_map(|c| Some((c.clone(), self.store.channels.get(c)?.last_message_id.clone()?)))
                    .collect();
                for (c, m) in &reads {
                    self.store.mark_read(c, m);
                }
                if !reads.is_empty() {
                    self.net.send(Cmd::Request(Request::AckBulk(reads)));
                }
                let mut out = Vec::new();
                self.sync_rooms(&mut out);
                return out;
            }
            ServerAdmin::CreateChannel { name, voice } => {
                let name = name.trim();
                if name.is_empty() || name.chars().count() > 100 {
                    return fail(server, "A channel name is 1 to 100 characters.");
                }
                Admin::CreateChannel {
                    name: name.to_owned(),
                    voice,
                }
            }
        };
        self.net.send(Cmd::Request(Request::Admin { guild, op: admin }));
        Vec::new()
    }

    pub(super) fn admin_answer(
        &mut self,
        guild: &str,
        result: Result<AdminAnswer, crate::backend::RequestError>,
        out: &mut Vec<Event>,
    ) {
        let Some(account) = self.account.clone() else { return };
        let server = ServerId::new(account.clone(), guild);
        let result = match result {
            Ok(AdminAnswer::Invites(list)) => Ok(ServerAdminResult::Invites(
                list.as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|i| {
                        let code = i["code"].as_str()?;
                        let inviter = i["inviter"]["id"].as_str().unwrap_or_default();
                        Some(ServerInvite {
                            code: code.into(),
                            link: format!("https://discord.gg/{code}").into(),
                            creator: i["inviter"]
                                .get("global_name")
                                .and_then(Value::as_str)
                                .or_else(|| i["inviter"]["username"].as_str())
                                .unwrap_or_default()
                                .into(),
                            creator_avatar: Some(store::avatar_url(inviter, i["inviter"]["avatar"].as_str()).into()),
                            channel: i["channel"]["name"].as_str().unwrap_or_default().into(),
                        })
                    })
                    .collect(),
            )),
            Ok(AdminAnswer::Bans(list)) => Ok(ServerAdminResult::Bans(
                list.as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|b| {
                        let user = &b["user"];
                        let id = user["id"].as_str()?;
                        Some(ServerBan {
                            user: UserId::new(account.clone(), id),
                            name: user["username"].as_str().unwrap_or(id).into(),
                            avatar_url: Some(store::avatar_url(id, user["avatar"].as_str()).into()),
                            reason: b["reason"].as_str().unwrap_or_default().into(),
                        })
                    })
                    .collect(),
            )),
            Ok(AdminAnswer::InviteCreated(code)) => Ok(ServerAdminResult::InviteCreated(
                format!("https://discord.gg/{code}").into(),
            )),
            Ok(AdminAnswer::Done) => Ok(ServerAdminResult::Done("Done.".into())),
            Err(e) => Err(e.into()),
        };
        // A list after a change (revoke, unban) also finishes the change.
        let listed = matches!(result, Ok(ServerAdminResult::Invites(_) | ServerAdminResult::Bans(_)));
        let failed = result.is_err();
        out.push(Event::ServerAdmin {
            server: server.clone(),
            result,
        });
        if failed {
            // A refused member change leaves the card's toggles ahead of the member.
            self.sync_profile(out);
        }
        if listed {
            out.push(Event::ServerAdmin {
                server,
                result: Ok(ServerAdminResult::Done("Done.".into())),
            });
        }
    }

    // The network's news.
}
