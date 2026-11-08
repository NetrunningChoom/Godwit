// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Commands from the app.

use super::*;

impl TeamsAccount {
    pub(super) fn answer(&mut self, answer: Answer, out: &mut Vec<Event>) {
        match answer {
            Answer::OwnPresence(result) => {
                match result {
                    Ok(list) => self.answer(Answer::Presence(list), out),
                    Err(e) => out.push(Event::Error(format!("Teams, setting presence: {e}").into())),
                }
                out.extend(self.own_state());
            }
            Answer::OwnStatus { status, result } => {
                match result {
                    Ok(()) => self.own_status = status,
                    Err(e) => out.push(Event::Error(format!("Teams, setting status: {e}").into())),
                }
                out.extend(self.own_state());
            }
            Answer::History {
                conversation,
                before,
                result,
            } => {
                let Some(room) = self.room_id(&conversation) else {
                    return;
                };
                match result {
                    Ok(v) => {
                        let page = v["messages"].as_array().cloned().unwrap_or_default();
                        let mut messages: Vec<Message> =
                            page.iter().filter_map(|m| self.message_in(&conversation, m)).collect();
                        self.sync_rooms(out);
                        self.ask_unnamed();
                        // Newest first from Teams; the core wants oldest first.
                        messages.sort_by_key(|m| m.sent_ms);
                        // Who's here now, each time a chat is opened (a snapshot).
                        let mut authors: Vec<String> = messages
                            .iter()
                            .map(|m| m.author.raw.to_string())
                            .filter(|a| a.starts_with("8:") && *a != self.me)
                            .collect();
                        authors.sort();
                        authors.dedup();
                        if !authors.is_empty() {
                            self.net.send(Cmd::Request(Request::Presence(authors)));
                        }
                        out.push(Event::History {
                            room,
                            before: before.map(|b| MessageId(b.into())),
                            messages,
                            reached_start: page.len() < PAGE,
                        });
                    }
                    Err(e) => {
                        out.push(Event::Error(format!("Teams: {e}").into()));
                        out.push(Event::HistoryFailed { room });
                    }
                }
            }
            Answer::Sent {
                conversation,
                client_id,
                result,
            } => {
                let Some(room) = self.room_id(&conversation) else {
                    return;
                };
                match result {
                    // `{"OriginalArrivalTime": ms}`: the message's id. Its echo
                    // over Trouter may come first; then the nonce is gone.
                    Ok(v) => {
                        let Some(nonce) = self.sending.remove(&client_id) else {
                            return;
                        };
                        let Some(arrived) = ms(&v["OriginalArrivalTime"]) else {
                            // Confirmed by the echo instead.
                            self.sending.insert(client_id, nonce);
                            return;
                        };
                        let Some(account) = self.account.clone() else { return };
                        let id = arrived.to_string();
                        self.confirmed.insert(id.clone());
                        out.push(Event::MessageConfirmed {
                            room,
                            nonce: nonce.into(),
                            message: Message {
                                id: MessageId(id.into()),
                                author: UserId::new(account, self.me.as_str()),
                                author_name: self.my_name.as_str().into(),
                                author_avatar: None,
                                author_colour: None,
                                sent_ms: arrived,
                                edited: false,
                                system: false,
                                text: godwit_core::rich::escape(&self.pending_text(&conversation, &client_id)).into(),
                                markup: Markup::Rich,
                                source: "".into(),
                                reply_to: None,
                                jumbo: Vec::new(),
                                attachments: v["godwit_content"]
                                    .as_str()
                                    .map(|content| crate::media::attachments(content, &[], self.media.as_ref()))
                                    .unwrap_or_default(),
                                embeds: Vec::new(),
                                invites: Vec::new(),
                                reactions: Vec::new(),
                                pending: false,
                                snippet: None,
                                pinned: false,
                                forwardable: false,
                                poll_endable: false,
                            },
                        });
                    }
                    Err(error) => {
                        if let Some(nonce) = self.sending.remove(&client_id) {
                            out.push(Event::MessageFailed {
                                room,
                                nonce: nonce.into(),
                                error: error.into(),
                            });
                        }
                    }
                }
            }
            Answer::Conversations(v) => {
                self.put_conversations(&v);
                self.sync_rooms(out);
            }
            Answer::Directory(people) => {
                let Some(account) = self.account.clone() else { return };
                let list = people
                    .iter()
                    .filter(|(mri, _)| *mri != self.me)
                    .map(|(mri, name)| godwit_core::application::Person {
                        id: UserId::new(account.clone(), mri.as_str()),
                        name: name.as_str().into(),
                        avatar_url: self.picture(mri).map(Into::into),
                        online: false,
                    })
                    .collect();
                self.names.extend(people);
                out.push(Event::PeopleDirectory { account, people: list });
                self.sync_rooms(out);
            }
            Answer::Presence(list) => {
                if let Some((_, state, _)) = list.iter().find(|(id, _, _)| id == &self.me) {
                    self.own_presence = if state == "Offline" {
                        Presence::Invisible
                    } else {
                        crate::api::presence_of(state)
                    };
                    out.extend(self.own_state());
                }
                let Some(account) = self.account.clone() else { return };
                // New status text goes straight into DM subtitles: only those
                // rooms are refreshed, and only when the text really changed.
                let mut changed: Vec<String> = Vec::new();
                for (mri, _, note) in list.iter().filter(|(mri, _, _)| *mri != self.me) {
                    let replaced = self.statuses.insert(mri.clone(), note.clone().unwrap_or_default());
                    if replaced.as_deref() != note.as_deref() && self.partner_room(mri).is_some() {
                        changed.push(mri.clone());
                    }
                }
                self.statuses.retain(|_, s| !s.is_empty());
                let presence = list
                    .iter()
                    .map(|(mri, a, _)| (UserId::new(account.clone(), mri.as_str()), crate::api::presence_of(a)))
                    .collect();
                out.push(Event::Presences { account, presence });
                for mri in changed {
                    if let Some(room) = self.partner_room(&mri) {
                        out.push(Event::RoomUpdated(room));
                    }
                }
            }
            Answer::Names(names) => {
                if names.is_empty() {
                    return;
                }
                let account = self.account.clone();
                // Our own name, when Teams' profile didn't give it (it showed "Me").
                if let Some(name) = names.iter().find(|(id, _)| *id == self.me).map(|(_, n)| n.clone())
                    && !name.is_empty()
                    && name != self.my_name
                    && let Some(account) = account.clone()
                {
                    self.my_name = name.clone();
                    out.push(Event::AccountIdentity {
                        account,
                        display_name: name.as_str().into(),
                        username: name.as_str().into(),
                        avatar_url: self.picture(&self.me).map(Into::into),
                    });
                }
                // Messages shown before the names came take them.
                if let Some(account) = account {
                    let people = names
                        .iter()
                        .map(|(id, name)| godwit_core::application::Person {
                            id: UserId::new(account.clone(), id.as_str()),
                            name: name.as_str().into(),
                            avatar_url: None,
                            online: false,
                        })
                        .collect();
                    out.push(Event::Authors { account, people });
                }
                self.names.extend(names);
                self.sync_rooms(out);
            }
            Answer::Done { what, result } => {
                if let Err(e) = result {
                    out.push(Event::Error(format!("Teams, {what}: {e}").into()));
                }
            }
        }
    }

