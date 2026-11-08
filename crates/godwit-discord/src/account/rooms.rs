// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Rooms and servers: building them from the store and keeping core in sync.

use super::*;

/// What a Discord poll takes: question up to 300 characters, 2 to 10 answers
/// of up to 55: the web client's composer (docs.discord.food, Poll resource,
/// documents only the 10-answer cap and durations up to 32 days).
const POLL_LIMITS: godwit_core::poll::PollLimits = godwit_core::poll::PollLimits {
    question: 300,
    answer: 55,
    min_answers: 2,
    max_answers: 10,
};

impl DiscordAccount {
    pub(super) fn avatar(&self, user: &str, guild: Option<&str>) -> String {
        if let Some((g, hash)) = guild.and_then(|g| {
            let m = self.store.members.get(&(g.to_owned(), user.to_owned()))?;
            Some((g, m.avatar.clone()?))
        }) {
            return store::member_avatar_url(g, user, &hash);
        }
        let hash = self.store.users.get(user).and_then(|u| u.avatar.clone());
        store::avatar_url(user, hash.as_deref())
    }

    /// A voice channel's session start for the core: `value` is Unix seconds
    /// (the update event) or an ISO 8601 time (CHANNEL_INFO's documented
    /// type; its description says seconds, so both are read), null if none.
    pub(super) fn voice_since(&self, channel: &str, value: &Value) -> Option<Event> {
        let room = self.room_id(channel)?;
        let since_ms = value
            .as_i64()
            .or_else(|| value.as_str().and_then(|s| s.parse::<i64>().ok()))
            .map(|secs| secs.saturating_mul(1000))
            .or_else(|| {
                value
                    .as_str()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|t| t.timestamp_millis())
            })
            .filter(|ms| *ms > 0);
        Some(Event::VoiceSince { room, since_ms })
    }

    pub(super) fn room(&self, channel: &str) -> Option<Room> {
        let account = self.account.clone()?;
        let c = self.store.channels.get(channel)?;
        // Forums are rooms too (their posts are listed under them), but
        // archived threads aren't listed.
        if !(c.has_messages() || c.forum())
            || (c.thread() && c.archived && !self.store.opened_archived.contains(channel))
        {
            return None;
        }
        let perms = self.store.channel_permissions(channel);
        if perms & p::VIEW_CHANNEL == 0 {
            return None;
        }
        let names = |ids: &[String]| {
            ids.iter()
                .map(|u| self.store.display_name(u, None))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let (name, kind, icon) = match c.kind {
            store::DM => {
                let with = c.recipients.first().cloned().unwrap_or_default();
                (
                    self.store.display_name(&with, None),
                    RoomKind::Direct {
                        with: UserId::new(account.clone(), with.as_str()),
                    },
                    Some(self.avatar(&with, None)),
                )
            }
            store::GROUP_DM => (
                if c.name.is_empty() {
                    names(&c.recipients)
                } else {
                    c.name.clone()
                },
                RoomKind::Group,
                c.icon.as_deref().map(|h| store::group_icon_url(&c.id, h)),
            ),
            _ => (
                c.name.clone(),
                RoomKind::Channel {
                    server: ServerId::new(account.clone(), c.guild.clone().unwrap_or_default().as_str()),
                },
                None,
            ),
        };
        let can = |bit| perms & bit == bit;
        // A forum has nothing to write in (its posts do); threads take their own permission.
        let send = !c.forum()
            && can(if c.thread() {
                p::SEND_MESSAGES_IN_THREADS
            } else {
                p::SEND_MESSAGES
            });
        let capabilities = Capabilities {
            send,
            // Forwarding out: the server must allow it (its FORWARDING_DISABLED
            // feature) and we read the channel's history; in: we may send there
            // (Yapfast's `Ready::forwards_from`, docs.discord.food Message).
            forward_from: can(p::READ_MESSAGE_HISTORY)
                && !c
                    .guild
                    .as_ref()
                    .and_then(|g| self.store.guilds.get(g))
                    .is_some_and(|g| g.forwarding_disabled),
            forward_to: send,
            polls: (send && can(p::SEND_POLLS)).then_some(POLL_LIMITS),
            react: can(p::ADD_REACTIONS),
            // Other servers' emoji: Nitro only (and, in a server, the permission).
            external_emoji: self.store.premium_type != 0 && (c.private() || can(p::USE_EXTERNAL_EMOJIS)),
            attachments: can(p::ATTACH_FILES),
            delete_others: c.guild.is_some() && can(p::MANAGE_MESSAGES),
            // Voice channels we may connect to, and calls in DMs and group
            // DMs. Stage channels (speakers and an audience) aren't built:
            // they show, disabled, with a note.
            voice: c.private() || (c.kind == store::GUILD_VOICE && can(p::CONNECT)),
            // READY leaves defaults out, so a voice channel without a
            // user_limit has none (0, unlimited); a missing one isn't unknown.
            voice_user_limit: c.voice().then(|| c.user_limit.unwrap_or(0)),
            voice_unavailable: c.kind == store::GUILD_STAGE_VOICE,
            // Camera and Go Live: in DM calls, and in voice channels with
            // the Video permission. Discord's free quality only (720p30).
            video: c.private() || (c.kind == store::GUILD_VOICE && can(p::CONNECT) && can(p::STREAM)),
            video_resolution: [1280, 720],
            pins: can(p::READ_MESSAGE_HISTORY),
            // Servers hold only the members seen so far; the gateway finds
            // the rest by name (as the web client's mention search does).
            member_search: c.guild.is_some(),
            search: can(p::READ_MESSAGE_HISTORY),
            // A server's search also takes `in:` and the whole-server scope.
            search_filters: if c.guild.is_some() {
                godwit_core::search::SearchFilters::ALL
            } else {
                godwit_core::search::SearchFilters::DIRECT
            },
            // In DMs anyone may pin; in servers it takes the permission.
            pin_messages: c.private() || can(p::PIN_MESSAGES) || can(p::MANAGE_MESSAGES),
            // Starting threads (from a message, or a forum's post); not inside a thread.
            threads: !c.thread()
                && c.guild.is_some()
                && can(p::CREATE_PUBLIC_THREADS)
                && can(p::SEND_MESSAGES_IN_THREADS),
            forum: c.forum(),
            // The channel's settings and delete: Manage Channels on that
            // channel (threads have their own permissions).
            manage_channel: c.guild.is_some() && !c.thread() && can(p::MANAGE_CHANNELS),
            // Channel overrides in our guild settings; DMs take a mute only.
            notify_settings: !c.thread(),
            // The picker sends the GIF's link, which needs embedding.
            // Stickers: a server's own, in its channels (no Nitro, so no other
            // servers and no DMs); the same permission as any message.
            stickers: send && c.guild.is_some(),
            gifs: can(p::SEND_MESSAGES) && (c.guild.is_none() || can(p::EMBED_LINKS)),
            max_upload: upload_limit(
                self.store.premium_type,
                c.guild
                    .as_ref()
                    .and_then(|g| self.store.guilds.get(g))
                    .map(|g| g.premium_tier),
            ),
            ..Capabilities::of(Network::Discord)
        };
        let with = match &kind {
            RoomKind::Direct { with } => Some(with.raw.to_string()),
            _ => None,
        };
        Some(Room {
            notify: godwit_core::room::RoomNotify {
                level: self.store.channel_notify.get(channel).copied().filter(|l| *l <= 2),
                muted: self.store.channel_muted(channel),
            },
            id: RoomId::new(account, channel),
            name: name.into(),
            kind,
            icon_url: icon.map(Into::into),
            capabilities,
            encryption: RoomEncryption::Unencrypted,
            unread: self.store.unread(channel) && !self.store.channel_muted(channel),
            mentions: self.store.mentions(channel),
            last_read: self
                .store
                .read
                .get(channel)
                .and_then(|r| r.last_message_id.clone())
                .map(|m| MessageId(m.into())),
            // A message request: pending until accepted, nothing loaded before.
            invited_by: c
                .request
                .then(|| c.recipients.first().map(|u| self.store.display_name(u, None)))
                .flatten()
                .map(Into::into),
            presence: with.as_deref().map(|u| self.presence(u)),
            status: None,
            member_count: (c.kind == store::GROUP_DM)
                .then(|| u32::try_from(c.recipients.len() + 1).unwrap_or(u32::MAX)),
            // The newest message's time, from its snowflake (or the DM's own).
            last_activity: crate::api::snowflake_ms(c.last_message_id.as_deref().unwrap_or(&c.id)),
            parent: c.thread().then(|| c.parent.clone()).flatten().map(Into::into),
        })
    }

    /// DMs newest first (as Discord's list), then server channels.
    pub(super) fn all_rooms(&self) -> Vec<Room> {
        let mut private: Vec<&store::Channel> = self.store.channels.values().filter(|c| c.private()).collect();
        private.sort_by(|a, b| {
            let (x, y) = (
                b.last_message_id.as_deref().unwrap_or(&b.id),
                a.last_message_id.as_deref().unwrap_or(&a.id),
            );
            (x.len(), x).cmp(&(y.len(), y))
        });
        let mut out: Vec<Room> = private.iter().filter_map(|c| self.room(&c.id)).collect();
        for g in &self.store.guild_order {
            if let Some(guild) = self.store.guilds.get(g) {
                out.extend(guild.channels.iter().filter_map(|c| self.room(c)));
            }
        }
        out
    }

    pub(super) fn build_servers(&self) -> Vec<CoreServer> {
        let Some(account) = self.account.clone() else {
            return Vec::new();
        };
        self.store
            .guild_order
            .iter()
            .filter_map(|id| self.store.guilds.get(id))
            .map(|g| {
                // Channels and forums; threads are listed under their parent below.
                let visible = |c: &&store::Channel| {
                    (c.has_messages() || c.forum()) && !c.thread() && self.store.can(&c.id, p::VIEW_CHANNEL)
                };
                let channels: Vec<&store::Channel> =
                    g.channels.iter().filter_map(|c| self.store.channels.get(c)).collect();
                // Discord's order: text before voice, then by position.
                let order = |a: &&store::Channel, b: &&store::Channel| {
                    (a.voice(), a.position, &a.id).cmp(&(b.voice(), b.position, &b.id))
                };
                let mut loose: Vec<&store::Channel> = channels
                    .iter()
                    .copied()
                    .filter(|c| c.parent.is_none())
                    .filter(visible)
                    .collect();
                loose.sort_by(order);
                let mut categories: Vec<&store::Channel> = channels
                    .iter()
                    .copied()
                    .filter(|c| c.kind == store::GUILD_CATEGORY)
                    .collect();
                categories.sort_by_key(|c| (c.position, c.id.clone()));
                let room = |c: &store::Channel| RoomId::new(account.clone(), c.id.as_str());
                // A channel, then its open threads, newest first (as Discord's
                // sidebar lists the ones we've joined).
                let with_threads = |list: &[&store::Channel]| -> Vec<RoomId> {
                    let mut ids = Vec::new();
                    for c in list {
                        ids.push(room(c));
                        let mut threads: Vec<&store::Channel> = channels
                            .iter()
                            .copied()
                            .filter(|t| t.thread() && !t.archived && t.parent.as_deref() == Some(c.id.as_str()))
                            .filter(|t| self.store.can(&t.id, p::VIEW_CHANNEL))
                            .collect();
                        // Snowflakes order by (length, text); newest first.
                        let last = |t: &store::Channel| t.last_message_id.clone().unwrap_or_else(|| t.id.clone());
                        threads.sort_by_key(|t| {
                            let l = last(t);
                            std::cmp::Reverse((l.len(), l))
                        });
                        ids.extend(threads.iter().map(|t| room(t)));
                    }
                    ids
                };
                let mut out = vec![Category {
                    id: None,
                    can_manage: false,
                    title: "".into(),
                    rooms: with_threads(&loose),
                }];
                for cat in categories {
                    let mut inside: Vec<&store::Channel> = channels
                        .iter()
                        .copied()
                        .filter(|c| c.parent.as_deref() == Some(cat.id.as_str()))
                        .filter(visible)
                        .collect();
                    inside.sort_by(order);
                    // Manage Channels on the category itself (its overwrites
                    // count), as the official client's category menu.
                    let can_manage = self.store.can(&cat.id, p::MANAGE_CHANNELS);
                    let seen = can_manage && self.store.can(&cat.id, p::VIEW_CHANNEL);
                    let rooms = with_threads(&inside);
                    // An empty category stays for the people who run it, so
                    // channels can be made in it and it can be deleted.
                    if rooms.is_empty() && !seen {
                        continue;
                    }
                    out.push(Category {
                        id: Some(cat.id.as_str().into()),
                        can_manage,
                        title: cat.name.as_str().into(),
                        rooms,
                    });
                }
                out.retain(|c| c.id.is_some() || !c.rooms.is_empty());
                let perms = self.store.guild_permissions(&g.id);
                let has = |bit| perms & bit == bit;
                CoreServer {
                    id: ServerId::new(account.clone(), g.id.as_str()),
                    name: g.name.as_str().into(),
                    icon_url: g.icon.as_deref().map(|h| store::guild_icon_url(&g.id, h).into()),
                    banner_url: g.banner.as_deref().map(|h| store::guild_banner_url(&g.id, h).into()),
                    categories: out,
                    muted: self.store.muted_guilds.contains(&g.id),
                    folder: self.store.folder_of.get(&g.id).cloned(),
                    rights: ServerRights {
                        invite: has(p::CREATE_INSTANT_INVITE),
                        create_channels: has(p::MANAGE_CHANNELS),
                        // Create Guild Channel (type 4) and Modify Guild
                        // Channel Positions both need Manage Channels.
                        manage_categories: has(p::MANAGE_CHANNELS),
                        reorder_channels: has(p::MANAGE_CHANNELS),
                        settings: has(p::MANAGE_GUILD) || has(p::MANAGE_ROLES) || has(p::BAN_MEMBERS),
                        owner: g.owner == self.me(),
                        notify_levels: true,
                        notify: self.store.notify_level(&g.id),
                        report: false,
                    },
                }
            })
            .collect()
    }

    pub(super) fn sync_rooms(&mut self, out: &mut Vec<Event>) {
        let now = self.all_rooms();
        self.rooms.sync(now, out);
    }

    pub(super) fn sync_servers(&mut self, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else { return };
        let now = self.build_servers();
        if now != self.servers {
            self.servers = now.clone();
            out.push(Event::ServersChanged { account, servers: now });
        }
    }
}
