// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Backend events that update several parts of application state.
use super::*;

impl Application {
    pub(super) fn receive_server_overview(
        &mut self,
        server: ServerId,
        request: u64,
        result: Result<crate::server_settings::ServerOverview, Arc<str>>,
    ) -> Vec<Effect> {
        if self.state.server(&server).is_none() {
            return Vec::new();
        }
        let Some(s) = &mut self.state.server_settings else {
            return Vec::new();
        };
        if s.server != server || s.request != request || !s.busy {
            return Vec::new();
        }
        s.busy = false;
        match result {
            Ok(overview) => {
                s.saved = s.overview.is_some();
                s.overview = Some(overview);
                s.error = None;
            }
            Err(error) => {
                s.error = Some(error);
                s.saved = false;
            }
        }
        Vec::new()
    }

    pub(super) fn receive_channel_overview(
        &mut self,
        room: RoomId,
        request: u64,
        result: Result<Option<crate::channel_settings::ChannelOverview>, Arc<str>>,
    ) -> Vec<Effect> {
        let Some(s) = &mut self.state.channel_settings else {
            return Vec::new();
        };
        if s.room != room || s.request != request || !s.busy {
            return Vec::new();
        }
        s.busy = false;
        match result {
            Ok(Some(overview)) => {
                s.saved = s.overview.is_some();
                s.overview = Some(overview);
                s.error = None;
            }
            // Deleted: nothing left to edit.
            Ok(None) => self.state.channel_settings = None,
            Err(error) => {
                s.error = Some(error);
                s.saved = false;
            }
        }
        Vec::new()
    }

    pub(super) fn receive_account_ready(
        &mut self,
        account: AccountId,
        display_name: Arc<str>,
        username: Arc<str>,
        avatar_url: Option<Arc<str>>,
        rooms: Vec<Room>,
        servers: Vec<Server>,
    ) -> Vec<Effect> {
        self.state.rooms.retain(|r| r.id.account != account);
        let rooms = rooms
            .into_iter()
            .map(|mut room| {
                self.apply_local_read(&mut room);
                Arc::new(room)
            })
            .collect::<Vec<_>>();
        self.state.rooms.extend(rooms);
        self.set_servers(&account, servers);
        // Loads from before a reconnect may never answer (their results
        // are dropped): don't leave those rooms waiting.
        for (room, timeline) in &mut self.state.timelines {
            if room.account == account {
                timeline.loading = false;
            }
        }
        // In place on a reconnect: the order stays, and so does what's
        // known about the device; it's connected again.
        let fresh = Account {
            id: account.clone(),
            display_name,
            username,
            avatar_url,
            unverified: false,
            offline: false,
            presence: None,
            status: None,
        };
        match self.state.accounts.iter_mut().find(|a| a.id == account) {
            Some(a) => {
                *a = Account {
                    unverified: a.unverified,
                    presence: a.presence,
                    status: a.status.clone(),
                    ..fresh
                }
            }
            None => self.state.accounts.push(fresh),
        }
        self.refresh_own_lines(&account);
        // After a reconnect the open room may have missed messages.
        match self.state.selected.clone() {
            Some(room)
                if room.account == account
                    && self.state.room(&room).is_some()
                    && !self.state.showing_friends
                    && !self.state.showing_people
                    && !self.state.showing_active
                    && !self.state.showing_notifications
                    && !self.state.showing_threads =>
            {
                let timeline = self.state.timelines.entry(room.clone()).or_default();
                timeline.loading = true;
                timeline.failed = false;
                vec![
                    Effect::Backend {
                        account: account.clone(),
                        command: Command::Focus {
                            room: Some(room.clone()),
                        },
                    },
                    Effect::Backend {
                        account,
                        command: Command::LoadHistory { room, before: None },
                    },
                ]
            }
            _ => Vec::new(),
        }
    }

    pub(super) fn apply_local_read(&mut self, room: &mut Room) {
        let Some(read) = self.local_reads.get(&room.id).cloned() else {
            return;
        };
        let newest = self
            .state
            .timelines
            .get(&room.id)
            .and_then(|t| t.messages.iter().rev().find(|m| !m.pending))
            .map(|m| m.id.clone());
        if let Some(newest) = newest {
            match read.as_ref() {
                Some(read) if &newest != read => {
                    self.local_reads.remove(&room.id);
                    return;
                }
                None => {
                    self.local_reads.insert(room.id.clone(), Some(newest.clone()));
                    room.last_read = Some(newest);
                }
                _ => {}
            }
        }
        room.unread = false;
        room.mentions = 0;
        if let Some(read) = read {
            room.last_read = Some(read.clone());
        }
    }

