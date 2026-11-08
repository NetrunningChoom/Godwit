// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Fills the window from the core's state: the main view's models and
//! properties. Shared by the app and the off-screen preview
//! (`examples/preview.rs`), so the preview shows exactly what the app does.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use godwit_core::application::{AppState, Compose, Verify};
use godwit_core::contacts::ContactId;
use godwit_core::ids::{AccountId, MessageId, RoomId, UserId};
use godwit_core::members::{MemberGroup, Presence};
use godwit_core::room::RoomKind;
use slint::Model as _;
use slint::{ModelRc, SharedString, VecModel};

use crate::images::Images;
use crate::inline_player::InlinePlayers;
use crate::ui::{
    AccountItem, ActiveItem, AppWindow, CallMemberItem, ContactItem, DirectoryItem, FileItem, GroupPersonItem,
    MemberItem, MessageItem, PersonItem, RequestItem, RoomItem, ServerItem,
};
use crate::view;

/// A frame taken out of `godwit_voice::video::FRAMES` to be shown.
struct Shown {
    serial: u64,
    size: [usize; 2],
    rgba: std::sync::Arc<Vec<u8>>,
}

#[derive(Default)]
pub struct Presenter {
    call_video_images: std::collections::HashMap<String, (u64, slint::Image)>,
    /// The call's pictures by tile (the `CallFrames` global), and each
    /// tile's video sid, for swapping frames without a full refresh.
    call_frames: Rc<VecModel<slint::Image>>,
    call_frame_sids: Vec<String>,
    /// The profile card's text as laid out (for selecting it).
    pub card_laid: Vec<crate::textlayout::Laid>,
    /// The video filling the window (its sid), followed as others come and
    /// go; when it ends the window comes back (it had no other way out).
    pub fullscreen_sid: Option<String>,
    /// When our call connected, for the call timer (the UI counts on from
    /// the seconds it's given, so nothing here ticks).
    call_since: Option<(godwit_core::ids::RoomId, std::time::Instant)>,
    pub servers: Rc<VecModel<ServerItem>>,
    pub rooms: Rc<VecModel<RoomItem>>,
    pub messages: Rc<VecModel<MessageItem>>,
    pub friends: Rc<VecModel<PersonItem>>,
    /// Active now's rows, and what each opens.
    pub active: Rc<VecModel<ActiveItem>>,
    pub active_rows: Vec<view::ActiveTarget>,
    pub threads: Rc<VecModel<crate::ui::ThreadItem>>,
    pub thread_rows: Vec<RoomId>,
    pub requests: Rc<VecModel<RequestItem>>,
    pub people: Rc<VecModel<ContactItem>>,
    /// People who might be one (suggestions on People).
    pub merges: Rc<VecModel<crate::ui::MergeItem>>,
    /// A verification request from another device was being shown.
    asked: bool,
    /// The link request People last acted on (see `AppState::link_request`).
    link_seen: u32,
    /// The contact each People row is, with its accounts in chip order.
    pub people_rows: Vec<(ContactId, Vec<UserId>)>,
    /// People's search text and network filter ("" for all): rows that
    /// don't match are left out (the one being linked always stays).
    pub people_query: String,
    pub people_network: String,
    pub people_favourites_filter: bool,
    pub call_members: Rc<VecModel<CallMemberItem>>,
    /// The call's videos and streams, kept and updated in place: a new
    /// model each refresh (many a second while video plays) rebuilt their
    /// buttons under the pointer, so clicks were lost and they flickered.
    pub call_videos: Rc<VecModel<crate::ui::CallVideoItem>>,
    pub call_streams: Rc<VecModel<crate::ui::StreamItem>>,
    /// Streams' sound per person, from Settings (the shell keeps it current).
    pub stream_sound: godwit_voice::call::VoiceSettings,
    pub members: Rc<VecModel<MemberItem>>,
    pub files: Rc<VecModel<FileItem>>,
    pub accounts: Rc<VecModel<AccountItem>>,
    /// The person each friends row is.
    pub friend_rows: Vec<UserId>,
    pub group_people: Rc<VecModel<GroupPersonItem>>,
    pub group_person_rows: Vec<UserId>,
    pub group_person_selected: HashSet<UserId>,
    pub request_rows: Vec<UserId>,
    pub server_rows: Vec<view::RailRow>,
    /// The account whose profile was opened from the rail: the "you" views
    /// (profile popup, presence, status) act on it. None: the open room's.
    pub focus: Option<AccountId>,
    /// The network whose status choices the profile popup shows.
    choices_for: Option<godwit_core::network::Network>,
    /// Who each network in the open chat's switcher is.
    pub talk_on_rows: Vec<UserId>,
    /// Accounts whose rail section is collapsed.
    pub collapsed: std::collections::HashSet<AccountId>,
    /// The rail's servers as the user ordered them (settings' keys).
    pub server_order: Vec<String>,
    pub account_order: Vec<String>,
    /// Kept sign-ins still connecting, by network (set by the shell).
    pub pending: Vec<godwit_core::network::Network>,
    /// Accounts to sign in to again (a copy of the settings').
    pub sign_in: Vec<crate::settings::SignInNeeded>,
    /// Muzzle mode's default and per-chat choices (a copy of the settings').
    pub muzzle: crate::settings::Settings,
    /// Collapsed channel categories (settings' keys), and each sidebar
    /// row's category key (headings only).
    pub collapsed_categories: Vec<String>,
    pub category_rows: Vec<Option<String>>,
    /// Home's search ("" for none).
    pub home_query: String,
    /// Home's network filter (a network's name; "" for all).
    pub home_network: String,
    /// A line per account for Settings (Steam: its sign-in's device name).
    pub account_details: std::collections::HashMap<AccountId, String>,
    /// Each account's alert choices for Settings.
    pub account_alerts: std::collections::HashMap<AccountId, AccountAlerts>,
    pub room_rows: Vec<Option<RoomId>>,
    pub directory: Rc<VecModel<DirectoryItem>>,
    /// The room each directory row is.
    pub directory_rows: Vec<RoomId>,
    /// The person each member row is (None for headings).
    pub member_rows: Vec<Option<UserId>>,
    /// The member list the model holds (rebuilt only when it changes).
    members_shown: Option<Arc<Vec<MemberGroup>>>,
    /// The timeline rows in view (first, last), as the list last said.
    pub window: Option<(usize, usize)>,
    /// Whose messages the timeline model holds.
    pub timeline_of: Option<RoomId>,
    /// What each timeline row was built from.
    pub rows: Vec<view::RowKey>,
    /// Messages whose spoilers were clicked open.
    pub revealed: HashSet<MessageId>,
    /// Blocked messages clicked open (not persisted; cleared with the room).
    pub revealed_runs: HashSet<MessageId>,
    /// The runs of blocked messages folded in the open room's rows, by first message.
    pub blocked_runs: std::collections::HashMap<MessageId, Vec<MessageId>>,

