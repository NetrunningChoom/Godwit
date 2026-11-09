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
        self.sync_stickers(out);
    }

    /// The servers' own stickers, for the picker. Only still-sendable ones
    /// are offered: Discord lets a user without Nitro send a server's own
    /// stickers in that server, nothing from other servers or the standard
    /// packs (Nitro, docs.discord.food Stickers), so the core shows each
    /// server's in its own channels only. Lottie ones (servers with the
    /// VERIFIED/PARTNERED feature) can't be drawn, so they're left out.
    pub(super) fn sync_stickers(&mut self, out: &mut Vec<Event>) {
        use godwit_core::stickers::{Sticker, StickerGroup};
        let Some(account) = self.account.clone() else { return };
        let groups: Vec<StickerGroup> = self
            .store
            .guild_order
            .iter()
            .filter_map(|id| self.store.guilds.get(id))
            .filter_map(|g| {
                let mut stickers: Vec<Sticker> = g
                    .stickers
                    .iter()
                    .filter_map(|s| {
                        Some(Sticker {
                            id: s.id.as_str().into(),
                            name: s.name.as_str().into(),
                            tags: s.tags.as_str().into(),
                            image_url: store::sticker_url(&s.id, s.format, 160)?.into(),
                            available: s.available,
                        })
                    })
                    .collect();
                if stickers.is_empty() {
                    return None;
                }
                stickers.sort_by_key(|s| s.name.to_lowercase());
                Some(StickerGroup {
                    server: ServerId::new(account.clone(), g.id.as_str()),
                    title: g.name.as_str().into(),
                    stickers,
                })
            })
            .collect();
        if self.stickers_sent.as_deref().unwrap_or_default() != groups.as_slice() {
            self.stickers_sent = Some(groups.clone());
            out.push(Event::Stickers { account, groups });
        }
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
            can_ignore: user != self.me(),
            ignored: self.store.ignored.contains(user),
            // Discord's report menu is server-driven (/reporting/menu, breadcrumbs), which the neutral fixed reason lists can't express.
            can_report: false,
            mutual_servers: self.mutual_servers(fetched),
            mutual_friends: self.mutual_friends(fetched),
            note: self
                .notes
                .get(user)
                .filter(|n| !n.is_empty())
                .map(|n| Arc::from(n.as_str())),
            // Only once the note arrived, so typing never races the fetch.
            can_note: user != self.me() && self.notes.contains_key(user),
            note_limit: crate::api::NOTE_LIMIT,
        })
    }

    /// What the member's card offers (the web client's member menu): Kick,
    /// Ban, the Roles checklist, Timeout and server nicknames, each with its
    /// permission and only for members ranked below us; for ourselves only
    /// the nickname (Change Nickname).
    pub(super) fn moderation(&self, guild: &str, user: &str) -> Option<Moderation> {
        let account = self.account.clone()?;
        let g = self.store.guilds.get(guild)?;
        let perms = self.store.guild_permissions(guild);
        let member = self.store.members.get(&(guild.to_owned(), user.to_owned()));
        let mut m = Moderation::new(ServerId::new(account, guild));
        m.nick = member.and_then(|m| m.nick.as_deref()).map(Arc::from);
        if user == self.me() {
            m.nickname = perms & p::CHANGE_NICKNAME != 0;
            return m.any().then_some(m);
        }
        let mine = self.store.rank(guild, self.me());
        if mine <= self.store.rank(guild, user) {
            return None;
        }
        m.kick = perms & p::KICK_MEMBERS != 0;
        m.ban = perms & p::BAN_MEMBERS != 0;
        m.ban_history = m.ban;
        m.nickname = perms & p::MANAGE_NICKNAMES != 0;
        m.timeout = perms & p::MODERATE_MEMBERS != 0 && !self.store.is_admin(guild, user);
        m.timed_out = m.timeout && member.is_some_and(|m| m.timed_out(chrono::Utc::now().timestamp()));
        if perms & p::MANAGE_ROLES != 0 {
            // Only roles below our highest (the owner's is unbounded); @everyone
            // and managed (integration) roles can't be handed out.
            let mut roles: Vec<&store::Role> = g
                .roles
                .values()
                .filter(|r| r.id != guild && !r.managed && r.position < mine)
                .collect();
            roles.sort_by_key(|r| std::cmp::Reverse(r.position));
            m.roles = roles
                .into_iter()
                .map(|r| ModerationRole {
                    id: Arc::from(r.id.as_str()),
                    name: Arc::from(r.name.as_str()),
                    colour: (r.color != 0).then_some([(r.color >> 16) as u8, (r.color >> 8) as u8, r.color as u8]),
                    assigned: member.is_some_and(|m| m.roles.contains(&r.id)),
                })
                .collect();
        }
        m.any().then_some(m)
    }

    /// Keeps (or, when empty, forgets) our note on `user`; whether it changed.
    pub(super) fn set_note(&mut self, user: &str, note: &str) -> bool {
        let note: String = note.chars().take(crate::api::NOTE_LIMIT).collect();
        if note.is_empty() {
            // Remember "none" too, so the profile doesn't ask again.
            return self
                .notes
                .insert(user.to_owned(), String::new())
                .is_none_or(|old| !old.is_empty());
        }
        self.notes.insert(user.to_owned(), note.clone()) != Some(note)
    }

    /// The servers we share with `user`, from the profile's `mutual_guilds`
    /// ([{id, nick}], docs.discord.food, Get User Profile); only ones we
    /// have in the store, since the card names and opens them from there.
    fn mutual_servers(&self, fetched: Option<&Value>) -> Vec<MutualServer> {
        let Some(account) = self.account.clone() else {
            return Vec::new();
        };
        fetched
            .and_then(|f| f["mutual_guilds"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|g| {
                let id = g["id"].as_str()?;
                self.store.guilds.contains_key(id).then(|| MutualServer {
                    server: ServerId::new(account.clone(), id),
                    nick: g["nick"].as_str().filter(|n| !n.is_empty()).map(Arc::from),
                })
            })
            .collect()
    }

    /// Friends we share with the profile's owner (`mutual_friends`, users).
    fn mutual_friends(&self, fetched: Option<&Value>) -> Vec<MutualFriend> {
        fetched
            .and_then(|f| f["mutual_friends"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|u| {
                let id = u["id"].as_str()?;
                Some(MutualFriend {
                    user: self.user_id(id)?,
                    name: u["name"].as_str().unwrap_or(id).into(),
                    avatar_url: u["avatar"].as_str().map(Arc::from),
                })
            })
            .collect()
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

impl DiscordAccount {
    /// Looks up members of `room`'s server by name that aren't loaded. Only
    /// the user's own typing gets here (the UI debounces it); see
    /// [`crate::member_search`] for the one-in-flight rule.
    pub(super) fn search_members(&mut self, room: &RoomId, query: &str, _out: &mut Vec<Event>) {
        let Some(guild) = self
            .channel_of(room)
            .and_then(|c| self.store.channels.get(&c).and_then(|c| c.guild.clone()))
        else {
            return;
        };
        if let Some(search) = self.member_search.ask(Instant::now(), room, &guild, query) {
            self.send_member_search(&search);
        }
    }

    fn send_member_search(&self, search: &crate::member_search::Search) {
        if self.demo.is_none() {
            self.net.send(Cmd::Gateway {
                op: crate::member_search::REQUEST_GUILD_MEMBERS,
                data: search.payload(),
            });
        }
    }

    /// GUILD_MEMBERS_CHUNK: the members are kept (the user looked for them,
    /// so names and mentions resolve), and the search they answer, if it's
    /// ours and still wanted, reports who it found.
    pub(super) fn members_chunk(&mut self, d: &Value, out: &mut Vec<Event>) {
        let Some((guild, nonce, members)) = crate::member_search::parse_chunk(d) else {
            return;
        };
        for m in members {
            self.store.put_member(guild, m);
        }
        let Some(done) = nonce.and_then(|n| self.member_search.answer(Instant::now(), n)) else {
            return;
        };
        if let Some(next) = &done.next {
            self.send_member_search(next);
        }
        if done.stale {
            return;
        }
        let Some(account) = self.account.clone() else {
            return;
        };
        let found: Vec<Member> = members
            .iter()
            .filter_map(|m| m["user"]["id"].as_str())
            .map(|u| Member {
                user: UserId::new(account.clone(), u),
                name: self.store.display_name(u, Some(guild)).into(),
                avatar_url: Some(self.avatar(u, Some(guild)).into()),
                colour: self.store.colour(u, guild),
                presence: self.presence(u),
                status: None,
            })
            .collect();
        out.push(Event::MemberSearch {
            room: done.room,
            query: done.query,
            members: found,
        });
    }

    /// Demo: answers a search with the chunk Discord would send, of made-up
    /// members whose names start with the query.
    pub(super) fn demo_member_search(&mut self, room: &RoomId, query: &str, out: &mut Vec<Event>) {
        let Some(guild) = self
            .channel_of(room)
            .and_then(|c| self.store.channels.get(&c).and_then(|c| c.guild.clone()))
        else {
            return;
        };
        let Some(search) = self.member_search.ask(Instant::now(), room, &guild, query) else {
            return;
        };
        let members: Vec<Value> = ["ina", "bert", "ley"]
            .iter()
            .enumerate()
            .map(|(i, tail)| {
                let name = format!("{query}{tail}");
                json!({"user": {"id": format!("9000{i}"), "username": name.to_lowercase(), "global_name": name},
                       "roles": [], "nick": null})
            })
            .collect();
        let chunk = json!({"guild_id": guild, "members": members, "nonce": search.nonce,
                           "chunk_index": 0, "chunk_count": 1});
        self.dispatch("GUILD_MEMBERS_CHUNK", &chunk, out);
    }
}

#[cfg(test)]
mod member_search_tests {
    use super::*;

    #[test]
    fn the_demo_answers_a_search_with_a_chunk_and_keeps_the_members() {
        let ready = json!({
            "user": {"id": "100", "username": "me_bloop"},
            "guilds": [{
                "id": "10",
                "properties": {"name": "Qux Hall", "owner_id": "100"},
                "roles": [{"id": "10", "name": "@everyone", "permissions": "0", "position": 0}],
                "channels": [{"id": "11", "type": 0, "name": "general", "position": 0}]
            }],
            "private_channels": [], "relationships": [], "merged_members": [[]],
        });
        let (mut a, _) = DiscordAccount::demo(ready, HashMap::new());
        let room = RoomId::new(a.account.clone().unwrap(), "11");
        let out = a.run(Command::SearchMembers {
            room: room.clone(),
            query: "al".into(),
        });
        let Some(Event::MemberSearch {
            room: got,
            query,
            members,
        }) = out.last()
        else {
            panic!("no answer: {out:?}");
        };
        assert_eq!((got, query.as_str(), members.len()), (&room, "al", 3));
        assert!(members.iter().all(|m| m.name.starts_with("al")));
        assert!(
            a.store.members.contains_key(&("10".to_owned(), "90000".to_owned())),
            "kept for names and mentions"
        );
    }

    #[test]
    fn a_newer_query_waits_for_the_answer_and_the_stale_result_is_dropped() {
        let mut a = DiscordAccount::demo(
            json!({"user": {"id": "100", "username": "me"}, "guilds": [{
            "id": "10", "properties": {"name": "Q", "owner_id": "100"},
            "roles": [{"id": "10", "name": "@everyone", "permissions": "0", "position": 0}],
            "channels": [{"id": "11", "type": 0, "name": "general", "position": 0}]}],
            "private_channels": [], "relationships": [], "merged_members": [[]]}),
            HashMap::new(),
        )
        .0;
        let room = RoomId::new(a.account.clone().unwrap(), "11");
        // Two asks before any answer (the real path sends nothing in demo, so ask by hand).
        let first = a.member_search.ask(Instant::now(), &room, "10", "a").unwrap();
        assert!(a.member_search.ask(Instant::now(), &room, "10", "al").is_none());
        let mut out = Vec::new();
        a.dispatch(
            "GUILD_MEMBERS_CHUNK",
            &json!({"guild_id": "10", "nonce": first.nonce, "members": [{"user": {"id": "5", "username": "ann"}}]}),
            &mut out,
        );
        assert!(
            !out.iter().any(|e| matches!(e, Event::MemberSearch { .. })),
            "stale answer dropped"
        );
        assert!(a.store.members.contains_key(&("10".to_owned(), "5".to_owned())));
    }
}
