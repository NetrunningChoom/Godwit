// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Turns a user action into effects, without touching the network.

use super::*;

impl Application {
    /// The open profile card is `user`'s and allows blocking them.
    fn shown_profile_can_block(&self, user: &UserId) -> bool {
        self.state
            .profile
            .as_ref()
            .is_some_and(|(shown, profile)| shown == user && profile.as_ref().is_some_and(|p| p.can_block && !p.me))
    }

    pub fn dispatch(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::EditFolders(account, edit) => {
                if account.network.has_server_folders()
                    && edit.server_ids().iter().all(|id| id.account == account)
                    && edit.changes(&self.state.servers)
                {
                    vec![Effect::Backend {
                        account,
                        command: Command::EditFolders(edit),
                    }]
                } else {
                    Vec::new()
                }
            }
            Action::SetPreferences { account, prefs } if account.network.syncs_preferences() => {
                vec![Effect::Backend {
                    account,
                    command: Command::SetPreferences(prefs),
                }]
            }
            Action::SetPreferences { .. } => Vec::new(),
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
            Action::AccountSecurity(account, action) => {
                if account
                    .network
                    .account_pages()
                    .contains(&crate::network::AccountPage::Security)
                {
                    vec![Effect::Backend {
                        account,
                        command: Command::AccountSecurity(action),
                    }]
                } else {
                    Vec::new()
                }
            }
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
            Action::OpenChannelSettings(room) => self.dispatch_open_channel_settings(room),
            Action::OpenCategorySettings { server, category } => self.dispatch_open_category_settings(server, category),
            Action::CloseChannelSettings => {
                self.state.channel_settings = None;
                self.state.webhooks = None;
                Vec::new()
            }
            Action::SaveChannelOverview(edit) => self.dispatch_save_channel_overview(edit),
            Action::DeleteChannel => self.dispatch_delete_channel(),
            Action::SetChannelOverride { target, allow, deny } => {
                self.dispatch_channel_override(target, Some((allow, deny)))
            }
            Action::DeleteChannelOverride(target) => self.dispatch_channel_override(target, None),
            Action::LoadWebhooks => {
                let Some(s) = self.state.channel_settings.as_ref() else {
                    return Vec::new();
                };
                let room = s.room.clone();
                if !self.state.room(&room).is_some_and(|r| r.capabilities.manage_webhooks) {
                    return Vec::new();
                }
                if self
                    .state
                    .webhooks
                    .as_ref()
                    .is_some_and(|w| w.room == room && w.loading)
                {
                    return Vec::new();
                }
                self.state.webhooks = Some(crate::webhooks::WebhooksState {
                    room: room.clone(),
                    entries: Vec::new(),
                    loading: true,
                    busy: false,
                    error: None,
                });
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::LoadWebhooks { room },
                }]
            }
            Action::ChangeWebhook(change) => {
                let Some(room) = self.state.webhooks.as_ref().map(|s| s.room.clone()) else {
                    return Vec::new();
                };
                if self
                    .state
                    .channel_settings
                    .as_ref()
                    .is_none_or(|settings| settings.room != room)
                {
                    return Vec::new();
                }
                if !self.state.room(&room).is_some_and(|r| r.capabilities.manage_webhooks) {
                    return Vec::new();
                }
                // Copying only reads, so it needs neither the busy gate nor a request lock.
                if let crate::webhooks::WebhookChange::CopyUrl { id } = &change {
                    let offered = self.state.room(&room).is_some_and(|r| r.capabilities.webhook_url);
                    return if offered && !id.trim().is_empty() {
                        vec![Effect::Backend {
                            account: room.account.clone(),
                            command: Command::ChangeWebhook { room, change },
                        }]
                    } else {
                        Vec::new()
                    };
                }
                let Some(s) = self.state.webhooks.as_mut() else {
                    return Vec::new();
                };
                if s.busy {
                    return Vec::new();
                }
                if matches!(&change, crate::webhooks::WebhookChange::Create { name } if name.trim().is_empty() || name.trim().chars().count() > 32)
                    || matches!(&change, crate::webhooks::WebhookChange::Rename { id, name } if name.trim().is_empty() || name.trim().chars().count() > 32 || id.trim().is_empty())
                    || matches!(&change, crate::webhooks::WebhookChange::Delete { id } if id.trim().is_empty())
                {
                    return Vec::new();
                }
                s.busy = true;
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::ChangeWebhook { room, change },
                }]
            }
            Action::OpenServerSettings(server) => self.dispatch_open_server_settings(server),
            Action::ServerSettingsPage(page) => {
                let Some(s) = &mut self.state.server_settings else {
                    return Vec::new();
                };
                if s.page == page {
                    let retry_log = page == crate::server_settings::ServerPage::ModerationLog
                        && self
                            .state
                            .moderation_logs
                            .get(&s.server)
                            .is_some_and(|log| log.error.is_some());
                    return if retry_log {
                        self.load_moderation_log_first()
                    } else {
                        Vec::new()
                    };
                }
                s.page = page;
                s.notice = None;
                if page == crate::server_settings::ServerPage::ModerationLog {
                    self.load_moderation_log_first()
                } else {
                    self.load_server_page(page)
                }
            }
            Action::LoadMoreModerationLog => {
                let Some(server) = self
                    .state
                    .server_settings
                    .as_ref()
                    .filter(|s| s.page == crate::server_settings::ServerPage::ModerationLog)
                    .map(|s| s.server.clone())
                else {
                    return Vec::new();
                };
                let Some(account) = self
                    .state
                    .server(&server)
                    .filter(|s| s.can_view_moderation_log)
                    .map(|s| s.id.account.clone())
                else {
                    return Vec::new();
                };
                let log = self.state.moderation_logs.entry(server.clone()).or_default();
                if log.loading || (!log.has_more && log.error.is_none()) {
                    return Vec::new();
                }
                log.loading = true;
                log.error = None;
                let before = log.entries.last().map(|e| e.id.clone());
                vec![Effect::Backend {
                    account,
                    command: Command::LoadModerationLog { server, before },
                }]
            }
            Action::ServerAdmin { server, op } => self.dispatch_server_admin(server, op),
            Action::StepUpAnswer { method, code } => {
                let Some(prompt) = self.state.step_up.first_mut().filter(|p| !p.busy) else {
                    return Vec::new();
                };
                if !method.plausible(&code.0) || !prompt.methods.contains(&method) {
                    return Vec::new();
                }
                prompt.busy = true;
                prompt.error = None;
                vec![Effect::Backend {
                    account: prompt.account.clone(),
                    command: Command::StepUpAnswer {
                        id: prompt.id,
                        method,
                        code,
                    },
                }]
            }
            Action::StepUpCancel => {
                if self.state.step_up.is_empty() {
                    return Vec::new();
                }
                let prompt = self.state.step_up.remove(0);
                vec![Effect::Backend {
                    account: prompt.account,
                    command: Command::StepUpCancel { id: prompt.id },
                }]
            }
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
            Action::InviteToActivity(user) => {
                let allowed = self.state.profile.as_ref().is_some_and(|(shown, profile)| {
                    shown == &user && profile.as_ref().is_some_and(|p| p.can_invite_to_activity)
                });
                if allowed {
                    vec![Effect::Backend {
                        account: user.account.clone(),
                        command: Command::InviteToActivity { user },
                    }]
                } else {
                    Vec::new()
                }
            }
            Action::LoadServerProfile(server) => self.dispatch_load_server_profile(server),
            Action::SaveServerProfile { server, edit } => self.dispatch_save_server_profile(server, edit),
            Action::SelectServer(server) => self.dispatch_select_server(server),
            Action::OpenServerEvents(server) => {
                let Some(info) = self.state.server(&server).filter(|s| s.scheduled_events) else {
                    return Vec::new();
                };
                if self.state.showing_server_events.as_ref() == Some(&server) {
                    return Vec::new();
                }
                let account = info.id.account.clone();
                self.state.selected_server = Some(server.clone());
                self.state.server_events_error.remove(&server);
                self.state.showing_friends = false;
                self.state.showing_people = false;
                self.state.showing_active = false;
                self.state.showing_notifications = false;
                self.state.showing_games = false;
                self.state.showing_threads = false;
                self.state.showing_server_events = Some(server.clone());
                if !self.state.server_events_loading.insert(server.clone()) {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account,
                    command: Command::LoadServerEvents { server },
                }]
            }
            Action::CloseServerEvents => {
                self.state.showing_server_events = None;
                Vec::new()
            }
            Action::SetServerEventInterest { event, interested } => {
                let Some(server) = self.state.showing_server_events.clone() else {
                    return Vec::new();
                };
                let Some(server_info) = self.state.server(&server).filter(|s| s.scheduled_events) else {
                    return Vec::new();
                };
                let account = server_info.id.account.clone();
                let Some(entry) = self
                    .state
                    .server_events
                    .get_mut(&server)
                    .and_then(|events| events.iter_mut().find(|e| e.id == event))
                else {
                    return Vec::new();
                };
                if entry.interested == interested {
                    return Vec::new();
                }
                entry.interested = interested;
                entry.interested_count = if interested {
                    entry.interested_count.saturating_add(1)
                } else {
                    entry.interested_count.saturating_sub(1)
                };
                vec![Effect::Backend {
                    account,
                    command: Command::SetServerEventInterest {
                        server,
                        event,
                        interested,
                    },
                }]
            }
            Action::CreateServer { account, name } => {
                let name = name.trim();
                if name.is_empty() || name.chars().count() > 32 || !self.state.accounts.iter().any(|a| a.id == account)
                {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account,
                    command: Command::CreateServer { name: name.to_owned() },
                }]
            }
            Action::SelectRoom(room) => {
                self.state.marked_unread.retain(|r| *r != room);
                // The member search box starts empty in each room.
                self.state.member_filter.clear();
                self.state.member_found = None;
                self.select(room)
            }
            Action::PinRoom { room, pinned } => {
                self.pin_room(room, pinned);
                Vec::new()
            }
            Action::HideRoom { room, hidden } => {
                self.hide_room(room, hidden);
                Vec::new()
            }
            Action::MovePin { room, by } => {
                self.move_pin(&room, by);
                Vec::new()
            }
            Action::RestorePinSeeds(accounts) => {
                self.state.pins_seeded = accounts;
                Vec::new()
            }
            Action::ToggleHiddenRooms => {
                self.state.show_hidden = !self.state.show_hidden;
                Vec::new()
            }
            Action::OpenNotes(account) => self.dispatch_open_notes(account),
            Action::MarkRoomUnread(room) => {
                if self.state.room(&room).is_some() && !self.state.marked_unread.contains(&room) {
                    self.state.marked_unread.push(room);
                }
                Vec::new()
            }
            Action::MarkRoomRead(room) => self.dispatch_mark_room_read(room),
            Action::SetRoomNotify { room, change } => self.dispatch_set_room_notify(room, change),
            Action::RestoreRoomPrefs { pinned, hidden } => {
                self.state.pinned_rooms = pinned;
                self.state.hidden_rooms = hidden;
                Vec::new()
            }
            Action::SetDraft(text) => self.dispatch_set_draft(text),
            Action::Send { now_ms } => self.send(now_ms),
            Action::CancelSend(id) => self.cancel_send(id),
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
            Action::SendSticker { sticker } => {
                let Some(room) = self.state.selected.clone() else {
                    return Vec::new();
                };
                let usable = self
                    .state
                    .stickers_for(&room)
                    .iter()
                    .any(|g| g.stickers.iter().any(|s| s.id == sticker && s.available));
                if !usable || !self.allows(&room, |c| c.send) {
                    return Vec::new();
                }
                let reply_to = match self.state.compose.get(&room) {
                    Some(Compose::Reply(id)) => Some(id.clone()),
                    _ => None,
                };
                if reply_to.is_some() {
                    self.state.compose.remove(&room);
                }
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::SendSticker {
                        room,
                        sticker,
                        reply_to,
                    },
                }]
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
            Action::CreatePoll {
                question,
                answers,
                hours,
                multi,
            } => self.dispatch_create_poll(question, answers, hours, multi),
            Action::EndPoll(message) => self.dispatch_end_poll(message),
            Action::Search(query) => self.dispatch_search(query),
            Action::SearchScope(scope) => self.dispatch_search_scope(scope),
            Action::SearchMore => self.dispatch_search_more(),
            Action::CloseSearch => {
                self.state.search = None;
                Vec::new()
            }
            Action::CreateThread {
                name,
                message,
                text,
                tags,
            } => self.dispatch_create_thread(name, message, text, tags),
            Action::JoinThread(room) => {
                if self.allows(&room, |c| c.join_thread) && self.state.room(&room).is_some_and(|r| !r.joined_thread) {
                    vec![Effect::Backend {
                        account: room.account.clone(),
                        command: Command::JoinThread { room },
                    }]
                } else {
                    Vec::new()
                }
            }
            Action::LeaveThread(room) => {
                if self.allows(&room, |c| c.join_thread) && self.state.room(&room).is_some_and(|r| r.joined_thread) {
                    vec![Effect::Backend {
                        account: room.account.clone(),
                        command: Command::LeaveThread { room },
                    }]
                } else {
                    Vec::new()
                }
            }
            Action::SetForumSort(sort) => self.dispatch_forum(|f| f.set_sort(sort)),
            Action::SetForumTag(tag) => self.dispatch_forum(|f| f.set_tag(tag)),
            Action::LoadMoreForum => self.dispatch_forum_more(),
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
            Action::Report { target, reason, detail } => self.dispatch_report(target, reason, detail),
            Action::OpenGamesInCommon { account, with } => self.dispatch_open_games(account, with),
            Action::PickForGames { user, picked } => self.dispatch_pick_for_games(user, picked),
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
            Action::DeleteFrom(message) => self.dispatch_delete_from(message),
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
            Action::ToggleFileSpoiler(index) => {
                if let Some(room) = self.state.selected.clone()
                    && self.state.room(&room).is_some_and(|r| r.capabilities.file_spoilers)
                    && let Some(file) = self.state.files.get_mut(&room).and_then(|list| list.get_mut(index))
                {
                    file.spoiler = !file.spoiler;
                }
                Vec::new()
            }
            Action::ShowFriends => self.dispatch_show_friends(),
            Action::ShowPeople => self.show_page(super::Page::People),
            Action::ShowActive => self.show_page(super::Page::Active),
            Action::ShowNotifications => self.show_page(super::Page::Notifications),
            Action::ShowThreads => {
                if let Some(server) = self
                    .state
                    .selected_server
                    .clone()
                    .filter(|s| s.account.network.thread_style() == crate::network::ThreadStyle::Panel)
                {
                    let mut effects = self.show_page(super::Page::Threads);
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
            Action::SetPresence {
                account,
                presence,
                choice,
            } => self.dispatch_set_presence(account, presence, choice),
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
                let Some(c) = self.state.contacts.get(contact).cloned() else {
                    return Vec::new();
                };
                let nickname = nickname.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
                if c.nickname == nickname {
                    return Vec::new();
                }
                if let Some(c) = self.state.contacts.get_mut(contact) {
                    c.nickname = nickname.clone();
                }
                let mut effects = vec![Effect::SaveContacts];
                for user in c.identities.into_iter().filter(|u| u.network().keeps_nicknames()) {
                    effects.push(Effect::Backend {
                        account: user.account.clone(),
                        command: Command::SetNickname {
                            user,
                            nickname: nickname.clone().unwrap_or_default(),
                        },
                    });
                }
                effects
            }
            Action::SetContactFavourite { contact, favourite } => {
                let Some(c) = self.state.contacts.get_mut(contact) else {
                    return Vec::new();
                };
                if c.favourite == favourite {
                    return Vec::new();
                }
                c.favourite = favourite;
                vec![Effect::SaveContacts]
            }
            Action::OpenDirect(user) => self.dispatch_open_direct(user),
            Action::CreateGroup { account, name, users } => {
                let name = name.trim().to_owned();
                let valid_account =
                    account.network.can_create_group() && self.state.accounts.iter().any(|a| a.id == account);
                let unique: std::collections::HashSet<_> = users.iter().collect();
                let within_limit = account
                    .network
                    .create_group_member_limit()
                    .is_none_or(|limit| users.len() < limit);
                if !valid_account
                    || name.is_empty()
                    || users.len() < 2
                    || !within_limit
                    || unique.len() != users.len()
                    || users.iter().any(|u| u.account != account)
                {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account,
                    command: Command::CreateGroup { name, users },
                }]
            }
            Action::LeaveGroup(room) => {
                if !self
                    .state
                    .room(&room)
                    .is_some_and(|r| matches!(r.kind, crate::room::RoomKind::Group) && r.capabilities.leave)
                {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::LeaveGroup { room },
                }]
            }
            Action::CloseRoom(room) => {
                if !self
                    .state
                    .room(&room)
                    .is_some_and(|r| matches!(r.kind, crate::room::RoomKind::Direct { .. }) && r.capabilities.close)
                {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::CloseRoom { room },
                }]
            }
            Action::AddFriend(user) => vec![Effect::Backend {
                account: user.account.clone(),
                command: Command::AddFriend { user },
            }],
            Action::SetFavourite { users, favourite } => self.dispatch_set_favourite(users, favourite),
            Action::RemoveFriend(user) => vec![Effect::Backend {
                account: user.account.clone(),
                command: Command::RemoveFriend { user },
            }],
            Action::BlockUser(user) => {
                if !self.shown_profile_can_block(&user) {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: user.account.clone(),
                    command: Command::BlockUser { user },
                }]
            }
            Action::SetNote { user, note } => {
                let allowed = self
                    .state
                    .profile
                    .as_ref()
                    .is_some_and(|(shown, p)| shown == &user && p.as_ref().is_some_and(|p| p.can_note && !p.me));
                if !allowed {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: user.account.clone(),
                    command: Command::SetNote { user, note },
                }]
            }
            Action::UnblockUser(user) => {
                if !self.shown_profile_can_block(&user) {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: user.account.clone(),
                    command: Command::UnblockUser { user },
                }]
            }
            Action::IgnoreUser(user) => {
                let allowed = self.state.profile.as_ref().is_some_and(|(shown, profile)| {
                    shown == &user && profile.as_ref().is_some_and(|p| p.can_ignore && !p.me)
                });
                if !allowed {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: user.account.clone(),
                    command: Command::IgnoreUser { user },
                }]
            }
            Action::UnignoreUser(user) => {
                let allowed = self.state.profile.as_ref().is_some_and(|(shown, profile)| {
                    shown == &user && profile.as_ref().is_some_and(|p| p.can_ignore && !p.me)
                });
                if !allowed {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: user.account.clone(),
                    command: Command::UnignoreUser { user },
                }]
            }
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
            Action::AddGroupMember { room, user } => {
                let permitted = room.account == user.account
                    && self.state.profile.as_ref().is_some_and(|(shown, profile)| {
                        shown == &user
                            && profile.as_ref().is_some_and(|p| {
                                p.group_moderation
                                    .as_ref()
                                    .is_some_and(|m| m.add_member && m.room == room)
                            })
                    });
                if !permitted
                    || !self.state.room(&room).is_some_and(|r| {
                        matches!(r.kind, crate::room::RoomKind::Group) && r.capabilities.add_group_member
                    })
                {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::AddGroupMember { room, user },
                }]
            }
            Action::TransferGroupOwnership { room, user } => {
                let permitted = room.account == user.account
                    && self.state.profile.as_ref().is_some_and(|(shown, profile)| {
                        shown == &user
                            && profile.as_ref().is_some_and(|p| {
                                p.group_moderation
                                    .as_ref()
                                    .is_some_and(|m| m.transfer_ownership && m.room == room)
                            })
                    });
                if !permitted {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::TransferGroupOwnership { room, user },
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
            Action::FilterMembers { query } => {
                if query.is_empty() {
                    self.state.member_found = None;
                }
                self.state.member_filter = query;
                Vec::new()
            }
            Action::SearchMembers { query } => {
                let query = query.trim().to_owned();
                match self.state.selected.clone() {
                    Some(room)
                        if !query.is_empty()
                            && self.state.room(&room).is_some_and(|r| r.capabilities.member_search) =>
                    {
                        vec![Effect::Backend {
                            account: room.account.clone(),
                            command: Command::SearchMembers { room, query },
                        }]
                    }
                    _ => Vec::new(),
                }
            }
            Action::LookUpPeople { room, query } => {
                let query = query.trim().to_owned();
                let asked = self
                    .state
                    .people_asked
                    .as_ref()
                    .is_some_and(|(r, q)| *r == room && *q == query);
                if query.is_empty() || asked || !self.state.room(&room).is_some_and(|r| r.capabilities.member_search) {
                    return Vec::new();
                }
                self.state.people_asked = Some((room.clone(), query.clone()));
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::SearchMembers { room, query },
                }]
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
            Action::PlaySound { sound } => {
                let Some(call) = self.state.call.as_ref() else {
                    return Vec::new();
                };
                let room = call.room.clone();
                if call.status != crate::call::CallStatus::Connected
                    || !self.state.room(&room).is_some_and(|r| r.capabilities.soundboard)
                    || !call.sounds.iter().any(|s| s.id == sound)
                {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::PlaySound { room, sound },
                }]
            }
            Action::LoadCallSounds => {
                let Some(call) = self.state.call.as_ref() else {
                    return Vec::new();
                };
                let room = call.room.clone();
                if !self.state.room(&room).is_some_and(|r| r.capabilities.soundboard) {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::LoadCallSounds { room },
                }]
            }
            Action::DeclineCall(room) => vec![Effect::Backend {
                account: room.account.clone(),
                command: Command::DeclineCall { room },
            }],
            Action::LeaveCall => self.leave_call(),
            Action::SetCallShown(shown) => match &self.state.call {
                Some(call) => vec![Effect::Backend {
                    account: call.room.account.clone(),
                    command: Command::SetCallShown(shown),
                }],
                None => Vec::new(),
            },
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
            Action::Stage { user, request } => {
                let Some(call) = self.state.call.as_ref() else {
                    return Vec::new();
                };
                if call.status != CallStatus::Connected || call.stage.is_none() {
                    return Vec::new();
                }
                vec![Effect::Backend {
                    account: call.room.account.clone(),
                    command: Command::Stage {
                        room: call.room.clone(),
                        user,
                        request,
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
            Action::ForwardMessage {
                room,
                message,
                to,
                comment,
            } => self.dispatch_forward_message(room, message, to, comment),
            Action::ActivateMessageButton {
                room,
                message,
                custom_id,
            } => self.dispatch_activate_message_button(room, message, custom_id),
            Action::ToggleReaction { message, emoji } => self.dispatch_toggle_reaction(message, emoji),
            Action::RemoveReactions { message, emoji } => self.dispatch_remove_reactions(message, emoji),
            Action::ShowReactors { message, emoji } => self.dispatch_show_reactors(message, emoji),
            Action::CloseReactors => {
                self.state.reactors = None;
                Vec::new()
            }
        }
    }

    fn load_moderation_log_first(&mut self) -> Vec<Effect> {
        let Some(server) = self.state.server_settings.as_ref().map(|s| s.server.clone()) else {
            return Vec::new();
        };
        let Some(account) = self
            .state
            .server(&server)
            .filter(|s| s.can_view_moderation_log)
            .map(|s| s.id.account.clone())
        else {
            return Vec::new();
        };
        let log = self.state.moderation_logs.entry(server.clone()).or_default();
        if log.loading || !log.entries.is_empty() {
            return Vec::new();
        }
        log.loading = true;
        log.error = None;
        vec![Effect::Backend {
            account,
            command: Command::LoadModerationLog { server, before: None },
        }]
    }
}
