// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors

use super::*;
use crate::room::RoomEncryption;
use crate::server::Category;

fn account(n: Network) -> AccountId {
    AccountId::new(n, "me")
}

fn room(n: Network, raw: &str, kind: RoomKind) -> Room {
    Room {
        id: RoomId::new(account(n), raw),
        name: raw.into(),
        kind,
        icon_url: None,
        capabilities: Capabilities::of(n),
        encryption: RoomEncryption::Unencrypted,
        unread: false,
        mentions: 0,
        last_read: None,
        invited_by: None,
        presence: None,
        status: None,
        member_count: None,
        last_activity: 0,
        parent: None,
    }
}

fn dm(n: Network, raw: &str, with: &str) -> Room {
    room(
        n,
        raw,
        RoomKind::Direct {
            with: UserId::new(account(n), with),
        },
    )
}

fn msg(id: &str) -> Message {
    Message {
        id: MessageId(id.into()),
        author: UserId::new(account(Network::Stoat), "them"),
        author_name: "Them".into(),
        author_avatar: None,
        author_colour: None,
        sent_ms: 0,
        edited: false,
        system: false,
        text: "hi".into(),
        markup: Markup::Rich,
        source: "hi".into(),
        reply_to: None,
        jumbo: Vec::new(),
        attachments: Vec::new(),
        embeds: Vec::new(),
        invites: Vec::new(),
        reactions: Vec::new(),
        pending: false,
        snippet: None,
        pinned: false,
    }
}

#[test]
fn cache_models_round_trip_through_json() {
    let room = dm(Network::Stoat, "room", "friend");
    let message = msg("message");
    let encoded = serde_json::to_vec(&(&room, &message)).unwrap();
    let (restored_room, restored_message): (Room, Message) = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(restored_room, room);
    assert_eq!(restored_message, message);
}

#[test]
fn cached_history_displays_immediately_and_refreshes_when_opened() {
    let room = dm(Network::Stoat, "cached-room", "friend");
    let account = room.id.account.clone();
    let cached = AccountSnapshot {
        account: account.clone(),
        rooms: vec![room.clone()],
        servers: Vec::new(),
        timelines: vec![CachedTimeline {
            room: room.id.clone(),
            messages: vec![msg("cached-message")],
            reached_start: false,
        }],
        profiles: Vec::new(),
    };
    let mut app = Application::new(Contacts::default());
    app.restore_cache(cached);
    ready(&mut app, Network::Stoat, vec![room.clone()]);
    assert_eq!(app.state().timeline(&room.id).unwrap().messages.len(), 1);
    assert!(app.state().timeline(&room.id).unwrap().cached);
    assert_eq!(app.cache_snapshot(&account).timelines.len(), 1);

    let effects = app.dispatch(Action::SelectRoom(room.id.clone()));
    assert!(effects.iter().any(|effect| matches!(effect,
        Effect::Backend { command: Command::LoadHistory { room: id, before: None }, .. } if id == &room.id
    )));
    assert!(app.state().timeline(&room.id).unwrap().loading);
    assert!(!app.state().timeline(&room.id).unwrap().messages.is_empty());
}

#[test]
fn friend_request_uses_the_chosen_account_and_rejects_signed_out_accounts() {
    let mut app = Application::default();
    ready(&mut app, Network::Stoat, vec![]);
    ready(&mut app, Network::Steam, vec![]);
    let chosen = account(Network::Steam);
    let effects = app.dispatch(Action::SendFriendRequest {
        account: chosen.clone(),
        name: "  Friend  ".into(),
    });
    assert_eq!(
        effects,
        vec![Effect::Backend {
            account: chosen.clone(),
            command: Command::SendFriendRequest {
                request: 1,
                name: "Friend".into()
            },
        }]
    );
    assert!(
        app.dispatch(Action::SendFriendRequest {
            account: AccountId::new(Network::Steam, "signed-out"),
            name: "Friend".into(),
        })
        .is_empty()
    );
    assert!(
        app.dispatch(Action::SendFriendRequest {
            account: chosen,
            name: " ".into(),
        })
        .is_empty()
    );
}

#[test]
fn friend_request_feedback_ignores_old_accounts_and_old_requests() {
    let mut app = Application::default();
    ready(&mut app, Network::Steam, vec![]);
    ready(&mut app, Network::Stoat, vec![]);
    app.dispatch(Action::SendFriendRequest {
        account: account(Network::Steam),
        name: "first".into(),
    });
    app.dispatch(Action::SendFriendRequest {
        account: account(Network::Stoat),
        name: "second".into(),
    });
    app.apply(Event::FriendRequestSent {
        account: account(Network::Steam),
        request: 1,
        result: Ok(()),
    });
    assert!(app.state.friend_request_status.is_none());
    app.apply(Event::FriendRequestSent {
        account: account(Network::Stoat),
        request: 2,
        result: Ok(()),
    });
    assert_eq!(app.state.friend_request_status, Some(Ok(())));
    app.dispatch(Action::SendFriendRequest {
        account: account(Network::Steam),
        name: "third".into(),
    });
    app.apply(Event::AccountLost {
        account: account(Network::Steam),
        reason: "".into(),
    });
    let signed_out = app.state.friend_request_status.clone();
    app.apply(Event::FriendRequestSent {
        account: account(Network::Steam),
        request: 3,
        result: Ok(()),
    });
    assert_eq!(app.state.friend_request_status, signed_out);
    assert!(matches!(signed_out, Some(Err(_))));
}

fn ready(app: &mut Application, n: Network, rooms: Vec<Room>) -> Vec<Effect> {
    app.apply(Event::AccountReady {
        account: account(n),
        display_name: "Me".into(),
        username: "me".into(),
        avatar_url: None,
        rooms,
        servers: Vec::new(),
    })
}

#[test]
fn late_steam_personas_refresh_loaded_authors_only_on_their_account() {
    let mut app = Application::default();
    // Results only count for signed-in accounts.
    ready(&mut app, Network::Steam, Vec::new());
    let first = account(Network::Steam);
    let second = AccountId::new(Network::Steam, "another-account");
    let mut rooms = Vec::new();
    for account in [&first, &second] {
        let room = RoomId::new(account.clone(), "dm");
        let mut message = msg("early");
        message.author = UserId::new(account.clone(), "friend");
        app.state.timelines.insert(
            room.clone(),
            Timeline {
                messages: Arc::new(vec![Arc::new(message)]),
                ..Timeline::default()
            },
        );
        rooms.push(room);
    }
    let contact = app
        .state
        .contacts
        .ensure(UserId::new(first.clone(), "friend"), "Fallback");
    app.state.contacts.get_mut(contact).unwrap().nickname = Some("My nickname".into());
    let effects = app.apply(Event::Friends {
        account: first.clone(),
        people: vec![Person {
            id: UserId::new(first, "friend"),
            name: "Arrived Name".into(),
            avatar_url: Some("https://example.com/avatar.jpg".into()),
            online: true,
        }],
    });
    assert_eq!(app.state.contacts.get(contact).unwrap().name, "Arrived Name");
    assert_eq!(
        app.state.contacts.get(contact).unwrap().nickname.as_deref(),
        Some("My nickname")
    );
    assert_eq!(effects, vec![Effect::SaveContacts]);
    assert_eq!(&*app.state.timelines[&rooms[0]].messages[0].author_name, "Arrived Name");
    assert!(app.state.timelines[&rooms[0]].messages[0].author_avatar.is_some());
    assert_eq!(&*app.state.timelines[&rooms[1]].messages[0].author_name, "Them");
    assert!(app.state.timelines[&rooms[1]].messages[0].author_avatar.is_none());
}

#[test]
fn late_room_history_and_send_results_cannot_restore_a_signed_out_account() {
    let mut app = Application::default();
    let room = dm(Network::Steam, "dm", "friend");
    ready(&mut app, Network::Steam, vec![room.clone()]);
    app.dispatch(Action::SelectRoom(room.id.clone()));
    app.apply(Event::AccountLost {
        account: account(Network::Steam),
        reason: "".into(),
    });
    app.apply(Event::RoomUpdated(room.clone()));
    app.apply(page(&room.id, None, &["late"], true));
    app.apply(Event::MessageConfirmed {
        room: room.id,
        nonce: "old".into(),
        message: msg("late"),
    });
    assert!(app.state.rooms.is_empty());
    assert!(app.state.timelines.is_empty());
    assert!(app.state.accounts.is_empty());
}

fn page(room: &RoomId, before: Option<&str>, ids: &[&str], reached_start: bool) -> Event {
    Event::History {
        room: room.clone(),
        before: before.map(|b| MessageId(b.into())),
        messages: ids.iter().map(|i| msg(i)).collect(),
        reached_start,
    }
}

fn ids(t: &Timeline) -> Vec<&str> {
    t.messages.iter().map(|m| &*m.id.0).collect()
}

fn commands(effects: &[Effect]) -> Vec<&Command> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Backend { command, .. } => Some(command),
            Effect::SaveContacts | Effect::WatchStream { .. } => None,
        })
        .collect()
}

/// One Stoat DM, selected, with its first page loaded.
fn open_dm(pages: &[&str]) -> (Application, RoomId) {
    let mut app = Application::default();
    let r = dm(Network::Stoat, "r1", "them");
    let id = r.id.clone();
    ready(&mut app, Network::Stoat, vec![r]);
    app.dispatch(Action::SelectRoom(id.clone()));
    app.apply(page(&id, None, pages, true));
    (app, id)
}

fn send(app: &mut Application, text: &str) -> Arc<str> {
    app.dispatch(Action::SetDraft(text.into()));
    let effects = app.dispatch(Action::Send { now_ms: 1 });
    match commands(&effects)[..] {
        [Command::SendMessage { nonce, .. }] => nonce.clone(),
        _ => panic!("expected a send, got {effects:?}"),
    }
}

#[test]
fn opening_a_room_focuses_it_and_loads_its_newest_page_once() {
    let mut app = Application::default();
    let r = dm(Network::Stoat, "r1", "them");
    let id = r.id.clone();
    ready(&mut app, Network::Stoat, vec![r]);
    let effects = app.dispatch(Action::SelectRoom(id.clone()));
    assert_eq!(
        commands(&effects),
        [
            &Command::Focus { room: Some(id.clone()) },
            &Command::LoadHistory {
                room: id.clone(),
                before: None
            }
        ]
    );
    assert!(app.dispatch(Action::LoadEarlier).is_empty(), "still loading");
    app.apply(page(&id, None, &["m2", "m3"], false));
    let effects = app.dispatch(Action::LoadEarlier);
    assert_eq!(
        commands(&effects),
        [&Command::LoadHistory {
            room: id.clone(),
            before: Some(MessageId("m2".into()))
        }]
    );
    app.apply(page(&id, Some("m2"), &["m1", "m2"], true));
    assert_eq!(
        ids(app.state().timeline(&id).unwrap()),
        ["m1", "m2", "m3"],
        "overlap skipped"
    );
    assert!(app.dispatch(Action::LoadEarlier).is_empty(), "start reached");
}

#[test]
fn opening_an_unread_room_remembers_where_reading_stopped() {
    let mut app = Application::default();
    let mut unread = dm(Network::Stoat, "u", "x");
    unread.unread = true;
    unread.last_read = Some(MessageId("m4".into()));
    let read = dm(Network::Stoat, "r", "y");
    let (iu, ir) = (unread.id.clone(), read.id.clone());
    ready(&mut app, Network::Stoat, vec![unread, read]);
    app.dispatch(Action::SelectRoom(iu));
    assert_eq!(app.state().unread_from, Some(MessageId("m4".into())));
    app.dispatch(Action::SelectRoom(ir));
    assert_eq!(app.state().unread_from, None);
}

