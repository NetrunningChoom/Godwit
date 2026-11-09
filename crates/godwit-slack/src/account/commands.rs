// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Commands from the app.

use super::*;

impl SlackAccount {
    pub(super) fn answer(&mut self, team: &str, answer: Answer, out: &mut Vec<Event>) {
        match answer {
            Answer::NotificationPrefs(result) => {
                if let Some(ws) = self.workspaces.get_mut(team) {
                    ws.prefs_pending = false;
                }
                match result {
                    Ok(value) => self.notification_prefs(team, &value["prefs"]),
                    Err(e) => out.push(Event::Error(format!("Slack, notification settings: {e}").into())),
                }
            }
            Answer::Conversation(result) => match result {
                Ok(value) => {
                    self.put_conversation(team, &value["channel"]);
                    self.sync_rooms(out);
                }
                Err(e) => out.push(Event::Error(format!("Slack, thread's channel: {e}").into())),
            },
            Answer::OwnState(result) => {
                match result {
                    Ok(state) => {
                        self.own_presence = match state["presence"].as_str() {
                            Some("active") => Presence::Online,
                            Some("away") => Presence::Idle,
                            _ => Presence::Unknown,
                        };
                        if let Some(profile) = state.get("profile") {
                            self.own_status = profile["status_text"]
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned);
                        }
                    }
                    Err(e) => out.push(Event::Error(format!("Slack, updating status: {e}").into())),
                }
                if let Some(account) = self.account.clone() {
                    out.push(Event::OwnPresence {
                        account,
                        presence: self.own_presence,
                        status: self.own_status.as_deref().map(Into::into),
                    });
                }
            }
            Answer::Threads { more, result } => {
                if let Some(ws) = self.workspaces.get_mut(team) {
                    ws.threads_pending = false;
                }
                match result {
                    Ok((items, next)) => {
                        let mut unknown = HashSet::new();
                        if let Some(ws) = self.workspaces.get_mut(team) {
                            if more {
                                ws.threads_cursor = next;
                            } else {
                                // A fresh first page replaces what it covers;
                                // older pages scrolled into before stay, and so
                                // does the cursor past them.
                                for t in ws.threads.values_mut() {
                                    if next.as_ref().is_none_or(|oldest| t.latest >= *oldest) {
                                        t.subscribed = false;
                                    }
                                }
                                ws.threads_cursor = match (ws.threads_cursor.take(), next) {
                                    (Some(old), Some(new)) if old < new => Some(old),
                                    (_, next) => next,
                                };
                            }
                        }
                        for item in items {
                            let root = &item["root_msg"];
                            let (Some(channel), Some(ts)) = (
                                root["channel"].as_str(),
                                root["thread_ts"].as_str().or_else(|| root["ts"].as_str()),
                            ) else {
                                continue;
                            };
                            if root["subscribed"] != true {
                                continue;
                            }
                            if !self
                                .workspaces
                                .get(team)
                                .is_some_and(|w| w.conversations.contains_key(channel))
                            {
                                unknown.insert(channel.to_owned());
                            }
                            let latest = root["latest_reply"].as_str().unwrap_or(ts);
                            let read = root["last_read"].as_str().unwrap_or_default();
                            let converted = mrkdwn::convert(
                                root["text"].as_str().unwrap_or_default(),
                                &Names { account: self, team },
                            );
                            self.put_thread(team, channel, ts, Some(thread_name(&converted.plain)), latest, false);
                            if let Some(t) = self
                                .workspaces
                                .get_mut(team)
                                .and_then(|w| w.threads.get_mut(&(channel.to_owned(), ts.to_owned())))
                            {
                                t.subscribed = true;
                                t.preview = converted.plain.chars().take(400).collect();
                                if newer(read, &t.last_read) {
                                    t.last_read = read.to_owned();
                                }
                                t.unread = newer(&t.latest, &t.last_read);
                            }
                        }
                        self.sync_rooms(out);
                        for channel in unknown {
                            self.request(team, Request::Conversation(channel));
                        }
                    }
                    Err(e) => out.push(Event::Error(format!("Slack, loading Threads: {e}").into())),
                }
            }
            Answer::ThreadMarked {
                channel,
                root,
                ts,
                result,
            } => {
                let mut next = None;
                // The root or reply was deleted: nothing left to mark, so it
                // counts as read here, without an error.
                let gone = result.as_ref().is_err_and(|e| e.contains("message_not_found"));
                if let Some(t) = self
                    .workspaces
                    .get_mut(team)
                    .and_then(|w| w.threads.get_mut(&(channel.clone(), root.clone())))
                {
                    t.mark_pending = None;
                    if result.is_ok() || gone {
                        if newer(&ts, &t.last_read) {
                            t.last_read = ts.clone();
                        }
                        t.unread = newer(&t.latest, &t.last_read);
                        next = t.mark_wanted.take().filter(|wanted| newer(wanted, &ts));
                    }
                }
                if let Err(e) = &result
                    && !gone
                {
                    out.push(Event::Error(format!("Slack, marking thread read: {e}").into()));
                }
                if let Some(next) = next {
                    self.mark_thread(team, &channel, &root, next);
                }
                out.extend(self.thread_room(team, &channel, &root).map(Event::RoomUpdated));
            }
            Answer::History {
                channel,
                thread,
                before,
                result,
            } => {
                let room = match &thread {
                    Some(ts) => self
                        .account
                        .clone()
                        .map(|a| RoomId::new(a, format!("{team}:{channel}:{ts}").as_str())),
                    None => self.room_id(team, &channel),
                };
                let Some(room) = room else {
                    return;
                };
                match result {
                    Ok(v) => {
                        let page = v["messages"].as_array().cloned().unwrap_or_default();
                        let in_thread = thread.is_some();
                        let mut messages: Vec<Message> = page
                            .iter()
                            .filter_map(|m| self.message(team, &channel, m, in_thread))
                            .collect();
                        messages.sort_by_key(|m| m.sent_ms);
                        // A thread's root names it.
                        if let Some(ts) = &thread
                            && let Some(root) = messages.iter().find(|m| &*m.id.0 == ts.as_str())
                        {
                            let name = thread_name(root.snippet.as_deref().unwrap_or_default());
                            let newest = messages.last().map(|m| m.id.0.to_string()).unwrap_or_default();
                            self.put_thread(team, &channel, ts, Some(name), &newest, false);
                        }
                        out.push(Event::History {
                            room,
                            before: before.map(|b| MessageId(b.into())),
                            messages,
                            reached_start: thread.is_some() || v["has_more"] != true || page.len() < PAGE,
                        });
                        // Posts with replies became threads: list them.
                        self.sync_rooms(out);
                    }
                    Err(e) => {
                        out.push(Event::Error(format!("Slack: {e}").into()));
                        out.push(Event::HistoryFailed { room });
                    }
                }
            }
            Answer::Sent {
                channel,
                thread,
                nonce,
                result,
            } => {
                let room = match &thread {
                    Some(ts) => self
                        .account
                        .clone()
                        .map(|a| RoomId::new(a, format!("{team}:{channel}:{ts}").as_str())),
                    None => self.room_id(team, &channel),
                };
                let Some(room) = room else {
                    return;
                };
                let in_thread = thread.is_some();
                match result {
                    Ok(v) => {
                        let mut m = v["message"].clone();
                        if m["ts"].is_null() {
                            m["ts"] = v["ts"].clone();
                        }
                        if let Some(message) = self.message(team, &channel, &m, in_thread) {
                            self.confirmed.insert(message.id.0.to_string());
                            out.push(Event::MessageConfirmed {
                                room,
                                nonce: nonce.into(),
                                message,
                            });
                        }
                    }
                    Err(error) => out.push(Event::MessageFailed {
                        room,
                        nonce: nonce.into(),
                        error: error.into(),
                    }),
                }
            }
            Answer::Users(names) => {
                if names.is_empty() {
                    return;
                }
                let mut people = Vec::new();
                for (id, name, picture) in names {
                    if let Some(user) = self.user_id(team, &id) {
                        people.push(godwit_core::application::Person {
                            id: user,
                            name: name.as_str().into(),
                            avatar_url: picture.as_deref().map(Into::into),
                            online: false,
                        });
                    }
                    let me = self.workspaces.get(team).is_some_and(|w| w.me == id);
                    if me && self.account.is_some() {
                        self.my_name = name.clone();
                    }
                    if let Some(p) = picture {
                        self.pictures.insert(format!("{team}:{id}"), p);
                    }
                    if me && let Some(account) = self.account.clone() {
                        let workspace = self.workspaces.get(team).map(|w| w.name.as_str()).unwrap_or("Slack");
                        out.push(Event::AccountIdentity {
                            account,
                            display_name: name.as_str().into(),
                            username: format!("{name}@{workspace}").into(),
                            avatar_url: self.pictures.get(&format!("{team}:{id}")).map(|p| p.as_str().into()),
                        });
                    }
                    self.names.insert(format!("{team}:{id}"), name);
                }
                // Messages shown before the names came ("Someone") take them.
                if let Some(account) = self.account.clone() {
                    out.push(Event::Authors { account, people });
                }
                self.sync_rooms(out);
            }
            Answer::Directory(list) => {
                for (id, name, picture) in &list {
                    if let Some(p) = picture {
                        self.pictures.insert(format!("{team}:{id}"), p.clone());
                    }
                    self.names.insert(format!("{team}:{id}"), name.clone());
                }
                let me = self.workspaces.get(team).map(|w| w.me.clone()).unwrap_or_default();
                let people: Vec<godwit_core::application::Person> = list
                    .into_iter()
                    .filter(|(id, _, _)| *id != me)
                    .filter_map(|(id, name, picture)| {
                        Some(godwit_core::application::Person {
                            id: self.user_id(team, &id)?,
                            name: name.into(),
                            avatar_url: picture.map(Into::into),
                            online: false,
                        })
                    })
                    .collect();
                self.directory.insert(team.to_owned(), people);
                if let Some(account) = self.account.clone() {
                    out.push(Event::PeopleDirectory {
                        account,
                        people: self.directory.values().flatten().cloned().collect(),
                    });
                }
                self.sync_rooms(out);
            }
            Answer::Conversations(list) => {
                if let Some(ws) = self.workspaces.get_mut(team) {
                    ws.conversations_pending = false;
                }
                for c in &list {
                    self.put_conversation(team, c);
                }
                self.sync_rooms(out);
            }
            Answer::Done { what, result } => {
                if what == "loading channels"
                    && let Some(ws) = self.workspaces.get_mut(team)
                {
                    ws.conversations_pending = false;
                }
                if let Err(e) = result {
                    out.push(Event::Error(format!("Slack, {what}: {e}").into()));
                }
            }
        }
    }

    /// The workspace and channel of a room, and the thread if it's one.
    pub(super) fn target(&self, room: &RoomId) -> Option<(String, String, Option<String>)> {
        let (team, channel, thread) = parts(&room.raw)?;
        self.workspaces
            .get(team)?
            .conversations
            .contains_key(channel)
            .then_some(())
            .or_else(|| {
                thread
                    .filter(|ts| {
                        self.workspaces
                            .get(team)
                            .is_some_and(|w| w.threads.contains_key(&(channel.to_owned(), (*ts).to_owned())))
                    })
                    .map(|_| ())
            })
            .map(|_| (team.to_owned(), channel.to_owned(), thread.map(str::to_owned)))
    }

    pub(super) fn run_command(&mut self, command: Command) -> Vec<Event> {
        let mut out = Vec::new();
        match command {
            // Slack message components are not mapped by this account.
            Command::ActivateMessageButton { .. } => {}
            // Slack contact nicknames stay in Godwit's local contact model.
            Command::SetNickname { .. } => {}
            // Slack rooms do not currently expose group leave or recipient editing.
            Command::LeaveGroup { .. }
            | Command::AddGroupMember { .. }
            | Command::RemoveGroupMember { .. }
            | Command::JoinThread { .. }
            | Command::LeaveThread { .. } => {}
            // Slack has no channel webhook manager in this account.
            Command::LoadWebhooks { .. } | Command::ChangeWebhook { .. } => {}
            Command::LoadThreads { server, more } => self.refresh_threads(&server.raw, more),
            Command::SetPresence(presence) => {
                if matches!(presence, Presence::Online | Presence::Idle) {
                    for team in &self.teams {
                        self.request(team, Request::SetOwnPresence(presence == Presence::Online));
                    }
                }
            }
            Command::SetStatusText(status) => {
                for team in &self.teams {
                    self.request(team, Request::SetStatus(status.clone()));
                }
            }
            Command::LoadHistory { room, before } => {
                if let Some((team, channel, thread)) = self.target(&room) {
                    self.request(
                        &team,
                        Request::History {
                            channel,
                            thread,
                            before: before.map(|b| b.0.to_string()),
                        },
                    );
                }
            }
            Command::SendMessage {
                room,
                text,
                nonce,
                files,
                reply_to,
            } => {
                let Some((team, channel, thread)) = self.target(&room) else {
                    return out;
                };
                // A reply goes into the replied-to post's thread, as Slack's
                // own client replies (it has no quote replies).
                let thread = thread.or_else(|| reply_to.map(|m| m.0.to_string()));
                self.request(
                    &team,
                    Request::Send {
                        channel,
                        thread,
                        text: mrkdwn::from_text(&text),
                        nonce: nonce.to_string(),
                        files,
                    },
                );
            }
            Command::EditMessage { room, message, text } => {
                if let Some((team, channel, _)) = self.target(&room) {
                    self.request(
                        &team,
                        Request::Edit {
                            channel,
                            ts: message.0.to_string(),
                            text: mrkdwn::from_text(&text),
                        },
                    );
                }
            }
            Command::DeleteMessage { room, message } => {
                if let Some((team, channel, _)) = self.target(&room) {
                    self.request(
                        &team,
                        Request::Delete {
                            channel,
                            ts: message.0.to_string(),
                        },
                    );
                }
            }
            Command::React {
                room,
                message,
                emoji,
                add,
            } => {
                if let Some((team, channel, _)) = self.target(&room) {
                    // Slack reacts by name: `:+1:` for 👍; a custom one is its `:name:`.
                    let name = mrkdwn::reaction_name(&emoji).unwrap_or_else(|| emoji.trim_matches(':').to_owned());
                    self.request(
                        &team,
                        Request::React {
                            channel,
                            ts: message.0.to_string(),
                            name,
                            add,
                        },
                    );
                }
            }
            Command::MarkRead { room, .. } | Command::MarkRoomRead { room } => {
                if let Some((team, channel, Some(root))) = self.target(&room) {
                    if let Some(t) = self
                        .workspaces
                        .get(&team)
                        .and_then(|w| w.threads.get(&(channel.clone(), root.clone())))
                        && t.unread
                    {
                        let ts = t.latest.clone();
                        self.mark_thread(&team, &channel, &root, ts);
                    }
                    return out;
                }
                if let Some((team, channel, _)) = self.target(&room)
                    && let Some(c) = self
                        .workspaces
                        .get_mut(&team)
                        .and_then(|w| w.conversations.get_mut(&channel))
                    && !c.latest.is_empty()
                    && (c.has_unreads || newer(&c.latest, &c.last_read))
                {
                    c.last_read = c.latest.clone();
                    c.has_unreads = false;
                    c.mentions = 0;
                    let ts = c.latest.clone();
                    self.request(
                        &team,
                        Request::Mark {
                            channel: channel.clone(),
                            ts,
                        },
                    );
                    if let Some(r) = self.room(&team, &channel) {
                        out.push(Event::RoomUpdated(r));
                    }
                }
            }
            // A thread on one of a channel's messages: Slack makes it with
            // the first reply, so it opens at once and `text` is that reply.
            Command::CreateThread {
                room,
                name,
                message: Some(root),
                text,
                ..
            } => {
                if let Some((team, channel, None)) = self.target(&room) {
                    let ts = root.0.to_string();
                    let name = Some(name.trim().to_owned()).filter(|n| !n.is_empty());
                    self.put_thread(&team, &channel, &ts, name, &ts, false);
                    self.sync_rooms(&mut out);
                    if let Some(thread) = self.thread_room(&team, &channel, &ts) {
                        out.push(Event::RoomOpened(thread.id));
                    }
                    if !text.trim().is_empty() {
                        self.request(
                            &team,
                            Request::Send {
                                channel,
                                thread: Some(ts),
                                text: mrkdwn::from_text(&text),
                                nonce: format!("thread-{}", rand::random::<u64>()),
                                files: Vec::new(),
                            },
                        );
                    }
                }
            }
            Command::CreateServer { .. }
            | Command::LoadServerEvents { .. }
            | Command::SetServerEventInterest { .. } => {}
            Command::LoadServerProfile { .. } | Command::SaveServerProfile { .. } => {}
            // Slack has no separate ignore operation.
            Command::IgnoreUser { .. } | Command::UnignoreUser { .. } => {}
            _ => {}
        }
        out
    }
}