    pub(super) fn receive_account_lost(&mut self, account: AccountId, reason: Arc<str>) -> Vec<Effect> {
        if self
            .state
            .friend_request_pending
            .as_ref()
            .is_some_and(|(ours, _)| *ours == account)
        {
            self.state.friend_request_pending = None;
            self.state.friend_request_status = Some(Err("The selected account signed out.".into()));
        }
        if self
            .state
            .server_settings
            .as_ref()
            .is_some_and(|s| s.server.account == account)
        {
            self.state.server_settings = None;
        }
        if self
            .state
            .channel_settings
            .as_ref()
            .is_some_and(|s| s.room.account == account)
        {
            self.state.channel_settings = None;
        }
        self.state.own_profiles.remove(&account);
        // Its friends and requests go with it (contacts stay, with any
        // merges, for when it signs in again; People hides them).
        self.state.friends.retain(|p| p.id.account != account);
        self.state.blocked.retain(|u| u.account != account);
        self.state.favourites.retain(|u| u.account != account);
        self.state.people_directory.remove(&account);
        self.state.presence.retain(|user, _| user.account != account);
        self.state.friend_requests.retain(|(p, _)| p.id.account != account);
        self.state.invites.retain(|(a, _), _| *a != account);
        self.state.rooms.retain(|r| r.id.account != account);
        self.state.servers.retain(|s| s.id.account != account);
        if self
            .state
            .invite_link
            .as_ref()
            .is_some_and(|(s, _)| s.account == account)
        {
            self.state.invite_link = None;
        }
        self.state.timelines.retain(|id, _| id.account != account);
        self.state.profiles.retain(|user, _| user.account != account);
        self.state.members.retain(|room, _| room.account != account);
        self.state.directory.retain(|server, _| server.account != account);
        self.state.gifs.remove(&account);
        self.state.typing.retain(|room, _| room.account != account);
        self.state.emoji.retain(|group| group.server.account != account);
        if self
            .state
            .profile
            .as_ref()
            .is_some_and(|(user, _)| user.account == account)
        {
            self.state.profile = None;
        }
        if self
            .state
            .verification
            .as_ref()
            .is_some_and(|(ours, _)| *ours == account)
        {
            self.state.verification = None;
        }
        self.state.accounts.retain(|a| a.id != account);
        self.state.voice.retain(|room, _| room.account != account);
        if self.state.call.as_ref().is_some_and(|c| c.room.account == account) {
            self.state.call = None;
        }
        if self
            .state
            .selected_server
            .as_ref()
            .is_some_and(|s| s.account == account)
        {
            self.state.selected_server = None;
        }
        if self.state.selected.as_ref().is_some_and(|s| s.account == account) {
            self.state.selected = None;
            self.state.nudge = None;
        }
        // An empty reason: the user logged out themselves, so it's no error.
        if !reason.is_empty() {
            self.state.errors.push(reason);
        }
        Vec::new()
    }

    pub(super) fn receive_room_removed(&mut self, id: RoomId) -> Vec<Effect> {
        let effects = if self.state.call.as_ref().is_some_and(|call| call.room == id) {
            self.leave_call()
        } else {
            Vec::new()
        };
        self.state.rooms.retain(|r| r.id != id);
        if self.state.channel_settings.as_ref().is_some_and(|s| s.room == id) {
            self.state.channel_settings = None;
        }
        self.state.timelines.remove(&id);
        self.state.drafts.remove(&id);
        self.state.compose.remove(&id);
        self.state.typing.remove(&id);
        self.state.files.remove(&id);
        self.state.voice.remove(&id);
        if self.state.selected.as_ref() == Some(&id) {
            self.state.selected = None;
            self.state.nudge = None;
        }
        effects
    }

