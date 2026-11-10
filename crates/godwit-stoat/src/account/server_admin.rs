// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A server's settings: roles, invites, overview and admin commands.

use super::*;

/// Apply the sidebar's one-step move to Stoat's ordered category payload.
/// The default bucket is first; an edge channel crosses into the adjacent
/// category, matching the core MoveChannel contract.
pub(super) fn move_category_item(cats: &mut [crate::api::Category], id: &str, is_category: bool, up: bool) {
    if is_category {
        let Some(from) = cats.iter().position(|c| c.id == id) else {
            return;
        };
        let to = if up {
            from.checked_sub(1)
        } else {
            (from + 1 < cats.len()).then_some(from + 1)
        };
        if let Some(to) = to {
            cats.swap(from, to);
        }
        return;
    }
    let Some(from) = cats.iter().position(|c| c.channels.iter().any(|ch| ch == id)) else {
        return;
    };
    let Some(index) = cats[from].channels.iter().position(|ch| ch == id) else {
        return;
    };
    if up && index > 0 {
        cats[from].channels.swap(index, index - 1);
    } else if !up && index + 1 < cats[from].channels.len() {
        cats[from].channels.swap(index, index + 1);
    } else if up {
        let Some(to) = from.checked_sub(1) else { return };
        cats[from].channels.remove(index);
        cats[to].channels.push(id.to_owned());
    } else {
        let to = from + 1;
        if to >= cats.len() {
            return;
        }
        cats[from].channels.remove(index);
        cats[to].channels.insert(0, id.to_owned());
    }
}

/// A fresh category id in ULID form, as for-web's `ulid()` makes them (the
/// API takes 1 to 32 characters). Time-ordered; the low bits mix the clock's
/// nanoseconds with a counter so two ids in one millisecond differ.
pub(super) fn new_category_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let ms = now.as_millis() & ((1 << 48) - 1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let low = (u128::from(now.subsec_nanos()) << 48 | u128::from(n & 0xffff_ffff_ffff)) & ((1 << 80) - 1);
    encode_ulid(ms << 80 | low)
}

