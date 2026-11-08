// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Backend and websocket events.

use super::*;

impl StoatAccount {
    pub(super) fn handle(&mut self, ev: Ev, out: &mut Vec<Event>) {
        match ev {
            Ev::ServerOverviewSaved {
                server,
                request,
                result,
            } => {
                let Some(account) = self.account.clone() else { return };
                if !self.store.servers.contains_key(&server) {
                    return;
                }
                let id = ServerId::new(account, server.as_str());
                let result = match result {
                    Ok(updated) if updated.id == server => {
                        self.store.servers.insert(server, updated);
                        self.sync_servers(out);
                        self.sync_rooms(out);
                        self.server_overview(&id)
                            .ok_or_else(|| "This server is no longer available.".into())
                    }
                    Ok(_) => Err("The server returned an unexpected response.".into()),
                    Err(error) => Err(error.into()),
                };
                out.push(Event::ServerOverview {
                    server: id,
                    request,
                    result,
                });
            }
            Ev::ChannelSaved { .. } | Ev::ChannelDeleted { .. } => self.channel_answer(ev, out),
            Ev::UserSaved(user) => {
                self.store.users.insert(user.id.clone(), user);
                self.sync_rooms(out);
                self.sync_friends(out);
                self.sync_members(out);
                self.sync_own(out);
            }
            Ev::ServerJoined(server) => {
                // Creation returns a complete server before its websocket
                // events arrive; install it and refresh both sidebar models.
                self.store.add_server(server);
                self.sync_servers(out);
                self.sync_rooms(out);
            }
            Ev::OwnProfile { after_save, result } => {
                let Some(account) = self.account.clone() else { return };
                let result = match result {
                    Ok(profile) => {
                        self.store.profiles.insert(account.user.to_string(), Some(profile));
                        self.own_profile()
                            .ok_or_else(|| "Your account profile is not available.".into())
                    }
                    Err(error) => Err(error.into()),
                };
                if after_save {
                    out.push(Event::OwnProfileSaved { account, result });
                } else {
                    out.push(Event::OwnProfileLoaded { account, result });
                }
                if let Some(profile) = self.profile() {
                    out.push(Event::Profile(profile));
                }
            }
            Ev::ProfileSaved { user: None, result } => {
                let Some(account) = self.account.clone() else { return };
                match result {
                    Ok(()) => self.net.send(Cmd::People(PeopleCmd::FetchOwnProfile {
                        user: account.user.to_string(),
                        after_save: true,
                    })),
                    Err(error) => out.push(Event::OwnProfileSaved {
                        account,
                        result: Err(error.into()),
                    }),
                }
            }
            Ev::Config(c) => {
                let first = self.config.is_none();
                self.config = Some(c);
                // Messages that came before it showed custom emoji as `:name:`
                // (no address for their pictures yet): convert them again.
                if first {
                    self.reconvert_emoji_messages(None, out);
                }
            }
            Ev::MfaRequired { ticket } => {
                self.mfa_ticket = Some(ticket);
                self.status = Status::NeedsMfa { error: None };
            }
            Ev::LoggedIn(session) => {
                if self.persist_session {
                    session_file::save(self.slot, &session);
                }
                self.mfa_ticket = None;
                self.status = Status::Connecting;
                self.net.send(Cmd::Session(SessionCmd::UseSession(session)));
            }
            Ev::LoggedOut => {
                self.autodelete_logged_out();
                self.forget_session();
                self.status = Status::SignedOut { error: None };
                // Only ever after the user's own Logout: nothing to report.
                self.lose("", out);
            }
            Ev::Connecting => {
                if self.status == Status::Ready {
                    self.autodelete_disconnected();
                    self.status = Status::Connecting;
                    // Dropped while in use: say so until it's back (Ready
                    // re-sends the account, connected).
                    if let Some(account) = self.account.clone() {
                        out.push(Event::Connection { account, online: false });
                    }
                }
            }
            Ev::Ready(ready) => {
                // One unread sync per (re)connect, as the official client does.
                self.net.send(Cmd::Message(MessageCmd::FetchUnreads));
                self.net.send(Cmd::Settings(SettingsCmd::FetchSyncedSettings));
                self.store.apply_ready(ready);
                self.status = Status::Ready;
                let Some(me) = self.store.me.clone() else {
                    return;
                };
                let account = AccountId::new(Network::Stoat, me.as_str());
                self.account = Some(account.clone());
                let rooms = self.build_rooms(&account);
                let servers = self.build_servers(&account);
                self.rooms.reset(rooms.iter().cloned());
                self.servers = servers.clone();
                let username = self
                    .store
                    .users
                    .get(&me)
                    .map(|u| format!("{}#{}", u.username, u.discriminator))
                    .unwrap_or_default();
                out.push(Event::AccountReady {
                    account,
                    display_name: self.store.display_name(&me, None).into(),
                    username: username.into(),
                    avatar_url: Some(self.avatar_url(&me, None).into()),
                    rooms,
                    servers,
                });
                self.friends.clear();
                self.sync_friends(out);
                self.occupants.clear();
                self.sync_voice(out);
                self.emoji_sent = None;
                self.sync_emoji(out);
                self.own_presence = None;
                self.sync_own(out);
                self.autodelete_connected();
            }
            Ev::AutoDelete(ev) => self.autodelete_event(ev, out),
            Ev::AutoDeleteTick => self.autodelete_tick(),
            Ev::Event(v) => {
                self.autodelete_live(&v);
                // Only these can change an open Emoji page; skip rebuilding
                // its list for every other socket message.
                let emoji_event = matches!(
                    v.get("type").and_then(serde_json::Value::as_str),
                    Some("EmojiCreate" | "EmojiDelete" | "ServerDelete")
                );
                self.handle_socket_event(v, out);
                self.sync_roles(out);
                if emoji_event {
                    self.sync_emoji_page(out);
                }
            }
            Ev::EmojiSaved(result) => self.emoji_saved(result, out),
            Ev::ServerInvites { server, invites } => {
                let list = invites.iter().filter_map(|v| self.invite(v)).collect();
                self.admin_answer(&server, Ok(ServerAdminResult::Invites(list)), out);
            }
            Ev::Bans { server, data } => {
                let Some(account) = self.account.clone() else {
                    return;
                };
                let list = ban_list(&data, |user| self.avatar_for(user))
                    .into_iter()
                    .map(
                        |(user, name, avatar_url, reason)| godwit_core::server_settings::ServerBan {
                            user: UserId::new(account.clone(), user.as_str()),
                            name: name.into(),
                            avatar_url: avatar_url.map(Into::into),
                            reason: reason.into(),
                        },
                    )
                    .collect();
                self.admin_answer(&server, Ok(ServerAdminResult::Bans(list)), out);
            }
            Ev::Invite(result) => {
                let Some(server) = self.invite_for.take() else {
                    return;
                };
                let result = result
                    .map(|code| ServerAdminResult::InviteCreated(format!("https://stt.gg/{code}").into()))
                    .map_err(Into::into);
                self.admin_answer(&server, result, out);
            }
            Ev::ChannelCreated { channel, .. } => {
                let Some(server) = self.channel_for.take() else {
                    return;
                };
                let result = match channel.map(serde_json::from_value::<Channel>) {
                    Ok(Ok(c)) => {
                        // Shown now; the ChannelCreate event that follows is a repeat.
                        self.store.add_channel(c);
                        self.sync_rooms(out);
                        self.sync_servers(out);
                        Ok(ServerAdminResult::Done("Channel created.".into()))
                    }
                    Ok(Err(_)) => Err("The server returned an unexpected channel.".into()),
                    Err(e) => Err(e.into()),
                };
                self.admin_answer(&server, result, out);
            }
            Ev::AdminDone { server, result } => {
                let result = result
                    .map(|()| ServerAdminResult::Done("Done.".into()))
                    .map_err(Into::into);
                self.admin_answer(&server, result, out);
            }
            Ev::RoleCreated { server, .. } => {
                self.admin_answer(&server, Ok(ServerAdminResult::Done("Role created.".into())), out);
            }
            Ev::RoleSaved { server, result } => {
                let result = result
                    .map(|()| ServerAdminResult::Done("Saved.".into()))
                    .map_err(Into::into);
                self.admin_answer(&server, result, out);
            }
            Ev::Messages { channel, before, data } => {
                let page: Vec<Message> = data.messages.clone();
                self.autodelete_seen_page(&channel, before.as_deref(), &page, false);
                self.store.apply_messages(&channel, before.as_deref(), data);
                let Some(room) = self.room_id(&channel) else {
                    return;
                };
                let reached_start = self.store.messages.get(&channel).is_some_and(|m| m.reached_start);
                let mut messages: Vec<CoreMessage> = page.iter().map(|m| self.message(m)).collect();
                // The API answers newest first; the core wants oldest first.
                messages.sort_by(|a, b| a.id.cmp(&b.id));
                out.push(Event::History {
                    room,
                    before: before.map(|b| MessageId(b.into())),
                    messages,
                    reached_start,
                });
            }
            Ev::MessagesNearby { channel, around, data } => {
                let page: Vec<Message> = data.messages.clone();
                self.autodelete_seen_page(&channel, None, &page, true);
                self.store.apply_nearby(&channel, data);
                if let Some(room) = self.room_id(&channel) {
                    let mut messages: Vec<CoreMessage> = page.iter().map(|m| self.message(m)).collect();
                    messages.sort_by(|a, b| a.id.cmp(&b.id));
                    out.push(Event::HistoryAround {
                        room,
                        around: MessageId(around.into()),
                        messages,
                    });
                }
            }
            Ev::MessagesFailed { channel } => {
                if let Some(room) = self.room_id(&channel) {
                    out.push(Event::HistoryFailed { room });
                }
            }
            Ev::Sent {
                channel,
                nonce,
                message,
            } => {
                if let Some(room) = self.room_id(&channel) {
                    let message = self.message(&message);
                    out.push(Event::MessageConfirmed {
                        room,
                        nonce: nonce.into(),
                        message,
                    });
                }
            }
            Ev::SendFailed { channel, nonce, error } => {
                if let Some(room) = self.room_id(&channel) {
                    out.push(Event::MessageFailed {
                        room,
                        nonce: nonce.into(),
                        error: error.into(),
                    });
                }
            }
            Ev::Voice {
                channel,
                generation,
                event,
            } => {
                if self
                    .call
                    .as_ref()
                    .is_some_and(|call| call.channel == channel && call.generation == generation)
                {
                    self.handle_voice(event, out);
                }
            }
            Ev::Profile { user, profile } => {
                self.store.profiles.insert(user.clone(), Some(profile));
                if self.profile_for.as_ref().is_some_and(|(u, _, _)| *u == user)
                    && let Some(p) = self.profile()
                {
                    out.push(Event::Profile(p));
                }
            }
            Ev::MemberSaved(result) => {
                let Some(action) = self.group_member_pending.take() else {
                    return;
                };
                match result {
                    Ok(()) => {
                        match action {
                            GroupMemberAction::Remove { channel, user } => {
                                if let Some(c) = self.store.channels.get_mut(&channel) {
                                    c.recipients.retain(|u| u != &user);
                                }
                            }
                            GroupMemberAction::Transfer { channel, user } => {
                                if let Some(c) = self.store.channels.get_mut(&channel) {
                                    c.owner = Some(user);
                                }
                            }
                        }
                        self.sync_rooms(out);
                        self.sync_members(out);
                    }
                    Err(error) => out.push(Event::Error(format!("Stoat, removing group member: {error}").into())),
                }
            }
            Ev::Members { data } => {
                self.store.apply_members(data);
                self.sync_members(out);
            }
            Ev::AudioDevices(d) => self.devices = Some(d),
            Ev::MfaFailed(error) => {
                if !self.session_revoke_pending {
                    out.push(Event::Error(
                        format!("Stoat, confirming account security: {error}").into(),
                    ));
                    return;
                }
                self.session_revoke_pending = false;
                if let Some(account) = self.account.clone() {
                    out.push(Event::SessionActionFailed {
                        account,
                        error: error.into(),
                    });
                }
            }
            Ev::Sessions(sessions) => {
                self.session_revoke_pending = false;
                let Some(account) = self.account.clone() else { return };
                let mut sessions: Vec<_> = sessions
                    .into_iter()
                    .map(|session| godwit_core::privacy::Session {
                        id: session.id.clone().into(),
                        device: session.name.into(),
                        location: "".into(),
                        created_ms: crate::api::ulid_timestamp_ms(&session.id),
                        last_used_ms: None,
                    })
                    .collect();
                sessions.sort_by_key(|session| std::cmp::Reverse(session.created_ms));
                let view = godwit_core::privacy::SessionsView {
                    sessions,
                    ..Default::default()
                };
                out.push(Event::Sessions { account, view });
            }
            Ev::SessionRenamed(_) => {
                // Refresh names from Stoat's authoritative session list.
                self.net.send(Cmd::Settings(SettingsCmd::FetchSessions));
            }
            Ev::GifCategories(result) => {
                let Some(account) = self.account.clone() else { return };
                match result {
                    Ok(list) => out.push(Event::GifCategories {
                        account,
                        categories: list
                            .into_iter()
                            .map(|c| godwit_core::gifs::GifCategory {
                                title: c.title.into(),
                                image: c.image.into(),
                            })
                            .collect(),
                    }),
                    Err(e) => out.push(Event::Error(format!("Stoat, loading GIF categories: {e}").into())),
                }
            }
            Ev::Gifs { query, more, page } => {
                let Some(account) = self.account.clone() else { return };
                match page {
                    Ok(page) => out.push(Event::Gifs {
                        account,
                        query,
                        more,
                        results: page.results.into_iter().filter_map(gif).collect(),
                        next: page.next.filter(|n| !n.is_empty()),
                    }),
                    Err(error) => out.push(Event::GifsFailed {
                        account,
                        query,
                        error: error.into(),
                    }),
                }
            }
            Ev::InvitePreview { code, preview } => {
                let Some(account) = self.account.clone() else { return };
                let preview = match preview {
                    // Only server invites have a card (as the official element).
                    Ok(p) => p.server_name.clone().map(|name| godwit_core::message::InvitePreview {
                        server: p.server_id.as_deref().map(|s| ServerId::new(account.clone(), s)),
                        name: name.into(),
                        icon_url: p.server_icon.as_ref().and_then(|f| self.file_url(f)).map(Into::into),
                        members: p.member_count,
                    }),
                    Err(e) => {
                        out.push(Event::Error(format!("Stoat, reading an invite: {e}").into()));
                        None
                    }
                };
                self.invites.insert(code.clone(), preview.clone());
                out.push(Event::InviteLoaded {
                    account,
                    code: code.into(),
                    preview,
                });
            }
            Ev::InviteJoined { code, result } => {
                let Some(account) = self.account.clone() else { return };
                match result {
                    Ok(joined) => {
                        let server = ServerId::new(account.clone(), joined.server.id.as_str());
                        self.store.add_server(joined);
                        self.sync_servers(out);
                        self.sync_rooms(out);
                        out.push(Event::InviteJoined {
                            account,
                            code: code.into(),
                            server,
                        });
                    }
                    Err(e) => {
                        out.push(Event::Error(format!("Stoat, joining through an invite: {e}").into()));
                        // The card goes back to Join.
                        out.push(Event::InviteLoaded {
                            account,
                            preview: self.invites.get(&code).cloned().flatten(),
                            code: code.into(),
                        });
                    }
                }
            }
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
            // The dialog shows the outcome; a failure is logged too.
            Ev::Reported(result) => {
                if let Some(account) = self.account.clone() {
                    out.push(Event::Reported {
                        account,
                        result: result.map_err(Into::into),
                    });
                }
            }
            // A friend request or removal went through: the user as they
            // stand with us now.
            Ev::UserChanged(user) => {
                self.store.upsert_relationship(user);
                self.sync_friends(out);
                if let Some(p) = self.profile() {
                    out.push(Event::Profile(p));
                }
            }
            Ev::DmOpened(mut channel) => {
                // We opened it, so it belongs in the list even if it was hidden.
                channel.active = Some(true);
                let id = channel.id.clone();
                self.store.channels.insert(id.clone(), channel);
                self.sync_rooms(out);
                if let Some(room) = self.room_id(&id) {
                    out.push(Event::RoomOpened(room));
                }
            }
            Ev::Unreads(u) => {
                self.store.apply_unreads(u);
                self.sync_rooms(out);
            }
            Ev::SyncedSettings(v) => {
                self.store.apply_synced(&v);
                self.sync_rooms(out);
                self.sync_servers(out);
            }
            Ev::Error(e) => match self.status {
                // A failed login or code: show it on the form that sent it.
                Status::LoggingIn | Status::NeedsMfa { .. } => {
                    self.status = match self.mfa_ticket {
                        Some(_) => Status::NeedsMfa { error: Some(e) },
                        None => Status::SignedOut { error: Some(e) },
                    };
                }
                // The saved session was rejected: sign in again.
                Status::Connecting if e.contains("HTTP 401") => {
                    self.forget_session();
                    self.status = Status::SignedOut { error: Some(e) };
                }
                _ => out.push(Event::Error(e.into())),
            },
            Ev::SearchResults { key, result } => {
                let mut parts = key.split(KEY_SEP);
                let (kind, channel, query) = (parts.next(), parts.next(), parts.next());
                let Some(room) = channel.and_then(|c| self.room_id(c)) else {
                    return;
                };
                if let Err(e) = &result {
                    out.push(Event::Error(format!("Stoat search: {e}").into()));
                }
                let messages = result.ok().map(|data| {
                    let page = data.messages.clone();
                    for u in data.users {
                        self.store.users.insert(u.id.clone(), u);
                    }
                    for m in data.members {
                        self.store.members.insert(m.id.clone(), m);
                    }
                    page.iter().map(|m| self.message(m)).collect::<Vec<_>>()
                });
                match kind {
                    Some("pins") => out.push(Event::Pins { room, messages }),
                    Some("search") => out.push(Event::SearchResults {
                        room,
                        query: godwit_core::search::SearchQuery::text_only(query.unwrap_or_default()),
                        offset: 0,
                        next_offset: None,
                        total: None,
                        notice: None,
                        rooms: Vec::new(),
                        messages,
                    }),
                    _ => {}
                }
            }
            Ev::Pinned { channel, result } => match result {
                // The list again: what the panel shows is Stoat's.
                Ok(()) => self.net.send(Cmd::Message(MessageCmd::SearchMessages {
                    key: format!("pins{KEY_SEP}{channel}"),
                    channel,
                    body: serde_json::json!({ "pinned": true, "limit": 50, "sort": "Latest" }),
                })),
                Err(e) => out.push(Event::Error(format!("Stoat pin: {e}").into())),
            },
            // Everything else belongs to features not ported yet.
            _ => {}
        }
    }