#[test]
fn jumping_loads_around_unloaded_messages_and_back_to_the_present() {
    let (mut app, id) = open_dm(&["m5", "m6"]);
    // Loaded: no request, just the jump.
    assert!(app.dispatch(Action::JumpTo(MessageId("m6".into()))).is_empty());
    assert_eq!(app.state().jump, Some((MessageId("m6".into()), 1)));
    // Not loaded: the messages around it, then the jump.
    let effects = app.dispatch(Action::JumpTo(MessageId("m2".into())));
    assert_eq!(
        commands(&effects),
        [&Command::LoadAround {
            room: id.clone(),
            message: MessageId("m2".into())
        }]
    );
    app.apply(Event::HistoryAround {
        room: id.clone(),
        around: MessageId("m2".into()),
        messages: vec![msg("m1"), msg("m2"), msg("m3")],
    });
    let t = app.state().timeline(&id).unwrap();
    assert!(t.detached && !t.reached_start);
    assert_eq!(app.state().jump, Some((MessageId("m2".into()), 2)));
    // Live messages wait while detached; the present brings them back.
    app.apply(Event::MessageAdded {
        room: id.clone(),
        message: msg("m9"),
    });
    assert_eq!(ids(app.state().timeline(&id).unwrap()), ["m1", "m2", "m3"]);
    let effects = app.dispatch(Action::JumpToPresent);
    assert_eq!(
        commands(&effects),
        [&Command::LoadHistory {
            room: id.clone(),
            before: None
        }]
    );
    app.apply(page(&id, None, &["m8", "m9"], false));
    assert!(!app.state().timeline(&id).unwrap().detached);
    assert!(app.dispatch(Action::JumpToPresent).is_empty(), "already there");
}

#[test]
fn a_failed_load_can_be_retried() {
    let mut app = Application::default();
    let r = dm(Network::Stoat, "r1", "them");
    let id = r.id.clone();
    ready(&mut app, Network::Stoat, vec![r]);
    app.dispatch(Action::SelectRoom(id.clone()));
    app.apply(Event::HistoryFailed { room: id.clone() });
    assert!(app.state().timelines[&id].failed, "shown as failed");
    assert_eq!(
        commands(&app.dispatch(Action::LoadEarlier)),
        [&Command::LoadHistory {
            room: id.clone(),
            before: None
        }]
    );
    // Failing again, then opening the room again also retries.
    app.apply(Event::HistoryFailed { room: id.clone() });
    let effects = app.dispatch(Action::SelectRoom(id.clone()));
    assert!(commands(&effects).contains(&&Command::LoadHistory {
        room: id.clone(),
        before: None
    }));
    app.apply(page(&id, None, &["m1"], true));
    assert!(!app.state().timelines[&id].failed, "a page clears it");
}

#[test]
fn sending_shows_a_pending_message_until_confirmed() {
    let (mut app, id) = open_dm(&["m1"]);
    assert!(app.dispatch(Action::Send { now_ms: 1 }).is_empty(), "no draft");
    app.dispatch(Action::SetDraft("   ".into()));
    assert!(app.dispatch(Action::Send { now_ms: 1 }).is_empty(), "blank draft");

    let nonce = send(&mut app, "hello");
    assert!(app.state().drafts.is_empty(), "draft cleared");
    let t = app.state().timeline(&id).unwrap();
    assert!(t.messages.last().unwrap().pending);
    assert_eq!(&*t.messages.last().unwrap().author_name, "Me");

    app.apply(Event::MessageConfirmed {
        room: id.clone(),
        nonce,
        message: msg("m2"),
    });
    let t = app.state().timeline(&id).unwrap();
    assert_eq!(ids(t), ["m1", "m2"]);
    assert!(!t.messages.iter().any(|m| m.pending));
}

#[test]
fn a_live_copy_before_the_confirmation_is_not_shown_twice() {
    let (mut app, id) = open_dm(&["m1"]);
    let nonce = send(&mut app, "hello");
    app.apply(Event::MessageAdded {
        room: id.clone(),
        message: msg("m2"),
    });
    app.apply(Event::MessageAdded {
        room: id.clone(),
        message: msg("m2"),
    });
    app.apply(Event::MessageConfirmed {
        room: id.clone(),
        nonce,
        message: msg("m2"),
    });
    assert_eq!(ids(app.state().timeline(&id).unwrap()), ["m1", "m2"]);
}

#[test]
fn a_failed_send_is_removed_and_reported() {
    let (mut app, id) = open_dm(&[]);
    let nonce = send(&mut app, "hello");
    app.apply(Event::MessageFailed {
        room: id.clone(),
        nonce,
        error: "slowmode".into(),
    });
    assert!(app.state().timeline(&id).unwrap().messages.is_empty());
    assert_eq!(app.state().errors.len(), 1);
    app.dispatch(Action::ClearErrors);
    assert!(app.state().errors.is_empty());
}

#[test]
fn late_results_after_sign_out_do_not_bring_its_state_back() {
    let (mut app, id) = open_dm(&["m1"]);
    let me = account(Network::Stoat);
    app.apply(Event::AccountLost {
        account: me.clone(),
        reason: "".into(),
    });
    let them = Person {
        id: UserId::new(me.clone(), "them"),
        name: "Them".into(),
        avatar_url: None,
        online: true,
    };
    app.apply(Event::Friends {
        account: me.clone(),
        people: vec![them.clone()],
    });
    app.apply(Event::FriendRequests {
        account: me.clone(),
        incoming: vec![them],
        outgoing: Vec::new(),
    });
    app.apply(Event::Typing {
        room: id.clone(),
        names: vec!["Them".into()],
    });
    app.apply(Event::HistoryAround {
        room: id.clone(),
        around: MessageId("m1".into()),
        messages: vec![msg("m1")],
    });
    let state = app.state();
    assert!(state.friends.is_empty());
    assert!(state.friend_requests.is_empty());
    assert!(state.typing.is_empty());
    assert!(!state.timelines.contains_key(&id));
}

#[test]
fn a_late_own_steam_persona_renames_our_loaded_lines_only_on_that_account() {
    let mut app = Application::default();
    let me = account(Network::Steam);
    ready(&mut app, Network::Steam, vec![dm(Network::Steam, "dm", "friend")]);
    let room = RoomId::new(me.clone(), "dm");
    let mut ours = msg("ours");
    ours.author = UserId::new(me.clone(), me.user.clone());
    ours.author_name = "Steam user 1".into();
    let mut theirs = msg("theirs");
    theirs.author = UserId::new(me.clone(), "friend");
    theirs.author_name = "Friend".into();
    app.state.timelines.insert(
        room.clone(),
        Timeline {
            messages: Arc::new(vec![Arc::new(ours), Arc::new(theirs)]),
            ..Timeline::default()
        },
    );
    app.apply(Event::AccountIdentity {
        account: me,
        display_name: "Real Name".into(),
        username: "me".into(),
        avatar_url: Some("https://example.com/me.jpg".into()),
    });
    let names: Vec<&str> = app.state.timelines[&room]
        .messages
        .iter()
        .map(|m| &*m.author_name)
        .collect();
    assert_eq!(names, ["Real Name", "Friend"]);
}

#[test]
fn linking_from_a_profile_releases_focus_like_the_people_button() {
    let (mut app, id) = open_dm(&["m1"]);
    let them = UserId::new(id.account.clone(), "them");
    let effects = app.dispatch(Action::LinkFrom(them));
    assert!(app.state().showing_people);
    assert!(commands(&effects).contains(&&Command::Focus { room: None }));
}

#[test]
fn a_failed_send_gives_its_text_back_unless_something_new_was_typed() {
    let (mut app, id) = open_dm(&[]);
    let nonce = send(&mut app, "hello");
    app.apply(Event::MessageFailed {
        room: id.clone(),
        nonce,
        error: "Not connected to Steam.".into(),
    });
    assert_eq!(app.state().drafts.get(&id).map(String::as_str), Some("hello"));

    let nonce = send(&mut app, "first");
    app.dispatch(Action::SetDraft("second".into()));
    app.apply(Event::MessageFailed {
        room: id.clone(),
        nonce,
        error: "Not connected to Steam.".into(),
    });
    assert_eq!(app.state().drafts.get(&id).map(String::as_str), Some("second"));
}

#[test]
fn edits_and_deletions_apply_in_place() {
    let (mut app, id) = open_dm(&["m1", "m2"]);
    let mut edited = msg("m1");
    edited.text = "changed".into();
    edited.edited = true;
    app.apply(Event::MessageUpdated {
        room: id.clone(),
        message: edited,
    });
    app.apply(Event::MessageDeleted {
        room: id.clone(),
        id: MessageId("m2".into()),
    });
    let t = app.state().timeline(&id).unwrap();
    assert_eq!(ids(t), ["m1"]);
    assert_eq!(&*t.messages[0].text, "changed");
}

#[test]
fn a_newest_page_replaces_the_timeline_but_keeps_pending_sends() {
    let (mut app, id) = open_dm(&["m1", "m2"]);
    send(&mut app, "hello");
    app.apply(page(&id, None, &["m5", "m6"], false));
    let t = app.state().timeline(&id).unwrap();
    assert_eq!(ids(t)[..2], ["m5", "m6"]);
    assert!(t.messages[2].pending);
}

#[test]
fn our_live_copy_takes_the_pending_rows_place() {
    let (mut app, id) = open_dm(&["m1"]);
    let nonce = send(&mut app, "hello");
    let mine = Message {
        author: UserId::new(id.account.clone(), id.account.user.clone()),
        source: "hello".into(),
        text: "hello".into(),
        ..msg("m2")
    };
    app.apply(Event::MessageAdded {
        room: id.clone(),
        message: mine.clone(),
    });
    let t = app.state().timeline(&id).unwrap();
    assert_eq!(ids(t), ["m1", "m2"], "shown once, where the pending row was");
    assert!(!t.messages[1].pending);
    app.apply(Event::MessageConfirmed {
        room: id.clone(),
        nonce,
        message: mine,
    });
    assert_eq!(ids(app.state().timeline(&id).unwrap()), ["m1", "m2"]);
}

#[test]
fn live_messages_in_the_open_room_mark_it_read() {
    let (mut app, id) = open_dm(&[]);
    let effects = app.apply(Event::MessageAdded {
        room: id.clone(),
        message: msg("m9"),
    });
    assert_eq!(
        commands(&effects),
        [&Command::MarkRead {
            room: id.clone(),
            last: MessageId("m9".into())
        }]
    );
    assert_eq!(ids(app.state().timeline(&id).unwrap()), ["m9"]);
}

#[test]
fn an_unread_room_is_read_once_its_history_arrives() {
    let mut app = Application::default();
    let mut room = dm(Network::Steam, "dm", "friend");
    // A mention count alone (no unread flag) counts as unread too.
    room.mentions = 2;
    ready(&mut app, Network::Steam, vec![room.clone()]);
    app.dispatch(Action::SelectRoom(room.id.clone()));
    let effects = app.apply(page(&room.id, None, &["m1", "m2"], true));
    assert_eq!(
        commands(&effects),
        [&Command::MarkRead {
            room: room.id.clone(),
            last: MessageId("m2".into())
        }]
    );
    // An older page doesn't read again.
    assert!(commands(&app.apply(page(&room.id, Some("m1"), &["m0"], true))).is_empty());
}

#[test]
fn a_reconnect_reloads_the_open_room() {
    let (mut app, id) = open_dm(&["m1"]);
    let effects = ready(&mut app, Network::Stoat, vec![dm(Network::Stoat, "r1", "them")]);
    assert_eq!(
        commands(&effects),
        [
            &Command::Focus { room: Some(id.clone()) },
            &Command::LoadHistory {
                room: id.clone(),
                before: None
            }
        ]
    );
    assert!(app.state().timeline(&id).unwrap().loading);
}

#[test]
fn switching_to_another_accounts_room_unfocuses_the_first() {
    let mut app = Application::default();
    let s = dm(Network::Stoat, "s", "x");
    let m = dm(Network::Discord, "!m", "@x:y");
    let (is, im) = (s.id.clone(), m.id.clone());
    ready(&mut app, Network::Stoat, vec![s]);
    ready(&mut app, Network::Discord, vec![m]);
    app.dispatch(Action::SelectRoom(is));
    let effects = app.dispatch(Action::SelectRoom(im));
    assert_eq!(
        effects[0],
        Effect::Backend {
            account: account(Network::Stoat),
            command: Command::Focus { room: None }
        }
    );
}

