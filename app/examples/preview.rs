// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Renders Godwit off-screen into PNGs, with made-up data: no window opens,
//! nothing touches the network or any account. For checking layouts.
//!
//! cargo run --example preview -- <output dir>

use std::rc::Rc;

use godwit::demo::{account, call_member, filler, me, msg, person, pictures, profile, room, user, world};
use godwit::images::Images;
use godwit::present::Presenter;
use godwit::theme;
use godwit::ui::{AppWindow, Screen};
use godwit_core::application::{Action, Application, Event, Outgoing, Person, Verify};
use godwit_core::call::CallMember;
use godwit_core::contacts::Contacts;
use godwit_core::ids::{AccountId, MessageId, RoomId, ServerId, UserId};
use godwit_core::members::Profile;
use godwit_core::network::Network;
use godwit_core::room::RoomKind;
use godwit_core::server::{Category, DirectoryRoom, Server};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, Rgb8Pixel};

const W: u32 = 1280;
const H: u32 = 800;

struct Offscreen(Rc<MinimalSoftwareWindow>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

// Where "Link" sits on People's first row.
const LINK_X: f32 = 1109.0;
const LINK_Y: f32 = 106.0;

/// People: the friends, each a contact (Ann and Bo merged across networks).
fn people(app: &mut Application, _: &AppWindow, _: &RoomId, _: &RoomId) -> Screen {
    app.apply(Event::Friends {
        account: account(),
        // The fourth shares the first one's first name: People suggests a link.
        people: [
            person(0),
            person(1),
            person(2),
            format!("{} (alt)", first_name(&person(0))),
        ]
        .iter()
        .enumerate()
        .map(|(i, n)| Person {
            id: user(&format!("u{i}")),
            name: n.as_str().into(),
            avatar_url: None,
            online: true,
        })
        .collect(),
    });
    app.dispatch(Action::ShowPeople);
    Screen::Main
}

/// The member dialogs' fixed choice lists, which the app sets once at startup.
fn member_choices(ui: &AppWindow) {
    use godwit_core::server_settings::{BAN_DELETE_CHOICES, TIMEOUT_CHOICES};
    let labels = |l: Vec<&str>| {
        slint::ModelRc::new(slint::VecModel::from(
            l.into_iter().map(slint::SharedString::from).collect::<Vec<_>>(),
        ))
    };
    ui.set_ban_delete_choices(labels(BAN_DELETE_CHOICES.iter().map(|c| c.0).collect()));
    ui.set_timeout_choices(labels(TIMEOUT_CHOICES.iter().map(|c| c.0).collect()));
}

/// Ann's profile card, open.
fn open_profile(app: &mut Application, blocked: bool) -> Screen {
    app.dispatch(Action::OpenProfile(user("u0")));
    app.apply(Event::Profile(Profile {
        username: format!("{}#4242", first_name(&person(0)).to_lowercase()).into(),
        avatar_url: None,
        status: Some(filler(31, 2).to_lowercase().into()),
        // Rich text with a hard break, as bios have.
        bio: Some(
            format!(
                "{}, *{}* **{}**.\\\n{}.",
                filler(32, 1),
                filler(33, 1),
                filler(34, 2),
                filler(35, 1)
            )
            .into(),
        ),
        roles: vec![
            ("Moderators".into(), Some([0xff, 0x7b, 0x72])),
            ("Members".into(), None),
        ],
        // Seen in our server, so the card shows Kick and Ban.
        can_report: true,
        moderation: Some(godwit_core::members::Moderation {
            kick: true,
            ban: true,
            ban_history: true,
            timeout: true,
            nickname: true,
            nick: Some("Nickname".into()),
            roles: ["Moderators", "Helpers", "Artists", "Event hosts"]
                .iter()
                .enumerate()
                .map(|(i, name)| godwit_core::members::ModerationRole {
                    id: format!("R{i}").into(),
                    name: (*name).into(),
                    colour: (i % 2 == 0).then_some([0xff, 0x7b, 0x72]),
                    assigned: i < 2,
                })
                .collect(),
            ..godwit_core::members::Moderation::new(ServerId::new(account(), "S"))
        }),
        // Block user (or Unblock, for `blocked`).
        can_block: true,
        relationship: if blocked {
            godwit_core::members::Relationship::Blocked
        } else {
            Default::default()
        },
        ..profile(user("u0"), &person(0))
    }));
    Screen::Main
}

/// The share dialog as Windows shows it, on tab `tab`.
fn share_source(ui: &AppWindow, tab: i32) -> Screen {
    let strings = |items: Vec<String>| {
        slint::ModelRc::new(slint::VecModel::from(
            items.into_iter().map(slint::SharedString::from).collect::<Vec<_>>(),
        ))
    };
    ui.set_share_screens(strings(vec!["Screen 1".into(), "Screen 2".into()]));
    ui.set_share_windows(strings(
        (0..9)
            .map(|i| format!("{} — {}", filler(70 + i, 3), filler(80 + i, 1)))
            .collect(),
    ));
    ui.set_share_qualities(strings(vec![
        "720p 30FPS".into(),
        "1080p 30FPS".into(),
        "Source 5FPS".into(),
    ]));
    ui.set_share_quality_index(0);
    ui.set_share_source_tab(tab);
    ui.set_share_source_index(1);
    ui.set_share_pick_source(true);
    ui.set_share_dialog_open(true);
    Screen::Main
}

/// Puts the app in a scene's state; says which screen shows.
type Setup<'a> = Box<dyn Fn(&mut Application, &AppWindow, &RoomId, &RoomId) -> Screen + 'a>;

struct Scene<'a> {
    name: &'a str,
    setup: Setup<'a>,
    /// Where the pointer rests (for hover states).
    hover: Option<(f32, f32)>,
    /// The data arrives after the window first draws, as in the app (its
    /// rooms come from the network once the window is up).
    late: bool,
    /// A mouse-wheel scroll at a point (dy < 0 scrolls down).
    scroll: Option<(f32, f32, f32)>,
    /// A right-click at a point (opens context menus).
    right_click: Option<(f32, f32)>,
    /// A left-click at a point, after the hover (so hover-only buttons exist).
    click: Option<(f32, f32)>,
    /// The window size, if not the default.
    size: Option<(u32, u32)>,
    /// Escape pressed after the first draw.
    escape: bool,
    /// A rail folder of the demo account shown open (the preview has no
    /// shell to answer a click on its tile).
    open_folder: Option<&'a str>,
}

impl<'a> Scene<'a> {
    fn new(name: &'a str, setup: Setup<'a>) -> Self {
        Self {
            name,
            setup,
            hover: None,
            late: false,
            scroll: None,
            right_click: None,
            click: None,
            size: None,
            escape: false,
            open_folder: None,
        }
    }
}

fn friends_scene(app: &mut Application, _: &AppWindow, _: &RoomId, _: &RoomId) -> Screen {
    let person_row = |i: usize, pending: bool| Person {
        id: user(&format!("{}-{i}", if pending { "pending" } else { "friend" })),
        name: format!("{} {}", person(i % 3), i + 1).into(),
        avatar_url: None,
        online: i != 2,
    };
    app.apply(Event::Friends {
        account: account(),
        people: (0..3).map(|i| person_row(i, false)).collect(),
    });
    app.apply(Event::FriendRequests {
        account: account(),
        incoming: (0..8).map(|i| person_row(i, true)).collect(),
        outgoing: (8..12).map(|i| person_row(i, true)).collect(),
    });
    app.dispatch(Action::ShowFriends);
    Screen::Main
}

fn friends_create_group_scene(app: &mut Application, ui: &AppWindow, a: &RoomId, b: &RoomId) -> Screen {
    friends_scene(app, ui, a, b);
    ui.set_friend_account_key("stoat:me".into());
    ui.set_friend_group_capable(true);
    ui.set_group_name("Project crew".into());
    Screen::Main
}

/// A name's first word (People suggests links by first name).
fn first_name(name: &str) -> String {
    name.split(' ').next().unwrap_or(name).to_owned()
}

/// A second made-up account id (for the second-account scenes).
fn matrix_id() -> String {
    format!("@{}:example.org", first_name(&me()).to_lowercase())
}