    /// Pictures arrived since the window was last filled.
    pub images_changed: bool,
    /// Only moving pictures changed (an animation's next frame, a player's
    /// frame): just the timeline rows showing them are rebuilt, not the
    /// other lists.
    pub frames_changed: bool,
    /// The width message text is laid out in, as the timeline reports it.
    pub text_width: f32,
}

/// Puts the call's pictures in place: rows replaced one by one, so a tile
/// whose picture didn't change isn't redrawn.
fn set_frames(model: &VecModel<slint::Image>, pictures: Vec<slint::Image>) {
    use slint::Model;
    if model.row_count() != pictures.len() {
        model.set_vec(pictures);
        return;
    }
    for (i, picture) in pictures.into_iter().enumerate() {
        model.set_row_data(i, picture);
    }
}

impl Presenter {
    pub fn toggle_group_person(&mut self, index: i32) -> Option<usize> {
        use slint::Model;
        let index = usize::try_from(index).ok()?;
        let user = self.group_person_rows.get(index)?.clone();
        if self.group_person_selected.remove(&user) {
            if let Some(mut row) = self.group_people.row_data(index) {
                row.selected = false;
                self.group_people.set_row_data(index, row);
            }
        } else {
            self.group_person_selected.insert(user);
            if let Some(mut row) = self.group_people.row_data(index) {
                row.selected = true;
                self.group_people.set_row_data(index, row);
            }
        }
        Some(self.group_person_selected.len())
    }

    pub fn selected_group_users(&self, account: &AccountId) -> Vec<UserId> {
        self.group_person_selected
            .iter()
            .filter(|u| &u.account == account)
            .cloned()
            .collect()
    }

    /// The video sid of the call tile at `index`, as last shown.
    /// Keeps the fullscreen view on its video after the list changed, or
    /// leaves fullscreen when that video ended (a stream stopped, the call
    /// ended): the overlay, and Esc with it, only exist while it does.
    fn follow_fullscreen(&mut self, ui: &AppWindow) {
        use slint::ComponentHandle;
        let Some(sid) = self.fullscreen_sid.as_deref() else {
            return;
        };
        match self.call_frame_sids.iter().position(|s| s == sid) {
            Some(index) => ui.set_fullscreen_video(i32::try_from(index).unwrap_or(-1)),
            None => {
                self.fullscreen_sid = None;
                ui.set_fullscreen_video(-1);
                ui.window().set_fullscreen(false);
                godwit_voice::video::focus_preview(None);
            }
        }
    }