#[test]
fn selecting_a_channel_selects_its_server() {
    let mut app = Application::default();
    let server = ServerId::new(account(Network::Stoat), "S");
    let ch = room(Network::Stoat, "c1", RoomKind::Channel { server: server.clone() });
    let cid = ch.id.clone();
    let s = Server {
        id: server.clone(),
        name: "S".into(),
        icon_url: None,
        banner_url: None,
        categories: vec![Category {
            title: "".into(),
            rooms: vec![cid.clone()],
        }],
        muted: false,
        rights: Default::default(),
    };
    app.apply(Event::AccountReady {
        account: account(Network::Stoat),
        display_name: "Me".into(),
        username: "me".into(),
        avatar_url: None,
        rooms: vec![ch, dm(Network::Stoat, "d", "x")],
        servers: vec![s],
    });
    assert_eq!(app.state().direct_rooms().count(), 1);
    app.dispatch(Action::SelectRoom(cid));
    assert_eq!(app.state().selected_server, Some(server.clone()));
    app.dispatch(Action::SelectServer(Some(ServerId::new(
        account(Network::Stoat),
        "nope",
    ))));
    assert_eq!(app.state().selected_server, Some(server), "unknown servers are ignored");
    app.apply(Event::ServersChanged {
        account: account(Network::Stoat),
        servers: Vec::new(),
    });
    assert_eq!(app.state().selected_server, None, "a server that's gone is deselected");
}

#[test]
fn drafts_are_kept_per_room() {
    let mut app = Application::default();
    let a = dm(Network::Stoat, "a", "x");
    let b = dm(Network::Stoat, "b", "y");
    let (ia, ib) = (a.id.clone(), b.id.clone());
    ready(&mut app, Network::Stoat, vec![a, b]);
    app.dispatch(Action::SelectRoom(ia.clone()));
    app.dispatch(Action::SetDraft("for a".into()));
    app.dispatch(Action::SelectRoom(ib.clone()));
    app.dispatch(Action::SetDraft("for b".into()));
    assert_eq!(app.state().drafts[&ia], "for a");
    assert_eq!(app.state().drafts[&ib], "for b");
}

#[test]
fn a_dm_with_someone_on_a_better_network_suggests_it_once() {
    let mut contacts = Contacts::default();
    let a = contacts.ensure(UserId::new(account(Network::Discord), "alex"), "Alex");
    let b = contacts.ensure(UserId::new(account(Network::Stoat), "alex-s"), "Alex");
    assert!(contacts.merge(a, b));

    let mut app = Application::new(contacts);
    let r = dm(Network::Discord, "dm", "alex");
    let id = r.id.clone();
    ready(&mut app, Network::Discord, vec![r]);
    app.dispatch(Action::SelectRoom(id.clone()));
    assert_eq!(
        app.state().nudge,
        Some(Nudge {
            contact: a,
            to: Network::Stoat
        })
    );

    assert_eq!(
        app.dispatch(Action::DismissNudge { never: false }),
        vec![Effect::SaveContacts]
    );
    assert_eq!(app.state().nudge, None);
    app.dispatch(Action::SelectRoom(id));
    assert_eq!(app.state().nudge, None, "shown once only");
}

#[test]
fn splitting_a_merge_gives_each_side_its_own_name_not_an_id() {
    let mut app = Application::default();
    let mut stoat = dm(Network::Stoat, "r1", "alice-id");
    stoat.name = "Alice".into();
    let mut steam = dm(Network::Steam, "r2", "76561190000000002");
    steam.name = "alice_on_steam".into();
    ready(&mut app, Network::Stoat, vec![stoat]);
    ready(&mut app, Network::Steam, vec![steam]);
    let a = UserId::new(account(Network::Stoat), "alice-id");
    let b = UserId::new(account(Network::Steam), "76561190000000002");
    let ca = app.state.contacts.ensure(a.clone(), "Alice");
    let cb = app.state.contacts.ensure(b.clone(), "alice_on_steam");
    app.dispatch(Action::MergeContacts { into: ca, from: cb });
    assert_eq!(app.state().contacts.get(ca).map(|c| c.identities.len()), Some(2));
    // Split the one the merged name came from: it keeps "Alice", the
    // rest is called by its own name again (no ids anywhere).
    app.dispatch(Action::SplitIdentity(a.clone()));
    let name_of = |u: &UserId| app.state().contacts.of(u).map(|c| c.name.clone());
    assert_eq!(name_of(&a).as_deref(), Some("Alice"));
    assert_eq!(name_of(&b).as_deref(), Some("alice_on_steam"));
}

#[test]
fn losing_an_account_drops_its_rooms_and_selection() {
    let mut app = Application::default();
    let r = dm(Network::Stoat, "r1", "them");
    let id = r.id.clone();
    ready(&mut app, Network::Stoat, vec![r]);
    ready(&mut app, Network::Discord, vec![dm(Network::Discord, "!m", "@x:y")]);
    app.dispatch(Action::SelectRoom(id));
    let friend = |net| Person {
        id: UserId::new(account(net), "friend"),
        name: "Friend".into(),
        avatar_url: None,
        online: true,
    };
    app.apply(Event::Friends {
        account: account(Network::Stoat),
        people: vec![friend(Network::Stoat)],
    });
    app.apply(Event::Friends {
        account: account(Network::Discord),
        people: vec![friend(Network::Discord)],
    });
    app.apply(Event::FriendRequests {
        account: account(Network::Stoat),
        incoming: vec![friend(Network::Stoat)],
        outgoing: Vec::new(),
    });
    app.apply(Event::AccountLost {
        account: account(Network::Stoat),
        reason: "logged out".into(),
    });
    assert_eq!(app.state().rooms.len(), 1);
    assert_eq!(app.state().selected, None);
    assert_eq!(app.state().accounts.len(), 1);
    // Its friends and requests went with it; the other account's stay.
    assert_eq!(app.state().friends.len(), 1);
    assert_eq!(app.state().friends[0].id.account, account(Network::Discord));
    assert!(app.state().friend_requests.is_empty());
}

#[test]
fn only_recent_rooms_keep_their_messages() {
    let mut app = Application::default();
    let rooms: Vec<Room> = (0..10)
        .map(|i| dm(Network::Stoat, &format!("r{i}"), &format!("u{i}")))
        .collect();
    let ids: Vec<RoomId> = rooms.iter().map(|r| r.id.clone()).collect();
    ready(&mut app, Network::Stoat, rooms);
    for id in &ids {
        app.dispatch(Action::SelectRoom(id.clone()));
        app.apply(page(id, None, &["m"], true));
    }
    assert_eq!(app.state().timelines.len(), KEEP_TIMELINES);
    assert!(!app.state().timelines.contains_key(&ids[0]), "the oldest went");
    assert!(app.state().timelines.contains_key(&ids[9]));
    // Opening a forgotten room loads it again.
    let effects = app.dispatch(Action::SelectRoom(ids[0].clone()));
    assert!(commands(&effects).contains(&&Command::LoadHistory {
        room: ids[0].clone(),
        before: None
    }));
}

#[test]
fn our_presence_shows_at_once_and_only_where_there_is_presence() {
    let mut app = Application::default();
    ready(&mut app, Network::Stoat, Vec::new());
    let me = account(Network::Stoat);
    assert!(
        app.dispatch(Action::SetPresence {
            account: me.clone(),
            presence: Presence::Idle
        })
        .is_empty(),
        "not known yet"
    );
    app.apply(Event::OwnPresence {
        account: me.clone(),
        presence: Presence::Online,
        status: None,
    });
    let effects = app.dispatch(Action::SetPresence {
        account: me.clone(),
        presence: Presence::Invisible,
    });
    assert_eq!(commands(&effects), [&Command::SetPresence(Presence::Invisible)]);
    assert_eq!(app.state().accounts[0].presence, Some(Presence::Invisible));
    ready(&mut app, Network::Stoat, Vec::new());
    assert_eq!(
        app.state().accounts[0].presence,
        Some(Presence::Invisible),
        "kept on a reconnect"
    );
}

#[test]
fn reconnecting_keeps_the_account_order_and_what_is_known() {
    let mut app = Application::default();
    ready(&mut app, Network::Stoat, Vec::new());
    ready(&mut app, Network::Discord, Vec::new());
    app.apply(Event::DeviceTrust {
        account: account(Network::Discord),
        unverified: true,
    });
    app.apply(Event::Connection {
        account: account(Network::Discord),
        online: false,
    });
    assert!(app.state().accounts[1].offline);
    ready(&mut app, Network::Stoat, Vec::new());
    ready(&mut app, Network::Discord, Vec::new());
    let order: Vec<Network> = app.state().accounts.iter().map(|a| a.id.network).collect();
    assert_eq!(order, [Network::Stoat, Network::Discord]);
    assert!(app.state().accounts[1].unverified, "still needs its key");
    assert!(!app.state().accounts[1].offline, "connected again");
}

#[test]
fn logging_out_yourself_is_no_error_but_losing_an_account_is() {
    let mut app = Application::default();
    ready(&mut app, Network::Discord, Vec::new());
    app.apply(Event::AccountLost {
        account: account(Network::Discord),
        reason: "".into(),
    });
    assert!(app.state().errors.is_empty() && app.state().accounts.is_empty());
    ready(&mut app, Network::Discord, Vec::new());
    app.apply(Event::AccountLost {
        account: account(Network::Discord),
        reason: "Your sign-in ended".into(),
    });
    assert_eq!(app.state().errors.len(), 1);
}

#[test]
fn presence_lights_up_dms_people_and_member_lists() {
    let mut app = Application::default();
    ready(&mut app, Network::Slack, vec![dm(Network::Slack, "T:D1", "T:ann")]);
    let account = account(Network::Slack);
    let ann = UserId::new(account.clone(), "T:ann");
    app.apply(Event::Presences {
        account,
        presence: vec![(ann.clone(), Presence::Online)],
    });
    let state = app.state();
    assert!(
        state.friends.iter().any(|p| p.id == ann && p.online),
        "People shows them online"
    );
    let room = state.rooms.iter().find(|r| &*r.id.raw == "T:D1").unwrap();
    assert_eq!(room.presence, Some(Presence::Online), "their DM's dot");
}

#[test]
fn workspace_threads_navigation_refreshes_the_server_index_and_releases_chat_focus() {
    let mut app = Application::default();
    let server = ServerId::new(account(Network::Slack), "T");
    let room = room(Network::Slack, "T:C", RoomKind::Channel { server: server.clone() });
    let id = room.id.clone();
    ready(&mut app, Network::Slack, vec![room]);
    app.dispatch(Action::SelectRoom(id.clone()));
    let effects = app.dispatch(Action::ShowThreads);
    assert!(app.state().showing_threads);
    assert!(app.state().selected.is_none());
    assert_eq!(app.state().selected_server.as_ref(), Some(&server));
    assert!(
        commands(&effects)
            .iter()
            .any(|c| matches!(c,Command::LoadThreads { server:s, more: false } if s == &server))
    );
    assert!(
        commands(&effects)
            .iter()
            .any(|c| matches!(c, Command::Focus { room: None }))
    );
    app.dispatch(Action::SelectRoom(id));
    assert!(!app.state().showing_threads);
    app.dispatch(Action::ShowThreads);
    app.dispatch(Action::ShowFriends);
    assert!(!app.state().showing_threads);
}

#[test]
fn refreshing_room_metadata_preserves_live_presence() {
    let mut app = Application::default();
    let room = dm(Network::Teams, "chat", "ann");
    let ann = UserId::new(account(Network::Teams), "ann");
    ready(&mut app, Network::Teams, vec![room.clone()]);
    app.apply(Event::Presences {
        account: account(Network::Teams),
        presence: vec![(ann, Presence::Online)],
    });
    app.apply(Event::RoomUpdated(room.clone()));
    assert_eq!(app.state().room(&room.id).unwrap().presence, Some(Presence::Online));
}

#[test]
fn a_directory_adds_people_and_cards_open_with_the_right_actions() {
    let mut app = Application::default();
    ready(&mut app, Network::Slack, vec![dm(Network::Slack, "T:D1", "T:ann")]);
    let account = account(Network::Slack);
    let person = |raw: &str, name: &str| Person {
        id: UserId::new(account.clone(), raw),
        name: name.into(),
        avatar_url: None,
        online: false,
    };
    app.apply(Event::PeopleDirectory {
        account: account.clone(),
        people: vec![person("T:ann", "Ann"), person("T:zed", "Zed")],
    });
    let ids: Vec<&str> = app.state().friends.iter().map(|p| &*p.id.raw).collect();
    assert_eq!(ids.len(), 2, "the DM's person and the directory's, once each");
    assert!(ids.contains(&"T:zed"));
    // A card opens at once, without friend requests on Slack.
    app.dispatch(Action::OpenProfile(UserId::new(account, "T:zed")));
    let Some((_, Some(card))) = &app.state().profile else {
        panic!("no card")
    };
    assert_eq!(&*card.name, "Zed");
    assert!(!card.has_friends);
}