/// One person on twelve accounts (four on Steam, eight on Stoat): every
/// view must still show them all, or say how many.
fn many_scene(name: &'static str, screen: Screen, people: bool) -> Scene<'static> {
    Scene::new(
        name,
        Box::new(move |app, _, _, _| {
            let chat = godwit::demo::multi(app);
            let others: Vec<UserId> = (1..4)
                .map(godwit::demo::steam_user)
                .chain((5..13).map(|i| user(&format!("u{i}"))))
                .collect();
            for other in others {
                let main = app.state().contacts.of(&godwit::demo::steam_user(0)).map(|c| c.id);
                let from = app.state().contacts.of(&other).map(|c| c.id);
                if let (Some(into), Some(from)) = (main, from)
                    && into != from
                {
                    app.dispatch(Action::MergeContacts { into, from });
                }
            }
            if people {
                app.dispatch(Action::ShowPeople);
            } else {
                app.dispatch(Action::SelectRoom(chat));
            }
            screen
        }),
    )
}

/// The demo's Discord account through the real bridge: a server channel
/// (or the DM) opened, its history loaded from the made-up data.
fn discord_scene(name: &'static str, dm: bool) -> Scene<'static> {
    Scene::new(
        name,
        Box::new(move |app, _, _, _| {
            use godwit_core::account::NetworkAccount;
            let (ready, history) = godwit::demo::discord_world();
            let (mut discord, events) = godwit_discord::account::DiscordAccount::demo(ready, history);
            for event in events {
                app.apply(event);
            }
            let room = app
                .state()
                .rooms
                .iter()
                .find(|r| {
                    r.id.network() == Network::Discord
                        && if dm {
                            matches!(r.kind, RoomKind::Direct { .. })
                        } else {
                            matches!(r.kind, RoomKind::Channel { .. })
                        }
                })
                .map(|r| r.id.clone());
            if let Some(room) = room {
                let mut effects = Vec::new();
                if let Some(RoomKind::Channel { server }) = app.state().room(&room).map(|r| r.kind.clone()) {
                    effects.extend(app.dispatch(Action::SelectServer(Some(server))));
                }
                effects.extend(app.dispatch(Action::SelectRoom(room)));
                while let Some(effect) = effects.pop() {
                    if let godwit_core::application::Effect::Backend { command, .. } = effect {
                        for event in discord.run(command) {
                            effects.extend(app.apply(event));
                        }
                    }
                }
            }
            Screen::Main
        }),
    )
}

/// Server settings on an admin page, filled the way the app does it: the
/// demo account answers the core, and the real editor shows the result.
fn server_admin_scene(name: &'static str, page: i32) -> Scene<'static> {
    Scene::new(
        name,
        Box::new(move |app, ui, _, _| {
            use godwit_core::account::NetworkAccount;
            let mut demo = godwit::demo::DemoAccount::new(app);
            let mut run = |app: &mut Application, action: Action| {
                for effect in app.dispatch(action) {
                    if let godwit_core::application::Effect::Backend { command, .. } = effect {
                        for event in demo.run(command) {
                            app.apply(event);
                        }
                    }
                }
            };
            run(app, Action::OpenServerSettings(ServerId::new(account(), "S")));
            run(
                app,
                Action::ServerSettingsPage(godwit::server_settings::page_from_index(page)),
            );
            let mut images = Images::offline();
            pictures(&mut images);
            let mut editor = godwit::server_settings::ServerEditor::default();
            editor.show(ui, app.state(), &mut images);
            if page == 1 {
                editor.select_role(ui, app.state(), 1);
                editor.show(ui, app.state(), &mut images);
            }
            Screen::Main
        }),
    )
}

/// A server channel's (or category's) settings, filled the way the app does
/// it: the demo account answers the core, and the real editor shows the result.
fn channel_settings_scene() -> Scene<'static> {
    settings_scene("channel-settings", |_| {
        Action::OpenChannelSettings(RoomId::new(account(), "voice"))
    })
}

/// A category's settings: just its name, and Delete Category.
fn category_settings_scene() -> Scene<'static> {
    settings_scene("category-settings", |app| Action::OpenCategorySettings {
        server: app.state().servers[0].id.clone(),
        category: "C1".into(),
    })
}

fn settings_scene(name: &'static str, open: fn(&Application) -> Action) -> Scene<'static> {
    Scene::new(
        name,
        Box::new(move |app, ui, _, _| {
            use godwit_core::account::NetworkAccount;
            let mut demo = godwit::demo::DemoAccount::new(app);
            let action = open(app);
            for effect in app.dispatch(action) {
                if let godwit_core::application::Effect::Backend { command, .. } = effect {
                    for event in demo.run(command) {
                        app.apply(event);
                    }
                }
            }
            let mut images = Images::offline();
            pictures(&mut images);
            let mut editor = godwit::channel_settings::ChannelEditor::default();
            editor.show(ui, app.state(), &mut images);
            ui.set_channel_edit_mature(true);
            editor.edited();
            editor.show(ui, app.state(), &mut images);
            Screen::Main
        }),
    )
}

/// The Create channel dialog with Category chosen (the server allows it).
fn create_category_scene() -> Scene<'static> {
    Scene::new(
        "create-category",
        Box::new(|_, ui, _, _| {
            ui.set_server_can_create_category(true);
            ui.set_new_channel_kind(2);
            ui.set_new_channel_name(filler(5, 2).into());
            ui.set_creating_channel(true);
            Screen::Main
        }),
    )
}

/// The same dialog from a category's "Create channel" entry.
fn create_in_category_scene() -> Scene<'static> {
    Scene::new(
        "create-in-category",
        Box::new(|_, ui, _, _| {
            ui.set_server_can_create_category(true);
            ui.set_new_channel_category("C1".into());
            ui.set_new_channel_category_name("Announcements".into());
            ui.set_new_channel_name(filler(6, 2).into());
            ui.set_creating_channel(true);
            Screen::Main
        }),
    )
}

/// A smooth test picture, `w`×`h`.
fn gradient(w: u32, h: u32) -> slint::Image {
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(w, h);
    for (i, p) in pixels.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (i as u32 % w, i as u32 / w);
        *p = slint::Rgba8Pixel {
            r: (x * 255 / w) as u8,
            g: 90,
            b: (y * 255 / h) as u8,
            a: 255,
        };
    }
    slint::Image::from_rgba8(pixels)
}

