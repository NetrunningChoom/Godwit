// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Turns a user action into effects, without touching the network.

use super::*;

impl Application {
    pub fn dispatch(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::LoadPrivacy(account) => vec![Effect::Backend {
                account,
                command: Command::LoadPrivacy,
            }],
            Action::SetPrivacy { account, key, on } => vec![Effect::Backend {
                account,
                command: Command::SetPrivacy { key, on },
            }],
            Action::LoadSessions(account) => vec![Effect::Backend {
                account,
                command: Command::LoadSessions,
            }],
            Action::LogOutSession { account, session } => vec![Effect::Backend {
                account,
                command: Command::LogOutSession(session),
            }],
            Action::LogOutSessionWithProof { account, target, proof } => vec![Effect::Backend {
                account,
                command: Command::LogOutSessionWithProof { target, proof },
            }],
            Action::RenameSession { account, session, name } => vec![Effect::Backend {
                account,
                command: Command::RenameSession { session, name },
            }],
            Action::AutoDelete(account, command) => vec![Effect::Backend {
                account,
                command: Command::AutoDelete(command),
            }],
            Action::StartScreenShare { room, quality } => {
                if !self
                    .state
                    .call
                    .as_ref()
                    .is_some_and(|c| c.room == room && c.status == CallStatus::Connected)
                    || !self.state.room(&room).is_some_and(|r| r.capabilities.video)
                {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: room.account,
                    command: Command::StartScreenShare { quality },
                }]
            }
            Action::OpenServerSettings(server) => self.dispatch_open_server_settings(server),
            Action::ServerSettingsPage(page) => {
                let Some(s) = &mut self.state.server_settings else {
                    return Vec::new();
                };
                if s.page == page {
                    return Vec::new();
                }
                s.page = page;
                s.notice = None;
                self.load_server_page(page)
            }
            Action::ServerAdmin { server, op } => self.dispatch_server_admin(server, op),
            Action::CloseServerSettings => {
                self.state.server_settings = None;
                Vec::new()
            }
            Action::CloseInviteLink => {
                self.state.invite_link = None;
                Vec::new()
            }
            Action::SaveServerOverview(edit) => self.dispatch_save_server_overview(edit),
            Action::LoadOwnProfile(account) => self.dispatch_load_own_profile(account),
            Action::SaveOwnProfile { account, edit } => self.dispatch_save_own_profile(account, edit),
            Action::SelectServer(server) => self.dispatch_select_server(server),
            Action::SelectRoom(room) => {
                self.state.marked_unread.retain(|r| *r != room);
                self.select(room)
            }
            Action::PinRoom { room, pinned } => {
                self.state.pinned_rooms.retain(|r| *r != room);
                if pinned {
                    self.state.hidden_rooms.retain(|r| *r != room);
                    self.state.pinned_rooms.push(room);
                }
                Vec::new()
            }
            Action::HideRoom { room, hidden } => {
                self.state.hidden_rooms.retain(|r| *r != room);
                if hidden {
                    self.state.pinned_rooms.retain(|r| *r != room);
                    self.state.hidden_rooms.push(room);
                }
                Vec::new()
            }
            Action::ToggleHiddenRooms => {
                self.state.show_hidden = !self.state.show_hidden;
                Vec::new()
            }
            Action::MarkRoomUnread(room) => {
                if self.state.room(&room).is_some() && !self.state.marked_unread.contains(&room) {
                    self.state.marked_unread.push(room);
                }
                Vec::new()
            }
            Action::MarkRoomRead(room) => self.dispatch_mark_room_read(room),
            Action::RestoreRoomPrefs { pinned, hidden } => {
                self.state.pinned_rooms = pinned;
                self.state.hidden_rooms = hidden;
                Vec::new()
            }
            Action::SetDraft(text) => self.dispatch_set_draft(text),
            Action::Send { now_ms } => self.send(now_ms),
            Action::OpenGifs => {
                let Some(room) = self.state.selected.clone().filter(|r| self.allows(r, |c| c.gifs)) else {
                    return Vec::new();
                };
                let picker = self.state.gifs.entry(room.account.clone()).or_default();
                if std::mem::replace(&mut picker.categories_asked, true) {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: room.account,
                    command: Command::LoadGifCategories,
                }]
            }
            Action::SearchGifs { query, more } => self.dispatch_search_gifs(query, more),
            Action::SendGif { url, now_ms } => {
                let Some(room) = self.state.selected.clone().filter(|r| self.allows(r, |c| c.gifs)) else {
                    return Vec::new();
                };
                // The official picker sends the link as the message, keeping a
                // reply; its button only shows while the composer is empty.
                let draft = self.state.drafts.insert(room.clone(), url.to_string());
                let effects = self.send(now_ms);
                if let Some(draft) = draft.filter(|d| !d.trim().is_empty()) {
                    self.state.drafts.insert(room, draft);
                }
                effects
            }
            Action::LoadEarlier => self.load_earlier(),
            Action::MergeContacts { into, from } => {
                if self.state.contacts.merge(into, from) {
                    self.refresh_nudge();
                    vec![Effect::SaveContacts]
                } else {
                    Vec::new()
                }
            }
            Action::DismissNudge { never } => {
                let Some(nudge) = self.state.nudge.take() else {
                    return Vec::new();
                };
                if let Some(c) = self.state.contacts.get_mut(nudge.contact) {
                    c.nudged = true;
                    c.no_nudges |= never;
                }
                vec![Effect::SaveContacts]
            }
            Action::ClearErrors => {
                self.state.errors.clear();
                Vec::new()
            }
            Action::ReplyTo(message) => {
                if let Some(room) = self.state.selected.clone()
                    && self.allows(&room, |c| c.reply)
                    && self
                        .state
                        .selected_message(&message)
                        .is_some_and(|m| !m.pending && !m.system)
                {
                    if matches!(self.state.compose.get(&room), Some(Compose::Edit(_))) {
                        self.state.drafts.remove(&room);
                    }
                    self.state.compose.insert(room, Compose::Reply(message));
                }
                Vec::new()
            }
            Action::StartEdit(message) => {
                let Some(room) = self.state.selected.clone() else {
                    return Vec::new();
                };
                if let Some(m) = self.state.selected_message(&message).cloned()
                    && self.allows(&room, |c| c.edit)
                    && self.state.is_mine(&m)
                    && !m.pending
                    && !m.system
                {
                    self.state.drafts.insert(room.clone(), m.source.to_string());
                    self.state.compose.insert(room, Compose::Edit(message));
                }
                Vec::new()
            }
            Action::EditLast => self.dispatch_edit_last(),
            Action::JumpTo(message) => self.dispatch_jump_to(message),
            Action::VotePoll { message, answer } => {
                let Some(room) = self.state.selected.clone() else {
                    return Vec::new();
                };
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::VotePoll { room, message, answer },
                }]
            }
            Action::Search(query) => self.dispatch_search(query),
            Action::CloseSearch => {
                self.state.search = None;
                Vec::new()
            }
            Action::CreateThread { name, message, text } => self.dispatch_create_thread(name, message, text),
            Action::OpenPins => {
                let Some(room) = self.state.selected.clone().filter(|r| self.allows(r, |c| c.pins)) else {
                    return Vec::new();
                };
                self.state.pins = Some(Pins {
                    room: room.clone(),
                    messages: None,
                    failed: false,
                });
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::LoadPins { room },
                }]
            }
            Action::ClosePins => {
                self.state.pins = None;
                Vec::new()
            }
            Action::SetPinned { message, pinned } => self.dispatch_set_pinned(message, pinned),
            Action::JumpToPresent => self.dispatch_jump_to_present(),
            Action::CancelCompose => {
                if let Some(room) = self.state.selected.clone()
                    && let Some(Compose::Edit(_)) = self.state.compose.remove(&room)
                {
                    self.state.drafts.remove(&room);
                }
                Vec::new()
            }
            Action::Delete(message) => self.dispatch_delete(message),
            Action::AddFiles(files) => self.dispatch_add_files(files),
            Action::RemoveFile(index) => {
                if let Some(room) = self.state.selected.clone()
                    && let Some(list) = self.state.files.get_mut(&room)
                    && index < list.len()
                {
                    list.remove(index);
                    if list.is_empty() {
                        self.state.files.remove(&room);
                    }
                }
                Vec::new()
            }
            Action::ShowFriends => self.dispatch_show_friends(),
            Action::ShowPeople => self.show_page(false),
            Action::ShowActive => self.show_page(true),
            Action::ShowThreads => {
                if let Some(server) = self
                    .state
                    .selected_server
                    .clone()
                    .filter(|s| s.account.network.thread_style() == crate::network::ThreadStyle::Panel)
                {
                    let mut effects = self.show_page(false);
                    self.state.showing_people = false;
                    self.state.showing_threads = true;
                    self.state.selected = None;
                    self.state.selected_server = Some(server.clone());
                    effects.push(Effect::Backend {
                        account: server.account.clone(),
                        command: Command::LoadThreads { server, more: false },
                    });
                    effects
                } else {
                    Vec::new()
                }
            }
            Action::LoadMoreThreads => match self.state.selected_server.clone() {
                Some(server) if self.state.showing_threads => vec![Effect::Backend {
                    account: server.account.clone(),
                    command: Command::LoadThreads { server, more: true },
                }],
                _ => Vec::new(),
            },
            Action::SplitIdentity(user) => self.dispatch_split_identity(user),
            Action::LinkFrom(user) => self.dispatch_link_from(user),
            Action::SetNudges { contact, on } => {
                let Some(c) = self.state.contacts.get_mut(contact) else {
                    return Vec::new();
                };
                c.no_nudges = !on;
                self.refresh_nudge();
                vec![Effect::SaveContacts]
            }
            Action::JoinRoom(room) => {
                if self.state.room(&room).is_some() {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::JoinRoom { room },
                }]
            }
            Action::AnswerInvite { room, accept } => {
                if !self.state.room(&room).is_some_and(|r| r.invited_by.is_some()) {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::AnswerInvite { room, accept },
                }]
            }
            Action::SetPresence { account, presence } => self.dispatch_set_presence(account, presence),
            Action::SetStatusText { account, text } => self.dispatch_set_status_text(account, text),
            Action::StartVerification(account) => {
                if !self.state.accounts.iter().any(|a| a.id == account) {
                    return Vec::new();
                }
                self.state.verification = Some((account.clone(), Verify::Waiting));
                vec![Effect::Backend {
                    account,
                    command: Command::StartVerification,
                }]
            }
            Action::AcceptVerification => {
                let Some((account, Verify::Incoming)) = self.state.verification.clone() else {
                    return Vec::new();
                };
                self.state.verification = Some((account.clone(), Verify::Waiting));
                vec![Effect::Backend {
                    account,
                    command: Command::AcceptVerification,
                }]
            }
            Action::AnswerVerification { matches } => self.dispatch_answer_verification(matches),
            Action::CloseVerification => match self.state.verification.take() {
                Some((account, Verify::Incoming | Verify::Waiting | Verify::Compare(_) | Verify::Confirmed)) => {
                    vec![Effect::Backend {
                        account,
                        command: Command::CancelVerification,
                    }]
                }
                _ => Vec::new(),
            },
            Action::UnlockWithKey { account, key } => {
                if !self.state.accounts.iter().any(|a| a.id == account) || key.0.trim().is_empty() {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account,
                    command: Command::UnlockWithKey { key },
                }]
            }
            Action::NotSame { a, b } => {
                self.state.contacts.not_same(a, b);
                vec![Effect::SaveContacts]
            }
            Action::SetPreferred { contact, network } => {
                let Some(c) = self.state.contacts.get_mut(contact) else {
                    return Vec::new();
                };
                if c.preferred != network {
                    c.preferred_user = None;
                }
                c.preferred = network;
                self.refresh_nudge();
                vec![Effect::SaveContacts]
            }
            Action::PreferIdentity { contact, user } => {
                let Some(c) = self.state.contacts.get_mut(contact) else {
                    return Vec::new();
                };
                if !c.identities.contains(&user) {
                    return Vec::new();
                }
                c.preferred = Some(user.network());
                c.preferred_user = Some(user);
                self.refresh_nudge();
                vec![Effect::SaveContacts]
            }
            Action::TalkOn(user) => {
                let mut effects = Vec::new();
                if let Some(c) = self.state.contacts.of(&user).map(|c| c.id)
                    && let Some(c) = self.state.contacts.get_mut(c)
                    && (c.preferred != Some(user.network()) || c.preferred_user.as_ref() != Some(&user))
                {
                    c.preferred = Some(user.network());
                    c.preferred_user = Some(user.clone());
                    effects.push(Effect::SaveContacts);
                }
                effects.extend(self.dispatch(Action::OpenDirect(user)));
                self.refresh_nudge();
                effects
            }
            Action::SetNickname { contact, nickname } => {
                let Some(c) = self.state.contacts.get_mut(contact) else {
                    return Vec::new();
                };
                let nickname = nickname.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
                if c.nickname == nickname {
                    return Vec::new();
                }
                c.nickname = nickname;
                vec![Effect::SaveContacts]
            }
            Action::OpenDirect(user) => self.dispatch_open_direct(user),
            Action::AddFriend(user) => vec![Effect::Backend {
                account: user.account.clone(),
                command: Command::AddFriend { user },
            }],
            Action::RemoveFriend(user) => vec![Effect::Backend {
                account: user.account.clone(),
                command: Command::RemoveFriend { user },
            }],
            Action::RemoveGroupMember { room, user } => {
                let permitted = room.account == user.account
                    && self.state.profile.as_ref().is_some_and(|(shown, profile)| {
                        shown == &user
                            && profile.as_ref().is_some_and(|p| {
                                p.group_moderation
                                    .as_ref()
                                    .is_some_and(|m| m.remove_member && m.room == room)
                            })
                    });
                if !permitted {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::RemoveGroupMember { room, user },
                }]
            }
            Action::LoadInvite { account, code } => self.dispatch_load_invite(account, code),
            Action::JoinInvite { account, code } => {
                let key = (account.clone(), code.clone());
                let Some(crate::message::Invite::Ready(preview)) = self.state.invites.get(&key).cloned() else {
                    return Vec::new();
                };
                // Already in: open it, as the official card's Joined does nothing.
                if let Some(server) = preview.server.clone().filter(|s| self.state.server(s).is_some()) {
                    return self.dispatch(Action::SelectServer(Some(server)));
                }
                self.state.invites.insert(key, crate::message::Invite::Joining(preview));
                vec![Effect::Backend {
                    account,
                    command: Command::JoinInvite { code },
                }]
            }
            Action::SendFriendRequest { account, name } => self.dispatch_send_friend_request(account, name),
            Action::ToggleMembers => {
                self.state.show_members = !self.state.show_members;
                match self.state.selected.clone() {
                    Some(room) if self.state.show_members && self.has_members(&room) => {
                        vec![Effect::Backend {
                            account: room.account.clone(),
                            command: Command::LoadMembers { room },
                        }]
                    }
                    _ => Vec::new(),
                }
            }
            Action::MemberRows { first, last } => match self.state.selected.clone() {
                Some(room) if self.state.show_members && self.has_members(&room) => vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::MemberRows { room, first, last },
                }],
                _ => Vec::new(),
            },
            Action::OpenProfile(user) => {
                let cached = self
                    .state
                    .profiles
                    .get(&user)
                    .cloned()
                    .or_else(|| self.known_profile(&user));
                self.state.profile = Some((user.clone(), cached));
                let room = self.state.selected.clone().filter(|r| r.account == user.account);
                vec![Effect::Backend {
                    account: user.account.clone(),
                    command: Command::LoadProfile { user, room },
                }]
            }
            Action::CloseProfile => {
                self.state.profile = None;
                Vec::new()
            }
            Action::JoinCall(room) => self.dispatch_join_call(room),
            Action::DeclineCall(room) => vec![Effect::Backend {
                account: room.account.clone(),
                command: Command::DeclineCall { room },
            }],
            Action::LeaveCall => self.leave_call(),
            Action::SetCamera(active) | Action::SetScreenShare(active) => {
                let Some(call) = self.state.call.as_ref() else {
                    return Vec::new();
                };
                if call.status != CallStatus::Connected
                    || (active && !self.state.room(&call.room).is_some_and(|r| r.capabilities.video))
                {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: call.room.account.clone(),
                    command: if matches!(action, Action::SetCamera(_)) {
                        Command::SetCamera(active)
                    } else {
                        Command::SetScreenShare(active)
                    },
                }]
            }
            Action::WatchStream { user, watch } => self.dispatch_watch_stream(user, watch),
            Action::ToggleMute => {
                // Self-deafening also mutes the microphone. Keep that true
                // while deafened; the user can unmute after undeafening.
                if self.state.self_deafened && self.state.self_muted {
                    return Vec::new();
                }
                self.state.self_muted = !self.state.self_muted;
                let Some(call) = self.state.call.as_mut() else {
                    return Vec::new();
                };
                call.muted = self.state.self_muted;
                vec![Effect::Backend {
                    account: call.room.account.clone(),
                    command: Command::SetMuted(call.muted),
                }]
            }
            Action::ToggleDeafen => {
                self.state.self_deafened = !self.state.self_deafened;
                let mut effects = Vec::new();
                if self.state.self_deafened && !self.state.self_muted {
                    self.state.self_muted = true;
                    if let Some(call) = self.state.call.as_mut() {
                        call.muted = true;
                        effects.push(Effect::Backend {
                            account: call.room.account.clone(),
                            command: Command::SetMuted(true),
                        });
                    }
                }
                if let Some(call) = self.state.call.as_mut() {
                    call.deafened = self.state.self_deafened;
                    effects.push(Effect::Backend {
                        account: call.room.account.clone(),
                        command: Command::SetDeafened(call.deafened),
                    });
                }
                effects
            }
            Action::RestoreVoiceFlags { muted, deafened } => {
                self.state.self_muted = muted;
                self.state.self_deafened = deafened;
                Vec::new()
            }
            Action::MarkUnread(message) => self.dispatch_mark_unread(message),
            Action::ToggleReaction { message, emoji } => self.dispatch_toggle_reaction(message, emoji),
            Action::RemoveReactions { message, emoji } => self.dispatch_remove_reactions(message, emoji),
        }
    }
}
