// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Turns a network event into effects, without touching the network.

use super::*;

impl Application {
    pub fn apply(&mut self, mut event: Event) -> Vec<Effect> {
        if !self.has_owner(&event) {
            return Vec::new();
        }
        // GIFs whose video needs a player get a picture that animates
        // natively instead, whichever network sent them.
        match &mut event {
            Event::History { messages, .. } => {
                for m in messages {
                    crate::gifs::play_gifs_natively(&mut m.embeds);
                }
            }
            Event::SearchResults { messages, .. } | Event::Pins { messages, .. } => {
                for m in messages.iter_mut().flatten() {
                    crate::gifs::play_gifs_natively(&mut m.embeds);
                }
            }
            Event::MessageAdded { message, .. }
            | Event::MessageUpdated { message, .. }
            | Event::MessageConfirmed { message, .. } => crate::gifs::play_gifs_natively(&mut message.embeds),
            Event::EmbedsUpdated { embeds, .. } => crate::gifs::play_gifs_natively(embeds),
            _ => {}
        }
        match event {
            Event::Privacy { account, view } => {
                self.state.privacy.insert(account, view);
                Vec::new()
            }
            Event::Sessions { account, view } => {
                self.state.sessions.insert(account, view);
                Vec::new()
            }
            Event::SessionActionPending { account } => {
                let view = self.state.sessions.entry(account).or_default();
                view.loading = true;
                view.error = None;
                Vec::new()
            }
            Event::SessionActionFailed { account, error } => {
                let view = self.state.sessions.entry(account).or_default();
                view.loading = false;
                view.error = Some(error);
                Vec::new()
            }
            Event::AutoDelete { account, view } => {
                self.state.autodelete.insert(account, view);
                Vec::new()
            }
            Event::ServerAdmin { server, result } => {
                self.apply_server_admin(server, result);
                Vec::new()
            }
            Event::ChannelOverrides { room, overrides } => self.receive_channel_overrides(room, overrides),
            Event::ChannelOverview { room, request, result } => self.receive_channel_overview(room, request, result),
            Event::ServerOverview {
                server,
                request,
                result,
            } => self.receive_server_overview(server, request, result),
            Event::OwnProfileLoaded { account, result } => {
                self.apply_own_profile(account, result, false);
                Vec::new()
            }
            Event::OwnProfileSaved { account, result } => {
                self.apply_own_profile(account, result, true);
                Vec::new()
            }
            Event::AccountIdentity {
                account,
                display_name,
                username,
                avatar_url,
            } => {
                if let Some(ours) = self.state.accounts.iter_mut().find(|a| a.id == account) {
                    ours.display_name = display_name;
                    ours.username = username;
                    ours.avatar_url = avatar_url;
                }
                self.refresh_own_lines(&account);
                Vec::new()
            }
            Event::AccountReady {
                account,
                display_name,
                username,
                avatar_url,
                rooms,
                servers,
            } => {
                let network = account.network;
                let id = account.clone();
                let mut effects =
                    self.receive_account_ready(account, display_name, username, avatar_url, rooms, servers);
                if !network.has_presence()
                    && let Some(a) = self.state.accounts.iter_mut().find(|a| a.id == id)
                {
                    a.presence = Some(Presence::Online);
                }
                effects.extend(self.derive_people(&id));
                effects
            }
            Event::AccountLost { account, reason } => self.receive_account_lost(account, reason),
            Event::RoomUpdated(mut room) => {
                self.apply_local_read(&mut room);
                if let RoomKind::Direct { with } = &room.kind
                    && room.presence.is_none()
                {
                    room.presence = self.state.presence.get(with).copied();
                }
                if !self.state.accounts.iter().any(|account| account.id == room.id.account) {
                    return Vec::new();
                }
                let account = room.id.account.clone();
                let direct = matches!(room.kind, crate::room::RoomKind::Direct { .. });
                match self.state.rooms.iter_mut().find(|r| r.id == room.id) {
                    Some(slot) => *slot = Arc::new(room),
                    None => self.state.rooms.push(Arc::new(room)),
                }
                if direct {
                    self.derive_people(&account)
                } else {
                    Vec::new()
                }
            }
            Event::RoomRemoved(id) => self.receive_room_removed(id),
            Event::ServersChanged { account, servers } => {
                if !self.state.accounts.iter().any(|ours| ours.id == account) {
                    return Vec::new();
                }
                self.set_servers(&account, servers);
                Vec::new()
            }
            Event::History {
                room,
                before,
                messages,
                reached_start,
            } => {
                let id = room.clone();
                let effects = self.receive_history(room, before, messages, reached_start);
                self.derive_members(&id);
                effects
            }
            Event::HistoryAround { room, around, messages } => {
                let t = self.state.timelines.entry(room).or_default();
                t.messages = Arc::new(messages.into_iter().map(Arc::new).collect());
                // "Around" pages both ways: a short one isn't the start.
                t.reached_start = false;
                t.loading = false;
                t.detached = true;
                t.cached = false;
                if self.state.jump_pending.as_ref() == Some(&around) {
                    self.state.jump_pending = None;
                    self.mark_jump(around);
                }
                Vec::new()
            }
            Event::HistoryFailed { room } => {
                if let Some(t) = self.state.timelines.get_mut(&room) {
                    t.loading = false;
                    t.failed = true;
                }
                Vec::new()
            }
            Event::MessageAdded { room, message } => {
                let id = room.clone();
                // Sending a message ends its author's typing, as officially.
                if let Some(names) = self.state.typing.get_mut(&id) {
                    names.retain(|n| *n != message.author_name);
                    if names.is_empty() {
                        self.state.typing.remove(&id);
                        self.typing_at.remove(&id);
                    }
                }
                let effects = self.receive_message_added(room, message);
                self.derive_members(&id);
                effects
            }
            Event::MessageUpdated { room, message } => {
                if let Some(t) = self.state.timelines.get_mut(&room)
                    && let Some(slot) = t.messages.iter().position(|m| m.id == message.id)
                {
                    Arc::make_mut(&mut t.messages)[slot] = Arc::new(message);
                }
                Vec::new()
            }
            Event::EmbedsUpdated { room, id, embeds } => {
                if let Some(t) = self.state.timelines.get_mut(&room)
                    && let Some(slot) = t.messages.iter().position(|m| m.id == id)
                {
                    let messages = Arc::make_mut(&mut t.messages);
                    Arc::make_mut(&mut messages[slot]).embeds = embeds;
                }
                Vec::new()
            }
            Event::KeysArrived { account } => {
                // The open room reloads in place (its newest page replaces
                // what's shown); the others load fresh when opened.
                let open = self.state.selected.clone().filter(|r| r.account == account);
                self.state
                    .timelines
                    .retain(|id, _| id.account != account || Some(id) == open.as_ref());
                let Some(room) = open else { return Vec::new() };
                let t = self.state.timelines.entry(room.clone()).or_default();
                t.loading = true;
                vec![Effect::Backend {
                    account,
                    command: Command::LoadHistory { room, before: None },
                }]
            }
            Event::Directory { server, rooms } => {
                self.state.directory.insert(server, rooms);
                Vec::new()
            }
            Event::Verification { account, step } => {
                match &mut self.state.verification {
                    // The one going on (and not closed).
                    Some((a, current)) if *a == account => *current = step,
                    // Another device asking, while nothing else is going on.
                    None if step == Verify::Incoming => self.state.verification = Some((account, step)),
                    _ => {}
                }
                Vec::new()
            }
            Event::OwnPresence {
                account,
                presence,
                status,
            } => {
                if let Some(a) = self.state.accounts.iter_mut().find(|a| a.id == account) {
                    a.presence = Some(presence);
                    a.status = status;
                }
                Vec::new()
            }
            Event::Connection { account, online } => {
                if let Some(a) = self.state.accounts.iter_mut().find(|a| a.id == account) {
                    a.offline = !online;
                }
                Vec::new()
            }
            Event::DeviceTrust { account, unverified } => {
                if let Some(a) = self.state.accounts.iter_mut().find(|a| a.id == account) {
                    a.unverified = unverified;
                }
                Vec::new()
            }
            Event::ReactionsChanged { room, id, reactions } => {
                if let Some(t) = self.state.timelines.get_mut(&room)
                    && let Some(slot) = t.messages.iter().position(|m| m.id == id)
                {
                    Arc::make_mut(&mut Arc::make_mut(&mut t.messages)[slot]).reactions = reactions;
                }
                Vec::new()
            }
            Event::MessageEdited {
                room,
                id,
                text,
                markup,
                source,
            } => {
                if let Some(t) = self.state.timelines.get_mut(&room)
                    && let Some(slot) = t.messages.iter().position(|m| m.id == id)
                {
                    let m = Arc::make_mut(&mut Arc::make_mut(&mut t.messages)[slot]);
                    m.text = text;
                    m.markup = markup;
                    m.source = source;
                    m.edited = true;
                }
                Vec::new()
            }
            Event::MessageDeleted { room, id } => {
                if let Some(t) = self.state.timelines.get_mut(&room)
                    && t.messages.iter().any(|m| m.id == id)
                {
                    Arc::make_mut(&mut t.messages).retain(|m| m.id != id);
                }
                Vec::new()
            }
            Event::MessageConfirmed { room, nonce, message } => self.receive_message_confirmed(room, nonce, message),
            Event::MessageFailed { room, nonce, error } => self.receive_message_failed(room, nonce, error),
            Event::Friends { account, people } => self.receive_friends(account, people),
            Event::Blocked { account, users } => {
                self.state.blocked.retain(|u| u.account != account);
                self.state.blocked.extend(users);
                Vec::new()
            }
            Event::Favourites { account, users } => {
                self.state.favourites.retain(|u| u.account != account);
                self.state.favourites.extend(users);
                Vec::new()
            }
            Event::Presences { account, presence } => self.receive_presences(&account, presence),
            Event::VoiceSince { room, since_ms } => {
                match since_ms {
                    Some(ms) => self.state.voice_since.insert(room, ms),
                    None => self.state.voice_since.remove(&room),
                };
                Vec::new()
            }
            Event::Activities { account, activities } => {
                self.state.activities.retain(|user, _| user.account != account);
                self.state.activities.extend(activities);
                Vec::new()
            }
            Event::PeopleDirectory { account, people } => {
                self.state.people_directory.insert(account.clone(), people);
                self.derive_people(&account)
            }
            Event::Authors { account, people } => {
                self.rename_authors(&account, &people);
                Vec::new()
            }
            Event::FriendRequests {
                account,
                incoming,
                outgoing,
            } => {
                self.state.friend_requests.retain(|(p, _)| p.id.account != account);
                self.state.friend_requests.extend(
                    incoming
                        .into_iter()
                        .map(|p| (p, true))
                        .chain(outgoing.into_iter().map(|p| (p, false))),
                );
                Vec::new()
            }
            Event::GifCategories { account, categories } => {
                self.state.gifs.entry(account).or_default().categories = categories;
                Vec::new()
            }
            Event::Gifs {
                account,
                query,
                more,
                results,
                next,
            } => {
                let picker = self.state.gifs.entry(account).or_default();
                // A reply to an older search than the one shown is dropped.
                if picker.query == query {
                    if !more {
                        picker.results.clear();
                    }
                    picker.results.extend(results);
                    picker.next = next;
                    picker.loading = false;
                }
                Vec::new()
            }
            Event::GifsFailed { account, query, error } => {
                let picker = self.state.gifs.entry(account).or_default();
                if picker.query == query {
                    picker.loading = false;
                    picker.error = Some(error);
                }
                Vec::new()
            }
            Event::InviteLoaded { account, code, preview } => {
                self.state.invites.insert(
                    (account, code),
                    preview.map_or(crate::message::Invite::Invalid, crate::message::Invite::Ready),
                );
                Vec::new()
            }
            Event::InviteJoined { account, code, server } => {
                let key = (account, code);
                if let Some(crate::message::Invite::Joining(p)) = self.state.invites.get(&key).cloned() {
                    self.state.invites.insert(key, crate::message::Invite::Ready(p));
                }
                self.dispatch(Action::SelectServer(Some(server)))
            }
            Event::FriendRequestSent {
                account,
                request,
                result,
            } => {
                if self.state.friend_request_pending.as_ref() == Some(&(account.clone(), request))
                    && self.state.accounts.iter().any(|a| a.id == account)
                {
                    self.state.friend_request_status = Some(result);
                    self.state.friend_request_pending = None;
                }
                Vec::new()
            }
            Event::FriendRequestFormError(error) => {
                self.state.friend_request_pending = None;
                self.state.friend_request_status = Some(Err(error));
                Vec::new()
            }
            Event::RoomOpened(room) => {
                if self.state.room(&room).is_some() {
                    self.state.selected_server = None;
                    self.select(room)
                } else {
                    Vec::new()
                }
            }
            Event::CallVideos { room, videos } => {
                if let Some(call) = self.state.call.as_mut().filter(|c| c.room == room) {
                    call.videos = videos;
                }
                Vec::new()
            }
            Event::CallStreams { room, streams } => {
                if let Some(call) = self.state.call.as_mut().filter(|c| c.room == room) {
                    call.streams = streams;
                }
                Vec::new()
            }
            Event::CallCamera { room, active } => {
                if let Some(call) = self.state.call.as_mut().filter(|c| c.room == room) {
                    call.camera = active;
                }
                Vec::new()
            }
            Event::CallScreenShare { room, active } => {
                if let Some(call) = self.state.call.as_mut().filter(|c| c.room == room) {
                    call.screen_share = active;
                }
                Vec::new()
            }
            Event::CallConnected { room } => {
                let Some(call) = self.state.call.as_mut().filter(|c| c.room == room) else {
                    return Vec::new();
                };
                call.status = CallStatus::Connected;
                // Told again once connected: a backend whose voice wasn't
                // running yet at join may have dropped the first telling.
                [
                    call.muted.then_some(Command::SetMuted(true)),
                    call.deafened.then_some(Command::SetDeafened(true)),
                ]
                .into_iter()
                .flatten()
                .map(|command| Effect::Backend {
                    account: room.account.clone(),
                    command,
                })
                .collect()
            }
            Event::CallLatency { room, milliseconds } => {
                if let Some(call) = self.state.call.as_mut().filter(|c| c.room == room) {
                    call.latency_ms = milliseconds;
                }
                Vec::new()
            }
            Event::CallEnded { room, reason } => {
                if self.state.call.as_ref().is_some_and(|c| c.room == room) {
                    self.state.call = None;
                }
                if let Some(r) = reason {
                    self.state.errors.push(r);
                }
                Vec::new()
            }
            Event::CallMembers { room, members } => {
                if let Some(call) = self.state.call.as_mut().filter(|c| c.room == room) {
                    call.members = members;
                }
                Vec::new()
            }
            Event::Profile(p) => {
                self.state.profiles.insert(p.user.clone(), p.clone());
                if let Some((user, slot)) = self.state.profile.as_mut()
                    && *user == p.user
                {
                    *slot = Some(p);
                }
                Vec::new()
            }
            Event::Stickers { account, groups } => {
                self.state.stickers.retain(|g| g.server.account != account);
                self.state.stickers.extend(groups);
                Vec::new()
            }
            Event::CustomEmoji { account, groups } => {
                self.state.emoji.retain(|g| g.server.account != account);
                self.state.emoji.extend(groups);
                self.refresh_reaction_images(&account);
                Vec::new()
            }
            Event::SearchResults {
                room,
                query,
                offset,
                next_offset,
                total,
                notice,
                rooms,
                messages,
            } => {
                // Only the latest search's answer counts.
                if let Some(s) = self
                    .state
                    .search
                    .as_mut()
                    .filter(|s| s.room == room && s.parsed.as_ref() == Some(&query))
                {
                    let more = offset > 0;
                    // A page nobody asked for (the first, while the list is
                    // there already) is dropped.
                    if more && !s.loading_more {
                        return Vec::new();
                    }
                    s.loading_more = false;
                    s.notice = notice;
                    let page = messages
                        .as_ref()
                        .map(|m| m.iter().cloned().map(Arc::new).collect::<Vec<_>>());
                    if more && page.is_none() {
                        // The page failed: keep what's shown and let "load
                        // more" be tried again.
                        return Vec::new();
                    }
                    s.failed = page.is_none();
                    let page = page.unwrap_or_default();
                    let rooms = if rooms.len() == page.len() {
                        rooms
                    } else {
                        vec![room.clone(); page.len()]
                    };
                    match (more, s.messages.as_mut()) {
                        (true, Some(list)) => {
                            list.extend(page);
                            s.rooms.extend(rooms);
                        }
                        _ => {
                            s.messages = Some(page);
                            s.rooms = rooms;
                        }
                    }
                    s.next_offset = next_offset;
                    s.total = total;
                }
                Vec::new()
            }
            Event::Forum(page) => {
                if let Some(f) = self.state.forum.as_mut() {
                    f.apply(*page);
                }
                Vec::new()
            }
            Event::ForumFailed {
                room,
                archived,
                sort,
                tag,
            } => {
                if let Some(f) = self.state.forum.as_mut().filter(|f| f.room == room) {
                    f.fail(archived, sort, tag);
                }
                Vec::new()
            }
            Event::Pins { room, messages } => {
                if let Some(p) = self.state.pins.as_mut().filter(|p| p.room == room) {
                    p.failed = messages.is_none();
                    p.messages = messages.map(|list| list.into_iter().map(Arc::new).collect());
                }
                Vec::new()
            }
            Event::GameLibrary { user, library } => {
                if let Some(g) = self.state.games.as_mut().filter(|g| g.account == user.account) {
                    g.libraries.insert(user, library);
                }
                Vec::new()
            }
            Event::Members { room, groups } => {
                self.state.members.insert(room, Arc::new(groups));
                Vec::new()
            }
            Event::VoiceOccupants { room, members } => {
                if members.is_empty() {
                    self.state.voice.remove(&room);
                } else {
                    self.state.voice.insert(room, members);
                }
                Vec::new()
            }
            Event::Typing { room, names } => {
                if names.is_empty() {
                    self.typing_at.remove(&room);
                    self.state.typing.remove(&room);
                } else {
                    self.typing_at.insert(room.clone(), std::time::Instant::now());
                    self.state.typing.insert(room, names);
                }
                Vec::new()
            }
            Event::Reported { result, .. } => {
                if let Err(e) = &result {
                    self.state.errors.push(format!("Report failed: {e}").into());
                }
                self.state.report_outcome = Some(result);
                Vec::new()
            }
            Event::Error(e) => {
                self.state.errors.push(e);
                Vec::new()
            }
            // For the shell (notifications and sounds); nothing to keep.
            Event::Alert { .. } | Event::AlertDone { .. } => Vec::new(),
        }
    }
}
