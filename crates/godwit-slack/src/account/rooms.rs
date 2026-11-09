// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Conversations and workspaces: rooms, counts and names.

use super::*;

impl SlackAccount {
    pub(super) fn refresh_conversations(&mut self, team: &str) {
        let Some(ws) = self.workspaces.get_mut(team) else {
            return;
        };
        if !ws.conversations_pending {
            ws.conversations_pending = true;
            self.request(team, Request::Conversations);
        }
    }

    /// Follows these people's presence too (DM partners, people who write
    /// in loaded chats), up to `FOLLOWED` per workspace.
    pub(super) fn follow(&mut self, team: &str, users: impl IntoIterator<Item = String>) {
        let list = self.followed.entry(team.to_owned()).or_default();
        let before = list.len();
        for u in users {
            if list.len() >= FOLLOWED {
                break;
            }
            if u.starts_with('U') || u.starts_with('W') {
                list.insert(u);
            }
        }
        if list.len() != before {
            let ids = list.iter().cloned().collect();
            self.request(team, Request::Presence(ids));
        }
    }

    pub(super) fn want_names(&mut self, team: &str, users: impl IntoIterator<Item = String>) {
        let wanted: Vec<String> = users
            .into_iter()
            .filter(|u| !u.is_empty())
            .filter(|u| {
                let key = format!("{team}:{u}");
                !self.names.contains_key(&key) && self.asked.insert(key)
            })
            .collect();
        if !wanted.is_empty() {
            self.request(team, Request::Users(wanted));
        }
    }

    pub(super) fn request(&self, team: &str, request: Request) {
        self.net.send(Cmd::Request {
            team: team.to_owned(),
            request,
        });
    }

    pub(super) fn put_conversation(&mut self, team: &str, c: &Value) {
        let Some(id) = c["id"].as_str() else { return };
        let kind = if c["is_im"] == true {
            Kind::Direct(c["user"].as_str().unwrap_or_default().to_owned())
        } else if c["is_mpim"] == true {
            Kind::Group
        } else {
            Kind::Channel
        };
        // Group DMs are named `mpdm-ada--bea--cy-1`: the people, until names come.
        let name = match &kind {
            Kind::Group => c["purpose"]["value"]
                .as_str()
                .filter(|p| p.starts_with("Group messaging with:"))
                .map(|p| p.trim_start_matches("Group messaging with:").trim().replace('@', ""))
                .unwrap_or_else(|| {
                    c["name"]
                        .as_str()
                        .unwrap_or_default()
                        .trim_start_matches("mpdm-")
                        .rsplit_once('-')
                        .map_or("Group", |(n, _)| n)
                        .replace("--", ", ")
                }),
            _ => c["name"].as_str().unwrap_or_default().to_owned(),
        };
        let ws = self.workspaces.entry(team.to_owned()).or_default();
        let old = ws.conversations.remove(id);
        ws.conversations.insert(
            id.to_owned(),
            Conversation {
                kind,
                name,
                latest: old.as_ref().map(|o| o.latest.clone()).unwrap_or_default(),
                last_read: c["last_read"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| old.as_ref().map(|o| o.last_read.clone()))
                    .unwrap_or_default(),
                mentions: old.as_ref().map_or(0, |o| o.mentions),
                has_unreads: old.as_ref().is_some_and(|o| o.has_unreads),
            },
        );
    }

