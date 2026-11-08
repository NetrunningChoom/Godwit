// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The application loop: the UI sends [`Action`]s, network backends send
//! [`Event`]s, and both come back as [`Effect`]s for the shell to carry out.
//! No I/O happens here, so every rule is unit-testable.

mod action;
mod command;
mod effect;
mod event;
mod on_action;
mod on_event;
mod state;

mod apply;
mod dispatch;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::cache::{AccountSnapshot, CachedTimeline};
use crate::call::{Call, CallMember, CallStatus};
use crate::contacts::{ContactId, Contacts};
use crate::ids::{AccountId, MessageId, RoomId, ServerId, UserId};
use crate::members::{MemberGroup, Presence, Profile};
use crate::message::{Attachment, AttachmentKind, EmojiGroup, Markup, Message, Reaction};
use crate::network::Network;
use crate::own_profile::{OwnProfile, OwnProfileState, ProfileEdit};
use crate::room::{Capabilities, Room, RoomKind};
use crate::server::{DirectoryRoom, Server};

pub use action::Action;
pub use command::Command;
pub use effect::Effect;
pub use event::Event;
pub use state::*;

impl Application {
    /// The name a network gave `user`: as a friend, else their direct
    /// chat's, else their loaded profile's.
    fn known_name(&self, user: &UserId) -> Option<String> {
        let s = &self.state;
        s.friends
            .iter()
            .find(|p| p.id == *user)
            .map(|p| p.name.to_string())
            .or_else(|| {
                s.rooms
                    .iter()
                    .find(|r| matches!(&r.kind, RoomKind::Direct { with } if with == user))
                    .map(|r| r.name.to_string())
            })
            .or_else(|| s.profiles.get(user).map(|p| p.name.to_string()))
    }
}

/// The pages shown in place of a room (see `show_page`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    People,
    Active,
    Notifications,
    /// Games in common (its data is `games`, dropped on leaving).
    Games,
    /// Threads sets its own flag (it keeps a server selected).
    Threads,
}

/// How many rooms keep their loaded messages.
const KEEP_TIMELINES: usize = 8;

/// Owns the state and applies the rules.
#[derive(Debug, Default)]
pub struct Application {
    state: AppState,
    /// Counter for pending messages' nonces.
    sent: u64,
    /// Servers whose directory was asked for this session.
    directory_asked: HashSet<ServerId>,
    /// Rooms opened, most recent first (see `forget_old_timelines`).
    recent: Vec<RoomId>,
    /// The room last open in each server (None: Home), reopened when the
    /// server is picked again, as officially.
    last_room: HashMap<Option<ServerId>, RoomId>,
    /// When each room's typing notice last came, so a notice whose stop
    /// never arrives (any network) still runs out: see `expire_typing`.
    typing_at: HashMap<RoomId, std::time::Instant>,
    /// Locally acknowledged read positions. Network snapshots can lag or omit
    /// read receipts; keep the UI consistent until a newer message arrives.
    local_reads: HashMap<RoomId, Option<MessageId>>,
}

/// How long a typing notice lasts without being repeated. Networks repeat
/// theirs every 3–8 s while someone keeps typing.
pub const TYPING_FOR: std::time::Duration = std::time::Duration::from_secs(10);

impl Application {
    pub fn new(contacts: Contacts) -> Self {
        Self {
            state: AppState {
                contacts,
                show_members: true,
                // Home opens on Active now, including before the first account sync.
                showing_active: true,
                ..AppState::default()
            },
            sent: 0,
            directory_asked: HashSet::new(),
            recent: Vec::new(),
            last_room: HashMap::new(),
            typing_at: HashMap::new(),
            local_reads: HashMap::new(),
        }
    }

    /// Clears typing notices older than `TYPING_FOR`; true if any changed.
    pub fn expire_typing(&mut self, now: std::time::Instant) -> bool {
        let stale: Vec<RoomId> = self
            .typing_at
            .iter()
            .filter(|(_, at)| now.saturating_duration_since(**at) >= TYPING_FOR)
            .map(|(r, _)| r.clone())
            .collect();
        for room in &stale {
            self.typing_at.remove(room);
            self.state.typing.remove(room);
        }
        !stale.is_empty()
    }

