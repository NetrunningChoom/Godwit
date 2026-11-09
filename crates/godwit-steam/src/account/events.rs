// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Backend events.

use super::*;

impl SteamAccount {
    pub(super) fn handle(&mut self, ev: Ev, out: &mut Vec<Event>) {
        match ev {
            Ev::FriendRequest { request, result } => {
                if let (Some(account), Some(request)) = (self.account.clone(), request) {
                    out.push(Event::FriendRequestSent {
                        account,
                        request,
                        result: result.map_err(Into::into),
                    });
                } else if let Err(error) = result {
                    out.push(Event::Error(error.into()));
                }
            }
            Ev::Qr { url, scanned } => {
                self.status = Status::Qr {
                    url: Some(url),
                    scanned,
                };
            }
            Ev::NeedsCode(guard) => {
                let error = match &self.status {
                    Status::NeedsCode { error, .. } => error.clone(),
                    _ => None,
                };
                self.status = Status::NeedsCode {
                    hint: guard_hint(&guard),
                    error,
                };
            }
            Ev::NeedsApproval(guard) => {
                self.status = Status::NeedsApproval {
                    hint: guard_hint(&guard),
                };
            }
            Ev::LoginFailed(error) => {
                self.status = match &self.status {
                    // A wrong code: ask again, saying why.
                    Status::LoggingIn | Status::NeedsCode { .. } if error.contains("code") => Status::NeedsCode {
                        hint: "Enter the Steam Guard code.".into(),
                        error: Some(error),
                    },
                    _ => Status::SignedOut { error: Some(error) },
                };
            }
            Ev::SignedIn(saved) => {
                session::save(self.slot, &saved);
                self.device_name = saved.device_name.clone();
                self.status = Status::Connecting;
            }
            Ev::Connecting => {
                if self.account.is_none() {
                    self.status = Status::Connecting;
                }
            }
            Ev::Connected { me } => {
                self.me = Some(me);
                self.account = Some(AccountId::new(Network::Steam, me.to_string()));
                self.ready = false;
                if let Some(account) = self.account.clone() {
                    out.push(Event::Connection { account, online: true });
                }
            }
            Ev::ReadyDeadline => self.make_ready(out),
            Ev::Call { call, event } => self.on_call(call, event, out),
            Ev::IncomingCall { voice_chat, partner } => {
                self.ringing.insert(voice_chat, partner);
                // A friend without a chat yet: make it, so the call has a room to show in.
                if !self.chats.contains_key(&partner) {
                    self.chats.entry(partner).or_default();
                    if let Some(room) = self.room(partner) {
                        out.push(Event::RoomUpdated(room));
                    }
                }
                if let Some(room) = self.room_id(partner) {
                    let members = self.ringing_from(partner);
                    out.push(Event::VoiceOccupants {
                        room: room.clone(),
                        members,
                    });
                    out.push(Event::Alert {
                        from: None,
                        room,
                        kind: AlertKind::Call,
                        title: self.name_of(partner).into(),
                        body: "Calling you on Steam".into(),
                    });
                }
            }
            Ev::VoiceChatEnded { voice_chat } => {
                if let Some(partner) = self.ringing.remove(&voice_chat)
                    && let Some(room) = self.room_id(partner)
                {
                    out.push(Event::VoiceOccupants {
                        room: room.clone(),
                        members: Vec::new(),
                    });
                    out.push(Event::AlertDone { room });
                }
            }
            Ev::Disconnected { reason, fatal } => {
                // Calls ringing us went with the connection.
                for (_, partner) in std::mem::take(&mut self.ringing) {
                    if let Some(room) = self.room_id(partner) {
                        out.push(Event::VoiceOccupants {
                            room: room.clone(),
                            members: Vec::new(),
                        });
                        out.push(Event::AlertDone { room });
                    }
                }
                if fatal {
                    session::clear(self.slot);
                    if let Some(account) = self.account.take() {
                        out.push(Event::AccountLost {
                            account,
                            reason: reason.as_str().into(),
                        });
                    }
                    self.me = None;
                    self.my_name = None;
                    self.my_avatar = None;
                    self.my_state = None;
                    self.viewing = None;
                    self.profile_for = None;
                    self.typing_seq.clear();
                    self.typing_sent.clear();
                    self.ready = false;
                    self.friends.clear();
                    self.blocked.clear();
                    self.chats.clear();
                    let error = Some(reason).filter(|r| !r.is_empty());
                    self.status = Status::SignedOut { error };
                } else if let Some(account) = self.account.clone() {
                    out.push(Event::Connection { account, online: false });
                } else {
                    self.status = Status::Connecting;
                }
            }
            Ev::FriendsList { friends, incremental } => {
                if !incremental {
                    self.friends.retain(|id, _| friends.iter().any(|(f, _)| f == id));
                }
                for (id, relationship) in friends {
                    if relationship == 0 {
                        self.friends.remove(&id);
                    } else {
                        self.friends.entry(id).or_default().relationship = relationship;
                    }
                }
                self.send_friends(out);
            }
            Ev::Favourites(people) => {
                if let Some(account) = self.account.clone() {
                    let users = people
                        .into_iter()
                        .map(|a| UserId::new(account.clone(), crate::backend::steam_id(a).to_string()))
                        .collect();
                    out.push(Event::Favourites { account, users });
                }
            }
            Ev::Nicknames(nicknames) => {
                if let Some(account) = self.account.clone() {
                    let nicknames = nicknames
                        .into_iter()
                        .map(|(id, nickname)| {
                            (
                                UserId::new(account.clone(), crate::backend::steam_id(id).to_string()),
                                nickname,
                            )
                        })
                        .collect();
                    out.push(Event::Nicknames { account, nicknames });
                }
            }
            Ev::Library { steam_id, library } => {
                self.loading_libraries.remove(&steam_id);
                // A failure isn't kept, so opening the dialog again retries.
                if !matches!(library, godwit_core::games::Library::Failed(_)) {
                    self.libraries.insert(steam_id, library.clone());
                }
                if let Some(account) = self.account.clone() {
                    out.push(Event::GameLibrary {
                        user: UserId::new(account, steam_id.to_string()),
                        library,
                    });
                }
            }
            Ev::AppNames(names) => {
                let mut changed_rooms = Vec::new();
                self.app_names.extend(names);
                for (id, friend) in &mut self.friends {
                    let p = &mut friend.persona;
                    if p.game_name.is_none()
                        && let Some(name) = p.game_app.and_then(|a| self.app_names.get(&a))
                    {
                        p.game_name = Some(name.clone());
                        if self.chats.contains_key(id) {
                            changed_rooms.push(*id);
                        }
                    }
                }
                self.personas_changed(true, changed_rooms, out);
                self.games_named(out);
            }
            Ev::Personas(list) => {
                let mut changed_rooms = Vec::new();
                let mut changed_friends = false;
                let mut ask = Vec::new();
                for p in list {
                    if Some(p.steam_id) == self.me {
                        if p.name.is_some() {
                            self.my_name = p.name.clone();
                        }
                        if let Some(hash) = &p.avatar_hash {
                            self.my_avatar = Some(avatar_url(hash));
                        }
                        if p.state.is_some() {
                            self.my_state = p.state;
                            if let (Some(account), Some(presence)) = (self.account.clone(), p.state.map(presence)) {
                                out.push(Event::OwnPresence {
                                    account,
                                    presence,
                                    status: None,
                                });
                            }
                        }
                        if self.ready {
                            if let Some(account) = self.account.clone() {
                                out.push(Event::AccountIdentity {
                                    account,
                                    display_name: self.my_name.clone().unwrap_or_else(|| "Steam".into()).into(),
                                    username: self.my_name.clone().unwrap_or_default().into(),
                                    avatar_url: self.my_avatar.clone().map(Into::into),
                                });
                            }
                        } else {
                            self.make_ready(out);
                        }
                        if self.profile_for == self.me
                            && let Some(profile) = self.me.and_then(|id| self.profile(id))
                        {
                            out.push(Event::Profile(profile));
                        }
                        continue;
                    }
                    let friend = self.friends.entry(p.steam_id).or_default();
                    merge(&mut friend.persona, p.clone());
                    // A Steam game: its name from the lookup, or look it up.
                    if friend.persona.game_name.is_none()
                        && let Some(app) = friend.persona.game_app
                    {
                        match self.app_names.get(&app) {
                            Some(name) => friend.persona.game_name = Some(name.clone()),
                            None if self.asked_apps.insert(app) => ask.push(app),
                            None => {}
                        }
                    }
                    changed_friends = true;
                    if self.chats.contains_key(&p.steam_id) {
                        changed_rooms.push(p.steam_id);
                    }
                }
                if !ask.is_empty() {
                    self.net.send(Cmd::AppNames(ask));
                }
                self.personas_changed(changed_friends, changed_rooms, out);
            }
            Ev::Groups(list) => {
                self.on_groups(list, out);
                self.finish_join(out);
            }
            Ev::InvitePreview { code, group } => {
                let Some(account) = self.account.clone() else { return };
                let preview = group.map(|(g, members)| InvitePreview {
                    // A server card says "Joined" once we're in; a group DM's can't.
                    server: g
                        .is_server()
                        .then(|| ServerId::new(account.clone(), group_server_raw(g.id))),
                    name: if g.name.is_empty() {
                        "Group chat".into()
                    } else {
                        g.name.as_str().into()
                    },
                    icon_url: g.avatar_url.clone().map(Into::into),
                    members: Some(u64::from(members)).filter(|n| *n > 0),
                });
                out.push(Event::InviteLoaded {
                    account,
                    code: code.into(),
                    preview,
                });
            }
            Ev::GroupJoined { code, group, result } => match result {
                Ok(()) => {
                    self.joining = Some((code, group));
                    self.finish_join(out);
                }
                Err(e) => {
                    out.push(Event::Error(format!("Couldn't join the Steam group chat: {e}").into()));
                    if let Some(account) = self.account.clone() {
                        out.push(Event::InviteLoaded {
                            account,
                            code: code.into(),
                            preview: None,
                        });
                    }
                }
            },
            Ev::GroupIncoming {
                group,
                chat,
                line,
                mentioned,
            } => {
                log_unknown_markup(&line.text);
                let Some(me) = self.me else { return };
                let mine = backend::steam_id(line.from) == me;
                let viewing = self.viewing_group == Some((group, chat));
                let c = self.group_chats.entry((group, chat)).or_default();
                c.last = c.last.max(line.time);
                if !mine && !viewing {
                    c.unread = true;
                    if mentioned {
                        c.mentions += 1;
                    }
                }
                if viewing && !mine {
                    self.net.send(Cmd::GroupAck {
                        group,
                        chat,
                        time: line.time,
                    });
                }
                let sender = backend::steam_id(line.from);
                if !self.friends.get(&sender).is_some_and(|f| f.persona.name.is_some()) && !mine {
                    self.net.send(Cmd::RequestPersonas(vec![sender]));
                }
                let Some(room) = self.group_room(group, chat) else {
                    return;
                };
                let id = room.id.clone();
                out.push(Event::RoomUpdated(room));
                if let Some(message) = self.group_message(&id, &line) {
                    // Group DMs alert for every message; servers (busy
                    // communities) only when we're mentioned, as Steam's default.
                    let alert = !mine
                        && !viewing
                        && (mentioned || !self.groups.get(&group).is_some_and(GroupInfo::is_server))
                        && !message.text.trim().is_empty();
                    if alert {
                        out.push(Event::Alert {
                            from: self.account.clone().map(|a| UserId::new(a, sender.to_string())),
                            room: id.clone(),
                            kind: AlertKind::Message,
                            title: format!("{} · {}", self.name_of(sender), self.group_title(group)).into(),
                            body: shorten(&message.text).into(),
                        });
                    }
                    out.push(Event::MessageAdded { room: id, message });
                }
            }
            Ev::GroupSent {
                nonce,
                group,
                chat,
                line,
            } => {
                let Some(room) = self.group_room_id(group, chat) else {
                    return;
                };
                let c = self.group_chats.entry((group, chat)).or_default();
                c.last = c.last.max(line.time);
                if let Some(message) = self.group_message(&room, &line) {
                    out.push(Event::MessageConfirmed {
                        room,
                        nonce: nonce.into(),
                        message,
                    });
                }
            }
            Ev::GroupSendFailed {
                nonce,
                group,
                chat,
                error,
            } => {
                if let Some(room) = self.group_room_id(group, chat) {
                    out.push(Event::MessageFailed {
                        room,
                        nonce: nonce.into(),
                        error: error.into(),
                    });
                }
            }
            Ev::GroupHistory {
                group,
                chat,
                before,
                lines,
                more,
            } => {
                let Some(room) = self.group_room_id(group, chat) else {
                    return;
                };
                // Senders we don't know yet: ask for their names.
                let unknown: Vec<u64> = lines
                    .iter()
                    .map(|l| backend::steam_id(l.from))
                    .filter(|id| {
                        Some(*id) != self.me && !self.friends.get(id).is_some_and(|f| f.persona.name.is_some())
                    })
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                if !unknown.is_empty() {
                    self.net.send(Cmd::RequestPersonas(unknown));
                }
                let mut messages: Vec<Message> = lines.iter().filter_map(|l| self.group_message(&room, l)).collect();
                messages.sort_by_key(|m| m.sent_ms);
                out.push(Event::History {
                    room,
                    before: before.map(|(t, o)| message_id(t, o)),
                    messages,
                    reached_start: !more,
                });
            }
            Ev::GroupHistoryFailed { group, chat } => {
                if let Some(room) = self.group_room_id(group, chat) {
                    out.push(Event::HistoryFailed { room });
                }
            }
            Ev::Emoticons { emoticons, stickers } => {
                self.owned_emoticons = emoticons.iter().cloned().collect();
                if let Some(account) = self.account.clone() {
                    let groups = emoji_groups(&account, &emoticons, &stickers);
                    out.push(Event::CustomEmoji { account, groups });
                }
            }
            Ev::Reaction {
                friend,
                time,
                ordinal,
                reactor,
                reaction,
                add,
            } => {
                let room = self.room_id(friend);
                self.update_reaction(room, (time, ordinal), reactor, reaction, add, out);
            }
            Ev::GroupReaction {
                group,
                chat,
                time,
                ordinal,
                reactor,
                reaction,
                add,
            } => {
                let room = self
                    .account
                    .clone()
                    .map(|account| RoomId::new(account, group_room_raw(group, chat)));
                self.update_reaction(room, (time, ordinal), reactor, reaction, add, out);
            }
            Ev::Sessions(sessions) => {
                for (account_id, last, viewed, unread) in sessions {
                    let chat = self.chats.entry(backend::steam_id(account_id)).or_default();
                    chat.last = chat.last.max(last);
                    chat.unread = unread > 0 && last > viewed;
                    chat.mentions = if chat.unread { unread } else { 0 };
                }
                if self.ready {
                    let rooms: Vec<Room> = self.chats.keys().copied().filter_map(|id| self.room(id)).collect();
                    out.extend(rooms.into_iter().map(Event::RoomUpdated));
                }
            }
            Ev::Incoming { friend, mine, line } => {
                log_unknown_markup(&line.text);
                let Some(room) = self.room_id(friend) else { return };
                let viewing = self.viewing == Some(friend);
                let chat = self.chats.entry(friend).or_default();
                let new_chat = chat.last == 0;
                chat.last = chat.last.max(line.time);
                if !mine && !viewing {
                    chat.unread = true;
                    chat.mentions = chat.mentions.saturating_add(1);
                }
                if viewing && !mine {
                    self.net.send(Cmd::Ack {
                        friend,
                        time: line.time,
                    });
                }
                if (new_chat || !mine)
                    && let Some(r) = self.room(friend)
                {
                    out.push(Event::RoomUpdated(r));
                }
                if let Some(message) = self.message(friend, &line) {
                    // Like Steam's client: a friend's message in a chat not on screen.
                    if !mine && !viewing && !message.text.trim().is_empty() {
                        out.push(Event::Alert {
                            from: self.account.clone().map(|a| UserId::new(a, friend.to_string())),
                            room: room.clone(),
                            kind: AlertKind::Message,
                            title: self.name_of(friend).into(),
                            body: shorten(&message.text).into(),
                        });
                    }
                    out.push(Event::MessageAdded { room, message });
                }
            }
            Ev::Typing { friend, typing, seq } => {
                let latest = self.typing_seq.entry(friend).or_default();
                if seq < *latest {
                    return;
                }
                *latest = seq;
                let Some(room) = self.room_id(friend) else { return };
                let names = if typing {
                    vec![self.name_of(friend).into()]
                } else {
                    Vec::new()
                };
                out.push(Event::Typing { room, names });
            }
            Ev::Sent { nonce, friend, line } => {
                let Some(room) = self.room_id(friend) else { return };
                self.chats.entry(friend).or_default().last = line.time;
                if let Some(message) = self.message(friend, &line) {
                    out.push(Event::MessageConfirmed {
                        room,
                        nonce: nonce.into(),
                        message,
                    });
                }
            }
            Ev::SendFailed { nonce, friend, error } => {
                if let Some(room) = self.room_id(friend) {
                    out.push(Event::MessageFailed {
                        room,
                        nonce: nonce.into(),
                        error: error.into(),
                    });
                }
            }
            Ev::History {
                friend,
                before,
                lines,
                more,
            } => {
                let Some(room) = self.room_id(friend) else { return };
                let mut messages: Vec<Message> = lines.iter().filter_map(|l| self.message(friend, l)).collect();
                // Steam answers newest first; the core wants oldest first.
                messages.sort_by_key(|m| m.sent_ms);
                out.push(Event::History {
                    room,
                    before: before.map(|(t, o)| message_id(t, o)),
                    messages,
                    reached_start: !more,
                });
            }
            Ev::Notice(text) => out.push(Event::Error(text.into())),
            Ev::NeedPersonas => {
                // Name and avatar arrive in separate persona updates. Recover
                // either missing field after the stream falls behind.
                let ids: Vec<u64> = self
                    .friends
                    .iter()
                    .filter(|(_, f)| f.persona.name.is_none() || f.persona.avatar_hash.is_none())
                    .map(|(id, _)| *id)
                    .collect();
                if !ids.is_empty() {
                    self.net.send(Cmd::RequestPersonas(ids));
                }
            }
            Ev::HistoryFailed { friend } => {
                if let Some(room) = self.room_id(friend) {
                    out.push(Event::HistoryFailed { room });
                }
            }
        }
    }
}
