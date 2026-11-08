// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Group chats: rooms, servers, messages and members.

use super::*;

impl SteamAccount {
    pub(super) fn group_room_id(&self, group: u64, chat: u64) -> Option<RoomId> {
        Some(RoomId::new(self.account.clone()?, group_room_raw(group, chat)))
    }

    /// What to call a group: its name, or (like Steam) its other members' names.
    pub(super) fn group_title(&self, group: u64) -> String {
        let Some(g) = self.groups.get(&group) else {
            return "Group chat".into();
        };
        if !g.name.is_empty() {
            return g.name.clone();
        }
        let names: Vec<String> = g
            .members
            .iter()
            .map(|a| backend::steam_id(*a))
            .filter(|id| Some(*id) != self.me)
            .take(4)
            .map(|id| self.name_of(id))
            .collect();
        if names.is_empty() {
            "Group chat".into()
        } else {
            names.join(", ")
        }
    }

    /// A group room as Godwit's room: a channel of the group's server, or
    /// (an ad-hoc chat with one room) the group itself as a group DM.
    pub(super) fn group_room(&self, group: u64, chat: u64) -> Option<Room> {
        let account = self.account.clone()?;
        let g = self.groups.get(&group)?;
        let r = g.rooms.iter().find(|r| r.chat == chat)?;
        let c = self.group_chats.get(&(group, chat));
        let server = g.is_server();
        Some(Room {
            id: RoomId::new(account.clone(), group_room_raw(group, chat)),
            name: if server {
                r.name.clone()
            } else {
                self.group_title(group)
            }
            .into(),
            kind: if server {
                RoomKind::Channel {
                    server: ServerId::new(account, group_server_raw(group)),
                }
            } else {
                RoomKind::Group
            },
            icon_url: if server {
                None
            } else {
                g.avatar_url.clone().map(Into::into)
            },
            capabilities: Capabilities {
                // Steam rooms have text; voice only where the group allows it.
                voice: r.voice_allowed,
                voice_unavailable: false,
                // Steam's group typing notices aren't wired.
                typing: false,
                ..Capabilities::of(Network::Steam)
            },
            encryption: RoomEncryption::Unencrypted,
            unread: c.is_some_and(|c| c.unread),
            mentions: c.map_or(0, |c| c.mentions),
            last_read: None,
            invited_by: None,
            presence: None,
            status: None,
            member_count: u32::try_from(g.members.len()).ok().filter(|n| *n > 0),
            last_activity: c.map_or(0, |c| i64::from(c.last) * 1000),
            parent: None,
        })
    }

    pub(super) fn all_group_rooms(&self) -> Vec<Room> {
        let mut keys: Vec<(u64, u64)> = self
            .groups
            .values()
            .flat_map(|g| g.rooms.iter().map(move |r| (g.id, r.chat)))
            .collect();
        keys.sort_unstable();
        keys.into_iter().filter_map(|(g, c)| self.group_room(g, c)).collect()
    }

    /// The groups shown as servers, by name.
    pub(super) fn group_servers(&self) -> Vec<Server> {
        let Some(account) = self.account.clone() else {
            return Vec::new();
        };
        let mut groups: Vec<&GroupInfo> = self.groups.values().filter(|g| g.is_server()).collect();
        groups.sort_by_key(|g| (self.group_title(g.id).to_lowercase(), g.id));
        groups
            .into_iter()
            .map(|g| Server {
                id: ServerId::new(account.clone(), group_server_raw(g.id)),
                name: self.group_title(g.id).into(),
                icon_url: g.avatar_url.clone().map(Into::into),
                banner_url: None,
                categories: vec![Category {
                    id: None,
                    can_manage: false,
                    title: "".into(),
                    rooms: g
                        .rooms
                        .iter()
                        .map(|r| RoomId::new(account.clone(), group_room_raw(g.id, r.chat)))
                        .collect(),
                }],
                muted: false,
                rights: Default::default(),
            })
            .collect()
    }

    pub(super) fn group_message(&mut self, room: &RoomId, line: &Line) -> Option<Message> {
        let account = self.account.clone()?;
        let me = self.me?;
        let from = backend::steam_id(line.from);
        let avatar = if from == me {
            self.my_avatar.clone()
        } else {
            self.friends
                .get(&from)
                .and_then(|f| f.persona.avatar_hash.as_deref())
                .map(avatar_url)
        };
        let converted = crate::bbcode::convert(&line.text);
        let id = message_id(line.time, line.ordinal);
        let embeds = self.game_embeds(Some(room.clone()), &id, from, converted.games);
        Some(Message {
            id,
            author: UserId::new(account, from.to_string()),
            author_name: self.name_of(from).into(),
            author_avatar: Some(avatar.unwrap_or_else(|| avatar_url(&[])).into()),
            author_colour: None,
            sent_ms: i64::from(line.time) * 1000,
            edited: false,
            system: false,
            text: converted.rich.into(),
            markup: Markup::Rich,
            source: converted.plain.into(),
            reply_to: None,
            jumbo: converted
                .jumbo
                .into_iter()
                .map(|(url, name)| (url.into(), name.into()))
                .collect(),
            attachments: images(converted.images),
            embeds,
            invites: converted.invites.into_iter().map(Into::into).collect(),
            reactions: Vec::new(),
            pending: false,
            snippet: None,
            pinned: false,
        })
    }

