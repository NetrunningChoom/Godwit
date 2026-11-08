// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! People: profiles, members, presence, friends and emoji.

use super::*;

impl StoatAccount {
    /// The open profile card, from what's known now.
    pub(super) fn profile(&self) -> Option<Profile> {
        let (user, server, group) = self.profile_for.as_ref()?;
        let server = server.as_deref();
        let member = self.member(user, server);
        let u = self.store.users.get(user);
        let fetched = self.store.profiles.get(user).and_then(Option::as_ref);
        let names = Names {
            store: &self.store,
            server,
            autumn: self.config.as_ref().map(|c| c.features.autumn.url.as_str()),
        };
        let roles = server
            .and_then(|s| {
                let srv = self.store.servers.get(s)?;
                let key = MemberId {
                    server: s.to_owned(),
                    user: user.clone(),
                };
                let mut roles: Vec<&crate::api::Role> = self
                    .store
                    .members
                    .get(&key)?
                    .roles
                    .iter()
                    .filter_map(|r| srv.roles.get(r))
                    .collect();
                roles.sort_by_key(|r| r.rank);
                Some(
                    roles
                        .into_iter()
                        .map(|r| {
                            let colour = r
                                .colour
                                .as_deref()
                                .and_then(crate::api::parse_hex_colour)
                                .map(|(r, g, b)| [r, g, b]);
                            (r.name.as_str().into(), colour)
                        })
                        .collect(),
                )
            })
            .unwrap_or_default();
        Some(Profile {
            user: member.user,
            name: member.name,
            username: u
                .map(|u| format!("{}#{}", u.username, u.discriminator))
                .unwrap_or_default()
                .into(),
            avatar_url: member.avatar_url,
            banner_url: fetched
                .and_then(|p| p.background.as_ref())
                .and_then(|f| self.file_url(f))
                .map(Into::into),
            presence: member.presence,
            status: member.status,
            bio: fetched
                .and_then(|p| p.content.as_deref())
                .filter(|b| !b.trim().is_empty())
                .map(|b| rich::to_rich(b, &names).into()),
            roles,
            me: self.store.me.as_deref() == Some(user.as_str()),
            relationship: match u.and_then(|u| u.relationship.as_deref()) {
                Some("Friend") => Relationship::Friend,
                Some("Incoming") => Relationship::Incoming,
                Some("Outgoing") => Relationship::Outgoing,
                Some("Blocked") => Relationship::Blocked,
                Some("BlockedOther") => Relationship::BlockedOther,
                _ => Relationship::None,
            },
            bot: u.is_some_and(|u| u.bot.is_some()),
            has_friends: true,
            // The official UserContextMenu offers Block for anyone but us.
            can_block: self.store.me.as_deref().is_some_and(|me| me != user.as_str()),
            moderation: server.and_then(|s| self.moderation(s, user)),
            group_moderation: group.as_ref().and_then(|channel| {
                let c = self.store.channels.get(channel)?;
                let me = self.store.me.as_deref()?;
                let remove_member = c.channel_type == "Group"
                    && user != me
                    && c.owner.as_deref() != Some(user.as_str())
                    && c.recipients.iter().any(|u| u == user)
                    && (c.owner.as_deref() == Some(me) || self.store.can(channel, crate::permissions::MANAGE_CHANNEL));
                let account = self.account.as_ref()?.clone();
                // The official UserContextMenu offers transfer only to the
                // current owner for another group recipient.
                remove_member.then(|| godwit_core::members::GroupModeration {
                    room: RoomId::new(account, channel.as_str()),
                    add_member: false,
                    remove_member,
                    transfer_ownership: c.owner.as_deref() == Some(me)
                        && user != me
                        && c.recipients.iter().any(|u| u == user),
                })
            }),
            // Reports about other people, except the network's own staff
            // (official UserContextMenu: "Report user" needs !user.privileged).
            can_report: !u.is_some_and(|u| u.privileged) && self.store.me.as_deref() != Some(user),
            // Stoat has no private notes; its mutuals need a request of their own (left off).
            mutual_servers: Vec::new(),
            mutual_friends: Vec::new(),
            note: None,
            can_note: false,
            note_limit: 0,
        })
    }

