// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Commands from the app.

use super::*;

impl DiscordAccount {
    /// A search of `room` as Discord takes it: the server (None for a DM),
    /// the channel it was opened in, and the query's parameters.
    pub(super) fn search_request(
        &self,
        room: &RoomId,
        query: &godwit_core::search::SearchQuery,
        offset: usize,
    ) -> Option<(Option<String>, String, crate::search::SearchParams)> {
        let channel = self.channel_of(room)?;
        let (guild, nsfw) = self
            .store
            .channels
            .get(&channel)
            .map_or((None, false), |c| (c.guild.clone(), c.nsfw));
        let in_channel = query.in_room.as_ref().and_then(|r| self.channel_of(r));
        let params =
            crate::search::SearchParams::from_query(query, guild.is_some(), &channel, in_channel, nsfw, offset);
        Some((guild, channel, params))
    }

    pub(super) fn answer(&mut self, answer: Answer, out: &mut Vec<Event>) {
        match answer {
            Answer::Folders(result) => {
                if let Err(error) = result {
                    out.push(Event::Error(format!("Discord, saving server folders: {error}").into()));
                }
            }
            Answer::Soundboard { channel, result } => {
                let Some(call) = self.call.as_ref().filter(|call| call.channel == channel) else {
                    return;
                };
                let room = call.room.clone();
                match result {
                    Ok(values) => {
                        let sounds = crate::soundboard::parse_all(&serde_json::Value::Array(values));
                        self.soundboard_defaults = sounds.clone();
                        self.soundboard_known = sounds.clone();
                        out.push(Event::CallSounds { room, sounds });
                    }
                    Err(error) => {
                        self.soundboard_requested = false;
                        out.push(Event::Error(format!("Discord soundboard: {error}").into()));
                    }
                }
            }
            Answer::GuildSoundboard { channel, result } => {
                let Some(call) = self.call.as_ref().filter(|call| call.channel == channel) else {
                    return;
                };
                let room = call.room.clone();
                match result {
                    Ok(values) => {
                        let mut sounds = self.soundboard_defaults.clone();
                        sounds.extend(crate::soundboard::parse_all(&serde_json::Value::Array(values)));
                        self.soundboard_known = sounds.clone();
                        out.push(Event::CallSounds { room, sounds });
                    }
                    Err(error) => {
                        self.soundboard_requested = false;
                        out.push(Event::Error(format!("Discord soundboard: {error}").into()));
                    }
                }
            }
            Answer::AuditLog { guild, before, result } => {
                let Some(account) = self.account.clone() else { return };
                let server = ServerId::new(account, guild.as_str());
                let parsed = result
                    .map(|value| parse_audit_log(&value))
                    .map_err(|e| e.to_string().into());
                out.push(Event::ModerationLog {
                    server,
                    before: before.map(Into::into),
                    result: parsed,
                });
            }
            Answer::ScheduledEvents { guild, result } => {
                let Some(account) = self.account.clone() else { return };
                let server = ServerId::new(account, guild.as_str());
                match result {
                    Ok(value) => {
                        let events = value
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|v| self.parse_server_event(&guild, v))
                            .collect::<Vec<_>>();
                        self.scheduled_events.insert(guild.clone(), events.clone());
                        self.sync_servers(out);
                        out.push(Event::ServerEvents {
                            server,
                            result: Ok(events),
                            load_response: true,
                        });
                    }
                    Err(error) => out.push(Event::ServerEvents {
                        server,
                        result: Err(error.to_string().into()),
                        load_response: true,
                    }),
                }
            }
            Answer::ScheduledEventInterest {
                guild,
                event,
                interested,
                result,
            } => match result {
                Ok(()) => {
                    self.scheduled_event_membership
                        .insert((guild.clone(), event.clone()), interested);
                    if let Some(entry) = self
                        .scheduled_events
                        .get_mut(&guild)
                        .and_then(|events| events.iter_mut().find(|e| e.id.as_ref() == event))
                        && entry.interested != interested
                    {
                        entry.interested = interested;
                        entry.interested_count = if interested {
                            entry.interested_count.saturating_add(1)
                        } else {
                            entry.interested_count.saturating_sub(1)
                        };
                    }
                }
                Err(error) => {
                    godwit_core::verbose!("discord: setting scheduled event RSVP failed: {error}");
                    if let (Some(account), Some(events)) = (self.account.clone(), self.scheduled_events.get(&guild)) {
                        out.push(Event::ServerEvents {
                            server: ServerId::new(account, guild.as_str()),
                            result: Ok(events.clone()),
                            load_response: false,
                        });
                    }
                    out.push(Event::Error(format!("Discord, scheduled event RSVP: {error}").into()));
                }
            },
            Answer::Reactors {
                channel,
                message,
                emoji,
                result,
            } => {
                let Some(room) = self.room_id(&channel) else { return };
                let users = match result {
                    Ok(users) => users,
                    Err(e) => {
                        godwit_core::verbose!("discord: loading reaction users failed: {e}");
                        Value::Array(Vec::new())
                    }
                };
                let people = users
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|u| {
                        let id = u["id"].as_str()?;
                        let user = self.store.users.get(id);
                        Some(godwit_core::application::Person {
                            id: UserId::new(room.account.clone(), id),
                            name: self
                                .store
                                .display_name(id, self.store.channels.get(&channel).and_then(|c| c.guild.as_deref()))
                                .into(),
                            avatar_url: user.map(|u| crate::store::avatar_url(id, u.avatar.as_deref()).into()),
                            online: self.store.presences.get(id).is_some_and(|status| status != "offline"),
                        })
                    })
                    .collect();
                out.push(Event::Reactors {
                    room,
                    message: MessageId(message.into()),
                    emoji: emoji.into(),
                    people,
                });
            }
            Answer::History {
                channel,
                before,
                around,
                result,
            } => {
                let Some(room) = self.room_id(&channel) else { return };
                match result {
                    Ok(page) => {
                        for m in &page {
                            if let Some(a) = m.get("author") {
                                self.store.put_user(a);
                            }
                        }
                        // A short page is the channel's start.
                        let reached_start = page.len() < crate::api::PAGE;
                        self.store.note_history(&page);
                        let mut messages: Vec<Message> = page.iter().filter_map(|m| self.message(m)).collect();
                        for m in &messages {
                            self.remember_reactions(&channel, m);
                        }
                        // Discord answers newest first; the core wants oldest first.
                        messages.sort_by(|a, b| (a.id.0.len(), &a.id.0).cmp(&(b.id.0.len(), &b.id.0)));
                        if let Some(around) = around {
                            out.push(Event::HistoryAround {
                                room,
                                around: MessageId(around.into()),
                                messages,
                            });
                        } else {
                            out.push(Event::History {
                                room,
                                before: before.map(|b| MessageId(b.into())),
                                messages,
                                reached_start,
                            });
                        }
                    }
                    Err(e) => {
                        godwit_core::verbose!("discord: loading messages failed: {e}");
                        out.push(Event::HistoryFailed { room });
                    }
                }
            }
            Answer::Sent { channel, nonce, result } => {
                let Some(room) = self.room_id(&channel) else { return };
                match result {
                    Ok(m) => {
                        // MESSAGE_CREATE may have confirmed it already.
                        if let Some(core) = self.pending.remove(&nonce)
                            && let Some(message) = self.message(&m)
                        {
                            out.push(Event::MessageConfirmed {
                                room,
                                nonce: core,
                                message,
                            });
                        }
                    }
                    Err(e) => {
                        if let Some(core) = self.pending.remove(&nonce) {
                            out.push(Event::MessageFailed {
                                room,
                                nonce: core,
                                error: e.into(),
                            });
                        }
                    }
                }
            }
            Answer::Done { what, result } => {
                if let Err(e) = result {
                    // Typing notices failing isn't worth a problem report.
                    if what != "typing" {
                        out.push(Event::Error(format!("Discord, {what}: {e}").into()));
                    }
                }
            }
            Answer::ChannelNotify {
                guild,
                previous,
                result,
            } => {
                if let Err(e) = result {
                    self.apply_override(guild.as_deref(), &previous);
                    if let Some(room) = previous["channel_id"].as_str().and_then(|c| self.room(c)) {
                        out.push(Event::RoomUpdated(room));
                    }
                    out.push(Event::Error(format!("Discord, notification setting: {e}").into()));
                }
            }
            Answer::DmOpened(result) => match result {
                Ok(c) => {
                    for u in c["recipients"].as_array().into_iter().flatten() {
                        self.store.put_user(u);
                    }
                    self.store.put_channel(&c);
                    self.sync_rooms(out);
                    if std::mem::take(&mut self.opening_dm)
                        && let Some(room) = c["id"].as_str().and_then(|id| self.room_id(id))
                    {
                        out.push(Event::RoomOpened(room));
                    }
                }
                Err(e) => {
                    self.opening_dm = false;
                    out.push(Event::Error(format!("Discord, opening a DM: {e}").into()));
                }
            },
            Answer::Sessions(result) => {
                if let Some(account) = self.account.clone() {
                    let view = match result {
                        Ok(v) => godwit_core::privacy::SessionsView {
                            sessions: sessions_of(&v),
                            ..Default::default()
                        },
                        Err(e) => godwit_core::privacy::SessionsView {
                            error: Some(e.into()),
                            ..Default::default()
                        },
                    };
                    out.push(Event::Sessions { account, view });
                }
            }
            Answer::Consents(result) => match result {
                Ok(v) => {
                    self.consents = Some(
                        CONSENTS
                            .iter()
                            .map(|(key, _, _)| ((*key).to_owned(), v[*key]["consented"].as_bool().unwrap_or(false)))
                            .collect(),
                    );
                    self.send_privacy(false, None, out);
                }
                Err(error) => self.send_privacy(false, Some(error.into()), out),
            },
            Answer::ProfileSaved(result) => match result {
                // Read back what Discord kept, as the official client does.
                Ok(()) => {
                    if let Some(me) = self.store.me.as_ref().map(|m| m.id.clone()) {
                        self.own_profile = Some(true);
                        self.net.send(Cmd::Request(Request::Profile { user: me, guild: None }));
                    }
                }
                Err(error) => {
                    if let Some(account) = self.account.clone() {
                        out.push(Event::OwnProfileSaved {
                            account,
                            result: Err(error.into()),
                        });
                    }
                }
            },
            Answer::ServerProfile { guild, result } => {
                if let Some(account) = self.account.clone() {
                    let result = result
                        .map(|value| server_profile_of(&value, &guild, &self.store))
                        .map_err(Into::into);
                    let server = ServerId::new(account, guild.clone());
                    if self.server_profile_saved.remove(&guild) {
                        out.push(Event::ServerProfileSaved { server, result });
                    } else {
                        out.push(Event::ServerProfileLoaded { server, result });
                    }
                }
            }
            Answer::ServerProfileSaved { guild, result } => match result {
                Ok(()) => {
                    self.server_profile_saved.insert(guild.clone());
                    self.net.send(Cmd::Request(Request::LoadServerProfile {
                        user: self.me().to_owned(),
                        guild,
                    }));
                }
                Err(error) => {
                    if let Some(account) = self.account.clone() {
                        out.push(Event::ServerProfileSaved {
                            server: ServerId::new(account, guild),
                            result: Err(error.into()),
                        });
                    }
                }
            },
            Answer::Note { user, result } => {
                // A failed fetch leaves the field off (asked again on the next
                // open); a late answer never overwrites a newer save or update.
                if let Ok(note) = result
                    && !self.notes.contains_key(&user)
                {
                    self.set_note(&user, &note);
                    self.sync_profile(out);
                }
            }
            Answer::NoteSaved { user, previous, result } => {
                if let Err(e) = result {
                    self.set_note(&user, &previous);
                    self.sync_profile(out);
                    out.push(Event::Error(format!("Discord, saving a note: {e}").into()));
                }
            }
            Answer::Profile { user, guild, result } => {
                let mine = self.store.me.as_ref().is_some_and(|m| m.id == user);
                if mine
                    && let Some(saved) = self.own_profile.take()
                    && let Some(account) = self.account.clone()
                {
                    let result = match &result {
                        Ok(v) => Ok(own_profile_of(v, &self.store, self.avatar(&user, None))),
                        Err(e) => Err(e.to_string().into()),
                    };
                    out.push(if saved {
                        Event::OwnProfileSaved { account, result }
                    } else {
                        Event::OwnProfileLoaded { account, result }
                    });
                }
                if let Ok(v) = result {
                    if let Some(u) = v.get("user") {
                        self.store.put_user(u);
                    }
                    // Only what the card needs is kept (the raw answer holds much
                    // more), and the caches stay small: the shown profile and a
                    // few recent ones; the rest is asked for again when reopened.
                    if self.profiles.len() >= PROFILE_CACHE {
                        let shown = self.profile_for.as_ref().map(|p| (p.0.clone(), p.1.clone()));
                        self.profiles.retain(|k, _| Some(k) == shown.as_ref());
                        self.notes.retain(|k, _| shown.as_ref().is_some_and(|s| &s.0 == k));
                    }
                    self.profiles.insert((user, guild), slim_profile(&v));
                    self.sync_profile(out);
                }
            }
            Answer::FriendRequest { request, result } => {
                if let Some(account) = self.account.clone() {
                    out.push(Event::FriendRequestSent {
                        account,
                        request,
                        result: result.map_err(Into::into),
                    });
                }
            }
            Answer::RequestAnswered {
                channel,
                accept,
                result,
            } => match result {
                Ok(()) => {
                    if accept {
                        if let Some(c) = self.store.channels.get_mut(&channel) {
                            c.request = false;
                        }
                    } else {
                        self.store.remove_channel(&channel);
                    }
                    self.sync_rooms(out);
                }
                Err(e) => out.push(Event::Error(format!("Discord message request: {e}").into())),
            },
            answer @ (Answer::ChannelSaved { .. } | Answer::ChannelDeleted { .. } | Answer::OverwriteSaved { .. }) => {
                self.channel_answer(answer, out)
            }
            Answer::ThreadCreated(result) => match result {
                // Opened at once; THREAD_CREATE will say the same again.
                Ok(thread) => {
                    self.store.put_channel(&thread);
                    self.sync_rooms(out);
                    self.sync_servers(out);
                    if let Some(room) = thread["id"].as_str().and_then(|id| self.room_id(id)) {
                        out.push(Event::RoomOpened(room));
                    }
                }
                Err(e) => out.push(Event::Error(format!("Couldn't start the thread: {e}").into())),
            },
            Answer::ThreadMembership { channel, join, result } => match result {
                Ok(()) => {
                    if let Some(thread) = self.store.channels.get_mut(&channel) {
                        thread.joined = join;
                    }
                    self.sync_rooms(out);
                }
                Err(e) => out.push(Event::Error(
                    format!("Discord, {} thread: {e}", if join { "joining" } else { "leaving" }).into(),
                )),
            },
            Answer::ForumThreads {
                channel,
                sort,
                tag,
                offset,
                result,
            } => self.forum_older(&channel, sort, tag, offset, result, out),
            Answer::Search {
                channel,
                query,
                offset,
                result,
            } => {
                let Some(room) = self.room_id(&channel) else { return };
                if let Err(e) = &result {
                    out.push(Event::Error(format!("Discord search: {e}").into()));
                }
                let mut notice = None;
                let (mut messages, mut total, mut next_offset) = (None, None, None);
                let mut rooms = Vec::new();
                if let Ok(v) = &result {
                    // 202 with `retry_after` while Discord indexes a chat
                    // for search: say so, rather than "no results". Not
                    // retried by itself (no polling); searching again will.
                    match crate::search::page(v, offset) {
                        crate::search::Page::Indexing(secs) => {
                            notice = Some(format!("Search is indexing, try again in {secs}s"));
                        }
                        crate::search::Page::Found {
                            groups,
                            total: t,
                            next_offset: n,
                        } => {
                            // Each group holds a match; it's the one marked
                            // `hit`, or the first.
                            let hits: Vec<(RoomId, Message)> = groups
                                .iter()
                                .filter_map(|group| {
                                    let list = group.as_array()?;
                                    let hit = list
                                        .iter()
                                        .find(|m| m["hit"].as_bool() == Some(true))
                                        .or(list.first())?;
                                    // A server-wide search finds other channels.
                                    let at = hit["channel_id"].as_str().and_then(|c| self.room_id(c));
                                    Some((at.unwrap_or_else(|| room.clone()), self.message(hit)?))
                                })
                                .collect();
                            if hits.iter().any(|(r, _)| *r != room) {
                                rooms = hits.iter().map(|(r, _)| r.clone()).collect();
                            }
                            messages = Some(hits.into_iter().map(|(_, m)| m).collect());
                            (total, next_offset) = (t, n);
                        }
                    }
                }
                out.push(Event::SearchResults {
                    room,
                    query: *query,
                    rooms,
                    offset,
                    next_offset,
                    total,
                    notice,
                    messages,
                });
            }
            Answer::Pins { channel, result } => {
                let Some(room) = self.room_id(&channel) else { return };
                if let Err(e) = &result {
                    out.push(Event::Error(format!("Discord pins: {e}").into()));
                }
                let messages = result
                    .ok()
                    .map(|list| list.iter().filter_map(|m| self.message(m)).collect());
                out.push(Event::Pins { room, messages });
            }
            Answer::GifCategories(result) => {
                let Some(account) = self.account.clone() else { return };
                let categories = result
                    .ok()
                    .and_then(|v| v["categories"].as_array().cloned())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|c| {
                        Some(godwit_core::gifs::GifCategory {
                            title: c["name"].as_str()?.into(),
                            image: c["src"].as_str()?.into(),
                        })
                    })
                    .collect();
                out.push(Event::GifCategories { account, categories });
            }
            Answer::Gifs { query, result } => {
                let Some(account) = self.account.clone() else { return };
                match result {
                    Ok(v) => out.push(Event::Gifs {
                        account,
                        query,
                        more: false,
                        results: gifs(&v),
                        next: None,
                    }),
                    Err(error) => out.push(Event::GifsFailed {
                        account,
                        query,
                        error: error.into(),
                    }),
                }
            }
            Answer::Invite { code, result } => {
                let Some(account) = self.account.clone() else { return };
                let preview = result.ok().map(|v| InvitePreview {
                    server: v["guild"]["id"]
                        .as_str()
                        .filter(|g| self.store.guilds.contains_key(*g))
                        .map(|g| ServerId::new(account.clone(), g)),
                    name: v["guild"]["name"].as_str().unwrap_or("Discord server").into(),
                    icon_url: v["guild"]["id"]
                        .as_str()
                        .zip(v["guild"]["icon"].as_str())
                        .map(|(g, h)| store::guild_icon_url(g, h).into()),
                    members: v["approximate_member_count"].as_u64(),
                });
                out.push(Event::InviteLoaded {
                    account,
                    code: code.into(),
                    preview,
                });
            }
            Answer::Joined { code, result } => match result {
                Ok(v) => {
                    if let (Some(account), Some(g)) = (
                        self.account.clone(),
                        v["guild"]["id"].as_str().or_else(|| v["guild_id"].as_str()),
                    ) {
                        out.push(Event::InviteJoined {
                            account: account.clone(),
                            code: code.into(),
                            server: ServerId::new(account, g),
                        });
                    }
                }
                Err(e) => {
                    out.push(Event::Error(format!("Discord, joining: {e}").into()));
                    // As the core's JoinInvite asks: a failed join (an
                    // expired or used-up invite) reads as not valid, or the
                    // card stayed on "Joining…".
                    if let Some(account) = self.account.clone() {
                        out.push(Event::InviteLoaded {
                            account,
                            code: code.into(),
                            preview: None,
                        });
                    }
                }
            },
            Answer::Admin { guild, result } => self.admin_answer(&guild, result, out),
        }
    }
}

