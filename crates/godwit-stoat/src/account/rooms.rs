// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Rooms and servers: building them and keeping core in sync.

use super::*;

impl StoatAccount {
    /// Sends the rooms that changed since the last call.
    pub(super) fn sync_rooms(&mut self, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else {
            return;
        };
        let now = self.build_rooms(&account);
        self.rooms.sync(now, out);
    }

    pub(super) fn sync_servers(&mut self, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else {
            return;
        };
        let now = self.build_servers(&account);
        if now != self.servers {
            self.servers = now.clone();
            out.push(Event::ServersChanged { account, servers: now });
        }
    }

    /// Every room the user can see: notes, DMs and groups (most recent
    /// first, as officially), then server channels.
    pub(super) fn build_rooms(&self, account: &AccountId) -> Vec<Room> {
        let notes = self.store.saved_notes();
        let private = self.store.private_channels();
        let mut servers: Vec<&Channel> = self
            .store
            .channels
            .values()
            .filter(|c| c.server.is_some() && self.store.can(&c.id, VIEW_CHANNEL))
            .collect();
        servers.sort_by(|a, b| a.id.cmp(&b.id));
        notes
            .into_iter()
            .chain(private)
            .chain(servers)
            .map(|c| self.room(account, c))
            .collect()
    }

    pub(super) fn room(&self, account: &AccountId, c: &Channel) -> Room {
        let me = self.store.me.as_deref();
        let kind = match (c.channel_type.as_str(), &c.server) {
            (_, Some(server)) => RoomKind::Channel {
                server: ServerId::new(account.clone(), server.as_str()),
            },
            ("SavedMessages", None) => RoomKind::Notes,
            ("DirectMessage", None) => match c.recipients.iter().find(|r| Some(r.as_str()) != me) {
                Some(other) => RoomKind::Direct {
                    with: UserId::new(account.clone(), other.as_str()),
                },
                None => RoomKind::Group,
            },
            _ => RoomKind::Group,
        };
        let direct = match &kind {
            RoomKind::Direct { with } => Some(&*with.raw),
            _ => None,
        };
        // Worked out before `kind` moves into the room.
        let presence = direct.map(|u| self.presence(u));
        let status = direct.and_then(|u| self.status_text(u));
        let icon_url = match &kind {
            RoomKind::Direct { with } => Some(self.avatar_url(&with.raw, None)),
            _ => c.icon.as_ref().and_then(|f| self.file_url(f)),
        };
        let can = |p: u64| self.store.can(&c.id, p);
        let capabilities = Capabilities {
            delete_others: can(MANAGE_MESSAGES),
            // The pinned list is readable by anyone who reads the channel;
            // pinning takes Manage Messages in servers (anyone in DMs).
            pins: can(crate::permissions::READ_MESSAGE_HISTORY),
            search: can(crate::permissions::READ_MESSAGE_HISTORY),
            pin_messages: c.server.is_none() || can(MANAGE_MESSAGES),
            send: can(SEND_MESSAGE),
            react: can(REACT),
            attachments: can(UPLOAD_FILES),
            voice: (c.voice.is_some() || c.server.is_none()) && can(CONNECT),
            video: self.video_limit().is_some() && can(crate::permissions::VIDEO),
            video_resolution: self.video_limit().unwrap_or([1280, 720]),
            external_emoji: c.server.is_none() || can(USE_EXTERNAL_EMOJIS),
            // The official GIF button needs SendEmbeds.
            gifs: can(SEND_MESSAGE) && can(SEND_EMBEDS),
            ..Capabilities::of(Network::Stoat)
        };
        Room {
            id: RoomId::new(account.clone(), c.id.as_str()),
            name: self.store.channel_title(c).into(),
            kind,
            icon_url: icon_url.map(Into::into),
            capabilities,
            // Stoat has no end-to-end encryption.
            encryption: RoomEncryption::Unencrypted,
            unread: self.store.is_unread(&c.id),
            last_read: self.store.last_read.get(&c.id).map(|m| MessageId(m.as_str().into())),
            invited_by: None,
            presence,
            status,
            member_count: (c.channel_type == "Group").then(|| u32::try_from(c.recipients.len()).unwrap_or(u32::MAX)),
            last_activity: c.last_message_id.as_deref().map_or(0, ulid_ms),
            parent: None,
            mentions: u32::try_from(self.store.mention_count(&c.id)).unwrap_or(u32::MAX),
        }
    }

    /// Servers in the user's synced order; folders are flattened for now.
    pub(super) fn build_servers(&self, account: &AccountId) -> Vec<CoreServer> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.store
            .rail()
            .into_iter()
            .flat_map(|e| match e {
                RailEntry::Server(id) => vec![id],
                RailEntry::Folder { servers, .. } => servers,
            })
            .filter_map(|id| self.store.servers.get(&id))
            .map(|s| CoreServer {
                id: ServerId::new(account.clone(), s.id.as_str()),
                name: s.name.as_str().into(),
                icon_url: s.icon.as_ref().and_then(|f| self.file_url(f)).map(Into::into),
                banner_url: s.banner.as_ref().and_then(|f| self.file_url(f)).map(Into::into),
                categories: self
                    .store
                    .ordered_categories(s)
                    .into_iter()
                    .map(|cat| CoreCategory {
                        // The implicit first category has no heading, as officially.
                        title: if cat.id == "default" {
                            "".into()
                        } else {
                            cat.title.as_str().into()
                        },
                        rooms: cat
                            .channels
                            .iter()
                            .filter(|ch| self.store.can(ch, VIEW_CHANNEL))
                            .map(|ch| RoomId::new(account.clone(), ch.as_str()))
                            .collect(),
                    })
                    .filter(|cat| !cat.rooms.is_empty())
                    .collect(),
                muted: self.store.notification_prefs.mute(true, &s.id, now_ms).is_some(),
                rights: {
                    use crate::permissions as p;
                    let can = |bit| self.store.can_in_server(&s.id, bit);
                    godwit_core::server::ServerRights {
                        invite: can(p::INVITE_OTHERS),
                        create_channels: can(p::MANAGE_CHANNEL),
                        settings: can(p::MANAGE_SERVER) || can(p::MANAGE_ROLE) || can(p::BAN_MEMBERS),
                        owner: account.user.as_ref() == s.owner,
                        // The official client's levels (default "mention");
                        // a muted server hears nothing.
                        notify_levels: true,
                        notify: if self.store.notification_prefs.mute(true, &s.id, now_ms).is_some() {
                            2
                        } else {
                            match self.store.notification_prefs.server.get(&s.id).map(String::as_str) {
                                Some("all") => 0,
                                Some("none") => 2,
                                _ => 1,
                            }
                        },
                    }
                },
            })
            .collect()
    }
}