/// Crockford base32, 26 characters, as ULIDs are written.
fn encode_ulid(mut v: u128) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut out = [b'0'; 26];
    for slot in out.iter_mut().rev() {
        *slot = ALPHABET[(v & 31) as usize];
        v >>= 5;
    }
    out.iter().map(|&b| char::from(b)).collect()
}

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
            | ServerAdmin::SetRoleRanks(_)
            | ServerAdmin::DeleteRole(_)
            | ServerAdmin::SetDefaultPermissions(_) => Some((p::MANAGE_ROLE, "Manage Roles")),
            // for-web gates the Emoji page on Manage Customisation.
            ServerAdmin::LoadEmoji
            | ServerAdmin::CreateEmoji { .. }
            | ServerAdmin::RenameEmoji { .. }
            | ServerAdmin::DeleteEmoji(_) => Some((p::MANAGE_CUSTOMISATION, "Manage Customisation")),
            ServerAdmin::LoadModerationRules
            | ServerAdmin::SetModerationRuleEnabled { .. }
            | ServerAdmin::DeleteModerationRule(_) => {
                return fail(server, "This network has no AutoMod rules to manage.");
            }
            ServerAdmin::LoadExpressions(_)
            | ServerAdmin::CreateExpression { .. }
            | ServerAdmin::RenameExpression { .. }
            | ServerAdmin::DeleteExpression { .. } => {
                return fail(server, "This network has no stickers or sounds to manage.");
            }
            ServerAdmin::CreateInvite => Some((p::INVITE_OTHERS, "Invite Others")),
            ServerAdmin::CreateChannel { .. } | ServerAdmin::MoveChannel { .. } => {
                Some((p::MANAGE_CHANNEL, "Manage Channels"))
            }
            // Checked below with the owner and rank rules.
            ServerAdmin::SetMemberRole { .. } => None,
            // Stoat has no Members tab on a role (the page doesn't offer it).
            ServerAdmin::SetRoleMembers { .. } | ServerAdmin::LoadRoleMembers(_) => {
                return fail(server, "This server can't list a role's members.");
            }
            ServerAdmin::Timeout { .. } => Some((p::TIMEOUT_MEMBERS, "Timeout Members")),
            // Checked below: our own needs Change Nickname, others' Manage Nicknames.
            ServerAdmin::SetServerNickname { .. } => None,
            // Stoat member cards don't offer call controls (moderation leaves `voice` empty).
            ServerAdmin::VoiceMember { .. } => return fail(server, "You can't change this member."),
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
            ServerAdmin::SetMemberRole { user, role, on } => {
                if user.account != server.account {
                    return fail(server, "That member belongs to another account.");
                }
                let target = user.raw.to_string();
                let key = MemberId {
                    server: raw.clone(),
                    user: target.clone(),
                };
                let Some(held) = self.store.members.get(&key).map(|m| m.roles.clone()) else {
                    return fail(server, "They're no longer in this server.");
                };
                // The same rules the card was built with, checked again now.
                if !self.assignable_roles(&raw, &target).iter().any(|r| r.id == role) {
                    return fail(server, "You can't change that role for them.");
                }
                // for-web sends the whole list, minus roles the server no
                // longer has.
                let mut roles: Vec<String> = held.into_iter().filter(|r| r.as_str() != role.as_ref()).collect();
                if on {
                    roles.push(role.to_string());
                }
                let known = self
                    .store
                    .servers
                    .get(&raw)
                    .map(|s| s.roles.clone())
                    .unwrap_or_default();
                roles.retain(|r| known.contains_key(r));
                Cmd::Member(MemberCmd::SetRoles {
                    server: raw,
                    user: target,
                    roles,
                })
            }
            ServerAdmin::Timeout { user, duration } => {
                if user.account != server.account {
                    return fail(server, "That member belongs to another account.");
                }
                let target = user.raw.to_string();
                let key = MemberId {
                    server: raw.clone(),
                    user: target.clone(),
                };
                if self.store.me.as_deref() == Some(target.as_str())
                    || !self.store.outranks(&raw, &target)
                    || !self.store.members.contains_key(&key)
                {
                    return fail(server, "You can't time out that member.");
                }
                // DataMemberEdit.timeout is an ISO 8601 timestamp; removal uses FieldsMember.
                let body = match duration {
                    Some(duration) => {
                        let until = chrono::TimeDelta::from_std(duration)
                            .ok()
                            .and_then(|d| chrono::Utc::now().checked_add_signed(d));
                        let Some(until) = until else {
                            return fail(server, "That timeout is too long.");
                        };
                        serde_json::json!({"timeout": until.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)})
                    }
                    None => serde_json::json!({"remove": ["Timeout"]}),
                };
                Cmd::Member(MemberCmd::EditMember {
                    server: raw,
                    user: target,
                    body,
                    avatar: None,
                })
            }
            ServerAdmin::SetServerNickname { user, nick } => {
                if user.account != server.account {
                    return fail(server, "That member belongs to another account.");
                }
                let target = user.raw.to_string();
                let allowed = self.moderation(&raw, &target).is_some_and(|m| m.nickname);
                if !allowed {
                    return fail(server, "You can't change that nickname.");
                }
                // DataMemberEdit.nickname (1-32 characters); clearing it removes
                // the field (FieldsMember "Nickname"), as for-web's ServerIdentity
                // modal does.
                let nick = nick.as_deref().map(str::trim).filter(|n| !n.is_empty());
                if nick.is_some_and(|n| n.chars().count() > 32) {
                    return fail(server, "Nicknames can be at most 32 characters.");
                }
                let body = match nick {
                    Some(nick) => serde_json::json!({ "nickname": nick }),
                    None => serde_json::json!({ "remove": ["Nickname"] }),
                };
                Cmd::Member(MemberCmd::EditMember {
                    server: raw,
                    user: target,
                    body,
                    avatar: None,
                })
            }
            ServerAdmin::MoveChannel { id, category, up } => {
                let Some(s) = self.store.servers.get(&raw) else {
                    return fail(server, "This server's channel order is unavailable.");
                };
                // The ordered list includes the "default" bucket of
                // uncategorised channels: the official sidebar saves the
                // whole list, default included.
                let mut cats = self.store.ordered_categories(s);
                let before = cats.clone();
                move_category_item(&mut cats, &id, category, up);
                if cats == before {
                    return vec![Event::ServerAdmin {
                        server,
                        result: Ok(ServerAdminResult::Done("Channel order unchanged.".into())),
                    }];
                }
                self.net.send(Cmd::Server(ServerCmd::EditCategories {
                    server: raw,
                    body: serde_json::json!({ "categories": crate::store::Store::categories_value(&cats) }),
                }));
                return Vec::new();
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
            // Refused above.
            ServerAdmin::SetRoleMembers { .. } | ServerAdmin::LoadRoleMembers(_) => return Vec::new(),
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
            ServerAdmin::SetRoleRanks(ranks) => {
                // The official server Roles page orders the highest priority first.
                Cmd::Role(RoleCmd::Edit(RoleEditCmd::SetRoleRanks {
                    server: raw,
                    ranks: ranks.iter().map(ToString::to_string).collect(),
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
            ServerAdmin::LoadModerationRules
            | ServerAdmin::SetModerationRuleEnabled { .. }
            | ServerAdmin::DeleteModerationRule(_) => {
                return fail(server, "This network has no AutoMod rules to manage.");
            }
            ServerAdmin::LoadExpressions(_)
            | ServerAdmin::CreateExpression { .. }
            | ServerAdmin::RenameExpression { .. }
            | ServerAdmin::DeleteExpression { .. } => {
                return fail(server, "This network has no stickers or sounds to manage.");
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
            // Refused before the permission check above.
            ServerAdmin::VoiceMember { .. } => return fail(server, "You can't change this member."),
            ServerAdmin::Leave => {
                if owner == *server.account.user {
                    return fail(server, "The owner can't leave; delete the server instead.");
                }
                Cmd::Server(ServerCmd::LeaveServer {
                    server: raw,
                    silent: false,
                })
            }
            ServerAdmin::CreateChannel { name, kind, category } => {
                let voice = match kind {
                    godwit_core::server_settings::NewChannelKind::Text => false,
                    godwit_core::server_settings::NewChannelKind::Voice => true,
                    godwit_core::server_settings::NewChannelKind::Category => {
                        let name = name.trim();
                        if name.is_empty() || name.chars().count() > 32 {
                            return fail(server, "A category name is 1 to 32 characters.");
                        }
                        // for-web's CreateCategory: PATCH the server with the
                        // existing categories plus a new empty one.
                        let title = name.to_owned();
                        let Some(body) = self.store.edit_categories(&raw, |cats| {
                            cats.push(crate::api::Category {
                                id: new_category_id(),
                                title,
                                channels: Vec::new(),
                            });
                        }) else {
                            return fail(server, "This server is no longer available.");
                        };
                        self.net.send(Cmd::Server(ServerCmd::EditCategories {
                            server: raw,
                            body: serde_json::json!({ "categories": body }),
                        }));
                        return Vec::new();
                    }
                };
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
                    category: category.map(|id| id.to_string()),
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
            can_reorder: true,
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
            role_members: false,
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
            kind: None,
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
            can_manage_stickers: false,
            can_manage_sounds: false,
            can_manage_automod: false,
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
            safety: None,
        })
    }
}
