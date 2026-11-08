// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! User actions that update several parts of application state.
use super::*;

impl Application {
    pub(super) fn dispatch_open_server_settings(&mut self, server: ServerId) -> Vec<Effect> {
        if !server.account.network.has_server_settings() || self.state.server(&server).is_none() {
            return Vec::new();
        }
        if self.state.server_settings.as_ref().is_some_and(|s| s.busy) {
            return Vec::new();
        }
        self.state.server_settings_sequence = self.state.server_settings_sequence.wrapping_add(1);
        let request = self.state.server_settings_sequence;
        // A reload stays on its page; the page's list is fetched again.
        let page = self
            .state
            .server_settings
            .as_ref()
            .filter(|s| s.server == server)
            .map(|s| s.page)
            .unwrap_or_default();
        let mut state = crate::server_settings::ServerSettingsState::new(server.clone(), request);
        state.page = page;
        self.state.server_settings = Some(state);
        let mut effects = vec![Effect::Backend {
            account: server.account.clone(),
            command: Command::LoadServerOverview { server, request },
        }];
        effects.extend(self.load_server_page(page));
        effects
    }

    pub(super) fn dispatch_open_channel_settings(&mut self, room: RoomId) -> Vec<Effect> {
        // Only a room its network lets this account manage.
        if !self.state.room(&room).is_some_and(|r| r.capabilities.manage_channel) {
            return Vec::new();
        }
        if self.state.channel_settings.as_ref().is_some_and(|s| s.busy) {
            return Vec::new();
        }
        self.state.channel_settings_sequence = self.state.channel_settings_sequence.wrapping_add(1);
        let request = self.state.channel_settings_sequence;
        self.state.channel_settings = Some(crate::channel_settings::ChannelSettingsState::new(
            room.clone(),
            request,
        ));
        vec![Effect::Backend {
            account: room.account.clone(),
            command: Command::LoadChannelOverview { room, request },
        }]
    }

    /// A category's settings: the channel page, aimed at the category.
    pub(super) fn dispatch_open_category_settings(
        &mut self,
        server: ServerId,
        category: std::sync::Arc<str>,
    ) -> Vec<Effect> {
        let allowed = self.state.server(&server).is_some_and(|s| {
            s.categories
                .iter()
                .any(|c| c.id.as_ref() == Some(&category) && c.can_manage)
        });
        if !allowed || self.state.channel_settings.as_ref().is_some_and(|s| s.busy) {
            return Vec::new();
        }
        let room = RoomId::new(server.account, &*category);
        self.state.channel_settings_sequence = self.state.channel_settings_sequence.wrapping_add(1);
        let request = self.state.channel_settings_sequence;
        self.state.channel_settings = Some(crate::channel_settings::ChannelSettingsState::new(
            room.clone(),
            request,
        ));
        vec![Effect::Backend {
            account: room.account.clone(),
            command: Command::LoadChannelOverview { room, request },
        }]
    }

    pub(super) fn dispatch_save_channel_overview(
        &mut self,
        edit: crate::channel_settings::ChannelOverviewEdit,
    ) -> Vec<Effect> {
        let Some(s) = &mut self.state.channel_settings else {
            return Vec::new();
        };
        if s.busy || !s.overview.as_ref().is_some_and(|o| o.can_manage) {
            return Vec::new();
        }
        s.error = None;
        s.saved = edit.is_empty();
        if edit.is_empty() {
            return Vec::new();
        }
        // Only what this channel's network offers: slowmode and the icon are
        // network capabilities, and a category has just its name.
        if let Some(o) = &s.overview
            && ((edit.slowmode.is_some() && o.slowmode.is_none())
                || (edit.icon != crate::own_profile::ProfileImage::Keep && !o.icons)
                || (o.is_category && (edit.description.is_some() || edit.mature.is_some())))
        {
            s.error = Some("This channel doesn't have that setting.".into());
            return Vec::new();
        }
        self.state.channel_settings_sequence = self.state.channel_settings_sequence.wrapping_add(1);
        s.request = self.state.channel_settings_sequence;
        s.busy = true;
        vec![Effect::Backend {
            account: s.room.account.clone(),
            command: Command::SaveChannelOverview {
                room: s.room.clone(),
                request: s.request,
                edit: Box::new(edit),
            },
        }]
    }

    pub(super) fn dispatch_delete_channel(&mut self) -> Vec<Effect> {
        let Some(s) = &mut self.state.channel_settings else {
            return Vec::new();
        };
        if s.busy || !s.overview.as_ref().is_some_and(|o| o.can_manage) {
            return Vec::new();
        }
        self.state.channel_settings_sequence = self.state.channel_settings_sequence.wrapping_add(1);
        s.request = self.state.channel_settings_sequence;
        s.busy = true;
        s.saved = false;
        s.error = None;
        vec![Effect::Backend {
            account: s.room.account.clone(),
            command: Command::DeleteChannel {
                room: s.room.clone(),
                request: s.request,
            },
        }]
    }

    /// An override change on the open channel, only what its network
    /// offers this account (`ChannelOverrides::may_set`); None removes it.
    pub(super) fn dispatch_channel_override(
        &mut self,
        target: crate::channel_settings::OverrideTarget,
        change: Option<(u64, u64)>,
    ) -> Vec<Effect> {
        let Some(s) = &mut self.state.channel_settings else {
            return Vec::new();
        };
        let Some(o) = s.overview.as_ref().and_then(|o| o.overrides.as_ref()) else {
            return Vec::new();
        };
        let allowed = match change {
            Some((allow, deny)) => o.may_set(&target, allow, deny),
            // @everyone's entry stays; removing needs an existing entry.
            None => o.can_edit && o.entries.iter().any(|e| e.target == target && !e.everyone),
        };
        if s.busy || !allowed {
            return Vec::new();
        }
        self.state.channel_settings_sequence = self.state.channel_settings_sequence.wrapping_add(1);
        s.request = self.state.channel_settings_sequence;
        s.busy = true;
        s.saved = false;
        s.error = None;
        vec![Effect::Backend {
            account: s.room.account.clone(),
            command: Command::SetChannelOverride {
                room: s.room.clone(),
                request: s.request,
                target,
                change,
            },
        }]
    }