    pub(super) fn run_command(&mut self, command: Command) -> Vec<Event> {
        let mut out = Vec::new();
        let req = |net: &Handle, r: Request| net.send(Cmd::Request(r));
        match command {
            Command::SetPresence(presence) => req(
                &self.net,
                Request::SetPresence {
                    presence,
                    user: self.me.clone(),
                },
            ),
            Command::SetStatusText(status) => req(&self.net, Request::SetStatus(status)),
            Command::CreateThread {
                room,
                name,
                message: Some(root),
                text,
            } => {
                if let Some(id) = self.remember_thread(&room.raw, &root.0, None) {
                    if let Some(c) = self.conversations.get_mut(&id)
                        && !name.trim().is_empty()
                    {
                        c.topic = name.trim().chars().take(60).collect();
                    }
                    self.sync_rooms(&mut out);
                    if let Some(thread) = self.room(&id) {
                        out.push(Event::RoomOpened(thread.id.clone()));
                        if !text.trim().is_empty() {
                            out.extend(self.run_command(Command::SendMessage {
                                room: thread.id,
                                text,
                                nonce: format!("thread-{}", rand::random::<u64>()).into(),
                                files: Vec::new(),
                                reply_to: None,
                            }));
                        }
                    }
                }
            }
            Command::LoadHistory { room, before } => {
                if let Some(conversation) = self.conversation_of_room(&room) {
                    req(
                        &self.net,
                        Request::History {
                            conversation,
                            before: before.map(|b| b.0.to_string()),
                        },
                    );
                }
            }
            Command::SendMessage {
                room,
                text,
                nonce,
                reply_to: _,
                files,
            } => {
                let Some(conversation) = self.conversation_of_room(&room) else {
                    return out;
                };
                let recipient = self.conversations.get(&conversation).and_then(|c| match &c.kind {
                    Kind::Direct(user) => Some(user.clone()),
                    _ => None,
                });
                if !files.is_empty() && (recipient.is_none() || files.len() != 1) {
                    out.push(Event::MessageFailed {
                        room,
                        nonce,
                        error: "Send one file at a time in a one-to-one Teams chat.".into(),
                    });
                    return out;
                }
                // Teams' client message ids are numbers; the web client uses the time.
                let mut client_id = format!("{}{:04}", crate::api::now_ms(), rand::random_range(0..10_000));
                while self.sending.contains_key(&client_id) {
                    client_id.push('0');
                }
                self.sending.insert(client_id.clone(), nonce.to_string());
                self.pending.insert(client_id.clone(), text.to_string());
                req(
                    &self.net,
                    Request::Send {
                        conversation,
                        html: html::from_text(&text),
                        client_id,
                        my_name: self.my_name.clone(),
                        recipient,
                        files,
                    },
                );
            }
            Command::DeleteMessage { room, message } => {
                if let Some(conversation) = self.conversation_of_room(&room) {
                    req(
                        &self.net,
                        Request::Delete {
                            conversation,
                            message: message.0.to_string(),
                        },
                    );
                }
            }
            Command::Typing { room } => {
                if let Some(conversation) = self.conversation_of_room(&room) {
                    let now = Instant::now();
                    if self
                        .typing_sent
                        .get(&conversation)
                        .is_none_or(|t| now.duration_since(*t) >= TYPING_EVERY)
                    {
                        self.typing_sent.insert(conversation.clone(), now);
                        req(&self.net, Request::Typing { conversation });
                    }
                }
            }
            Command::MarkRead { room, .. } | Command::MarkRoomRead { room } => {
                if let Some(conversation) = self.conversation_of_room(&room)
                    && let Some(c) = self.conversations.get_mut(&conversation)
                    && let Some((last, client_id, _)) = c.last.clone()
                    && let Ok(n) = last.parse::<i64>()
                    && n > c.read
                {
                    c.read = n;
                    req(
                        &self.net,
                        Request::Ack {
                            conversation: conversation.clone(),
                            message: last,
                            client_id,
                        },
                    );
                    if let Some(r) = self.room(&conversation) {
                        out.push(Event::RoomUpdated(r));
                    }
                }
            }
            _ => {}
        }
        out
    }
}
