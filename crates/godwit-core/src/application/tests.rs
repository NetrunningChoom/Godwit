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
        notify: Default::default(),
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
        forwardable: false,
        poll_endable: false,
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
fn a_failed_session_action_keeps_the_loaded_sessions_for_retry() {
    let mut app = Application::new(Contacts::default());
    let account = account(Network::Stoat);
    app.state.sessions.insert(
        account.clone(),
        crate::privacy::SessionsView {
            sessions: vec![crate::privacy::Session {
                id: "session".into(),
                device: "Linux".into(),
                location: "".into(),
                created_ms: None,
                last_used_ms: None,
            }],
            loading: true,
            error: None,
        },
    );
    app.apply(Event::SessionActionFailed {
        account: account.clone(),
        error: "The code didn't match.".into(),
    });
    let sessions = &app.state.sessions[&account];
    assert_eq!(sessions.sessions.len(), 1);
    assert!(!sessions.loading);
    assert_eq!(sessions.error.as_deref(), Some("The code didn't match."));
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
fn nonces_are_unique_across_restarts() {
    // A restarted app counts from one again; the send time keeps its nonce
    // new, or the network refuses it as a repeat (Stoat: 409 Conflict).
    let send_at = |now_ms: i64| {
        let mut app = Application::default();
        let r = dm(Network::Stoat, "r1", "them");
        let id = r.id.clone();
        ready(&mut app, Network::Stoat, vec![r]);
        app.dispatch(Action::SelectRoom(id));
        app.dispatch(Action::SetDraft("hi".into()));
        match commands(&app.dispatch(Action::Send { now_ms }))[..] {
            [Command::SendMessage { nonce, .. }] => nonce.clone(),
            _ => panic!("expected a send"),
        }
    };
    assert_ne!(send_at(1_000), send_at(2_000));
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
    app.apply(Event::Blocked {
        account: me.clone(),
        users: vec![UserId::new(me.clone(), "them")],
    });
    assert_eq!(app.state().blocked.len(), 1);
    app.apply(Event::AccountLost {
        account: me.clone(),
        reason: "".into(),
    });
    assert!(app.state().blocked.is_empty(), "signing out clears the blocked list");
    app.apply(Event::Blocked {
        account: me.clone(),
        users: vec![UserId::new(me.clone(), "them")],
    });
    assert!(app.state().blocked.is_empty(), "late lists are ignored");
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
            id: None,
            can_manage: false,
            title: "".into(),
            rooms: vec![cid.clone()],
        }],
        muted: false,
        folder: None,
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
                id: None,
                can_manage: false,
                title: "".into(),
                rooms: ["talk", "general", "memes"]
                    .map(|r| RoomId::new(account(n), r))
                    .to_vec(),
            }],
            muted: false,
            folder: None,
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
        folder: None,
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
fn reaction_moderation_requires_capability_and_targets_the_requested_emoji() {
    let mut app = Application::default();
    let mut r = dm(Network::Stoat, "moderate", "them");
    r.capabilities.moderate_reactions = true;
    let room = r.id.clone();
    ready(&mut app, Network::Stoat, vec![r]);
    app.dispatch(Action::SelectRoom(room.clone()));
    let mut message = mine("with-reactions");
    message.reactions = vec![Reaction {
        emoji: "👍".into(),
        image_url: None,
        count: 2,
        me: false,
    }];
    app.apply(Event::MessageAdded { room, message });

    let remove_one = app.dispatch(Action::RemoveReactions {
        message: MessageId("with-reactions".into()),
        emoji: Some("👍".into()),
    });
    assert!(matches!(commands(&remove_one)[..], [Command::RemoveReactions { emoji: Some(e), .. }] if &**e == "👍"));
    let remove_all = app.dispatch(Action::RemoveReactions {
        message: MessageId("with-reactions".into()),
        emoji: None,
    });
    assert!(matches!(
        commands(&remove_all)[..],
        [Command::RemoveReactions { emoji: None, .. }]
    ));

    let mut denied = Application::default();
    let r = dm(Network::Stoat, "no-moderate", "them");
    let room = r.id.clone();
    ready(&mut denied, Network::Stoat, vec![r]);
    denied.dispatch(Action::SelectRoom(room.clone()));
    let mut denied_message = mine("with-reactions");
    denied_message.reactions = vec![Reaction {
        emoji: "👍".into(),
        image_url: None,
        count: 2,
        me: false,
    }];
    denied.apply(Event::MessageAdded {
        room,
        message: denied_message,
    });
    assert!(
        denied
            .dispatch(Action::RemoveReactions {
                message: MessageId("with-reactions".into()),
                emoji: None,
            })
            .is_empty()
    );
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
    assert!(matches!(commands(&effects)[..], [Command::SearchMessages { query, .. }] if query.text == "lunch"));
    app.dispatch(Action::Search("dinner".into()));
    app.apply(Event::SearchResults {
        room: id.clone(),
        query: crate::search::SearchQuery::text_only("lunch"),
        offset: 0,
        next_offset: None,
        total: None,
        notice: None,
        rooms: Vec::new(),
        messages: Some(vec![msg("old")]),
    });
    assert!(
        app.state().search.as_ref().is_some_and(|s| s.messages.is_none()),
        "a stale answer is ignored"
    );
    app.apply(Event::SearchResults {
        room: id,
        query: crate::search::SearchQuery::text_only("dinner"),
        offset: 0,
        next_offset: None,
        total: None,
        notice: None,
        rooms: Vec::new(),
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
fn search_filters_resolve_members_page_and_switch_scope() {
    use crate::search::{SearchFilters, SearchScope};
    let mut app = Application::default();
    let mut r = dm(Network::Discord, "c1", "robin");
    r.name = "Robin".into();
    r.capabilities.search = true;
    r.capabilities.search_filters = SearchFilters::ALL;
    let id = r.id.clone();
    ready(&mut app, Network::Discord, vec![r]);
    app.dispatch(Action::SelectRoom(id.clone()));
    // A name nobody has: nothing is sent, the hint says why.
    assert!(app.dispatch(Action::Search("from:ghost hi".into())).is_empty());
    assert_eq!(app.state().search.as_ref().map(|s| s.errors.len()), Some(1));
    // The DM's other person resolves, case-insensitively.
    let effects = app.dispatch(Action::Search("from:ROBIN has:link hi".into()));
    let [Command::SearchMessages { query, offset: 0, .. }] = commands(&effects)[..] else {
        panic!("one search");
    };
    assert_eq!(
        (query.text.as_str(), query.from.is_some(), query.has.len()),
        ("hi", true, 1)
    );
    let query = query.clone();
    let page = |offset, next| Event::SearchResults {
        room: id.clone(),
        query: query.clone(),
        offset,
        next_offset: next,
        total: Some(3),
        notice: None,
        rooms: Vec::new(),
        messages: Some(vec![msg("m")]),
    };
    // Load more only after a page said there is more, one at a time.
    assert!(app.dispatch(Action::SearchMore).is_empty());
    app.apply(page(0, Some(2)));
    let effects = app.dispatch(Action::SearchMore);
    assert!(matches!(
        commands(&effects)[..],
        [Command::SearchMessages { offset: 2, .. }]
    ));
    assert!(app.dispatch(Action::SearchMore).is_empty(), "one at a time");
    app.apply(page(2, None));
    let s = app.state().search.as_ref().expect("search");
    assert_eq!(
        (s.messages.as_ref().map(Vec::len), s.total, s.next_offset),
        (Some(2), Some(3), None)
    );
    // The scope toggle asks again, only where the network has one.
    let effects = app.dispatch(Action::SearchScope(SearchScope::Server));
    assert!(matches!(
        commands(&effects)[..],
        [Command::SearchMessages { query, .. }] if query.scope == SearchScope::Server
    ));
}

#[test]
fn a_hit_from_another_channel_is_jumped_to_in_its_own_room() {
    let mut app = Application::default();
    let (mut a, mut b) = (dm(Network::Discord, "c1", "x"), dm(Network::Discord, "c2", "y"));
    (a.capabilities.search, b.capabilities.search) = (true, true);
    let (ia, ib) = (a.id.clone(), b.id.clone());
    ready(&mut app, Network::Discord, vec![a, b]);
    app.dispatch(Action::SelectRoom(ia.clone()));
    let effects = app.dispatch(Action::Search("hi".into()));
    let [Command::SearchMessages { query, .. }] = commands(&effects)[..] else {
        panic!("one search")
    };
    app.apply(Event::SearchResults {
        room: ia.clone(),
        query: query.clone(),
        offset: 0,
        next_offset: None,
        total: Some(2),
        notice: None,
        rooms: vec![ia.clone(), ib.clone()],
        messages: Some(vec![msg("m1"), msg("m2")]),
    });
    let s = app.state().search.as_ref().expect("search");
    assert_eq!(s.rooms, [ia, ib.clone()]);
    // What the shell does for the second hit: close, select its room, jump.
    app.dispatch(Action::CloseSearch);
    app.dispatch(Action::SelectRoom(ib.clone()));
    let effects = app.dispatch(Action::JumpTo(MessageId("m2".into())));
    assert!(
        commands(&effects)
            .iter()
            .any(|c| matches!(c, Command::LoadAround { room, .. } if *room == ib))
    );
}

#[test]
fn search_without_network_support_filters_loaded_messages_locally() {
    let mut app = Application::default();
    let room = dm(Network::Steam, "local-search", "them");
    let id = room.id.clone();
    ready(&mut app, Network::Steam, vec![room]);
    app.dispatch(Action::SelectRoom(id.clone()));
    let mut needle = msg("needle");
    needle.source = "Needle in a haystack".into();
    let other = msg("other");
    app.apply(Event::History {
        room: id.clone(),
        before: None,
        messages: vec![needle, other],
        reached_start: true,
    });
    let effects = app.dispatch(Action::Search(" NEEDLE ".into()));
    assert!(effects.is_empty());
    let results = app
        .state()
        .search
        .as_ref()
        .and_then(|search| search.messages.as_ref())
        .unwrap();
    assert_eq!(
        results.iter().map(|message| message.id.0.as_ref()).collect::<Vec<_>>(),
        ["needle"]
    );
}

#[test]
fn reports_need_the_capability_and_a_reason_of_the_targets_list() {
    use crate::report::{ReportReason, ReportTarget};
    let mut app = Application::default();
    let mut allowed = dm(Network::Stoat, "c1", "them");
    allowed.capabilities.report = true;
    let denied = dm(Network::Stoat, "c2", "else");
    let (yes, no) = (allowed.id.clone(), denied.id.clone());
    ready(&mut app, Network::Stoat, vec![allowed, denied]);
    let message = |room: &RoomId| ReportTarget::Message {
        message: MessageId("m".into()),
        room: room.clone(),
    };
    let report = |target, reason| Action::Report {
        target,
        reason,
        detail: " why ".into(),
    };

    let effects = app.dispatch(report(message(&yes), ReportReason::Malware));
    assert!(matches!(
        commands(&effects)[..],
        [Command::Report { reason: ReportReason::Malware, detail, .. }] if detail == "why"
    ));
    assert!(app.state().report_outcome.is_none());
    app.apply(Event::Reported {
        account: yes.account.clone(),
        result: Ok(()),
    });
    assert_eq!(app.state().report_outcome, Some(Ok(())));

    // No capability, or a user-only reason for a message: refused, and the dialog is told.
    assert!(app.dispatch(report(message(&no), ReportReason::Malware)).is_empty());
    assert!(matches!(app.state().report_outcome, Some(Err(_))));
    assert!(app.dispatch(report(message(&yes), ReportReason::Underage)).is_empty());
    // Nobody we have no profile for.
    let stranger = ReportTarget::User {
        user: UserId::new(yes.account.clone(), "x"),
        message: None,
    };
    assert!(app.dispatch(report(stranger, ReportReason::Impersonation)).is_empty());
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
fn marking_unread_needs_the_capability() {
    let mut app = Application::default();
    let mut r = dm(Network::Stoat, "r1", "them");
    r.capabilities.mark_unread = false;
    let id = r.id.clone();
    ready(&mut app, Network::Stoat, vec![r]);
    app.dispatch(Action::SelectRoom(id.clone()));
    app.apply(page(&id, None, &["m1", "m2"], true));
    assert!(app.dispatch(Action::MarkUnread(MessageId("m2".into()))).is_empty());
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
fn creating_a_group_requires_two_unique_users_on_a_supported_signed_in_account() {
    let (mut app, _) = open_dm(&[]);
    let account = account(Network::Stoat);
    let users = vec![UserId::new(account.clone(), "ann"), UserId::new(account.clone(), "bo")];
    let effects = app.dispatch(Action::CreateGroup {
        account: account.clone(),
        name: "  Project  ".into(),
        users: users.clone(),
    });
    assert_eq!(
        commands(&effects),
        [&Command::CreateGroup {
            name: "Project".into(),
            users: users.clone(),
        }]
    );
    assert!(
        app.dispatch(Action::CreateGroup {
            account,
            name: "Project".into(),
            users: vec![users[0].clone(), users[0].clone()],
        })
        .is_empty()
    );
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
            room: Some(id.clone())
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
        group_moderation: None,
        can_block: false,
        can_report: false,
        mutual_servers: Vec::new(),
        mutual_friends: Vec::new(),
        note: None,
        can_note: false,
        note_limit: 0,
    };
    app.apply(Event::Profile(profile(&UserId::new(account(Network::Stoat), "bo"))));
    assert_eq!(
        app.state().profile,
        Some((ann.clone(), None)),
        "someone else's answer is ignored"
    );
    app.apply(Event::Profile(profile(&ann)));
    assert!(app.state().profile.as_ref().is_some_and(|(_, p)| p.is_some()));
    let mut owner_profile = profile(&ann);
    owner_profile.group_moderation = Some(crate::members::GroupModeration {
        room: id.clone(),
        remove_member: false,
        transfer_ownership: true,
    });
    app.apply(Event::Profile(owner_profile));
    assert_eq!(
        commands(&app.dispatch(Action::TransferGroupOwnership {
            room: id.clone(),
            user: ann.clone(),
        })),
        [&Command::TransferGroupOwnership {
            room: id.clone(),
            user: ann.clone(),
        }],
    );
    assert!(
        app.dispatch(Action::TransferGroupOwnership {
            room: dm(Network::Stoat, "other", "someone").id,
            user: ann.clone(),
        })
        .is_empty()
    );
    // Blocking needs the shown profile's capability.
    assert!(app.dispatch(Action::BlockUser(ann.clone())).is_empty());
    let mut blockable = profile(&ann);
    blockable.can_block = true;
    app.apply(Event::Profile(blockable));
    assert_eq!(
        commands(&app.dispatch(Action::BlockUser(ann.clone()))),
        [&Command::BlockUser { user: ann.clone() }]
    );
    assert_eq!(
        commands(&app.dispatch(Action::UnblockUser(ann.clone()))),
        [&Command::UnblockUser { user: ann.clone() }]
    );
    // A note needs the shown profile's capability too.
    let note = |text: &str| Action::SetNote {
        user: ann.clone(),
        note: text.into(),
    };
    assert!(app.dispatch(note("hi")).is_empty());
    let mut noting = profile(&ann);
    noting.can_note = true;
    app.apply(Event::Profile(noting));
    assert_eq!(
        commands(&app.dispatch(note("hi"))),
        [&Command::SetNote {
            user: ann.clone(),
            note: "hi".into()
        }]
    );
    let bo = UserId::new(account(Network::Stoat), "bo");
    assert!(app.dispatch(Action::BlockUser(bo)).is_empty(), "only the shown profile");
    app.apply(Event::Blocked {
        account: account(Network::Stoat),
        users: vec![ann.clone()],
    });
    assert!(app.state().blocked.contains(&ann));
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
            viewers: None,
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
            folder: None,
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
fn emoji_changes_need_the_capability_and_valid_names() {
    use crate::server_settings::{EmojiFile, ServerAdmin, ServerAdminResult, ServerEmojis, ServerPage};
    let mut app = Application::default();
    let (server, request) = server_settings_open(&mut app);
    let mut o = overview(true);
    app.apply(Event::ServerOverview {
        server: server.clone(),
        request,
        result: Ok(o.clone()),
    });
    let create = |name: &str| Action::ServerAdmin {
        server: server.clone(),
        op: ServerAdmin::CreateEmoji {
            name: name.into(),
            file: EmojiFile {
                path: "/x/a.png".into(),
                size: 10,
            },
        },
    };
    // Without the capability nothing is sent.
    assert!(app.dispatch(create("ok")).is_empty());
    o.can_manage_emoji = true;
    let (server2, request2) = {
        app.dispatch(Action::CloseServerSettings);
        server_settings_open(&mut app)
    };
    app.apply(Event::ServerOverview {
        server: server2,
        request: request2,
        result: Ok(o),
    });
    let effects = app.dispatch(Action::ServerSettingsPage(ServerPage::Emoji));
    assert!(matches!(
        effects.as_slice(),
        [Effect::Backend {
            command: Command::ServerAdmin {
                op: ServerAdmin::LoadEmoji,
                ..
            },
            ..
        }]
    ));
    app.apply(Event::ServerAdmin {
        server: server.clone(),
        result: Ok(ServerAdminResult::Emoji(ServerEmojis::default())),
    });
    // A bad name is refused with a notice and isn't sent.
    assert!(app.dispatch(create("Bad Name")).is_empty());
    let s = app.state().server_settings.as_ref().unwrap();
    assert!(matches!(s.notice, Some(Err(_))));
    assert!(!s.admin_busy);
    assert_eq!(app.dispatch(create("good_name")).len(), 1);
    assert!(app.state().server_settings.as_ref().unwrap().admin_busy);
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

/// Two Stoat accounts that DM each other share the DM's raw id: each side's
/// draft, send and history stay on the account whose room is open.
#[test]
fn two_accounts_sharing_a_raw_dm_id_never_mix() {
    let mut app = Application::default();
    let (a, b) = (AccountId::new(Network::Stoat, "A"), AccountId::new(Network::Stoat, "B"));
    let side = |me: &AccountId, them: &str| Room {
        id: RoomId::new(me.clone(), "SHARED"),
        kind: RoomKind::Direct {
            with: UserId::new(me.clone(), them),
        },
        ..dm(Network::Stoat, "SHARED", them)
    };
    let (ra, rb) = (side(&a, "B"), side(&b, "A"));
    let (ia, ib) = (ra.id.clone(), rb.id.clone());
    for (account, room) in [(&a, ra), (&b, rb)] {
        app.apply(Event::AccountReady {
            account: account.clone(),
            display_name: "Me".into(),
            username: "me".into(),
            avatar_url: None,
            rooms: vec![room],
            servers: Vec::new(),
        });
    }
    assert_eq!(app.state().rooms.len(), 2);

    // A draft typed in A's side isn't B's.
    app.dispatch(Action::SelectRoom(ia.clone()));
    app.dispatch(Action::SetDraft("from A".into()));
    app.dispatch(Action::SelectRoom(ib.clone()));
    assert_eq!(app.state().drafts.get(&ib), None);

    // Sending from B's side goes to B's connection, B's room.
    app.dispatch(Action::SetDraft("from B".into()));
    let effects = app.dispatch(Action::Send { now_ms: 1 });
    let sent: Vec<_> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::Backend {
                account,
                command: Command::SendMessage { room, text, .. },
            } => Some((account.clone(), room.clone(), text.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(sent, [(b.clone(), ib.clone(), "from B".to_string())]);
    assert_eq!(app.state().drafts.get(&ia).map(String::as_str), Some("from A"));

    // History for A's side lands only there.
    app.apply(page(&ia, None, &["m1"], true));
    let has_m1 = |id: &RoomId| {
        app.state()
            .timelines
            .get(id)
            .is_some_and(|t| t.messages.iter().any(|m| &*m.id.0 == "m1"))
    };
    assert!(has_m1(&ia) && !has_m1(&ib));
}

/// A signed-in Stoat account with one channel it may manage (`C`) and one
/// it may not (`L`).
fn channel_settings_app() -> Application {
    let n = Network::Stoat;
    let mut app = Application::default();
    let kind = || RoomKind::Channel {
        server: ServerId::new(account(n), "S"),
    };
    let mut managed = room(n, "C", kind());
    managed.capabilities.manage_channel = true;
    let locked = room(n, "L", kind());
    ready(&mut app, n, vec![managed, locked]);
    app
}

#[test]
fn room_notify_settings_need_the_rooms_capability() {
    use crate::room::RoomNotifyChange as C;
    let n = Network::Stoat;
    let mut app = Application::default();
    let plain = room(n, "P", RoomKind::Group);
    let mut able = room(n, "A", RoomKind::Group);
    able.capabilities.notify_settings = true;
    ready(&mut app, n, vec![plain, able]);
    let change = C::Level(Some(9));
    let none = app.dispatch(Action::SetRoomNotify {
        room: RoomId::new(account(n), "P"),
        change,
    });
    assert!(none.is_empty());
    let effects = app.dispatch(Action::SetRoomNotify {
        room: RoomId::new(account(n), "A"),
        change,
    });
    assert!(matches!(
        commands(&effects).as_slice(),
        [Command::SetRoomNotify {
            change: C::Level(Some(2)),
            ..
        }]
    ));
}

fn open_channel_settings(app: &mut Application, can_manage: bool) -> RoomId {
    let room = RoomId::new(account(Network::Stoat), "C");
    let effects = app.dispatch(Action::OpenChannelSettings(room.clone()));
    let [
        Effect::Backend {
            command: Command::LoadChannelOverview { request, .. },
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("expected a load, got {effects:?}");
    };
    let request = *request;
    app.apply(Event::ChannelOverview {
        room: room.clone(),
        request,
        result: Ok(Some(crate::channel_settings::ChannelOverview {
            name: "general".into(),
            can_manage,
            ..Default::default()
        })),
    });
    room
}

#[test]
fn channel_settings_open_only_for_managed_channels_and_load_once() {
    let mut app = channel_settings_app();
    let n = Network::Stoat;
    assert!(
        app.dispatch(Action::OpenChannelSettings(RoomId::new(account(n), "L")))
            .is_empty()
    );
    assert!(app.state().channel_settings.is_none());
    let effects = app.dispatch(Action::OpenChannelSettings(RoomId::new(account(n), "C")));
    assert!(matches!(
        effects.as_slice(),
        [Effect::Backend {
            command: Command::LoadChannelOverview { .. },
            ..
        }]
    ));
    // Loading: another open waits for it.
    assert!(
        app.dispatch(Action::OpenChannelSettings(RoomId::new(account(n), "C")))
            .is_empty()
    );
    app.dispatch(Action::CloseChannelSettings);
    assert!(app.state().channel_settings.is_none());
}

#[test]
fn channel_settings_save_and_delete_are_gated_and_answered() {
    use crate::channel_settings::{ChannelOverview, ChannelOverviewEdit};
    let mut app = channel_settings_app();
    open_channel_settings(&mut app, false);
    let edit = ChannelOverviewEdit {
        name: Some("lobby".into()),
        ..Default::default()
    };
    assert!(
        app.dispatch(Action::SaveChannelOverview(edit.clone())).is_empty(),
        "no permission"
    );
    assert!(app.dispatch(Action::DeleteChannel).is_empty(), "no permission");

    app.dispatch(Action::CloseChannelSettings);
    let room = open_channel_settings(&mut app, true);
    // An empty edit is a no-op that counts as saved.
    assert!(app.dispatch(Action::SaveChannelOverview(Default::default())).is_empty());
    let effects = app.dispatch(Action::SaveChannelOverview(edit));
    let [
        Effect::Backend {
            command: Command::SaveChannelOverview { request, .. },
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("expected a save, got {effects:?}");
    };
    let request = *request;
    assert!(app.state().channel_settings.as_ref().unwrap().busy);
    // A stale answer is ignored; the right one shows the new overview.
    app.apply(Event::ChannelOverview {
        room: room.clone(),
        request: request.wrapping_sub(1),
        result: Err("old".into()),
    });
    assert!(app.state().channel_settings.as_ref().unwrap().busy);
    app.apply(Event::ChannelOverview {
        room: room.clone(),
        request,
        result: Ok(Some(ChannelOverview {
            name: "lobby".into(),
            can_manage: true,
            ..Default::default()
        })),
    });
    let s = app.state().channel_settings.as_ref().unwrap();
    assert!(!s.busy && s.saved);
    assert_eq!(&*s.overview.as_ref().unwrap().name, "lobby");

    // A failed delete keeps the page open with the error; a good one closes it.
    let effects = app.dispatch(Action::DeleteChannel);
    let [
        Effect::Backend {
            command: Command::DeleteChannel { request, .. },
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("expected a delete, got {effects:?}");
    };
    app.apply(Event::ChannelOverview {
        room: room.clone(),
        request: *request,
        result: Err("nope".into()),
    });
    let s = app.state().channel_settings.as_ref().unwrap();
    assert_eq!(s.error.as_deref(), Some("nope"));
    let effects = app.dispatch(Action::DeleteChannel);
    let [
        Effect::Backend {
            command: Command::DeleteChannel { request, .. },
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("expected a delete, got {effects:?}");
    };
    app.apply(Event::ChannelOverview {
        room,
        request: *request,
        result: Ok(None),
    });
    assert!(app.state().channel_settings.is_none());
}

#[test]
fn channel_settings_close_when_the_channel_goes() {
    let mut app = channel_settings_app();
    let room = open_channel_settings(&mut app, true);
    app.apply(Event::RoomRemoved(room));
    assert!(app.state().channel_settings.is_none());
}

#[test]
fn favourites_reach_only_networks_that_keep_them_and_follow_their_list() {
    let mut app = Application::default();
    ready(&mut app, Network::Stoat, vec![]);
    ready(&mut app, Network::Steam, vec![]);
    let on_steam = UserId::new(account(Network::Steam), "76561197960265729");
    let on_stoat = UserId::new(account(Network::Stoat), "ann");
    let signed_out = UserId::new(AccountId::new(Network::Steam, "gone"), "1");
    let star = |favourite| Action::SetFavourite {
        users: vec![on_steam.clone(), on_stoat.clone(), signed_out.clone()],
        favourite,
    };
    // Only the signed-in account whose network keeps favourites is told.
    assert_eq!(
        app.dispatch(star(true)),
        vec![Effect::Backend {
            account: account(Network::Steam),
            command: Command::SetFavourite {
                user: on_steam.clone(),
                favourite: true,
            },
        }]
    );
    assert!(app.state.favourites.contains(&on_steam));
    // Already starred: nothing to send.
    assert!(app.dispatch(star(true)).is_empty());
    // The network's own list replaces the account's stars.
    let other = UserId::new(account(Network::Steam), "76561197960265730");
    app.apply(Event::Favourites {
        account: account(Network::Steam),
        users: vec![other.clone()],
    });
    assert!(!app.state.favourites.contains(&on_steam));
    assert!(app.state.favourites.contains(&other));
    assert!(app.dispatch(star(false)).is_empty());
}

#[test]
fn games_in_common_loads_each_library_once_and_keeps_us_in() {
    let mut app = Application::new(Contacts::default());
    // Networks without libraries offer nothing.
    ready(&mut app, Network::Stoat, Vec::new());
    let stoat = account(Network::Stoat);
    assert!(
        app.dispatch(Action::OpenGamesInCommon {
            account: stoat,
            with: Vec::new()
        })
        .is_empty()
    );
    assert!(app.state().games.is_none());

    ready(&mut app, Network::Steam, Vec::new());
    let steam = account(Network::Steam);
    let me = UserId::new(steam.clone(), "me");
    let (a, b) = (UserId::new(steam.clone(), "a"), UserId::new(steam.clone(), "b"));
    let effects = app.dispatch(Action::OpenGamesInCommon {
        account: steam.clone(),
        with: vec![a.clone(), a.clone(), me.clone()],
    });
    assert_eq!(
        commands(&effects),
        [&Command::LoadGameLibraries {
            users: vec![me.clone(), a.clone()]
        }]
    );
    app.apply(Event::GameLibrary {
        user: a.clone(),
        library: crate::games::Library::Private,
    });
    let games = app.state().games.as_ref().expect("open");
    assert_eq!(games.libraries.get(&a), Some(&crate::games::Library::Private));
    // Picking someone new loads theirs; taking them out and back doesn't.
    let effects = app.dispatch(Action::PickForGames {
        user: b.clone(),
        picked: true,
    });
    assert_eq!(
        commands(&effects),
        [&Command::LoadGameLibraries { users: vec![b.clone()] }]
    );
    app.dispatch(Action::PickForGames {
        user: a.clone(),
        picked: false,
    });
    assert!(
        app.dispatch(Action::PickForGames {
            user: a.clone(),
            picked: true
        })
        .is_empty()
    );
    // We can't be taken out.
    app.dispatch(Action::PickForGames {
        user: me.clone(),
        picked: false,
    });
    assert_eq!(
        app.state().games.as_ref().map(|g| g.people.clone()),
        Some(vec![me, b, a])
    );
    assert!(app.state().showing_games);
    // Leaving the page frees the comparison.
    app.dispatch(Action::ShowFriends);
    assert!(app.state().games.is_none() && !app.state().showing_games);
}

#[test]
fn notifications_page_replaces_the_room_until_one_is_opened() {
    let (mut app, room) = open_dm(&["m1"]);
    app.dispatch(Action::ShowNotifications);
    let s = app.state();
    assert!(s.showing_notifications && !s.showing_active && !s.showing_people && !s.showing_friends);
    app.dispatch(Action::ShowActive);
    assert!(!app.state().showing_notifications && app.state().showing_active);
    app.dispatch(Action::ShowNotifications);
    app.dispatch(Action::SelectRoom(room.clone()));
    assert!(!app.state().showing_notifications);
    assert_eq!(app.state().selected.as_ref(), Some(&room));
    app.dispatch(Action::ShowNotifications);
    app.dispatch(Action::ShowFriends);
    assert!(!app.state().showing_notifications && app.state().showing_friends);
}

#[test]
fn games_page_replaces_the_room_and_goes_with_its_data() {
    let (mut app, room) = open_dm(&["m1"]);
    ready(&mut app, Network::Steam, Vec::new());
    let steam = account(Network::Steam);
    app.dispatch(Action::OpenGamesInCommon {
        account: steam.clone(),
        with: Vec::new(),
    });
    let s = app.state();
    assert!(s.showing_games && s.games.is_some());
    assert!(!s.showing_notifications && !s.showing_active && !s.showing_people && !s.showing_friends);
    app.dispatch(Action::ShowNotifications);
    assert!(!app.state().showing_games && app.state().games.is_none());
    app.dispatch(Action::OpenGamesInCommon {
        account: steam,
        with: Vec::new(),
    });
    app.dispatch(Action::SelectRoom(room.clone()));
    assert!(!app.state().showing_games && app.state().games.is_none());
    assert_eq!(app.state().selected.as_ref(), Some(&room));
}

#[test]
fn member_changes_need_the_open_card_and_its_powers() {
    use crate::members::{Moderation, ModerationRole};
    use crate::server_settings::{NICKNAME_MAX, ServerAdmin};
    use std::time::Duration;
    let mut app = Application::default();
    let (server, _) = server_settings_open(&mut app);
    // The card sends these without the settings' one-change-at-a-time wait.
    app.dispatch(Action::CloseServerSettings);
    let zed = UserId::new(account(Network::Stoat), "zed");
    let other = UserId::new(account(Network::Stoat), "other");
    let role = |id: &str| -> std::sync::Arc<str> { id.into() };
    let op_role = |user: &UserId, id: &str| Action::ServerAdmin {
        server: server.clone(),
        op: ServerAdmin::SetMemberRole {
            user: user.clone(),
            role: role(id),
            on: true,
        },
    };
    assert!(app.dispatch(op_role(&zed, "r1")).is_empty(), "no card is open");
    app.dispatch(Action::OpenProfile(zed.clone()));
    let card = |moderation: Moderation| Profile {
        user: zed.clone(),
        name: "Zed".into(),
        username: "zed".into(),
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
        moderation: Some(moderation),
        group_moderation: None,
        can_block: false,
        can_report: false,
        mutual_servers: Vec::new(),
        mutual_friends: Vec::new(),
        note: None,
        can_note: false,
        note_limit: 0,
    };
    let mut powers = Moderation::new(server.clone());
    app.apply(Event::Profile(card(powers.clone())));
    assert!(app.dispatch(op_role(&zed, "r1")).is_empty(), "no roles offered");
    powers.roles = vec![ModerationRole {
        id: role("r1"),
        name: "Helpers".into(),
        colour: None,
        assigned: false,
    }];
    powers.nickname = true;
    app.apply(Event::Profile(card(powers)));
    assert_eq!(app.dispatch(op_role(&zed, "r1")).len(), 1);
    assert!(
        app.dispatch(op_role(&zed, "r9")).is_empty(),
        "only roles the card lists"
    );
    assert!(
        app.dispatch(op_role(&other, "r1")).is_empty(),
        "only the open card's member"
    );
    let timeout = |secs: Option<u64>| Action::ServerAdmin {
        server: server.clone(),
        op: ServerAdmin::Timeout {
            user: zed.clone(),
            duration: secs.map(Duration::from_secs),
        },
    };
    assert!(app.dispatch(timeout(Some(60))).is_empty(), "no timeout power");
    let nick = |n: Option<&str>| Action::ServerAdmin {
        server: server.clone(),
        op: ServerAdmin::SetServerNickname {
            user: zed.clone(),
            nick: n.map(Into::into),
        },
    };
    assert_eq!(app.dispatch(nick(Some("Zedd"))).len(), 1);
    assert_eq!(app.dispatch(nick(None)).len(), 1, "reset");
    assert!(app.dispatch(nick(Some(&"x".repeat(NICKNAME_MAX + 1)))).is_empty());
    let mut powers = Moderation::new(server.clone());
    powers.timeout = true;
    app.apply(Event::Profile(card(powers)));
    assert_eq!(app.dispatch(timeout(Some(60))).len(), 1);
    assert_eq!(app.dispatch(timeout(None)).len(), 1, "lifting");
    assert!(app.dispatch(timeout(Some(29 * 86_400))).is_empty(), "28 days at most");
    assert!(app.dispatch(timeout(Some(0))).is_empty());
}

/// A server `S` with a real category `K` (we may manage it) holding `C`, and
/// `rights` as the network gave them.
fn structured_server(app: &mut Application, rights: crate::server::ServerRights) -> ServerId {
    let n = Network::Stoat;
    let server = ServerId::new(account(n), "S");
    app.apply(Event::ServersChanged {
        account: account(n),
        servers: vec![crate::server::Server {
            id: server.clone(),
            name: "S".into(),
            icon_url: None,
            banner_url: None,
            categories: vec![
                Category {
                    id: None,
                    can_manage: false,
                    title: "".into(),
                    rooms: vec![RoomId::new(account(n), "L")],
                },
                Category {
                    id: Some("K".into()),
                    can_manage: true,
                    title: "Talk".into(),
                    rooms: vec![RoomId::new(account(n), "C")],
                },
                Category {
                    id: Some("J".into()),
                    can_manage: false,
                    title: "Locked".into(),
                    rooms: Vec::new(),
                },
            ],
            muted: false,
            folder: None,
            rights,
        }],
    });
    server
}

#[test]
fn creating_and_moving_channels_needs_the_servers_rights() {
    use crate::server_settings::{NewChannelKind, ServerAdmin};
    let mut app = channel_settings_app();
    let server = structured_server(
        &mut app,
        crate::server::ServerRights {
            create_channels: true,
            ..Default::default()
        },
    );
    let create = |kind, category: Option<&str>| Action::ServerAdmin {
        server: server.clone(),
        op: ServerAdmin::CreateChannel {
            name: "new".into(),
            kind,
            category: category.map(Into::into),
        },
    };
    let mv = |id: &str, category| Action::ServerAdmin {
        server: server.clone(),
        op: ServerAdmin::MoveChannel {
            id: id.into(),
            category,
            up: true,
        },
    };
    // Text and voice channels, also inside a category of the server.
    assert_eq!(app.dispatch(create(NewChannelKind::Text, None)).len(), 1);
    assert_eq!(app.dispatch(create(NewChannelKind::Voice, Some("K"))).len(), 1);
    // Not inside a category the server doesn't have.
    assert!(app.dispatch(create(NewChannelKind::Text, Some("nope"))).is_empty());
    // Nor into one we can't manage.
    assert!(app.dispatch(create(NewChannelKind::Text, Some("J"))).is_empty());
    // Categories and moving need their own rights.
    assert!(app.dispatch(create(NewChannelKind::Category, None)).is_empty());
    assert!(app.dispatch(mv("C", false)).is_empty());
    assert!(app.dispatch(mv("K", true)).is_empty());

    structured_server(
        &mut app,
        crate::server::ServerRights {
            create_channels: true,
            manage_categories: true,
            reorder_channels: true,
            ..Default::default()
        },
    );
    assert_eq!(app.dispatch(create(NewChannelKind::Category, None)).len(), 1);
    // A category can't go inside another.
    assert!(app.dispatch(create(NewChannelKind::Category, Some("K"))).is_empty());
    assert_eq!(app.dispatch(mv("C", false)).len(), 1);
    assert_eq!(app.dispatch(mv("K", true)).len(), 1);
    // Only things the server has.
    assert!(app.dispatch(mv("ghost", false)).is_empty());
    assert!(app.dispatch(mv("C", true)).is_empty());
}

#[test]
fn category_settings_open_only_for_categories_we_manage() {
    let mut app = channel_settings_app();
    let server = structured_server(&mut app, Default::default());
    let open = |category: &str| Action::OpenCategorySettings {
        server: server.clone(),
        category: category.into(),
    };
    assert!(app.dispatch(open("ghost")).is_empty());
    let effects = app.dispatch(open("K"));
    let [
        Effect::Backend {
            command: Command::LoadChannelOverview { room, request },
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("expected a load, got {effects:?}");
    };
    assert_eq!(&*room.raw, "K");
    // A category takes a name only: the page's other fields are refused.
    app.apply(Event::ChannelOverview {
        room: room.clone(),
        request: *request,
        result: Ok(Some(crate::channel_settings::ChannelOverview {
            name: "Talk".into(),
            can_manage: true,
            is_category: true,
            ..Default::default()
        })),
    });
    let mature = crate::channel_settings::ChannelOverviewEdit {
        mature: Some(true),
        ..Default::default()
    };
    assert!(app.dispatch(Action::SaveChannelOverview(mature)).is_empty());
    assert!(app.state().channel_settings.as_ref().is_some_and(|s| s.error.is_some()));
    let rename = crate::channel_settings::ChannelOverviewEdit {
        name: Some("Chat".into()),
        ..Default::default()
    };
    assert_eq!(app.dispatch(Action::SaveChannelOverview(rename)).len(), 1);
}

#[test]
fn slowmode_and_icons_only_go_out_where_the_network_has_them() {
    use crate::channel_settings::{ChannelOverview, ChannelOverviewEdit};
    use crate::own_profile::ProfileImage;
    let slow = ChannelOverviewEdit {
        slowmode: Some(30),
        ..Default::default()
    };
    // No slowmode on this channel: refused, with a reason.
    let mut app = channel_settings_app();
    open_channel_settings(&mut app, true);
    assert!(app.dispatch(Action::SaveChannelOverview(slow.clone())).is_empty());
    assert!(app.state().channel_settings.as_ref().is_some_and(|s| s.error.is_some()));
    let icon = ChannelOverviewEdit {
        icon: ProfileImage::Remove,
        ..Default::default()
    };
    assert!(app.dispatch(Action::SaveChannelOverview(icon)).is_empty());
    // With slowmode (and no icons) offered, the change goes out.
    let mut app = channel_settings_app();
    let room = RoomId::new(account(Network::Stoat), "C");
    let effects = app.dispatch(Action::OpenChannelSettings(room.clone()));
    let [
        Effect::Backend {
            command: Command::LoadChannelOverview { request, .. },
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("expected a load, got {effects:?}");
    };
    app.apply(Event::ChannelOverview {
        room,
        request: *request,
        result: Ok(Some(ChannelOverview {
            name: "general".into(),
            can_manage: true,
            slowmode: Some(0),
            ..Default::default()
        })),
    });
    assert_eq!(app.dispatch(Action::SaveChannelOverview(slow)).len(), 1);
}

fn overrides_page(app: &mut Application, can_edit: bool, grantable: u64) -> RoomId {
    use crate::channel_settings::{ChannelOverride, ChannelOverrides, ChannelOverview, OverrideChoice, OverrideTarget};
    use crate::server_settings::PermissionInfo;
    let room = RoomId::new(account(Network::Stoat), "C");
    let effects = app.dispatch(Action::OpenChannelSettings(room.clone()));
    let [
        Effect::Backend {
            command: Command::LoadChannelOverview { request, .. },
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("expected a load, got {effects:?}");
    };
    let perm = |bit| PermissionInfo {
        bit,
        heading: None,
        title: "p".into(),
        description: "".into(),
    };
    let target = |id: &str| OverrideTarget {
        id: id.into(),
        member: false,
    };
    app.apply(Event::ChannelOverview {
        room: room.clone(),
        request: *request,
        result: Ok(Some(ChannelOverview {
            name: "general".into(),
            can_manage: true,
            overrides: Some(ChannelOverrides {
                entries: vec![
                    ChannelOverride {
                        target: target("E"),
                        name: "@everyone".into(),
                        colour: None,
                        everyone: true,
                        allow: 0,
                        deny: 0,
                    },
                    ChannelOverride {
                        target: target("R"),
                        name: "Mods".into(),
                        colour: None,
                        everyone: false,
                        allow: 1,
                        deny: 0,
                    },
                ],
                permissions: vec![perm(1), perm(2), perm(4)].into(),
                choices: vec![OverrideChoice {
                    target: target("S"),
                    name: "Subs".into(),
                }],
                can_edit,
                grantable,
            }),
            ..Default::default()
        })),
    });
    room
}

#[test]
fn channel_overrides_go_out_only_for_listed_targets_and_grantable_bits() {
    use crate::channel_settings::OverrideTarget;
    let target = |id: &str| OverrideTarget {
        id: id.into(),
        member: false,
    };
    let set = |id: &str, allow, deny| Action::SetChannelOverride {
        target: target(id),
        allow,
        deny,
    };
    // Without Manage Permissions nothing goes out, removing included.
    let mut app = channel_settings_app();
    overrides_page(&mut app, false, u64::MAX);
    assert!(app.dispatch(set("R", 2, 0)).is_empty());
    assert!(app.dispatch(Action::DeleteChannelOverride(target("R"))).is_empty());
    // With it: a listed role or a choice, listed bits that don't clash.
    let mut app = channel_settings_app();
    overrides_page(&mut app, true, 0b011);
    assert!(app.dispatch(set("ghost", 2, 0)).is_empty(), "not a role of this server");
    assert!(
        app.dispatch(set("R", 8, 0)).is_empty(),
        "not a permission of this channel"
    );
    assert!(app.dispatch(set("R", 3, 2)).is_empty(), "allow and deny clash");
    assert!(app.dispatch(set("R", 1 | 4, 0)).is_empty(), "4 isn't ours to give");
    assert!(app.dispatch(set("R", 1, 4)).is_empty(), "nor to deny");
    // The change that keeps a bit we can't give as it was is fine.
    let effects = app.dispatch(set("R", 1, 2));
    let [
        Effect::Backend {
            command: Command::SetChannelOverride { change, target: t, .. },
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("expected a set, got {effects:?}");
    };
    assert_eq!((*change, &*t.id), (Some((1, 2)), "R"));
    // The page waits for the answer before anything else goes out.
    assert!(app.dispatch(set("S", 1, 0)).is_empty());
}

#[test]
fn channel_overrides_remove_only_existing_entries_but_never_everyone() {
    use crate::channel_settings::OverrideTarget;
    let target = |id: &str| OverrideTarget {
        id: id.into(),
        member: false,
    };
    let mut app = channel_settings_app();
    overrides_page(&mut app, true, u64::MAX);
    assert!(app.dispatch(Action::DeleteChannelOverride(target("E"))).is_empty());
    assert!(app.dispatch(Action::DeleteChannelOverride(target("S"))).is_empty());
    let effects = app.dispatch(Action::DeleteChannelOverride(target("R")));
    assert!(matches!(
        effects.as_slice(),
        [Effect::Backend {
            command: Command::SetChannelOverride { change: None, .. },
            ..
        }]
    ));
}

#[test]
fn channel_overrides_changed_elsewhere_refresh_the_open_page() {
    let mut app = channel_settings_app();
    let room = overrides_page(&mut app, true, u64::MAX);
    let mut changed = app
        .state()
        .channel_settings
        .as_ref()
        .and_then(|s| s.overview.as_ref())
        .and_then(|o| o.overrides.clone())
        .unwrap();
    changed.entries[1].deny = 2;
    app.apply(Event::ChannelOverrides {
        room,
        overrides: changed,
    });
    let o = app
        .state()
        .channel_settings
        .as_ref()
        .and_then(|s| s.overview.as_ref())
        .unwrap();
    assert_eq!(o.overrides.as_ref().map(|o| o.entries[1].deny), Some(2));
    // Another channel's change is ignored.
    let mut other = o.overrides.clone().unwrap();
    other.entries[1].deny = 4;
    app.apply(Event::ChannelOverrides {
        room: RoomId::new(account(Network::Stoat), "other"),
        overrides: other,
    });
    let o = app
        .state()
        .channel_settings
        .as_ref()
        .and_then(|s| s.overview.as_ref())
        .unwrap();
    assert_eq!(o.overrides.as_ref().map(|o| o.entries[1].deny), Some(2));
}

/// Two DMs of one account (`from` forwards out, `to` takes forwards) and the
/// first one open with a message, a system notice and a pending one loaded.
fn forwarding() -> (Application, RoomId, RoomId) {
    let mut app = Application::default();
    let mut from = dm(Network::Stoat, "r1", "them");
    from.capabilities.forward_from = true;
    let mut to = dm(Network::Stoat, "r2", "other");
    to.capabilities.forward_to = true;
    let (a, b) = (from.id.clone(), to.id.clone());
    ready(&mut app, Network::Stoat, vec![from, to]);
    app.dispatch(Action::SelectRoom(a.clone()));
    let mut notice = msg("sys");
    notice.system = true;
    notice.forwardable = false;
    let mut sending = msg("wait");
    sending.pending = true;
    sending.forwardable = true;
    app.apply(Event::History {
        room: a.clone(),
        before: None,
        messages: vec![forwardable(msg("m1")), notice, forwardable(sending)],
        reached_start: true,
    });
    (app, a, b)
}

fn forwardable(mut m: Message) -> Message {
    m.forwardable = true;
    m
}

fn forward(message: &str, to: &RoomId) -> Action {
    Action::ForwardMessage {
        room: RoomId::new(account(Network::Stoat), "r1"),
        message: MessageId(message.into()),
        to: to.clone(),
        comment: "  look  ".into(),
    }
}

#[test]
fn forwarding_goes_to_the_account_with_a_trimmed_comment() {
    let (mut app, from, to) = forwarding();
    assert_eq!(
        commands(&app.dispatch(forward("m1", &to))),
        [&Command::ForwardMessage {
            room: from,
            message: MessageId("m1".into()),
            to,
            comment: "look".into()
        }]
    );
}

#[test]
fn forwarding_needs_both_capabilities_the_same_account_and_a_real_message() {
    let (mut app, from, to) = forwarding();
    // Unknown, system and unconfirmed messages are refused.
    for id in ["nope", "sys", "wait"] {
        assert!(app.dispatch(forward(id, &to)).is_empty(), "{id}");
    }
    assert_eq!(app.state().errors.len(), 3, "each refusal is shown");
    // A destination that takes no forwards (the source room itself).
    assert!(app.dispatch(forward("m1", &from)).is_empty());
    // Another account's room.
    let other = RoomId::new(AccountId::new(Network::Discord, "elsewhere"), "r2");
    assert!(app.dispatch(forward("m1", &other)).is_empty());
    // A source room that doesn't allow forwarding out.
    let (mut app, _, to) = forwarding();
    let mut shut = dm(Network::Stoat, "r1", "them");
    shut.capabilities.forward_from = false;
    app.apply(Event::RoomUpdated(shut));
    assert!(app.dispatch(forward("m1", &to)).is_empty());
}

#[test]
fn forward_destinations_are_the_accounts_rooms_that_take_forwards() {
    let mut app = Application::default();
    let mut rooms = Vec::new();
    for (raw, to, activity) in [("a", true, 1), ("b", true, 9), ("c", false, 5), ("ab", true, 0)] {
        let mut r = dm(Network::Stoat, raw, "x");
        r.capabilities.forward_to = to;
        r.last_activity = activity;
        rooms.push(r);
    }
    let from = rooms[2].id.clone();
    ready(&mut app, Network::Stoat, rooms);
    let names = |q: &str, n: usize| {
        app.state()
            .forward_destinations(&from, q, n)
            .iter()
            .map(|r| r.name.to_string())
            .collect::<Vec<_>>()
    };
    // Not the room that takes none; the most recent first; capped.
    assert_eq!(names("", 9), ["b", "a", "ab"]);
    assert_eq!(names("", 1), ["b"]);
    // Names that start with the search come first.
    assert_eq!(names("b", 9), ["b", "ab"]);
    assert!(names("zzz", 9).is_empty());
}

/// A DM that takes polls, open, with our open poll "p1" and someone's "p2".
fn polling(send: bool, polls: bool) -> (Application, RoomId) {
    let mut app = Application::default();
    let mut r = dm(Network::Stoat, "r1", "them");
    r.capabilities.send = send;
    r.capabilities.polls = polls.then_some(crate::poll::PollLimits {
        question: 300,
        answer: 55,
        min_answers: 2,
        max_answers: 10,
    });
    let id = r.id.clone();
    ready(&mut app, Network::Stoat, vec![r]);
    app.dispatch(Action::SelectRoom(id.clone()));
    let mut mine = msg("p1");
    mine.poll_endable = true;
    app.apply(Event::History {
        room: id.clone(),
        before: None,
        messages: vec![mine, msg("p2")],
        reached_start: true,
    });
    (app, id)
}

fn new_poll(question: &str, answers: &[&str], hours: u32) -> Action {
    Action::CreatePoll {
        question: question.into(),
        answers: answers.iter().map(|a| (*a).to_owned()).collect(),
        hours,
        multi: true,
    }
}

#[test]
fn creating_a_poll_is_checked_and_trimmed() {
    let (mut app, room) = polling(true, true);
    assert_eq!(
        commands(&app.dispatch(new_poll(" Lunch? ", &["Pizza ", "", "Soup"], 24))),
        [&Command::CreatePoll {
            room,
            question: "Lunch?".into(),
            answers: vec!["Pizza".into(), "Soup".into()],
            hours: 24,
            multi: true,
        }]
    );
    // Too few answers, a blank question, an unoffered duration.
    assert!(app.dispatch(new_poll("Lunch?", &["Pizza"], 24)).is_empty());
    assert!(app.dispatch(new_poll(" ", &["a", "b"], 24)).is_empty());
    assert!(app.dispatch(new_poll("Lunch?", &["a", "b"], 5)).is_empty());
    assert_eq!(app.state().errors.len(), 3, "each refusal is shown");
}

#[test]
fn polls_need_the_capability_and_permission_to_send() {
    for (send, polls) in [(true, false), (false, true)] {
        let (mut app, _) = polling(send, polls);
        assert!(app.dispatch(new_poll("Lunch?", &["a", "b"], 24)).is_empty());
        assert_eq!(app.state().errors.len(), 1);
    }
}

#[test]
fn only_our_open_polls_can_be_ended() {
    let (mut app, room) = polling(true, true);
    assert_eq!(
        commands(&app.dispatch(Action::EndPoll(MessageId("p1".into())))),
        [&Command::EndPoll {
            room,
            message: MessageId("p1".into())
        }]
    );
    assert!(app.dispatch(Action::EndPoll(MessageId("p2".into()))).is_empty());
    assert!(app.dispatch(Action::EndPoll(MessageId("nope".into()))).is_empty());
}

#[test]
fn a_forum_loads_its_posts_and_a_required_tag_gates_new_posts() {
    use crate::forum::{ForumInfo, ForumPage, ForumSort, ForumTag};
    let mut r = dm(Network::Discord, "f1", "them");
    r.capabilities.forum = true;
    r.capabilities.threads = true;
    let id = r.id.clone();
    let mut app = Application::default();
    ready(&mut app, Network::Discord, vec![r]);
    let effects = app.dispatch(Action::SelectRoom(id.clone()));
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::Backend {
                command: Command::LoadForum { offset: None, .. },
                ..
            }
        )),
        "opening a forum asks for its posts"
    );
    let tag = |id: &str, moderated| ForumTag {
        id: id.into(),
        name: id.into(),
        emoji: None,
        moderated,
    };
    app.apply(Event::Forum(Box::new(ForumPage {
        room: id.clone(),
        info: Some(ForumInfo {
            tags: vec![tag("a", false), tag("m", true)],
            require_tag: true,
            default_sort: ForumSort::LatestActivity,
            moderate_tags: false,
        }),
        posts: Vec::new(),
        archived: false,
        replace_older: false,
        sort: ForumSort::LatestActivity,
        tag: None,
        next_offset: None,
    })));
    let post = |tags: &[&str]| Action::CreateThread {
        name: "Title".into(),
        message: None,
        text: "Body".into(),
        tags: tags.iter().map(|t| Arc::<str>::from(*t)).collect(),
    };
    assert!(app.dispatch(post(&[])).is_empty(), "a tag is required");
    assert!(
        app.dispatch(post(&["m"])).is_empty(),
        "a moderated tag isn't ours to give"
    );
    let effects = app.dispatch(post(&["a", "m"]));
    assert!(matches!(
        effects.as_slice(),
        [Effect::Backend { command: Command::CreateThread { tags, .. }, .. }] if tags.len() == 1
    ));
    // "Load older posts" asks from the start, once.
    let more = app.dispatch(Action::LoadMoreForum);
    assert!(matches!(
        more.as_slice(),
        [Effect::Backend {
            command: Command::LoadForum { offset: Some(0), .. },
            ..
        }]
    ));
    assert!(app.dispatch(Action::LoadMoreForum).is_empty(), "not while loading");
}

fn sticker(id: &str, available: bool) -> crate::stickers::Sticker {
    crate::stickers::Sticker {
        id: id.into(),
        name: id.into(),
        tags: "".into(),
        image_url: "".into(),
        available,
    }
}

#[test]
fn stickers_send_only_from_their_own_server_and_only_when_available() {
    let mut app = Application::default();
    let n = Network::Discord;
    let mut here = room(
        n,
        "c1",
        RoomKind::Channel {
            server: ServerId::new(account(n), "s1"),
        },
    );
    here.capabilities.stickers = true;
    let mut there = room(
        n,
        "c2",
        RoomKind::Channel {
            server: ServerId::new(account(n), "s2"),
        },
    );
    there.capabilities.stickers = true;
    ready(&mut app, n, vec![here, there, dm(n, "d", "them")]);
    app.apply(Event::Stickers {
        account: account(n),
        groups: vec![crate::stickers::StickerGroup {
            server: ServerId::new(account(n), "s1"),
            title: "One".into(),
            stickers: vec![sticker("ok", true), sticker("boost-lost", false)],
        }],
    });
    let sent = |app: &mut Application, room: &str, id: &str| {
        app.dispatch(Action::SelectRoom(RoomId::new(account(n), room)));
        let effects = app.dispatch(Action::SendSticker { sticker: id.into() });
        commands(&effects)
            .iter()
            .any(|c| matches!(c, Command::SendSticker { sticker, .. } if &**sticker == id))
    };
    assert!(sent(&mut app, "c1", "ok"));
    assert!(!sent(&mut app, "c1", "boost-lost"), "the server refuses it");
    assert!(!sent(&mut app, "c2", "ok"), "another server's sticker needs Nitro");
    assert!(!sent(&mut app, "d", "ok"), "no server stickers in DMs");
    assert!(app.state().stickers_for(&RoomId::new(account(n), "c2")).is_empty());
    assert!(app.state().stickers_for(&RoomId::new(account(n), "d")).is_empty());
    // Signing out forgets them.
    app.apply(Event::AccountLost {
        account: account(n),
        reason: "gone".into(),
    });
    assert!(app.state().stickers.is_empty());
}

#[test]
fn member_search_asks_only_where_the_room_can_and_drops_stale_answers() {
    let mut app = Application::new(Contacts::default());
    let server = ServerId::new(account(Network::Discord), "S");
    let mut ch = room(Network::Discord, "c1", RoomKind::Channel { server: server.clone() });
    ch.capabilities.member_search = true;
    let plain = room(Network::Stoat, "c2", RoomKind::Channel { server });
    let (ic, ip) = (ch.id.clone(), plain.id.clone());
    ready(&mut app, Network::Discord, vec![ch]);
    ready(&mut app, Network::Stoat, vec![plain]);
    let searches = |effects: &[Effect]| {
        commands(effects)
            .iter()
            .filter(|c| matches!(c, Command::SearchMembers { .. }))
            .count()
    };
    app.dispatch(Action::SelectRoom(ic.clone()));
    assert_eq!(
        searches(&app.dispatch(Action::SearchMembers { query: "  ".into() })),
        0,
        "nothing typed"
    );
    assert_eq!(searches(&app.dispatch(Action::SearchMembers { query: "al".into() })), 1);
    app.dispatch(Action::FilterMembers { query: "ali".into() });
    let found = |query: &str| Event::MemberSearch {
        room: ic.clone(),
        query: query.into(),
        members: Vec::new(),
    };
    app.apply(found("al"));
    assert!(
        app.state().member_found.is_none(),
        "typed on: the older answer is stale"
    );
    app.apply(found("ali"));
    assert!(app.state().member_found.is_some());
    app.dispatch(Action::SelectRoom(ip));
    assert_eq!(
        searches(&app.dispatch(Action::SearchMembers { query: "al".into() })),
        0,
        "capability off"
    );
}

#[test]
fn last_interactions_take_each_persons_newest_dm_and_calls_as_now() {
    let mut state = AppState::default();
    let mut a = dm(Network::Stoat, "d1", "ann");
    a.last_activity = 100;
    let mut a2 = dm(Network::Stoat, "d2", "ann");
    a2.last_activity = 300;
    let mut bob = dm(Network::Stoat, "d3", "bob");
    bob.last_activity = 200;
    // Never written in: not an interaction.
    let cat = dm(Network::Stoat, "d4", "cat");
    let call = dm(Network::Stoat, "d5", "dan");
    state.voice.insert(
        call.id.clone(),
        vec![CallMember {
            user: UserId::new(account(Network::Stoat), "dan"),
            name: "Dan".into(),
            avatar_url: None,
            speaking: false,
            muted: false,
            deafened: false,
        }],
    );
    state.rooms = [a, a2, bob, cat, call].into_iter().map(Arc::new).collect();
    let seen = state.last_interactions();
    let at = |who: &str| seen.get(&UserId::new(account(Network::Stoat), who)).copied();
    assert_eq!(at("ann"), Some(300));
    assert_eq!(at("bob"), Some(200));
    assert_eq!(at("cat"), None);
    assert_eq!(at("dan"), Some(i64::MAX));
}

#[test]
fn unloading_an_account_removes_all_of_it_and_only_it() {
    let mut app = Application::new(Contacts::default());
    let (s, t) = (Network::Steam, Network::Stoat);
    let rs = dm(s, "dm", "friend");
    let rt = dm(t, "dm", "friend");
    ready(&mut app, s, vec![rs.clone()]);
    ready(&mut app, t, vec![rt.clone()]);
    for r in [&rs, &rt] {
        app.state.drafts.insert(r.id.clone(), "hi".into());
        app.state
            .compose
            .insert(r.id.clone(), Compose::Reply(MessageId("m".into())));
        app.state.files.insert(r.id.clone(), Vec::new());
        app.state.voice.insert(r.id.clone(), Vec::new());
        app.state.marked_unread.push(r.id.clone());
        app.state.pinned_rooms.push(r.id.clone());
        app.state.hidden_rooms.push(r.id.clone());
        app.state.timelines.insert(r.id.clone(), Timeline::default());
        app.state
            .activities
            .insert(UserId::new(r.id.account.clone(), "friend"), "game".into());
    }
    app.state.games = Some(GamesInCommon {
        account: account(s),
        people: Vec::new(),
        libraries: HashMap::new(),
    });
    app.state.pins = Some(Pins {
        room: rs.id.clone(),
        messages: None,
        failed: false,
    });
    app.unload_account(&account(s));
    let st = app.state();
    assert_eq!(st.accounts.len(), 1);
    assert!(st.accounts.iter().all(|a| a.id == account(t)));
    assert!(st.rooms.iter().all(|r| r.id.account == account(t)));
    assert!(st.games.is_none() && st.pins.is_none());
    assert_eq!(st.drafts.len(), 1);
    assert_eq!(st.compose.len(), 1);
    assert_eq!(st.files.len(), 1);
    assert_eq!(st.voice.len(), 1);
    assert_eq!(st.marked_unread, vec![rt.id.clone()]);
    assert_eq!(st.pinned_rooms, vec![rt.id.clone()]);
    assert_eq!(st.hidden_rooms, vec![rt.id.clone()]);
    assert_eq!(st.timelines.len(), 1);
    assert_eq!(st.activities.len(), 1);
    assert!(st.errors.is_empty(), "unloading is no sign-out error");
}

#[test]
fn unloading_resets_a_selection_on_that_account_only() {
    let mut app = Application::new(Contacts::default());
    let rs = dm(Network::Steam, "dm", "friend");
    let rt = dm(Network::Stoat, "dm", "friend");
    ready(&mut app, Network::Steam, vec![rs.clone()]);
    ready(&mut app, Network::Stoat, vec![rt.clone()]);
    app.dispatch(Action::SelectRoom(rt.id.clone()));
    app.unload_account(&account(Network::Steam));
    assert_eq!(app.state().selected, Some(rt.id.clone()));
    app.dispatch(Action::SelectRoom(rs.id.clone()));
    app.state.selected = Some(rs.id);
    app.unload_account(&account(Network::Steam));
    assert_eq!(app.state().selected, None);
}

#[test]
fn set_contacts_swaps_the_merges() {
    let mut app = Application::new(Contacts::default());
    let mut other = Contacts::default();
    other.ensure(UserId::new(account(Network::Stoat), "01A"), "Ann");
    app.set_contacts(other.clone());
    assert_eq!(app.state().contacts, other);
}