    /// Role, timeout and nickname changes only go out for the member whose
    /// card is open, with the powers the network gave that card, and within
    /// the length limits (so a stale or forged action never reaches a network).
    fn member_change_allowed(&self, server: &ServerId, op: &crate::server_settings::ServerAdmin) -> bool {
        use crate::server_settings::{NICKNAME_MAX, ServerAdmin as A, TIMEOUT_MAX_SECS};
        let Some(target) = op.member_target() else {
            return true;
        };
        let Some((user, Some(profile))) = &self.state.profile else {
            return false;
        };
        let Some(m) = profile
            .moderation
            .as_ref()
            .filter(|m| user == target && m.server == *server)
        else {
            return false;
        };
        match op {
            A::SetMemberRole { role, .. } => m.roles.iter().any(|r| r.id == *role),
            A::Timeout { duration, .. } => {
                m.timeout && duration.is_none_or(|d| (1..=TIMEOUT_MAX_SECS).contains(&d.as_secs()))
            }
            A::SetServerNickname { nick, .. } => {
                m.nickname && nick.as_ref().is_none_or(|n| n.trim().chars().count() <= NICKNAME_MAX)
            }
            _ => true,
        }
    }

    /// Creating and moving channels and categories only goes out with the
    /// rights the network gave this server (a category's parent must be one
    /// of its categories, and only a channel goes inside one).
    fn structure_change_allowed(&self, server: &ServerId, op: &crate::server_settings::ServerAdmin) -> bool {
        use crate::server_settings::{NewChannelKind, ServerAdmin as A};
        let Some(s) = self.state.server(server) else {
            return false;
        };
        match op {
            A::CreateChannel { kind, category, .. } => {
                let kind_ok = match kind {
                    NewChannelKind::Category => s.rights.manage_categories && category.is_none(),
                    _ => true,
                };
                kind_ok
                    && s.rights.create_channels
                    && category
                        .as_ref()
                        // Manage Channels on that category too.
                        .is_none_or(|id| s.categories.iter().any(|c| c.id.as_ref() == Some(id) && c.can_manage))
            }
            A::MoveChannel { id, category, .. } => {
                s.rights.reorder_channels
                    && if *category {
                        s.categories.iter().any(|c| c.id.as_ref() == Some(id))
                    } else {
                        s.rooms().any(|r| r.raw == *id)
                    }
            }
            _ => true,
        }
    }

    pub(super) fn dispatch_server_admin(
        &mut self,
        server: ServerId,
        op: crate::server_settings::ServerAdmin,
    ) -> Vec<Effect> {
        if self.state.server(&server).is_none() {
            return Vec::new();
        }
        if !self.member_change_allowed(&server, &op) || !self.structure_change_allowed(&server, &op) {
            return Vec::new();
        }
        if op == crate::server_settings::ServerAdmin::CreateInvite {
            self.state.invite_link = Some((server.clone(), None));
        }
        if let Some(s) = self.state.server_settings.as_mut().filter(|s| s.server == server)
            && !op.is_load()
        {
            if s.admin_busy {
                return Vec::new();
            }
            // Emoji changes: the capability, and the name and file rules,
            // are checked before anything is sent.
            let emoji_check = match &op {
                crate::server_settings::ServerAdmin::CreateEmoji { name, file } => {
                    Some(s.emoji.as_ref().map_or(Ok(()), |e| e.check_new(name.trim(), file.size)))
                }
                crate::server_settings::ServerAdmin::RenameEmoji { name, .. } => {
                    Some(s.emoji.as_ref().map_or(Ok(()), |e| e.rules.validate_name(name.trim())))
                }
                crate::server_settings::ServerAdmin::DeleteEmoji(_) => Some(Ok(())),
                _ => None,
            };
            if let Some(check) = emoji_check {
                if !s.overview.as_ref().is_some_and(|o| o.can_manage_emoji) {
                    return Vec::new();
                }
                if let Err(e) = check {
                    s.notice = Some(Err(e.into()));
                    return Vec::new();
                }
            }
            s.admin_busy = true;
            s.notice = None;
        }
        vec![Effect::Backend {
            account: server.account.clone(),
            command: Command::ServerAdmin { server, op },
        }]
    }

    pub(super) fn dispatch_save_server_overview(
        &mut self,
        edit: crate::server_settings::ServerOverviewEdit,
    ) -> Vec<Effect> {
        let Some(s) = &mut self.state.server_settings else {
            return Vec::new();
        };
        if s.busy || !s.overview.as_ref().is_some_and(|o| o.can_manage) {
            return Vec::new();
        }
        s.error = None;
        s.saved = edit.is_empty();
        if edit.is_empty() {
            return Vec::new();
        }
        self.state.server_settings_sequence = self.state.server_settings_sequence.wrapping_add(1);
        s.request = self.state.server_settings_sequence;
        s.busy = true;
        vec![Effect::Backend {
            account: s.server.account.clone(),
            command: Command::SaveServerOverview {
                server: s.server.clone(),
                request: s.request,
                edit: Box::new(edit),
            },
        }]
    }

