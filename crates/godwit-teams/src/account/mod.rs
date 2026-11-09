// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A Teams account as the core sees it: the chat service's conversation
//! list and the aggregator's teams into rooms and servers, its messages
//! (HTML) into Godwit's, Trouter's news into events, and the core's
//! commands into requests. First version: 1:1 and group chats and team
//! channels (flat, replies shown as replies to their post), sending,
//! editing, deleting, reactions, files, read state and typing. Calls are not
//! available yet.

mod commands;
mod events;
mod messages;
mod rooms;

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use godwit_core::account::NetworkAccount;
use godwit_core::application::{Command, Event};
use godwit_core::ids::{AccountId, MessageId, RoomId, ServerId, UserId};
use godwit_core::members::Presence;
use godwit_core::message::{Markup, Message, Reaction};
use godwit_core::network::Network;
use godwit_core::room::{Capabilities, Room, RoomEncryption, RoomKind};
use godwit_core::server::{Category, Server, ServerRights};
use serde_json::Value;

use crate::api::PAGE;
use crate::backend::{self, Answer, Cmd, Ev, Handle, Request};
use crate::{html, session};

/// Where signing in stands, for the sign-in screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    SignedOut {
        error: Option<String>,
    },
    /// Asking Microsoft for a code.
    Starting,
    /// Waiting for the user to enter `code` at `url`.
    Code {
        code: String,
        url: String,
    },
    Connecting,
    Ready,
}

/// Typing notices go at most this often per conversation (the web client
/// repeats about every 20 s while typing).
const TYPING_EVERY: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Kind {
    #[default]
    Group,
    /// With this person's id.
    Direct(String),
    /// In this team.
    Channel(String),
}

#[derive(Clone, Debug, Default)]
struct Conversation {
    id: String,
    kind: Kind,
    /// The chat's or channel's name, if it has one.
    topic: String,
    members: Vec<String>,
    last_ms: i64,
    /// The newest message: its id, client id and author.
    last: Option<(String, String, String)>,
    /// Read up to this message id (ids are arrival times in ms).
    read: i64,
    hidden: bool,
    parent: Option<String>,
    participating: bool,
}

#[derive(Clone, Debug, Default)]
struct Team {
    id: String,
    name: String,
    channels: Vec<(String, String)>,
}

pub struct TeamsAccount {
    /// The rooms last sent to the app.
    room_sync: godwit_core::room_sync::RoomSync,
    slot: u32,
    net: Handle,
    events: mpsc::Receiver<Ev>,
    status: Status,
    account: Option<AccountId>,
    me: String,
    my_name: String,
    own_presence: Presence,
    own_status: Option<String>,
    conversations: HashMap<String, Conversation>,
    teams: Vec<Team>,
    names: HashMap<String, String>,
    /// People's custom status text (`presence.note.message`), as DM
    /// subtitles: the availability dots ride on `Event::Presences`.
    statuses: HashMap<String, String>,
    /// Channel threads we took part in (we wrote there): their replies
    /// alert, like Slack's subscribed threads.
    threads_mine: HashSet<String>,
    /// Names asked for, so each is asked once.
    asked: HashSet<String>,
    /// Our sends: client message id → the core's nonce.
    sending: HashMap<String, String>,
    /// Messages already confirmed from the send's answer, so their echo
    /// over Trouter isn't added again.
    confirmed: HashSet<String>,
    /// What each send said (client message id → text), for its confirmation.
    pending: HashMap<String, String>,
    /// Who is typing per conversation, and when they last said so: a name
    /// runs out after core's `TYPING_FOR` when no ClearTyping comes.
    typing: HashMap<String, Vec<(String, std::time::Instant)>>,
    typing_sent: HashMap<String, Instant>,
    /// Keep the sign-in in the keychain (off in tests).
    persist: bool,
    media: Option<godwit_core::media::Media>,
    media_region: String,
    /// Authors met without a name since names were last asked for.
    unnamed: HashSet<String>,
}

/// A contact address's id: `…/contacts/8:orgid:…` → `8:orgid:…`.
fn mri(s: &str) -> Option<String> {
    s.rsplit('/')
        .next()
        .filter(|last| last.contains(':'))
        .map(str::to_owned)
}