    /// Kick, Ban, Timeout and the server nickname on a member's card: with the
    /// permission, and only for members ranked below us (the server refuses the
    /// rest; the official UserContextMenu hides them the same way). For
    /// ourselves only the nickname, with Change Nickname (for-web's
    /// ServerIdentity modal).
    pub(super) fn moderation(&self, server: &str, user: &str) -> Option<godwit_core::members::Moderation> {
        use crate::permissions::{BAN_MEMBERS, CHANGE_NICKNAME, KICK_MEMBERS, MANAGE_NICKNAMES, TIMEOUT_MEMBERS};
        let account = self.account.as_ref()?;
        let member = self.store.members.get(&MemberId {
            server: server.to_owned(),
            user: user.to_owned(),
        });
        let mut m = godwit_core::members::Moderation::new(ServerId::new(account.clone(), server));
        m.nick = member
            .and_then(|m| m.nickname.as_deref())
            .filter(|n| !n.is_empty())
            .map(Arc::from);
        if self.store.me.as_deref() == Some(user) {
            m.nickname = member.is_some() && self.store.can_in_server(server, CHANGE_NICKNAME);
            return m.nickname.then_some(m);
        }
        if !self.store.outranks(server, user) {
            return None;
        }
        m.kick = self.store.can_in_server(server, KICK_MEMBERS);
        m.ban = self.store.can_in_server(server, BAN_MEMBERS);
        m.timeout = self.store.can_in_server(server, TIMEOUT_MEMBERS);
        m.nickname = member.is_some() && self.store.can_in_server(server, MANAGE_NICKNAMES);
        m.roles = self.assignable_roles(server, user);
        m.timed_out = m.timeout
            && member
                .and_then(|m| m.timeout.as_deref())
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .is_some_and(|t| t > chrono::Utc::now());
        (m.kick || m.ban || m.timeout || m.nickname || !m.roles.is_empty()).then_some(m)
    }

    /// The roles we may toggle on `user`, highest first, as for-web's
    /// UserProfileRoles: the owner edits every role; otherwise Assign Roles,
    /// a member below us, and only roles below our own best rank (the
    /// server refuses adding one at or above it). Empty when we may not.
    pub(super) fn assignable_roles(&self, server: &str, user: &str) -> Vec<godwit_core::members::ModerationRole> {
        let Some(s) = self.store.servers.get(server) else {
            return Vec::new();
        };
        let Some(me) = self.store.me.as_deref() else {
            return Vec::new();
        };
        let owner = s.owner == me;
        if !owner
            && (me == user
                || !self.store.can_in_server(server, crate::permissions::ASSIGN_ROLES)
                || !self.store.outranks(server, user))
        {
            return Vec::new();
        }
        let mine = self.store.member_ranking(server, me);
        let held: Vec<String> = self
            .store
            .members
            .get(&MemberId {
                server: server.to_string(),
                user: user.to_string(),
            })
            .map(|m| m.roles.clone())
            .unwrap_or_default();
        let mut roles: Vec<(&String, &crate::api::Role)> =
            s.roles.iter().filter(|(_, r)| owner || r.rank > mine).collect();
        roles.sort_by(|a, b| a.1.rank.cmp(&b.1.rank).then_with(|| a.0.cmp(b.0)));
        roles
            .into_iter()
            .map(|(id, r)| godwit_core::members::ModerationRole {
                id: id.as_str().into(),
                name: r.name.as_str().into(),
                colour: r
                    .colour
                    .as_deref()
                    .and_then(crate::api::parse_hex_colour)
                    .map(|(r, g, b)| [r, g, b]),
                assigned: held.contains(id),
            })
            .collect()
    }

    /// Sends the servers' custom emoji if they changed.
    pub(super) fn sync_emoji(&mut self, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else { return };
        let Some(autumn) = self.config.as_ref().map(|c| c.features.autumn.url.clone()) else {
            return;
        };
        let mut groups: Vec<EmojiGroup> = Vec::new();
        for sid in &self.store.server_order {
            let Some(server) = self.store.servers.get(sid) else {
                continue;
            };
            let mut emoji: Vec<CustomEmoji> = self
                .store
                .emojis
                .values()
                .filter(|e| e.server() == Some(sid.as_str()))
                .map(|e| CustomEmoji {
                    name: e.name.as_str().into(),
                    image_url: format!("{autumn}/emojis/{}", e.id).into(),
                    reaction: e.id.as_str().into(),
                    code: format!(":{}:", e.id).into(),
                })
                .collect();
            if emoji.is_empty() {
                continue;
            }
            emoji.sort_by(|a, b| a.name.cmp(&b.name));
            groups.push(EmojiGroup {
                server: ServerId::new(account.clone(), sid.as_str()),
                title: server.name.as_str().into(),
                emoji,
            });
        }
        // Nothing sent yet counts as none, so an account without any sends nothing.
        if self.emoji_sent.as_deref().unwrap_or_default() != groups.as_slice() {
            self.emoji_sent = Some(groups.clone());
            out.push(Event::CustomEmoji { account, groups });
        }
    }

    /// Sends the shown member list if it changed.
    pub(super) fn sync_members(&mut self, out: &mut Vec<Event>) {
        let Some(channel) = self.members_room.clone() else {
            return;
        };
        let Some(room) = self.room_id(&channel) else { return };
        let groups = self.member_groups(&channel);
        if self.members_sent.as_ref() != Some(&groups) {
            self.members_sent = Some(groups.clone());
            out.push(Event::Members { room, groups });
        }
    }

