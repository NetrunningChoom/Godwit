// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Commands from the app.

use super::*;

impl StoatAccount {
    pub(super) fn run_command(&mut self, command: Command) -> Vec<Event> {
        match command {
            Command::AutoDelete(command) => self.autodelete_command(command),
            // Stoat's privacy switches aren't offered here (yet).
            Command::LoadPrivacy | Command::SetPrivacy { .. } => Vec::new(),
            // Stoat's sessions page isn't wired to this yet (roadmap: its Sessions tab).
            Command::LoadSessions | Command::LogOutSession(_) => Vec::new(),
            Command::ServerAdmin { server, op } => self.server_admin(server, op),
            Command::LoadServerOverview { server, request } => {
                let result = self.server_overview(&server).ok_or_else(|| "This server is no longer available.".into());
                vec![Event::ServerOverview { server, request, result }]
            }
            Command::SaveServerOverview { server, request, edit } => {
                let failed = |error: &str| vec![Event::ServerOverview { server: server.clone(), request, result: Err(error.into()) }];
                if self.account.as_ref() != Some(&server.account) || !self.store.can_in_server(&server.raw, crate::permissions::MANAGE_SERVER) {
                    return failed("You no longer have permission to manage this server.");
                }
                if let Some(channels) = &edit.system_channels {
                    let Some(current) = self.store.servers.get(server.raw.as_ref()) else { return failed("This server is no longer available.") };
                    if channels.iter().flatten().any(|ch| !current.channels.iter().any(|id| id == ch.as_ref())) {
                        return failed("A selected system message channel is no longer in this server.");
                    }
                }
                let limits = self.config.as_ref().and_then(|c| c.features.limits.as_ref()).map(|l| l.default.file_upload_size_limits.clone()).unwrap_or_default();
                match crate::server_edit::prepare(*edit, &limits) {
                    Ok(p) => { self.net.send(Cmd::Server(ServerCmd::SaveServerOverview { server: server.raw.to_string(), request, body: p.body, icon: p.icon, banner: p.banner })); Vec::new() }
                    Err(error) => failed(&error),
                }
            }
            Command::LoadOwnProfile => {
                if let Some(account) = &self.account {
                    self.net.send(Cmd::People(PeopleCmd::FetchOwnProfile { user: account.user.to_string(), after_save: false }));
                }
                Vec::new()
            }
            Command::SaveOwnProfile(edit) => {
                let Some(account) = self.account.clone() else { return Vec::new() };
                let limits = self.config.as_ref().and_then(|c| c.features.limits.as_ref())
                    .map(|limits| limits.default.file_upload_size_limits.clone()).unwrap_or_default();
                match crate::profile_edit::prepare(edit, &limits) {
                    Ok(request) => {
                        self.net.send(Cmd::Settings(SettingsCmd::SaveProfile { user: None, body: request.body, avatar: request.avatar, banner: request.banner }));
                        Vec::new()
                    }
                    Err(error) => vec![Event::OwnProfileSaved { account, result: Err(error.into()) }],
                }
            }
            // Stoat has no end-to-end encryption, so nothing to unlock.
            // Our status object keeps its text: the PATCH replaces it whole.
            Command::SetPresence(presence) => {
                let value = match presence {
                    Presence::Idle => "Idle",
                    Presence::Busy => "Busy",
                    Presence::Focus => "Focus",
                    Presence::Invisible | Presence::Offline => "Invisible",
                    Presence::Online | Presence::Unknown => "Online",
                };
                let text = self.store.me.as_deref().and_then(|me| self.status_text(me));
                let mut status = serde_json::json!({ "presence": value });
                if let Some(text) = text {
                    status["text"] = serde_json::json!(text.as_ref());
                }
                self.net.send(Cmd::Settings(SettingsCmd::EditMe(serde_json::json!({ "status": status }))));
                Vec::new()
            }
            // The official custom status: the text alone, or removed.
            Command::SetStatusText(text) => {
                // The PATCH replaces the status whole: keep our presence in it.
                let presence = match self.own_presence {
                    Some(Presence::Idle) => "Idle",
                    Some(Presence::Busy) => "Busy",
                    Some(Presence::Focus) => "Focus",
                    Some(Presence::Invisible | Presence::Offline) => "Invisible",
                    _ => "Online",
                };
                let body = match text {
                    Some(text) => serde_json::json!({ "status": { "text": text, "presence": presence } }),
                    None => serde_json::json!({ "remove": ["StatusText"] }),
                };
                self.net.send(Cmd::Settings(SettingsCmd::EditMe(body)));
                Vec::new()
            }
            Command::UnlockWithKey { .. }
            | Command::StartVerification
            | Command::AnswerVerification { .. }
            | Command::CancelVerification
            | Command::AcceptVerification
            // Stoat has no invites to answer this way.
            | Command::AnswerInvite { .. }
            // Servers' channels are all listed already.
            | Command::LoadDirectory { .. }
            | Command::JoinRoom { .. } => Vec::new(),
            Command::Focus { room } => {
                self.viewing = room.map(|r| r.raw.to_string());
                if let Some(call) = &self.call {
                    self.net.send(Cmd::Voice(VoiceCommand::Voice(VoiceCmd::Watch(
                        if self.viewing.as_deref() == Some(&call.channel) {
                            call.video_sids.clone()
                        } else { Vec::new() }
                    ))));
                }
                Vec::new()
            }
            Command::LoadProfile { user, room } => {
                let user = user.raw.to_string();
                let server = room
                    .and_then(|r| self.store.channels.get(&*r.raw))
                    .and_then(|c| c.server.clone());
                // The bio and banner come from one request per person per session.
                if !self.store.profiles.contains_key(&user) {
                    self.store.profiles.insert(user.clone(), None);
                    self.net.send(Cmd::People(PeopleCmd::FetchProfile { user: user.clone() }));
                }
                self.profile_for = Some((user, server));
                self.profile().map(Event::Profile).into_iter().collect()
            }
            // Stoat sends whole member lists, not ranges.
            Command::MemberRows { .. } => Vec::new(),
            Command::LoadMembers { room } => {
                let channel = room.raw.to_string();
                // One member-list request per server per session, as officially.
                if let Some(server) = self.store.channels.get(&channel).and_then(|c| c.server.clone())
                    && self.store.members_requested.insert(server.clone())
                {
                    let large = self
                        .store
                        .servers
                        .get(&server)
                        .and_then(|s| s.approximate_member_count)
                        .unwrap_or(0);
                    self.net.send(Cmd::People(PeopleCmd::FetchMembers {
                        server,
                        online_only: large > LARGE_SERVER,
                    }));
                }
                self.members_room = Some(channel);
                self.members_sent = None;
                let mut out = Vec::new();
                self.sync_members(&mut out);
                out
            }
            Command::JoinCall { room } => {
                let channel = room.raw.to_string();
                self.next_call = self.next_call.wrapping_add(1);
                let generation = self.next_call;
                self.call = Some(LiveCall {
                    generation,
                    channel: channel.clone(),
                    participants: Vec::new(),
                    speaking: Vec::new(),
                    muted: HashMap::new(),
                    video_sids: Vec::new(),
                    videos: Vec::new(),
                    watching: std::collections::HashSet::new(),
                });
                self.net.send(Cmd::Voice(VoiceCommand::JoinVoice {
                    generation,
                    channel,
                    settings: Box::new(self.voice_settings.clone()),
                }));
                Vec::new()
            }
            // Stoat calls don't ring: nothing to turn down.
            Command::DeclineCall { .. } => Vec::new(),
            // Stoat's pins aren't wired up (its rooms don't offer them).
            // Pins and search go through Stoat's search, as the official
            // pinned and search sidebars do; `key` says which answer is which.
            Command::LoadPins { room } => {
                let channel = room.raw.to_string();
                self.net.send(Cmd::Message(MessageCmd::SearchMessages {
                    key: format!("pins{KEY_SEP}{channel}"),
                    channel,
                    body: serde_json::json!({ "pinned": true, "limit": 50, "sort": "Latest" }),
                }));
                Vec::new()
            }
            Command::SearchMessages { room, query } => {
                let channel = room.raw.to_string();
                // Stoat takes at most 64 characters.
                let asked: String = query.chars().take(64).collect();
                self.net.send(Cmd::Message(MessageCmd::SearchMessages {
                    key: format!("search{KEY_SEP}{channel}{KEY_SEP}{query}"),
                    channel,
                    body: serde_json::json!({ "query": asked, "limit": 50, "sort": "Latest" }),
                }));
                Vec::new()
            }
            Command::SetPinned { room, message, pinned } => {
                self.net.send(Cmd::Message(MessageCmd::Pin {
                    channel: room.raw.to_string(),
                    id: message.0.to_string(),
                    pin: pinned,
                }));
                Vec::new()
            }
            // Stoat has no polls or threads.
            Command::VotePoll { .. } | Command::CreateThread { .. } => Vec::new(),
            Command::LeaveCall => {
                self.net.send(Cmd::Voice(VoiceCommand::Voice(VoiceCmd::Leave)));
                Vec::new()
            }
            Command::SetMuted(muted) => {
                self.net.send(Cmd::Voice(VoiceCommand::Voice(VoiceCmd::SetMuted(muted))));
                Vec::new()
            }
            Command::SetDeafened(deafened) => {
                self.net.send(Cmd::Voice(VoiceCommand::Voice(VoiceCmd::SetDeafened(deafened))));
                Vec::new()
            }
            Command::StartScreenShare { quality } => {
                let Some(call) = &self.call else { return Vec::new() };
                let Some(resolution) = self.video_limit().filter(|_| self.store.can(&call.channel, crate::permissions::VIDEO)) else {
                    return vec![Event::Error("Video is not allowed in this call.".into())];
                };
                let quality = match quality {
                    godwit_core::call::ScreenShareQuality::Low => godwit_voice::screen::ScreenQuality::Low,
                    godwit_core::call::ScreenShareQuality::High => godwit_voice::screen::ScreenQuality::High,
                    godwit_core::call::ScreenShareQuality::Text => godwit_voice::screen::ScreenQuality::Text,
                };
                self.net.send(Cmd::Voice(VoiceCommand::Voice(VoiceCmd::StartScreenShare(godwit_voice::screen::ScreenOptions::new(quality, resolution)))));
                Vec::new()
            }
            Command::SetCamera(active) | Command::SetScreenShare(active) => {
                let Some(call) = &self.call else { return Vec::new() };
                let camera = matches!(command, Command::SetCamera(_));
                let resolution = self.video_limit();
                if active && (resolution.is_none() || !self.store.can(&call.channel, crate::permissions::VIDEO)) {
                    return vec![Event::Error("Video is not allowed in this call.".into())];
                }
                let resolution = resolution.unwrap_or([1280, 720]);
                self.net.send(Cmd::Voice(VoiceCommand::Voice(if camera {
                    VoiceCmd::SetCamera { active, resolution }
                } else if active {
                    VoiceCmd::StartScreenShare(godwit_voice::screen::ScreenOptions::new(self.voice_settings.screen_quality, resolution))
                } else { VoiceCmd::StopScreenShare })));
                Vec::new()
            }
            Command::AddFriend { user } => {
                // The official addFriend sends the tag, accepting or asking alike.
                if let Some(u) = self.store.users.get(&*user.raw) {
                    self.net.send(Cmd::People(PeopleCmd::FriendRequest {
                        request: None,
                        username: format!("{}#{}", u.username, u.discriminator),
                    }));
                }
                Vec::new()
            }
            Command::LoadInvite { code } => {
                self.net.send(Cmd::Server(ServerCmd::FetchInvite { code: code.to_string() }));
                Vec::new()
            }
            Command::LoadGifCategories => {
                self.net.send(Cmd::Media(MediaCmd::GifCategories));
                Vec::new()
            }
            Command::SearchGifs { query, position } => {
                self.net.send(Cmd::Media(MediaCmd::SearchGifs { query, position }));
                Vec::new()
            }
            Command::JoinInvite { code } => {
                self.net.send(Cmd::Server(ServerCmd::JoinInvite { code: code.to_string() }));
                Vec::new()
            }
            Command::SendFriendRequest { request, name } => {
                self.net.send(Cmd::People(PeopleCmd::FriendRequest { request: Some(request), username: name }));
                Vec::new()
            }
            Command::RemoveFriend { user } => {
                self.net.send(Cmd::People(PeopleCmd::Relationship {
                    user: user.raw.to_string(),
                    action: RelationshipAction::RemoveFriend,
                }));
                Vec::new()
            }
            Command::OpenDirect { user } => {
                self.net.send(Cmd::People(PeopleCmd::OpenDm {
                    user: user.raw.to_string(),
                }));
                Vec::new()
            }
            Command::SendMessage {
                room,
                text,
                nonce,
                reply_to,
                files,
            } => {
                self.net.send(Cmd::Message(MessageCmd::Send {
                    channel: room.raw.to_string(),
                    content: text,
                    // Replies don't mention their author unless asked, as Godwit did.
                    replies: reply_to
                        .map(|id| ReplyIntent {
                            id: id.0.to_string(),
                            mention: false,
                        })
                        .into_iter()
                        .collect(),
                    files: files
                        .into_iter()
                        .map(|f| Upload {
                            name: f.name.to_string(),
                            size: f.size,
                            source: UploadSource::Path(f.path),
                        })
                        .collect(),
                    nonce: nonce.to_string(),
                }));
                Vec::new()
            }
            Command::EditMessage { room, message, text } => {
                self.net.send(Cmd::Message(MessageCmd::Edit {
                    channel: room.raw.to_string(),
                    id: message.0.to_string(),
                    content: text,
                }));
                Vec::new()
            }
            Command::DeleteMessage { room, message } => {
                self.net.send(Cmd::Message(MessageCmd::Delete {
                    channel: room.raw.to_string(),
                    id: message.0.to_string(),
                }));
                Vec::new()
            }
            Command::React {
                room,
                message,
                emoji,
                add,
            } => {
                self.net.send(Cmd::Message(MessageCmd::React {
                    channel: room.raw.to_string(),
                    id: message.0.to_string(),
                    emoji: emoji.to_string(),
                    add,
                }));
                Vec::new()
            }
            Command::Typing { room } => {
                // At most one typing signal per channel every few seconds.
                let now = Instant::now();
                let channel = room.raw.to_string();
                if self
                    .typing_sent
                    .get(&channel)
                    .is_none_or(|t| now.duration_since(*t) >= TYPING_EVERY)
                {
                    self.typing_sent.insert(channel.clone(), now);
                    self.net.send(Cmd::Message(MessageCmd::Typing { channel }));
                }
                Vec::new()
            }
            Command::LoadAround { room, message } => {
                self.net.send(Cmd::Message(MessageCmd::FetchMessagesNearby {
                    channel: room.raw.to_string(),
                    around: message.0.to_string(),
                }));
                Vec::new()
            }
            Command::LoadHistory { room, before } => {
                self.net.send(Cmd::Message(MessageCmd::FetchMessages {
                    channel: room.raw.to_string(),
                    before: before.map(|b| b.0.to_string()),
                }));
                Vec::new()
            }
            Command::MarkRoomRead { room } => {
                let channel = room.raw.to_string();
                let last = self.store.channels.get(&channel).and_then(|c| c.last_message_id.clone());
                let mut out = Vec::new();
                if let Some(last) = last {
                    self.store.mark_read(&channel, &last);
                    self.net.send(Cmd::Message(MessageCmd::Ack { channel, message: last }));
                    self.sync_rooms(&mut out);
                }
                out
            }
            Command::MarkUnread { room, last_read } => {
                let channel = room.raw.to_string();
                // Before the first message: everything is unread. ULIDs sort,
                // so the empty id is before them all.
                let last = last_read.map(|m| m.0.to_string()).unwrap_or_default();
                self.store.mark_unread_after(&channel, &last);
                if !last.is_empty() {
                    self.net.send(Cmd::Message(MessageCmd::Ack { channel, message: last }));
                }
                let mut out = Vec::new();
                self.sync_rooms(&mut out);
                out
            }
            Command::MarkRead { room, last } => {
                let channel = room.raw.to_string();
                // Up to the channel's newest message, as the official client's
                // `channel.ack()` (its lastMessageId): a deleted or unshown
                // newest message would otherwise keep the chat unread for good.
                // ULIDs sort by time.
                let message = self
                    .store
                    .channels
                    .get(&channel)
                    .and_then(|c| c.last_message_id.clone())
                    .filter(|newest| newest.as_str() > last.0.as_ref())
                    .unwrap_or_else(|| last.0.to_string());
                self.store.mark_read(&channel, &message);
                self.net.send(Cmd::Message(MessageCmd::Ack { channel, message }));
                let mut out = Vec::new();
                self.sync_rooms(&mut out);
                out
            }
            Command::LoadThreads { .. } => Vec::new(),
        }
    }
}