#[test]
fn workplace_chats_list_their_dm_people_and_use_confirmed_presence() {
    let mut app = Application::default();
    ready(
        &mut app,
        Network::GoogleChat,
        vec![
            dm(Network::GoogleChat, "dm:1", "ann"),
            dm(Network::GoogleChat, "dm:2", "bob"),
        ],
    );
    let people: Vec<&str> = app.state().friends.iter().map(|p| &*p.id.raw).collect();
    assert_eq!(people.len(), 2);
    assert!(people.contains(&"ann") && people.contains(&"bob"));
    assert_eq!(
        app.state().accounts[0].presence,
        None,
        "connection alone is not availability"
    );
    app.apply(Event::OwnPresence {
        account: account(Network::GoogleChat),
        presence: Presence::Online,
        status: None,
    });
    assert_eq!(app.state().accounts[0].presence, Some(Presence::Online));
    // Networks with their own friends list keep it.
    ready(&mut app, Network::Discord, vec![dm(Network::Discord, "d1", "carl")]);
    assert!(!app.state().friends.iter().any(|p| &*p.id.raw == "carl"));
    assert_eq!(app.state().accounts[1].presence, None);
}

#[test]
fn a_resync_replaces_only_that_accounts_rooms() {
    let mut app = Application::default();
    ready(
        &mut app,
        Network::Stoat,
        vec![dm(Network::Stoat, "a", "x"), dm(Network::Stoat, "b", "y")],
    );
    ready(&mut app, Network::Discord, vec![dm(Network::Discord, "!m", "@x:y")]);
    ready(&mut app, Network::Stoat, vec![dm(Network::Stoat, "c", "z")]);
    let raw: Vec<&str> = app.state().rooms.iter().map(|r| &*r.id.raw).collect();
    assert_eq!(raw, ["!m", "c"]);
    assert_eq!(app.state().accounts.len(), 2);
}

fn mine(id: &str) -> Message {
    let mut m = msg(id);
    m.author = UserId::new(account(Network::Stoat), "me");
    m.source = "**raw** text".into();
    m
}

#[test]
fn unlocking_with_a_key_goes_to_that_account_and_never_shows_the_key() {
    let mut app = Application::default();
    ready(&mut app, Network::Discord, Vec::new());
    app.apply(Event::DeviceTrust {
        account: account(Network::Discord),
        unverified: true,
    });
    assert!(app.state().accounts[0].unverified);
    let key = Secret("EsT0 abcd efgh".into());
    let effects = app.dispatch(Action::UnlockWithKey {
        account: account(Network::Discord),
        key: key.clone(),
    });
    assert_eq!(commands(&effects), [&Command::UnlockWithKey { key }]);
    assert!(!format!("{effects:?}").contains("abcd"), "redacted in debug output");
    assert!(
        app.dispatch(Action::UnlockWithKey {
            account: account(Network::Discord),
            key: Secret("  ".into())
        })
        .is_empty()
    );
    assert!(
        app.dispatch(Action::UnlockWithKey {
            account: account(Network::Stoat),
            key: Secret("k".into())
        })
        .is_empty()
    );
    app.apply(Event::DeviceTrust {
        account: account(Network::Discord),
        unverified: false,
    });
    assert!(!app.state().accounts[0].unverified);
}

#[test]
fn verifying_with_another_device_compares_then_finishes() {
    let mut app = Application::default();
    let me = account(Network::Discord);
    ready(&mut app, Network::Discord, Vec::new());
    let effects = app.dispatch(Action::StartVerification(me.clone()));
    assert_eq!(commands(&effects), [&Command::StartVerification]);
    assert!(
        app.dispatch(Action::AnswerVerification { matches: true }).is_empty(),
        "nothing to compare yet"
    );
    let emoji = vec![("🐶".into(), "Dog".into())];
    app.apply(Event::Verification {
        account: me.clone(),
        step: Verify::Compare(emoji),
    });
    let effects = app.dispatch(Action::AnswerVerification { matches: true });
    assert_eq!(commands(&effects), [&Command::AnswerVerification { matches: true }]);
    assert_eq!(app.state().verification, Some((me.clone(), Verify::Confirmed)));
    app.apply(Event::Verification {
        account: me.clone(),
        step: Verify::Done,
    });
    assert!(
        app.dispatch(Action::CloseVerification).is_empty(),
        "nothing to cancel once done"
    );
    assert_eq!(app.state().verification, None);
    // Closing midway cancels it.
    app.dispatch(Action::StartVerification(me.clone()));
    assert_eq!(
        commands(&app.dispatch(Action::CloseVerification)),
        [&Command::CancelVerification]
    );
    // News about a closed one is ignored.
    app.apply(Event::Verification {
        account: me,
        step: Verify::Done,
    });
    assert_eq!(app.state().verification, None);
}

#[test]
fn another_device_asking_can_be_accepted_or_declined() {
    let mut app = Application::default();
    let me = account(Network::Discord);
    ready(&mut app, Network::Discord, Vec::new());
    app.apply(Event::Verification {
        account: me.clone(),
        step: Verify::Incoming,
    });
    assert_eq!(app.state().verification, Some((me.clone(), Verify::Incoming)));
    assert_eq!(
        commands(&app.dispatch(Action::AcceptVerification)),
        [&Command::AcceptVerification]
    );
    assert_eq!(app.state().verification, Some((me.clone(), Verify::Waiting)));
    // A second request while one goes on is left alone.
    app.apply(Event::Verification {
        account: me.clone(),
        step: Verify::Incoming,
    });
    assert_eq!(
        app.state().verification,
        Some((me.clone(), Verify::Incoming)),
        "it's the same flow, updated"
    );
    assert_eq!(
        commands(&app.dispatch(Action::CloseVerification)),
        [&Command::CancelVerification],
        "declined"
    );
    assert_eq!(app.state().verification, None);
}

#[test]
fn new_keys_reload_the_open_room_and_drop_the_others() {
    let (mut app, open) = open_dm(&["m1"]);
    let other = RoomId::new(account(Network::Stoat), "elsewhere");
    app.apply(page(&other, None, &["x"], true));
    let effects = app.apply(Event::KeysArrived {
        account: account(Network::Stoat),
    });
    assert_eq!(
        commands(&effects),
        [&Command::LoadHistory {
            room: open.clone(),
            before: None
        }]
    );
    assert!(
        app.state().timelines.contains_key(&open),
        "shown until the new page comes"
    );
    assert!(!app.state().timelines.contains_key(&other));
}

#[test]
fn picking_a_server_opens_its_last_or_first_text_channel_and_home_its_last_dm() {
    let mut app = Application::default();
    let n = Network::Stoat;
    let server = ServerId::new(account(n), "S");
    let channel = |raw: &str, voice: bool| {
        let mut r = room(n, raw, RoomKind::Channel { server: server.clone() });
        r.capabilities.voice = voice;
        r
    };
    ready(
        &mut app,
        n,
        vec![
            channel("talk", true),
            channel("general", false),
            channel("memes", false),
            dm(n, "d", "them"),
        ],
    );
    app.apply(Event::ServersChanged {
        account: account(n),
        servers: vec![Server {
            id: server.clone(),
            name: "S".into(),
            icon_url: None,
            banner_url: None,
            categories: vec![Category {
                title: "".into(),
                rooms: ["talk", "general", "memes"]
                    .map(|r| RoomId::new(account(n), r))
                    .to_vec(),
            }],
            muted: false,
            rights: Default::default(),
        }],
    });
    let id = |raw: &str| Some(RoomId::new(account(n), raw));
    app.dispatch(Action::SelectRoom(RoomId::new(account(n), "d")));
    app.dispatch(Action::SelectServer(Some(server.clone())));
    assert_eq!(
        app.state().selected,
        id("general"),
        "the first text channel, not the DM"
    );
    app.dispatch(Action::SelectRoom(RoomId::new(account(n), "memes")));
    app.dispatch(Action::SelectServer(None));
    assert_eq!(app.state().selected, id("d"), "Home reopens the last DM");
    app.dispatch(Action::SelectServer(Some(server)));
    assert_eq!(app.state().selected, id("memes"), "the channel last open there");
}

#[test]
fn gif_picker_loads_categories_once_pages_searches_and_drops_stale_replies() {
    use crate::gifs::Gif;
    let mut app = Application::default();
    let n = Network::Stoat;
    ready(&mut app, n, vec![dm(n, "d", "them")]);
    app.dispatch(Action::SelectRoom(RoomId::new(account(n), "d")));
    assert_eq!(commands(&app.dispatch(Action::OpenGifs)), [&Command::LoadGifCategories]);
    assert!(app.dispatch(Action::OpenGifs).is_empty(), "once per session");
    let search = |q: &str, more| Action::SearchGifs { query: q.into(), more };
    assert_eq!(
        commands(&app.dispatch(search(" Wave ", false))),
        [&Command::SearchGifs {
            query: "wave".into(),
            position: None
        }]
    );
    assert!(app.dispatch(search("wave", true)).is_empty(), "no page while one loads");
    app.dispatch(search("cat", false));
    let gif = |u: &str| Gif {
        url: u.into(),
        preview: u.into(),
        width: 1,
        height: 1,
    };
    let page = |q: &str, more, next: Option<&str>| Event::Gifs {
        account: account(n),
        query: q.into(),
        more,
        results: vec![gif(q)],
        next: next.map(Into::into),
    };
    app.apply(page("wave", false, Some("x")));
    let picker = &app.state().gifs[&account(n)];
    assert!(
        picker.results.is_empty() && picker.loading,
        "an older search's reply is dropped"
    );
    app.apply(page("cat", false, Some("p2")));
    assert_eq!(
        commands(&app.dispatch(search("cat", true))),
        [&Command::SearchGifs {
            query: "cat".into(),
            position: Some("p2".into())
        }]
    );
    app.apply(page("cat", true, None));
    let picker = &app.state().gifs[&account(n)];
    assert_eq!(picker.results.len(), 2);
    assert!(app.dispatch(search("cat", true)).is_empty(), "the last page");
    assert!(
        app.dispatch(search("", false)).is_empty(),
        "the categories need no request"
    );
}

#[test]
fn sending_a_gif_sends_its_link_and_keeps_the_draft() {
    let mut app = Application::default();
    let n = Network::Stoat;
    ready(&mut app, n, vec![dm(n, "d", "them")]);
    let room = RoomId::new(account(n), "d");
    app.dispatch(Action::SelectRoom(room.clone()));
    app.dispatch(Action::SetDraft("half a thought".into()));
    let effects = app.dispatch(Action::SendGif {
        url: "https://tenor.com/view/x".into(),
        now_ms: 0,
    });
    assert!(
        commands(&effects)
            .iter()
            .any(|c| matches!(c, Command::SendMessage { text, .. } if text == "https://tenor.com/view/x"))
    );
    assert_eq!(
        app.state().drafts.get(&room).map(String::as_str),
        Some("half a thought")
    );
}

#[test]
fn home_without_a_previous_dm_shows_friends() {
    let mut app = Application::default();
    ready(&mut app, Network::Stoat, vec![dm(Network::Stoat, "d", "them")]);
    app.dispatch(Action::SelectServer(None));
    assert!(app.state().showing_friends);
}

#[test]
fn a_servers_directory_loads_once_and_rooms_can_be_joined() {
    let mut app = Application::default();
    ready(&mut app, Network::Discord, Vec::new());
    let server = Server {
        id: ServerId::new(account(Network::Discord), "!space"),
        name: "Space".into(),
        icon_url: None,
        banner_url: None,
        categories: Vec::new(),
        muted: false,
        rights: Default::default(),
    };
    app.apply(Event::ServersChanged {
        account: account(Network::Discord),
        servers: vec![server.clone()],
    });
    let effects = app.dispatch(Action::SelectServer(Some(server.id.clone())));
    assert_eq!(
        commands(&effects),
        [&Command::LoadDirectory {
            server: server.id.clone()
        }]
    );
    assert!(
        app.dispatch(Action::SelectServer(Some(server.id.clone()))).is_empty(),
        "once per session"
    );
    let open = RoomId::new(account(Network::Discord), "!open");
    app.apply(Event::Directory {
        server: server.id.clone(),
        rooms: vec![DirectoryRoom {
            id: open.clone(),
            name: "open".into(),
            topic: None,
            members: 3,
        }],
    });
    assert_eq!(app.state().directory[&server.id].len(), 1);
    assert_eq!(
        commands(&app.dispatch(Action::JoinRoom(open.clone()))),
        [&Command::JoinRoom { room: open }]
    );
}