    /// Steam's group list, all of it: rooms and servers that changed, appeared
    /// or went away, and who's in each room's voice chat.
    pub(super) fn on_groups(&mut self, list: Vec<GroupInfo>, out: &mut Vec<Event>) {
        let before: HashSet<(u64, u64)> = self
            .groups
            .values()
            .flat_map(|g| g.rooms.iter().map(move |r| (g.id, r.chat)))
            .collect();
        self.groups = list.into_iter().map(|g| (g.id, g)).collect();
        let mut unknown = HashSet::new();
        for g in self.groups.values() {
            for r in &g.rooms {
                let c = self.group_chats.entry((g.id, r.chat)).or_default();
                c.last = c.last.max(r.last_message);
                if r.last_message > r.last_read && r.last_read > 0 && self.viewing_group != Some((g.id, r.chat)) {
                    c.unread = true;
                }
            }
            for a in &g.members {
                let id = backend::steam_id(*a);
                if Some(id) != self.me && !self.friends.get(&id).is_some_and(|f| f.persona.name.is_some()) {
                    unknown.insert(id);
                }
            }
        }
        if !unknown.is_empty() {
            self.net.send(Cmd::RequestPersonas(unknown.into_iter().collect()));
        }
        if !self.ready {
            return;
        }
        let Some(account) = self.account.clone() else { return };
        let now: HashSet<(u64, u64)> = self
            .groups
            .values()
            .flat_map(|g| g.rooms.iter().map(move |r| (g.id, r.chat)))
            .collect();
        for (g, c) in before.difference(&now) {
            self.group_chats.remove(&(*g, *c));
            out.push(Event::RoomRemoved(RoomId::new(account.clone(), group_room_raw(*g, *c))));
        }
        out.extend(self.all_group_rooms().into_iter().map(Event::RoomUpdated));
        out.push(Event::ServersChanged {
            account,
            servers: self.group_servers(),
        });
        self.push_voice_occupants(out);
        if let Some((g, c)) = self.members_for
            && let Some(event) = self.group_members(g, c)
        {
            out.push(event);
        }
    }

    /// An accepted invite whose group has arrived: open it (a server's
    /// card goes to "Joined"; a group DM opens its room).
    pub(super) fn finish_join(&mut self, out: &mut Vec<Event>) {
        let Some((code, group)) = self.joining.clone() else {
            return;
        };
        let Some(g) = self.groups.get(&group) else { return };
        let Some(account) = self.account.clone() else { return };
        let room = g.rooms.first().map_or(g.default_chat, |r| r.chat);
        let server = g.is_server();
        self.joining = None;
        if server {
            out.push(Event::InviteJoined {
                account: account.clone(),
                code: code.into(),
                server: ServerId::new(account, group_server_raw(group)),
            });
        } else {
            out.push(Event::RoomOpened(RoomId::new(account, group_room_raw(group, room))));
        }
    }

    /// A group room's member list: online first, then offline, as Steam's
    /// group chat lists them; in each, the owner, officers and moderators
    /// first (their rank as the status line when nothing's playing), then by
    /// name. Big groups only give some of their members.
    pub(super) fn group_members(&self, group: u64, chat: u64) -> Option<Event> {
        let account = self.account.clone()?;
        let g = self.groups.get(&group)?;
        let mut online = Vec::new();
        let mut offline = Vec::new();
        for a in &g.members {
            let id = backend::steam_id(*a);
            let persona = self.friends.get(&id).map(|f| &f.persona);
            let state = if Some(id) == self.me {
                self.my_state.map_or(Presence::Online, presence)
            } else {
                persona.and_then(|p| p.state).map_or(Presence::Offline, presence)
            };
            let avatar = if Some(id) == self.me {
                self.my_avatar.clone()
            } else {
                persona.and_then(|p| p.avatar_hash.as_deref()).map(avatar_url)
            };
            let rank = g.ranks.get(a).copied().unwrap_or(0);
            let title = match rank {
                50 => Some("Owner"),
                40 => Some("Officer"),
                30 => Some("Moderator"),
                _ => None,
            };
            let member = ListMember {
                user: UserId::new(account.clone(), id.to_string()),
                name: self.name_of(id).into(),
                avatar_url: Some(avatar.unwrap_or_else(|| avatar_url(&[])).into()),
                colour: None,
                presence: state,
                status: persona
                    .and_then(playing)
                    .map(Into::into)
                    .or_else(|| title.map(Into::into)),
            };
            let entry = (std::cmp::Reverse(rank), member);
            if state == Presence::Offline {
                offline.push(entry);
            } else {
                online.push(entry);
            }
        }
        let order = |(rank, m): &(std::cmp::Reverse<i32>, ListMember)| (*rank, m.name.to_lowercase());
        online.sort_by_key(order);
        offline.sort_by_key(order);
        let online: Vec<ListMember> = online.into_iter().map(|(_, m)| m).collect();
        let offline: Vec<ListMember> = offline.into_iter().map(|(_, m)| m).collect();
        let mut groups = Vec::new();
        for (title, members) in [("Online", online), ("Offline", offline)] {
            if !members.is_empty() {
                groups.push(MemberGroup {
                    title: format!("{title} — {}", members.len()).into(),
                    members,
                });
            }
        }
        Some(Event::Members {
            room: RoomId::new(account, group_room_raw(group, chat)),
            groups,
        })
    }
}