    pub(super) fn member(&self, user: &str, server: Option<&str>) -> Member {
        let account = self
            .account
            .clone()
            .unwrap_or_else(|| AccountId::new(Network::Stoat, ""));
        let presence = self.presence(user);
        Member {
            user: UserId::new(account, user),
            name: self.store.display_name(user, server).into(),
            avatar_url: Some(self.avatar_url(user, server).into()),
            colour: server
                .and_then(|s| self.store.role_colour(s, user))
                .map(|(r, g, b)| [r, g, b]),
            presence,
            status: self.status_text(user),
        }
    }

    /// Someone's presence, as the official client shows it: offline when
    /// not connected or invisible.
    pub(super) fn presence(&self, user: &str) -> Presence {
        let u = self.store.users.get(user);
        let online = u.is_some_and(|u| u.online == Some(true));
        match (
            online,
            u.and_then(|u| u.status.as_ref()).and_then(|s| s.presence.as_deref()),
        ) {
            (false, _) | (_, Some("Invisible")) => Presence::Offline,
            (_, Some("Idle")) => Presence::Idle,
            (_, Some("Busy")) => Presence::Busy,
            (_, Some("Focus")) => Presence::Focus,
            _ => Presence::Online,
        }
    }

    /// Our own presence, when it changed (here or on another device).
    pub(super) fn sync_own(&mut self, out: &mut Vec<Event>) {
        let (Some(account), Some(me)) = (self.account.clone(), self.store.me.clone()) else {
            return;
        };
        let set = self
            .store
            .users
            .get(&me)
            .and_then(|u| u.status.as_ref())
            .and_then(|s| s.presence.clone());
        let presence = match set.as_deref() {
            Some("Invisible") => Presence::Invisible,
            Some("Idle") => Presence::Idle,
            Some("Busy") => Presence::Busy,
            Some("Focus") => Presence::Focus,
            _ => Presence::Online,
        };
        let status = self.status_text(&me);
        if self.own_presence != Some(presence) || self.own_status != status {
            self.own_presence = Some(presence);
            self.own_status.clone_from(&status);
            out.push(Event::OwnPresence {
                account,
                presence,
                status,
            });
        }
    }

    /// Someone's custom status text, if they set one.
    pub(super) fn status_text(&self, user: &str) -> Option<Arc<str>> {
        let u = self.store.users.get(user)?;
        u.status
            .as_ref()?
            .text
            .clone()
            .filter(|t| !t.is_empty())
            .map(Into::into)
    }

    /// The member list as the official sidebar groups it: in a server, who's
    /// online under their highest hoisted role (by rank), then "Online", then
    /// everyone offline; in a group, everyone, online first.
    pub(super) fn member_groups(&self, channel: &str) -> Vec<MemberGroup> {
        let Some(c) = self.store.channels.get(channel) else {
            return Vec::new();
        };
        let by_name = |list: &mut Vec<Member>| list.sort_by_cached_key(|m| m.name.to_lowercase());
        let Some(sid) = c.server.as_deref() else {
            let mut members: Vec<Member> = c.recipients.iter().map(|u| self.member(u, None)).collect();
            members.sort_by_cached_key(|m| (m.presence == Presence::Offline, m.name.to_lowercase()));
            return vec![MemberGroup {
                title: format!("Members — {}", members.len()).into(),
                members,
            }];
        };
        let Some(srv) = self.store.servers.get(sid) else {
            return Vec::new();
        };
        let mut hoisted: HashMap<Option<&String>, Vec<Member>> = HashMap::new();
        let mut offline = Vec::new();
        // A restricted channel lists only who may see it, as the official
        // member sidebar does.
        let restricted = self.store.potentially_restricted(channel);
        for (key, m) in self.store.members.iter().filter(|(k, _)| k.server == sid) {
            if restricted && !self.store.member_sees(channel, &key.user) {
                continue;
            }
            let member = self.member(&key.user, Some(sid));
            if member.presence == Presence::Offline {
                offline.push(member);
                continue;
            }
            let role = m
                .roles
                .iter()
                .filter(|r| srv.roles.get(*r).is_some_and(|r| r.hoist))
                .min_by_key(|r| srv.roles[*r].rank);
            hoisted.entry(role).or_default().push(member);
        }
        let mut roles: Vec<&String> = hoisted.keys().flatten().copied().collect();
        // Roles of equal rank in a fixed order (by id): they come out of a
        // HashMap, and an unstable order swapped groups between updates,
        // which flickered the list.
        roles.sort_by_key(|r| (srv.roles[*r].rank, *r));
        let mut groups = Vec::new();
        for role in roles.into_iter().map(Some).chain(std::iter::once(None)) {
            let Some(mut members) = hoisted.remove(&role) else {
                continue;
            };
            by_name(&mut members);
            let title = role.map_or_else(|| "Online".to_owned(), |r| srv.roles[r].name.clone());
            groups.push(MemberGroup {
                title: format!("{title} — {}", members.len()).into(),
                members,
            });
        }
        if !offline.is_empty() {
            by_name(&mut offline);
            groups.push(MemberGroup {
                title: format!("Offline — {}", offline.len()).into(),
                members: offline,
            });
        }
        groups
    }