/// A large call exercises both the occupancy badge and scrolling to the last participant.
fn call_many(app: &mut Application, _: &AppWindow, _: &RoomId, _: &RoomId) -> Screen {
    let lounge = RoomId::new(account(), "voice");
    if let Some(existing) = app.state().rooms.iter().find(|room| room.id == lounge) {
        let mut room = (**existing).clone();
        room.capabilities.voice_user_limit = Some(20);
        app.apply(Event::RoomUpdated(room));
    }
    app.dispatch(Action::SelectRoom(lounge.clone()));
    app.dispatch(Action::JoinCall(lounge.clone()));
    app.apply(Event::CallConnected { room: lounge.clone() });
    let members: Vec<CallMember> = (0..15)
        .map(|i| CallMember {
            user: user(&format!("guest{i}")),
            name: format!("Guest {}", i + 1).into(),
            avatar_url: None,
            speaking: i == 0,
            muted: i % 3 == 0,
            deafened: false,
        })
        .collect();
    app.apply(Event::VoiceOccupants {
        room: lounge.clone(),
        members: members.clone(),
    });
    app.apply(Event::CallMembers { room: lounge, members });
    Screen::Main
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::args()
        .nth(1)
        .ok_or("usage: preview <output dir> [scene prefix]")?;
    // Only scenes whose name starts with this, if given.
    let only = std::env::args().nth(2);
    std::fs::create_dir_all(&out)?;
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window.clone())))?;

    let multi = |name: &'static str, show: fn(&mut Application, &RoomId), hover: Option<(f32, f32)>| Scene {
        hover,
        ..Scene::new(
            name,
            Box::new(move |app, _, _, _| {
                let chat = godwit::demo::multi(app);
                show(app, &chat);
                Screen::Main
            }),
        )
    };
    let scenes = vec![
        // Several accounts (the demo's Steam one): Home merges a person on
        // two networks into one row; the Steam chat shows its markup.
        multi(
            "multi-home",
            |app, chat| {
                app.dispatch(Action::SelectServer(None));
                app.dispatch(Action::SelectRoom(chat.clone()));
            },
            None,
        ),
        multi(
            "multi-friends",
            |app, _| {
                app.dispatch(Action::ShowFriends);
            },
            None,
        ),
        multi(
            "multi-people",
            |app, _| {
                app.dispatch(Action::ShowPeople);
            },
            None,
        ),
        // The pointer on the first account's avatar: the fold arrow.
        multi(
            "multi-rail-hover",
            |app, _| {
                app.dispatch(Action::SelectServer(None));
            },
            Some((32.0, 92.0)),
        ),
        // Home's own arrangement: one chat pinned, one hidden (section open).
        Scene::new(
            "home-pinned-hidden",
            Box::new(|app, _, _, dm| {
                app.dispatch(Action::SelectServer(None));
                app.dispatch(Action::PinRoom {
                    room: RoomId::new(account(), "dm3"),
                    pinned: true,
                });
                app.dispatch(Action::HideRoom {
                    room: RoomId::new(account(), "dm5"),
                    hidden: true,
                });
                app.dispatch(Action::ToggleHiddenRooms);
                app.dispatch(Action::SelectRoom(dm.clone()));
                Screen::Main
            }),
        ),
        many_scene("many-accounts-chat", Screen::Main, false),
        many_scene("many-accounts-people", Screen::Main, true),
        // A merged person with an alt (two Steam accounts) and Stoat: each
        // network tab says which account it is, on a strip of its own.
        Scene::new(
            "merged-alt-chat",
            Box::new(|app, _, _, _| {
                let chat = godwit::demo::multi(app);
                let main = app.state().contacts.of(&godwit::demo::steam_user(0)).map(|c| c.id);
                let alt = app.state().contacts.of(&godwit::demo::steam_user(1)).map(|c| c.id);
                if let (Some(into), Some(from)) = (main, alt) {
                    app.dispatch(Action::MergeContacts { into, from });
                }
                app.dispatch(Action::SelectRoom(chat));
                Screen::Main
            }),
        ),
        Scene::new(
            "steam-login",
            Box::new(|_, ui, _, _| {
                let steam = godwit::view::ACCOUNT_NETWORKS
                    .iter()
                    .position(|n| *n == Network::Steam)
                    .and_then(|i| i32::try_from(i).ok());
                ui.set_login_network(steam.unwrap_or(-1));
                if let Some(qr) = godwit::qr::image("https://s.team/q/1/1234567890123456789") {
                    ui.set_login_qr(qr);
                    ui.set_login_has_qr(true);
                }
                Screen::Login
            }),
        ),
        discord_scene("discord-channel", false),
        discord_scene("discord-dm", true),
        Scene::new(
            "discord-privacy",
            Box::new(|_, ui, _, _| {
                ui.set_discord_privacy_open(true);
                Screen::Main
            }),
        ),
        // Discord's QR sign-in, after someone scanned the code.
        Scene::new(
            "discord-login",
            Box::new(|_, ui, _, _| {
                let discord = godwit::view::ACCOUNT_NETWORKS
                    .iter()
                    .position(|n| *n == Network::Discord)
                    .and_then(|i| i32::try_from(i).ok());
                ui.set_login_network(discord.unwrap_or(-1));
                if let Some(qr) = godwit::qr::image("https://discord.com/ra/0123456789abcdefABCDEF0123456789abcdefABCD")
                {
                    ui.set_login_qr(qr);
                    ui.set_login_has_qr(true);
                }
                ui.set_login_qr_scanned(true);
                ui.set_login_mfa_hint(
                    format!(
                        "Scanned by {}. Confirm the sign-in on your phone.",
                        filler(91, 1).to_lowercase()
                    )
                    .into(),
                );
                Screen::Login
            }),
        ),
        Scene::new("channel", Box::new(|_, _, _, _| Screen::Main)),
        Scene::new(
            "composer-emoji",
            Box::new(|app, _, _, _| {
                app.dispatch(Action::SetDraft("Hello 👋 😀 ❤️ 👩‍💻 🇫🇮 — text after emoji".into()));
                Screen::Main
            }),
        ),
        Scene {
            hover: Some((700.0, 330.0)),
            ..Scene::new("channel-hover", Box::new(|_, _, _, _| Screen::Main))
        },
        Scene {
            hover: Some((150.0, 116.0)),
            ..Scene::new("sidebar-hover", Box::new(|_, _, _, _| Screen::Main))
        },
        Scene::new(
            "home-dms",
            Box::new(|app, _, _, dm| {
                app.dispatch(Action::SelectServer(None));
                app.dispatch(Action::SelectRoom(dm.clone()));
                Screen::Main
            }),
        ),
        Scene::new(
            "settings",
            Box::new(|_, ui, _, _| {
                ui.set_settings_open(true);
                Screen::Main
            }),
        ),
        // Notifications, from the demo's made-up alerts (the badge as it
        // shows elsewhere; on the page itself it clears).
        Scene::new(
            "notifications",
            Box::new(|app, ui, _, _| {
                godwit::demo::multi(app);
                app.dispatch(Action::ShowNotifications);
                let mut list = godwit::notifications::Notifications::default();
                for n in godwit::demo::notifications(app.state()) {
                    list.push(n);
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as i64);
                let rows = godwit::notifications::rows(&list, app.state(), now, |_| (slint::Image::default(), false));
                ui.set_notification_items(slint::ModelRc::new(slint::VecModel::from(rows)));
                ui.set_notifications_unseen(3);
                Screen::Main
            }),
        ),
        Scene::new("login", Box::new(|_, _, _, _| Screen::Login)),
        Scene {
            late: true,
            scroll: Some((180.0, 400.0, -2000.0)),
            ..Scene::new(
                "home-dms-scrolled",
                Box::new(|app, _, _, dm| {
                    app.dispatch(Action::SelectServer(None));
                    app.dispatch(Action::SelectRoom(dm.clone()));
                    Screen::Main
                }),
            )
        },
        Scene::new("profile", Box::new(|app, _, _, _| open_profile(app, false))),
        Scene::new("profile-blocked", Box::new(|app, _, _, _| open_profile(app, true))),
        Scene::new(
            "picker",
            Box::new(|_, ui, _, _| {
                ui.set_picker_open(true);
                Screen::Main
            }),
        ),
        Scene {
            hover: Some((700.0, 140.0)),
            ..Scene::new("people", Box::new(people))
        },
        Scene::new(
            "people-from-card",
            Box::new(|app, ui, a, b| {
                people(app, ui, a, b);
                open_profile(app, false);
                app.dispatch(Action::LinkFrom(user("u0")));
                Screen::Main
            }),
        ),
        Scene {
            hover: Some((700.0, 220.0)),
            click: Some((LINK_X, LINK_Y)),
            ..Scene::new("people-linking", Box::new(people))
        },
        Scene::new("friends", Box::new(friends_scene)),
        Scene::new("friends-create-group", Box::new(friends_create_group_scene)),
        Scene {
            scroll: Some((650.0, 300.0, -700.0)),
            size: Some((800, 480)),
            ..Scene::new("friends-pending-scrolled", Box::new(friends_scene))
        },
        Scene::new(
            "join-server-preview",
            Box::new(|_, ui, _, _| {
                ui.set_join_server_accounts(slint::ModelRc::new(slint::VecModel::from(vec!["Demo — Stoat".into()])));
                ui.set_join_server_name("Demo server".into());
                ui.set_join_server_status("87 members".into());
                ui.set_join_server_ready(true);
                ui.set_join_server_open(true);
                Screen::Main
            }),
        ),
        Scene::new(
            "profile-settings",
            Box::new(|_, ui, _, _| {
                ui.set_settings_open(true);
                ui.set_settings_section(10);
                ui.set_settings_account(0);
                ui.set_settings_account_profile(true);
                ui.set_settings_account_tab(0);
                ui.set_edit_profile_accounts(slint::ModelRc::new(slint::VecModel::from(vec!["Demo — Stoat".into()])));
                ui.set_edit_profile_loaded(true);
                ui.set_edit_profile_name("Demo profile".into());
                ui.set_edit_profile_pronouns("they/them".into());
                ui.set_edit_profile_bio("A **profile bio**.\nIt can span several lines.".into());
                ui.set_edit_profile_avatar_label("Current avatar".into());
                ui.set_edit_profile_banner_label("Current banner".into());
                Screen::Main
            }),
        ),
        // The server menu, opened from the sidebar's header.
        Scene {
            click: Some((140.0, 36.0)),
            ..Scene::new(
                "server-menu",
                Box::new(|_, ui, _, _| {
                    ui.set_server_menu(true);
                    ui.set_server_settings_available(true);
                    ui.set_server_can_invite(true);
                    ui.set_server_can_create_channel(true);
                    ui.set_server_can_leave(true);
                    Screen::Main
                }),
            )
        },
        Scene::new(
            "server-invite",
            Box::new(|_, ui, _, _| {
                ui.set_invite_open(true);
                ui.set_invite_link(format!("https://stt.gg/{}", filler(80, 1).to_lowercase()).into());
                Screen::Main
            }),
        ),
        server_admin_scene("server-roles", 1),
        server_admin_scene("server-emoji", 2),
        server_admin_scene("server-invites", 3),
        server_admin_scene("server-bans", 4),
        // A banner wider and taller than its frame must stay inside it.
        Scene::new(
            "server-banner",
            Box::new(|_, ui, _, _| {
                let (w, h) = (1500u32, 500u32);
                let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(w, h);
                for (i, p) in pixels.make_mut_slice().iter_mut().enumerate() {
                    let (x, y) = (i as u32 % w, i as u32 / w);
                    *p = slint::Rgba8Pixel {
                        r: (x * 255 / w) as u8,
                        g: 90,
                        b: (y * 255 / h) as u8,
                        a: 255,
                    };
                }
                ui.set_server_settings_open(true);
                ui.set_server_edit_loaded(true);
                ui.set_server_edit_allowed(true);
                ui.set_server_edit_name(filler(60, 2).into());
                ui.set_server_edit_banner(slint::Image::from_rgba8(pixels));
                ui.set_server_edit_has_banner(true);
                Screen::Main
            }),
        ),
        Scene::new(
            "viewer",
            Box::new(|_, ui, _, _| {
                ui.set_viewer_image(gradient(1600, 900));
                ui.set_viewer_width(1600);
                ui.set_viewer_height(900);
                ui.set_viewer_name("holiday.png".into());
                ui.set_viewer_position("2 / 3".into());
                ui.set_viewer_web(true);
                ui.set_viewer_zoom(2.0);
                ui.set_viewer_pan_x(300.0);
                ui.set_viewer_open(true);
                Screen::Main
            }),
        ),
        // Windows' screen share picker (screens and apps in two tabs).
        Scene::new("share-source-screens", Box::new(|_, ui, _, _| share_source(ui, 0))),
        Scene::new("share-source-apps", Box::new(|_, ui, _, _| share_source(ui, 1))),
        Scene::new(
            "crop-avatar",
            Box::new(|_, ui, _, _| {
                ui.set_crop_image(gradient(1200, 800));
                ui.set_crop_image_width(1200);
                ui.set_crop_image_height(800);
                ui.set_crop_frame_width(280.0);
                ui.set_crop_frame_height(280.0);
                ui.set_crop_round(true);
                ui.set_crop_title("Crop your avatar".into());
                ui.set_crop_note("Saved as 512×512, at most 2.5 MB. Drag to move, scroll or slide to zoom.".into());
                ui.set_crop_zoom(1.5);
                ui.set_crop_open(true);
                Screen::Main
            }),
        ),
        Scene::new(
            "crop-banner",
            Box::new(|_, ui, _, _| {
                ui.set_crop_image(gradient(1200, 800));
                ui.set_crop_image_width(1200);
                ui.set_crop_image_height(800);
                ui.set_crop_frame_width(440.0);
                ui.set_crop_frame_height(220.0);
                ui.set_crop_round(false);
                ui.set_crop_title("Crop the server banner".into());
                ui.set_crop_note("Saved as 1200×600, at most 6.0 MB.".into());
                ui.set_crop_open(true);
                Screen::Main
            }),
        ),
        Scene::new(
            "pins",
            Box::new(|_, ui, _, _| {
                let item = |a: &str, t: &str| godwit::ui::PinItem {
                    author: a.into(),
                    text: t.into(),
                    time: "3 Nov 2026".into(),
                };
                ui.set_pins(slint::ModelRc::new(slint::VecModel::from(vec![
                    item("Robin", "The meeting moved to Thursday at 10."),
                    item("Sam", &filler(30, 3)),
                ])));
                ui.set_can_pin(true);
                ui.set_pins_open(true);
                Screen::Main
            }),
        ),
        Scene::new(
            "games",
            Box::new(|_, ui, _, _| {
                use slint::{ModelRc, VecModel};
                let friend = |n: &str, picked: bool| godwit::ui::GamesFriendItem { name: n.into(), picked };
                ui.set_games_friends(ModelRc::new(VecModel::from(vec![
                    friend("Robin", true),
                    friend("Sam", true),
                    friend("Kai", false),
                ])));
                ui.set_games_notes(ModelRc::new(VecModel::from(vec![slint::SharedString::from(
                    "Kai's games are private.",
                )])));
                ui.set_games_venn(3);
                ui.set_games_legend("Left: You · Right: Robin · Bottom: Sam".into());
                ui.set_games_regions(ModelRc::new(VecModel::from(vec![0, 4, 2, 3, 1, 5, 2, 6])));
                ui.set_games_region(7);
                let row = |header: bool, t: &str| godwit::ui::GameRowItem {
                    header,
                    title: t.into(),
                    image: slint::Image::default(),
                    has_image: false,
                };
                ui.set_games_rows(ModelRc::new(VecModel::from(vec![
                    row(true, "Everyone owns (6)"),
                    row(false, "Goose Chase"),
                    row(false, "Harbour Lights"),
                    row(false, "Star Forge"),
                ])));
                ui.set_games_accounts(ModelRc::new(VecModel::from(vec![
                    slint::SharedString::from("Ana"),
                    slint::SharedString::from("Ana (work)"),
                ])));
                ui.set_games_query("".into());
                ui.set_games_open(true);
                ui.set_showing_games(true);
                ui.set_can_show_games(true);
                Screen::Main
            }),
        ),
        Scene::new(
            "settings-account",
            Box::new(|_, ui, _, _| {
                ui.set_settings_open(true);
                ui.set_settings_section(10);
                ui.set_settings_account(0);
                ui.set_settings_account_profile(false);
                ui.set_settings_account_tab(1);
                Screen::Main
            }),
        ),
        Scene::new(
            "settings-discord-privacy",
            Box::new(|_, ui, _, _| {
                use slint::ComponentHandle;
                ui.set_settings_open(true);
                ui.set_settings_section(10);
                ui.set_settings_account(0);
                ui.set_settings_account_profile(true);
                ui.set_settings_account_discord(true);
                ui.set_settings_account_tab(3);
                let item = |label: &str, detail: &str, on| godwit::ui::PrivItem {
                    label: label.into(),
                    detail: detail.into(),
                    on,
                };
                ui.global::<godwit::ui::AccountPages>()
                    .set_privacy(slint::ModelRc::new(slint::VecModel::from(vec![
                        item(
                            "Use data to improve Discord",
                            "Lets Discord use how you use it to improve the service.",
                            false,
                        ),
                        item(
                            "Use data to personalize my Discord experience",
                            "Lets Discord use what you do to tailor what it shows you.",
                            false,
                        ),
                        item("Share my activity", "Shows what you're playing in your status.", true),
                    ])));
                Screen::Main
            }),
        ),
        Scene::new(
            "settings-discord-sessions",
            Box::new(|_, ui, _, _| {
                use slint::ComponentHandle;
                ui.set_settings_open(true);
                ui.set_settings_section(10);
                ui.set_settings_account(0);
                ui.set_settings_account_profile(true);
                ui.set_settings_account_discord(true);
                ui.set_settings_account_tab(4);
                let device = |device: &str, detail: &str| godwit::ui::SessItem {
                    device: device.into(),
                    detail: detail.into(),
                };
                let pages = ui.global::<godwit::ui::AccountPages>();
                pages.set_sessions(slint::ModelRc::new(slint::VecModel::from(vec![
                    device("Linux · Discord Client", "Lorem, NL · last used Nov 4"),
                    device("Android", "last used Nov 1"),
                ])));
                pages.set_confirm_session(1);
                Screen::Main
            }),
        ),
        Scene::new(
            "settings-autodelete",
            Box::new(|_, ui, _, _| {
                use slint::ComponentHandle;
                ui.set_settings_open(true);
                ui.set_settings_section(10);
                ui.set_settings_account(0);
                ui.set_settings_account_profile(true);
                ui.set_settings_account_autodelete(true);
                ui.set_settings_account_tab(2);
                let rule = godwit::ui::AdRuleItem {
                    name: "Old DMs".into(),
                    enabled: true,
                    everyone: false,
                    r#where: "all DMs and groups".into(),
                    what: "All messages".into(),
                    when: "Older than 90 days (keeps going)".into(),
                    progress: "Indexing: 3 of 5 channels done · ready to delete now: 12".into(),
                    deleted: "48".into(),
                    error: "".into(),
                };
                let ad = ui.global::<godwit::ui::AdState>();
                ad.set_rules(slint::ModelRc::new(slint::VecModel::from(vec![rule])));
                ad.set_recheck_days(90);
                Screen::Main
            }),
        ),
        Scene {
            size: Some((1280, 1000)),
            ..Scene::new(
                "settings-autodelete-editor",
                Box::new(|_, ui, _, _| {
                    use slint::ComponentHandle;
                    ui.set_settings_open(true);
                    ui.set_settings_section(10);
                    ui.set_settings_account(0);
                    ui.set_settings_account_profile(true);
                    ui.set_settings_account_autodelete(true);
                    ui.set_settings_account_tab(2);
                    let ad = ui.global::<godwit::ui::AdState>();
                    let place = |name: &str, chosen, header| godwit::ui::AdPlaceItem {
                        name: name.into(),
                        chosen,
                        header,
                    };
                    ad.set_places(slint::ModelRc::new(slint::VecModel::from(vec![
                        place("Direct messages and groups", false, true),
                        place("Xaxais", true, false),
                        place("Dolor sit", false, false),
                        place("Servers (each taken whole)", false, true),
                        place("Lorem", true, false),
                        place("Ipsum", false, false),
                    ])));
                    ad.set_step(1);
                    Screen::Main
                }),
            )
        },
        Scene::new(
            "settings-appearance",
            Box::new(|_, ui, _, _| {
                ui.set_settings_open(true);
                ui.set_settings_section(3);
                ui.set_accent_index(2);
                ui.set_style_index(1);
                Screen::Main
            }),
        ),
        Scene::new(
            "sidebar-banner",
            Box::new(|app, ui, _, _| {
                // The selected server gets a banner (its picture is put in the cache below).
                let account = app.state().servers.first().map(|sv| sv.id.account.clone());
                if let Some(account) = account {
                    let servers: Vec<_> = app
                        .state()
                        .servers
                        .iter()
                        .filter(|sv| sv.id.account == account)
                        .map(|sv| {
                            let mut sv = (**sv).clone();
                            sv.banner_url = Some("https://example.com/banner.png".into());
                            sv
                        })
                        .collect();
                    app.apply(Event::ServersChanged { account, servers });
                }
                let _ = ui;
                Screen::Main
            }),
        ),
        Scene::new(
            "incoming-call",
            Box::new(|_, ui, _, _| {
                ui.set_incoming_call_name("Robin".into());
                ui.set_incoming_call_body("Incoming voice call · Steam".into());
                ui.set_incoming_call_initials("R".into());
                ui.set_incoming_call_open(true);
                Screen::Main
            }),
        ),
        Scene::new(
            "search",
            Box::new(|_, ui, _, _| {
                let item = |a: &str, t: &str| godwit::ui::PinItem {
                    author: a.into(),
                    text: t.into(),
                    time: "1 Nov 2026".into(),
                };
                ui.set_search_results(slint::ModelRc::new(slint::VecModel::from(vec![
                    item("Robin", "Lunch at the usual place?"),
                    item("Sam", "lunch was great, thanks"),
                ])));
                ui.set_search_asked(true);
                ui.set_search_open(true);
                Screen::Main
            }),
        ),
        Scene {
            late: true,
            ..Scene::new(
                "call-video",
                Box::new(|app, ui, general, _| {
                    app.dispatch(Action::SelectRoom(general.clone()));
                    ui.set_call_here(true);
                    let tile = |label: &str, screen: bool| godwit::ui::CallVideoItem {
                        label: label.into(),
                        frame: gradient(1280, 720),
                        has_frame: true,
                        screen,
                        stream: -1,
                        participant: -1,
                        primary: false,
                    };
                    ui.set_call_videos(slint::ModelRc::new(slint::VecModel::from(vec![tile(
                        "Robin · Screen",
                        true,
                    )])));
                    ui.set_call_screen_videos(1);
                    Screen::Main
                }),
            )
        },
        Scene {
            scroll: Some((640.0, 400.0, -2000.0)),
            ..Scene::new(
                "server-settings",
                Box::new(|_, ui, _, _| {
                    let channels: Vec<slint::SharedString> = std::iter::once("Disabled".to_owned())
                        .chain((0..4).map(|i| filler(70 + i, 1).to_lowercase()))
                        .map(Into::into)
                        .collect();
                    ui.set_server_settings_open(true);
                    ui.set_server_edit_loaded(true);
                    ui.set_server_edit_allowed(true);
                    ui.set_server_edit_dirty(true);
                    ui.set_server_edit_name(filler(60, 2).into());
                    ui.set_server_edit_description(filler(61, 18).into());
                    ui.set_server_edit_icon_label("Will be removed when you save.".into());
                    ui.set_server_edit_channels(slint::ModelRc::new(slint::VecModel::from(channels)));
                    ui.set_server_edit_joined(1);
                    ui.set_server_edit_left(1);
                    Screen::Main
                }),
            )
        },
        channel_settings_scene(),
        category_settings_scene(),
        create_category_scene(),
        create_in_category_scene(),
        Scene::new(
            "replying",
            Box::new(|app, _, _, _| {
                app.dispatch(Action::ReplyTo(MessageId("07".into())));
                app.dispatch(Action::AddFiles(vec![Outgoing {
                    path: "/nonexistent/holiday.jpg".into(),
                    name: format!("{}.jpg", filler(41, 1).to_lowercase()).into(),
                    size: 2_400_000,
                }]));
                app.dispatch(Action::SetDraft(filler(42, 5)));
                Screen::Main
            }),
        ),
        Scene::new("light", Box::new(|_, _, _, _| Screen::Main)),
        Scene::new(
            "settings-verify",
            Box::new(|app, ui, _, _| {
                // A second account whose device needs verifying.
                let matrix = AccountId::new(Network::Discord, matrix_id());
                app.apply(Event::AccountReady {
                    account: matrix.clone(),
                    display_name: me().into(),
                    username: matrix_id().into(),
                    avatar_url: None,
                    rooms: Vec::new(),
                    servers: Vec::new(),
                });
                app.apply(Event::DeviceTrust {
                    account: matrix,
                    unverified: true,
                });
                ui.set_settings_open(true);
                Screen::Main
            }),
        ),
        Scene::new(
            "settings-compare",
            Box::new(|app, ui, _, _| {
                let matrix = AccountId::new(Network::Discord, matrix_id());
                app.apply(Event::AccountReady {
                    account: matrix.clone(),
                    display_name: me().into(),
                    username: matrix_id().into(),
                    avatar_url: None,
                    rooms: Vec::new(),
                    servers: Vec::new(),
                });
                app.apply(Event::DeviceTrust {
                    account: matrix.clone(),
                    unverified: true,
                });
                app.dispatch(Action::StartVerification(matrix.clone()));
                let seven = [
                    ("🐶", "Dog"),
                    ("🦄", "Unicorn"),
                    ("❤️", "Heart"),
                    ("🎅", "Santa"),
                    ("✈️", "Aeroplane"),
                    ("⚽", "Ball"),
                    ("📌", "Pin"),
                ];
                app.apply(Event::Verification {
                    account: matrix,
                    step: Verify::Compare(seven.iter().map(|(e, n)| ((*e).into(), (*n).into())).collect()),
                });
                ui.set_settings_open(true);
                Screen::Main
            }),
        ),
        {
            // Settings isn't opened here: the request opens it.
            Scene::new(
                "settings-incoming",
                Box::new(|app, _, _, _| {
                    let matrix = AccountId::new(Network::Discord, matrix_id());
                    app.apply(Event::AccountReady {
                        account: matrix.clone(),
                        display_name: me().into(),
                        username: matrix_id().into(),
                        avatar_url: None,
                        rooms: Vec::new(),
                        servers: Vec::new(),
                    });
                    app.apply(Event::Verification {
                        account: matrix,
                        step: Verify::Incoming,
                    });
                    Screen::Main
                }),
            )
        },
        Scene::new(
            "invite",
            Box::new(|app, _, _, _| {
                let mut r = room("invite", &person(9), RoomKind::Direct { with: user("u9") }, true, 0);
                r.invited_by = Some(person(9).into());
                let id = r.id.clone();
                app.apply(Event::RoomUpdated(r));
                app.dispatch(Action::SelectRoom(id));
                Screen::Main
            }),
        ),
        Scene::new(
            "directory",
            Box::new(|app, _, _, _| {
                let server = ServerId::new(account(), "S");
                let d = |id: &str, name: &str, topic: Option<&str>, members: u64| DirectoryRoom {
                    id: RoomId::new(account(), id),
                    name: name.into(),
                    topic: topic.map(Into::into),
                    members,
                };
                app.apply(Event::Directory {
                    server,
                    rooms: vec![
                        d("d1", "announcements", Some("News about Godwit, rarely"), 412),
                        d("d2", "off-topic", None, 37),
                        d("general", "general", None, 9),
                    ],
                });
                Screen::Main
            }),
        ),
        Scene {
            size: Some((1280, 1400)),
            ..Scene::new("channel-tall", Box::new(|_, _, _, _| Screen::Main))
        },
        Scene {
            click: Some((32.0, 92.0)),
            ..{
                // Our avatar in the rail opens it.
                Scene::new("account-menu", Box::new(|_, _, _, _| Screen::Main))
            }
        },
        Scene::new(
            "settings-voice",
            Box::new(|_, ui, _, _| {
                ui.set_settings_open(true);
                ui.set_settings_section(2);
                let devices = |names: &[&str]| {
                    slint::ModelRc::new(slint::VecModel::from(
                        names.iter().map(|n| slint::SharedString::from(*n)).collect::<Vec<_>>(),
                    ))
                };
                ui.set_inputs(devices(&["System default", "USB microphone", "Webcam microphone"]));
                ui.set_outputs(devices(&["System default", "Headphones"]));
                ui.set_noise_index(2);
                ui.set_echo_cancellation(true);
                ui.set_auto_gain(true);
                Screen::Main
            }),
        ),
        Scene::new(
            "settings-about",
            Box::new(|_, ui, _, _| {
                ui.set_settings_open(true);
                ui.set_settings_section(4);
                ui.set_memory("14.0 MB".into());
                ui.set_version("0.1.0".into());
                Screen::Main
            }),
        ),
        Scene {
            scroll: Some((650.0, 220.0, -800.0)),
            ..Scene::new("call-many-bottom", Box::new(call_many))
        },
        Scene::new("call-many", Box::new(call_many)),
        Scene::new("call", Box::new(|app, _, _, _| call_with_camera(app))),
        Scene {
            // Right-click the third person's tile (their sound menu).
            right_click: Some((876.0, 370.0)),
            ..Scene::new("call-muted-here", Box::new(|app, _, _, _| call_with_camera(app)))
        },
        Scene {
            late: true,
            // "Show people", top right.
            click: Some((1150.0, 32.0)),
            ..Scene::new(
                "call-fullscreen-people",
                Box::new(|app, ui, _, _| {
                    let screen = call_with_camera(app);
                    ui.set_fullscreen_video(0);
                    screen
                }),
            )
        },
        Scene {
            late: true,
            // The tile's eye button, bottom right beside fullscreen.
            click: Some((CALL_STREAM_EYE.0, CALL_STREAM_EYE.1)),
            ..Scene::new("call-stream-viewers", Box::new(|app, _, _, _| call_with_stream(app)))
        },
        Scene {
            late: true,
            ..Scene::new(
                "call-stream-fullscreen",
                Box::new(|app, ui, _, _| {
                    let screen = call_with_stream(app);
                    ui.set_fullscreen_video(0);
                    screen
                }),
            )
        },
        Scene::new(
            "signin",
            Box::new(|_, ui, _, _| {
                ui.set_login_network(0);
                Screen::Login
            }),
        ),
        Scene {
            right_click: Some((600.0, 660.0)),
            ..Scene::new("menu", Box::new(|_, _, _, _| Screen::Main))
        },
        Scene {
            size: Some((800, 600)),
            ..Scene::new("narrow", Box::new(|_, _, _, _| Screen::Main))
        },
        Scene::new(
            "empty-room",
            Box::new(|app, _, _, _| {
                let random = RoomId::new(account(), "random");
                app.dispatch(Action::SelectRoom(random.clone()));
                app.apply(Event::History {
                    room: random,
                    before: None,
                    messages: Vec::new(),
                    reached_start: true,
                });
                Screen::Main
            }),
        ),
        Scene::new("connecting", Box::new(|_, _, _, _| Screen::Connecting)),
        Scene::new(
            "long-names",
            Box::new(|app, _, general, _| {
                let server = ServerId::new(account(), "S");
                let long = "a-really-long-channel-name-that-goes-on-and-on-and-on";
                app.apply(Event::RoomUpdated(room(
                    "general",
                    long,
                    RoomKind::Channel { server: server.clone() },
                    false,
                    0,
                )));
                let mut servers: Vec<Server> = (0..18)
                    .map(|i| Server {
                        id: ServerId::new(account(), format!("X{i}").as_str()),
                        name: format!("Server number {i}").into(),
                        icon_url: None,
                        banner_url: None,
                        categories: Vec::new(),
                        muted: false,
                        folder: None,
                        rights: Default::default(),
                    })
                    .collect();
                servers.insert(
                    0,
                    Server {
                        id: server.clone(),
                        name: filler(9, 11).into(),
                        icon_url: None,
                        banner_url: None,
                        categories: vec![Category {
                            id: None,
                            can_manage: false,
                            title: "A category whose title is much too long for the sidebar".into(),
                            rooms: vec![general.clone(), RoomId::new(account(), "dev")],
                        }],
                        muted: false,
                        folder: None,
                        rights: Default::default(),
                    },
                );
                app.apply(Event::ServersChanged {
                    account: account(),
                    servers,
                });
                app.apply(Event::RoomUpdated(room(
                    "dev",
                    "dev",
                    RoomKind::Channel { server },
                    true,
                    1234,
                )));
                Screen::Main
            }),
        ),
        // Server folders: the first is open (a header tile, its servers on
        // its colour), the second closed (a grid of its servers' pictures).
        Scene {
            open_folder: Some("fa"),
            ..Scene::new(
                "rail-folders",
                Box::new(|app, _, _, _| {
                    use godwit_core::server::ServerFolder;
                    let folder = |id: &str, name: Option<&str>, colour: u32| {
                        Some(std::sync::Arc::new(ServerFolder {
                            id: id.into(),
                            name: name.map(Into::into),
                            colour: Some(colour),
                        }))
                    };
                    let one =
                        |id: &str, folder: Option<std::sync::Arc<ServerFolder>>, unread_room: Option<&str>| Server {
                            id: ServerId::new(account(), id),
                            name: filler(9, 7).into(),
                            icon_url: None,
                            banner_url: None,
                            categories: unread_room
                                .map(|r| {
                                    vec![Category {
                                        id: None,
                                        can_manage: false,
                                        title: "".into(),
                                        rooms: vec![RoomId::new(account(), r)],
                                    }]
                                })
                                .unwrap_or_default(),
                            muted: false,
                            folder,
                            rights: Default::default(),
                        };
                    let (a, b) = (folder("fa", Some("Friends"), 0x5865f2), folder("fb", None, 0x3ba55d));
                    app.apply(Event::ServersChanged {
                        account: account(),
                        servers: vec![
                            one("F1", a.clone(), None),
                            one("F2", a, None),
                            one("F3", b.clone(), None),
                            one("F4", b.clone(), None),
                            one("F5", b, None),
                            one("F6", None, None),
                        ],
                    });
                    Screen::Main
                }),
            )
        },
        Scene::new(
            "nudge",
            Box::new(|app, _, _, dm| {
                app.dispatch(Action::SelectServer(None));
                app.dispatch(Action::SelectRoom(dm.clone()));
                app.apply(Event::History {
                    room: dm.clone(),
                    before: None,
                    messages: vec![msg("d1", "u0", &person(0), 0, &filler(11, 6))],
                    reached_start: true,
                });
                Screen::Main
            }),
        ),
        Scene {
            hover: Some((700.0, 660.0)),
            click: Some((963.0, 638.0)),
            ..Scene::new("delete", Box::new(|_, _, _, _| Screen::Main))
        },
        Scene::new(
            "light-settings",
            Box::new(|_, ui, _, _| {
                ui.set_settings_open(true);
                ui.set_settings_section(1);
                ui.set_notifications(true);
                Screen::Main
            }),
        ),
        Scene {
            escape: true,
            ..Scene::new(
                "settings-escape",
                Box::new(|_, ui, _, _| {
                    ui.set_settings_open(true);
                    Screen::Main
                }),
            )
        },
        Scene {
            escape: true,
            ..Scene::new(
                "picker-escape",
                Box::new(|_, ui, _, _| {
                    ui.set_picker_open(true);
                    Screen::Main
                }),
            )
        },
        // Dialog composition and two-way bindings after extraction. All
        // content is generated; these scenes never create a backend.
        Scene::new(
            "cleanup-delete",
            Box::new(|_, ui, _, _| {
                ui.set_confirm_delete(0);
                Screen::Main
            }),
        ),
        Scene::new(
            "cleanup-leave",
            Box::new(|_, ui, _, _| {
                ui.set_confirm_leave(true);
                Screen::Main
            }),
        ),
        Scene::new(
            "cleanup-signout",
            Box::new(|_, ui, _, _| {
                ui.set_confirm_log_out(0);
                Screen::Main
            }),
        ),
        Scene::new(
            "cleanup-ban",
            Box::new(|app, ui, _, _| {
                open_profile(app, false);
                member_choices(ui);
                ui.set_confirm_moderate(2);
                ui.set_ban_delete_index(4);
                ui.set_ban_reason("Example reason".into());
                Screen::Main
            }),
        ),
        // The member card's roles checklist, timeout and nickname dialogs.
        Scene::new(
            "member-roles",
            Box::new(|app, ui, _, _| {
                open_profile(app, false);
                ui.set_roles_open(true);
                Screen::Main
            }),
        ),
        Scene::new(
            "member-timeout",
            Box::new(|app, ui, _, _| {
                open_profile(app, false);
                member_choices(ui);
                ui.set_timeout_open(true);
                Screen::Main
            }),
        ),
        Scene::new(
            "member-nickname",
            Box::new(|app, ui, _, _| {
                open_profile(app, false);
                ui.set_nickname_text("Nickname".into());
                ui.set_nickname_open(true);
                Screen::Main
            }),
        ),
        Scene::new(
            "report-user",
            Box::new(|app, ui, _, _| {
                open_profile(app, false);
                let reasons: Vec<slint::SharedString> = godwit_core::report::ReportReason::USER
                    .iter()
                    .map(|r| r.label().into())
                    .collect();
                ui.set_report_title("Report user".into());
                ui.set_report_reasons(slint::ModelRc::new(slint::VecModel::from(reasons)));
                ui.set_report_reason(3);
                ui.set_report_detail(filler(92, 6).into());
                ui.set_report_open(true);
                Screen::Main
            }),
        ),
        Scene::new(
            "cleanup-thread",
            Box::new(|_, ui, _, _| {
                ui.set_thread_open(true);
                ui.set_thread_name(filler(91, 3).into());
                Screen::Main
            }),
        ),
        Scene::new(
            "cleanup-channel",
            Box::new(|_, ui, _, _| {
                ui.set_creating_channel(true);
                ui.set_new_channel_name("example-channel".into());
                Screen::Main
            }),
        ),
        Scene::new(
            "cleanup-rejoin",
            Box::new(|_, ui, _, _| {
                ui.set_rejoin_open(true);
                ui.set_rejoin_room("Example lounge".into());
                Screen::Main
            }),
        ),
        Scene::new(
            "cleanup-camera",
            Box::new(|app, ui, _, _| {
                let screen = call_with_camera(app);
                ui.set_camera_confirm(true);
                screen
            }),
        ),
        Scene::new(
            "cleanup-quiet",
            Box::new(|_, ui, _, _| {
                ui.set_quiet_notice_open(true);
                ui.set_quiet_notice_account(me().into());
                Screen::Main
            }),
        ),
        Scene::new(
            "cleanup-fullscreen",
            Box::new(|app, ui, _, _| {
                let screen = call_with_camera(app);
                ui.set_fullscreen_video(0);
                screen
            }),
        ),
        Scene::new("switcher", Box::new(|_, _, _, _| Screen::Main)),
        Scene::new("ctrl-k", Box::new(|_, _, _, _| Screen::Main)),
        Scene::new("light-profile", Box::new(|app, _, _, _| open_profile(app, false))),
        Scene::new(
            "light-home",
            Box::new(|app, _, _, dm| {
                app.dispatch(Action::SelectServer(None));
                app.dispatch(Action::SelectRoom(dm.clone()));
                Screen::Main
            }),
        ),
        Scene::new("light-login", Box::new(|_, _, _, _| Screen::Login)),
        Scene::new(
            "light-signin",
            Box::new(|_, ui, _, _| {
                ui.set_login_network(0);
                Screen::Login
            }),
        ),
        Scene::new(
            "uploading",
            Box::new(|app, _, _, _| {
                app.dispatch(Action::AddFiles(vec![Outgoing {
                    path: "/nonexistent/notes.pdf".into(),
                    name: format!("{}.pdf", filler(43, 1).to_lowercase()).into(),
                    size: 640_000,
                }]));
                app.dispatch(Action::SetDraft(filler(44, 4).to_lowercase()));
                app.dispatch(Action::Send {
                    now_ms: 1_793_000_000_000 + 18 * 60_000,
                });
                Screen::Main
            }),
        ),
        Scene::new(
            "unread",
            Box::new(|app, _, general, _| {
                // #general was read up to Bo's "agreed" (08) when opened.
                let mut r = room(
                    "general",
                    "general",
                    RoomKind::Channel {
                        server: ServerId::new(account(), "S"),
                    },
                    true,
                    0,
                );
                r.capabilities.voice = false;
                r.last_read = Some(MessageId("08".into()));
                app.apply(Event::RoomUpdated(r));
                app.dispatch(Action::SelectRoom(RoomId::new(account(), "dm0")));
                app.dispatch(Action::SelectRoom(general.clone()));
                Screen::Main
            }),
        ),
        Scene {
            scroll: Some((660.0, 180.0, 4000.0)),
            size: Some((1280, 400)),
            ..Scene::new("scrolled-up", Box::new(|_, _, _, _| Screen::Main))
        },
        Scene {
            scroll: Some((800.0, 500.0, -3000.0)),
            ..Scene::new(
                "picker-scrolled",
                Box::new(|_, ui, _, _| {
                    ui.set_picker_open(true);
                    Screen::Main
                }),
            )
        },
        Scene::new("jump-old", Box::new(|_, _, _, _| Screen::Main)),
        Scene::new("jump", Box::new(|_, _, _, _| Screen::Main)),
        Scene::new(
            "pin-confirm",
            Box::new(|_, ui, _, _| {
                ui.set_confirm_pin(0);
                ui.set_confirm_pin_value(true);
                Screen::Main
            }),
        ),
        Scene {
            late: true,
            ..Scene::new("channel-late", Box::new(|_, _, _, _| Screen::Main))
        },
        Scene {
            late: true,
            ..Scene::new(
                "home-dms-late",
                Box::new(|app, _, _, dm| {
                    app.dispatch(Action::SelectServer(None));
                    app.dispatch(Action::SelectRoom(dm.clone()));
                    Screen::Main
                }),
            )
        },
    ];

    for scene in scenes {
        if only.as_deref().is_some_and(|p| !scene.name.starts_with(p)) {
            continue;
        }
        let ui = AppWindow::new()?;
        theme::apply(&ui, theme::DEFAULT_ACCENT, 0, !scene.name.starts_with("light"));
        // The one shell callback the scenes check: closing the picker.
        let weak = ui.as_weak();
        ui.on_close_picker(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_picker_open(false);
            }
        });
        let mut images = Images::offline();
        pictures(&mut images);
        godwit::demo::steam_pictures(&mut images);
        godwit::demo::discord_pictures(&mut images);
        godwit::demo::picker_pictures(&mut images);
        let mut presenter = Presenter::new(&ui);
        // One person muted for us only, at 140%, with their sound menu open.
        if scene.name == "call-muted-here" {
            presenter.stream_sound.muted_users.push("u2".to_owned());
            presenter.stream_sound.user_volumes.insert("u2".to_owned(), 1.4);
        }
        // Person 0 is also on Signal (a merged contact), for the suggestion bar.
        let mut contacts = Contacts::default();
        if scene.name == "nudge" || scene.name == "profile" || scene.name.starts_with("people") {
            let a = contacts.ensure(user("u0"), &person(0));
            let b = contacts.ensure(
                UserId::new(AccountId::new(Network::Discord, "me"), "100000000000000001"),
                &first_name(&person(0)),
            );
            contacts.merge(a, b);
        }
        if scene.name.starts_with("people") {
            // Person 1 also on Steam, with Stoat as their chosen network.
            let bo = contacts.ensure(user("u1"), &person(1));
            let m = contacts.ensure(
                UserId::new(AccountId::new(Network::Steam, "me"), "76561198000000001"),
                &person(1),
            );
            contacts.merge(bo, m);
            if let Some(c) = contacts.get_mut(bo) {
                c.preferred = Some(Network::Stoat);
            }
            // Person 0 said "Don't suggest again" once (not in the nudge scene).
            let ann = contacts.ensure(user("u0"), &person(0));
            if let Some(c) = contacts.get_mut(ann) {
                c.no_nudges = true;
            }
        }
        let (mut app, general, dm) = world(contacts);
        let screen = (scene.setup)(&mut app, &ui, &general, &dm);
        if scene.name == "sidebar-banner" {
            let (w, h) = (720u32, 272u32);
            let rgba: Vec<u8> = (0..w * h)
                .flat_map(|i| [((i % w) * 255 / w) as u8, 90, ((i / w) * 255 / h) as u8, 255])
                .collect();
            images.put(
                "https://example.com/banner.png",
                godwit::images::SIDEBAR_BANNER,
                w,
                h,
                &rgba,
            );
        }
        if scene.name == "channel" {
            // A call in progress, to show the call bar.
            let lounge = RoomId::new(account(), "voice");
            app.dispatch(Action::JoinCall(lounge.clone()));
            app.apply(Event::CallConnected { room: lounge.clone() });
            app.apply(Event::CallMembers {
                room: lounge,
                members: vec![call_member("u1", &person(1), true, false)],
            });
        }
        ui.set_screen(screen);
        ui.set_networks(slint::ModelRc::new(slint::VecModel::from(
            godwit::view::ACCOUNT_NETWORKS
                .iter()
                .map(|n| godwit::view::network(*n, matches!(n, Network::Stoat | Network::Steam)))
                .collect::<Vec<_>>(),
        )));
        let (w, h) = scene.size.unwrap_or((W, H));
        ui.show()?;
        window.set_size(PhysicalSize::new(w, h));
        let mut buffer = vec![Rgb8Pixel { r: 0, g: 0, b: 0 }; (w * h) as usize];
        if scene.late {
            // First draw with nothing in it, as the app does before the network answers.
            let empty = Application::new(Contacts::default());
            presenter.show(&ui, empty.state(), true, &mut images);
            slint::platform::update_timers_and_animations();
            window.request_redraw();
            window.draw_if_needed(|renderer| {
                renderer.render(&mut buffer, w as usize);
            });
        }
        if let Some(id) = scene.open_folder {
            presenter.open_folders.insert((account(), id.into()));
        }
        presenter.show(&ui, app.state(), screen == Screen::Main, &mut images);
        // The picker, with its scroll wired as the shell does: rows coming
        // into view get their pictures.
        let picker = std::rc::Rc::new(std::cell::RefCell::new(godwit::picker::Picker::default()));
        ui.set_emoji_rows(slint::ModelRc::from(picker.borrow().model.clone()));
        let images = std::rc::Rc::new(std::cell::RefCell::new(images));
        if scene.name.starts_with("picker") {
            picker.borrow_mut().set(
                godwit::picker::rows(&[], "", godwit::picker::Allowed::All),
                &mut images.borrow_mut(),
            );
            let (picker, images) = (picker.clone(), images.clone());
            ui.on_picker_scrolled(move |first| {
                picker
                    .borrow_mut()
                    .show(usize::try_from(first).unwrap_or(0), &mut images.borrow_mut());
            });
        }
        // What the switcher lists before anything is typed (the shell's search
        // fills this in the app).
        let (rows, _) = godwit::view::switch_results(app.state(), "", &mut images.borrow_mut());
        ui.set_switch_results(slint::ModelRc::new(slint::VecModel::from(rows)));
        if scene.name == "switcher" {
            let (rows, _) = godwit::view::switch_results(app.state(), "e", &mut images.borrow_mut());
            ui.set_switch_results(slint::ModelRc::new(slint::VecModel::from(rows)));
            ui.set_switcher_open(true);
        }
        // Several passes, as a running event loop would make: layout first,
        // then the pointer (hover needs geometry), then queued `changed`
        // handlers settle.
        for pass in 0..8 {
            if pass == 1
                && let Some((x, y)) = scene.hover
            {
                window.dispatch_event(WindowEvent::PointerMoved {
                    position: LogicalPosition::new(x, y),
                });
            }
            // Shortcut scenes: the keys as a keyboard sends them.
            let press = |keys: &[slint::SharedString]| {
                for k in keys {
                    window.dispatch_event(WindowEvent::KeyPressed { text: k.clone() });
                }
                for k in keys.iter().rev() {
                    window.dispatch_event(WindowEvent::KeyReleased { text: k.clone() });
                }
            };
            if pass == 2 && scene.name == "ctrl-k" {
                press(&[slint::platform::Key::Control.into(), "k".into()]);
            }
            // A jump to Ann's long message (as clicking Bo's reply does).
            if pass == 2 && scene.name == "jump" {
                app.dispatch(Action::JumpTo(MessageId("07".into())));
                presenter.show(&ui, app.state(), true, &mut images.borrow_mut());
            }
            // A jump to an old message that isn't loaded: the messages around
            // it arrive as from the network.
            if pass == 2 && scene.name == "jump-old" {
                app.dispatch(Action::JumpTo(MessageId("old2".into())));
                let general = RoomId::new(account(), "general");
                app.apply(Event::HistoryAround {
                    room: general,
                    around: MessageId("old2".into()),
                    messages: vec![
                        msg("old1", "u1", &person(1), -3 * 24 * 60, &filler(21, 6)),
                        msg("old2", "u0", &person(0), -3 * 24 * 60 + 2, &filler(22, 7)),
                        msg("old3", "u1", &person(1), -3 * 24 * 60 + 3, &filler(23, 1)),
                    ],
                });
                presenter.show(&ui, app.state(), true, &mut images.borrow_mut());
            }
            if pass == 2 && scene.escape {
                let text: slint::SharedString = slint::platform::Key::Escape.into();
                window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
                window.dispatch_event(WindowEvent::KeyReleased { text });
            }
            if pass == 2
                && let Some((x, y)) = scene.click
            {
                let position = LogicalPosition::new(x, y);
                let button = slint::platform::PointerEventButton::Left;
                window.dispatch_event(WindowEvent::PointerMoved { position });
                window.dispatch_event(WindowEvent::PointerPressed { position, button });
                window.dispatch_event(WindowEvent::PointerReleased { position, button });
            }
            if pass == 1
                && let Some((x, y)) = scene.right_click
            {
                let position = LogicalPosition::new(x, y);
                let button = slint::platform::PointerEventButton::Right;
                window.dispatch_event(WindowEvent::PointerMoved { position });
                window.dispatch_event(WindowEvent::PointerPressed { position, button });
                window.dispatch_event(WindowEvent::PointerReleased { position, button });
            }
            if pass == 1
                && let Some((x, y, dy)) = scene.scroll
            {
                let position = LogicalPosition::new(x, y);
                window.dispatch_event(WindowEvent::PointerMoved { position });
                window.dispatch_event(WindowEvent::PointerScrolled {
                    position,
                    delta_x: 0.0,
                    delta_y: dy,
                });
            }
            // Real time passes, so Slint timers can run as they would.
            std::thread::sleep(std::time::Duration::from_millis(40));
            slint::platform::update_timers_and_animations();
            window.request_redraw();
            window.draw_if_needed(|renderer| {
                renderer.render(&mut buffer, w as usize);
            });
        }
        let raw: Vec<u8> = buffer.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        let img = image::RgbImage::from_raw(w, h, raw).ok_or("buffer size")?;
        let path = format!("{out}/{}.png", scene.name);
        img.save(&path)?;
        println!("{path}");
        ui.hide()?;
    }
    Ok(())
}

