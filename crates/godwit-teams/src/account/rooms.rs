// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Conversations, teams and channels: rooms, names and user state.

use super::*;

impl TeamsAccount {
    /// The 1:1 chat with `mri`, if we know it: its status text lives in the
    /// room's own status.
    pub(super) fn partner_room(&self, mri: &str) -> Option<Room> {
        let id = self
            .conversations
            .iter()
            .find(|(_, c)| matches!(&c.kind, Kind::Direct(with) if with == mri))
            .map(|(id, _)| id.clone())?;
        self.room(&id)
    }

    pub(super) fn conversation_of_room(&self, room: &RoomId) -> Option<String> {
        let id = room.raw.to_string();
        self.conversations.contains_key(&id).then_some(id)
    }

    /// Asks, in one request, for the names of authors seen without one.
    pub(super) fn ask_unnamed(&mut self) {
        let unnamed: Vec<String> = std::mem::take(&mut self.unnamed).into_iter().collect();
        self.want_names(&unnamed);
    }

    /// Asks once for the names of people we have none for.
    pub(super) fn want_names<'a>(&mut self, people: impl IntoIterator<Item = &'a String>) {
        let wanted: Vec<String> = people
            .into_iter()
            // Ourselves too while Teams' profile left our name out ("Me").
            .filter(|p| p.starts_with("8:") && (**p != self.me || self.my_name == "Me"))
            .filter(|p| !self.names.contains_key(*p) && !self.asked.contains(*p))
            .cloned()
            .collect();
        if wanted.is_empty() {
            return;
        }
        self.asked.extend(wanted.iter().cloned());
        self.net.send(Cmd::Request(Request::Names(wanted)));
    }

    /// The other person of a 1:1 chat: from its members, else from its id
    /// (`19:{guid}_{guid}@unq.gbl.spaces`).
    pub(super) fn other(&self, id: &str, members: &[String]) -> Option<String> {
        if let Some(m) = members.iter().find(|m| **m != self.me) {
            return Some(m.clone());
        }
        let guids = id.strip_prefix("19:")?.strip_suffix("@unq.gbl.spaces")?;
        let mine = self.me.strip_prefix("8:orgid:").unwrap_or_default();
        guids
            .split('_')
            .find(|g| !g.is_empty() && *g != mine)
            .map(|g| format!("8:orgid:{g}"))
    }

    /// One entry of `GET /v1/users/ME/conversations` (or a `ConversationUpdate`).
    pub(super) fn put_conversation(&mut self, v: &Value) {
        let Some(raw) = v["id"].as_str().filter(|s| !s.is_empty()) else {
            return;
        };
        let (id, _) = split_thread(raw);
        // System conversations (`48:notes`, `48:calllogs`, …) aren't chats.
        if id.starts_with("48:") {
            return;
        }
        let tp = &v["threadProperties"];
        let props = &v["properties"];
        let prop = |k: &str| tp[k].as_str().unwrap_or_default();
        let old = self.conversations.remove(&id).unwrap_or_default();
        let members = old.members.clone();
        let kind = if id.ends_with("@thread.tacv2") || id.ends_with("@thread.skype") && !prop("spaceId").is_empty() {
            Kind::Channel(
                Some(prop("spaceId"))
                    .filter(|s| !s.is_empty())
                    .or(Some(prop("groupId")).filter(|s| !s.is_empty()))
                    .map_or_else(
                        || match &old.kind {
                            Kind::Channel(t) => t.clone(),
                            _ => String::new(),
                        },
                        str::to_owned,
                    ),
            )
        } else if prop("uniquerosterthread") == "true"
            || prop("productThreadType") == "OneToOneChat"
            || id.ends_with("@unq.gbl.spaces")
            || id.starts_with("19:uni01_")
        {
            self.other(&id, &members).map_or(Kind::Group, Kind::Direct)
        } else {
            Kind::Group
        };
        let last = &v["lastMessage"];
        let last_id = last["id"].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        let from = last["from"].as_str().and_then(mri).unwrap_or_default();
        if let Some(name) = last["imdisplayname"].as_str().filter(|n| !n.is_empty())
            && !from.is_empty()
            && from != self.me
        {
            self.names.insert(from.clone(), name.to_owned());
        }
        let last_ms = iso_ms(&last["originalarrivaltime"])
            .or_else(|| last_id.as_deref().and_then(|i| i.parse().ok()))
            .or_else(|| ms(&props["lastimreceivedtime"]))
            .unwrap_or(old.last_ms);
        let read = props["consumptionhorizon"]
            .as_str()
            .and_then(|h| h.split(';').next())
            .and_then(|i| i.parse().ok())
            .unwrap_or(old.read);
        // Teams' "Hide" sets `historyHiddenTime`: hidden until something newer arrives.
        let hidden_until = ms(&props["historyHiddenTime"]).unwrap_or(0);
        let topic = tp["topic"].as_str().map_or(old.topic.clone(), str::to_owned);
        self.conversations.insert(
            id.clone(),
            Conversation {
                id,
                kind,
                topic,
                members,
                last_ms,
                last: last_id
                    .map(|i| (i, last["clientmessageid"].as_str().unwrap_or_default().to_owned(), from))
                    .or(old.last),
                read,
                hidden: hidden_until > 0 && last_ms <= hidden_until,
                parent: old.parent,
                participating: old.participating,
            },
        );
    }

    /// The aggregator's answer: teams with their channels, and chats with
    /// their members and titles.
    pub(super) fn put_user_state(&mut self, v: &Value) {
        let mut teams = Vec::new();
        for t in v["teams"].as_array().into_iter().flatten() {
            let Some(team) = t["id"].as_str() else { continue };
            if t["isArchived"] == true || t["isDeleted"] == true {
                continue;
            }
            let mut channels = Vec::new();
            for c in t["channels"].as_array().into_iter().flatten() {
                let Some(id) = c["id"].as_str() else { continue };
                if c["isArchived"] == true || c["isDeleted"] == true {
                    continue;
                }
                let name = c["displayName"].as_str().unwrap_or("General").to_owned();
                let conv = self.conversations.entry(id.to_owned()).or_insert_with(|| Conversation {
                    id: id.to_owned(),
                    ..Conversation::default()
                });
                conv.kind = Kind::Channel(team.to_owned());
                conv.topic = name.clone();
                if let Some(t) = iso_ms(&c["lastMessage"]["originalArrivalTime"]) {
                    conv.last_ms = conv.last_ms.max(t);
                }
                channels.push((id.to_owned(), name));
            }
            teams.push(Team {
                id: team.to_owned(),
                name: t["displayName"].as_str().unwrap_or_default().to_owned(),
                channels,
            });
        }
        self.teams = teams;
        let mut seen = Vec::new();
        for c in v["chats"].as_array().into_iter().flatten() {
            let Some(id) = c["id"].as_str() else { continue };
            let members: Vec<String> = c["members"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m["mri"].as_str().map(str::to_owned))
                .collect();
            for m in c["members"].as_array().into_iter().flatten() {
                if let (Some(who), Some(name)) =
                    (m["mri"].as_str(), m["friendlyName"].as_str().filter(|n| !n.is_empty()))
                {
                    self.names.insert(who.to_owned(), name.to_owned());
                }
            }
            let one_on_one = c["isOneOnOne"] == true;
            let other = if one_on_one { self.other(id, &members) } else { None };
            if let Some(conv) = self.conversations.get_mut(id) {
                if let Some(title) = c["title"].as_str().filter(|t| !t.is_empty()) {
                    conv.topic = title.to_owned();
                }
                if let Some(other) = other {
                    conv.kind = Kind::Direct(other);
                }
                conv.members = members.clone();
            }
            seen.extend(members);
        }
        self.want_names(&seen);
    }

    pub(super) fn room(&self, id: &str) -> Option<Room> {
        let account = self.account.clone()?;
        let c = self.conversations.get(id)?;
        if c.hidden {
            return None;
        }
        // A channel the aggregator didn't list (left, archived) isn't shown.
        if let Kind::Channel(team) = &c.kind
            && !self
                .teams
                .iter()
                .any(|t| t.id == *team && t.channels.iter().any(|(ch, _)| ch == c.parent.as_deref().unwrap_or(id)))
        {
            return None;
        }
        let (name, kind) = match &c.kind {
            Kind::Direct(with) => (
                if c.topic.is_empty() {
                    self.name(with)
                } else {
                    c.topic.clone()
                },
                RoomKind::Direct {
                    with: UserId::new(account.clone(), with.as_str()),
                },
            ),
            Kind::Group => (
                if c.topic.is_empty() {
                    let names: Vec<String> = c
                        .members
                        .iter()
                        .filter(|m| **m != self.me)
                        .take(4)
                        .map(|m| self.name(m))
                        .collect();
                    if names.is_empty() {
                        "Group chat".into()
                    } else {
                        names.join(", ")
                    }
                } else {
                    c.topic.clone()
                },
                RoomKind::Group,
            ),
            Kind::Channel(team) => (
                c.topic.clone(),
                RoomKind::Channel {
                    server: ServerId::new(account.clone(), team.as_str()),
                },
            ),
        };
        let last_id = c.last.as_ref().and_then(|(i, _, _)| i.parse::<i64>().ok()).unwrap_or(0);
        let ours = c.last.as_ref().is_some_and(|(_, _, from)| *from == self.me);
        Some(Room {
            notify: Default::default(),
            id: RoomId::new(account, id),
            name: name.into(),
            kind,
            icon_url: match &c.kind {
                Kind::Direct(user) => self.picture(user).map(Into::into),
                _ => None,
            },
            capabilities: Capabilities {
                attachments: matches!(&c.kind, Kind::Direct(_)),
                threads: matches!(c.kind, Kind::Channel(_)) && c.parent.is_none(),
                max_upload: godwit_core::media::MAX_UPLOAD,
                ..Capabilities::of(Network::Teams)
            },
            // Teams chats aren't end-to-end encrypted (only some 1:1 calls can be).
            encryption: RoomEncryption::Unencrypted,
            unread: last_id > c.read && !ours,
            mentions: 0,
            last_read: (c.read > 0).then(|| MessageId(c.read.to_string().into())),
            invited_by: None,
            presence: None,
            // The other person's custom status text (a DM's subtitle).
            status: match &c.kind {
                Kind::Direct(with) => self.statuses.get(with).map(|s| s.as_str().into()),
                _ => None,
            },
            member_count: matches!(c.kind, Kind::Group)
                .then(|| u32::try_from(c.members.len()).unwrap_or(u32::MAX))
                .filter(|n| *n > 0),
            last_activity: c.last_ms,
            parent: c.parent.as_deref().map(Into::into),
            joined_thread: false,
        })
    }

    pub(super) fn remember_thread(&mut self, channel: &str, root: &str, message: Option<&Message>) -> Option<String> {
        let parent = self.conversations.get(channel)?;
        if !matches!(parent.kind, Kind::Channel(_)) || parent.parent.is_some() {
            return None;
        }
        let kind = parent.kind.clone();
        let id = format!("{channel};messageid={root}");
        let thread = self.conversations.entry(id.clone()).or_insert_with(|| Conversation {
            id: id.clone(),
            kind,
            topic: "Thread".into(),
            parent: Some(channel.to_owned()),
            ..Conversation::default()
        });
        if let Some(message) = message {
            if &*message.id.0 == root {
                thread.topic = message
                    .snippet
                    .as_deref()
                    .unwrap_or(&message.source)
                    .lines()
                    .next()
                    .unwrap_or("Thread")
                    .chars()
                    .take(60)
                    .collect();
            }
            thread.last_ms = thread.last_ms.max(message.sent_ms);
            if thread
                .last
                .as_ref()
                .is_none_or(|(last, _, _)| last.parse::<i64>().unwrap_or(0) <= message.id.0.parse::<i64>().unwrap_or(0))
            {
                thread.last = Some((message.id.0.to_string(), String::new(), message.author.raw.to_string()));
            }
            thread.participating |= *message.author.raw == self.me;
        }
        Some(id)
    }

    pub(super) fn all_rooms(&self) -> Vec<Room> {
        let mut ids: Vec<&String> = self.conversations.keys().collect();
        ids.sort();
        ids.into_iter().filter_map(|id| self.room(id)).collect()
    }

    pub(super) fn servers(&self) -> Vec<Server> {
        let Some(account) = self.account.clone() else {
            return Vec::new();
        };
        self.teams
            .iter()
            .map(|t| Server {
                id: ServerId::new(account.clone(), t.id.as_str()),
                name: t.name.as_str().into(),
                icon_url: None,
                scheduled_events: false,
                scheduled_event_count: 0,
                edit_own_profile: false,
                banner_url: None,
                categories: vec![Category {
                    id: None,
                    can_manage: false,
                    title: "".into(),
                    rooms: t
                        .channels
                        .iter()
                        .flat_map(|(channel, _)| {
                            let mut threads: Vec<_> = self
                                .conversations
                                .values()
                                .filter(|c| c.parent.as_deref() == Some(channel))
                                .collect();
                            threads.sort_by_key(|c| std::cmp::Reverse(c.last_ms));
                            std::iter::once(RoomId::new(account.clone(), channel.as_str()))
                                .chain(
                                    threads
                                        .into_iter()
                                        .take(10)
                                        .map(|c| RoomId::new(account.clone(), c.id.as_str())),
                                )
                                .collect::<Vec<_>>()
                        })
                        .collect(),
                }],
                muted: false,
                folder: None,
                rights: ServerRights::default(),
            })
            .collect()
    }

    pub(super) fn sync_rooms(&mut self, out: &mut Vec<Event>) {
        // Only what changed, and rooms that went (left, or removed from).
        let now = self.all_rooms();
        self.room_sync.sync(now, out);
        if let Some(account) = self.account.clone() {
            out.push(Event::ServersChanged {
                account,
                servers: self.servers(),
            });
        }
    }

    pub(super) fn put_conversations(&mut self, v: &Value) {
        for c in v["conversations"].as_array().into_iter().flatten() {
            self.put_conversation(c);
        }
        let people: Vec<String> = self
            .conversations
            .values()
            .filter_map(|c| match &c.kind {
                Kind::Direct(with) => Some(with.clone()),
                _ => None,
            })
            .collect();
        self.want_names(&people);
    }

    pub(super) fn own_state(&self) -> Option<Event> {
        Some(Event::OwnPresence {
            account: self.account.clone()?,
            presence: self.own_presence,
            status: self.own_status.as_deref().map(Into::into),
        })
    }
}