#[test]
fn status_text_is_trimmed_to_the_limit_and_blank_clears_it() {
    let mut app = Application::default();
    let me = account(Network::Stoat);
    app.apply(Event::AccountReady {
        account: me.clone(),
        display_name: "Me".into(),
        username: "me#1".into(),
        avatar_url: None,
        rooms: Vec::new(),
        servers: Vec::new(),
    });
    app.apply(Event::OwnPresence {
        account: me.clone(),
        presence: Presence::Online,
        status: None,
    });
    let long = format!("  {}  ", "a".repeat(200));
    let effects = app.dispatch(Action::SetStatusText {
        account: me.clone(),
        text: Some(long),
    });
    assert!(matches!(commands(&effects)[..], [Command::SetStatusText(Some(t))] if t.chars().count() == 128));
    let effects = app.dispatch(Action::SetStatusText {
        account: me.clone(),
        text: Some("   ".into()),
    });
    assert_eq!(commands(&effects), [&Command::SetStatusText(None)]);
    assert_eq!(app.state().accounts[0].status, None);
}

#[test]
fn invite_cards_load_once_and_failed_joins_go_back_to_join() {
    use crate::message::{Invite, InvitePreview};
    let mut app = Application::default();
    // Results only count for signed-in accounts.
    ready(&mut app, Network::Stoat, Vec::new());
    let acc = account(Network::Stoat);
    let code: Arc<str> = "Testers".into();
    let load = || Action::LoadInvite {
        account: account(Network::Stoat),
        code: "Testers".into(),
    };
    assert_eq!(app.dispatch(load()).len(), 1);
    assert!(app.dispatch(load()).is_empty(), "fetched once per session");
    let preview = InvitePreview {
        server: Some(ServerId::new(acc.clone(), "S1")),
        name: "Testers".into(),
        icon_url: None,
        members: Some(12),
    };
    app.apply(Event::InviteLoaded {
        account: acc.clone(),
        code: code.clone(),
        preview: Some(preview.clone()),
    });
    let join = app.dispatch(Action::JoinInvite {
        account: acc.clone(),
        code: code.clone(),
    });
    assert!(matches!(
        &join[..],
        [Effect::Backend {
            command: Command::JoinInvite { .. },
            ..
        }]
    ));
    assert!(matches!(
        app.state().invites.get(&(acc.clone(), code.clone())),
        Some(Invite::Joining(_))
    ));
    // The join failed: the network sends the preview again.
    app.apply(Event::InviteLoaded {
        account: acc.clone(),
        code: code.clone(),
        preview: Some(preview),
    });
    assert!(matches!(app.state().invites.get(&(acc, code)), Some(Invite::Ready(_))));
}

#[test]
fn an_invite_loads_nothing_until_answered() {
    let mut app = Application::default();
    let mut r = dm(Network::Discord, "!inv", "@ann:x");
    r.invited_by = Some("Ann".into());
    let id = r.id.clone();
    ready(&mut app, Network::Discord, vec![r]);
    let effects = app.dispatch(Action::SelectRoom(id.clone()));
    assert!(
        !commands(&effects)
            .iter()
            .any(|c| matches!(c, Command::LoadHistory { .. }))
    );
    assert_eq!(app.state().selected_server, None, "a direct chat opens in Home");
    assert_eq!(
        commands(&app.dispatch(Action::AnswerInvite {
            room: id.clone(),
            accept: true
        })),
        [&Command::AnswerInvite { room: id, accept: true }]
    );
    let (mut app, joined) = open_dm(&[]);
    assert!(
        app.dispatch(Action::AnswerInvite {
            room: joined,
            accept: false
        })
        .is_empty(),
        "not an invite"
    );
}

#[test]
fn what_a_room_doesnt_allow_does_nothing() {
    let mut app = Application::default();
    let mut r = dm(Network::Stoat, "r1", "them");
    r.capabilities = Capabilities {
        reply: false,
        edit: false,
        delete: false,
        ..r.capabilities
    };
    let id = r.id.clone();
    ready(&mut app, Network::Stoat, vec![r]);
    app.dispatch(Action::SelectRoom(id.clone()));
    app.apply(page(&id, None, &[], true));
    app.apply(Event::MessageAdded {
        room: id.clone(),
        message: mine("m1"),
    });
    assert!(
        app.state().selected_message(&MessageId("m1".into())).is_some(),
        "loaded"
    );
    app.dispatch(Action::ReplyTo(MessageId("m1".into())));
    app.dispatch(Action::StartEdit(MessageId("m1".into())));
    assert!(app.state().compose.is_empty());
    assert!(app.dispatch(Action::Delete(MessageId("m1".into()))).is_empty());
}

#[test]
fn deleting_system_and_other_messages_requires_ownership_or_moderation() {
    for system in [false, true] {
        for moderator in [false, true] {
            for own in [false, true] {
                for pending in [false, true] {
                    let mut app = Application::default();
                    let mut room = dm(Network::Stoat, "room", "them");
                    room.capabilities.delete_others = moderator;
                    let id = room.id.clone();
                    ready(&mut app, Network::Stoat, vec![room]);
                    app.dispatch(Action::SelectRoom(id.clone()));
                    app.apply(page(&id, None, &[], true));
                    let mut message = if own { mine("system") } else { msg("system") };
                    message.system = system;
                    message.pending = pending;
                    app.apply(Event::MessageAdded { room: id, message });
                    let deleted = app.dispatch(Action::Delete(MessageId("system".into())));
                    assert_eq!(
                        !deleted.is_empty(),
                        !pending && (own || moderator),
                        "system={system} moderator={moderator} own={own} pending={pending}"
                    );
                }
            }
        }
    }
}

#[test]
fn profile_editor_serializes_requests_and_preserves_loaded_data_after_failure() {
    let (mut app, room) = open_dm(&[]);
    let acc = room.account.clone();
    let load = || Action::LoadOwnProfile(acc.clone());
    assert_eq!(app.dispatch(load()).len(), 1);
    assert!(app.dispatch(load()).is_empty(), "do not duplicate an in-flight load");
    let old = OwnProfile {
        name: "Original".into(),
        ..Default::default()
    };
    app.apply(Event::OwnProfileLoaded {
        account: acc.clone(),
        result: Ok(old.clone()),
    });
    let save = || Action::SaveOwnProfile {
        account: acc.clone(),
        edit: ProfileEdit {
            display_name: Some("Changed".into()),
            ..Default::default()
        },
    };
    assert_eq!(app.dispatch(save()).len(), 1);
    assert!(app.dispatch(save()).is_empty());
    assert!(app.dispatch(load()).is_empty(), "loading must not race a save");
    app.apply(Event::OwnProfileSaved {
        account: acc.clone(),
        result: Err("Try again".into()),
    });
    assert_eq!(app.state().own_profiles[&acc].profile, Some(old));
    assert!(!app.state().own_profiles[&acc].saving);
    assert_eq!(app.dispatch(save()).len(), 1);
    app.apply(Event::OwnProfileSaved {
        account: acc.clone(),
        result: Ok(OwnProfile {
            name: "Changed".into(),
            display_name: "Changed".into(),
            ..Default::default()
        }),
    });
    assert!(app.state().own_profiles[&acc].saved);
    assert_eq!(
        app.state()
            .accounts
            .iter()
            .find(|a| a.id == acc)
            .unwrap()
            .display_name
            .as_ref(),
        "Changed"
    );
    app.apply(Event::AccountLost {
        account: acc.clone(),
        reason: "".into(),
    });
    app.apply(Event::OwnProfileLoaded {
        account: acc.clone(),
        result: Ok(OwnProfile::default()),
    });
    assert!(
        !app.state().own_profiles.contains_key(&acc),
        "late responses must not restore a logged-out account"
    );
}

#[test]
fn a_reply_goes_out_with_the_message_and_then_clears() {
    let (mut app, id) = open_dm(&["m1"]);
    app.dispatch(Action::ReplyTo(MessageId("m1".into())));
    assert_eq!(
        app.state().compose.get(&id),
        Some(&Compose::Reply(MessageId("m1".into())))
    );
    app.dispatch(Action::SetDraft("yes".into()));
    let effects = app.dispatch(Action::Send { now_ms: 1 });
    assert!(matches!(
        commands(&effects)[..],
        [Command::SendMessage { reply_to: Some(MessageId(r)), .. }] if &**r == "m1"
    ));
    assert!(app.state().compose.is_empty());
    assert!(app.dispatch(Action::ReplyTo(MessageId("nope".into()))).is_empty());
    assert!(app.state().compose.is_empty(), "only loaded messages can be answered");
}

#[test]
fn editing_starts_from_the_source_and_sends_an_edit() {
    let (mut app, id) = open_dm(&[]);
    app.apply(Event::MessageAdded {
        room: id.clone(),
        message: mine("m1"),
    });
    app.apply(Event::MessageAdded {
        room: id.clone(),
        message: msg("m2"),
    });
    app.dispatch(Action::StartEdit(MessageId("m2".into())));
    assert!(app.state().compose.is_empty(), "not ours");
    app.dispatch(Action::StartEdit(MessageId("m1".into())));
    assert_eq!(app.state().drafts[&id], "**raw** text");
    app.dispatch(Action::SetDraft("fixed".into()));
    let effects = app.dispatch(Action::Send { now_ms: 1 });
    assert_eq!(
        commands(&effects),
        [&Command::EditMessage {
            room: id.clone(),
            message: MessageId("m1".into()),
            text: "fixed".into()
        }]
    );
    assert_eq!(
        app.state().timeline(&id).unwrap().messages.len(),
        2,
        "no pending copy for an edit"
    );
}

#[test]
fn arrow_up_edits_our_latest_message_only_with_an_empty_draft() {
    let (mut app, id) = open_dm(&[]);
    app.apply(Event::MessageAdded {
        room: id.clone(),
        message: mine("m1"),
    });
    app.apply(Event::MessageAdded {
        room: id.clone(),
        message: mine("m2"),
    });
    app.apply(Event::MessageAdded {
        room: id.clone(),
        message: msg("m3"),
    });
    app.dispatch(Action::SetDraft("typing".into()));
    app.dispatch(Action::EditLast);
    assert!(app.state().compose.is_empty(), "a draft in progress isn't replaced");
    app.dispatch(Action::SetDraft("".into()));
    app.dispatch(Action::EditLast);
    assert_eq!(
        app.state().compose.get(&id),
        Some(&Compose::Edit(MessageId("m2".into())))
    );
}

#[test]
fn cancelling_an_edit_drops_its_text_but_a_reply_keeps_the_draft() {
    let (mut app, id) = open_dm(&[]);
    app.apply(Event::MessageAdded {
        room: id.clone(),
        message: mine("m1"),
    });
    app.dispatch(Action::StartEdit(MessageId("m1".into())));
    app.dispatch(Action::CancelCompose);
    assert!(!app.state().drafts.contains_key(&id));
    app.dispatch(Action::SetDraft("draft".into()));
    app.dispatch(Action::ReplyTo(MessageId("m1".into())));
    app.dispatch(Action::CancelCompose);
    assert_eq!(app.state().drafts[&id], "draft");
}

#[test]
fn reactions_toggle_and_deletes_go_out() {
    let (mut app, id) = open_dm(&[]);
    let mut m = mine("m1");
    m.reactions = vec![Reaction {
        emoji: "👍".into(),
        image_url: None,
        count: 2,
        me: true,
    }];
    app.apply(Event::MessageAdded {
        room: id.clone(),
        message: m,
    });
    let toggle = |app: &mut Application, emoji: &str| {
        app.dispatch(Action::ToggleReaction {
            message: MessageId("m1".into()),
            emoji: emoji.into(),
        })
    };
    assert!(matches!(
        commands(&toggle(&mut app, "👍"))[..],
        [Command::React { add: false, .. }]
    ));
    assert!(matches!(
        commands(&toggle(&mut app, "🎉"))[..],
        [Command::React { add: true, .. }]
    ));
    assert!(matches!(
        commands(&app.dispatch(Action::Delete(MessageId("m1".into()))))[..],
        [Command::DeleteMessage { .. }]
    ));
}