/// A call in the voice channel: three people, one speaking, one muted, and a
/// camera.
/// A call where u2 streams and we watch with u1: the tile's eye shows two viewers.
/// Where the stream tile's eye button lands in the default 1280x800 window
/// (found from a first render).
const CALL_STREAM_EYE: (f32, f32) = (940.0, 219.0);

fn call_with_stream(app: &mut Application) -> Screen {
    let screen = call_with_camera(app);
    let lounge = RoomId::new(account(), "voice");
    let sid = "demo:camera".to_owned();
    app.apply(Event::CallVideos {
        room: lounge.clone(),
        videos: vec![godwit_core::call::CallVideo {
            sid,
            user: user("u2"),
            screen_share: true,
        }],
    });
    app.apply(Event::CallStreams {
        room: lounge,
        streams: vec![godwit_core::call::CallStream {
            user: user("u2"),
            watching: true,
            viewers: Some(vec![user("me"), user("u1")]),
            note: None,
        }],
    });
    screen
}

fn call_with_camera(app: &mut Application) -> Screen {
    let lounge = RoomId::new(account(), "voice");
    app.dispatch(Action::SelectRoom(lounge.clone()));
    app.dispatch(Action::JoinCall(lounge.clone()));
    app.apply(Event::CallConnected { room: lounge.clone() });
    // The muted one has deafened too, to show both icons.
    app.apply(Event::CallMembers {
        room: lounge.clone(),
        members: vec![
            call_member("me", &me(), false, false),
            call_member("u1", &person(1), true, false),
            call_member("u2", &person(2), false, true),
        ],
    });
    let sid = "demo:camera".to_owned();
    let mut rgba = vec![0u8; 320 * 180 * 4];
    for (i, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        pixel.copy_from_slice(&[(i % 320) as u8, (i / 320) as u8, 160, 255]);
    }
    godwit_voice::video::FRAMES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert_with(Default::default)
        .insert(
            sid.clone(),
            godwit_voice::video::Frame {
                size: [320, 180],
                rgba: rgba.into(),
                serial: 1,
            },
        );
    app.apply(Event::CallVideos {
        room: lounge,
        videos: vec![godwit_core::call::CallVideo {
            sid,
            user: user("u1"),
            screen_share: false,
        }],
    });
    Screen::Main
}