/// Discord audit-log event ids and their meanings follow its Audit Log Entry
/// object documentation: https://discord.com/developers/docs/resources/audit-log#audit-log-entry-object-audit-log-events
fn parse_audit_log(value: &Value) -> Vec<godwit_core::server_settings::ModerationLogEntry> {
    use godwit_core::server_settings::ModerationLogEntry;
    let users = value
        .get("users")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|u| {
            Some((
                u.get("id")?.as_str()?.to_owned(),
                (
                    u.get("global_name")
                        .or_else(|| u.get("username"))
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown user")
                        .to_owned(),
                    u.get("avatar").and_then(Value::as_str).map(|hash| {
                        format!(
                            "https://cdn.discordapp.com/avatars/{}/{}.png",
                            u.get("id").and_then(Value::as_str).unwrap_or(""),
                            hash
                        )
                    }),
                ),
            ))
        })
        .collect::<std::collections::HashMap<_, _>>();
    value
        .get("audit_log_entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?;
            let actor_id = entry.get("user_id").and_then(Value::as_str).unwrap_or("");
            let (actor, actor_avatar_url) = users
                .get(actor_id)
                .cloned()
                .unwrap_or_else(|| ("Unknown user".to_owned(), None));
            let action_type = entry.get("action_type").and_then(Value::as_u64).unwrap_or(0);
            let target_id = entry.get("target_id").and_then(Value::as_str);
            let mut target = target_id.map(|target| {
                users
                    .get(target)
                    .map(|u| u.0.clone())
                    .unwrap_or_else(|| target.to_owned())
            });
            let changes = entry
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            let named_change = changes.iter().find_map(|change| {
                let key = change.get("key")?.as_str()?;
                matches!(key, "name" | "nick")
                    .then(|| {
                        change.get("new_value").map(|value| match value {
                            Value::String(name) => name.clone(),
                            other => other.to_string(),
                        })
                    })
                    .flatten()
            });
            if target.is_none() {
                target = named_change.clone();
            }
            let (action, verb) = audit_action(action_type);
            let text = format!(
                "{}{}",
                verb,
                target.as_ref().map(|name| format!(" {name}")).unwrap_or_default()
            );
            let option_name = entry.get("options").and_then(Value::as_object).and_then(|options| {
                ["role_name", "channel_name", "name"]
                    .iter()
                    .find_map(|key| options.get(*key).and_then(Value::as_str))
            });
            if target.is_none() {
                target = option_name.map(str::to_owned);
            }
            let changed_name = changes
                .iter()
                .any(|change| change.get("key").and_then(Value::as_str) == Some("name"));
            let detail = match action_type {
                11 if changed_name => "name",
                31 => "role settings",
                _ => "",
            };
            let text = if detail.is_empty() {
                text
            } else {
                format!("{text} ({detail})")
            };
            Some(ModerationLogEntry {
                id: id.into(),
                time_ms: crate::api::snowflake_ms(id),
                actor: actor.into(),
                actor_avatar_url: actor_avatar_url.map(Into::into),
                action,
                text: text.into(),
                target: target.map(Into::into),
                reason: entry
                    .get("reason")
                    .and_then(Value::as_str)
                    .filter(|r| !r.is_empty())
                    .map(Into::into),
            })
        })
        .collect()
}