/// A conversation id and, for a channel reply, its post: `{id};messageid={root}`.
fn split_thread(id: &str) -> (String, Option<String>) {
    match id.split_once(";messageid=") {
        Some((bare, root)) if !root.is_empty() => (bare.to_owned(), Some(root.to_owned())),
        Some((bare, _)) => (bare.to_owned(), None),
        None => (id.to_owned(), None),
    }
}

/// The conversation a message belongs to, from `conversationLink` (a URL
/// ending in `/conversations/{id}`) or `conversationid`/`to`.
fn conversation_of(m: &Value) -> Option<(String, Option<String>)> {
    if let Some(link) = m["conversationLink"].as_str()
        && let Some((_, rest)) = link.split_once("/conversations/")
    {
        let id = rest.split(['/', '?']).next().unwrap_or_default();
        if !id.is_empty() {
            return Some(split_thread(&godwit_core::rich::percent_decode(id)));
        }
    }
    let id = m["conversationid"].as_str().or_else(|| m["to"].as_str())?;
    Some(split_thread(id))
}

fn ms(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn iso_ms(v: &Value) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(v.as_str()?)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// A property that's set: not missing, null or an empty string.
fn is_set(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn json_list(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::String(s) => serde_json::from_str(s).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Teams' reaction keys (emoticon ids) as emoji.
fn reaction_emoji(key: &str) -> String {
    crate::emoji::for_key(key).unwrap_or_else(|| key.to_owned())
}

impl TeamsAccount {
    /// Starts the network thread; with a kept sign-in in `slot`, connects.
    pub fn new(slot: u32, wake: backend::Waker) -> std::io::Result<Self> {
        let (net, events) = backend::spawn(wake)?;
        let saved = session::load(slot);
        let status = if saved.is_some() {
            Status::Connecting
        } else {
            Status::SignedOut { error: None }
        };
        if let Some(saved) = saved {
            net.send(Cmd::UseSession(saved));
        }
        Ok(Self::with(slot, net, events, status, true))
    }

    fn with(slot: u32, net: Handle, events: mpsc::Receiver<Ev>, status: Status, persist: bool) -> Self {
        Self {
            room_sync: Default::default(),
            slot,
            net,
            events,
            status,
            account: None,
            me: String::new(),
            my_name: String::new(),
            own_presence: Presence::Unknown,
            own_status: None,
            statuses: HashMap::new(),
            threads_mine: HashSet::new(),
            conversations: HashMap::new(),
            teams: Vec::new(),
            names: HashMap::new(),
            asked: HashSet::new(),
            unnamed: HashSet::new(),
            sending: HashMap::new(),
            confirmed: HashSet::new(),
            pending: HashMap::new(),
            typing: HashMap::new(),
            typing_sent: HashMap::new(),
            persist,
            media: None,
            media_region: "emea".into(),
        }
    }

    fn picture(&self, user: &str) -> Option<String> {
        if user.is_empty() || self.me.is_empty() {
            return None;
        }
        let own = self.me.strip_prefix("8:orgid:").unwrap_or(&self.me);
        let url = format!(
            "https://teams.microsoft.com/api/mt/{}/beta/users/{}/profilepicturev2/{}?size=HR128x128",
            crate::api::seg(&self.media_region),
            crate::api::seg(own),
            crate::api::seg(user)
        );
        self.media.as_ref()?.address("", &url)
    }

    pub fn slot(&self) -> u32 {
        self.slot
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Starts signing in with a device code.
    pub fn sign_in(&mut self) {
        if matches!(self.status, Status::SignedOut { .. }) {
            self.status = Status::Starting;
            self.net.send(Cmd::SignIn);
        }
    }

    pub fn cancel_sign_in(&mut self) {
        if matches!(self.status, Status::Starting | Status::Code { .. }) {
            self.net.send(Cmd::CancelSignIn);
            self.status = Status::SignedOut { error: None };
        }
    }

    fn room_id(&self, conversation: &str) -> Option<RoomId> {
        Some(RoomId::new(self.account.clone()?, conversation))
    }

    fn name(&self, who: &str) -> String {
        if who == self.me && !self.my_name.is_empty() {
            return self.my_name.clone();
        }
        self.names.get(who).cloned().unwrap_or_else(|| "Someone".into())
    }
}

impl NetworkAccount for TeamsAccount {
    fn network(&self) -> Network {
        Network::Teams
    }

    fn id(&self) -> Option<&AccountId> {
        self.account.as_ref()
    }

    fn connecting(&self) -> bool {
        self.id().is_none() && matches!(self.status(), Status::Connecting)
    }

    fn poll(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        godwit_core::backend::poll(self, |s| s.events.try_recv().ok(), Self::handle, &mut out);
        out
    }

    fn run(&mut self, command: Command) -> Vec<Event> {
        self.run_command(command)
    }

    /// Teams' deep link to a message (as its "Copy link" makes).
    fn message_link(&self, room: &RoomId, message: &MessageId) -> Option<String> {
        let c = self.conversations.get(room.raw.as_ref())?;
        Some(format!(
            "https://teams.microsoft.com/l/message/{}/{}",
            crate::api::seg(&c.id),
            message.0
        ))
    }

    fn media(&self) -> Option<(String, godwit_core::account::MediaFetch)> {
        self.media.as_ref().map(|media| media.registration())
    }

    fn log_out(&mut self) {
        if let Some(media) = &self.media {
            media.clear();
        }
        self.net.send(Cmd::Logout);
    }
}

impl Drop for TeamsAccount {
    fn drop(&mut self) {
        if let Some(media) = &self.media {
            media.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ME: &str = "8:orgid:aaaaaaaa-1111-1111-1111-111111111111";
    const BEA: &str = "8:orgid:bbbbbbbb-2222-2222-2222-222222222222";
    const DM: &str = "19:aaaaaaaa-1111-1111-1111-111111111111_bbbbbbbb-2222-2222-2222-222222222222@unq.gbl.spaces";
    const GROUP: &str = "19:group@thread.v2";
    const CHANNEL: &str = "19:chan@thread.tacv2";

    fn account() -> (
        TeamsAccount,
        tokio::sync::mpsc::UnboundedReceiver<Cmd>,
        mpsc::Sender<Ev>,
    ) {
        let (net, cmds) = Handle::detached();
        let (tx, events) = mpsc::channel();
        (
            TeamsAccount::with(u32::MAX, net, events, Status::Connecting, false),
            cmds,
            tx,
        )
    }

    /// `GET /v1/users/ME/properties` and the conversation list, as the chat service shapes them.
    fn ready(a: &mut TeamsAccount) -> Vec<Event> {
        let me = json!({"skypeName": "orgid:aaaaaaaa-1111-1111-1111-111111111111",
                        "userDetails": "{\"name\":\"Ada Example\",\"upn\":\"ada@example.com\"}"});
        let conversations = json!({"conversations": [
            {"id": DM, "threadProperties": {"uniquerosterthread": "true"},
             "properties": {"consumptionhorizon": "1760346000000;1760346000001;1"},
             "lastMessage": {"id": "1760346100000", "from": format!("https://emea.ng.msg.teams.microsoft.com/v1/users/ME/contacts/{BEA}"),
                             "imdisplayname": "Bea Example", "originalarrivaltime": "2025-10-13T09:01:40.000Z"}},
            {"id": GROUP, "threadProperties": {"topic": "Planning"}, "properties": {},
             "lastMessage": {"id": "1760346000000", "from": format!("…/contacts/{ME}")}},
            {"id": CHANNEL, "threadProperties": {"spaceId": "19:team@thread.tacv2", "topic": "General"}, "properties": {}},
            {"id": "48:notes", "properties": {}}
        ]});
        let mut out = Vec::new();
        a.handle(Ev::Ready { me, conversations }, &mut out);
        out
    }

    #[test]
    fn own_and_other_presence_updates_and_failed_writes_restore_confirmed_state() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        let requests: Vec<_> = std::iter::from_fn(|| cmds.try_recv().ok()).collect();
        assert!(requests.iter().any(|c| matches!(c, Cmd::Request(Request::Presence(ids)) if ids.contains(&ME.to_owned()) && ids.contains(&BEA.to_owned()))));
        let mut out = Vec::new();
        a.handle(Ev::News(json!({"resourceType":"Presence","resource":{"presence":[{"mri":ME,"presence":{"availability":"Busy"}},{"mri":BEA,"presence":{"availability":"Away"}}]}})), &mut out);
        assert!(out.iter().any(|e| matches!(
            e,
            Event::OwnPresence {
                presence: Presence::Busy,
                ..
            }
        )));
        assert!(out.iter().any(|e| matches!(e, Event::Presences { presence, .. } if presence.iter().any(|(id, p)| &*id.raw == BEA && *p == Presence::Idle))));
        a.run(Command::SetPresence {
            presence: Presence::Focus,
            choice: 2,
        });
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Request(Request::SetPresence { presence:Presence::Focus, user })) if user == ME)
        );
        a.run(Command::SetStatusText(Some("Reviewing".into())));
        assert!(matches!(cmds.try_recv(), Ok(Cmd::Request(Request::SetStatus(Some(text)))) if text == "Reviewing"));
        a.answer(
            Answer::OwnStatus {
                status: Some("Reviewing".into()),
                result: Ok(()),
            },
            &mut out,
        );
        out.clear();
        a.answer(Answer::OwnPresence(Err("unreachable".into())), &mut out);
        assert!(out.iter().any(
            |e| matches!(e, Event::OwnPresence { presence:Presence::Busy, status:Some(s), .. } if &**s == "Reviewing")
        ));
    }

    #[test]
    fn channel_threads_open_load_send_and_receive_in_their_own_room() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        a.handle(Ev::Teams(json!({"teams":[{"id":"19:team@thread.tacv2","displayName":"Design","channels":[{"id":CHANNEL,"displayName":"General"}]}]})), &mut Vec::new());
        while cmds.try_recv().is_ok() {}
        let parent = a.room(CHANNEL).unwrap();
        assert!(parent.capabilities.threads);
        let root = "1760346100000";
        let expected = format!("{CHANNEL};messageid={root}");
        let out = a.run(Command::CreateThread {
            room: parent.id,
            name: "Plan".into(),
            message: Some(MessageId(root.into())),
            text: "First reply".into(),
            tags: Vec::new(),
        });
        let thread = a.room(&expected).unwrap();
        assert_eq!(thread.parent.as_deref(), Some(CHANNEL));
        assert!(!thread.capabilities.threads);
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::RoomOpened(id) if *id.raw == expected))
        );
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Request(Request::Send { conversation, html, .. })) if conversation == expected && html.contains("First reply"))
        );
        a.run(Command::LoadHistory {
            room: thread.id.clone(),
            before: None,
        });
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Request(Request::History { conversation, .. })) if conversation == expected)
        );
        let mut events = Vec::new();
        a.handle(Ev::News(json!({"resourceType":"NewMessage","resource":{"conversationid":expected,"id":"1760346200000","rootMessageId":root,"from":BEA,"content":"A reply","messagetype":"Text","clientmessageid":"12"}})), &mut events);
        assert!(events.iter().any(|e| matches!(e, Event::MessageAdded { room, message } if *room.raw == expected && message.reply_to.is_none())));
        assert!(a.room(&expected).unwrap().unread);
        let mut out = Vec::new();
        a.answer(Answer::History { conversation:CHANNEL.into(), before:None, result:Ok(json!({"messages":[{"conversationid":CHANNEL,"id":root,"from":BEA,"content":"Plan","messagetype":"Text"}]})) }, &mut out);
        assert!(out.iter().any(
            |e| matches!(e, Event::History { messages, .. } if messages.iter().any(|m| m.text.contains("Open thread")))
        ));
        assert!(a.servers()[0].categories[0].rooms.iter().any(|r| *r.raw == expected));
    }

    #[test]
    fn channel_mentions_and_thread_replies_alert_and_updates_reach_threads() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let root = "1760346100000";
        let thread_id = format!("{CHANNEL};messageid={root}");
        // Bea writes into a thread we don't take part in yet: no alert.
        let mut out = Vec::new();
        a.handle(
            Ev::News(
                json!({"resourceType":"NewMessage","resource":{"conversationid":CHANNEL,"id":"1760346200000","rootMessageId":root,"from":BEA,"content":"A reply","messagetype":"Text","clientmessageid":"12"}}),
            ),
            &mut out,
        );
        assert!(!out.iter().any(|e| matches!(e, Event::Alert { .. })));
        // We answer in it, so the thread becomes ours: the next reply alerts.
        a.handle(
            Ev::News(
                json!({"resourceType":"NewMessage","resource":{"conversationid":CHANNEL,"id":"1760346300000","rootMessageId":root,"from":ME,"content":"Ours","messagetype":"Text","clientmessageid":"13"}}),
            ),
            &mut out,
        );
        out.clear();
        a.handle(
            Ev::News(
                json!({"resourceType":"NewMessage","resource":{"conversationid":CHANNEL,"id":"1760346400000","rootMessageId":root,"from":BEA,"content":"Another","messagetype":"Text","clientmessageid":"14"}}),
            ),
            &mut out,
        );
        assert!(out.iter().any(
            |e| matches!(e, Event::Alert { room, kind: godwit_core::application::AlertKind::Message, .. } if *room.raw == thread_id)
        ));
        // A channel mention alerts, in the channel itself.
        out.clear();
        a.handle(
            Ev::News(
                json!({"resourceType":"NewMessage","resource":{"conversationid":CHANNEL,"id":"1760346500000","from":BEA,"content":"Hi","messagetype":"Text","clientmessageid":"15","properties":{"mentions":format!("[{{\"mri\":\"{ME}\"}}]")}}}),
            ),
            &mut out,
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Alert { room, .. } if &*room.raw == CHANNEL))
        );
        out.clear();
        a.handle(
            Ev::News(
                json!({"resourceType":"NewMessage","resource":{"conversationid":CHANNEL,"id":"1760346600000","from":BEA,"content":"Hi again","messagetype":"Text","clientmessageid":"16","properties":{"mentions":[{"mri":ME}]}}}),
            ),
            &mut out,
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Alert { room, .. } if &*room.raw == CHANNEL)),
            "an array-shaped mention property also alerts"
        );
        // Edits and deletes of thread replies update the thread room, not
        // the channel's timeline.
        out.clear();
        a.handle(
            Ev::News(
                json!({"resourceType":"MessageUpdate","resource":{"conversationid":format!("{CHANNEL};messageid={root}"),"id":"1760346200000","from":BEA,"content":"Edited","messagetype":"Text"}}),
            ),
            &mut out,
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::MessageUpdated { room, .. } if *room.raw == thread_id))
        );
        assert!(
            !out.iter()
                .any(|e| matches!(e, Event::MessageUpdated { room, .. } if &*room.raw == CHANNEL))
        );
        out.clear();
        a.handle(
            Ev::News(
                json!({"resourceType":"MessageUpdate","resource":{"conversationid":format!("{CHANNEL};messageid={root}"),"id":"1760346200000","from":BEA,"properties":{"deletetime":"1"}}}),
            ),
            &mut out,
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::MessageDeleted { room, .. } if *room.raw == thread_id))
        );
    }

    #[test]
    fn edits_reactions_and_live_reaction_summaries() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let room = RoomId::new(a.id().unwrap().clone(), DM);
        assert!(a.room(DM).unwrap().capabilities.edit);
        assert!(a.room(DM).unwrap().capabilities.react);
        a.run(Command::EditMessage {
            room: room.clone(),
            message: MessageId("1760346100000".into()),
            text: "changed".into(),
        });
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Request(Request::Edit { conversation, message, html, .. })) if conversation == DM && message == "1760346100000" && html.contains("changed"))
        );
        a.run(Command::React {
            room,
            message: MessageId("1760346100000".into()),
            emoji: "👍".into(),
            add: true,
        });
        assert!(matches!(cmds.try_recv(), Ok(Cmd::Request(Request::React { key, add: true, .. })) if key == "like"));

        let mut events = Vec::new();
        a.handle(Ev::News(json!({"resourceType":"MessageUpdate","resource":{"conversationid":CHANNEL,"id":"1760346100000","from":BEA,"content":"hello","messagetype":"Text","annotationsSummary":{"emotions":{"like":2,"heart":1}}}})), &mut events);
        let Some(Event::MessageUpdated { message, .. }) =
            events.iter().find(|e| matches!(e, Event::MessageUpdated { .. }))
        else {
            panic!("expected update")
        };
        assert_eq!(message.reactions.len(), 2);
        assert!(message.reactions.iter().any(|r| &*r.emoji == "👍" && r.count == 2));
    }

    #[test]
    fn selected_files_reach_the_backend_without_reading_them_on_the_ui_thread() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let room = RoomId::new(a.id().unwrap().clone(), DM);
        let selected = godwit_core::application::Outgoing {
            path: "never-open-this-test-path.png".into(),
            name: "image.png".into(),
            size: 42,
        };
        let events = a.run(Command::SendMessage {
            room,
            text: "caption".into(),
            nonce: "file-nonce".into(),
            reply_to: None,
            files: vec![selected.clone()],
        });
        assert!(events.is_empty());
        let Ok(Cmd::Request(Request::Send {
            files,
            recipient: Some(_),
            ..
        })) = cmds.try_recv()
        else {
            panic!("file send not routed")
        };
        assert_eq!(files, vec![selected]);
    }

    #[test]
    fn ready_lists_chats_with_names_and_unread_state() {
        let (mut a, _cmds, _tx) = account();
        let out = ready(&mut a);
        let Some(Event::AccountReady {
            display_name,
            username,
            rooms,
            ..
        }) = out.first()
        else {
            panic!("{out:?}")
        };
        assert_eq!((&**display_name, &**username), ("Ada Example", "ada@example.com"));
        let dm = rooms.iter().find(|r| &*r.id.raw == DM).expect("the 1:1");
        assert_eq!(&*dm.name, "Bea Example");
        assert!(matches!(&dm.kind, RoomKind::Direct { with } if &*with.raw == BEA));
        assert!(dm.unread, "the newest message is after the read marker");
        let group = rooms.iter().find(|r| &*r.id.raw == GROUP).expect("the group");
        assert_eq!(&*group.name, "Planning");
        assert!(!group.unread, "our own message isn't unread");
        // Channels wait for the aggregator; system conversations never show.
        assert!(rooms.iter().all(|r| &*r.id.raw != CHANNEL && &*r.id.raw != "48:notes"));
    }

    #[test]
    fn teams_become_servers_with_their_channels() {
        let (mut a, _cmds, _tx) = account();
        ready(&mut a);
        let mut out = Vec::new();
        a.handle(
            Ev::Teams(json!({"teams": [{"id": "19:team@thread.tacv2", "displayName": "Design",
                "channels": [{"id": CHANNEL, "displayName": "General", "isGeneral": true},
                             {"id": "19:old@thread.tacv2", "displayName": "Old", "isArchived": true}]}],
                "chats": [{"id": GROUP, "members": [{"mri": ME}, {"mri": BEA, "friendlyName": "Bea"}]}]})),
            &mut out,
        );
        let servers = out
            .iter()
            .find_map(|e| match e {
                Event::ServersChanged { servers, .. } => Some(servers.clone()),
                _ => None,
            })
            .expect("servers");
        assert_eq!(&*servers[0].name, "Design");
        assert_eq!(
            servers[0].categories[0].rooms.len(),
            1,
            "archived channels are left out"
        );
        let channel = out.iter().find_map(|e| match e {
            Event::RoomUpdated(r) if &*r.id.raw == CHANNEL => Some(r.clone()),
            _ => None,
        });
        assert!(matches!(channel.map(|c| c.kind), Some(RoomKind::Channel { .. })));
    }

    #[test]
    fn sends_are_confirmed_once_and_their_echo_is_not_added_again() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        // What signing in asks for (directory, statuses, names) comes first.
        while cmds.try_recv().is_ok() {}
        let room = RoomId::new(a.account.clone().unwrap(), GROUP);
        a.run(Command::SendMessage {
            room: room.clone(),
            text: "hi <b>".into(),
            nonce: "n1".into(),
            reply_to: None,
            files: Vec::new(),
        });
        let Ok(Cmd::Request(Request::Send { html, client_id, .. })) = cmds.try_recv() else {
            panic!("no send")
        };
        assert_eq!(html, "<p>hi &lt;b&gt;</p>");
        let mut out = Vec::new();
        a.handle(
            Ev::Answer(Answer::Sent {
                conversation: GROUP.into(),
                client_id: client_id.clone(),
                result: Ok(json!({"OriginalArrivalTime": 1_760_346_200_000i64})),
            }),
            &mut out,
        );
        assert!(
            matches!(out.first(), Some(Event::MessageConfirmed { nonce, message, .. })
            if &**nonce == "n1" && &*message.id.0 == "1760346200000")
        );
        let mut out = Vec::new();
        a.handle(
            Ev::News(
                json!({"type": "EventMessage", "resourceType": "NewMessage", "resource": {
                "id": "1760346200000", "clientmessageid": client_id, "messagetype": "RichText/Html",
                "content": "<p>hi &lt;b&gt;</p>", "from": format!("…/contacts/{ME}"),
                "conversationLink": format!("https://x/v1/users/ME/conversations/{GROUP}")}}),
            ),
            &mut out,
        );
        assert!(
            !out.iter()
                .any(|e| matches!(e, Event::MessageAdded { .. } | Event::MessageConfirmed { .. }))
        );
    }

    #[test]
    fn pushed_messages_typing_and_deletes() {
        let (mut a, _cmds, _tx) = account();
        ready(&mut a);
        let from = format!("…/contacts/{BEA}");
        let link = format!("https://x/v1/users/ME/conversations/{DM}");
        let mut out = Vec::new();
        a.handle(
            Ev::News(json!({"resourceType": "NewMessage", "resource": {
                "id": "1", "messagetype": "Control/Typing", "from": from, "conversationLink": link}})),
            &mut out,
        );
        assert!(
            matches!(out.first(), Some(Event::Typing { names, .. }) if names.len() == 1 && &*names[0] == "Bea Example")
        );
        let mut out = Vec::new();
        a.handle(
            Ev::News(json!({"resourceType": "NewMessage", "resource": {
                "id": "1760346300000", "messagetype": "RichText/Html", "content": "<p><b>hello</b></p>",
                "imdisplayname": "Bea Example", "from": from, "conversationLink": link}})),
            &mut out,
        );
        assert!(
            matches!(&out[0], Event::Typing { names, .. } if names.is_empty()),
            "sending ends typing"
        );
        assert!(matches!(&out[1], Event::MessageAdded { message, .. } if &*message.text == "**hello**"));
        let mut out = Vec::new();
        a.handle(
            Ev::News(json!({"resourceType": "MessageUpdate", "resource": {
                "id": "1760346300000", "messagetype": "RichText/Html", "content": "",
                "properties": {"deletetime": "1760346400000"}, "from": from, "conversationLink": link}})),
            &mut out,
        );
        assert!(matches!(out.first(), Some(Event::MessageDeleted { id, .. }) if &*id.0 == "1760346300000"));
    }

    #[test]
    fn authors_without_a_display_name_are_looked_up_in_one_request() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let msg = |id: &str, from: &str| {
            json!({"id": id, "messagetype": "RichText/Html", "content": "<p>x</p>",
                   "from": format!("https://emea.ng.msg.teams.microsoft.com/v1/users/ME/contacts/{from}"),
                   "originalarrivaltime": "2026-11-04T10:00:00.000Z"})
        };
        for (id, from) in [("1", "8:orgid:x"), ("2", "8:orgid:y"), ("3", "8:orgid:x")] {
            assert!(a.message(&msg(id, from)).is_some());
        }
        a.ask_unnamed();
        let asked: Vec<Vec<String>> = std::iter::from_fn(|| cmds.try_recv().ok())
            .filter_map(|c| match c {
                Cmd::Request(Request::Names(ids)) => Some(ids),
                _ => None,
            })
            .collect();
        assert_eq!(asked.len(), 1, "one request");
        let mut ids = asked[0].clone();
        ids.sort();
        assert_eq!(ids, ["8:orgid:x", "8:orgid:y"]);
    }

    #[test]
    fn marking_read_sends_the_newest_message_once() {
        let (mut a, mut cmds, _tx) = account();
        ready(&mut a);
        // What signing in asks for (directory, statuses, names) comes first.
        while cmds.try_recv().is_ok() {}
        let room = RoomId::new(a.account.clone().unwrap(), DM);
        a.run(Command::MarkRoomRead { room: room.clone() });
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Request(Request::Ack { message, .. })) if message == "1760346100000")
        );
        a.run(Command::MarkRoomRead { room });
        assert!(cmds.try_recv().is_err(), "already read: nothing sent");
    }

    #[test]
    fn ids_and_links_parse() {
        assert_eq!(
            mri("https://x/v1/users/ME/contacts/8:orgid:abc").as_deref(),
            Some("8:orgid:abc")
        );
        assert_eq!(mri("nonsense"), None);
        assert_eq!(
            split_thread("19:c@thread.tacv2;messageid=17"),
            ("19:c@thread.tacv2".into(), Some("17".into()))
        );
        let m =
            json!({"conversationLink": "https://x/v1/users/ME/conversations/19%3Ac%40thread.tacv2%3Bmessageid%3D5"});
        assert_eq!(
            conversation_of(&m),
            Some(("19:c@thread.tacv2".into(), Some("5".into())))
        );
    }
}
