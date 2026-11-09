// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A Slack sign-in as the core sees it: every workspace a server with its
//! channels (by name), DMs and group DMs in the chat list, messages (mrkdwn)
//! into Godwit's, websocket events into events, and the core's commands
//! into requests. Ids carry their workspace: rooms are `T…:C…`, people
//! `T…:U…`, servers `T…`. First version: text; threads show flat (replies
//! as replies to their post), files as links, no calls.

mod commands;
mod events;
mod messages;
mod notifications;
mod rooms;
mod threads;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;

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
use crate::{mrkdwn, session};

/// Where signing in stands, for the sign-in screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    SignedOut {
        error: Option<String>,
    },
    /// Slack's sign-in window is open.
    Waiting,
    Connecting,
    Ready,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Channel,
    Direct(String),
    Group,
}

#[derive(Clone, Debug)]
struct Conversation {
    kind: Kind,
    name: String,
    /// The newest message's `ts`, and what we've read up to.
    latest: String,
    last_read: String,
    mentions: u32,
    has_unreads: bool,
}

/// A thread: replies under one message of a channel (its root's `ts`).
#[derive(Clone, Debug, Default)]
struct Thread {
    name: String,
    subscribed: bool,
    preview: String,
    mark_pending: Option<String>,
    mark_wanted: Option<String>,
    last_read: String,
    /// The newest reply's `ts`: threads list newest first.
    latest: String,
    unread: bool,
}

#[derive(Clone, Debug, Default)]
struct Workspace {
    name: String,
    icon: Option<String>,
    me: String,
    muted: HashSet<String>,
    /// Desktop notification level per channel ("everything", "mentions",
    /// "nothing"), from all_notifications_prefs; absent or "default" falls
    /// back to `desktop_level`.
    channel_levels: HashMap<String, String>,
    /// The workspace-wide level (`global.global_desktop`); empty = mentions.
    desktop_level: String,
    /// Highlight words (`global.global_keywords`), lowercased.
    keywords: Vec<String>,
    /// User-group ids whose membership includes the signed-in user.
    usergroups: HashSet<String>,
    notification_settings: Value,
    prefs_pending: bool,
    seen: HashSet<(String, String)>,
    seen_order: std::collections::VecDeque<(String, String)>,
    threads_pending: bool,
    /// Where the Threads view's next older page starts; None once exhausted.
    threads_cursor: Option<String>,
    conversations_pending: bool,
    conversations: HashMap<String, Conversation>,
    /// (channel, root `ts`) → the thread.
    threads: HashMap<(String, String), Thread>,
    emoji: Value,
}

/// People whose presence a workspace follows at most: Slack asks clients to
/// subscribe only to those they show.
const FOLLOWED: usize = 500;

/// Threads listed under a channel, the most recently active first: Slack
/// channels can have hundreds, and the rest are reached from their posts.
const THREADS_SHOWN: usize = 10;

/// A thread's name: its first post's first line, shortened.
fn thread_name(plain: &str) -> String {
    let line = plain.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    let mut name: String = line.chars().take(40).collect();
    if line.chars().count() > 40 {
        name.push('…');
    }
    if name.is_empty() { "Thread".into() } else { name }
}

pub struct SlackAccount {
    /// The rooms last sent to the app.
    room_sync: godwit_core::room_sync::RoomSync,
    slot: u32,
    net: Handle,
    events: mpsc::Receiver<Ev>,
    status: Status,
    account: Option<AccountId>,
    my_name: String,
    own_presence: Presence,
    own_status: Option<String>,
    teams: Vec<String>,
    workspaces: HashMap<String, Workspace>,
    /// `T:U` → name.
    names: HashMap<String, String>,
    /// `T:U` → their picture's address.
    pictures: HashMap<String, String>,
    asked: HashSet<String>,
    /// Our sends confirmed from their answer: their echo isn't added again.
    confirmed: HashSet<String>,
    /// Who is typing per `team:channel`, and when they last said so: Slack
    /// sends no stop, so a name runs out after core's `TYPING_FOR`.
    typing: HashMap<String, Vec<(String, std::time::Instant)>>,
    /// Whose presence each workspace follows.
    followed: HashMap<String, std::collections::BTreeSet<String>>,
    /// Each workspace's member directory, for People.
    directory: HashMap<String, Vec<godwit_core::application::Person>>,
    persist: bool,
    media: godwit_core::media::Media,
}

/// `ts` (`"1700000000.000100"`) in milliseconds.
pub fn ts_ms(ts: &str) -> i64 {
    let (secs, frac) = ts.split_once('.').unwrap_or((ts, "0"));
    let secs: i64 = secs.parse().unwrap_or(0);
    let ms: i64 = frac.get(..3).unwrap_or(frac).parse().unwrap_or(0);
    secs.saturating_mul(1000).saturating_add(ms)
}