#[test]
fn search_asks_the_network_and_keeps_only_the_latest_answer() {
    let mut app = Application::default();
    let mut r = dm(Network::Discord, "c1", "them");
    r.capabilities.search = true;
    let id = r.id.clone();
    ready(&mut app, Network::Discord, vec![r]);
    app.dispatch(Action::SelectRoom(id.clone()));
    assert!(
        app.dispatch(Action::Search(String::new())).is_empty(),
        "opening asks nothing"
    );
    let effects = app.dispatch(Action::Search(" lunch ".into()));
    assert!(matches!(commands(&effects)[..], [Command::SearchMessages { query, .. }] if query == "lunch"));
    app.dispatch(Action::Search("dinner".into()));
    app.apply(Event::SearchResults {
        room: id.clone(),
        query: "lunch".into(),
        messages: Some(vec![msg("old")]),
    });
    assert!(
        app.state().search.as_ref().is_some_and(|s| s.messages.is_none()),
        "a stale answer is ignored"
    );
    app.apply(Event::SearchResults {
        room: id,
        query: "dinner".into(),
        messages: Some(vec![msg("d")]),
    });
    assert_eq!(
        app.state()
            .search
            .as_ref()
            .and_then(|s| s.messages.as_ref())
            .map(Vec::len),
        Some(1)
    );
    // Enter again on the same search: no second request.
    assert!(app.dispatch(Action::Search("dinner".into())).is_empty());
    // A failed one may be asked again.
    if let Some(search) = app.state.search.as_mut() {
        search.failed = true;
    }
    assert_eq!(commands(&app.dispatch(Action::Search("dinner".into()))).len(), 1);
    app.dispatch(Action::CloseSearch);
    assert!(app.state().search.is_none());
}

#[test]
fn pins_open_load_unpin_and_close_with_the_room() {
    let mut app = Application::default();
    let mut r = dm(Network::Discord, "c1", "them");
    r.capabilities.pins = true;
    r.capabilities.pin_messages = true;
    let mut other = dm(Network::Discord, "c2", "else");
    other.capabilities.pins = true;
    let (id, other_id) = (r.id.clone(), other.id.clone());
    ready(&mut app, Network::Discord, vec![r, other]);
    app.dispatch(Action::SelectRoom(id.clone()));
    let effects = app.dispatch(Action::OpenPins);
    assert!(matches!(commands(&effects)[..], [Command::LoadPins { .. }]));
    assert!(app.state().pins.as_ref().is_some_and(|p| p.messages.is_none()));
    app.apply(Event::Pins {
        room: id.clone(),
        messages: Some(vec![msg("a"), msg("b")]),
    });
    let first = app
        .state()
        .pins
        .as_ref()
        .and_then(|p| p.messages.as_ref())
        .map(|l| l[0].id.clone());
    let first = first.expect("loaded");
    let effects = app.dispatch(Action::SetPinned {
        message: first,
        pinned: false,
    });
    assert!(matches!(
        commands(&effects)[..],
        [Command::SetPinned { pinned: false, .. }]
    ));
    assert_eq!(
        app.state()
            .pins
            .as_ref()
            .and_then(|p| p.messages.as_ref())
            .map(Vec::len),
        Some(1)
    );
    app.dispatch(Action::SelectRoom(other_id));
    assert!(app.state().pins.is_none(), "another room closes the panel");
}

#[test]
fn custom_emoji_refreshes_existing_and_restored_reaction_pictures() {
    use crate::message::CustomEmoji;
    let (mut app, room) = open_dm(&[]);
    let acc = room.account.clone();
    let mut message = msg("custom");
    message.reactions = vec![Reaction {
        emoji: "custom-id".into(),
        image_url: None,
        count: 3,
        me: true,
    }];
    app.apply(Event::MessageAdded {
        room: room.clone(),
        message,
    });
    let old_snapshot = app.cache_snapshot(&acc);
    let emoji_event = |url: &str| Event::CustomEmoji {
        account: acc.clone(),
        groups: vec![EmojiGroup {
            server: ServerId::new(acc.clone(), "server"),
            title: "Server".into(),
            emoji: vec![CustomEmoji {
                name: "custom".into(),
                reaction: "custom-id".into(),
                code: ":custom-id:".into(),
                image_url: url.into(),
            }],
        }],
    };
    let assert_picture = |app: &Application, url: &str| {
        let reaction = &app.state().timeline(&room).unwrap().messages[0].reactions[0];
        assert_eq!(reaction.image_url.as_deref(), Some(url));
        assert_eq!(reaction.count, 3);
        assert!(reaction.me);
    };
    app.apply(emoji_event("https://media.example/emojis/new"));
    assert_picture(&app, "https://media.example/emojis/new");
    // Cached messages may arrive after the initial emoji event.
    app.restore_cache(old_snapshot);
    assert_picture(&app, "https://media.example/emojis/new");
    // Reconnecting to a different media origin must replace stale URLs.
    app.apply(emoji_event("https://new.example/emojis/new"));
    assert_picture(&app, "https://new.example/emojis/new");
    app.apply(Event::CustomEmoji {
        account: account(Network::Discord),
        groups: Vec::new(),
    });
    assert_picture(&app, "https://new.example/emojis/new");
}

#[test]
fn invalid_invites_can_be_retried_explicitly_without_duplicate_inflight_requests() {
    let mut app = Application::default();
    // Results only count for signed-in accounts.
    ready(&mut app, Network::Stoat, Vec::new());
    let account = account(Network::Stoat);
    let code: Arc<str> = "Expired".into();
    let load = || Action::LoadInvite {
        account: account.clone(),
        code: code.clone(),
    };
    assert_eq!(app.dispatch(load()).len(), 1);
    assert!(app.dispatch(load()).is_empty());
    app.apply(Event::InviteLoaded {
        account: account.clone(),
        code: code.clone(),
        preview: None,
    });
    assert_eq!(app.dispatch(load()).len(), 1);
    assert!(app.dispatch(load()).is_empty());
}

#[test]
fn marking_unread_reads_up_to_the_message_before() {
    let (mut app, id) = open_dm(&["m1", "m2", "m3"]);
    assert_eq!(
        commands(&app.dispatch(Action::MarkUnread(MessageId("m2".into())))),
        [&Command::MarkUnread {
            room: id.clone(),
            last_read: Some(MessageId("m1".into()))
        }]
    );
    assert_eq!(
        commands(&app.dispatch(Action::MarkUnread(MessageId("m1".into())))),
        [&Command::MarkUnread {
            room: id,
            last_read: None
        }]
    );
}

#[test]
fn typing_is_sent_while_writing_and_shown_from_others() {
    let (mut app, id) = open_dm(&[]);
    assert!(matches!(
        commands(&app.dispatch(Action::SetDraft("h".into())))[..],
        [Command::Typing { .. }]
    ));
    assert!(
        app.dispatch(Action::SetDraft("".into())).is_empty(),
        "deleting isn't typing"
    );
    app.apply(Event::Typing {
        room: id.clone(),
        names: vec!["Ann".into()],
    });
    assert_eq!(app.state().typing[&id].len(), 1);
    app.apply(Event::Typing {
        room: id.clone(),
        names: Vec::new(),
    });
    assert!(app.state().typing.is_empty());
}

#[test]
fn typing_runs_out_when_no_stop_comes() {
    let (mut app, id) = open_dm(&[]);
    app.apply(Event::Typing {
        room: id.clone(),
        names: vec!["Ann".into()],
    });
    let now = std::time::Instant::now();
    assert!(!app.expire_typing(now), "fresh notices stay");
    assert_eq!(app.state().typing[&id].len(), 1);
    assert!(app.expire_typing(now + crate::application::TYPING_FOR));
    assert!(app.state().typing.is_empty());
}

fn file(name: &str) -> Outgoing {
    Outgoing {
        path: PathBuf::from(format!("/tmp/{name}")),
        name: name.into(),
        size: 3,
    }
}

#[test]
fn files_go_with_the_next_message_even_without_text() {
    let (mut app, id) = open_dm(&[]);
    app.dispatch(Action::AddFiles(vec![file("a.png"), file("b.txt")]));
    app.dispatch(Action::RemoveFile(0));
    let effects = app.dispatch(Action::Send { now_ms: 1 });
    let [Command::SendMessage { text, files, .. }] = &commands(&effects)[..] else {
        panic!("expected a send, got {effects:?}");
    };
    assert!(text.is_empty());
    assert_eq!(files.iter().map(|f| &*f.name).collect::<Vec<_>>(), ["b.txt"]);
    assert!(app.state().files.is_empty());
    let pending = app.state().timeline(&id).unwrap().messages.last().unwrap().clone();
    assert_eq!(&*pending.attachments[0].name, "b.txt");
}

#[test]
fn an_edit_changes_only_the_text() {
    let mut app = Application::default();
    // History only lands in rooms that exist (late results are dropped).
    let r = dm(Network::Discord, "!r", "them");
    let id = r.id.clone();
    ready(&mut app, Network::Discord, vec![r]);
    let original = Message {
        sent_ms: 42,
        reply_to: Some(MessageId("$p".into())),
        ..msg("$m")
    };
    app.apply(Event::History {
        room: id.clone(),
        before: None,
        messages: vec![original],
        reached_start: true,
    });
    app.apply(Event::MessageEdited {
        room: id.clone(),
        id: MessageId("$m".into()),
        text: "fixed".into(),
        markup: Markup::Plain,
        source: "fixed".into(),
    });
    let m = &app.state().timelines[&id].messages[0];
    assert_eq!((&*m.text, m.edited, m.sent_ms), ("fixed", true, 42));
    assert!(m.reply_to.is_some(), "the reply stays");
    let thumbs = Reaction {
        emoji: "👍".into(),
        image_url: None,
        count: 2,
        me: true,
    };
    app.apply(Event::ReactionsChanged {
        room: id.clone(),
        id: MessageId("$m".into()),
        reactions: vec![thumbs.clone()],
    });
    assert_eq!(app.state().timelines[&id].messages[0].reactions, vec![thumbs]);
}

#[test]
fn friends_become_contacts_that_can_be_linked_unlinked_and_preferred() {
    let mut app = Application::default();
    // Results only count for signed-in accounts.
    ready(&mut app, Network::Stoat, Vec::new());
    let person = |raw: &str, name: &str| Person {
        id: UserId::new(account(Network::Stoat), raw),
        name: name.into(),
        avatar_url: None,
        online: true,
    };
    let effects = app.apply(Event::Friends {
        account: account(Network::Stoat),
        people: vec![person("a", "Ann"), person("b", "Ann (alt)")],
    });
    assert_eq!(effects, vec![Effect::SaveContacts]);
    assert!(
        app.apply(Event::Friends {
            account: account(Network::Stoat),
            people: vec![person("a", "Ann")]
        })
        .is_empty(),
        "known already"
    );
    let ids: Vec<ContactId> = app.state().contacts.iter().map(|c| c.id).collect();
    assert_eq!(
        app.dispatch(Action::MergeContacts {
            into: ids[0],
            from: ids[1]
        }),
        vec![Effect::SaveContacts]
    );
    assert_eq!(app.state().contacts.iter().count(), 1);
    app.dispatch(Action::SetPreferred {
        contact: ids[0],
        network: Some(Network::Stoat),
    });
    assert_eq!(
        app.state().contacts.get(ids[0]).unwrap().preferred,
        Some(Network::Stoat)
    );
    app.dispatch(Action::SplitIdentity(UserId::new(account(Network::Stoat), "b")));
    assert_eq!(app.state().contacts.iter().count(), 2, "unlinked again");
    app.dispatch(Action::ShowPeople);
    assert!(app.state().showing_people && !app.state().showing_friends);
    app.dispatch(Action::SetNudges {
        contact: ids[0],
        on: false,
    });
    assert!(app.state().contacts.get(ids[0]).unwrap().no_nudges);
    app.dispatch(Action::SetNudges {
        contact: ids[0],
        on: true,
    });
    assert!(!app.state().contacts.get(ids[0]).unwrap().no_nudges);
    // From a stranger's profile: they become a contact, People links them.
    let stranger = UserId::new(account(Network::Stoat), "z");
    assert_eq!(
        app.dispatch(Action::LinkFrom(stranger.clone())),
        vec![Effect::SaveContacts]
    );
    let c = app.state().contacts.of(&stranger).unwrap().id;
    assert_eq!(app.state().link_request, Some((c, 1)));
    assert!(app.state().showing_people && app.state().profile.is_none());
    assert!(app.dispatch(Action::LinkFrom(stranger)).is_empty(), "known already");
    assert_eq!(app.state().link_request, Some((c, 2)), "asks again");
}