    pub fn video_sid(&self, index: i32) -> Option<&str> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.call_frame_sids.get(i))
            .map(String::as_str)
    }

    /// Only the call's video changed (a frame arrived): swaps the new
    /// pictures into `CallFrames`, leaving the rest of the window alone.
    /// True when a tile got its first picture, which needs a full refresh
    /// (its "connecting" note goes).
    pub fn update_call_frames(&mut self) -> bool {
        use slint::Model;
        // Only the new frames' handles under the lock: the decoders hand
        // theirs over meanwhile, and the copying happens after.
        let new: Vec<(usize, Shown)> = {
            let frames = godwit_voice::video::FRAMES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.call_frame_sids
                .iter()
                .enumerate()
                .filter_map(|(i, sid)| {
                    let frame = frames.as_ref()?.get(sid)?;
                    let old = self.call_video_images.get(sid).map(|(serial, _)| *serial);
                    (old != Some(frame.serial)).then(|| {
                        (
                            i,
                            Shown {
                                serial: frame.serial,
                                size: frame.size,
                                rgba: frame.rgba.clone(),
                            },
                        )
                    })
                })
                .collect()
        };
        let mut first = false;
        for (i, frame) in new {
            let Some(sid) = self.call_frame_sids.get(i) else {
                continue;
            };
            first |= !self.call_video_images.contains_key(sid);
            let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                &frame.rgba,
                frame.size[0] as u32,
                frame.size[1] as u32,
            );
            let image = slint::Image::from_rgba8(buffer);
            self.call_video_images
                .insert(sid.clone(), (frame.serial, image.clone()));
            if i < self.call_frames.row_count() {
                self.call_frames.set_row_data(i, image);
            }
        }
        first
    }

    /// Hands the models to the window.
    pub fn new(ui: &AppWindow) -> Self {
        let p = Self::default();
        ui.set_servers(ModelRc::from(p.servers.clone()));
        ui.set_rooms(ModelRc::from(p.rooms.clone()));
        ui.set_directory(ModelRc::from(p.directory.clone()));
        ui.set_messages(ModelRc::from(p.messages.clone()));
        ui.set_friends(ModelRc::from(p.friends.clone()));
        ui.set_active_items(ModelRc::from(p.active.clone()));
        ui.set_thread_items(ModelRc::from(p.threads.clone()));
        ui.set_friend_requests(ModelRc::from(p.requests.clone()));
        ui.set_people(ModelRc::from(p.people.clone()));
        ui.set_merges(ModelRc::from(p.merges.clone()));
        ui.set_call_members(ModelRc::from(p.call_members.clone()));
        ui.set_call_videos(ModelRc::from(p.call_videos.clone()));
        ui.set_call_extra_videos(p.call_videos.iter().any(|v| !v.primary || v.participant < 0));
        ui.set_call_quality(-1);
        ui.set_call_latency(-1);
        {
            use slint::ComponentHandle;
            ui.global::<crate::ui::CallFrames>()
                .set_frames(ModelRc::from(p.call_frames.clone()));
        }
        ui.set_call_streams(ModelRc::from(p.call_streams.clone()));
        ui.set_members(ModelRc::from(p.members.clone()));
        ui.set_files(ModelRc::from(p.files.clone()));
        ui.set_accounts(ModelRc::from(p.accounts.clone()));
        p
    }

    /// Fills the window from `state`; the main view only when `main`.
    pub fn show(&mut self, ui: &AppWindow, state: &AppState, main: bool, images: &mut Images) {
        self.show_with_players(ui, state, main, images, None);
    }

    pub fn show_with_players(
        &mut self,
        ui: &AppWindow,
        state: &AppState,
        main: bool,
        images: &mut Images,
        players: Option<&InlinePlayers>,
    ) {
        images.next_frame();
        if let Some(a) = focus_account(state, self.focus.as_ref()) {
            let (avatar, has_avatar) = view::avatar(images, a.avatar_url.as_deref());
            ui.set_account_avatar(avatar);
            ui.set_account_has_avatar(has_avatar);
            ui.set_account_initials(view::initials(&a.display_name).into());
            ui.set_account_username(a.username.to_string().into());
            ui.set_account_network(a.id.network.name().into());
            // Our own dot, where the network has presence.
            ui.set_account_has_presence(a.presence.is_some());
            ui.set_account_presence(a.presence.map(view::presence_colour).unwrap_or_default());
            ui.set_account_status(a.status.as_deref().unwrap_or_default().into());
            ui.set_account_presence_index(match a.presence {
                Some(Presence::Idle) => 1,
                Some(Presence::Focus) => 2,
                Some(Presence::Busy) => 3,
                Some(Presence::Invisible) => 4,
                _ => 0,
            });
            // The network's own choices: Steam has online, away and invisible,
            // and no custom status text.
            let network = a.id.network;
            if self.choices_for != Some(network) {
                self.choices_for = Some(network);
                ui.set_account_presence_choices(ModelRc::new(VecModel::from(view::presence_choices(network))));
                ui.set_account_has_status_text(network.has_status_text());
            }
        }
        let accounts: Vec<AccountItem> = state
            .accounts
            .iter()
            .map(|a| {
                let (avatar, has_avatar) = view::avatar(images, a.avatar_url.as_deref());
                AccountItem {
                    key: a.id.to_string().into(),
                    can_befriend: a.id.network.has_friends(),
                    can_create_group: a.id.network.can_create_group(),
                    network: a.id.network.name().into(),
                    name: a.display_name.to_string().into(),
                    username: a.username.to_string().into(),
                    detail: self.account_details.get(&a.id).cloned().unwrap_or_default().into(),
                    initials: view::initials(&a.display_name).into(),
                    avatar,
                    has_avatar,
                    unverified: a.unverified,
                    alert_options: self
                        .account_alerts
                        .get(&a.id)
                        .map(|x| x.options.clone())
                        .unwrap_or_default(),
                    alert_index: self.account_alerts.get(&a.id).map_or(0, |x| x.index),
                    can_offline: self.account_alerts.get(&a.id).is_some_and(|x| x.can_offline),
                    offline: self.account_alerts.get(&a.id).is_some_and(|x| x.offline),
                    ..view::verification(
                        state.verification.as_ref().filter(|(v, _)| *v == a.id).map(|(_, s)| s),
                        images,
                    )
                }
            })
            .collect();
        view::sync(&self.accounts, accounts);
        // Another device asking: show it where it's answered (once per ask).
        let asking = matches!(state.verification, Some((_, Verify::Incoming)));
        if asking && !self.asked {
            ui.set_settings_open(true);
            let index = state
                .verification
                .as_ref()
                .and_then(|(id, _)| state.accounts.iter().position(|a| &a.id == id))
                .unwrap_or(0);
            // Its account's page, Account tab: that's where it's answered.
            ui.set_settings_section(10);
            ui.set_settings_account(i32::try_from(index).unwrap_or(0));
            ui.set_settings_account_tab(1);
        }
        self.asked = asking;
        ui.set_account_name(
            focus_account(state, self.focus.as_ref())
                .map(|a| a.display_name.to_string())
                .unwrap_or_default()
                .into(),
        );
        if !main {
            return;
        }

        let (servers, server_rows) = view::servers(
            state,
            images,
            &self.collapsed,
            &self.server_order,
            &self.account_order,
            &self.pending,
            &self.sign_in,
        );
        view::sync(&self.servers, servers);
        self.server_rows = server_rows;
        ui.set_home_selected(
            state.selected_server.is_none()
                && !state.showing_friends
                && !state.showing_people
                && !state.showing_active
                && !state.showing_notifications
                && !state.showing_games
                && !state.showing_threads,
        );

        let (directory, directory_rows) = view::directory(state);
        view::sync(&self.directory, directory);
        self.directory_rows = directory_rows;
        let (title, rooms, room_rows, category_rows) = view::rooms(
            state,
            images,
            &self.home_query,
            &self.home_network,
            &self.collapsed_categories,
        );
        // Home's network filter: a chip per network with chats, once there are several.
        let mut home_networks: Vec<slint::SharedString> = Vec::new();
        for r in state.direct_rooms() {
            let n: slint::SharedString = r.network().name().into();
            if !home_networks.contains(&n) {
                home_networks.push(n);
            }
        }
        home_networks.sort();
        if !home_networks.iter().any(|n| n.as_str() == self.home_network) {
            self.home_network.clear();
        }
        ui.set_home_network(self.home_network.as_str().into());
        ui.set_home_networks(ModelRc::new(VecModel::from(home_networks)));
        self.category_rows = category_rows;
        ui.set_sidebar_title(title.into());
        // The selected server's banner, decoded at twice the panel's size.
        let banner = state
            .selected_server
            .as_ref()
            .and_then(|id| state.server(id))
            .and_then(|s| s.banner_url.as_deref())
            .and_then(|url| images.get(url, crate::images::SIDEBAR_BANNER));
        ui.set_sidebar_has_banner(banner.is_some());
        ui.set_sidebar_banner(banner.unwrap_or_default());
        let mut rooms = rooms;
        for (row, id) in rooms.iter_mut().zip(&room_rows) {
            row.muzzled = id.as_ref().is_some_and(|id| self.muzzle.muzzled(id));
            row.alerts = match id.as_ref().map(|id| self.muzzle.room_alerts(id)) {
                Some(crate::settings::RoomAlerts::All) => 1,
                Some(crate::settings::RoomAlerts::Off) => 2,
                _ => 0,
            };
        }
        view::sync(&self.rooms, rooms);
        self.room_rows = room_rows;

        ui.set_in_call(state.call.is_some());
        // Networks whose calls can carry our video: the share button shows
        // there, greyed out where this call doesn't allow it.
        ui.set_call_video_network(state.call.as_ref().is_some_and(|call| call.room.network().call_video()));
        ui.set_call_can_video(state.call.as_ref().is_some_and(|call| {
            state.room(&call.room).is_some_and(|r| r.capabilities.video)
                && call.status == godwit_core::call::CallStatus::Connected
        }));
        // The call timer: from when it connected; -1 while connecting or out of a call.
        let connected = state
            .call
            .as_ref()
            .filter(|c| c.status == godwit_core::call::CallStatus::Connected);
        let mut fresh = false;
        match connected {
            Some(call) => {
                if self.call_since.as_ref().is_none_or(|(room, _)| *room != call.room) {
                    self.call_since = Some((call.room.clone(), std::time::Instant::now()));
                    fresh = true;
                }
            }
            None => self.call_since = None,
        }
        // The UI's own one-second timer counts the call time between
        // refreshes, a fraction of a second ahead of whole seconds since
        // joining: setting those back on every refresh made the clock jump
        // back a second. Set it when the call starts; after that only
        // forward, catching up if the timer fell behind.
        let elapsed = self
            .call_since
            .as_ref()
            .map_or(-1, |(_, at)| i32::try_from(at.elapsed().as_secs()).unwrap_or(i32::MAX));
        ui.set_call_elapsed(if fresh || elapsed < 0 {
            elapsed
        } else {
            elapsed.max(ui.get_call_elapsed())
        });
        if let Some(call) = &state.call {
            let latency = call.latency_ms.map(i32::from).unwrap_or(-1);
            ui.set_call_latency(latency);
            ui.set_call_quality(if latency < 0 {
                -1
            } else if latency <= 250 {
                0
            } else if latency <= 500 {
                1
            } else {
                2
            });
            ui.set_call_camera(call.camera);
            ui.set_call_screen_share(call.screen_share);
            let mut displayed = call.videos.clone();
            for (active, sid, screen_share) in [
                (call.camera, godwit_voice::video::LOCAL_CAMERA, false),
                (call.screen_share, godwit_voice::video::LOCAL_SCREEN, true),
            ] {
                if active {
                    displayed.push(godwit_core::call::CallVideo {
                        sid: sid.into(),
                        user: UserId::new(call.room.account.clone(), call.room.account.user.clone()),
                        screen_share,
                    });
                }
            }
            // The shown videos' latest frames, taken out under the lock and
            // copied after it (see `update_call_frames`).
            let frames: Option<std::collections::HashMap<String, Shown>> = {
                let all = godwit_voice::video::FRAMES
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                all.as_ref().map(|all| {
                    displayed
                        .iter()
                        .filter_map(|v| {
                            let f = all.get(&v.sid)?;
                            Some((
                                v.sid.clone(),
                                Shown {
                                    serial: f.serial,
                                    size: f.size,
                                    rgba: f.rgba.clone(),
                                },
                            ))
                        })
                        .collect()
                })
            };
            self.call_video_images
                .retain(|sid, _| displayed.iter().any(|v| &v.sid == sid));
            let mut seen_video_users = std::collections::HashSet::new();
            let videos: Vec<crate::ui::CallVideoItem> = displayed
                .iter()
                .map(|video| {
                    if let Some(frame) = frames.as_ref().and_then(|frames| frames.get(&video.sid))
                        && self
                            .call_video_images
                            .get(&video.sid)
                            .is_none_or(|(serial, _)| *serial != frame.serial)
                    {
                        let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                            &frame.rgba,
                            frame.size[0] as u32,
                            frame.size[1] as u32,
                        );
                        self.call_video_images
                            .insert(video.sid.clone(), (frame.serial, slint::Image::from_rgba8(buffer)));
                    }
                    let name = call
                        .members
                        .iter()
                        .find(|m| m.user == video.user)
                        .map_or("Participant", |m| m.name.as_ref());
                    let participant = call
                        .members
                        .iter()
                        .position(|m| m.user == video.user)
                        .unwrap_or(usize::MAX);
                    let primary = !video.screen_share && seen_video_users.insert(video.user.clone());
                    let frame = self.call_video_images.get(&video.sid).map(|(_, image)| image.clone());
                    crate::ui::CallVideoItem {
                        label: format!("{name} · {}", if video.screen_share { "Screen" } else { "Camera" }).into(),
                        screen: video.screen_share,
                        participant: i32::try_from(participant).unwrap_or(-1),
                        primary,
                        stream: call
                            .streams
                            .iter()
                            .position(|s| video.screen_share && s.watching && s.user == video.user)
                            .and_then(|i| i32::try_from(i).ok())
                            .unwrap_or(-1),
                        has_frame: frame.is_some(),
                        // Shown through `CallFrames`, so new frames don't change the rows.
                        frame: slint::Image::default(),
                    }
                })
                .collect();
            self.call_frame_sids = displayed.iter().map(|v| v.sid.clone()).collect();
            self.follow_fullscreen(ui);
            let pictures: Vec<slint::Image> = self
                .call_frame_sids
                .iter()
                .map(|sid| {
                    self.call_video_images
                        .get(sid)
                        .map(|(_, i)| i.clone())
                        .unwrap_or_default()
                })
                .collect();
            set_frames(&self.call_frames, pictures);
            ui.set_call_screen_videos(i32::try_from(videos.iter().filter(|v| v.screen).count()).unwrap_or(0));
            ui.set_call_extra_videos(videos.iter().any(|v| !v.primary || v.participant < 0));
            ui.set_call_status(view::call_status(call.status).into());
            ui.set_call_room(
                state
                    .room(&call.room)
                    .map(|r| r.name.to_string())
                    .unwrap_or_default()
                    .into(),
            );
            ui.set_call_muted(state.self_muted);
            ui.set_call_speaking(
                !call.muted
                    && call
                        .members
                        .iter()
                        .any(|m| m.speaking && *m.user.raw == *call.room.account.user),
            );
            ui.set_call_deafened(state.self_deafened);
            let mut members = view::call_members(&call.members, images);
            for (item, member) in members.iter_mut().zip(&call.members) {
                let user = member.user.raw.as_ref();
                item.me = *member.user.raw == *call.room.account.user;
                item.volume = self.stream_sound.user_volume(user);
                item.local_muted = self.stream_sound.muted_users.iter().any(|u| u == user);
                item.video_index = videos
                    .iter()
                    .position(|v| {
                        v.primary
                            && v.participant >= 0
                            && usize::try_from(v.participant).ok()
                                == call.members.iter().position(|m| m.user == member.user)
                    })
                    .and_then(|i| i32::try_from(i).ok())
                    .unwrap_or(-1);
            }
            view::sync(&self.call_members, members);
            view::sync(&self.call_videos, videos);
            let streams: Vec<crate::ui::StreamItem> = call
                .streams
                .iter()
                .map(|s| {
                    let name_of = |user: &godwit_core::ids::UserId| {
                        call.members
                            .iter()
                            .find(|m| &m.user == user)
                            .map_or_else(|| "Someone".to_owned(), |m| m.name.to_string())
                    };
                    let viewer_names: Vec<String> = s.viewers.iter().flatten().map(name_of).collect();
                    crate::ui::StreamItem {
                        name: call
                            .members
                            .iter()
                            .find(|m| m.user == s.user)
                            .map_or_else(|| "Someone".to_owned(), |m| m.name.to_string())
                            .into(),
                        watching: s.watching,
                        viewers: s
                            .viewers
                            .as_ref()
                            .map_or_else(String::new, |v| v.len().to_string())
                            .into(),
                        viewer_names: viewer_names.join("\n").into(),
                        note: s.note.as_deref().unwrap_or_default().into(),
                        volume: self.stream_sound.screen_share_volume(&s.user.raw),
                        // Discord's streams are heard until muted (godwit-voice's `push_heard`).
                        muted: self
                            .stream_sound
                            .screen_share_mutes
                            .get(s.user.raw.as_ref())
                            .copied()
                            .unwrap_or(false),
                    }
                })
                .collect();
            view::sync(&self.call_streams, streams);
        } else {
            ui.set_call_quality(-1);
            ui.set_call_latency(-1);
            view::sync(&self.call_streams, Vec::new());
            self.call_video_images.clear();
            self.call_frame_sids.clear();
            set_frames(&self.call_frames, Vec::new());
            view::sync(&self.call_videos, Vec::new());
            ui.set_call_extra_videos(false);
            self.follow_fullscreen(ui);
        }
        ui.set_card_open(state.profile.is_some());
        if let Some((_, Some(p))) = &state.profile {
            view::profile(ui, p, images);
            // Its text as message text is laid out (emoji pictures, links,
            // selecting); a few short blocks, so each refresh.
            let (blocks, laid) = view::card_text(p, images);
            ui.set_card_blocks(ModelRc::new(VecModel::from(blocks)));
            self.card_laid = laid;
        } else {
            self.card_laid.clear();
        }
        if let Some((user, _)) = &state.profile {
            ui.set_card_linked(view::linked(state, user).into());
        }
        // Channels' call times count from now (the window ticks on).
        // Only forward: the window's own tick runs a little ahead of whole
        // seconds, and setting it back made the times jump back a second.
        let now = chrono::Utc::now().timestamp();
        ui.set_clock(i32::try_from(now).unwrap_or(0).max(ui.get_clock()));
        ui.set_voice_clock(
            state
                .voice_since
                .iter()
                .any(|(room, _)| state.voice.get(room).is_some_and(|m| !m.is_empty())),
        );
        // The account card's mute and deafen show outside calls too.
        ui.set_self_muted(state.self_muted);
        ui.set_self_deafened(state.self_deafened);
        ui.set_showing_friends(state.showing_friends);
        ui.set_showing_active(state.showing_active);
        ui.set_showing_notifications(state.showing_notifications);
        ui.set_showing_games(state.showing_games);
        ui.set_can_show_games(state.accounts.iter().any(|a| a.id.network.has_game_libraries()));
        ui.set_showing_threads(state.showing_threads);
        ui.set_can_show_threads(
            state
                .selected_server
                .as_ref()
                .is_some_and(|s| s.account.network.thread_style() == godwit_core::network::ThreadStyle::Panel),
        );
        if state.showing_threads {
            let mut rooms: Vec<_> = state
                .selected_server
                .as_ref()
                .and_then(|id| state.server(id))
                .into_iter()
                .flat_map(|server| server.categories.iter().filter(|c| &*c.title == "Threads"))
                .flat_map(|category| category.rooms.iter())
                .filter_map(|id| state.room(id))
                .collect();
            rooms.sort_by(|a, b| {
                b.last_activity
                    .cmp(&a.last_activity)
                    .then_with(|| a.id.raw.cmp(&b.id.raw))
            });
            let rows = rooms
                .iter()
                .map(|room| crate::ui::ThreadItem {
                    name: room.name.as_ref().into(),
                    context: room
                        .parent
                        .as_ref()
                        .and_then(|raw| state.room(&RoomId::new(room.id.account.clone(), raw.as_ref())))
                        .map_or_else(|| SharedString::from("Thread"), |parent| parent.name.as_ref().into()),
                    preview: room.status.as_deref().unwrap_or_default().into(),
                    when: view::last_talked(room.last_activity).into(),
                    unread: room.unread,
                })
                .collect();
            self.thread_rows = rooms.iter().map(|r| r.id.clone()).collect();
            view::sync(&self.threads, rows);
        }
        if state.showing_active {
            let (rows, targets) = view::active(state, images);
            view::sync(&self.active, rows);
            self.active_rows = targets;
        }
        if state.showing_friends || state.showing_people {
            let (rows, people) = view::friends(state, images);
            view::sync(&self.friends, rows);
            self.friend_rows = people;
            self.group_person_selected
                .retain(|u| state.friends.iter().any(|p| &p.id == u));
            let group_people: Vec<_> = state
                .friends
                .iter()
                .filter(|p| p.id.account.network.can_create_group())
                .map(|p| GroupPersonItem {
                    account_key: p.id.account.to_string().into(),
                    name: p.name.to_string().into(),
                    initials: view::initials(&p.name).into(),
                    selected: self.group_person_selected.contains(&p.id),
                })
                .collect();
            self.group_person_rows = state
                .friends
                .iter()
                .filter(|p| p.id.account.network.can_create_group())
                .map(|p| p.id.clone())
                .collect();
            view::sync(&self.group_people, group_people);
            ui.set_group_people(ModelRc::from(self.group_people.clone()));
            ui.set_group_selected_count(i32::try_from(self.group_person_selected.len()).unwrap_or(i32::MAX));
            let (rows, who) = view::requests(state, images);
            view::sync(&self.requests, rows);
            self.request_rows = who;
            let (status, failed) = match &state.friend_request_status {
                Some(Ok(())) => ("Friend request sent.".to_owned(), false),
                Some(Err(e)) => (e.to_string(), true),
                None => (String::new(), false),
            };
            ui.set_friend_request_status(status.into());
            ui.set_friend_request_failed(failed);
        }
        ui.set_showing_people(state.showing_people);
        if state.showing_people || state.showing_friends {
            ui.set_people_favourites_only(self.people_favourites_filter);
            let (rows, who, merges) = view::people(state, images, &self.muzzle.person_favourites);
            // The networks people are on, for the filter's chips.
            let mut networks: Vec<slint::SharedString> = Vec::new();
            // Labels may name an alt ("Discord · …"); the filter is by network.
            let network_of = |label: &str| label.split_whitespace().next().unwrap_or_default().to_owned();
            for id in rows.iter().flat_map(|r| r.identities.iter()) {
                let n: slint::SharedString = network_of(&id.network).into();
                if !networks.contains(&n) {
                    networks.push(n);
                }
            }
            networks.sort();
            ui.set_people_networks(ModelRc::new(VecModel::from(networks)));
            // Whoever is being linked, by contact, so filtering keeps them picked.
            let linking = usize::try_from(ui.get_people_linking())
                .ok()
                .and_then(|i| self.people_rows.get(i))
                .map(|(c, _)| *c);
            let query = self.people_query.trim().to_lowercase();
            let (rows, who): (Vec<ContactItem>, Vec<(ContactId, Vec<UserId>)>) = rows
                .into_iter()
                .zip(who)
                .filter(|(row, (contact, users))| {
                    // Any name they go by on any of their accounts matches.
                    let named = || {
                        row.name.to_lowercase().contains(&query)
                            || view::names_of(state, users)
                                .iter()
                                .any(|n| n.to_lowercase().contains(&query))
                    };
                    Some(*contact) == linking
                        || ((query.is_empty() || named())
                            && (!self.people_favourites_filter || row.favourite)
                            && (self.people_network.is_empty()
                                || row
                                    .identities
                                    .iter()
                                    .any(|i| network_of(&i.network) == self.people_network)))
                })
                .unzip();
            let starred: Vec<bool> = rows.iter().map(|r| r.favourite).collect();
            let mut rows = rows;
            for (row, section) in rows
                .iter_mut()
                .zip(view::sections(&starred, self.people_favourites_filter))
            {
                row.section = section.into();
            }
            view::sync(&self.people, rows);
            view::sync(&self.merges, merges);
            self.people_rows = who;
            if let Some(contact) = linking {
                let row = self.people_rows.iter().position(|(c, _)| *c == contact);
                ui.set_people_linking(row.and_then(|r| i32::try_from(r).ok()).unwrap_or(-1));
            }
            if let Some((contact, n)) = state.link_request
                && n != self.link_seen
            {
                self.link_seen = n;
                let row = self.people_rows.iter().position(|(c, _)| *c == contact);
                ui.set_people_linking(row.and_then(|r| i32::try_from(r).ok()).unwrap_or(-1));
            }
        }

        let room = state.selected.as_ref().and_then(|id| state.room(id));
        ui.set_has_room(room.is_some());
        let Some(room) = room else {
            self.timeline_of = None;
            return;
        };
        let (encrypted, security) = view::security(room);
        // A person: our nickname or their name on the network chosen; with
        // several networks, the switcher between them.
        let with = match &room.kind {
            RoomKind::Direct { with } => Some(with),
            _ => None,
        };
        let title = with
            .and_then(|w| view::person_name(state, w))
            .unwrap_or_else(|| room.name.to_string());
        ui.set_room_title(title.into());
        let contact = with.and_then(|w| state.contacts.of(w));
        ui.set_room_can_rename(contact.is_some());
        ui.set_room_nickname(contact.and_then(|c| c.nickname.clone()).unwrap_or_default().into());
        let talk_on: Vec<UserId> = with
            .and_then(|w| view::merged(state, w))
            .map(|c| {
                c.identities
                    .iter()
                    .filter(|u| state.accounts.iter().any(|a| a.id == u.account))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let items: Vec<crate::ui::TalkOnItem> = talk_on
            .iter()
            .map(|u| crate::ui::TalkOnItem {
                label: view::dm_account_label(state, u).into(),
                current: Some(u) == with,
                colour: view::identity_presence(state, u),
            })
            .collect();
        let labels: Vec<SharedString> = items.iter().map(|t| t.label.clone()).collect();
        let current = items
            .iter()
            .position(|t| t.current)
            .and_then(|i| i32::try_from(i).ok())
            .unwrap_or(-1);
        ui.set_room_talk_on_labels(ModelRc::new(VecModel::from(labels)));
        ui.set_room_talk_on_current(current);
        ui.set_room_talk_on(ModelRc::new(VecModel::from(items)));
        self.talk_on_rows = talk_on;
        // Which account it's on, once there's more than one to mix up.
        let account = state
            .accounts
            .iter()
            .find(|a| a.id == room.id.account)
            .filter(|_| state.accounts.len() > 1)
            .map(|a| format!("{} · {}", a.display_name, a.id.network.name()))
            .unwrap_or_default();
        ui.set_room_account(account.into());
        ui.set_invite_from(room.invited_by.as_deref().unwrap_or_default().into());
        ui.set_room_encrypted(encrypted);
        ui.set_room_is_channel(matches!(room.kind, RoomKind::Channel { .. }));
        ui.set_room_security(security.into());
        ui.set_can_send(room.capabilities.send);
        let draft = state.drafts.get(&room.id).cloned().unwrap_or_default();
        if ui.get_draft() != draft.as_str() {
            ui.set_draft(draft.into());
        }
        ui.set_nudge(
            state
                .nudge
                .and_then(|n| {
                    let name = &state.contacts.get(n.contact)?.name;
                    Some(format!(
                        "{name} is also on {}, which keeps your chats more private.",
                        n.to.name()
                    ))
                })
                .unwrap_or_default()
                .into(),
        );
        ui.set_can_react(room.capabilities.react);
        ui.set_can_reply(room.capabilities.reply);
        ui.set_can_edit(room.capabilities.edit);
        ui.set_can_delete(room.capabilities.delete);
        ui.set_can_delete_others(room.capabilities.delete_others);
        ui.set_can_moderate_reactions(room.capabilities.moderate_reactions);
        ui.set_has_pins(room.capabilities.pins);
        // Every conversation has search: networks without a search API use
        // the messages Godwit has loaded locally.
        ui.set_has_search(true);
        ui.set_can_thread(room.capabilities.threads);
        ui.set_is_forum(room.capabilities.forum);
        ui.set_can_pin(room.capabilities.pin_messages);
        ui.set_can_report(room.capabilities.report);
        ui.set_can_call(room.capabilities.voice);
        ui.set_call_unavailable(if room.capabilities.voice_unavailable {
            "Calls don't work in Godwit yet".into()
        } else {
            Default::default()
        });
        let has_members = matches!(room.kind, RoomKind::Channel { .. } | RoomKind::Group);
        ui.set_has_members(has_members);
        ui.set_members_shown(state.show_members);
        let groups = state.members.get(&room.id).cloned();
        let same = match (&groups, &self.members_shown) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same || self.images_changed {
            let (rows, who) = groups.as_deref().map(|g| view::members(g, images)).unwrap_or_default();
            view::sync(&self.members, rows);
            self.member_rows = who;
            self.members_shown = groups;
        }
        ui.set_call_here(state.call.as_ref().is_some_and(|c| c.room == room.id));
        ui.set_can_attach(
            room.capabilities.attachments && !matches!(state.compose.get(&room.id), Some(Compose::Edit(_))),
        );
        let files: Vec<FileItem> = state
            .files
            .get(&room.id)
            .map(|list| {
                list.iter()
                    .map(|f| FileItem {
                        name: f.name.as_ref().into(),
                        detail: view::human_size(f.size).into(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        view::sync(&self.files, files);
        ui.set_typing(view::typing(state.typing.get(&room.id).map_or(&[][..], Vec::as_slice)).into());
        let compose = match state.compose.get(&room.id) {
            Some(Compose::Reply(id)) => {
                let who = state
                    .selected_message(id)
                    .map_or_else(String::new, |m| m.author_name.to_string());
                format!("Replying to {who}")
            }
            Some(Compose::Edit(_)) => "Editing message".to_owned(),
            None => String::new(),
        };
        ui.set_compose_label(compose.into());
        let timeline = state.timeline(&room.id);
        ui.set_loading_history(timeline.is_some_and(|t| t.loading));
        ui.set_history_failed(timeline.is_some_and(|t| t.failed));
        let offline = state.accounts.iter().any(|a| a.id == room.id.account && a.offline);
        ui.set_room_offline(
            if offline {
                format!("Reconnecting to {}…", room.network().name())
            } else {
                String::new()
            }
            .into(),
        );
        ui.set_detached(timeline.is_some_and(|t| t.detached));
        ui.set_reached_start(timeline.is_some_and(|t| t.reached_start && !t.loading));
        if self.timeline_of.as_ref() != Some(&room.id) {
            // Another room: start over rather than diffing two rooms' messages.
            self.messages.set_vec(Vec::new());
            self.rows.clear();
            self.timeline_of = Some(room.id.clone());
            self.revealed_runs.clear();
            self.blocked_runs.clear();
            // Its rows haven't said where the view is yet.
            self.window = None;
        }
        let list = timeline.map(|t| t.messages.clone()).unwrap_or_default();
        let jump = state
            .jump
            .as_ref()
            .map(|(id, n)| (id.clone(), i32::try_from(*n).unwrap_or(i32::MAX)));
        let folded = view::collapse_blocked(
            &list,
            &state.blocked,
            &self.revealed_runs,
            state.unread_from.as_ref(),
            jump.as_ref().map(|(id, _)| id),
        );
        let marks = view::Marks {
            blocked_runs: &folded.runs,
            all: Some(&list),
            unread_from: state.unread_from.as_ref(),
            jump: jump.as_ref(),
            revealed: &self.revealed,
            players,
            invites: &state.invites,
            servers: &state.servers,
            text_width: if self.text_width > 0.0 { self.text_width } else { 600.0 },
        };
        self.rows = view::sync_timeline(
            &self.messages,
            &self.rows,
            &folded.list,
            &marks,
            &mut self.window,
            self.images_changed || self.frames_changed,
            images,
        );
        self.blocked_runs = folded.runs;
        self.images_changed = false;
        self.frames_changed = false;
    }
}

/// The account the "you" views show: the one opened from the rail, else the
/// open room's (or server's), else the first.
pub fn focus_account<'a>(
    state: &'a AppState,
    focus: Option<&AccountId>,
) -> Option<&'a godwit_core::application::Account> {
    let wanted = focus
        .or(state.selected.as_ref().map(|r| &r.account))
        .or(state.selected_server.as_ref().map(|s| &s.account));
    wanted
        .and_then(|id| state.accounts.iter().find(|a| &a.id == id))
        .or(state.accounts.first())
}

/// An account's alert settings as Settings shows them.
#[derive(Clone, Default)]
pub struct AccountAlerts {
    /// Shared per list of choices: rows compare models by identity.
    pub options: ModelRc<SharedString>,
    pub index: i32,
    pub can_offline: bool,
    pub offline: bool,
}

thread_local! {
    static ALERT_CHOICES: std::cell::RefCell<Vec<(Vec<crate::settings::AlertMode>, ModelRc<SharedString>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The (shared) model for a list of alert choices.
pub fn alert_options(choices: &[crate::settings::AlertMode]) -> ModelRc<SharedString> {
    ALERT_CHOICES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((_, model)) = cache.iter().find(|(c, _)| c == choices) {
            return model.clone();
        }
        let labels: Vec<SharedString> = choices.iter().map(|c| c.label().into()).collect();
        let model = ModelRc::from(Rc::new(VecModel::from(labels)));
        cache.push((choices.to_vec(), model.clone()));
        model
    })
}