fn audit_action(code: u64) -> (godwit_core::server_settings::ModerationLogAction, &'static str) {
    use godwit_core::server_settings::ModerationLogAction as K;
    match code {
        20 => (K::Member, "kicked"),
        21 => (K::Member, "pruned members"),
        22 => (K::Member, "banned"),
        23 => (K::Member, "unbanned"),
        30 => (K::Role, "created role"),
        31 => (K::Role, "updated role"),
        32 => (K::Role, "deleted role"),
        10 => (K::Channel, "created channel"),
        11 => (K::Channel, "updated channel"),
        12 => (K::Channel, "deleted channel"),
        72 => (K::Message, "deleted message by"),
        _ => (K::Other, "performed server action"),
    }
}

/// How many fetched profiles are kept (and notes with them).
const PROFILE_CACHE: usize = 50;

/// The parts of a Get User Profile answer the card shows: the bio, the
/// banner, the mutual servers and the mutual friends (name and avatar
/// resolved here, so no user records pile up in the store).
fn slim_profile(v: &Value) -> Value {
    let friends: Vec<Value> = v["mutual_friends"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| {
            let u = store::User::from(u)?;
            let name = u
                .global_name
                .clone()
                .filter(|n| !n.is_empty())
                .unwrap_or(u.username.clone());
            Some(json!({"id": u.id, "name": name, "avatar": store::avatar_url(&u.id, u.avatar.as_deref())}))
        })
        .collect();
    json!({
        "user_profile": {"bio": v["user_profile"]["bio"], "pronouns": v["user_profile"]["pronouns"]},
        "guild_member_profile": v["guild_member_profile"],
        "guild_member": v["guild_member"],
        "user": {"bio": v["user"]["bio"], "banner": v["user"]["banner"]},
        "mutual_guilds": v["mutual_guilds"],
        "mutual_friends": friends,
    })
}