    pub(super) fn dispatch_load_own_profile(&mut self, account: AccountId) -> Vec<Effect> {
        if !self.state.accounts.iter().any(|a| a.id == account) {
            return Vec::new();
        }
        let state = self.state.own_profiles.entry(account.clone()).or_default();
        if state.loading || state.saving {
            return Vec::new();
        }
        state.loading = true;
        state.saved = false;
        state.error = None;
        vec![Effect::Backend {
            account,
            command: Command::LoadOwnProfile,
        }]
    }

    pub(super) fn dispatch_save_own_profile(&mut self, account: AccountId, edit: ProfileEdit) -> Vec<Effect> {
        if !self.state.accounts.iter().any(|a| a.id == account) {
            return Vec::new();
        }
        let Some(state) = self.state.own_profiles.get_mut(&account) else {
            return Vec::new();
        };
        if state.profile.is_none() || state.loading || state.saving {
            return Vec::new();
        }
        state.error = None;
        state.saved = edit.is_empty();
        if edit.is_empty() {
            return Vec::new();
        }
        state.saving = true;
        vec![Effect::Backend {
            account,
            command: Command::SaveOwnProfile(edit),
        }]
    }

    pub(super) fn dispatch_select_server(&mut self, server: Option<ServerId>) -> Vec<Effect> {
        if server.as_ref().is_none_or(|s| self.state.server(s).is_some()) {
            self.state.selected_server = server.clone();
        }
        // Its directory (rooms we could join), once per session.
        let mut effects = match &server {
            Some(s) if self.state.server(s).is_some() && self.directory_asked.insert(s.clone()) => {
                vec![Effect::Backend {
                    account: s.account.clone(),
                    command: Command::LoadDirectory { server: s.clone() },
                }]
            }
            _ => Vec::new(),
        };
        effects.extend(self.open_in_server(server.as_ref()));
        effects
    }

    pub(super) fn dispatch_mark_room_read(&mut self, room: RoomId) -> Vec<Effect> {
        self.state.marked_unread.retain(|r| *r != room);
        let Some(_r) = self.state.room(&room) else {
            return Vec::new();
        };
        // The newest message known here, else the network's own.
        let last = self
            .state
            .timelines
            .get(&room)
            .and_then(|t| t.messages.iter().rev().find(|m| !m.pending))
            .map(|m| m.id.clone());
        let command = match last {
            Some(last) => Command::MarkRead {
                room: room.clone(),
                last,
            },
            None => Command::MarkRoomRead { room: room.clone() },
        };
        self.mark_read_local(
            &room,
            match &command {
                Command::MarkRead { last, .. } => Some(last.clone()),
                _ => None,
            },
        );
        vec![Effect::Backend {
            account: room.account.clone(),
            command,
        }]
    }

    pub(super) fn dispatch_set_draft(&mut self, text: String) -> Vec<Effect> {
        let Some(room) = self.state.selected.clone() else {
            return Vec::new();
        };
        let grew = text.len() > self.state.drafts.get(&room).map_or(0, String::len);
        if text.is_empty() {
            self.state.drafts.remove(&room);
        } else {
            self.state.drafts.insert(room.clone(), text);
        }
        // Typing (not deleting, not editing) tells the others.
        if grew && !matches!(self.state.compose.get(&room), Some(Compose::Edit(_))) {
            vec![Effect::Backend {
                account: room.account.clone(),
                command: Command::Typing { room },
            }]
        } else {
            Vec::new()
        }
    }

    pub(super) fn dispatch_search_gifs(&mut self, query: String, more: bool) -> Vec<Effect> {
        let Some(room) = self.state.selected.clone().filter(|r| self.allows(r, |c| c.gifs)) else {
            return Vec::new();
        };
        let picker = self.state.gifs.entry(room.account.clone()).or_default();
        let query = query.trim().to_lowercase();
        let position = if more {
            // Only the shown search continues, one page at a time.
            if query != picker.query || picker.loading {
                return Vec::new();
            }
            let Some(next) = picker.next.clone() else {
                return Vec::new();
            };
            Some(next)
        } else {
            if query == picker.query && (picker.loading || !picker.results.is_empty()) {
                return Vec::new();
            }
            picker.results.clear();
            picker.next = None;
            None
        };
        picker.query = query.clone();
        picker.error = None;
        if query.is_empty() {
            picker.loading = false;
            return Vec::new();
        }
        picker.loading = true;
        vec![Effect::Backend {
            account: room.account,
            command: Command::SearchGifs { query, position },
        }]
    }

    pub(super) fn dispatch_edit_last(&mut self) -> Vec<Effect> {
        let Some(room) = self.state.selected.clone() else {
            return Vec::new();
        };
        if self.state.drafts.get(&room).is_some_and(|d| !d.is_empty()) {
            return Vec::new();
        }
        let last = self.state.timelines.get(&room).and_then(|t| {
            t.messages
                .iter()
                .rev()
                .find(|m| self.state.is_mine(m) && !m.pending && !m.system)
                .map(|m| m.id.clone())
        });
        match last {
            Some(id) => self.dispatch(Action::StartEdit(id)),
            None => Vec::new(),
        }
    }

    pub(super) fn dispatch_jump_to(&mut self, message: MessageId) -> Vec<Effect> {
        let Some(room) = self.state.selected.clone() else {
            return Vec::new();
        };
        if self.state.selected_message(&message).is_some() {
            self.mark_jump(message);
            return Vec::new();
        }
        if let Some(t) = self.state.timelines.get_mut(&room) {
            t.loading = true;
        }
        self.state.jump_pending = Some(message.clone());
        vec![Effect::Backend {
            account: room.account.clone(),
            command: Command::LoadAround { room, message },
        }]
    }

