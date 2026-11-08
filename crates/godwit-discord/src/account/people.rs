// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! People: presence, friends, profiles, emoji and moderation.

use super::*;

impl DiscordAccount {
    pub(super) fn presence(&self, user: &str) -> Presence {
        match self.store.presences.get(user).map(String::as_str) {
            Some("online") => Presence::Online,
            Some("idle") => Presence::Idle,
            Some("dnd") => Presence::Busy,
            _ => Presence::Offline,
        }
    }

    // Rooms and servers.

    pub(super) fn person(&self, user: &str) -> Option<Person> {
        Some(Person {
            id: self.user_id(user)?,
            name: self.store.display_name(user, None).into(),
            avatar_url: Some(self.avatar(user, None).into()),
            online: !matches!(self.presence(user), Presence::Offline),
        })
    }

    pub(super) fn sync_friends(&mut self, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else { return };
        let of = |kind| {
            let mut v: Vec<Person> = self
                .store
                .relationships
                .iter()
                .filter(|(_, k)| **k == kind)
                .filter_map(|(u, _)| self.person(u))
                .collect();
            v.sort_by_key(|a| a.name.to_lowercase());
            v
        };
        let friends = of(store::FRIEND);
        let requests = (of(store::INCOMING), of(store::OUTGOING));
        // What friends are doing, for Active now.
        let mut activities: Vec<(UserId, Arc<str>)> = self
            .store
            .relationships
            .iter()
            .filter(|(_, k)| **k == store::FRIEND)
            .filter_map(|(u, _)| Some((self.user_id(u)?, self.store.activities.get(u)?.as_str().into())))
            .collect();
        activities.sort_by(|a, b| a.0.cmp(&b.0));
        if activities != self.activities {
            self.activities = activities.clone();
            out.push(Event::Activities {
                account: account.clone(),
                activities,
            });
        }
        if friends != self.friends {
            self.friends = friends.clone();
            out.push(Event::Friends {
                account: account.clone(),
                people: friends,
            });
        }
        let mut blocked: Vec<UserId> = self
            .store
            .relationships
            .iter()
            .filter(|(_, k)| **k == store::BLOCKED)
            .filter_map(|(u, _)| self.user_id(u))
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

    pub(super) fn sync_all(&mut self, out: &mut Vec<Event>) {
        self.sync_rooms(out);
        self.sync_servers(out);
        self.sync_friends(out);
        self.sync_roles(out);
        self.sync_profile(out);
        self.sync_emoji(out);
    }

    /// The servers' custom emoji we may use, for the picker. Without Nitro,
    /// Discord allows a server's own still emoji only, in that server (the
    /// rooms' `external_emoji` keeps other servers' out); animated ones,
    /// ones the server lost the boost for, and ones kept for roles we don't
    /// have are left out, so the picker never offers what Discord refuses.
    pub(super) fn sync_emoji(&mut self, out: &mut Vec<Event>) {
        use godwit_core::message::{CustomEmoji, EmojiGroup};
        let Some(account) = self.account.clone() else { return };
        let nitro = self.store.premium_type != 0;
        let me = self.me().to_owned();
        let groups: Vec<EmojiGroup> = self
            .store
            .guild_order
            .iter()
            .filter_map(|id| self.store.guilds.get(id))
            .filter_map(|g| {
                let ours = self
                    .store
                    .members
                    .get(&(g.id.clone(), me.clone()))
                    .map(|m| m.roles.as_slice())
                    .unwrap_or_default();
                let mut emoji: Vec<CustomEmoji> = g
                    .emojis
                    .iter()
                    .filter(|(id, (_, animated))| {
                        let rule = g.emoji_rules.get(*id);
                        (nitro || !animated)
                            && rule.is_none_or(|r| {
                                r.available && (r.roles.is_empty() || r.roles.iter().any(|x| ours.contains(x)))
                            })
                    })
                    .map(|(id, (name, animated))| CustomEmoji {
                        name: name.as_str().into(),
                        image_url: store::emoji_url(id, *animated).into(),
                        // Reacting sends `name:id`; text holds `<:name:id>` (`<a:…>` animated).
                        reaction: format!("{name}:{id}").into(),
                        code: format!("<{}:{name}:{id}>", if *animated { "a" } else { "" }).into(),
                    })
                    .collect();
                if emoji.is_empty() {
                    return None;
                }
                emoji.sort_by_key(|e| e.name.to_lowercase());
                Some(EmojiGroup {
                    server: ServerId::new(account.clone(), g.id.as_str()),
                    title: g.name.as_str().into(),
                    emoji,
                })
            })
            .collect();
        if self.emoji_sent.as_deref().unwrap_or_default() != groups.as_slice() {
            self.emoji_sent = Some(groups.clone());
            out.push(Event::CustomEmoji { account, groups });
        }
    }

    // Messages.

    pub(super) fn profile(&self) -> Option<Profile> {
        let (user, guild) = self.profile_for.as_ref()?;
        let fetched = self.profiles.get(user);
        let u = self.store.users.get(user);
        let roles = guild
            .as_deref()
            .and_then(|g| {
                let guild = self.store.guilds.get(g)?;
                let m = self.store.members.get(&(g.to_owned(), user.clone()))?;
                let mut roles: Vec<&store::Role> = m.roles.iter().filter_map(|r| guild.roles.get(r)).collect();
                roles.sort_by_key(|r| std::cmp::Reverse(r.position));
                Some(
                    roles
                        .into_iter()
                        .map(|r| {
                            let c = r.color;
                            let colour = (c != 0).then_some([(c >> 16) as u8, (c >> 8) as u8, c as u8]);
                            (Arc::from(r.name.as_str()), colour)
                        })
                        .collect(),
                )
            })
            .unwrap_or_default();
        let bio = fetched
            .and_then(|f| f["user_profile"]["bio"].as_str().or_else(|| f["user"]["bio"].as_str()))
            .filter(|b| !b.trim().is_empty())
            .map(|b| crate::markdown::to_rich(b, &self.names(guild.as_deref())).into());
        let banner = fetched
            .and_then(|f| f["user"]["banner"].as_str())
            .map(|h| format!("{}/banners/{user}/{h}.png?size=600", store::CDN).into());
        let relationship = match self.store.relationships.get(user) {
            Some(&store::FRIEND) => Relationship::Friend,
            Some(&store::INCOMING) => Relationship::Incoming,
            Some(&store::OUTGOING) => Relationship::Outgoing,
            Some(&store::BLOCKED) => Relationship::Blocked,
            _ => Relationship::None,
        };
        Some(Profile {
            user: self.user_id(user)?,
            name: self.store.display_name(user, guild.as_deref()).into(),
            username: u.map(|u| u.username.clone()).unwrap_or_default().into(),
            avatar_url: Some(self.avatar(user, guild.as_deref()).into()),
            banner_url: banner,
            presence: self.presence(user),
            status: None,
            bio,
            roles,
            me: user == self.me(),
            relationship,
            bot: u.is_some_and(|u| u.bot),
            has_friends: true,
            moderation: guild.as_deref().and_then(|g| self.moderation(g, user)),
            group_moderation: None,
            // The web client's user context menu offers Block for anyone but us.
            can_block: user != self.me(),
            // Discord's report menu is server-driven (/reporting/menu, breadcrumbs), which the neutral fixed reason lists can't express.
            can_report: false,
        })
    }

    /// Kick and Ban on a member's card: with the permission, only for
    /// members ranked below us (Discord refuses the rest).
    pub(super) fn moderation(&self, guild: &str, user: &str) -> Option<Moderation> {
        if user == self.me() || self.store.rank(guild, self.me()) <= self.store.rank(guild, user) {
            return None;
        }
        let perms = self.store.guild_permissions(guild);
        let (kick, ban) = (perms & p::KICK_MEMBERS != 0, perms & p::BAN_MEMBERS != 0);
        let account = self.account.clone()?;
        (kick || ban).then(|| Moderation {
            server: ServerId::new(account, guild),
            kick,
            ban,
        })
    }

    pub(super) fn sync_profile(&mut self, out: &mut Vec<Event>) {
        if let Some(profile) = self.profile() {
            out.push(Event::Profile(profile));
        }
    }

    // Server settings.

    /// Applies a member list update for the open channel (SYNC, INSERT,
    /// UPDATE, DELETE, INVALIDATE), and shows the list again.
    pub(super) fn member_list_update(&mut self, d: &Value, out: &mut Vec<Event>) {
        let Some(guild) = d["guild_id"].as_str().map(str::to_owned) else {
            return;
        };
        let open = self.list_channel.get(&guild).cloned();
        // An update without an id is the open channel's list.
        let fallback = open.as_deref().map(|c| self.store.member_list_id(c));
        let Some(key) = self.member_lists.apply(d, fallback) else {
            return;
        };
        // What the rows say about people, kept for names, roles and presence.
        let members: Vec<Value> = self.member_lists.members(&key).cloned().collect();
        for m in &members {
            self.store.put_member(&guild, m);
            if let Some(user) = m["user"]["id"].as_str() {
                let mut presence = m["presence"].clone();
                if presence.is_object() {
                    presence["user_id"] = json!(user);
                    self.store.put_presence(&presence);
                }
            }
        }
        // Shown only if it's the open channel's list.
        if let Some(channel) = open.filter(|c| self.store.member_list_id(c) == key.1)
            && let (Some(room), Some(groups)) = (self.room_id(&channel), self.list_groups(&channel))
        {
            out.push(Event::Members { room, groups });
        }
    }

    /// Subscribes `guild`'s open channel to the ranges around rows `first`
    /// to `last` (op 37, as the web client does while its list scrolls),
    /// only when they changed.
    pub(super) fn ask_ranges(&mut self, guild: &str, channel: &str, first: usize, last: usize) {
        let ranges = crate::member_list::ranges(first, last);
        if self.list_ranges.get(guild) == Some(&ranges) {
            return;
        }
        self.list_ranges.insert(guild.to_owned(), ranges.clone());
        self.net.send(Cmd::Gateway {
            op: crate::gateway::GUILD_SUBSCRIPTIONS_BULK,
            data: json!({"subscriptions": {guild: {
                "typing": true, "activities": true, "threads": true,
                "channels": {channel: ranges},
            }}}),
        });
    }

    /// The streamed member list's groups, if it's this channel's.
    pub(super) fn list_groups(&self, channel: &str) -> Option<Vec<MemberGroup>> {
        let guild = self.store.channels.get(channel)?.guild.as_ref()?;
        let rows = self.member_lists.rows(guild, &self.store.member_list_id(channel))?;
        let account = self.account.clone()?;
        Some(crate::member_list::groups(
            rows,
            &self.store,
            guild,
            &account,
            |user| self.avatar(user, Some(guild)),
            |user| self.presence(user),
        ))
    }
}