/// Whether `a` is a later `ts` than `b` (compared as numbers, not text).
fn newer(a: &str, b: &str) -> bool {
    let parse = |t: &str| {
        let (s, f) = t.split_once('.').unwrap_or((t, "0"));
        (
            s.parse::<u64>().unwrap_or(0),
            format!("{f:0<6}").parse::<u64>().unwrap_or(0),
        )
    };
    parse(a) > parse(b)
}

/// A room's parts: `T:C` (a conversation) or `T:C:ts` (a thread in it).
fn parts(raw: &str) -> Option<(&str, &str, Option<&str>)> {
    let (team, rest) = raw.split_once(':')?;
    Some(match rest.split_once(':') {
        Some((channel, ts)) => (team, channel, Some(ts)),
        None => (team, rest, None),
    })
}

impl SlackAccount {
    pub fn new(slot: u32, wake: backend::Waker) -> std::io::Result<Self> {
        let (net, events) = backend::spawn(wake)?;
        let saved = session::load(slot);
        let status = if saved.is_some() {
            Status::Connecting
        } else {
            Status::SignedOut { error: None }
        };
        let account = Self::with(slot, net, events, status, true);
        if let Some(saved) = saved {
            crate::media::configure(&account.media, &saved);
            account.net.send(Cmd::UseSession(saved));
        }
        Ok(account)
    }

    fn with(slot: u32, net: Handle, events: mpsc::Receiver<Ev>, status: Status, persist: bool) -> Self {
        Self {
            room_sync: Default::default(),
            slot,
            net,
            events,
            status,
            account: None,
            my_name: String::new(),
            own_presence: Presence::Unknown,
            own_status: None,
            teams: Vec::new(),
            workspaces: HashMap::new(),
            names: HashMap::new(),
            pictures: HashMap::new(),
            asked: HashSet::new(),
            confirmed: HashSet::new(),
            typing: HashMap::new(),
            directory: HashMap::new(),
            followed: HashMap::new(),
            persist,
            media: godwit_core::media::Media::new("slack"),
        }
    }

    pub fn slot(&self) -> u32 {
        self.slot
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Opens Slack's sign-in window (the player helper at `helper`).
    pub fn sign_in(&mut self, helper: PathBuf) {
        if matches!(self.status, Status::SignedOut { .. }) {
            self.status = Status::Waiting;
            self.net.send(Cmd::SignIn(helper));
        }
    }

    pub fn cancel_sign_in(&mut self) {
        if self.status == Status::Waiting {
            self.net.send(Cmd::CancelSignIn);
            self.status = Status::SignedOut { error: None };
        }
    }

    pub fn is_ours(&self, user: &UserId) -> bool {
        self.account.as_ref() == Some(&user.account)
            && user
                .raw
                .split_once(':')
                .is_some_and(|(team, id)| self.workspaces.get(team).is_some_and(|ws| ws.me == id))
    }

    fn room_id(&self, team: &str, channel: &str) -> Option<RoomId> {
        Some(RoomId::new(self.account.clone()?, format!("{team}:{channel}").as_str()))
    }

    fn user_id(&self, team: &str, user: &str) -> Option<UserId> {
        Some(UserId::new(self.account.clone()?, format!("{team}:{user}").as_str()))
    }

    fn name(&self, team: &str, user: &str) -> String {
        self.names
            .get(&format!("{team}:{user}"))
            .cloned()
            .unwrap_or_else(|| "Someone".into())
    }
}

struct Names<'a> {
    account: &'a SlackAccount,
    team: &'a str,
}

impl mrkdwn::Names for Names<'_> {
    fn user(&self, id: &str) -> (String, String) {
        (self.account.name(self.team, id), format!("{}:{id}", self.team))
    }
    fn channel(&self, id: &str) -> (String, Option<String>) {
        let known = self
            .account
            .workspaces
            .get(self.team)
            .and_then(|w| w.conversations.get(id));
        (
            known.map_or_else(|| "channel".into(), |c| c.name.clone()),
            known.map(|_| format!("{}:{id}", self.team)),
        )
    }
    fn custom_emoji(&self, name: &str) -> Option<(String, String)> {
        let map = self.account.workspaces.get(self.team)?.emoji.as_object()?;
        let (canonical, url) = mrkdwn::custom_emoji(name, map)?;
        Some((canonical.to_owned(), url.to_owned()))
    }
}

impl NetworkAccount for SlackAccount {
    // Slack's documented broadcast tokens and user-group token are described at
    // https://api.slack.com/reference/surfaces/formatting#special-mentions.
    fn mention_specials(&self, room: &RoomId) -> Vec<godwit_core::autocomplete::MentionSpecial> {
        let Some((team, channel)) = room.raw.split_once(':') else {
            return Vec::new();
        };
        let Some(ws) = self.workspaces.get(team) else {
            return Vec::new();
        };
        let mut items = vec![
            ("here", "@here", "Active members"),
            ("channel", "@channel", "Channel members"),
        ];
        if ws.conversations.get(channel).is_some_and(|c| c.name == "general") {
            items.push(("everyone", "@everyone", "Workspace members"));
        }
        items
            .into_iter()
            .map(|(id, label, detail)| godwit_core::autocomplete::MentionSpecial {
                id: id.into(),
                label: label.into(),
                detail: detail.into(),
            })
            .collect()
    }