    pub(super) fn dispatch_search(&mut self, query: String) -> Vec<Effect> {
        let Some(room) = self.state.selected.clone() else {
            return Vec::new();
        };
        let scope = self
            .state
            .search
            .as_ref()
            .filter(|s| s.room == room)
            .map_or_else(Default::default, |s| s.scope);
        self.run_search(room, query, scope)
    }

    /// The same words, in the other scope (only where the network has one).
    pub(super) fn dispatch_search_scope(&mut self, scope: crate::search::SearchScope) -> Vec<Effect> {
        let Some(s) = self.state.search.as_ref() else {
            return Vec::new();
        };
        let (room, query) = (s.room.clone(), s.query.clone());
        if !self.allows(&room, |c| c.search_filters.has(crate::search::SearchFilter::Scope)) || s.scope == scope {
            return Vec::new();
        }
        self.run_search(room, query, scope)
    }

    /// "Load more": the next page of the shown search, once at a time.
    pub(super) fn dispatch_search_more(&mut self) -> Vec<Effect> {
        let Some(s) = self.state.search.as_mut() else {
            return Vec::new();
        };
        let (Some(query), Some(offset)) = (s.parsed.clone(), s.next_offset) else {
            return Vec::new();
        };
        if s.loading_more || s.messages.is_none() {
            return Vec::new();
        }
        s.loading_more = true;
        let room = s.room.clone();
        vec![Effect::Backend {
            account: room.account.clone(),
            command: Command::SearchMessages { room, query, offset },
        }]
    }

    /// Who a typed `from:` or `mentions:` can name: the room's members, its
    /// recent authors, the other person of a DM.
    fn search_people(&self, room: &RoomId) -> Vec<(UserId, String)> {
        let mut people: Vec<(UserId, String)> = Vec::new();
        let mut seen: std::collections::HashSet<(UserId, String)> = std::collections::HashSet::new();
        let mut add = |id: &UserId, name: &str| {
            if seen.insert((id.clone(), name.to_owned())) {
                people.push((id.clone(), name.to_owned()));
            }
        };
        for group in self.state.members.get(room).into_iter().flat_map(|g| g.iter()) {
            for m in &group.members {
                add(&m.user, &m.name);
            }
        }
        for m in self
            .state
            .timelines
            .get(room)
            .into_iter()
            .flat_map(|t| t.messages.iter())
        {
            add(&m.author, &m.author_name);
        }
        if let Some(r) = self.state.room(room)
            && let RoomKind::Direct { with } = &r.kind
        {
            add(with, &r.name);
        }
        people
    }

    /// The other channels of the room's server, for `in:#channel`.
    fn search_channels(&self, room: &RoomId) -> Vec<(RoomId, String)> {
        let Some(RoomKind::Channel { server }) = self.state.room(room).map(|r| r.kind.clone()) else {
            return Vec::new();
        };
        self.state
            .rooms
            .iter()
            .filter(|r| {
                r.id.account == room.account && matches!(&r.kind, RoomKind::Channel { server: s } if *s == server)
            })
            .map(|r| (r.id.clone(), r.name.to_string()))
            .collect()
    }

    fn run_search(&mut self, room: RoomId, query: String, scope: crate::search::SearchScope) -> Vec<Effect> {
        if self.state.room(&room).is_none() {
            return Vec::new();
        }
        let remote = self.allows(&room, |c| c.search);
        let filters = if remote {
            self.state
                .room(&room)
                .map_or_else(Default::default, |r| r.capabilities.search_filters)
        } else {
            crate::search::SearchFilters::NONE
        };
        let query = query.trim().to_owned();
        let asked = !query.is_empty();
        // The same search again (Enter pressed twice) while it's
        // loading or showing its results: not another request.
        if asked
            && self
                .state
                .search
                .as_ref()
                .is_some_and(|s| s.room == room && s.query == query && s.scope == scope && !s.failed)
        {
            return Vec::new();
        }
        let mut search = Search {
            room: room.clone(),
            query: query.clone(),
            parsed: None,
            scope,
            messages: if asked { None } else { Some(Vec::new()) },
            rooms: Vec::new(),
            failed: false,
            errors: Vec::new(),
            notice: None,
            total: None,
            next_offset: None,
            loading_more: false,
        };
        if asked && remote {
            let parsed = crate::search::parse(&query, filters);
            // Only look people and channels up when a filter names them.
            let people = if parsed.from.is_some() || parsed.mentions.is_some() {
                self.search_people(&room)
            } else {
                Vec::new()
            };
            let channels = if parsed.in_channel.is_some() {
                self.search_channels(&room)
            } else {
                Vec::new()
            };
            match crate::search::resolve(parsed, scope, &people, &channels) {
                Ok(q) => search.parsed = Some(q),
                Err(errors) => {
                    // Nothing is sent: say what didn't match.
                    search.errors = errors;
                    search.messages = Some(Vec::new());
                }
            }
        }
        let parsed = search.parsed.clone();
        self.state.search = Some(search);
        if !asked || (remote && parsed.is_none()) {
            return Vec::new();
        }
        if !remote {
            let query = query.to_lowercase();
            let messages = self
                .state
                .timelines
                .get(&room)
                .into_iter()
                .flat_map(|timeline| timeline.messages.iter())
                .filter(|message| {
                    message.source.to_lowercase().contains(&query)
                        || message
                            .snippet
                            .as_deref()
                            .is_some_and(|snippet| snippet.to_lowercase().contains(&query))
                })
                .cloned()
                .collect();
            if let Some(search) = self.state.search.as_mut() {
                search.messages = Some(messages);
            }
            return Vec::new();
        }
        vec![Effect::Backend {
            account: room.account.clone(),
            command: Command::SearchMessages {
                room,
                query: parsed.unwrap_or_default(),
                offset: 0,
            },
        }]
    }

