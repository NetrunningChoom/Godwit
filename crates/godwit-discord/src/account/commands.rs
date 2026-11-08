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
            Answer::Profile { user, result } => {
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
                    self.profiles.insert(user, v);
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