    pub(super) fn sync_friends(&mut self, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else {
            return;
        };
        let people = |relationship: &str| -> Vec<Person> {
            self.store
                .users_with(relationship)
                .into_iter()
                .map(|u| Person {
                    id: UserId::new(account.clone(), u.id.as_str()),
                    name: u.name().into(),
                    avatar_url: Some(self.avatar_url(&u.id, None).into()),
                    online: u.online.unwrap_or(false),
                })
                .collect()
        };
        let now = people("Friend");
        let requests = (people("Incoming"), people("Outgoing"));
        if now != self.friends {
            self.friends = now.clone();
            out.push(Event::Friends {
                account: account.clone(),
                people: now,
            });
        }
        let mut blocked: Vec<UserId> = self
            .store
            .users_with("Blocked")
            .into_iter()
            .map(|u| UserId::new(account.clone(), u.id.as_str()))
            .collect();
        // By id, not by name: a rename mustn't send the list again.
        blocked.sort_by(|a, b| a.raw.cmp(&b.raw));
        if blocked != self.blocked {
            self.blocked = blocked.clone();
            out.push(Event::Blocked {
                account: account.clone(),
                users: blocked,
            });
        }
        if requests != self.requests {
            self.requests = requests.clone();
            out.push(Event::FriendRequests {
                account,
                incoming: requests.0,
                outgoing: requests.1,
            });
        }
    }

    /// Sends again every kept message with a custom emoji in it (`only`:
    /// that one), so it's converted with what's known now.
    pub(super) fn reconvert_emoji_messages(&self, only: Option<&str>, out: &mut Vec<Event>) {
        for (channel, kept) in &self.store.messages {
            let Some(room) = self.room_id(channel) else { return };
            for m in kept.messages.values() {
                let content = m.content.as_deref().unwrap_or_default();
                if m.system.is_none() && has_custom_emoji(content, only) {
                    out.push(Event::MessageUpdated {
                        room: room.clone(),
                        message: self.message(m),
                    });
                }
            }
        }
    }

    /// A banned user's avatar, from the user object in a ban list.
    pub(super) fn avatar_for(&self, user: &Value) -> Option<String> {
        let file: File = serde_json::from_value(user.get("avatar")?.clone()).ok()?;
        self.file_url(&file)
    }

    /// Editable values retain the original bio markup.
    pub(super) fn own_profile(&self) -> Option<OwnProfile> {
        let me = self.store.me.as_deref()?;
        let user = self.store.users.get(me)?;
        let profile = self.store.profiles.get(me)?.as_ref()?;
        Some(OwnProfile {
            name: self.store.display_name(me, None).into(),
            display_name: user.display_name.as_deref().unwrap_or_default().into(),
            pronouns: user.pronouns.as_deref().unwrap_or_default().into(),
            bio: profile.content.as_deref().unwrap_or_default().into(),
            avatar_url: Some(self.avatar_url(me, None).into()),
            banner_url: profile
                .background
                .as_ref()
                .and_then(|file| self.file_url(file))
                .map(Into::into),
        })
    }

    /// An emoji's picture: a custom upload or the official Unicode pack.
    pub(super) fn emoji_url(&self, emoji: &str) -> Option<String> {
        let is_id = emoji.len() == 26 && emoji.bytes().all(|b| b.is_ascii_alphanumeric());
        if !is_id {
            return Some(crate::emoji::unicode_url(crate::emoji::PACK, emoji));
        }
        let autumn = &self.config.as_ref()?.features.autumn.url;
        Some(format!("{autumn}/emojis/{emoji}"))
    }

    /// A member's server avatar, else their own, else Stoat's default.
    pub(super) fn avatar_url(&self, user: &str, server: Option<&str>) -> String {
        let member = server.and_then(|s| {
            let key = MemberId {
                server: s.to_owned(),
                user: user.to_owned(),
            };
            self.store.members.get(&key).and_then(|m| m.avatar.clone())
        });
        member
            .or_else(|| self.store.users.get(user).and_then(|u| u.avatar.clone()))
            .and_then(|a| self.file_url(&a))
            .unwrap_or_else(|| format!("{DEFAULT_API}/users/{user}/default_avatar"))
    }
}