    pub(super) fn dispatch_create_thread(
        &mut self,
        name: String,
        message: Option<MessageId>,
        text: String,
        tags: Vec<Arc<str>>,
    ) -> Vec<Effect> {
        let Some(room) = self.state.selected.clone().filter(|r| self.allows(r, |c| c.threads)) else {
            return Vec::new();
        };
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Vec::new();
        }
        // A forum's post takes only tags the forum has (moderated ones only
        // for those who may), and at least one where the forum requires it.
        let tags = if message.is_none() && self.allows(&room, |c| c.forum) {
            match self.state.forum.as_ref().filter(|f| f.room == room) {
                Some(f) => match f.post_tags(&tags) {
                    Some(tags) => tags,
                    None => return Vec::new(),
                },
                // Without the forum's rules, a required tag can't be checked.
                None => {
                    self.state
                        .errors
                        .push("This forum's tags haven't loaded yet, try again in a moment.".into());
                    return Vec::new();
                }
            }
        } else {
            Vec::new()
        };
        vec![Effect::Backend {
            account: room.account.clone(),
            command: Command::CreateThread {
                room,
                name,
                message,
                text,
                tags,
            },
        }]
    }

    /// Changes how the open forum lists its posts (a new order or filter
    /// drops the older posts, which load again on request).
    pub(super) fn dispatch_forum(&mut self, change: impl FnOnce(&mut crate::forum::Forum)) -> Vec<Effect> {
        if let Some(f) = self.state.forum.as_mut() {
            change(f);
        }
        Vec::new()
    }

    /// "Load older posts": the next page of the open forum's archived posts.
    pub(super) fn dispatch_forum_more(&mut self) -> Vec<Effect> {
        let Some(f) = self.state.forum.as_mut() else {
            return Vec::new();
        };
        let Some(offset) = f.older.filter(|_| !f.loading_older) else {
            return Vec::new();
        };
        f.loading_older = true;
        vec![Effect::Backend {
            account: f.room.account.clone(),
            command: Command::LoadForum {
                room: f.room.clone(),
                sort: f.sort,
                tag: f.tag.clone(),
                offset: Some(offset),
            },
        }]
    }

    /// A report, only where the capability says the network takes it, and
    /// only with a reason from the target's own list.
    pub(super) fn dispatch_report(
        &mut self,
        target: crate::report::ReportTarget,
        reason: crate::report::ReportReason,
        detail: String,
    ) -> Vec<Effect> {
        use crate::report::ReportTarget as T;
        let refused = |this: &mut Self| {
            this.state.report_outcome = Some(Err("Reporting isn't available here.".into()));
            Vec::new()
        };
        if !target.reasons().contains(&reason) {
            return refused(self);
        }
        let (account, allowed) = match &target {
            T::User { user, .. } => {
                let known = self
                    .state
                    .profile
                    .as_ref()
                    .filter(|(u, _)| u == user)
                    .and_then(|(_, p)| p.as_ref())
                    .or_else(|| self.state.profiles.get(user));
                (user.account.clone(), known.is_some_and(|p| p.can_report && !p.me))
            }
            T::Message { room, .. } => (room.account.clone(), self.allows(room, |c| c.report)),
            T::Server { server } => (
                server.account.clone(),
                self.state.server(server).is_some_and(|s| s.rights.report),
            ),
        };
        if !allowed {
            return refused(self);
        }
        self.state.report_outcome = None;
        vec![Effect::Backend {
            account,
            command: Command::Report {
                target,
                reason,
                detail: detail.trim().to_owned(),
            },
        }]
    }

    pub(super) fn dispatch_set_pinned(&mut self, message: MessageId, pinned: bool) -> Vec<Effect> {
        let Some(room) = self
            .state
            .selected
            .clone()
            .filter(|r| self.allows(r, |c| c.pin_messages))
        else {
            return Vec::new();
        };
        // Unpinning leaves the open list at once; the network's answer
        // replaces it.
        if !pinned
            && let Some(p) = self.state.pins.as_mut().filter(|p| p.room == room)
            && let Some(list) = p.messages.as_mut()
        {
            list.retain(|m| m.id != message);
        }
        vec![Effect::Backend {
            account: room.account.clone(),
            command: Command::SetPinned { room, message, pinned },
        }]
    }

    pub(super) fn dispatch_jump_to_present(&mut self) -> Vec<Effect> {
        let Some(room) = self.state.selected.clone() else {
            return Vec::new();
        };
        let Some(t) = self.state.timelines.get_mut(&room) else {
            return Vec::new();
        };
        if !t.detached {
            return Vec::new();
        }
        t.loading = true;
        vec![Effect::Backend {
            account: room.account.clone(),
            command: Command::LoadHistory { room, before: None },
        }]
    }

    pub(super) fn dispatch_delete(&mut self, message: MessageId) -> Vec<Effect> {
        let Some(room) = self.state.selected.clone() else {
            return Vec::new();
        };
        let Some(target) = self.state.selected_message(&message) else {
            return Vec::new();
        };
        let allowed =
            self.allows(&room, |c| c.delete_others) || (self.state.is_mine(target) && self.allows(&room, |c| c.delete));
        if target.pending || !allowed {
            return Vec::new();
        }
        vec![Effect::Backend {
            account: room.account.clone(),
            command: Command::DeleteMessage { room, message },
        }]
    }

    pub(super) fn dispatch_remove_reactions(&mut self, message: MessageId, emoji: Option<Arc<str>>) -> Vec<Effect> {
        let Some(room) = self.state.selected.clone() else {
            return Vec::new();
        };
        if !self.allows(&room, |c| c.moderate_reactions)
            || !self
                .state
                .selected_message(&message)
                .is_some_and(|m| !m.pending && !m.reactions.is_empty())
        {
            return Vec::new();
        }
        vec![Effect::Backend {
            account: room.account.clone(),
            command: Command::RemoveReactions { room, message, emoji },
        }]
    }

    pub(super) fn dispatch_add_files(&mut self, files: Vec<Outgoing>) -> Vec<Effect> {
        let Some(room) = self.state.selected.clone() else {
            return Vec::new();
        };
        let Some(caps) = self.state.room(&room).map(|r| r.capabilities) else {
            return Vec::new();
        };
        if !caps.attachments {
            return Vec::new();
        }
        // Files over the network's limit are left out, saying why,
        // rather than failing when sent.
        let (fit, over): (Vec<Outgoing>, Vec<Outgoing>) = files
            .into_iter()
            .partition(|f| caps.max_upload == 0 || f.size <= caps.max_upload);
        for f in &over {
            self.state.errors.push(
                format!(
                    "{} is {}; {} takes at most {} here.",
                    f.name,
                    megabytes(f.size),
                    room.network().name(),
                    megabytes(caps.max_upload)
                )
                .into(),
            );
        }
        if !fit.is_empty() {
            self.state.files.entry(room).or_default().extend(fit);
        }
        Vec::new()
    }

    pub(super) fn dispatch_show_friends(&mut self) -> Vec<Effect> {
        self.state.friend_request_status = None;
        self.state.showing_friends = true;
        self.state.showing_people = false;
        self.state.showing_active = false;
        self.state.showing_notifications = false;
        self.state.showing_games = false;
        self.state.games = None;
        self.state.showing_threads = false;
        self.state.selected_server = None;
        self.state
            .selected
            .as_ref()
            .map(|room| {
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::Focus { room: None },
                }]
            })
            .unwrap_or_default()
    }

    pub(super) fn dispatch_split_identity(&mut self, user: UserId) -> Vec<Effect> {
        // Each side keeps a name of its own: the split-off account
        // its network's name for it (not its id), the rest theirs
        // if the merged name was the one leaving.
        let name = self.known_name(&user).unwrap_or_else(|| user.raw.to_string());
        let Some(was) = self.state.contacts.of(&user).map(|c| (c.id, c.name.clone())) else {
            return Vec::new();
        };
        if self.state.contacts.split(&user, &name).is_none() {
            return Vec::new();
        }
        if was.1 == name {
            let rest = self
                .state
                .contacts
                .get(was.0)
                .and_then(|c| c.identities.iter().find_map(|u| self.known_name(u)));
            if let (Some(rest), Some(c)) = (rest, self.state.contacts.get_mut(was.0)) {
                c.name = rest;
            }
        }
        self.refresh_nudge();
        vec![Effect::SaveContacts]
    }

    pub(super) fn dispatch_link_from(&mut self, user: UserId) -> Vec<Effect> {
        let name = match &self.state.profile {
            Some((u, Some(p))) if *u == user => p.name.to_string(),
            _ => user.raw.to_string(),
        };
        let added = self.state.contacts.of(&user).is_none();
        let contact = self.state.contacts.ensure(user, &name);
        let n = self.state.link_request.as_ref().map_or(0, |(_, n)| *n) + 1;
        self.state.link_request = Some((contact, n));
        self.state.profile = None;
        // The same way into People as the button: the open conversation
        // loses focus, so nothing is marked read or kept streaming unseen.
        let mut effects = self.dispatch(Action::ShowPeople);
        if added {
            effects.push(Effect::SaveContacts);
        }
        effects
    }

    pub(super) fn dispatch_set_presence(&mut self, account: AccountId, presence: Presence) -> Vec<Effect> {
        let Some(a) = self
            .state
            .accounts
            .iter_mut()
            .find(|a| a.id == account && a.presence.is_some())
        else {
            return Vec::new();
        };
        // Shown at once; the network confirms (or corrects) it.
        a.presence = Some(presence);
        vec![Effect::Backend {
            account,
            command: Command::SetPresence(presence),
        }]
    }

    pub(super) fn dispatch_set_status_text(&mut self, account: AccountId, text: Option<String>) -> Vec<Effect> {
        // The network's limit (Stoat's UserStatus.text: 128).
        let text: Option<String> = text
            .map(|t| t.trim().chars().take(128).collect::<String>())
            .filter(|t| !t.is_empty());
        let Some(a) = self
            .state
            .accounts
            .iter_mut()
            .find(|a| a.id == account && a.presence.is_some())
        else {
            return Vec::new();
        };
        // Shown at once; the network confirms it.
        a.status = text.as_deref().map(Into::into);
        vec![Effect::Backend {
            account,
            command: Command::SetStatusText(text),
        }]
    }

    pub(super) fn dispatch_answer_verification(&mut self, matches: bool) -> Vec<Effect> {
        let Some((account, Verify::Compare(_))) = self.state.verification.clone() else {
            return Vec::new();
        };
        self.state.verification = Some((
            account.clone(),
            if matches {
                Verify::Confirmed
            } else {
                Verify::Failed("The emoji didn't match, so nothing was verified.".into())
            },
        ));
        vec![Effect::Backend {
            account,
            command: Command::AnswerVerification { matches },
        }]
    }

    /// Stars or unstars `users` on the networks that keep favourites, for
    /// the accounts signed in; shown at once (the network's own list
    /// follows and replaces it).
    pub(super) fn dispatch_set_favourite(&mut self, users: Vec<UserId>, favourite: bool) -> Vec<Effect> {
        let mut effects = Vec::new();
        for user in users {
            let signed_in = self.state.accounts.iter().any(|a| a.id == user.account);
            if !signed_in || !user.network().keeps_favourites() || self.state.favourites.contains(&user) == favourite {
                continue;
            }
            if favourite {
                self.state.favourites.insert(user.clone());
            } else {
                self.state.favourites.remove(&user);
            }
            effects.push(Effect::Backend {
                account: user.account.clone(),
                command: Command::SetFavourite { user, favourite },
            });
        }
        effects
    }

    pub(super) fn dispatch_open_direct(&mut self, user: UserId) -> Vec<Effect> {
        // An existing chat opens straight away.
        let existing = self
            .state
            .rooms
            .iter()
            .find(|r| matches!(&r.kind, RoomKind::Direct { with } if with == &user))
            .map(|r| r.id.clone());
        match existing {
            Some(room) => self.select(room),
            None => vec![Effect::Backend {
                account: user.account.clone(),
                command: Command::OpenDirect { user },
            }],
        }
    }

    pub(super) fn dispatch_open_notes(&mut self, account: AccountId) -> Vec<Effect> {
        if !account.network.has_notes_to_self() || !self.state.accounts.iter().any(|a| a.id == account) {
            return Vec::new();
        }
        if let Some(room) = self
            .state
            .rooms
            .iter()
            .find(|r| r.id.account == account && matches!(r.kind, RoomKind::Notes))
            .map(|r| r.id.clone())
        {
            return self.select(room);
        }
        // Not there yet: opening a DM with yourself makes it (Stoat's
        // `User.openDM()` on the own user, stoat.js `User.ts`).
        let user = UserId::new(account.clone(), account.user.clone());
        vec![Effect::Backend {
            account,
            command: Command::OpenDirect { user },
        }]
    }

    pub(super) fn dispatch_load_invite(&mut self, account: AccountId, code: Arc<str>) -> Vec<Effect> {
        let key = (account.clone(), code.clone());
        if self
            .state
            .invites
            .get(&key)
            .is_some_and(|invite| !matches!(invite, crate::message::Invite::Invalid))
        {
            return Vec::new();
        }
        self.state.invites.insert(key, crate::message::Invite::Loading);
        vec![Effect::Backend {
            account,
            command: Command::LoadInvite { code },
        }]
    }

    pub(super) fn dispatch_send_friend_request(&mut self, account: AccountId, name: String) -> Vec<Effect> {
        let name = name.trim().to_owned();
        let account = self
            .state
            .accounts
            .iter()
            .find(|a| a.id == account && a.id.network.has_friends())
            .map(|a| a.id.clone());
        match account {
            Some(account) if !name.is_empty() => {
                self.state.friend_request_status = None;
                self.state.friend_request_sequence = self.state.friend_request_sequence.wrapping_add(1);
                let request = self.state.friend_request_sequence;
                self.state.friend_request_pending = Some((account.clone(), request));
                vec![Effect::Backend {
                    account,
                    command: Command::SendFriendRequest { request, name },
                }]
            }
            _ => Vec::new(),
        }
    }

    pub(super) fn dispatch_join_call(&mut self, room: RoomId) -> Vec<Effect> {
        // Notes (a chat with yourself) never has a call, whatever a network's
        // capabilities say.
        if !self
            .state
            .room(&room)
            .is_some_and(|r| r.capabilities.voice && !matches!(r.kind, RoomKind::Notes))
            || self.state.call.as_ref().is_some_and(|c| c.room == room)
        {
            return Vec::new();
        }
        let mut effects = self.leave_call();
        self.state.call = Some(Call {
            room: room.clone(),
            status: CallStatus::Connecting,
            latency_ms: None,
            members: Vec::new(),
            // Our own mute and deafen carry into the call.
            muted: self.state.self_muted,
            deafened: self.state.self_deafened,
            camera: false,
            screen_share: false,
            videos: Vec::new(),
            streams: Vec::new(),
        });
        effects.push(Effect::Backend {
            account: room.account.clone(),
            command: Command::JoinCall { room: room.clone() },
        });
        // Backends start unmuted; the call is told straight after joining.
        for command in [
            self.state.self_muted.then_some(Command::SetMuted(true)),
            self.state.self_deafened.then_some(Command::SetDeafened(true)),
        ]
        .into_iter()
        .flatten()
        {
            effects.push(Effect::Backend {
                account: room.account.clone(),
                command,
            });
        }
        effects
    }

    pub(super) fn dispatch_watch_stream(&mut self, user: UserId, watch: bool) -> Vec<Effect> {
        let Some(call) = self.state.call.as_ref() else {
            return Vec::new();
        };
        if call.status != CallStatus::Connected
            || call
                .streams
                .iter()
                .find(|s| s.user == user)
                .is_none_or(|s| s.watching == watch)
        {
            return Vec::new();
        }
        vec![Effect::WatchStream {
            account: call.room.account.clone(),
            room: call.room.clone(),
            user,
            watch,
        }]
    }

    /// Only rooms whose network takes per-room notification settings.
    pub(super) fn dispatch_set_room_notify(
        &mut self,
        room: RoomId,
        change: crate::room::RoomNotifyChange,
    ) -> Vec<Effect> {
        use crate::room::RoomNotifyChange as C;
        if !self.state.room(&room).is_some_and(|r| r.capabilities.notify_settings) {
            return Vec::new();
        }
        let change = match change {
            C::Level(level) => C::Level(level.map(|l| l.min(2))),
            other => other,
        };
        vec![Effect::Backend {
            account: room.account.clone(),
            command: Command::SetRoomNotify { room, change },
        }]
    }

    pub(super) fn dispatch_mark_unread(&mut self, message: MessageId) -> Vec<Effect> {
        let Some(room) = self.state.selected.clone() else {
            return Vec::new();
        };
        if !self.state.room(&room).is_some_and(|r| r.capabilities.mark_unread) {
            return Vec::new();
        }
        let Some(list) = self.state.timelines.get(&room).map(|t| t.messages.clone()) else {
            return Vec::new();
        };
        let Some(at) = list.iter().position(|m| m.id == message) else {
            return Vec::new();
        };
        let last_read = list[..at].iter().rev().find(|m| !m.pending).map(|m| m.id.clone());
        vec![Effect::Backend {
            account: room.account.clone(),
            command: Command::MarkUnread { room, last_read },
        }]
    }

    pub(super) fn dispatch_forward_message(
        &mut self,
        room: RoomId,
        message: MessageId,
        to: RoomId,
        comment: String,
    ) -> Vec<Effect> {
        let room_takes = |r: &RoomId, f: fn(&crate::room::Capabilities) -> bool| {
            self.state.room(r).is_some_and(|r| f(&r.capabilities))
        };
        // Only what the network lets be forwarded (and not one still on its
        // way out), between rooms of one account.
        let allowed = room.account == to.account
            && room_takes(&room, |c| c.forward_from)
            && room_takes(&to, |c| c.forward_to)
            && self.state.timelines.get(&room).is_some_and(|t| {
                t.messages
                    .iter()
                    .any(|m| m.id == message && m.forwardable && !m.pending)
            });
        if !allowed {
            self.state.errors.push("This message can't be forwarded there.".into());
            return Vec::new();
        }
        vec![Effect::Backend {
            account: room.account.clone(),
            command: Command::ForwardMessage {
                room,
                message,
                to,
                comment: comment.trim().to_owned(),
            },
        }]
    }

    pub(super) fn dispatch_create_poll(
        &mut self,
        question: String,
        answers: Vec<String>,
        hours: u32,
        multi: bool,
    ) -> Vec<Effect> {
        let Some(room) = self.state.selected.clone() else {
            return Vec::new();
        };
        let limits = self
            .state
            .room(&room)
            .filter(|r| r.capabilities.send)
            .and_then(|r| r.capabilities.polls);
        let Some(limits) = limits else {
            self.state.errors.push("Polls can't be created here.".into());
            return Vec::new();
        };
        if !crate::poll::valid_duration(hours) {
            self.state.errors.push("Pick how long the poll runs.".into());
            return Vec::new();
        }
        match limits.check(&question, &answers) {
            Ok((question, answers)) => vec![Effect::Backend {
                account: room.account.clone(),
                command: Command::CreatePoll {
                    room,
                    question,
                    answers,
                    hours,
                    multi,
                },
            }],
            Err(why) => {
                self.state.errors.push(why.into());
                Vec::new()
            }
        }
    }

    pub(super) fn dispatch_end_poll(&mut self, message: MessageId) -> Vec<Effect> {
        let Some(room) = self.state.selected.clone() else {
            return Vec::new();
        };
        let endable = self.state.timelines.get(&room).is_some_and(|t| {
            t.messages
                .iter()
                .any(|m| m.id == message && m.poll_endable && !m.pending)
        });
        if !endable {
            self.state.errors.push("This poll can't be ended.".into());
            return Vec::new();
        }
        vec![Effect::Backend {
            account: room.account.clone(),
            command: Command::EndPoll { room, message },
        }]
    }

    pub(super) fn dispatch_toggle_reaction(&mut self, message: MessageId, emoji: Arc<str>) -> Vec<Effect> {
        let Some(room) = self.state.selected.clone() else {
            return Vec::new();
        };
        let Some(m) = self.state.selected_message(&message) else {
            return Vec::new();
        };
        if m.pending || !self.state.room(&room).is_some_and(|r| r.capabilities.react) {
            return Vec::new();
        }
        let add = !m.reactions.iter().any(|r| r.emoji == emoji && r.me);
        vec![Effect::Backend {
            account: room.account.clone(),
            command: Command::React {
                room,
                message,
                emoji,
                add,
            },
        }]
    }

    /// "Games in common…": us and `with` (people of the same account), and
    /// a request for every library. The network keeps what it loaded for
    /// the session, so opening the dialog again costs no new requests.
    pub(super) fn dispatch_open_games(&mut self, account: AccountId, with: Vec<UserId>) -> Vec<Effect> {
        if !account.network.has_game_libraries() || !self.state.accounts.iter().any(|a| a.id == account) {
            return Vec::new();
        }
        let me = UserId::new(account.clone(), account.user.as_ref());
        let mut people = vec![me];
        for user in with {
            if user.account == account && !people.contains(&user) && people.len() < crate::games::MAX_PEOPLE {
                people.push(user);
            }
        }
        let mut effects = self.show_page(super::Page::Games);
        self.state.games = Some(GamesInCommon {
            account: account.clone(),
            people: people.clone(),
            libraries: HashMap::new(),
        });
        effects.push(Effect::Backend {
            account,
            command: Command::LoadGameLibraries { users: people },
        });
        effects
    }

    /// A checkbox in "Games in common": someone joins the comparison (their
    /// library loads unless it's known) or leaves it. We always stay in.
    pub(super) fn dispatch_pick_for_games(&mut self, user: UserId, picked: bool) -> Vec<Effect> {
        let Some(g) = self.state.games.as_mut().filter(|g| g.account == user.account) else {
            return Vec::new();
        };
        if user.raw == user.account.user {
            return Vec::new();
        }
        if !picked {
            g.people.retain(|p| *p != user);
            return Vec::new();
        }
        if g.people.contains(&user) || g.people.len() >= crate::games::MAX_PEOPLE {
            return Vec::new();
        }
        g.people.push(user.clone());
        if g.libraries.contains_key(&user) {
            return Vec::new();
        }
        vec![Effect::Backend {
            account: user.account.clone(),
            command: Command::LoadGameLibraries { users: vec![user] },
        }]
    }
}