    pub(super) fn handle_socket_event(&mut self, v: Value, out: &mut Vec<Event>) {
        let kind = v.get("type").and_then(Value::as_str).unwrap_or_default().to_owned();
        let channel = v.get("channel").and_then(Value::as_str).map(str::to_owned);
        let id = v.get("id").and_then(Value::as_str).map(str::to_owned);
        // Reaction events name their channel `channel_id`.
        let v_channel_id = v.get("channel_id").and_then(Value::as_str).map(str::to_owned);
        let deleted: Vec<String> = match kind.as_str() {
            "BulkMessageDelete" => v
                .get("ids")
                .and_then(Value::as_array)
                .map(|ids| ids.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let new_message = (kind == "Message")
            .then(|| serde_json::from_value::<Message>(v.clone()).ok())
            .flatten();
        if matches!(
            kind.as_str(),
            "MessageReact" | "MessageUnreact" | "MessageRemoveReaction"
        ) {
            let emoji = v.get("emoji_id").and_then(Value::as_str).unwrap_or_default();
            let ours = v
                .get("user_id")
                .and_then(Value::as_str)
                .zip(self.store.me.as_deref())
                .is_some_and(|(user, me)| user == me);
            godwit_core::verbose!("reaction: received event={kind} emoji={emoji} ours={ours}");
        }
        // EmojiCreate is the emoji itself (`_id`, v1.ts).
        let new_emoji = (kind == "EmojiCreate")
            .then(|| v.get("_id").and_then(Value::as_str).map(str::to_owned))
            .flatten();
        self.store.apply_event(v, self.viewing.as_deref());

        // A new emoji can be in messages already shown as its id.
        if let Some(emoji) = new_emoji.as_deref() {
            self.reconvert_emoji_messages(Some(emoji), out);
        }
        match kind.as_str() {
            "Message" => {
                if let Some(m) = new_message
                    && let Some(room) = self.room_id(&m.channel)
                {
                    if let Some(alert) = self.notify(&m, &room) {
                        out.push(alert);
                    }
                    let message = self.message(&m);
                    out.push(Event::MessageAdded { room, message });
                }
            }
            "MessageReact" | "MessageUnreact" | "MessageRemoveReaction" => {
                let channel = v_channel_id.as_deref();
                if let (Some(channel), Some(id)) = (channel, &id)
                    && let Some(room) = self.room_id(channel)
                    && let Some(m) = self.store.messages.get(channel).and_then(|c| c.messages.get(id))
                {
                    let message = self.message(m);
                    out.push(Event::MessageUpdated { room, message });
                }
            }
            "ChannelStartTyping" | "ChannelStopTyping" => {
                if let Some(channel) = id.as_deref()
                    && let Some(room) = self.room_id(channel)
                {
                    let names = self.store.typing_in(channel).into_iter().map(Into::into).collect();
                    out.push(Event::Typing { room, names });
                }
            }
            // Append: link previews made after sending (a GIF from the picker
            // is a link until its embed comes this way).
            "MessageUpdate" | "MessageAppend" => {
                if let (Some(channel), Some(id)) = (&channel, &id)
                    && let Some(room) = self.room_id(channel)
                    && let Some(m) = self.store.messages.get(channel).and_then(|c| c.messages.get(id))
                {
                    let message = self.message(m);
                    out.push(Event::MessageUpdated { room, message });
                }
            }
            "MessageDelete" => {
                if let (Some(channel), Some(id)) = (&channel, id)
                    && let Some(room) = self.room_id(channel)
                {
                    out.push(Event::MessageDeleted {
                        room,
                        id: MessageId(id.into()),
                    });
                }
            }
            "BulkMessageDelete" => {
                if let Some(room) = channel.as_deref().and_then(|c| self.room_id(c)) {
                    for id in deleted {
                        out.push(Event::MessageDeleted {
                            room: room.clone(),
                            id: MessageId(id.into()),
                        });
                    }
                }
            }
            _ => {}
        }
        // Presence moves people between groups; other events can rename them.
        if kind != "Pong" && !kind.contains("Typing") {
            self.sync_members(out);
        }
        if !QUIET_EVENTS.contains(&kind.as_str()) {
            self.sync_rooms(out);
            if !kind.starts_with("Message") {
                self.sync_servers(out);
                self.sync_friends(out);
                if kind == "UserRelationship"
                    && let Some(p) = self.profile()
                {
                    out.push(Event::Profile(p));
                }
                self.sync_voice(out);
                self.sync_emoji(out);
                self.sync_own(out);
            }
        }
    }

    /// An alert (desktop notification and sound, as the app's settings allow)
    /// for a new message, following the synced per-server and per-channel
    /// levels and mutes, as the official client.
    pub(super) fn notify(&self, m: &Message, room: &RoomId) -> Option<Event> {
        if !self.notifications || m.system.is_some() {
            return None;
        }
        let me = self.store.me.as_deref()?;
        // The chat on screen is skipped by the app, only while its window
        // is focused: not here, by selection.
        if m.author == me {
            return None;
        }
        let channel = self.store.channels.get(&m.channel)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        if !self
            .store
            .notification_prefs
            .should_notify(channel, m.mentions.iter().any(|u| u == me), now_ms)
        {
            return None;
        }
        let server = channel.server.as_deref();
        let author = m
            .masquerade
            .as_ref()
            .and_then(|q| q.name.clone())
            .unwrap_or_else(|| self.store.display_name(&m.author, server));
        let title = match server.and_then(|s| self.store.servers.get(s)) {
            Some(s) => format!("{author} (#{}, {})", channel.name.as_deref().unwrap_or(""), s.name),
            None if channel.channel_type == "Group" => format!("{author} ({})", self.store.channel_title(channel)),
            None => author,
        };
        let names = Names {
            store: &self.store,
            server,
            autumn: self.config.as_ref().map(|c| c.features.autumn.url.as_str()),
        };
        let mut body = match m.content.as_deref().filter(|c| !c.is_empty()) {
            Some(c) => rich::to_plain(c, &names),
            None if !m.attachments.is_empty() => "Sent an attachment".to_owned(),
            None => return None,
        };
        if body.chars().count() > NOTIFY_CHARS {
            body = body.chars().take(NOTIFY_CHARS).collect::<String>() + "…";
        }
        Some(Event::Alert {
            from: self.account.clone().map(|a| UserId::new(a, m.author.as_str())),
            room: room.clone(),
            kind: AlertKind::Message,
            title: title.into(),
            body: body.into(),
        })
    }
}