    pub(super) fn receive_history(
        &mut self,
        room: RoomId,
        before: Option<MessageId>,
        messages: Vec<Message>,
        reached_start: bool,
    ) -> Vec<Effect> {
        // A background load may finish after logout or room removal.
        if self.state.room(&room).is_none() {
            return Vec::new();
        }
        let newest = before.is_none();
        let t = self.state.timelines.entry(room.clone()).or_default();
        let mut list: Vec<Arc<Message>> = messages.into_iter().map(Arc::new).collect();
        if before.is_some() {
            // Older page: goes in front, skipping anything already loaded.
            list.retain(|m| !t.messages.iter().any(|old| old.id == m.id));
            list.extend(t.messages.iter().cloned());
        } else {
            // Newest page replaces the timeline; keep sends still pending.
            list.extend(t.messages.iter().filter(|m| m.pending).cloned());
            t.detached = false;
        }
        t.messages = Arc::new(list);
        t.reached_start = reached_start;
        t.loading = false;
        t.failed = false;
        t.cached = false;
        // The open chat's newest messages arrived: read up to them,
        // as the official clients do on opening a chat. Without
        // this, a chat opened before its history loaded stayed unread.
        let last = t.messages.iter().rev().find(|m| !m.pending).map(|m| m.id.clone());
        let open = self.state.selected.as_ref() == Some(&room)
            && !self.state.showing_friends
            && !self.state.showing_people
            && !self.state.showing_active
            && !self.state.showing_notifications
            && !self.state.showing_threads
            && !self.state.marked_unread.contains(&room);
        let unread = self.state.room(&room).is_some_and(|r| r.unread || r.mentions > 0);
        match last {
            Some(last) if newest && open && unread && !self.state.marked_unread.contains(&room) => {
                self.mark_read_local(&room, Some(last.clone()));
                vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::MarkRead { room, last },
                }]
            }
            _ => Vec::new(),
        }
    }

    pub(super) fn receive_message_added(&mut self, room: RoomId, message: Message) -> Vec<Effect> {
        self.touch_room(&room, &message);
        let mut effects = Vec::new();
        let message_id = message.id.clone();
        let is_open = self.state.selected.as_ref() == Some(&room)
            && !self.state.showing_friends
            && !self.state.showing_people
            && !self.state.showing_active
            && !self.state.showing_notifications
            && !self.state.showing_threads;
        if is_open
            && !message.pending
            && !self.state.showing_friends
            && !self.state.showing_people
            && !self.state.showing_active
            && !self.state.showing_notifications
            && !self.state.showing_threads
        {
            self.mark_read_local(&room, Some(message_id.clone()));
            effects.push(Effect::Backend {
                account: room.account.clone(),
                command: Command::MarkRead {
                    room: room.clone(),
                    last: message.id.clone(),
                },
            });
        }
        // Only rooms whose history is loaded keep live messages; the
        // rest load fresh when opened.
        if let Some(t) = self.state.timelines.get_mut(&room)
            && !t.detached
        {
            let list = Arc::make_mut(&mut t.messages);
            // Our own message's live copy often beats the send's
            // answer: it takes the pending row's place, so the
            // timeline doesn't show it twice and then jump when
            // the answer removes the pending one.
            let ours = *message.author.raw == *room.account.user;
            let pending = (ours && !list.iter().any(|m| m.id == message.id))
                .then(|| {
                    list.iter()
                        .position(|m| m.pending && m.source.trim() == message.source.trim())
                })
                .flatten();
            match pending {
                Some(i) => list[i] = Arc::new(message),
                None => upsert(list, message),
            }
        }
        effects
    }

    pub(super) fn mark_read_local(&mut self, room: &RoomId, last: Option<MessageId>) {
        self.local_reads.insert(room.clone(), last.clone());
        if let Some(slot) = self.state.rooms.iter_mut().find(|r| r.id == *room) {
            let mut updated = (**slot).clone();
            updated.unread = false;
            updated.mentions = 0;
            if let Some(last) = last {
                updated.last_read = Some(last);
            }
            *slot = Arc::new(updated);
        }
    }

    pub(super) fn receive_message_confirmed(&mut self, room: RoomId, nonce: Arc<str>, message: Message) -> Vec<Effect> {
        if self.state.room(&room).is_none() {
            return Vec::new();
        }
        self.touch_room(&room, &message);
        if let Some(t) = self.state.timelines.get_mut(&room) {
            let list = Arc::make_mut(&mut t.messages);
            // The live copy may have arrived first.
            if list.iter().any(|m| m.id == message.id) {
                list.retain(|m| !(m.pending && *m.id.0 == *nonce));
            } else {
                match list.iter().position(|m| m.pending && *m.id.0 == *nonce) {
                    Some(i) => list[i] = Arc::new(message),
                    None => list.push(Arc::new(message)),
                }
            }
        }
        Vec::new()
    }

    pub(super) fn receive_message_failed(&mut self, room: RoomId, nonce: Arc<str>, error: Arc<str>) -> Vec<Effect> {
        let mut unsent = None;
        if let Some(t) = self.state.timelines.get_mut(&room) {
            let messages = Arc::make_mut(&mut t.messages);
            if let Some(i) = messages.iter().position(|m| m.pending && *m.id.0 == *nonce) {
                unsent = Some(messages.remove(i).source.clone());
            }
        }
        // Give the text back to the composer unless something new was typed
        // meanwhile, so a failed send never loses what was written.
        if let Some(text) = unsent.filter(|t| !t.trim().is_empty())
            && self.state.drafts.get(&room).is_none_or(|d| d.trim().is_empty())
        {
            self.state.drafts.insert(room.clone(), text.to_string());
        }
        self.state.errors.push(error);
        Vec::new()
    }

    /// People's presence arrived: their direct chats' dots, People and the
    /// member lists we make (see `derive_members`) follow it.
    pub(super) fn receive_presences(
        &mut self,
        account: &AccountId,
        presence: Vec<(UserId, crate::members::Presence)>,
    ) -> Vec<Effect> {
        let mut changed = false;
        for (user, p) in presence {
            if user.account == *account && self.state.presence.insert(user, p) != Some(p) {
                changed = true;
            }
        }
        if !changed {
            return Vec::new();
        }
        for room in &mut self.state.rooms {
            if let crate::room::RoomKind::Direct { with } = &room.kind
                && let Some(p) = self.state.presence.get(with)
                && room.presence != Some(*p)
            {
                Arc::make_mut(room).presence = Some(*p);
            }
        }
        let rooms: Vec<RoomId> = self
            .state
            .members
            .keys()
            .filter(|r| r.account == *account)
            .cloned()
            .collect();
        for room in rooms {
            self.derive_members(&room);
        }
        self.derive_people(account)
    }

    /// Already loaded messages by `people` take their names and pictures
    /// (names often arrive after the messages: Steam's personas, Slack's
    /// and Google Chat's user lookups).
    pub(super) fn rename_authors(&mut self, account: &AccountId, people: &[Person]) {
        let identities: HashMap<_, _> = people.iter().map(|p| (&p.id, p)).collect();
        for (room, timeline) in &mut self.state.timelines {
            if room.account != *account {
                continue;
            }
            let changed = |m: &Message| {
                identities.get(&m.author).is_some_and(|p| {
                    m.author_name != p.name || (p.avatar_url.is_some() && m.author_avatar != p.avatar_url)
                })
            };
            if !timeline.messages.iter().any(|m| changed(m)) {
                continue;
            }
            for message in Arc::make_mut(&mut timeline.messages) {
                if changed(message)
                    && let Some(person) = identities.get(&message.author)
                {
                    let message = Arc::make_mut(message);
                    message.author_name = person.name.clone();
                    if person.avatar_url.is_some() {
                        message.author_avatar = person.avatar_url.clone();
                    }
                }
            }
        }
    }

    /// On networks without a friends list of their own, the people we
    /// have direct chats with are the people list (as the workplace
    /// chats' own clients list them under Direct messages).
    pub(super) fn derive_people(&mut self, account: &AccountId) -> Vec<Effect> {
        if account.network.has_friends() {
            return Vec::new();
        }
        let mut people: Vec<Person> = self
            .state
            .rooms
            .iter()
            .filter(|r| r.id.account == *account)
            .filter_map(|r| match &r.kind {
                crate::room::RoomKind::Direct { with } => Some(Person {
                    id: with.clone(),
                    name: r.name.clone(),
                    avatar_url: r.icon_url.clone(),
                    online: self.state.presence.get(with).or(r.presence.as_ref())
                        == Some(&crate::members::Presence::Online),
                }),
                _ => None,
            })
            .collect();
        // And the directory, when the network has one (Slack's members).
        if let Some(directory) = self.state.people_directory.get(account) {
            let have: HashSet<UserId> = people.iter().map(|p| p.id.clone()).collect();
            people.extend(directory.iter().filter(|p| !have.contains(&p.id)).cloned());
        }
        for p in &mut people {
            if let Some(presence) = self.state.presence.get(&p.id) {
                p.online = *presence == crate::members::Presence::Online;
            }
        }
        people.sort_by_key(|p| p.name.to_lowercase());
        people.dedup_by(|a, b| a.id == b.id);
        let unchanged = {
            let mut ours = self.state.friends.iter().filter(|p| p.id.account == *account);
            people.len() == self.state.friends.iter().filter(|p| p.id.account == *account).count()
                && people.iter().all(|p| ours.next() == Some(p))
        };
        if unchanged {
            return Vec::new();
        }
        self.receive_friends(account.clone(), people)
    }

    pub(super) fn receive_friends(&mut self, account: AccountId, people: Vec<Person>) -> Vec<Effect> {
        // Steam history can arrive before persona details. Unlike
        // server nicknames, Steam's persona applies across its DMs.
        if account.network.names_network_wide() {
            self.rename_authors(&account, &people);
        }
        // Every friend is a contact (on their own until merged).
        let before = self.state.contacts.iter().count();
        let mut renamed = false;
        for p in &people {
            let id = self.state.contacts.ensure(p.id.clone(), &p.name);
            if let Some(contact) = self.state.contacts.get_mut(id)
                && contact.identities.len() == 1
                && contact.name != p.name.as_ref()
            {
                // Keep merged names stable and preserve the user's
                // nickname; refresh the underlying network name.
                contact.name = p.name.to_string();
                renamed = true;
            }
        }
        let added = self.state.contacts.iter().count() != before;
        self.state.friends.retain(|p| p.id.account != account);
        self.state.friends.extend(people);
        if added || renamed {
            vec![Effect::SaveContacts]
        } else {
            Vec::new()
        }
    }
}