#[test]
fn reopening_google_chat_refreshes_without_duplicate_inflight_requests() {
    let mut app = Application::default();
    let r = dm(Network::GoogleChat, "dm:1", "ann");
    let id = r.id.clone();
    ready(&mut app, Network::GoogleChat, vec![r]);
    app.dispatch(Action::SelectRoom(id.clone()));
    app.apply(Event::History {
        room: id.clone(),
        before: None,
        messages: Vec::new(),
        reached_start: true,
    });
    let effects = app.dispatch(Action::SelectRoom(id.clone()));
    assert!(
        commands(&effects)
            .iter()
            .any(|c| matches!(c, Command::LoadHistory { before: None, .. }))
    );
    let effects = app.dispatch(Action::SelectRoom(id));
    assert!(
        !commands(&effects)
            .iter()
            .any(|c| matches!(c, Command::LoadHistory { .. }))
    );
}

#[test]
fn opening_a_direct_chat_reuses_or_asks_for_one() {
    let mut app = Application::default();
    let r = dm(Network::Stoat, "d1", "ann");
    ready(&mut app, Network::Stoat, vec![r]);
    app.dispatch(Action::ShowFriends);
    assert!(app.state().showing_friends);
    let ann = UserId::new(account(Network::Stoat), "ann");
    let effects = app.dispatch(Action::OpenDirect(ann));
    assert!(matches!(
        commands(&effects)[..],
        [Command::Focus { .. }, Command::LoadHistory { .. }]
    ));
    assert!(!app.state().showing_friends);

    let bo = UserId::new(account(Network::Stoat), "bo");
    let effects = app.dispatch(Action::OpenDirect(bo.clone()));
    assert_eq!(commands(&effects), [&Command::OpenDirect { user: bo.clone() }]);
    app.apply(Event::RoomUpdated(dm(Network::Stoat, "d2", "bo")));
    app.apply(Event::RoomOpened(RoomId::new(account(Network::Stoat), "d2")));
    assert_eq!(app.state().selected.as_ref().map(|r| &*r.raw), Some("d2"));
}

#[test]
fn server_channels_load_members_and_the_list_toggles() {
    let mut app = Application::new(Contacts::default());
    let server = ServerId::new(account(Network::Stoat), "S");
    let ch = room(Network::Stoat, "c1", RoomKind::Channel { server });
    let group = room(Network::Stoat, "g1", RoomKind::Group);
    let (ic, ig) = (ch.id.clone(), group.id.clone());
    ready(&mut app, Network::Stoat, vec![ch, group, dm(Network::Stoat, "d", "x")]);
    assert!(app.state().show_members, "shown by default, as officially");
    let loads = |effects: &[Effect]| {
        commands(effects)
            .iter()
            .filter(|c| matches!(c, Command::LoadMembers { .. }))
            .count()
    };
    assert_eq!(loads(&app.dispatch(Action::SelectRoom(ic.clone()))), 1);
    assert_eq!(loads(&app.dispatch(Action::SelectRoom(ig.clone()))), 1);
    assert_eq!(
        loads(&app.dispatch(Action::SelectRoom(RoomId::new(account(Network::Stoat), "d")))),
        0,
        "a DM has none"
    );
    app.dispatch(Action::SelectRoom(ig.clone()));
    assert_eq!(loads(&app.dispatch(Action::ToggleMembers)), 0, "hiding asks nothing");
    assert_eq!(
        loads(&app.dispatch(Action::SelectRoom(ig))),
        0,
        "a hidden list isn't loaded for groups"
    );
    assert_eq!(
        loads(&app.dispatch(Action::SelectRoom(ic))),
        1,
        "server channels still are"
    );
}

#[test]
fn profiles_open_load_and_ignore_stale_answers() {
    let (mut app, id) = open_dm(&[]);
    let ann = UserId::new(account(Network::Stoat), "ann");
    let effects = app.dispatch(Action::OpenProfile(ann.clone()));
    assert_eq!(
        commands(&effects),
        [&Command::LoadProfile {
            user: ann.clone(),
            room: Some(id)
        }]
    );
    let profile = |user: &UserId| Profile {
        user: user.clone(),
        name: "Ann".into(),
        username: "ann#1".into(),
        avatar_url: None,
        banner_url: None,
        presence: Presence::Online,
        status: None,
        bio: None,
        roles: Vec::new(),
        me: false,
        relationship: Default::default(),
        bot: false,
        has_friends: true,
        moderation: None,
    };
    app.apply(Event::Profile(profile(&UserId::new(account(Network::Stoat), "bo"))));
    assert_eq!(
        app.state().profile,
        Some((ann.clone(), None)),
        "someone else's answer is ignored"
    );
    app.apply(Event::Profile(profile(&ann)));
    assert!(app.state().profile.as_ref().is_some_and(|(_, p)| p.is_some()));
    app.dispatch(Action::CloseProfile);
    assert!(app.state().profile.is_none());
    let refresh = app.dispatch(Action::OpenProfile(ann.clone()));
    assert!(
        app.state().profile.as_ref().is_some_and(|(_, p)| p.is_some()),
        "cached profile is immediate"
    );
    assert!(
        commands(&refresh)
            .iter()
            .any(|command| matches!(command, Command::LoadProfile { user, .. } if user == &ann)),
        "network refresh still runs"
    );
}

#[test]
fn streams_in_a_call_can_be_watched() {
    let mut app = Application::default();
    let a = dm(Network::Discord, "a", "x");
    let ia = a.id.clone();
    ready(&mut app, Network::Discord, vec![a]);
    app.dispatch(Action::JoinCall(ia.clone()));
    let zed = UserId::new(ia.account.clone(), "zed");
    let watch = Action::WatchStream {
        user: zed.clone(),
        watch: true,
    };
    assert!(app.dispatch(watch.clone()).is_empty(), "wait for connection");
    app.apply(Event::CallConnected { room: ia.clone() });
    assert!(app.dispatch(watch.clone()).is_empty(), "nobody streams");
    let streams = |watching| Event::CallStreams {
        room: ia.clone(),
        streams: vec![crate::call::CallStream {
            user: zed.clone(),
            watching,
            note: None,
        }],
    };
    app.apply(streams(false));
    assert_eq!(app.state().call.as_ref().unwrap().streams.len(), 1);
    assert!(matches!(
        app.dispatch(watch.clone()).as_slice(),
        [Effect::WatchStream { room, user, watch: true, .. }] if *room == ia && *user == zed
    ));
    app.apply(streams(true));
    assert!(app.dispatch(watch).is_empty(), "already watching");
}

#[test]
fn joining_leaving_and_switching_calls() {
    let mut app = Application::default();
    let a = dm(Network::Stoat, "a", "x");
    let b = dm(Network::Stoat, "b", "y");
    let (ia, ib) = (a.id.clone(), b.id.clone());
    ready(&mut app, Network::Stoat, vec![a, b]);

    assert_eq!(
        commands(&app.dispatch(Action::JoinCall(ia.clone()))),
        [&Command::JoinCall { room: ia.clone() }]
    );
    assert!(app.dispatch(Action::JoinCall(ia.clone())).is_empty(), "already in it");
    assert!(app.dispatch(Action::SetCamera(true)).is_empty(), "wait for connection");
    app.apply(Event::CallConnected { room: ia.clone() });
    assert_eq!(app.state().call.as_ref().unwrap().status, CallStatus::Connected);
    assert_eq!(
        commands(&app.dispatch(Action::SetCamera(true))),
        [&Command::SetCamera(true)]
    );
    assert_eq!(
        commands(&app.dispatch(Action::SetScreenShare(true))),
        [&Command::SetScreenShare(true)]
    );
    assert!(
        !app.state().call.as_ref().unwrap().camera,
        "wait for publication confirmation"
    );
    app.apply(Event::CallCamera {
        room: ib.clone(),
        active: true,
    });
    assert!(
        !app.state().call.as_ref().unwrap().camera,
        "ignore another room's update"
    );
    app.apply(Event::CallCamera {
        room: ia.clone(),
        active: true,
    });
    app.apply(Event::CallScreenShare {
        room: ia.clone(),
        active: true,
    });
    assert!(app.state().call.as_ref().unwrap().camera);
    assert!(app.state().call.as_ref().unwrap().screen_share);

    assert_eq!(commands(&app.dispatch(Action::ToggleMute)), [&Command::SetMuted(true)]);
    assert_eq!(
        commands(&app.dispatch(Action::ToggleDeafen)),
        [&Command::SetDeafened(true)]
    );

    let effects = app.dispatch(Action::JoinCall(ib.clone()));
    assert_eq!(
        commands(&effects),
        [
            &Command::LeaveCall,
            &Command::JoinCall { room: ib.clone() },
            &Command::SetMuted(true),
            &Command::SetDeafened(true)
        ]
    );
    // Our mute and deafen carry into the next call (see
    // dispatch_join_call), and the new call is told so.
    let call = app.state().call.as_ref().unwrap();
    assert!(call.muted && call.deafened);
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::Backend {
            command: Command::SetMuted(true),
            ..
        }
    )));
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::Backend {
            command: Command::SetDeafened(true),
            ..
        }
    )));

    // A stale end for the old room doesn't drop the new call.
    app.apply(Event::CallEnded { room: ia, reason: None });
    assert!(app.state().call.is_some());
    app.apply(Event::CallEnded {
        room: ib,
        reason: Some("server went away".into()),
    });
    assert!(app.state().call.is_none());
    assert_eq!(app.state().errors.len(), 1);
    assert!(app.dispatch(Action::LeaveCall).is_empty());
}

#[test]
fn rooms_without_voice_cant_be_called_and_occupants_are_tracked() {
    let mut app = Application::default();
    let mut r = dm(Network::Steam, "s", "x");
    r.capabilities.voice = false;
    let id = r.id.clone();
    ready(&mut app, Network::Steam, vec![r]);
    assert!(app.dispatch(Action::JoinCall(id.clone())).is_empty());

    let member = CallMember {
        user: UserId::new(account(Network::Steam), "x"),
        name: "X".into(),
        avatar_url: None,
        speaking: false,
        muted: true,
        deafened: false,
    };
    app.apply(Event::VoiceOccupants {
        room: id.clone(),
        members: vec![member],
    });
    assert_eq!(app.state().voice[&id].len(), 1);
    app.apply(Event::VoiceOccupants {
        room: id.clone(),
        members: Vec::new(),
    });
    assert!(app.state().voice.is_empty());
}