    /// `client.counts`: `channels`, `mpims` and `ims`, each `{id, last_read, latest, mention_count, has_unreads}`.
    pub(super) fn put_counts(&mut self, team: &str, counts: &Value) {
        let Some(ws) = self.workspaces.get_mut(team) else {
            return;
        };
        for list in ["channels", "mpims", "ims"] {
            for c in counts[list].as_array().into_iter().flatten() {
                let Some(conv) = c["id"].as_str().and_then(|id| ws.conversations.get_mut(id)) else {
                    continue;
                };
                if let Some(r) = c["last_read"].as_str() {
                    conv.last_read = r.to_owned();
                }
                if let Some(l) = c["latest"].as_str() {
                    conv.latest = l.to_owned();
                }
                conv.mentions = c["mention_count"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(0);
                conv.has_unreads = c["has_unreads"] == true;
            }
        }
    }

    pub(super) fn room(&self, team: &str, id: &str) -> Option<Room> {
        let ws = self.workspaces.get(team)?;
        let c = ws.conversations.get(id)?;
        let account = self.account.clone()?;
        let (name, kind) = match &c.kind {
            Kind::Direct(with) => (
                self.name(team, with),
                RoomKind::Direct {
                    with: UserId::new(account.clone(), format!("{team}:{with}").as_str()),
                },
            ),
            Kind::Group => (c.name.clone(), RoomKind::Group),
            Kind::Channel => (
                c.name.clone(),
                RoomKind::Channel {
                    server: ServerId::new(account.clone(), team),
                },
            ),
        };
        let mut capabilities = Capabilities::of(Network::Slack);
        capabilities.threads = true;
        // A DM shows the other person's picture.
        let icon = match &c.kind {
            Kind::Direct(with) => self.pictures.get(&format!("{team}:{with}")).cloned(),
            _ => None,
        };
        Some(Room {
            notify: Default::default(),
            id: RoomId::new(account, format!("{team}:{id}").as_str()),
            name: name.into(),
            kind,
            icon_url: icon.map(Into::into),
            capabilities,
            encryption: RoomEncryption::Unencrypted,
            unread: c.has_unreads || (!c.latest.is_empty() && newer(&c.latest, &c.last_read)),
            mentions: c.mentions,
            last_read: (!c.last_read.is_empty()).then(|| MessageId(c.last_read.as_str().into())),
            invited_by: None,
            presence: None,
            status: None,
            member_count: None,
            last_activity: ts_ms(&c.latest),
            parent: None,
            joined_thread: false,
        })
    }

    pub(super) fn all_rooms(&self) -> Vec<Room> {
        let mut out = Vec::new();
        for team in &self.teams {
            let Some(ws) = self.workspaces.get(team) else { continue };
            let mut ids: Vec<&String> = ws.conversations.keys().collect();
            ids.sort();
            for id in ids {
                out.extend(self.room(team, id));
            }
            // Every thread known (a post's link opens it); the sidebar lists
            // only the newest few under each channel (`servers`).
            let mut threads: Vec<&(String, String)> = ws.threads.keys().collect();
            threads.sort();
            out.extend(threads.into_iter().filter_map(|(c, ts)| self.thread_room(team, c, ts)));
        }
        out
    }

    /// Each workspace a server: its channels by name.
    pub(super) fn servers(&self) -> Vec<Server> {
        let Some(account) = self.account.clone() else {
            return Vec::new();
        };
        self.teams
            .iter()
            .filter_map(|team| {
                let ws = self.workspaces.get(team)?;
                let mut channels: Vec<(&String, &Conversation)> = ws
                    .conversations
                    .iter()
                    .filter(|(_, c)| c.kind == Kind::Channel)
                    .collect();
                channels.sort_by(|a, b| a.1.name.cmp(&b.1.name));
                Some(Server {
                    id: ServerId::new(account.clone(), team.as_str()),
                    name: ws.name.as_str().into(),
                    icon_url: ws.icon.as_deref().map(Into::into),
                    banner_url: None,
                    scheduled_events: false,
                    scheduled_event_count: 0,
                    // Each channel, then its threads, the most recently active first.
                    categories: {
                        let mut relevant: Vec<_> = ws.threads.iter().filter(|(_, t)| t.subscribed).collect();
                        relevant.sort_by(|a, b| ts_ms(&b.1.latest).cmp(&ts_ms(&a.1.latest)).then_with(|| a.0.cmp(b.0)));
                        let recent = Category {
                            id: None,
                            can_manage: false,
                            title: "Threads".into(),
                            rooms: relevant
                                .into_iter()
                                .map(|((channel, root), _)| {
                                    RoomId::new(account.clone(), format!("{team}:{channel}:{root}").as_str())
                                })
                                .collect(),
                        };
                        let channels = Category {
                            id: None,
                            can_manage: false,
                            title: "".into(),
                            rooms: channels
                                .into_iter()
                                .flat_map(|(id, _)| {
                                    std::iter::once(format!("{team}:{id}"))
                                        .chain(
                                            self.shown_threads(team, id)
                                                .into_iter()
                                                .map(|ts| format!("{team}:{id}:{ts}")),
                                        )
                                        .map(|raw| RoomId::new(account.clone(), raw.as_str()))
                                        .collect::<Vec<_>>()
                                })
                                .collect(),
                        };
                        if recent.rooms.is_empty() {
                            vec![channels]
                        } else {
                            vec![recent, channels]
                        }
                    },
                    muted: false,
                    folder: None,
                    rights: ServerRights::default(),
                })
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

    /// Where a message shows: its thread's room for a reply in a channel's
    /// thread, else its conversation's (with whether it's in a thread).
    pub(super) fn place(&self, team: &str, channel: &str, m: &Value) -> (Option<RoomId>, bool) {
        let ts = m["ts"].as_str().unwrap_or_default();
        let is_channel = self
            .workspaces
            .get(team)
            .and_then(|w| w.conversations.get(channel))
            .is_some();
        match m["thread_ts"].as_str() {
            Some(root) if is_channel && root != ts && m["subtype"] != "thread_broadcast" => (
                self.account
                    .clone()
                    .map(|a| RoomId::new(a, format!("{team}:{channel}:{root}").as_str())),
                true,
            ),
            _ => (self.room_id(team, channel), false),
        }
    }
}