#[cfg(test)]
mod audit_log_tests {
    use super::*;
    use godwit_core::server_settings::ModerationLogAction as K;
    use serde_json::json;

    #[test]
    fn parses_entries_resolves_actors_and_maps_common_actions() {
        let raw = json!({
            "users": [{"id":"101","username":"mod","global_name":"Moderator","avatar":"hash"},{"id":"202","username":"Member"}],
            "audit_log_entries": [
                {"id":"175928847299117063","user_id":"101","target_id":"202","action_type":22,"reason":"rule breaking"},
                {"id":"175928847299117064","user_id":"101","target_id":"202","action_type":20},
                {"id":"175928847299117065","user_id":"101","target_id":"303","action_type":31},
                {"id":"175928847299117066","user_id":"101","target_id":"304","action_type":10},
                {"id":"175928847299117067","user_id":"101","target_id":"305","action_type":11,"changes":[{"key":"name"}]},
                {"id":"175928847299117068","user_id":"101","target_id":"306","action_type":12},
                {"id":"175928847299117069","user_id":"101","target_id":"202","action_type":72}
            ]
        });
        let entries = parse_audit_log(&raw);
        assert_eq!(entries.len(), 7);
        assert_eq!(entries[0].actor.as_ref(), "Moderator");
        assert!(entries[0].actor_avatar_url.is_some());
        assert_eq!(entries[0].text.as_ref(), "banned Member");
        assert_eq!(entries[0].reason.as_deref(), Some("rule breaking"));
        assert_eq!(entries[1].text.as_ref(), "kicked Member");
        assert_eq!(entries[2].action, K::Role);
        assert_eq!(entries[3].text.as_ref(), "created channel 304");
        assert_eq!(entries[4].text.as_ref(), "updated channel 305 (name)");
        assert_eq!(entries[5].text.as_ref(), "deleted channel 306");
        assert_eq!(entries[6].text.as_ref(), "deleted message by Member");
        assert_eq!(entries[0].time_ms, crate::api::snowflake_ms("175928847299117063"));
    }

    #[test]
    fn unknown_actions_remain_readable() {
        assert_eq!(audit_action(999), (K::Other, "performed server action"));
    }
}
