// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! A server's settings: overview, roles and admin commands.

use super::*;
use godwit_core::server_settings::VoiceMemberOp;

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
            can_manage_emoji: perms & (p::MANAGE_GUILD_EXPRESSIONS | p::CREATE_GUILD_EXPRESSIONS) != 0,
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
            can_reorder: false,
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

    /// A server's custom emoji for the Emoji page, newest first (snowflakes
    /// sort by age), with Discord's name rule and upload size. The slot limit
    /// (50/100/150/250 by boost level) counts static and animated emoji
    /// separately, which the core's single limit can't say, so Discord
    /// refuses a full server itself.
    pub(super) fn server_emojis(&self, guild: &str) -> godwit_core::server_settings::ServerEmojis {
        use godwit_core::server_settings::{EmojiRules, ServerEmoji, ServerEmojis};
        let Some(g) = self.store.guilds.get(guild) else {
            return ServerEmojis::default();
        };
        let mut ids: Vec<&String> = g.emojis.keys().collect();
        ids.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| b.cmp(a)));
        ServerEmojis {
            emoji: ids
                .into_iter()
                .filter_map(|id| {
                    let (name, animated) = g.emojis.get(id)?;
                    Some(ServerEmoji {
                        id: id.as_str().into(),
                        name: name.as_str().into(),
                        url: Some(store::emoji_url(id, *animated).into()),
                        creator: g
                            .emoji_rules
                            .get(id)
                            .and_then(|r| r.creator.as_deref())
                            .map(|c| self.store.display_name(c, Some(guild)))
                            .unwrap_or_default()
                            .into(),
                    })
                })
                .collect(),
            limit: None,
            max_file_bytes: Some(backend::EMOJI_MAX_BYTES),
            rules: EmojiRules {
                min_len: 2,
                max_len: 32,
                uppercase: true,
            },
        }
    }

    /// Whether we may rename or delete this emoji: Manage Expressions covers
    /// any; Create Expressions only our own (as the official client).
    pub(super) fn emoji_change_allowed(&self, guild: &str, id: &str) -> Result<(), &'static str> {
        let perms = self.store.guild_permissions(guild);
        let Some(g) = self.store.guilds.get(guild).filter(|g| g.emojis.contains_key(id)) else {
            return Err("That emoji is no longer here.");
        };
        let own = g
            .emoji_rules
            .get(id)
            .and_then(|r| r.creator.as_deref())
            .is_some_and(|c| c == self.me());
        emoji_change_ok(perms, own)
    }

    /// Sends the open Emoji page's list again when the gateway changed it.
    pub(super) fn sync_emoji_page(&mut self, out: &mut Vec<Event>) {
        let Some((guild, sent)) = &self.emoji_page else { return };
        let (Some(account), true) = (self.account.clone(), self.store.guilds.contains_key(guild)) else {
            self.emoji_page = None;
            return;
        };
        let now = self.server_emojis(guild);
        if now != *sent {
            let guild = guild.clone();
            out.push(Event::ServerAdmin {
                server: ServerId::new(account, guild.as_str()),
                result: Ok(ServerAdminResult::Emoji(now.clone())),
            });
            self.emoji_page = Some((guild, now));
        }
    }

    /// Demo mode: the GUILD_EMOJIS_UPDATE a create (no `id`), rename (both)
    /// or delete (`id` only) brings.
    pub(super) fn demo_emoji(&mut self, guild: &str, id: Option<&str>, name: Option<&str>, out: &mut Vec<Event>) {
        let Some(g) = self.store.guilds.get(guild) else { return };
        let mut list: Vec<Value> = g
            .emojis
            .iter()
            .map(|(eid, (n, animated))| json!({"id": eid, "name": n, "animated": animated, "user": {"id": self.me()}}))
            .collect();
        match (id, name) {
            (None, Some(name)) => {
                let next = g
                    .emojis
                    .keys()
                    .filter_map(|k| k.parse::<u64>().ok())
                    .max()
                    .map_or_else(crate::api::nonce, |n| n.saturating_add(1).to_string());
                list.push(json!({"id": next, "name": name, "animated": false, "user": {"id": self.me()}}));
            }
            (Some(id), Some(name)) => {
                for e in list.iter_mut().filter(|e| e["id"] == id) {
                    e["name"] = json!(name);
                }
            }
            (Some(id), None) => list.retain(|e| e["id"] != id),
            (None, None) => {}
        }
        self.dispatch("GUILD_EMOJIS_UPDATE", &json!({"guild_id": guild, "emojis": list}), out);
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
        | ServerAdmin::SetServerNickname { user, .. }
        | ServerAdmin::VoiceMember { user, .. }) = op
        else {
            return Ok(None);
        };
        let user = user.raw.to_string();
        let allowed = self.moderation(guild, &user).is_some_and(|m| match op {
            ServerAdmin::SetMemberRole { role, .. } => m.roles.iter().any(|r| r.id == *role),
            ServerAdmin::Timeout { .. } => m.timeout,
            ServerAdmin::VoiceMember { op, .. } => m.voice.is_some_and(|v| match op {
                VoiceMemberOp::Mute(_) => v.mute,
                VoiceMemberOp::Deafen(_) => v.deafen,
                VoiceMemberOp::Disconnect => v.disconnect,
            }),
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
        // The Emoji page is left when another server or another page loads.
        if self.emoji_page.as_ref().is_some_and(|(g, _)| *g != guild) || (op.is_load() && op != ServerAdmin::LoadEmoji)
        {
            self.emoji_page = None;
        }
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
            | ServerAdmin::SetRoleRanks(_)
            | ServerAdmin::DeleteRole(_)
            | ServerAdmin::SetDefaultPermissions(_) => Some((p::MANAGE_ROLES, "Manage Roles")),
            ServerAdmin::CreateInvite => Some((p::CREATE_INSTANT_INVITE, "Create Invite")),
            ServerAdmin::CreateChannel { .. } | ServerAdmin::MoveChannel { .. } => {
                Some((p::MANAGE_CHANNELS, "Manage Channels"))
            }
            // Either expressions permission opens the page; whose emoji may be
            // changed is checked below.
            ServerAdmin::LoadEmoji
            | ServerAdmin::CreateEmoji { .. }
            | ServerAdmin::RenameEmoji { .. }
            | ServerAdmin::DeleteEmoji(_) => {
                if perms & (p::MANAGE_GUILD_EXPRESSIONS | p::CREATE_GUILD_EXPRESSIONS) == 0 {
                    return fail(server, "You need the Manage Expressions permission for this.");
                }
                None
            }
            // Checked against the member's own powers below (hierarchy included).
            ServerAdmin::SetMemberRole { .. }
            | ServerAdmin::Timeout { .. }
            | ServerAdmin::SetServerNickname { .. }
            | ServerAdmin::VoiceMember { .. } => None,
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
        let admin =
            match op {
                ServerAdmin::LoadInvites => Admin::Invites,
                ServerAdmin::DeleteInvite(code) => Admin::RevokeInvite(code.to_string()),
                ServerAdmin::LoadEmoji => {
                    if self.demo.is_some() {
                        let list = self.server_emojis(&guild);
                        self.emoji_page = Some((guild, list.clone()));
                        return vec![Event::ServerAdmin {
                            server,
                            result: Ok(ServerAdminResult::Emoji(list)),
                        }];
                    }
                    // The list with uploaders (the gateway's may lack them);
                    // the page is watched once the answer arrives.
                    Admin::Emojis
                }
                ServerAdmin::CreateEmoji { name, file } => {
                    let name = name.trim();
                    if let Err(e) = self.server_emojis(&guild).check_new(name, file.size) {
                        return fail(server, &e);
                    }
                    if self.demo.is_some() {
                        let mut out = Vec::new();
                        self.demo_emoji(&guild, None, Some(name), &mut out);
                        out.push(Event::ServerAdmin {
                            server,
                            result: Ok(ServerAdminResult::Done("Done.".into())),
                        });
                        return out;
                    }
                    Admin::CreateEmoji {
                        name: name.to_owned(),
                        path: file.path,
                    }
                }
                ServerAdmin::RenameEmoji { id, name } => {
                    let name = name.trim();
                    if let Err(e) = self.server_emojis(&guild).rules.validate_name(name) {
                        return fail(server, &e);
                    }
                    if let Err(e) = self.emoji_change_allowed(&guild, &id) {
                        return fail(server, e);
                    }
                    if self.demo.is_some() {
                        let mut out = Vec::new();
                        self.demo_emoji(&guild, Some(&id), Some(name), &mut out);
                        out.push(Event::ServerAdmin {
                            server,
                            result: Ok(ServerAdminResult::Done("Done.".into())),
                        });
                        return out;
                    }
                    Admin::RenameEmoji {
                        id: id.to_string(),
                        name: name.to_owned(),
                    }
                }
                ServerAdmin::DeleteEmoji(id) => {
                    if let Err(e) = self.emoji_change_allowed(&guild, &id) {
                        return fail(server, e);
                    }
                    if self.demo.is_some() {
                        let mut out = Vec::new();
                        self.demo_emoji(&guild, Some(&id), None, &mut out);
                        out.push(Event::ServerAdmin {
                            server,
                            result: Ok(ServerAdminResult::Done("Done.".into())),
                        });
                        return out;
                    }
                    Admin::DeleteEmoji(id.to_string())
                }
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
                // Modify Guild Member's mute, deaf and channel_id (null takes
                // them out of voice), docs.discord.food/resources/guild.
                ServerAdmin::VoiceMember { op, .. } => Admin::ModifyMember {
                    user: target.unwrap_or_default(),
                    body: match op {
                        VoiceMemberOp::Mute(on) => json!({"mute": on}),
                        VoiceMemberOp::Deafen(on) => json!({"deaf": on}),
                        VoiceMemberOp::Disconnect => json!({"channel_id": null}),
                    },
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
                ServerAdmin::SetRoleRanks(_) => {
                    return fail(server, "Role reordering isn't available here yet.");
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
                ServerAdmin::CreateChannel { name, kind, category } => {
                    let name = name.trim();
                    if name.is_empty() || name.chars().count() > 100 {
                        return fail(server, "A channel name is 1 to 100 characters.");
                    }
                    // Create Guild Channel types: 0 text, 2 voice, 4 category.
                    let kind = match kind {
                        NewChannelKind::Text => store::GUILD_TEXT,
                        NewChannelKind::Voice => store::GUILD_VOICE,
                        NewChannelKind::Category => store::GUILD_CATEGORY,
                    };
                    let parent = category.map(|c| c.to_string());
                    if let Some(parent) = &parent
                        && self.store.channels.get(parent).is_none_or(|c| {
                            c.kind != store::GUILD_CATEGORY || c.guild.as_deref() != Some(guild.as_str())
                        })
                    {
                        return fail(server, "That category is no longer available.");
                    }
                    if self.demo.is_some() {
                        let mut out = Vec::new();
                        self.demo_create(&guild, name, kind, parent.as_deref(), &mut out);
                        out.push(Event::ServerAdmin {
                            server,
                            result: Ok(ServerAdminResult::Done("Done.".into())),
                        });
                        return out;
                    }
                    Admin::CreateChannel {
                        name: name.to_owned(),
                        kind,
                        parent,
                    }
                }
                ServerAdmin::MoveChannel { id, category, up } => {
                    let moves = self.move_plan(&guild, &id, category, up);
                    // Already at the end of the list: nothing to send.
                    if moves.is_empty() || self.demo.is_some() {
                        let mut out = Vec::new();
                        if self.demo.is_some() {
                            self.demo_move(&Value::Array(moves), &mut out);
                        }
                        out.push(Event::ServerAdmin {
                            server,
                            result: Ok(ServerAdminResult::Done("Done.".into())),
                        });
                        return out;
                    }
                    Admin::MoveChannels(Value::Array(moves))
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
            Ok(AdminAnswer::Emojis(list)) => {
                // The answer carries uploaders the store may lack; the store
                // follows it, then the page shows it.
                if let Some(g) = self.store.guilds.get_mut(guild) {
                    store::put_emojis(g, &list);
                }
                let now = self.server_emojis(guild);
                self.emoji_page = Some((guild.to_owned(), now.clone()));
                Ok(ServerAdminResult::Emoji(now))
            }
            Ok(AdminAnswer::EmojiCreated(e)) => {
                // The POST answer names the uploader (us): the store takes it
                // now, so a Create-only user may change it before the gateway repeats.
                if let Some(g) = self.store.guilds.get_mut(guild) {
                    store::put_emoji(g, &e);
                }
                Ok(ServerAdminResult::Done("Done.".into()))
            }
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

/// Manage Expressions changes any emoji; Create Expressions only the ones we
/// uploaded (docs.discord.food, Permissions; the official client's gating).
fn emoji_change_ok(perms: u64, own: bool) -> Result<(), &'static str> {
    if perms & p::MANAGE_GUILD_EXPRESSIONS != 0 || (own && perms & p::CREATE_GUILD_EXPRESSIONS != 0) {
        Ok(())
    } else {
        Err("You can only change emoji you added.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use godwit_core::server_settings::EmojiFile;

    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0, 0, 0, 0];

    fn staffed(perms: u64) -> (DiscordAccount, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
        let (net, mut cmds) = Handle::detached();
        let (_tx, events) = mpsc::channel();
        let mut a = DiscordAccount::with(u32::MAX, net, events, Status::Connecting);
        let mut ready = crate::store::test_ready();
        ready["guilds"][0]["roles"][1]["permissions"] = json!(perms.to_string());
        ready["guilds"][0]["emojis"] = json!([
            {"id": "77", "name": "blob", "animated": false, "user": {"id": "999", "username": "other"}},
            {"id": "88", "name": "mine", "animated": true, "user": {"id": "100"}},
        ]);
        a.handle(Ev::Ready(Box::new(ready)), &mut Vec::new());
        while cmds.try_recv().is_ok() {}
        (a, cmds)
    }

    fn admin(
        a: &mut DiscordAccount,
        cmds: &mut tokio::sync::mpsc::UnboundedReceiver<Cmd>,
        op: ServerAdmin,
    ) -> Result<Option<Admin>, String> {
        let server = ServerId::new(a.account.clone().unwrap(), "10");
        let out = a.run(Command::ServerAdmin { server, op });
        if let Some(Event::ServerAdmin { result: Err(e), .. }) = out.last() {
            return Err(e.to_string());
        }
        Ok(match cmds.try_recv() {
            Ok(Cmd::Request(Request::Admin { op, .. })) => Some(op),
            _ => None,
        })
    }

    fn file(size: u64) -> EmojiFile {
        EmojiFile {
            path: "/nowhere/e.png".into(),
            size,
        }
    }

    #[test]
    fn emoji_list_has_uploaders_newest_first_and_discords_rules() {
        let (a, _c) = staffed(p::MANAGE_GUILD_EXPRESSIONS);
        let list = a.server_emojis("10");
        let names: Vec<&str> = list.emoji.iter().map(|e| &*e.name).collect();
        assert_eq!(names, ["mine", "blob"]);
        assert!(list.emoji[0].url.as_deref().unwrap().ends_with("88.gif?size=48"));
        assert_eq!(list.max_file_bytes, Some(256 << 10));
        assert!(list.check_new("ok_Name1", 10).is_ok());
        assert!(list.check_new("a", 10).is_err());
        assert!(list.check_new("no-dash", 10).is_err());
        assert!(list.check_new("fine", 257 << 10).is_err());
    }

    #[test]
    fn the_page_needs_an_expressions_permission_and_others_emoji_need_manage() {
        let (mut a, mut c) = staffed(0);
        assert!(admin(&mut a, &mut c, ServerAdmin::LoadEmoji).is_err());
        let (mut a, mut c) = staffed(p::CREATE_GUILD_EXPRESSIONS);
        assert!(matches!(
            admin(&mut a, &mut c, ServerAdmin::LoadEmoji),
            Ok(Some(Admin::Emojis))
        ));
        // Create Expressions: our own emoji only.
        assert!(admin(&mut a, &mut c, ServerAdmin::DeleteEmoji("77".into())).is_err());
        assert!(matches!(
            admin(&mut a, &mut c, ServerAdmin::DeleteEmoji("88".into())),
            Ok(Some(Admin::DeleteEmoji(id))) if id == "88"
        ));
        let (mut a, mut c) = staffed(p::MANAGE_GUILD_EXPRESSIONS);
        assert!(matches!(
            admin(&mut a, &mut c, ServerAdmin::RenameEmoji { id: "77".into(), name: " blub ".into() }),
            Ok(Some(Admin::RenameEmoji { id, name })) if id == "77" && name == "blub"
        ));
        assert!(admin(&mut a, &mut c, ServerAdmin::DeleteEmoji("1".into())).is_err());
        assert!(emoji_change_ok(p::CREATE_GUILD_EXPRESSIONS, false).is_err());
    }

    #[test]
    fn creating_validates_before_sending() {
        let (mut a, mut c) = staffed(p::CREATE_GUILD_EXPRESSIONS);
        let new = |name: &str, size| ServerAdmin::CreateEmoji {
            name: name.into(),
            file: file(size),
        };
        assert!(admin(&mut a, &mut c, new("x", 10)).is_err());
        assert!(admin(&mut a, &mut c, new("bad name", 10)).is_err());
        assert!(admin(&mut a, &mut c, new("big", 300 << 10)).is_err());
        assert!(matches!(
            admin(&mut a, &mut c, new("good_one", 10)),
            Ok(Some(Admin::CreateEmoji { name, .. })) if name == "good_one"
        ));
    }

    #[test]
    fn request_shapes_and_image_encoding() {
        assert_eq!(
            crate::api::emoji_create_body("a_b", "data:x"),
            json!({"name": "a_b", "image": "data:x", "roles": []})
        );
        assert!(backend::emoji_uri(&PNG).unwrap().starts_with("data:image/png;base64,"));
        assert!(
            backend::emoji_uri(b"GIF89a")
                .unwrap()
                .starts_with("data:image/gif;base64,")
        );
        assert!(backend::emoji_uri(b"nope").is_err());
        assert!(backend::emoji_uri(&vec![0xff; 300 << 10]).is_err());
    }

    #[test]
    fn gateway_updates_without_uploaders_keep_them_and_a_create_answer_records_ours() {
        let (mut a, _c) = staffed(p::CREATE_GUILD_EXPRESSIONS);
        a.dispatch(
            "GUILD_EMOJIS_UPDATE",
            &json!({"guild_id": "10", "emojis": [{"id": "88", "name": "mine"}, {"id": "77", "name": "blob"}]}),
            &mut Vec::new(),
        );
        assert!(a.emoji_change_allowed("10", "88").is_ok());
        assert!(a.emoji_change_allowed("10", "77").is_err());
        a.admin_answer(
            "10",
            Ok(AdminAnswer::EmojiCreated(
                json!({"id": "99", "name": "new", "user": {"id": "100"}}),
            )),
            &mut Vec::new(),
        );
        assert!(a.emoji_change_allowed("10", "99").is_ok());
        // A later update without `user` keeps it.
        a.dispatch(
            "GUILD_EMOJIS_UPDATE",
            &json!({"guild_id": "10", "emojis": [{"id": "99", "name": "new"}]}),
            &mut Vec::new(),
        );
        assert!(a.emoji_change_allowed("10", "99").is_ok());
    }

    #[test]
    fn the_list_answer_fills_the_store_and_gateway_updates_refresh_the_page() {
        let (mut a, _c) = staffed(p::MANAGE_GUILD_EXPRESSIONS);
        let mut out = Vec::new();
        a.admin_answer(
            "10",
            Ok(AdminAnswer::Emojis(
                json!([{"id": "5", "name": "new", "user": {"id": "100"}}]),
            )),
            &mut out,
        );
        assert!(
            matches!(&out[0], Event::ServerAdmin { result: Ok(ServerAdminResult::Emoji(l)), .. } if l.emoji.len() == 1)
        );
        let mut out = Vec::new();
        a.dispatch(
            "GUILD_EMOJIS_UPDATE",
            &json!({"guild_id": "10", "emojis": [{"id": "5", "name": "new"}, {"id": "6", "name": "more"}]}),
            &mut out,
        );
        assert!(out.iter().any(
            |e| matches!(e, Event::ServerAdmin { result: Ok(ServerAdminResult::Emoji(l)), .. } if l.emoji.len() == 2)
        ));
    }

    #[test]
    fn demo_creates_renames_and_deletes_through_gateway_events() {
        let ready = json!({
            "user": {"id": "100", "username": "me_bloop"},
            "guilds": [{
                "id": "10",
                "properties": {"name": "Qux Hall", "owner_id": "100"},
                "roles": [{"id": "10", "name": "@everyone", "permissions": "0", "position": 0}],
                "channels": [{"id": "11", "type": 0, "name": "general", "position": 0}],
                "emojis": [{"id": "77", "name": "blob", "animated": false}]
            }],
            "private_channels": [], "relationships": [], "merged_members": [[]],
        });
        let (mut a, _) = DiscordAccount::demo(ready, HashMap::new());
        let server = ServerId::new(a.account.clone().unwrap(), "10");
        let run = |a: &mut DiscordAccount, op| {
            let out = a.run(Command::ServerAdmin {
                server: server.clone(),
                op,
            });
            assert!(
                matches!(out.last(), Some(Event::ServerAdmin { result: Ok(_), .. })),
                "{out:?}"
            );
        };
        run(&mut a, ServerAdmin::LoadEmoji);
        run(
            &mut a,
            ServerAdmin::CreateEmoji {
                name: "fresh".into(),
                file: file(10),
            },
        );
        let names = |a: &DiscordAccount| -> Vec<String> {
            a.server_emojis("10").emoji.iter().map(|e| e.name.to_string()).collect()
        };
        assert_eq!(names(&a), ["fresh", "blob"]);
        run(
            &mut a,
            ServerAdmin::RenameEmoji {
                id: "77".into(),
                name: "blobby".into(),
            },
        );
        assert_eq!(names(&a), ["fresh", "blobby"]);
        run(&mut a, ServerAdmin::DeleteEmoji("77".into()));
        assert_eq!(names(&a), ["fresh"]);
    }
}