/// A signed-in Stoat account with one server whose settings are open,
/// and the request id the open sent.
fn server_settings_open(app: &mut Application) -> (ServerId, u64) {
    let n = Network::Stoat;
    let server = ServerId::new(account(n), "zq");
    ready(app, n, Vec::new());
    app.apply(Event::ServersChanged {
        account: account(n),
        servers: vec![Server {
            id: server.clone(),
            name: "Vorp".into(),
            icon_url: None,
            banner_url: None,
            categories: Vec::new(),
            muted: false,
            rights: Default::default(),
        }],
    });
    let effects = app.dispatch(Action::OpenServerSettings(server.clone()));
    let [
        Effect::Backend {
            command: Command::LoadServerOverview { request, .. },
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("opening should load the overview, got {effects:?}");
    };
    (server, *request)
}

fn overview(can_manage: bool) -> crate::server_settings::ServerOverview {
    crate::server_settings::ServerOverview {
        name: "Vorp".into(),
        can_manage,
        ..Default::default()
    }
}

fn rename(to: &str) -> crate::server_settings::ServerOverviewEdit {
    crate::server_settings::ServerOverviewEdit {
        name: Some(to.into()),
        ..Default::default()
    }
}

#[test]
fn server_overview_ignores_stale_results() {
    let mut app = Application::default();
    let (server, first) = server_settings_open(&mut app);
    let effects = app.dispatch(Action::OpenServerSettings(server.clone()));
    assert!(effects.is_empty(), "a second open waits for the first load");
    app.dispatch(Action::CloseServerSettings);
    let (_, second) = server_settings_open(&mut app);
    assert_ne!(first, second);
    app.apply(Event::ServerOverview {
        server: server.clone(),
        request: first,
        result: Ok(overview(true)),
    });
    let s = app.state().server_settings.as_ref().unwrap();
    assert!(s.busy && s.overview.is_none(), "the closed editor's answer is dropped");
    app.apply(Event::ServerOverview {
        server: server.clone(),
        request: second,
        result: Ok(overview(true)),
    });
    let s = app.state().server_settings.as_ref().unwrap();
    assert!(!s.busy && !s.saved && s.overview.is_some());
    // A repeat of a finished request changes nothing.
    app.apply(Event::ServerOverview {
        server,
        request: second,
        result: Err("late".into()),
    });
    assert!(app.state().server_settings.as_ref().unwrap().error.is_none());
}

#[test]
fn server_overview_saves_only_with_permission_and_keeps_it_after_failure() {
    let mut app = Application::default();
    let (server, request) = server_settings_open(&mut app);
    app.apply(Event::ServerOverview {
        server: server.clone(),
        request,
        result: Ok(overview(false)),
    });
    assert!(
        app.dispatch(Action::SaveServerOverview(rename("Blix"))).is_empty(),
        "no Manage Server, no request"
    );
    app.dispatch(Action::CloseServerSettings);
    let (server, request) = server_settings_open(&mut app);
    app.apply(Event::ServerOverview {
        server: server.clone(),
        request,
        result: Ok(overview(true)),
    });
    assert!(
        app.dispatch(Action::SaveServerOverview(Default::default())).is_empty(),
        "nothing changed, nothing sent"
    );
    let effects = app.dispatch(Action::SaveServerOverview(rename("Blix")));
    let [
        Effect::Backend {
            command: Command::SaveServerOverview {
                request: save, edit, ..
            },
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("expected a save, got {effects:?}");
    };
    let save = *save;
    assert_eq!(edit.name.as_deref(), Some("Blix"));
    assert!(
        app.dispatch(Action::SaveServerOverview(rename("Quorf"))).is_empty(),
        "one save at a time"
    );
    app.apply(Event::ServerOverview {
        server: server.clone(),
        request: save,
        result: Err("refused".into()),
    });
    let s = app.state().server_settings.as_ref().unwrap();
    assert_eq!(s.error.as_deref(), Some("refused"));
    assert!(!s.busy && !s.saved);
    assert_eq!(
        s.overview.as_ref().map(|o| o.name.as_ref()),
        Some("Vorp"),
        "the last good overview stays"
    );
    let effects = app.dispatch(Action::SaveServerOverview(rename("Blix")));
    let [
        Effect::Backend {
            command: Command::SaveServerOverview { request: retry, .. },
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("a retry should be allowed, got {effects:?}");
    };
    let mut saved = overview(true);
    saved.name = "Blix".into();
    app.apply(Event::ServerOverview {
        server,
        request: *retry,
        result: Ok(saved),
    });
    let s = app.state().server_settings.as_ref().unwrap();
    assert!(s.saved && s.error.is_none());
    assert_eq!(s.overview.as_ref().map(|o| o.name.as_ref()), Some("Blix"));
}

#[test]
fn video_starts_require_permission_and_explicit_shares_stay_with_their_call() {
    let (mut app, room) = open_dm(&[]);
    app.dispatch(Action::JoinCall(room.clone()));
    app.apply(Event::CallConnected { room: room.clone() });
    let quality = crate::call::ScreenShareQuality::High;
    assert_eq!(
        commands(&app.dispatch(Action::StartScreenShare {
            room: room.clone(),
            quality
        })),
        [&Command::StartScreenShare { quality }]
    );
    assert!(
        app.dispatch(Action::StartScreenShare {
            room: RoomId::new(room.account.clone(), "other"),
            quality
        })
        .is_empty()
    );
    let mut updated = app.state().room(&room).unwrap().as_ref().clone();
    updated.capabilities.video = false;
    app.apply(Event::RoomUpdated(updated));
    assert!(app.dispatch(Action::SetCamera(true)).is_empty());
    assert!(app.dispatch(Action::SetScreenShare(true)).is_empty());
    assert!(app.dispatch(Action::StartScreenShare { room, quality }).is_empty());
    assert_eq!(
        commands(&app.dispatch(Action::SetCamera(false))),
        [&Command::SetCamera(false)]
    );
    assert_eq!(
        commands(&app.dispatch(Action::SetScreenShare(false))),
        [&Command::SetScreenShare(false)]
    );
}

#[test]
fn server_settings_close_when_the_server_or_account_goes() {
    let mut app = Application::default();
    let (server, request) = server_settings_open(&mut app);
    app.apply(Event::ServersChanged {
        account: account(Network::Stoat),
        servers: Vec::new(),
    });
    assert!(
        app.state().server_settings.is_none(),
        "leaving the server closes its settings"
    );
    app.apply(Event::ServerOverview {
        server,
        request,
        result: Ok(overview(true)),
    });
    assert!(
        app.state().server_settings.is_none(),
        "and a late answer doesn't reopen them"
    );

    let mut app = Application::default();
    let (server, request) = server_settings_open(&mut app);
    app.apply(Event::AccountLost {
        account: account(Network::Stoat),
        reason: "".into(),
    });
    assert!(app.state().server_settings.is_none(), "signing out closes them");
    app.apply(Event::ServerOverview {
        server: server.clone(),
        request,
        result: Ok(overview(true)),
    });
    assert!(app.state().server_settings.is_none());
    assert!(
        app.dispatch(Action::OpenServerSettings(server)).is_empty(),
        "a gone server can't be opened"
    );
}

#[test]
fn server_admin_pages_load_once_shown_and_report_changes() {
    use crate::server_settings::{ServerAdmin, ServerAdminResult, ServerPage};
    let mut app = Application::default();
    let (server, request) = server_settings_open(&mut app);
    app.apply(Event::ServerOverview {
        server: server.clone(),
        request,
        result: Ok(overview(true)),
    });
    let effects = app.dispatch(Action::ServerSettingsPage(ServerPage::Bans));
    assert!(matches!(
        effects.as_slice(),
        [Effect::Backend {
            command: Command::ServerAdmin {
                op: ServerAdmin::LoadBans,
                ..
            },
            ..
        }]
    ));
    assert!(
        app.dispatch(Action::ServerSettingsPage(ServerPage::Bans)).is_empty(),
        "already there"
    );
    app.apply(Event::ServerAdmin {
        server: server.clone(),
        result: Ok(ServerAdminResult::Bans(Vec::new())),
    });
    assert_eq!(
        app.state()
            .server_settings
            .as_ref()
            .unwrap()
            .bans
            .as_ref()
            .map(Vec::len),
        Some(0)
    );

    let unban = || Action::ServerAdmin {
        server: server.clone(),
        op: ServerAdmin::Unban(UserId::new(account(Network::Stoat), "zork")),
    };
    assert_eq!(app.dispatch(unban()).len(), 1);
    assert!(app.state().server_settings.as_ref().unwrap().admin_busy);
    assert!(app.dispatch(unban()).is_empty(), "one change at a time");
    app.apply(Event::ServerAdmin {
        server: server.clone(),
        result: Err("nope".into()),
    });
    let s = app.state().server_settings.as_ref().unwrap();
    assert!(!s.admin_busy);
    assert_eq!(s.notice, Some(Err("nope".into())));

    // A reload stays on its page and fetches its list again.
    let effects = app.dispatch(Action::OpenServerSettings(server.clone()));
    assert_eq!(effects.len(), 2, "overview and bans: {effects:?}");
    assert_eq!(app.state().server_settings.as_ref().unwrap().page, ServerPage::Bans);

    // With the settings closed (a card's kick), a refusal goes to Problems.
    app.dispatch(Action::CloseServerSettings);
    app.apply(Event::ServerAdmin {
        server,
        result: Err("refused".into()),
    });
    assert!(app.state().errors.iter().any(|e| &**e == "refused"));
}

#[test]
fn the_invite_dialog_waits_for_its_link_and_closes_on_failure() {
    use crate::server_settings::{ServerAdmin, ServerAdminResult};
    let mut app = Application::default();
    let (server, _) = server_settings_open(&mut app);
    app.dispatch(Action::CloseServerSettings);
    let invite = || Action::ServerAdmin {
        server: server.clone(),
        op: ServerAdmin::CreateInvite,
    };
    app.dispatch(invite());
    assert_eq!(app.state().invite_link, Some((server.clone(), None)));
    app.apply(Event::ServerAdmin {
        server: server.clone(),
        result: Ok(ServerAdminResult::InviteCreated("https://stt.gg/blorp".into())),
    });
    assert_eq!(
        app.state().invite_link,
        Some((server.clone(), Some("https://stt.gg/blorp".into())))
    );
    app.dispatch(Action::CloseInviteLink);
    assert!(app.state().invite_link.is_none());

    app.dispatch(invite());
    app.apply(Event::ServerAdmin {
        server: server.clone(),
        result: Err("no".into()),
    });
    assert!(app.state().invite_link.is_none(), "a failure closes the dialog");
    assert!(app.state().errors.iter().any(|e| &**e == "no"), "and says why");

    app.dispatch(invite());
    app.apply(Event::ServersChanged {
        account: account(Network::Stoat),
        servers: Vec::new(),
    });
    assert!(app.state().invite_link.is_none(), "it goes with its server");
}

#[test]
fn home_pins_hides_and_marks_unread_by_choice() {
    let mut app = Application::default();
    let n = Network::Stoat;
    ready(&mut app, n, vec![dm(n, "a", "zib"), dm(n, "b", "zab")]);
    let a = RoomId::new(account(n), "a");
    app.dispatch(Action::PinRoom {
        room: a.clone(),
        pinned: true,
    });
    assert_eq!(app.state().pinned_rooms, std::slice::from_ref(&a));
    app.dispatch(Action::HideRoom {
        room: a.clone(),
        hidden: true,
    });
    assert!(app.state().pinned_rooms.is_empty(), "hiding unpins");
    assert_eq!(app.state().hidden_rooms, std::slice::from_ref(&a));
    // Someone writing brings it back, and moves it up.
    let mut m = msg("m1");
    m.sent_ms = 5_000;
    app.apply(Event::MessageAdded {
        room: a.clone(),
        message: m,
    });
    assert!(app.state().hidden_rooms.is_empty());
    assert_eq!(app.state().room(&a).map(|r| r.last_activity), Some(5_000));
    app.dispatch(Action::MarkRoomUnread(a.clone()));
    assert_eq!(app.state().marked_unread, std::slice::from_ref(&a));
    app.dispatch(Action::SelectRoom(a.clone()));
    assert!(app.state().marked_unread.is_empty(), "opening it reads it");
    app.dispatch(Action::ToggleHiddenRooms);
    assert!(app.state().show_hidden);
}

#[test]
fn a_removed_room_leaves_state() {
    let mut app = Application::new(Contacts::default());
    let gone = dm(Network::Slack, "T:D1", "T:ann");
    let kept = dm(Network::Slack, "T:D2", "T:bob");
    ready(&mut app, Network::Slack, vec![gone.clone(), kept.clone()]);
    app.apply(Event::RoomRemoved(gone.id.clone()));
    let ids: Vec<_> = app.state().rooms.iter().map(|r| r.id.clone()).collect();
    assert_eq!(ids, [kept.id]);
}

#[test]
fn embeds_arriving_alone_replace_only_the_embeds() {
    let (mut app, id) = open_dm(&["m1"]);
    let first = app.state().timelines[&id].messages[0].clone();
    assert!(first.embeds.is_empty());
    app.apply(Event::EmbedsUpdated {
        room: id.clone(),
        id: first.id.clone(),
        embeds: vec![crate::message::Embed {
            url: Some("https://example.com/cat".into()),
            ..crate::message::Embed::default()
        }],
    });
    let now = &app.state().timelines[&id].messages[0];
    assert_eq!(now.embeds.len(), 1);
    assert_eq!(now.source, first.source, "the rest stays");
}