    /// When the oldest typing notice runs out, if any is shown: the UI wakes
    /// then to call [`Application::expire_typing`], since a notice whose stop
    /// never comes changes nothing else that would wake it.
    pub fn typing_expires_at(&self) -> Option<std::time::Instant> {
        self.typing_at.values().min().map(|at| *at + TYPING_FOR)
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// A bounded snapshot for one account. Only recent timelines and their
    /// newest messages are retained; transient connection and UI state is not.
    pub fn cache_snapshot(&self, account: &AccountId) -> AccountSnapshot {
        const MAX_ROOMS: usize = 512;
        const MAX_SERVERS: usize = 128;
        const MAX_TIMELINES: usize = 16;
        const MAX_MESSAGES: usize = 100;
        const MAX_PROFILES: usize = 64;

        let timelines = self
            .recent
            .iter()
            .filter(|room| &room.account == account)
            .filter_map(|room| self.state.timelines.get(room).map(|timeline| (room, timeline)))
            .take(MAX_TIMELINES)
            .map(|(room, timeline)| CachedTimeline {
                room: room.clone(),
                messages: timeline
                    .messages
                    .iter()
                    .rev()
                    .take(MAX_MESSAGES)
                    .map(|message| (**message).clone())
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect(),
                reached_start: timeline.reached_start,
            })
            .collect();

        AccountSnapshot {
            account: account.clone(),
            rooms: self
                .state
                .rooms
                .iter()
                .filter(|room| &room.id.account == account)
                .take(MAX_ROOMS)
                .map(|r| (**r).clone())
                .collect(),
            servers: self
                .state
                .servers
                .iter()
                .filter(|server| &server.id.account == account)
                .take(MAX_SERVERS)
                .map(|s| (**s).clone())
                .collect(),
            timelines,
            profiles: self
                .state
                .profiles
                .values()
                .filter(|profile| &profile.user.account == account)
                .take(MAX_PROFILES)
                .cloned()
                .collect(),
        }
    }

    /// Asset URLs in snapshots can outlive a server's media configuration.
    /// Refresh matching reaction pictures when the network's emoji arrive,
    /// including when cached messages arrive after the emoji list.
    fn refresh_reaction_images(&mut self, account: &AccountId) {
        let images: HashMap<_, _> = self
            .state
            .emoji
            .iter()
            .filter(|group| &group.server.account == account)
            .flat_map(|group| &group.emoji)
            .map(|emoji| (emoji.reaction.clone(), emoji.image_url.clone()))
            .collect();
        let stale = |message: &Message| {
            message.reactions.iter().any(|reaction| {
                images
                    .get(&reaction.emoji)
                    .is_some_and(|url| reaction.image_url.as_ref() != Some(url))
            })
        };
        for (room, timeline) in &mut self.state.timelines {
            if &room.account != account || !timeline.messages.iter().any(|m| stale(m)) {
                continue;
            }
            for message in Arc::make_mut(&mut timeline.messages) {
                if !stale(message) {
                    continue;
                }
                for reaction in &mut Arc::make_mut(message).reactions {
                    if let Some(url) = images.get(&reaction.emoji) {
                        reaction.image_url = Some(url.clone());
                    }
                }
            }
        }
    }

    /// Restores stale data for immediate display after the matching account
    /// reconnects. Opening a restored timeline always requests fresh history.
    ///
    /// The cache only fills gaps: it loads off-thread and may finish after
    /// the network has already sent its rooms (READY creates the account,
    /// which the restore waits for), and then the network's rooms, servers,
    /// timelines and profiles win. Overwriting them put stale rooms back,
    /// with capabilities from an older build (Discord's video, 2026-11-04).
    pub fn restore_cache(&mut self, snapshot: AccountSnapshot) {
        let account = snapshot.account;
        let cached_rooms: Vec<_> = snapshot
            .timelines
            .iter()
            .map(|timeline| timeline.room.clone())
            .collect();
        let live = self.state.rooms.iter().any(|room| room.id.account == account);
        if !live {
            self.state.rooms.extend(snapshot.rooms.into_iter().map(Arc::new));
            self.set_servers(&account, snapshot.servers);
        }
        for room in cached_rooms {
            if !self.recent.contains(&room) {
                self.recent.push(room);
            }
        }
        for cached in snapshot.timelines {
            if self.state.timelines.contains_key(&cached.room) {
                continue;
            }
            self.state.timelines.insert(
                cached.room,
                Timeline {
                    messages: Arc::new(cached.messages.into_iter().map(Arc::new).collect()),
                    reached_start: cached.reached_start,
                    cached: true,
                    ..Timeline::default()
                },
            );
        }
        for profile in snapshot.profiles {
            self.state.profiles.entry(profile.user.clone()).or_insert(profile);
        }
        self.refresh_reaction_images(&account);
        self.refresh_own_lines(&account);
    }

    /// Steam can send our own persona after history loaded (or a cached
    /// timeline is restored after it came): lines we wrote then carry the
    /// fallback name. Steam's persona is the same in every chat (unlike
    /// server nicknames elsewhere), so they take the account's current one.
    fn refresh_own_lines(&mut self, account: &AccountId) {
        if !account.network.names_network_wide() {
            return;
        }
        let Some(ours) = self.state.accounts.iter().find(|a| a.id == *account) else {
            return;
        };
        let (name, avatar) = (ours.display_name.clone(), ours.avatar_url.clone());
        let me = UserId::new(account.clone(), account.user.clone());
        let stale = |m: &Message| m.author == me && (m.author_name != name || m.author_avatar != avatar);
        for (room, timeline) in &mut self.state.timelines {
            if room.account != *account || !timeline.messages.iter().any(|m| stale(m)) {
                continue;
            }
            for message in Arc::make_mut(&mut timeline.messages) {
                if stale(message) {
                    let message = Arc::make_mut(message);
                    message.author_name = name.clone();
                    message.author_avatar = avatar.clone();
                }
            }
        }
    }

    fn apply_own_profile(&mut self, account: AccountId, result: Result<OwnProfile, Arc<str>>, saved: bool) {
        if !self.state.accounts.iter().any(|a| a.id == account) {
            return;
        }
        let state = self.state.own_profiles.entry(account.clone()).or_default();
        state.loading = false;
        state.saving = false;
        state.saved = saved && result.is_ok();
        match result {
            Ok(profile) => {
                state.error = None;
                if let Some(a) = self.state.accounts.iter_mut().find(|a| a.id == account) {
                    a.display_name = profile.name.clone();
                    a.avatar_url = profile.avatar_url.clone();
                }
                state.profile = Some(profile);
            }
            Err(error) => {
                state.error = Some(error.clone());
                self.state
                    .errors
                    .push(format!("{} profile: {error}", if saved { "Saving" } else { "Loading" }).into());
            }
        }
    }

    /// Whether an event still has an owner: results that arrive after their
    /// account signed out (or their room went away) must not bring its state
    /// back. Only [`Event::AccountReady`] creates an account.
    fn has_owner(&self, event: &Event) -> bool {
        let signed_in = |account: &AccountId| self.state.accounts.iter().any(|a| a.id == *account);
        match event {
            Event::Friends { account, .. }
            | Event::Blocked { account, .. }
            | Event::Authors { account, .. }
            | Event::PeopleDirectory { account, .. }
            | Event::Presences { account, .. }
            | Event::Activities { account, .. }
            | Event::FriendRequests { account, .. }
            | Event::GifCategories { account, .. }
            | Event::Gifs { account, .. }
            | Event::GifsFailed { account, .. }
            | Event::InviteLoaded { account, .. }
            | Event::CustomEmoji { account, .. } => signed_in(account),
            Event::Profile(p) => signed_in(&p.user.account),
            Event::GameLibrary { user, .. } => signed_in(&user.account),
            Event::Directory { server, .. } => signed_in(&server.account),
            Event::Members { room, .. }
            | Event::VoiceSince { room, .. }
            | Event::Pins { room, .. }
            | Event::SearchResults { room, .. }
            | Event::VoiceOccupants { room, .. }
            | Event::Typing { room, .. }
            | Event::HistoryAround { room, .. } => self.state.room(room).is_some(),
            _ => true,
        }
    }

    /// Something happened in `room`: it moves up Home, and a hidden chat
    /// comes back when someone else writes in it (as Discord reopens a
    /// closed DM).
    fn touch_room(&mut self, room: &RoomId, message: &Message) {
        if let Some(r) = self.state.rooms.iter_mut().find(|r| r.id == *room)
            && message.sent_ms > r.last_activity
        {
            Arc::make_mut(r).last_activity = message.sent_ms;
        }
        if message.author.raw != room.account.user {
            self.state.hidden_rooms.retain(|r| r != room);
        }
    }

    /// The open settings' page list, fetched when the page shows (Overview
    /// comes with the settings themselves).
    fn load_server_page(&self, page: crate::server_settings::ServerPage) -> Vec<Effect> {
        use crate::server_settings::{ServerAdmin, ServerPage};
        let Some(s) = &self.state.server_settings else {
            return Vec::new();
        };
        let op = match page {
            ServerPage::Overview => return Vec::new(),
            ServerPage::Roles => ServerAdmin::LoadRoles,
            ServerPage::Emoji => ServerAdmin::LoadEmoji,
            ServerPage::Invites => ServerAdmin::LoadInvites,
            ServerPage::Bans => ServerAdmin::LoadBans,
        };
        vec![Effect::Backend {
            account: s.server.account.clone(),
            command: Command::ServerAdmin {
                server: s.server.clone(),
                op,
            },
        }]
    }

    /// An admin answer: lists fill the open settings; outcomes become its
    /// notice, or (with the settings closed, as from a member's card) an error.
    fn apply_server_admin(
        &mut self,
        server: ServerId,
        result: Result<crate::server_settings::ServerAdminResult, Arc<str>>,
    ) {
        use crate::server_settings::ServerAdminResult;
        if self.state.server(&server).is_none() {
            return;
        }
        if let Ok(ServerAdminResult::InviteCreated(link)) = &result {
            if let Some((for_server, slot)) = &mut self.state.invite_link
                && *for_server == server
            {
                *slot = Some(link.clone());
            }
            return;
        }
        // A failed invite closes its dialog; the reason goes with the others.
        if result.is_err()
            && self
                .state
                .invite_link
                .as_ref()
                .is_some_and(|(s, l)| *s == server && l.is_none())
        {
            self.state.invite_link = None;
        }
        let Some(s) = self.state.server_settings.as_mut().filter(|s| s.server == server) else {
            if let Err(e) = result {
                self.state.errors.push(e);
            }
            return;
        };
        match result {
            Ok(ServerAdminResult::Invites(list)) => s.invites = Some(list),
            Ok(ServerAdminResult::Bans(list)) => s.bans = Some(list),
            Ok(ServerAdminResult::Roles(roles)) => s.roles = Some(roles),
            Ok(ServerAdminResult::Emoji(emoji)) => s.emoji = Some(emoji),
            Ok(ServerAdminResult::Done(message)) => {
                s.admin_busy = false;
                s.notice = Some(Ok(message));
            }
            Ok(ServerAdminResult::InviteCreated(_)) => {}
            Err(e) => {
                s.admin_busy = false;
                s.notice = Some(Err(e));
            }
        }
    }

    fn set_servers(&mut self, account: &AccountId, servers: Vec<Server>) {
        let mut others: Vec<Arc<Server>> = self
            .state
            .servers
            .drain(..)
            .filter(|s| &s.id.account != account)
            .collect();
        others.extend(servers.into_iter().map(Arc::new));
        self.state.servers = others;
        if self
            .state
            .server_settings
            .as_ref()
            .is_some_and(|s| self.state.server(&s.server).is_none())
        {
            self.state.server_settings = None;
        }
        if self
            .state
            .invite_link
            .as_ref()
            .is_some_and(|(s, _)| self.state.server(s).is_none())
        {
            self.state.invite_link = None;
        }
        if let Some(sel) = &self.state.selected_server
            && self.state.server(sel).is_none()
        {
            self.state.selected_server = None;
        }
    }

    /// Picking a server (or Home) opens a room in it, as the official client
    /// does: the one last open there, else its first text channel; Home
    /// without a previous DM shows Friends. Without this the DM stayed open
    /// beside the server's channel list until a channel was clicked.
    /// People, Active now or Notifications in place of the open room.
    fn show_page(&mut self, page: Page) -> Vec<Effect> {
        self.state.showing_threads = false;
        self.state.showing_people = page == Page::People;
        self.state.showing_active = page == Page::Active;
        self.state.showing_notifications = page == Page::Notifications;
        self.state.showing_games = page == Page::Games;
        if page != Page::Games {
            // A list can be thousands of games: it goes with the page.
            self.state.games = None;
        }
        self.state.showing_friends = false;
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

    fn open_in_server(&mut self, server: Option<&ServerId>) -> Vec<Effect> {
        let in_it = |state: &AppState, room: &RoomId| {
            state.room(room).is_some_and(|r| match &r.kind {
                RoomKind::Channel { server: s } => Some(s) == server,
                _ => server.is_none(),
            })
        };
        if self.state.selected.as_ref().is_some_and(|r| in_it(&self.state, r))
            && !self.state.showing_friends
            && !self.state.showing_people
            && !self.state.showing_active
            && !self.state.showing_notifications
            && !self.state.showing_games
            && !self.state.showing_threads
        {
            return Vec::new();
        }
        let remembered = self
            .last_room
            .get(&server.cloned())
            .filter(|r| in_it(&self.state, r))
            .cloned();
        let room = remembered.or_else(|| {
            let server = self.state.server(server?)?;
            let rooms: Vec<&RoomId> = server.rooms().filter(|r| self.state.room(r).is_some()).collect();
            rooms
                .iter()
                .find(|r| self.state.room(r).is_some_and(|r| !r.capabilities.voice))
                .or(rooms.first())
                .map(|r| (*r).clone())
        });
        match room {
            Some(room) => self.select(room),
            None if server.is_none() => self.dispatch(Action::ShowFriends),
            None => Vec::new(),
        }
    }

    fn select(&mut self, room: RoomId) -> Vec<Effect> {
        let Some(r) = self.state.room(&room).cloned() else {
            return Vec::new();
        };
        // Another room's pins and search don't stay open.
        if self.state.pins.as_ref().is_some_and(|p| p.room != room) {
            self.state.pins = None;
        }
        if self.state.search.as_ref().is_some_and(|s| s.room != room) {
            self.state.search = None;
        }
        self.forget_old_timelines(&room);
        let mut effects = Vec::new();
        // The previous room's account loses focus if it's another account.
        if let Some(prev) = self.state.selected.take()
            && prev.account != room.account
        {
            effects.push(Effect::Backend {
                account: prev.account,
                command: Command::Focus { room: None },
            });
        }
        // An unread room keeps the place it was read up to, for the divider,
        // until another room opens.
        self.state.unread_from = if r.unread { r.last_read.clone() } else { None };
        self.state.selected = Some(room.clone());
        self.state.showing_friends = false;
        self.state.showing_people = false;
        self.state.showing_active = false;
        self.state.showing_notifications = false;
        self.state.showing_games = false;
        self.state.games = None;
        self.state.showing_threads = false;
        // A channel shows its server; anything else lives in Home.
        self.state.selected_server = match &r.kind {
            RoomKind::Channel { server } => Some(server.clone()),
            _ => None,
        };
        self.last_room.insert(self.state.selected_server.clone(), room.clone());
        self.refresh_nudge();
        let account = room.account.clone();
        effects.push(Effect::Backend {
            account: account.clone(),
            command: Command::Focus {
                room: Some(room.clone()),
            },
        });
        // An invite has nothing to read until it's accepted.
        if r.invited_by.is_some() {
            return effects;
        }
        let read_last = if r.unread || r.mentions > 0 {
            self.state
                .timelines
                .get(&room)
                .and_then(|t| t.messages.iter().rev().find(|m| !m.pending))
                .map(|m| m.id.clone())
        } else {
            None
        };
        if (r.unread || r.mentions > 0) && read_last.is_some() {
            self.mark_read_local(&room, read_last.clone());
        }
        // Server channels always ask: their members name and colour the messages.
        if matches!(r.kind, RoomKind::Channel { .. }) || (self.state.show_members && self.has_members(&room)) {
            effects.push(Effect::Backend {
                account: account.clone(),
                command: Command::LoadMembers { room: room.clone() },
            });
        }
        // A room whose first load failed tries again when opened.
        if self
            .state
            .timelines
            .get(&room)
            .is_some_and(|t| t.failed && t.messages.is_empty() && !t.loading)
        {
            self.state.timelines.remove(&room);
        }
        match self.state.timelines.get_mut(&room) {
            Some(t) => {
                // Until Google Chat has a push stream, reopening a loaded
                // conversation must catch up too. Avoid duplicate in-flight loads.
                if t.cached || (room.account.network.refetch_on_open() && !t.loading) {
                    t.loading = true;
                    effects.push(Effect::Backend {
                        account: account.clone(),
                        command: Command::LoadHistory {
                            room: room.clone(),
                            before: None,
                        },
                    });
                }
                // A mention count without the unread flag is unread too
                // (it stuck on the badge otherwise).
                if r.unread || r.mentions > 0 {
                    if let Some(last) = read_last {
                        effects.push(Effect::Backend {
                            account,
                            command: Command::MarkRead {
                                room: room.clone(),
                                last,
                            },
                        });
                    } else {
                        effects.push(Effect::Backend {
                            account,
                            command: Command::MarkRoomRead { room: room.clone() },
                        });
                    }
                }
            }
            None => {
                self.state.timelines.insert(
                    room.clone(),
                    Timeline {
                        loading: true,
                        ..Timeline::default()
                    },
                );
                effects.push(Effect::Backend {
                    account,
                    command: Command::LoadHistory { room, before: None },
                });
            }
        }
        effects
    }

    /// Rooms with a member list: server channels and groups.
    /// A profile card from what's already known of `user` (people list,
    /// member lists, loaded messages) until the network sends theirs, or
    /// for good on networks that don't (Slack, Teams, Fluxer, Google Chat).
    /// Its actions follow the network: no friend requests where it has none.
    fn known_profile(&self, user: &UserId) -> Option<Profile> {
        let from_people = self
            .state
            .friends
            .iter()
            .find(|p| p.id == *user)
            .map(|p| (p.name.clone(), p.avatar_url.clone(), true));
        let from_members = || {
            self.state
                .members
                .values()
                .flat_map(|g| g.iter())
                .flat_map(|g| &g.members)
                .find(|m| m.user == *user)
                .map(|m| (m.name.clone(), m.avatar_url.clone(), false))
        };
        let from_messages = || {
            self.state
                .timelines
                .values()
                .flat_map(|t| t.messages.iter())
                .find(|m| m.author == *user)
                .map(|m| (m.author_name.clone(), m.author_avatar.clone(), false))
        };
        let (name, avatar_url, friend) = from_people.or_else(from_members).or_else(from_messages)?;
        let has_friends = user.account.network.has_friends();
        Some(Profile {
            user: user.clone(),
            name,
            username: "".into(),
            avatar_url,
            banner_url: None,
            presence: Presence::Unknown,
            status: None,
            bio: None,
            roles: Vec::new(),
            me: *user.raw == *user.account.user,
            relationship: if friend && has_friends {
                crate::members::Relationship::Friend
            } else {
                crate::members::Relationship::None
            },
            bot: false,
            has_friends,
            moderation: None,
            group_moderation: None,
            can_block: false,
            can_report: false,
        })
    }

    /// On networks that don't list a room's members, its member list is
    /// who has written in what's loaded of it, newest first.
    fn derive_members(&mut self, room: &RoomId) {
        if room.account.network.has_member_lists() || !self.has_members(room) {
            return;
        }
        let Some(timeline) = self.state.timelines.get(room) else {
            return;
        };
        let mut seen = HashSet::new();
        let members: Vec<crate::members::Member> = timeline
            .messages
            .iter()
            .rev()
            .filter(|m| !m.system && seen.insert(m.author.clone()))
            .map(|m| crate::members::Member {
                user: m.author.clone(),
                name: m.author_name.clone(),
                avatar_url: m.author_avatar.clone(),
                colour: None,
                presence: self.state.presence.get(&m.author).copied().unwrap_or(Presence::Unknown),
                status: None,
            })
            .collect();
        // Online first, as member lists are; then the rest, newest first.
        let (online, rest): (Vec<_>, Vec<_>) = members.into_iter().partition(|m| m.presence == Presence::Online);
        let mut groups = Vec::new();
        if !online.is_empty() {
            groups.push(crate::members::MemberGroup {
                title: format!("Online — {}", online.len()).into(),
                members: online,
            });
        }
        if !rest.is_empty() {
            groups.push(crate::members::MemberGroup {
                title: format!("Recently active — {}", rest.len()).into(),
                members: rest,
            });
        }
        if self.state.members.get(room).is_none_or(|g| **g != groups) {
            self.state.members.insert(room.clone(), Arc::new(groups));
        }
    }

    fn has_members(&self, room: &RoomId) -> bool {
        self.state
            .room(room)
            .is_some_and(|r| matches!(r.kind, RoomKind::Channel { .. } | RoomKind::Group))
    }

    /// Asks the UI to scroll to `message` and flash it.
    /// Keeps the messages of the rooms opened most recently (Yapfast keeps
    /// recent channels too): older ones load again when opened, so a long
    /// session doesn't hold every room it ever visited. Rooms with unsent
    /// messages are kept.
    fn forget_old_timelines(&mut self, opening: &RoomId) {
        self.recent.retain(|r| r != opening);
        self.recent.insert(0, opening.clone());
        if self.recent.len() > KEEP_TIMELINES {
            for old in self.recent.split_off(KEEP_TIMELINES) {
                if !self
                    .state
                    .timelines
                    .get(&old)
                    .is_some_and(|t| t.messages.iter().any(|m| m.pending))
                {
                    self.state.timelines.remove(&old);
                }
            }
        }
    }

    /// The room exists and allows what `can` picks out.
    fn allows(&self, room: &RoomId, can: fn(&Capabilities) -> bool) -> bool {
        self.state.room(room).is_some_and(|r| can(&r.capabilities))
    }

    fn mark_jump(&mut self, message: MessageId) {
        let n = self.state.jump.as_ref().map_or(0, |(_, n)| *n) + 1;
        self.state.jump = Some((message, n));
    }

    fn leave_call(&mut self) -> Vec<Effect> {
        match self.state.call.take() {
            Some(call) => vec![Effect::Backend {
                account: call.room.account,
                command: Command::LeaveCall,
            }],
            None => Vec::new(),
        }
    }

    fn load_earlier(&mut self) -> Vec<Effect> {
        let Some(room) = self.state.selected.clone() else {
            return Vec::new();
        };
        let Some(t) = self.state.timelines.get_mut(&room) else {
            return Vec::new();
        };
        if t.loading || t.reached_start {
            return Vec::new();
        }
        let before = t.messages.iter().find(|m| !m.pending).map(|m| m.id.clone());
        t.loading = true;
        vec![Effect::Backend {
            account: room.account.clone(),
            command: Command::LoadHistory { room, before },
        }]
    }

    fn send(&mut self, now_ms: i64) -> Vec<Effect> {
        let Some(room) = self.state.selected.clone() else {
            return Vec::new();
        };
        if !self.state.room(&room).is_some_and(|r| r.capabilities.send) {
            return Vec::new();
        }
        let editing = matches!(self.state.compose.get(&room), Some(Compose::Edit(_)));
        let has_files = !editing && self.state.files.get(&room).is_some_and(|f| !f.is_empty());
        let text = self.state.drafts.get(&room).cloned().unwrap_or_default();
        if text.trim().is_empty() && !has_files {
            return Vec::new();
        }
        self.state.drafts.remove(&room);
        let files = if editing {
            Vec::new()
        } else {
            self.state.files.remove(&room).unwrap_or_default()
        };
        let reply_to = match self.state.compose.remove(&room) {
            Some(Compose::Edit(message)) => {
                return vec![Effect::Backend {
                    account: room.account.clone(),
                    command: Command::EditMessage { room, message, text },
                }];
            }
            Some(Compose::Reply(id)) => Some(id),
            None => None,
        };
        self.sent += 1;
        // Unique across runs, not just within one: networks remember the
        // nonce as an idempotency key for a while (Stoat answered a restarted
        // app's `pending-1` with 409 Conflict). The official clients send a ULID.
        let nonce: Arc<str> = format!("pending-{now_ms}-{}", self.sent).into();
        let account = room.account.clone();
        let me = self.state.accounts.iter().find(|a| a.id == account);
        let pending = Message {
            id: MessageId(nonce.clone()),
            author: UserId::new(account.clone(), account.user.clone()),
            author_name: me.map_or_else(|| account.user.clone(), |a| a.display_name.clone()),
            author_avatar: None,
            author_colour: None,
            sent_ms: now_ms,
            edited: false,
            system: false,
            text: text.as_str().into(),
            markup: Markup::Plain,
            source: text.as_str().into(),
            reply_to: reply_to.clone(),
            jumbo: Vec::new(),
            // Shown by name until the server has them.
            attachments: files
                .iter()
                .map(|f| Attachment {
                    name: f.name.clone(),
                    url: "".into(),
                    kind: AttachmentKind::File,
                    size: f.size,
                    dimensions: None,
                    original_url: None,
                })
                .collect(),
            embeds: Vec::new(),
            invites: Vec::new(),
            reactions: Vec::new(),
            pending: true,
            snippet: None,
            pinned: false,
            forwardable: false,
        };
        if let Some(t) = self.state.timelines.get_mut(&room) {
            Arc::make_mut(&mut t.messages).push(Arc::new(pending));
        }
        vec![Effect::Backend {
            account,
            command: Command::SendMessage {
                room,
                text,
                nonce,
                reply_to,
                files,
            },
        }]
    }

    /// Shows the "switch network" bar when the selected room is a DM with
    /// someone who is also on a better network.
    fn refresh_nudge(&mut self) {
        self.state.nudge = None;
        let Some(room) = self.state.selected.as_ref().and_then(|id| self.state.room(id)) else {
            return;
        };
        let RoomKind::Direct { with } = &room.kind else {
            return;
        };
        let Some(contact) = self.state.contacts.of(with) else {
            return;
        };
        if let Some(to) = contact.nudge_from(room.network()) {
            self.state.nudge = Some(Nudge {
                contact: contact.id,
                to,
            });
        }
    }
}

/// Replaces the message with the same id, or appends it.
fn upsert(list: &mut Vec<Arc<Message>>, message: Message) {
    match list.iter().position(|m| m.id == message.id) {
        Some(i) => list[i] = Arc::new(message),
        None => list.push(Arc::new(message)),
    }
}

/// A size for people: "12.5 MB".
fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
mod tests;