    fn autocomplete_text(&self, kind: godwit_core::autocomplete::TriggerKind, id: &str, name: &str) -> String {
        match kind {
            godwit_core::autocomplete::TriggerKind::Person => {
                format!("<@{}>", id.rsplit(':').next().unwrap_or(id))
            }
            godwit_core::autocomplete::TriggerKind::Channel => {
                // Room ids are `team:channel`; Slack's token takes the channel.
                format!("<#{}>", id.rsplit(':').next().unwrap_or(id))
            }
            godwit_core::autocomplete::TriggerKind::Emoji => format!(":{name}:"),
            godwit_core::autocomplete::TriggerKind::MentionSpecial => match id {
                "here" => "<!here>".into(),
                "channel" => "<!channel>".into(),
                "everyone" => "<!everyone>".into(),
                _ if id.starts_with("subteam^") => format!("<!{id}|@{}>", name.strip_prefix('@').unwrap_or(name)),
                _ => name.to_owned(),
            },
        }
    }

    fn network(&self) -> Network {
        Network::Slack
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

    /// Slack's link to a message: `https://{workspace}.slack.com/archives/{channel}/p{ts without the dot}`.
    fn message_link(&self, room: &RoomId, message: &MessageId) -> Option<String> {
        let (team, channel, thread) = parts(&room.raw)?;
        let _ = self.workspaces.get(team)?;
        let mut link = format!(
            "https://app.slack.com/archives/{channel}/p{}",
            message.0.replace('.', "")
        );
        // A reply links into its thread.
        if let Some(t) = thread {
            link.push_str(&format!("?thread_ts={t}&cid={channel}"));
        }
        Some(link)
    }

    fn media(&self) -> Option<(String, godwit_core::account::MediaFetch)> {
        Some(self.media.registration())
    }

    fn log_out(&mut self) {
        self.media.clear();
        self.net.send(Cmd::Logout);
    }
}

impl Drop for SlackAccount {
    fn drop(&mut self) {
        self.media.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn account() -> (SlackAccount, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
        let (net, cmds) = Handle::detached();
        let (_tx, events) = mpsc::channel();
        (
            SlackAccount::with(u32::MAX, net, events, Status::Connecting, false),
            cmds,
        )
    }

    fn ready(a: &mut SlackAccount) -> Vec<Event> {
        let mut out = Vec::new();
        a.ready(
            "T1",
            &json!({"ok": true, "user_id": "U1", "user": "ada", "team": "Acme"}),
            &[
                json!({"id": "C1", "name": "general", "is_channel": true}),
                json!({"id": "D1", "is_im": true, "user": "U2"}),
                json!({"id": "G1", "is_mpim": true, "name": "mpdm-ada--bea--cy-1"}),
            ],
            &json!({"channels": [{"id": "C1", "last_read": "1700000000.000100", "latest": "1700000005.000100", "mention_count": 2, "has_unreads": true}]}),
            &mut out,
        );
        out
    }

    #[test]
    fn repeated_read_marks_wait_for_the_current_request() {
        let (mut a, mut cmds) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        a.put_thread("T1", "C1", "1", None, "2", true);
        let room = a.thread_room("T1", "C1", "1").unwrap().id;
        for _ in 0..20 {
            a.run(Command::MarkRoomRead { room: room.clone() });
        }
        let queued: Vec<_> = std::iter::from_fn(|| cmds.try_recv().ok()).collect();
        assert_eq!(queued.len(), 1);
        a.put_thread("T1", "C1", "1", None, "3", true);
        a.run(Command::MarkRoomRead { room });
        assert!(cmds.try_recv().is_err());
        a.answer(
            "T1",
            Answer::ThreadMarked {
                channel: "C1".into(),
                root: "1".into(),
                ts: "2".into(),
                result: Ok(()),
            },
            &mut Vec::new(),
        );
        assert!(matches!(cmds.try_recv(),Ok(Cmd::Request { request:Request::MarkThread { ts,.. },.. }) if ts == "3"));
    }

    #[test]
    fn channel_alerts_follow_slack_levels_broadcasts_and_keywords() {
        let (mut a, mut cmds) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let alerts = |a: &mut SlackAccount, text: &str, ts: &str| {
            let mut out = Vec::new();
            a.news(
                "T1",
                &json!({"type":"message","channel":"C1","user":"U2","text":text,"ts":ts}),
                &mut out,
            );
            out.iter().filter(|e| matches!(e, Event::Alert { .. })).count()
        };
        // Default (mentions): plain chatter is quiet, @here and keywords ping.
        assert_eq!(alerts(&mut a, "lunch?", "1700000001.000100"), 0);
        assert_eq!(alerts(&mut a, "<!here> standup", "1700000002.000100"), 1);
        a.notification_prefs(
            "T1",
            &json!({"all_notifications_prefs": r#"{"global":{"global_desktop":"mentions","global_keywords":"deploy, Outage"},"channels":{}}"#}),
        );
        assert_eq!(alerts(&mut a, "an outage in eu", "1700000003.000100"), 1);
        // "All new messages" for the channel: everything pings.
        a.notification_prefs(
            "T1",
            &json!({"all_notifications_prefs": r#"{"global":{"global_desktop":"mentions"},"channels":{"C1":{"desktop":"everything"}}}"#}),
        );
        assert_eq!(alerts(&mut a, "lunch?", "1700000004.000100"), 1);
        // "Nothing": even a mention stays quiet.
        a.notification_prefs(
            "T1",
            &json!({"all_notifications_prefs": r#"{"global":{"global_desktop":"nothing"},"channels":{}}"#}),
        );
        assert_eq!(alerts(&mut a, "Hi <@U1>", "1700000005.000100"), 0);
    }

    #[test]
    fn user_group_membership_is_loaded_and_triggers_alerts() {
        let (mut a, mut cmds) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        a.answer(
            "T1",
            Answer::UserGroups(Ok(json!({"usergroups":[
                {"id":"S1","users":["U1","U2"]},
                {"id":"S2","users":["U2"]}
            ]}))),
            &mut Vec::new(),
        );
        assert!(a.workspaces["T1"].usergroups.contains("S1"));
        assert!(!a.workspaces["T1"].usergroups.contains("S2"));
        let mut out = Vec::new();
        a.news(
            "T1",
            &json!({"type":"message","channel":"C1","user":"U2","text":"hi <!subteam^S1>","ts":"1700000009.000100"}),
            &mut out,
        );
        assert!(out.iter().any(|e| matches!(e, Event::Alert { .. })));
    }

    #[test]
    fn slack_notification_preferences_populate_shared_room_state() {
        let (mut a, _) = account();
        ready(&mut a);
        a.notification_prefs("T1", &json!({"all_notifications_prefs":r#"{"global":{"global_desktop":"mentions"},"channels":{"C1":{"desktop":"everything","muted":true}}}"#}));
        let room = a.room("T1", "C1").expect("channel room");
        assert_eq!(room.notify.level, Some(0));
        assert!(room.notify.muted);
        assert!(!room.capabilities.notify_settings);
    }

    #[test]
    fn relevant_channel_messages_alert_and_muted_channels_stay_quiet() {
        let (mut a, mut cmds) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let mut out = Vec::new();
        let mention = json!({"type":"message","channel":"C1","user":"U2","text":"Hi <@U1>","ts":"1700000007.000100"});
        a.news("T1", &mention, &mut out);
        assert_eq!(out.iter().filter(|e| matches!(e, Event::Alert { .. })).count(), 1);
        out.clear();
        a.notification_prefs(
            "T1",
            &json!({"all_notifications_prefs": r#"{"channels":{"C1":{"muted":true}}}"#}),
        );
        let mut muted_mention = mention.clone();
        muted_mention["ts"] = "1700000007.000200".into();
        a.news("T1", &muted_mention, &mut out);
        assert!(!out.iter().any(|e| matches!(e, Event::Alert { .. })));
        assert!(a.notifications_muted(&RoomId::new(a.account.clone().unwrap(), "T1:C1")));
        a.news(
            "T1",
            &json!({"type":"pref_change","name":"all_notifications_prefs","value":r#"{"channels":{}}"#}),
            &mut out,
        );
        assert!(!a.notifications_muted(&RoomId::new(a.account.clone().unwrap(), "T1:C1")));
        a.put_thread("T1", "C1", "1700000001.000100", None, "1700000001.000100", false);
        a.workspaces
            .get_mut("T1")
            .unwrap()
            .threads
            .get_mut(&("C1".into(), "1700000001.000100".into()))
            .unwrap()
            .subscribed = true;
        out.clear();
        a.news("T1", &json!({"type":"message","channel":"C1","user":"U2","text":"reply","ts":"1700000008.000100","thread_ts":"1700000001.000100"}), &mut out);
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Alert { room, .. } if &*room.raw == "T1:C1:1700000001.000100"))
        );
        assert!(a.is_ours(&UserId::new(a.account.clone().unwrap(), "T1:U1")));
        assert!(!a.is_ours(&UserId::new(a.account.clone().unwrap(), "T1:U2")));
    }

    #[test]
    fn threads_in_dms_and_group_dms_keep_their_partner_context() {
        let (mut a, mut cmds) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let mut out = Vec::new();
        // A DM reply lands in the DM's thread room, which keeps the
        // partner context; a group DM's thread does too.
        a.news(
            "T1",
            &json!({"type":"message","channel":"D1","user":"U2","text":"dm reply","ts":"1700000009.000100","thread_ts":"1700000002.000100"}),
            &mut out,
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::MessageAdded { room, .. } if &*room.raw == "T1:D1:1700000002.000100")),
            "{out:?}"
        );
        let thread = a.thread_room("T1", "D1", "1700000002.000100").unwrap();
        assert!(
            matches!(&thread.kind, RoomKind::Channel { server } if &*server.raw == "T1"),
            "the DM thread opens in its workspace, not as a DM: {:?}",
            thread.kind
        );
        out.clear();
        a.news(
            "T1",
            &json!({"type":"message","channel":"G1","user":"U2","text":"group reply","ts":"1700000010.000100","thread_ts":"1700000003.000100"}),
            &mut out,
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::MessageAdded { room, .. } if &*room.raw == "T1:G1:1700000003.000100")),
            "{out:?}"
        );
        assert!(
            matches!(
                a.thread_room("T1", "G1", "1700000003.000100").unwrap().kind,
                RoomKind::Channel { .. }
            ),
            "the group DM thread opens in its workspace too"
        );
    }

    #[test]
    fn own_status_is_confirmed_and_away_is_not_offline() {
        let (mut a, mut cmds) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let mut out = Vec::new();
        a.answer(
            "T1",
            Answer::OwnState(Ok(json!({"presence":"away","profile":{"status_text":"Deep work"}}))),
            &mut out,
        );
        assert!(out.iter().any(|e| matches!(e, Event::OwnPresence { presence: Presence::Idle, status:Some(text), .. } if &**text == "Deep work")));
        a.run(Command::SetPresence {
            presence: Presence::Online,
            choice: 0,
        });
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Request {
                request: Request::SetOwnPresence(true),
                ..
            })
        ));
        a.run(Command::SetStatusText(Some("Lunch".into())));
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Request { request:Request::SetStatus(Some(text)), .. }) if text == "Lunch")
        );
        out.clear();
        a.answer("T1", Answer::OwnState(Err("unreachable".into())), &mut out);
        assert!(
            out.iter().any(|e| matches!(
                e,
                Event::OwnPresence {
                    presence: Presence::Idle,
                    ..
                }
            )),
            "failed writes restore the last confirmed presence"
        );
        out.clear();
        a.news(
            "T1",
            &json!({"type":"presence_change","user":"U1","presence":"active"}),
            &mut out,
        );
        assert!(out.iter().any(|e| matches!(
            e,
            Event::OwnPresence {
                presence: Presence::Online,
                ..
            }
        )));
    }

    #[test]
    fn subscribed_threads_are_discovered_without_loading_parent_history() {
        let (mut a, mut cmds) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let mut out = Vec::new();
        a.handle(
            Ev::Online {
                team: "T1".into(),
                online: true,
            },
            &mut out,
        );
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Request {
                request: Request::Threads(None),
                ..
            })
        ));
        let queued: Vec<_> = std::iter::from_fn(|| cmds.try_recv().ok()).collect();
        assert!(queued.iter().any(|c| matches!(
            c,
            Cmd::Request {
                request: Request::OwnState,
                ..
            }
        )));
        assert!(queued.iter().any(|c| matches!(
            c,
            Cmd::Request {
                request: Request::NotificationPrefs,
                ..
            }
        )));
        a.answer(
            "T1",
            Answer::Threads {
                more: false,
                result: Ok((
                    vec![
                        json!({"root_msg":{"channel":"C1","thread_ts":"1700000004.000100","text":"New plan","subscribed":true,"last_read":"1700000006.000100","latest_reply":"1700000006.000100"}}),
                    ],
                    Some("1700000006.000100".into()),
                )),
            },
            &mut out,
        );
        // Scrolling down fetches the next older page from the saved cursor.
        a.run(Command::LoadThreads {
            server: ServerId::new(a.account.clone().unwrap(), "T1"),
            more: true,
        });
        assert!(matches!(
            cmds.try_recv(),
            Ok(Cmd::Request { request: Request::Threads(Some(c)), .. }) if c == "1700000006.000100"
        ));
        a.answer(
            "T1",
            Answer::Threads {
                more: true,
                result: Ok((
                    vec![
                        json!({"root_msg":{"channel":"C1","thread_ts":"1700000001.000100","text":"Old plan","subscribed":true,"last_read":"1700000002.000100","latest_reply":"1700000003.000100"}}),
                    ],
                    None,
                )),
            },
            &mut out,
        );
        // The end of the list: no further page is requested.
        a.run(Command::LoadThreads {
            server: ServerId::new(a.account.clone().unwrap(), "T1"),
            more: true,
        });
        assert!(cmds.try_recv().is_err(), "no page past the end");
        let servers = a.servers();
        assert_eq!(&*servers[0].categories[0].title, "Threads");
        let threads = &servers[0].categories[0].rooms;
        assert_eq!(&*threads[0].raw, "T1:C1:1700000004.000100");
        let old = a.thread_room("T1", "C1", "1700000001.000100").unwrap();
        assert_eq!(&*old.name, "#general · Old plan");
        assert!(old.unread);
        assert!(!a.thread_room("T1", "C1", "1700000004.000100").unwrap().unread);
        a.run(Command::MarkRoomRead { room: old.id });
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Request { request: Request::MarkThread { ts, .. }, .. }) if ts == "1700000003.000100")
        );
        // A new reply arriving during the mark request must stay unread.
        a.put_thread("T1", "C1", "1700000001.000100", None, "1700000007.000100", true);
        a.answer(
            "T1",
            Answer::ThreadMarked {
                channel: "C1".into(),
                root: "1700000001.000100".into(),
                ts: "1700000003.000100".into(),
                result: Ok(()),
            },
            &mut out,
        );
        assert!(a.thread_room("T1", "C1", "1700000001.000100").unwrap().unread);
        a.answer(
            "T1",
            Answer::Threads {
                more: false,
                result: Err("unreachable".into()),
            },
            &mut out,
        );
        assert_eq!(
            a.servers()[0].categories[0].rooms.len(),
            2,
            "a failed refresh keeps the overview"
        );
    }

    #[test]
    fn selected_files_reach_the_backend_without_reading_them_on_the_ui_thread() {
        let (mut a, mut cmds) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let room = RoomId::new(a.id().unwrap().clone(), "T1:C1");
        let selected = godwit_core::application::Outgoing {
            path: "never-open-this-test-path.png".into(),
            name: "image.png".into(),
            size: 42,
            spoiler: false,
        };
        let events = a.run(Command::SendMessage {
            room,
            text: "caption".into(),
            nonce: "file-nonce".into(),
            reply_to: None,
            files: vec![selected.clone()],
        });
        assert!(events.is_empty());
        let Ok(Cmd::Request {
            request: Request::Send { files, .. },
            ..
        }) = cmds.try_recv()
        else {
            panic!("file send not routed")
        };
        assert_eq!(files, vec![selected]);
    }

    #[test]
    fn workspaces_become_servers_and_dms_chats() {
        let (mut a, _cmds) = account();
        let out = ready(&mut a);
        let Some(Event::AccountReady { rooms, servers, .. }) = out.first() else {
            panic!("{out:?}")
        };
        assert_eq!(&*servers[0].name, "Acme");
        assert_eq!(&*servers[0].categories[0].rooms[0].raw, "T1:C1");
        let general = rooms.iter().find(|r| &*r.id.raw == "T1:C1").unwrap();
        assert!(general.unread && general.mentions == 2);
        assert!(
            matches!(&rooms.iter().find(|r| &*r.id.raw == "T1:D1").unwrap().kind, RoomKind::Direct { with } if &*with.raw == "T1:U2")
        );
        assert_eq!(
            &*rooms.iter().find(|r| &*r.id.raw == "T1:G1").unwrap().name,
            "ada, bea, cy"
        );
    }

    #[test]
    fn mention_specials_offer_everyone_in_general() {
        let (mut a, _) = account();
        let mut ws = Workspace::default();
        ws.conversations.insert(
            "C1".into(),
            Conversation {
                kind: Kind::Channel,
                name: "general".into(),
                latest: String::new(),
                last_read: String::new(),
                mentions: 0,
                has_unreads: false,
            },
        );
        a.workspaces.insert("T1".into(), ws);
        let room = RoomId::new(AccountId::new(Network::Slack, "T1"), "T1:C1");
        let specials = a.mention_specials(&room);
        assert!(specials.iter().any(|s| s.id == "everyone"));
        use godwit_core::autocomplete::TriggerKind;
        assert_eq!(
            a.autocomplete_text(TriggerKind::MentionSpecial, "here", "@here"),
            "<!here>"
        );
        assert_eq!(
            a.autocomplete_text(TriggerKind::MentionSpecial, "subteam^S1", "@team"),
            "<!subteam^S1|@team>"
        );
    }

    #[test]
    fn app_messages_are_named_by_the_app_and_bare_bots_are_looked_up() {
        let (mut a, mut cmds) = account();
        ready(&mut a);
        while cmds.try_recv().is_ok() {}
        let added = |out: &[Event]| {
            out.iter()
                .find_map(|e| match e {
                    Event::MessageAdded { message, .. } => Some(message.author_name.to_string()),
                    _ => None,
                })
                .unwrap_or_default()
        };
        // An app posting as its bot user, with its profile on the message.
        let mut out = Vec::new();
        a.news(
            "T1",
            &json!({"type": "message", "channel": "C1", "user": "U9", "bot_id": "B1",
                    "bot_profile": {"name": "Jira"}, "text": "ticket", "ts": "1700000020.000100"}),
            &mut out,
        );
        assert_eq!(added(&out), "Jira");
        // Only a bot id: named by `bots.info`.
        let mut out = Vec::new();
        a.news(
            "T1",
            &json!({"type": "message", "channel": "C1", "bot_id": "B2", "text": "deploy", "ts": "1700000021.000100"}),
            &mut out,
        );
        let asked = std::iter::from_fn(|| cmds.try_recv().ok())
            .any(|c| matches!(c, Cmd::Request { request: Request::Users(ids), .. } if ids == ["B2"]));
        assert!(asked, "the bot is looked up");
    }

    #[test]
    fn live_messages_typing_edits_and_deletes() {
        let (mut a, _cmds) = account();
        ready(&mut a);
        a.names.insert("T1:U2".into(), "Bea".into());
        let mut out = Vec::new();
        a.news(
            "T1",
            &json!({"type": "user_typing", "channel": "C1", "user": "U2"}),
            &mut out,
        );
        assert!(matches!(out.first(), Some(Event::Typing { names, .. }) if &*names[0] == "Bea"));
        let mut out = Vec::new();
        a.news(
            "T1",
            &json!({"type": "message", "channel": "C1", "user": "U2", "text": "*hi* :+1:", "ts": "1700000010.000200"}),
            &mut out,
        );
        assert!(matches!(&out[1], Event::MessageAdded { message, .. } if &*message.text == "**hi** 👍"));
        let mut out = Vec::new();
        a.news(
            "T1",
            &json!({"type": "message", "subtype": "message_deleted", "channel": "C1", "deleted_ts": "1700000010.000200"}),
            &mut out,
        );
        assert!(matches!(out.first(), Some(Event::MessageDeleted { id, .. }) if &*id.0 == "1700000010.000200"));
    }

    #[test]
    fn sends_confirm_once_and_reactions_go_by_name() {
        let (mut a, mut cmds) = account();
        ready(&mut a);
        let room = RoomId::new(a.account.clone().unwrap(), "T1:C1");
        // The name lookups ready asked for.
        while cmds.try_recv().is_ok() {}
        a.run(Command::React {
            room: room.clone(),
            message: MessageId("1.2".into()),
            emoji: "👍".into(),
            add: true,
        });
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Request { request: Request::React { name, .. }, .. }) if name == "+1")
        );
        let mut out = Vec::new();
        a.answer(
            "T1",
            Answer::Sent {
                channel: "C1".into(),
                thread: None,
                nonce: "n1".into(),
                result: Ok(json!({"ok": true, "ts": "1700000020.000100", "message": {"user": "U1", "text": "yo", "ts": "1700000020.000100"}})),
            },
            &mut out,
        );
        assert!(matches!(out.first(), Some(Event::MessageConfirmed { nonce, .. }) if &**nonce == "n1"));
        let mut out = Vec::new();
        a.news(
            "T1",
            &json!({"type": "message", "channel": "C1", "user": "U1", "text": "yo", "ts": "1700000020.000100"}),
            &mut out,
        );
        assert!(
            !out.iter().any(|e| matches!(e, Event::MessageAdded { .. })),
            "the echo isn't added again"
        );
    }

    #[test]
    fn custom_reactions_keep_the_workspace_image_url() {
        let (mut a, _cmds) = account();
        ready(&mut a);
        a.custom_emoji(
            "T1",
            &json!({"emoji":{"party":"https://emoji.slack-edge.com/T/party.png","celebrate":"alias:party"}}),
            &mut Vec::new(),
        );
        let message = a
            .message(
                "T1",
                "C1",
                &json!({
                    "user":"U2", "text":"hi", "ts":"1700000010.000200",
                    "reactions":[{"name":"celebrate","count":2,"users":["U1"]}]
                }),
                false,
            )
            .unwrap();
        assert_eq!(&*message.reactions[0].emoji, "party");
        assert_eq!(
            message.reactions[0].image_url.as_deref(),
            Some("https://emoji.slack-edge.com/T/party.png")
        );
        assert!(message.reactions[0].me);
    }

    #[test]
    fn autocomplete_uses_slack_wire_tokens() {
        let (a, _cmds) = account();
        use godwit_core::account::NetworkAccount;
        use godwit_core::autocomplete::TriggerKind;
        assert_eq!(a.autocomplete_text(TriggerKind::Person, "T1:U2", "Bea"), "<@U2>");
        assert_eq!(a.autocomplete_text(TriggerKind::Channel, "T1:C3", "general"), "<#C3>");
        assert_eq!(a.autocomplete_text(TriggerKind::Emoji, "party", "party"), ":party:");
    }

    #[test]
    fn the_emoji_list_arrives_after_ready_and_its_failure_is_ignored() {
        let (mut a, _cmds) = account();
        let mut out = Vec::new();
        // Before Ready there is no workspace to hold it.
        a.answer(
            "T1",
            Answer::Emoji(json!({"emoji":{"party":"https://e/p.png"}})),
            &mut out,
        );
        assert!(out.is_empty());
        ready(&mut a);
        a.answer("T1", Answer::Emoji(Value::Null), &mut out);
        assert!(out.is_empty(), "a failed emoji.list changes nothing: {out:?}");
        a.answer(
            "T1",
            Answer::Emoji(json!({"emoji":{"party":"https://e/p.png","cheer":"alias:party"}})),
            &mut out,
        );
        let Some(Event::CustomEmoji { groups, .. }) = out.first() else {
            panic!("{out:?}")
        };
        assert_eq!(groups[0].emoji.len(), 1, "aliases are not listed twice");
    }

    #[test]
    fn timestamps_compare_as_numbers() {
        assert_eq!(ts_ms("1700000000.123456"), 1_700_000_000_123);
        assert!(newer("1700000000.000100", "1700000000.000099"));
        assert!(newer("1700000001.000000", "1700000000.999999"));
        assert!(!newer("", "1700000000.0"));
    }

    #[test]
    fn threads_list_under_their_channel_newest_first() {
        let (mut a, _cmds) = account();
        ready(&mut a);
        let mut out = Vec::new();
        a.answer(
            "T1",
            Answer::History {
                channel: "C1".into(),
                thread: None,
                before: None,
                result: Ok(json!({"messages": [
                    {"user": "U2", "text": "Old plan", "ts": "1700000001.000100", "thread_ts": "1700000001.000100", "reply_count": 2, "latest_reply": "1700000002.000100"},
                    {"user": "U2", "text": "New idea\nmore", "ts": "1700000003.000100", "thread_ts": "1700000003.000100", "reply_count": 1, "latest_reply": "1700000009.000100"}
                ]})),
            },
            &mut out,
        );
        let Some(Event::History { messages, .. }) = out.first() else {
            panic!("{out:?}")
        };
        assert!(
            messages[1]
                .text
                .contains("[💬 1 reply](godwit://room/T1:C1:1700000003.000100)"),
            "{}",
            messages[1].text
        );
        let servers = out.iter().rev().find_map(|e| match e {
            Event::ServersChanged { servers, .. } => Some(servers.clone()),
            _ => None,
        });
        let rooms: Vec<String> = servers.unwrap()[0].categories[0]
            .rooms
            .iter()
            .map(|r| r.raw.to_string())
            .collect();
        assert_eq!(rooms, ["T1:C1", "T1:C1:1700000003.000100", "T1:C1:1700000001.000100"]);
        let thread = out.iter().find_map(|e| match e {
            Event::RoomUpdated(r) if &*r.id.raw == "T1:C1:1700000003.000100" => Some(r.clone()),
            _ => None,
        });
        let thread = thread.expect("the thread's room");
        assert_eq!(&*thread.name, "New idea");
        assert_eq!(thread.parent.as_deref(), Some("T1:C1"));
    }

    #[test]
    fn live_replies_go_to_their_thread_and_sends_there_carry_it() {
        let (mut a, mut cmds) = account();
        ready(&mut a);
        let mut out = Vec::new();
        a.news(
            "T1",
            &json!({"type": "message", "channel": "C1", "user": "U2", "text": "a reply", "ts": "1700000020.000100", "thread_ts": "1700000010.000100"}),
            &mut out,
        );
        assert!(
            matches!(out.iter().find(|e| matches!(e, Event::MessageAdded { .. })), Some(Event::MessageAdded { room, message })
                if &*room.raw == "T1:C1:1700000010.000100" && message.reply_to.is_none()),
            "{out:?}"
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::RoomUpdated(r) if &*r.id.raw == "T1:C1:1700000010.000100" && r.unread))
        );
        while cmds.try_recv().is_ok() {}
        let room = RoomId::new(a.account.clone().unwrap(), "T1:C1:1700000010.000100");
        a.run(Command::SendMessage {
            room,
            text: "me too".into(),
            nonce: "n".into(),
            reply_to: None,
            files: Vec::new(),
        });
        assert!(
            matches!(cmds.try_recv(), Ok(Cmd::Request { request: Request::Send { channel, thread: Some(t), .. }, .. })
            if channel == "C1" && t == "1700000010.000100")
        );
        // A broadcast shows in the channel itself.
        let mut out = Vec::new();
        a.news(
            "T1",
            &json!({"type": "message", "subtype": "thread_broadcast", "channel": "C1", "user": "U2", "text": "fyi", "ts": "1700000030.000100", "thread_ts": "1700000010.000100"}),
            &mut out,
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::MessageAdded { room, .. } if &*room.raw == "T1:C1"))
        );
    }
}
