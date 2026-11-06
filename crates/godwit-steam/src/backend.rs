// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Steam's network thread: signing in (auth.rs, over HTTPS) and the client
//! connection (steam-vent), turned into [`Ev`]s for the account. The UI sends
//! [`Cmd`]s; nothing here polls Steam except what its sign-in asks for (at
//! the interval it gives), and reconnecting waits longer after each failure.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use futures_util::StreamExt;
use steam_vent::{Connection, ConnectionError, ConnectionTrait, LoginError, ServerList};
use steam_vent_proto_steam::steammessages_chat_steamclient::{
    CChatRoom_ChatRoomGroupRoomsChange_Notification, CChatRoom_IncomingChatMessage_Notification,
    CChatRoom_MemberStateChange_Notification, ChatRoomClient_NotifyChatGroupUserStateChanged_Notification,
};
use steam_vent_proto_steam::steammessages_clientserver_friends::{
    CMsgClientChangeStatus, CMsgClientEmoticonList, CMsgClientFriendsList, CMsgClientGetEmoticonList,
    CMsgClientPersonaState, CMsgClientRequestFriendData,
};
use steam_vent_proto_steam::steammessages_clientserver_login::CMsgClientLoggedOff;
use steam_vent_proto_steam::steammessages_friendmessages_steamclient::{
    CFriendMessages_AckMessage_Notification, CFriendMessages_GetRecentMessages_Request,
    CFriendMessages_IncomingMessage_Notification, CFriendMessages_SendMessage_Request,
    CFriendsMessages_GetActiveMessageSessions_Request,
};
use tokio::sync::mpsc as tmpsc;

use godwit_voice::call::VoiceSettings;

use crate::auth::{self, Guard, Poll};
use crate::session::Saved;
use crate::voice::session::{self, SessionCmd, SessionEvent};
use crate::voice::signaling::{self, VoiceEvent};

pub use godwit_core::backend::Waker;

/// Persona details asked for: status, name, presence, game (Steam's
/// EClientPersonaStateFlag: 1 | 2 | 16 | 256).
const PERSONA_FLAGS: u32 = 1 | 2 | 16 | 256;
/// Chat entry types (EChatEntryType).
const ENTRY_MESSAGE: i32 = 1;
const ENTRY_TYPING: i32 = 2;
/// How long "typing" shows after Steam says so (it says it again while it lasts).
const TYPING_FOR: Duration = Duration::from_secs(8);
/// A page of history.
const PAGE: u32 = 50;
/// Waits before reconnecting after a dropped connection, growing per try.
/// Steam sometimes resets a first connection: the first retries come soon,
/// later ones back off so a real outage isn't hammered.
const RECONNECT: [u64; 6] = [3, 10, 30, 60, 300, 900];

/// From the account to the network thread.
pub enum Cmd {
    RemoveFriend(u64),
    AddFriend {
        request: Option<u64>,
        name: String,
    },
    BeginQr,
    BeginPassword {
        account: String,
        password: String,
    },
    /// A Steam Guard code for the password sign-in under way.
    SubmitCode(String),
    CancelLogin,
    /// Connect with a kept sign-in.
    UseSession(Saved),
    /// Sign out: the connection closes.
    Logout,
    Send {
        friend: u64,
        text: String,
        nonce: String,
    },
    Typing {
        friend: u64,
    },
    /// A page of history with `friend`, before `(time, ordinal)` (None: newest).
    History {
        friend: u64,
        before: Option<(u32, u32)>,
    },
    /// Read up to `time`.
    Ack {
        friend: u64,
        time: u32,
    },
    /// Our persona state (EPersonaState).
    SetPersona(u32),
    /// Ask for these accounts' names, presence and games.
    RequestPersonas(Vec<u64>),
    /// Ask for these Steam games' names (the persona gives only their app id).
    AppNames(Vec<u32>),
    /// Call `partner`, or answer Steam's call `incoming` from them; `call`
    /// tags its [`Ev::Call`]s. Ends any call under way first.
    Call {
        call: u64,
        partner: u64,
        incoming: Option<u64>,
        settings: Box<VoiceSettings>,
    },
    /// To the call under way.
    CallCmd(SessionCmd),
    /// Turn down `partner`'s call `voice_chat` ringing us (as Steam's client
    /// does: `AnswerOneOnOneChat` with `accepted_request: false`).
    DeclineCall {
        voice_chat: u64,
        partner: u64,
    },
    /// Take the connection down (staying signed in), or bring it back.
    SetOffline(bool),
    /// Fetch the group chats again (answered with [`Ev::Groups`]).
    Groups,
    /// An invite card's group (answered with [`Ev::InvitePreview`]).
    InviteSummary {
        code: String,
        group: u64,
    },
    /// Accept a group chat invite (answered with [`Ev::GroupJoined`]).
    JoinGroup {
        code: String,
        group: u64,
        chat: u64,
    },
    GroupSend {
        group: u64,
        chat: u64,
        text: String,
        nonce: String,
    },
    /// A page of a group room's history before `(time, ordinal)` (None: newest).
    GroupHistory {
        group: u64,
        chat: u64,
        before: Option<(u32, u32)>,
    },
    /// Read a group room up to `time`.
    GroupAck {
        group: u64,
        chat: u64,
        time: u32,
    },
    /// Join a group room's voice chat (ending any call first); `call` tags its events.
    GroupCall {
        call: u64,
        group: u64,
        chat: u64,
        settings: Box<VoiceSettings>,
    },
}

/// A friend's persona, as far as Steam said.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Persona {
    pub steam_id: u64,
    pub name: Option<String>,
    /// EPersonaState (0 offline, 1 online, 2 busy, 3 away, 4 snooze, 5/6 looking, 7 invisible).
    pub state: Option<u32>,
    pub avatar_hash: Option<Vec<u8>>,
    /// The game being played: its name (non-Steam ones), or app id.
    pub game_name: Option<String>,
    pub game_app: Option<u32>,
}

/// A chat line.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// The sender's account id (the lower 32 bits of their Steam id).
    pub from: u32,
    pub time: u32,
    pub ordinal: u32,
    pub text: String,
}

/// From the network thread to the account.
#[derive(Debug)]
pub enum Ev {
    FriendRequest {
        request: Option<u64>,
        result: Result<(), String>,
    },
    /// Show this as a QR code (a new one replaces the old).
    Qr {
        url: String,
        scanned: bool,
    },
    /// The password sign-in needs a code (email or the mobile app).
    NeedsCode(Guard),
    /// The password sign-in waits for approval in the mobile app or email.
    NeedsApproval(Guard),
    LoginFailed(String),
    /// Signed in: keep this.
    SignedIn(Saved),
    Connecting,
    Connected {
        me: u64,
    },
    /// Own persona didn't come in time: ready anyway.
    ReadyDeadline,
    /// The connection is gone; `fatal`: the sign-in is no good any more.
    Disconnected {
        reason: String,
        fatal: bool,
    },
    /// (Steam id, EFriendRelationship) for each; `incremental`: only changes.
    FriendsList {
        friends: Vec<(u64, u32)>,
        incremental: bool,
    },
    Personas(Vec<Persona>),
    /// Steam games' names, by app id, as asked with [`Cmd::AppNames`].
    AppNames(Vec<(u32, String)>),
    /// A message with `friend` (`mine`: ours, from another of our devices).
    Incoming {
        friend: u64,
        mine: bool,
        line: Line,
    },
    /// `friend` is typing (`typing`), or stopped. `seq` grows per friend:
    /// a stop with an older one than the latest start is stale.
    Typing {
        friend: u64,
        typing: bool,
        seq: u64,
    },
    Sent {
        nonce: String,
        friend: u64,
        line: Line,
    },
    SendFailed {
        nonce: String,
        friend: u64,
        error: String,
    },
    History {
        friend: u64,
        before: Option<(u32, u32)>,
        lines: Vec<Line>,
        more: bool,
    },
    HistoryFailed {
        friend: u64,
    },
    /// Chats with something in them: (friend's account id, last message,
    /// last read, unread count).
    Sessions(Vec<(u32, u32, u32, u32)>),
    /// The account's group chats, all of them.
    Groups(Vec<crate::groups::GroupInfo>),
    /// An invite card's group, with how many are in it (None: gone or not allowed).
    InvitePreview {
        code: String,
        group: Option<(crate::groups::GroupInfo, u32)>,
    },
    GroupJoined {
        code: String,
        group: u64,
        result: Result<(), String>,
    },
    /// A message in a group room (`mentioned`: it mentions us, @all or @here).
    GroupIncoming {
        group: u64,
        chat: u64,
        line: Line,
        mentioned: bool,
    },
    GroupSent {
        nonce: String,
        group: u64,
        chat: u64,
        line: Line,
    },
    GroupSendFailed {
        nonce: String,
        group: u64,
        chat: u64,
        error: String,
    },
    GroupHistory {
        group: u64,
        chat: u64,
        before: Option<(u32, u32)>,
        lines: Vec<Line>,
        more: bool,
    },
    GroupHistoryFailed {
        group: u64,
        chat: u64,
    },
    /// The emoticons and stickers the account owns, recently used first
    /// (emoticon names without their colons).
    Emoticons {
        emoticons: Vec<String>,
        stickers: Vec<String>,
    },
    /// Something to log for the user (Settings → Problems).
    Notice(String),
    /// Persona states were missed: ask for these friends' again
    /// ([`Cmd::RequestPersonas`]).
    NeedPersonas,
    /// From the call started with [`Cmd::Call`] `call`.
    Call {
        call: u64,
        event: SessionEvent,
    },
    /// A friend is calling us (Steam's call id `voice_chat`).
    IncomingCall {
        voice_chat: u64,
        partner: u64,
    },
    /// A voice chat ended (a call we didn't answer stops ringing).
    VoiceChatEnded {
        voice_chat: u64,
    },
}

#[derive(Clone)]
struct Out {
    events: godwit_core::backend::Events<Ev>,
    /// The loop's current generation (sign-in or connection).
    current: Arc<AtomicU64>,
    /// The generation this sender's work started in, if tagged.
    generation: Option<u64>,
}

impl Out {
    fn send(&self, ev: Ev) {
        // Work from a replaced connection or sign-in (history, personas,
        // listeners) must not land after the new one's. Answers to a specific
        // request still count: the message was sent, the request was made.
        let answer = matches!(
            ev,
            Ev::Sent { .. } | Ev::SendFailed { .. } | Ev::FriendRequest { .. } | Ev::Call { .. }
        );
        if !answer
            && let Some(generation) = self.generation
            && generation != self.current.load(Ordering::Acquire)
        {
            return;
        }
        self.events.send(ev);
    }

    /// For work started now: its results are dropped once the generation moves on.
    fn tagged(&self, generation: u64) -> Out {
        Out {
            generation: Some(generation),
            ..self.clone()
        }
    }
}

pub type Handle = godwit_core::backend::Handle<Cmd>;

pub fn spawn(wake: Waker) -> std::io::Result<(Handle, mpsc::Receiver<Ev>)> {
    godwit_core::backend::spawn(
        "steam-net",
        // One thread is plenty for a chat connection.
        tokio::runtime::Builder::new_current_thread(),
        move |tx| Out {
            events: godwit_core::backend::Events::new(tx, wake),
            current: Arc::new(AtomicU64::new(0)),
            generation: None,
        },
        run,
        |out, e| out.send(Ev::LoginFailed(format!("Couldn't start Steam's network thread: {e}"))),
    )
}

/// What the loop learns from its own tasks.
enum Inner {
    Connected(Arc<Connection>, Saved),
    ConnectFailed { saved: Saved, reason: String, fatal: bool },
    Dropped,
    Reconnect,
}

/// Tags background connection work so logout/new sign-in invalidates it.
#[derive(Clone)]
struct InnerSender {
    generation: u64,
    tx: tmpsc::UnboundedSender<(u64, Inner)>,
}

impl InnerSender {
    fn send(&self, event: Inner) -> bool {
        self.tx.send((self.generation, event)).is_ok()
    }
}

struct State {
    generation: u64,
    http: Option<reqwest::Client>,
    login: Option<tokio::task::AbortHandle>,
    /// Codes for the password sign-in under way.
    codes: Option<tmpsc::UnboundedSender<String>>,
    conn: Option<Arc<Connection>>,
    /// Tasks reading the connection: stopped with it.
    tasks: Vec<tokio::task::AbortHandle>,
    saved: Option<Saved>,
    tries: usize,
    /// The call under way.
    call: Option<tmpsc::UnboundedSender<SessionCmd>>,
    /// Taken offline by the user: no connecting until they bring it back.
    offline: bool,
}

async fn run(mut cmds: tmpsc::UnboundedReceiver<Cmd>, out: Out) {
    let (inner_tx, mut inner) = tmpsc::unbounded_channel::<(u64, Inner)>();
    let mut st = State {
        generation: 0,
        http: None,
        login: None,
        codes: None,
        conn: None,
        tasks: Vec::new(),
        saved: None,
        tries: 0,
        call: None,
        offline: false,
    };
    loop {
        tokio::select! {
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { break };
                if matches!(&cmd, Cmd::BeginQr | Cmd::BeginPassword { .. } | Cmd::UseSession(_) | Cmd::CancelLogin | Cmd::Logout | Cmd::SetOffline(_)) {
                    st.generation = st.generation.wrapping_add(1);
                    out.current.store(st.generation, Ordering::Release);
                }
                let sender = InnerSender { generation: st.generation, tx: inner_tx.clone() };
                let tagged = out.tagged(st.generation);
                command(&mut st, cmd, &tagged, &sender);
            }
            Some((generation, msg)) = inner.recv() => {
                if generation != st.generation { continue; }
                let inner_tx = InnerSender { generation, tx: inner_tx.clone() };
                match msg {
                Inner::Connected(conn, saved) => {
                    st.tries = 0;
                    let me = conn.steam_id().steam64();
                    st.conn = Some(conn.clone());
                    st.saved = Some(saved);
                    diag("connected");
                    out.send(Ev::Connected { me });
                    st.tasks = listen(conn, &out.tagged(generation), &inner_tx);
                }
                Inner::ConnectFailed { saved, reason, fatal } => {
                    diag(&format!("connecting failed (signs out: {fatal}): {reason}"));
                    if fatal {
                        st.saved = None;
                        out.send(Ev::Disconnected { reason, fatal: true });
                    } else {
                        out.send(Ev::Disconnected { reason, fatal: false });
                        st.saved = Some(saved);
                        schedule_reconnect(&mut st, &inner_tx);
                    }
                }
                Inner::Dropped => {
                    stop_connection(&mut st);
                    if st.saved.is_some() && !st.offline {
                        out.send(Ev::Disconnected { reason: "Connection to Steam lost; reconnecting.".into(), fatal: false });
                        schedule_reconnect(&mut st, &inner_tx);
                    }
                }
                Inner::Reconnect => {
                    if st.conn.is_none() && !st.offline && let Some(saved) = st.saved.clone() {
                        st.generation = st.generation.wrapping_add(1);
                        out.current.store(st.generation, Ordering::Release);
                        out.send(Ev::Connecting);
                        connect(saved, InnerSender { generation: st.generation, tx: inner_tx.tx.clone() });
                    }
                }
                }
            }
        }
    }
    stop_connection(&mut st);
}

fn schedule_reconnect(st: &mut State, inner: &InnerSender) {
    let wait = RECONNECT.get(st.tries).or(RECONNECT.last()).copied().unwrap_or(900);
    st.tries += 1;
    let inner = inner.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(wait)).await;
        let _ = inner.send(Inner::Reconnect);
    });
}

fn stop_connection(st: &mut State) {
    end_call(st);
    for task in st.tasks.drain(..) {
        task.abort();
    }
    st.conn = None;
}

/// Hangs up the call under way (it tells Steam and reports its end itself).
fn end_call(st: &mut State) {
    if let Some(call) = st.call.take() {
        let _ = call.send(SessionCmd::Leave);
    }
}

fn http(st: &mut State) -> Result<reqwest::Client, String> {
    if let Some(h) = &st.http {
        return Ok(h.clone());
    }
    let client = auth::client().map_err(|e| e.0)?;
    st.http = Some(client.clone());
    Ok(client)
}

fn command(st: &mut State, cmd: Cmd, out: &Out, inner: &InnerSender) {
    match cmd {
        Cmd::RemoveFriend(friend) => {
            let Some(conn) = st.conn.clone() else {
                return out.send(Ev::Notice(
                    "Steam is disconnected. Try again after reconnecting.".into(),
                ));
            };
            let out = out.clone();
            tokio::spawn(async move {
                use steam_vent_proto_steam::steammessages_clientserver_friends::CMsgClientRemoveFriend;
                if conn
                    .send(CMsgClientRemoveFriend {
                        friendid: Some(friend),
                        ..Default::default()
                    })
                    .await
                    .is_err()
                {
                    out.send(Ev::Notice("Couldn't send the Steam friend removal request.".into()));
                }
            });
        }
        Cmd::AddFriend {
            request: request_id,
            name,
        } => {
            let Some(conn) = st.conn.clone() else {
                return out.send(Ev::FriendRequest {
                    request: request_id,
                    result: Err("Steam is disconnected. Try again after reconnecting.".into()),
                });
            };
            let out = out.clone();
            tokio::spawn(async move {
                use steam_vent_proto_steam::steammessages_clientserver_friends::{
                    CMsgClientAddFriend, CMsgClientAddFriendResponse,
                };
                let id = name.parse::<u64>().ok().filter(|id| is_individual(*id));
                let request = CMsgClientAddFriend {
                    steamid_to_add: id,
                    accountname_or_email_to_add: id.is_none().then_some(name),
                    ..Default::default()
                };
                let result = match tokio::time::timeout(Duration::from_secs(15), conn.job::<_, CMsgClientAddFriendResponse>(request)).await {
                    Ok(Ok(response)) if response.eresult == Some(1) => Ok(()),
                    Ok(Ok(_)) => Err("Steam couldn't add that friend. Check their account name or Steam ID and your account's friend restrictions.".into()),
                    _ => Err("Couldn't send the Steam friend request. Try again later.".into()),
                };
                out.send(Ev::FriendRequest {
                    request: request_id,
                    result,
                });
            });
        }
        Cmd::BeginQr => {
            cancel_login(st);
            let http = match http(st) {
                Ok(h) => h,
                Err(e) => return out.send(Ev::LoginFailed(e)),
            };
            let (out, inner) = (out.clone(), inner.clone());
            st.login = Some(tokio::spawn(qr_login(http, out, inner)).abort_handle());
        }
        Cmd::BeginPassword { account, password } => {
            cancel_login(st);
            let http = match http(st) {
                Ok(h) => h,
                Err(e) => return out.send(Ev::LoginFailed(e)),
            };
            let (codes_tx, codes) = tmpsc::unbounded_channel();
            st.codes = Some(codes_tx);
            let (out, inner) = (out.clone(), inner.clone());
            st.login = Some(tokio::spawn(password_login(http, account, password, codes, out, inner)).abort_handle());
        }
        Cmd::SubmitCode(code) => {
            if let Some(codes) = &st.codes {
                let _ = codes.send(code);
            }
        }
        Cmd::CancelLogin => cancel_login(st),
        Cmd::UseSession(saved) => {
            st.saved = Some(saved.clone());
            out.send(Ev::Connecting);
            connect(saved, inner.clone());
        }
        Cmd::Call {
            call,
            partner,
            incoming,
            settings,
        } => {
            end_call(st);
            let Some(conn) = st.conn.clone() else {
                return out.send(Ev::Call {
                    call,
                    event: SessionEvent::Ended(Some("Not connected to Steam.".into())),
                });
            };
            let (tx, rx) = tmpsc::unbounded_channel();
            st.call = Some(tx);
            let out = out.clone();
            tokio::spawn(session::run(conn, partner, incoming, *settings, rx, move |event| {
                out.send(Ev::Call { call, event })
            }));
        }
        Cmd::SetOffline(offline) => {
            st.offline = offline;
            if offline {
                // The loop moved to a new generation: work from the old
                // connection is dropped, and stop_connection ends any call.
                stop_connection(st);
                st.tries = 0;
            } else if st.conn.is_none()
                && let Some(saved) = st.saved.clone()
            {
                out.send(Ev::Connecting);
                connect(saved, inner.clone());
            }
        }
        Cmd::InviteSummary { code, group } => {
            let Some(conn) = st.conn.clone() else {
                return out.send(Ev::InvitePreview { code, group: None });
            };
            let out = out.clone();
            tokio::spawn(async move {
                let group = crate::groups::summary(&conn, group).await.ok();
                out.send(Ev::InvitePreview { code, group });
            });
        }
        Cmd::JoinGroup { code, group, chat } => {
            let Some(conn) = st.conn.clone() else {
                return out.send(Ev::GroupJoined {
                    code,
                    group,
                    result: Err("Not connected to Steam.".into()),
                });
            };
            let out = out.clone();
            tokio::spawn(async move {
                let result = crate::groups::join(&conn, group, chat).await;
                if result.is_ok() {
                    fetch_groups(conn.clone(), out.clone()).await;
                }
                out.send(Ev::GroupJoined { code, group, result });
            });
        }
        Cmd::Groups => {
            if let Some(conn) = st.conn.clone() {
                tokio::spawn(fetch_groups(conn, out.clone()));
            }
        }
        Cmd::GroupSend {
            group,
            chat,
            text,
            nonce,
        } => {
            let Some(conn) = st.conn.clone() else {
                return out.send(Ev::GroupSendFailed {
                    nonce,
                    group,
                    chat,
                    error: "Not connected to Steam.".into(),
                });
            };
            let out = out.clone();
            tokio::spawn(async move {
                match crate::groups::send(&conn, group, chat, &text).await {
                    Ok(line) => out.send(Ev::GroupSent {
                        nonce,
                        group,
                        chat,
                        line,
                    }),
                    Err(error) => out.send(Ev::GroupSendFailed {
                        nonce,
                        group,
                        chat,
                        error,
                    }),
                }
            });
        }
        Cmd::GroupHistory { group, chat, before } => {
            let Some(conn) = st.conn.clone() else {
                return out.send(Ev::GroupHistoryFailed { group, chat });
            };
            let out = out.clone();
            tokio::spawn(async move {
                match crate::groups::history(&conn, group, chat, before).await {
                    Ok((lines, more)) => out.send(Ev::GroupHistory {
                        group,
                        chat,
                        before,
                        lines,
                        more,
                    }),
                    Err(_) => out.send(Ev::GroupHistoryFailed { group, chat }),
                }
            });
        }
        Cmd::GroupAck { group, chat, time } => {
            if let Some(conn) = st.conn.clone() {
                tokio::spawn(async move { crate::groups::ack(&conn, group, chat, time).await });
            }
        }
        Cmd::GroupCall {
            call,
            group,
            chat,
            settings,
        } => {
            end_call(st);
            let Some(conn) = st.conn.clone() else {
                return out.send(Ev::Call {
                    call,
                    event: SessionEvent::Ended(Some("Not connected to Steam.".into())),
                });
            };
            let (tx, rx) = tmpsc::unbounded_channel();
            st.call = Some(tx);
            let out = out.clone();
            tokio::spawn(session::run_group(conn, group, chat, *settings, rx, move |event| {
                out.send(Ev::Call { call, event })
            }));
        }
        Cmd::DeclineCall { voice_chat, partner } => {
            if let Some(conn) = st.conn.clone() {
                tokio::spawn(async move {
                    // Steam answers a decline with Fail though it worked (vaporchat's
                    // `failIsExpected`): the outcome doesn't matter.
                    let _ = crate::voice::signaling::answer_one_on_one(&conn, voice_chat, partner, false).await;
                });
            }
        }
        Cmd::CallCmd(cmd) => {
            if matches!(cmd, SessionCmd::Leave) {
                end_call(st);
            } else if let Some(call) = &st.call {
                let _ = call.send(cmd);
            }
        }
        Cmd::Logout => {
            end_call(st);
            cancel_login(st);
            let saved = st.saved.take();
            let conn = st.conn.clone();
            // The listeners stop now; the connection itself lives on in the
            // task below until Steam has revoked the sign-in.
            for task in st.tasks.drain(..) {
                task.abort();
            }
            st.conn = None;
            out.send(Ev::Disconnected {
                reason: String::new(),
                fatal: true,
            });
            let Some(saved) = saved else { return };
            let out = out.clone();
            tokio::spawn(async move {
                let revoked = match conn {
                    Some(conn) => revoke(&conn, &saved.refresh_token).await,
                    None => false,
                };
                if !revoked {
                    out.send(Ev::Notice(format!(
                        "Steam couldn't be told about signing out, so this sign-in still works there. Remove \"{}\" under Steam → Settings → Security → Authorized devices.",
                        if saved.device_name.is_empty() { "the Godwit device" } else { saved.device_name.as_str() }
                    )));
                }
            });
        }
        Cmd::Send { friend, text, nonce } => {
            let Some(conn) = st.conn.clone() else {
                return out.send(Ev::SendFailed {
                    nonce,
                    friend,
                    error: "Not connected to Steam.".into(),
                });
            };
            let out = out.clone();
            tokio::spawn(async move {
                let req = CFriendMessages_SendMessage_Request {
                    steamid: Some(friend),
                    chat_entry_type: Some(ENTRY_MESSAGE),
                    // As Steam's clients send it (vaporchat's `sendMessage`): BBCode,
                    // with our own brackets escaped, so Steam turns `:name:` into
                    // our emoticons and `/sticker <name>` into the sticker.
                    message: Some(text.replace('[', "\\[")),
                    contains_bbcode: Some(true),
                    ..Default::default()
                };
                match conn.service_method(req).await {
                    Ok(res) => {
                        let line = Line {
                            from: account_id(conn.steam_id().steam64()),
                            time: res.server_timestamp.unwrap_or_else(now),
                            ordinal: res.ordinal.unwrap_or_default(),
                            // As Steam keeps it: its markup, our brackets escaped.
                            text: res
                                .modified_message
                                .clone()
                                .filter(|t| !t.is_empty())
                                .unwrap_or_else(|| text.replace('[', "\\[")),
                        };
                        out.send(Ev::Sent { nonce, friend, line });
                    }
                    Err(e) => out.send(Ev::SendFailed {
                        nonce,
                        friend,
                        error: format!("Steam didn't take the message: {e}"),
                    }),
                }
            });
        }
        Cmd::Typing { friend } => {
            if let Some(conn) = st.conn.clone() {
                tokio::spawn(async move {
                    let req = CFriendMessages_SendMessage_Request {
                        steamid: Some(friend),
                        chat_entry_type: Some(ENTRY_TYPING),
                        ..Default::default()
                    };
                    let _ = conn.service_method(req).await;
                });
            }
        }
        Cmd::History { friend, before } => {
            let Some(conn) = st.conn.clone() else {
                return out.send(Ev::HistoryFailed { friend });
            };
            let out = out.clone();
            tokio::spawn(async move {
                let req = CFriendMessages_GetRecentMessages_Request {
                    steamid1: Some(conn.steam_id().steam64()),
                    steamid2: Some(friend),
                    count: Some(PAGE),
                    most_recent_conversation: Some(false),
                    // Steam's markup: emoticons, stickers, links (bbcode.rs).
                    bbcode_format: Some(true),
                    time_last: before.map(|b| b.0),
                    ordinal_last: before.map(|b| b.1),
                    ..Default::default()
                };
                match conn.service_method(req).await {
                    Ok(res) => {
                        let lines = res
                            .messages
                            .iter()
                            .map(|m| Line {
                                from: m.accountid.unwrap_or_default(),
                                time: m.timestamp.unwrap_or_default(),
                                ordinal: m.ordinal.unwrap_or_default(),
                                text: m.message.clone().unwrap_or_default(),
                            })
                            // Steam includes the message paged from: drop it.
                            .filter(|l| before != Some((l.time, l.ordinal)))
                            .collect();
                        out.send(Ev::History {
                            friend,
                            before,
                            lines,
                            more: res.more_available.unwrap_or(false),
                        });
                    }
                    Err(_) => out.send(Ev::HistoryFailed { friend }),
                }
            });
        }
        Cmd::Ack { friend, time } => {
            if let Some(conn) = st.conn.clone() {
                tokio::spawn(async move {
                    let req = CFriendMessages_AckMessage_Notification {
                        steamid_partner: Some(friend),
                        timestamp: Some(time),
                        ..Default::default()
                    };
                    let _ = conn.service_method(req).await;
                });
            }
        }
        Cmd::RequestPersonas(ids) => {
            if let Some(conn) = st.conn.clone() {
                tokio::spawn(request_personas(conn, ids));
            }
        }
        Cmd::AppNames(apps) => {
            if let Some(conn) = st.conn.clone() {
                let out = out.clone();
                tokio::spawn(async move {
                    let names = app_names(&conn, apps).await;
                    if !names.is_empty() {
                        out.send(Ev::AppNames(names));
                    }
                });
            }
        }
        Cmd::SetPersona(state) => {
            if let Some(conn) = st.conn.clone() {
                tokio::spawn(async move {
                    let _ = conn
                        .send(CMsgClientChangeStatus {
                            persona_state: Some(state),
                            persona_set_by_user: Some(true),
                            need_persona_response: Some(true),
                            ..Default::default()
                        })
                        .await;
                });
            }
        }
    }
}

fn cancel_login(st: &mut State) {
    if let Some(task) = st.login.take() {
        task.abort();
    }
    st.codes = None;
}

/// QR sign-in: show the code, wait for the app, renew the code when Steam does.
async fn qr_login(http: reqwest::Client, out: Out, inner: InnerSender) {
    let device = auth::device_name();
    let mut session = match auth::begin_qr(&http, &device).await {
        Ok(s) => s,
        Err(e) => return out.send(Ev::LoginFailed(e.0)),
    };
    let Some(url) = session.challenge_url.clone() else {
        return out.send(Ev::LoginFailed("Steam didn't send a QR code; try again.".into()));
    };
    out.send(Ev::Qr { url, scanned: false });
    let mut scanned = false;
    loop {
        tokio::time::sleep(session.interval).await;
        match auth::poll(&http, &session).await {
            Ok(Poll::Done(tokens)) => return signed_in(Saved::new(tokens, device), &out, &inner),
            Ok(Poll::Waiting {
                new_challenge_url,
                new_client_id,
                scanned: now_scanned,
            }) => {
                if let Some(id) = new_client_id {
                    session.client_id = id;
                }
                if let Some(url) = new_challenge_url {
                    session.challenge_url = Some(url.clone());
                    scanned = false;
                    out.send(Ev::Qr { url, scanned });
                } else if now_scanned && !scanned {
                    scanned = true;
                    if let Some(url) = session.challenge_url.clone() {
                        out.send(Ev::Qr { url, scanned });
                    }
                }
            }
            Err(e) => return out.send(Ev::LoginFailed(e.0)),
        }
    }
}

/// Password sign-in: a code when one's asked for, or approval elsewhere.
async fn password_login(
    http: reqwest::Client,
    account: String,
    password: String,
    mut codes: tmpsc::UnboundedReceiver<String>,
    out: Out,
    inner: InnerSender,
) {
    let device = auth::device_name();
    let session = match auth::begin_password(&http, &device, &account, &password, None).await {
        Ok(s) => s,
        Err(e) => return out.send(Ev::LoginFailed(e.0)),
    };
    drop(password);
    // Steam lists the ways in its order of preference.
    let code = session.guards.iter().find(|g| g.wants_code()).cloned();
    let approval = session
        .guards
        .iter()
        .find(|g| matches!(g, Guard::DeviceConfirmation | Guard::EmailConfirmation))
        .cloned();
    match (&code, &approval) {
        (Some(code), _) => out.send(Ev::NeedsCode(code.clone())),
        (None, Some(approval)) => out.send(Ev::NeedsApproval(approval.clone())),
        _ => {}
    }
    loop {
        tokio::select! {
            Some(typed) = codes.recv() => {
                let kind = code.clone().unwrap_or(Guard::DeviceCode);
                if let Err(e) = auth::submit_code(&http, &session, &typed, &kind).await {
                    out.send(Ev::LoginFailed(e.0));
                    out.send(Ev::NeedsCode(kind));
                    continue;
                }
            }
            () = tokio::time::sleep(session.interval) => {}
        }
        match auth::poll(&http, &session).await {
            Ok(Poll::Done(tokens)) => return signed_in(Saved::new(tokens, device), &out, &inner),
            Ok(Poll::Waiting { .. }) => {}
            Err(e) => return out.send(Ev::LoginFailed(e.0)),
        }
    }
}

fn signed_in(saved: Saved, out: &Out, inner: &InnerSender) {
    out.send(Ev::SignedIn(saved.clone()));
    out.send(Ev::Connecting);
    connect(saved, inner.clone());
}

/// Ends this sign-in at Steam, as its client does on signing out: the
/// refresh token stops working everywhere. True if Steam confirmed.
async fn revoke(conn: &Connection, refresh_token: &str) -> bool {
    use steam_vent_proto_common::protobuf::EnumOrUnknown;
    use steam_vent_proto_steam::steammessages_auth_steamclient::{
        CAuthentication_Token_Revoke_Request, EAuthTokenRevokeAction,
    };
    let req = CAuthentication_Token_Revoke_Request {
        token: Some(refresh_token.to_owned()),
        revoke_action: Some(EnumOrUnknown::new(EAuthTokenRevokeAction::k_EAuthTokenRevokeLogout)),
        ..Default::default()
    };
    matches!(
        tokio::time::timeout(Duration::from_secs(15), conn.service_method(req)).await,
        Ok(Ok(_))
    )
}

/// Connects the client with a kept sign-in.
fn connect(saved: Saved, inner: InnerSender) {
    tokio::spawn(async move {
        if auth::token_expiry(&saved.refresh_token).is_some_and(|exp| exp <= u64::from(now())) {
            let _ = inner.send(Inner::ConnectFailed {
                saved,
                reason: "The Steam sign-in expired. Sign in again.".into(),
                fatal: true,
            });
            return;
        }
        let result = async {
            let servers = ServerList::discover()
                .await
                .map_err(|e| (format!("Couldn't find Steam's servers: {e}"), false))?;
            // The device name this sign-in was given, so the account's
            // device list shows one device rather than also "Godwit".
            let machine = if saved.device_name.is_empty() {
                "DESKTOP-PC"
            } else {
                saved.device_name.as_str()
            };
            Connection::access(&servers, &saved.account_name, &saved.refresh_token, machine)
                .await
                .map_err(|e| describe(&e))
        }
        .await;
        let _ = inner.send(match result {
            Ok(conn) => Inner::Connected(Arc::new(conn), saved),
            Err((reason, fatal)) => Inner::ConnectFailed { saved, reason, fatal },
        });
    });
}

/// What a failed connection means, and whether signing in again is needed.
fn describe(e: &ConnectionError) -> (String, bool) {
    match e {
        ConnectionError::AccessToken(_)
        | ConnectionError::LoginError(
            LoginError::InvalidCredentials | LoginError::UnavailableAccount | LoginError::InvalidSteamId,
        ) => ("Steam no longer accepts this sign-in. Sign in again.".into(), true),
        ConnectionError::LoginError(LoginError::RateLimited) => {
            ("Steam asked us to wait before connecting again.".into(), false)
        }
        other => (format!("Couldn't connect to Steam: {other}"), false),
    }
}

/// After connecting: say we're online, ask for friends' personas and the
/// chats with something in them, and read what Steam sends.
fn listen(conn: Arc<Connection>, out: &Out, inner: &InnerSender) -> Vec<tokio::task::AbortHandle> {
    let me = conn.steam_id().steam64();
    let mut tasks = Vec::new();

    // Subscribe before anything else, so nothing falls between reading what
    // came early and listening for the rest.
    let mut lists = conn.on::<CMsgClientFriendsList>();
    let mut states = conn.on::<CMsgClientPersonaState>();
    let mut incoming = conn.on_notification::<CFriendMessages_IncomingMessage_Notification>();
    let mut voice = signaling::events(&conn);
    let mut logged_off = conn.on::<CMsgClientLoggedOff>();
    let mut emoticon_lists = conn.on::<CMsgClientEmoticonList>();
    let mut group_messages = conn.on_notification::<CChatRoom_IncomingChatMessage_Notification>();
    let mut group_rooms = conn.on_notification::<CChatRoom_ChatRoomGroupRoomsChange_Notification>();
    let mut group_states = conn.on_notification::<ChatRoomClient_NotifyChatGroupUserStateChanged_Notification>();
    let mut member_states = conn.on_notification::<CChatRoom_MemberStateChange_Notification>();

    // Messages that came before we listened (the friends list arrives with
    // the sign-in): read them first.
    let early = conn.take_unprocessed();
    diag(&format!("{} messages came before listening", early.len()));
    for raw in early {
        if let Ok(list) = raw.clone().into_message::<CMsgClientFriendsList>() {
            friends_list(&conn, &list, me, out);
        } else if let Ok(list) = raw.clone().into_message::<CMsgClientEmoticonList>() {
            out.send(emoticons(&list));
        } else if let Ok(state) = raw.into_message::<CMsgClientPersonaState>() {
            out.send(Ev::Personas(personas(&state)));
        }
    }

    // Steam ending our session (signed in elsewhere, maintenance): say why. The
    // connection closing then ends the listeners, and we reconnect.
    tasks.push(
        tokio::spawn(async move {
            if let Some(Ok(off)) = logged_off.next().await {
                diag(&format!("Steam logged this session off (EResult {})", off.eresult()));
            }
        })
        .abort_handle(),
    );

    // The emoticons and stickers we own (asked for below; Steam sends the
    // list again when it changes).
    let o = out.clone();
    tasks.push(
        tokio::spawn(async move {
            while let Some(item) = emoticon_lists.next().await {
                if let Ok(list) = item {
                    o.send(emoticons(&list));
                }
            }
        })
        .abort_handle(),
    );

    // Group chats: their messages, and changes worth fetching the list for
    // (rooms added or renamed, joining or leaving a group, members).
    let o = out.clone();
    tasks.push(
        tokio::spawn(async move {
            while let Some(item) = group_messages.next().await {
                let Ok(n) = item else { continue };
                if n.server_message.is_some() {
                    continue;
                }
                let mentioned = n.mentions.as_ref().is_some_and(|m| {
                    m.mention_all() || m.mention_here() || m.mention_accountids.contains(&account_id(me))
                });
                o.send(Ev::GroupIncoming {
                    group: n.chat_group_id(),
                    chat: n.chat_id(),
                    line: Line {
                        from: account_id(n.steamid_sender()),
                        time: n.timestamp(),
                        ordinal: n.ordinal(),
                        text: n.message().to_owned(),
                    },
                    mentioned,
                });
            }
        })
        .abort_handle(),
    );
    let c = conn.clone();
    let o = out.clone();
    tasks.push(
        tokio::spawn(async move {
            loop {
                let more = tokio::select! {
                    n = group_rooms.next() => n.is_some(),
                    n = group_states.next() => n.is_some(),
                    n = member_states.next() => n.is_some(),
                };
                if !more {
                    break;
                }
                // Changes come in bursts (a room added, members moving): fetch
                // once they settle, not per notification.
                tokio::time::sleep(Duration::from_secs(2)).await;
                fetch_groups(c.clone(), o.clone()).await;
            }
        })
        .abort_handle(),
    );

    // Calls ringing us, and calls ending before we answered.
    let o = out.clone();
    tasks.push(
        tokio::spawn(async move {
            while let Some(event) = voice.next().await {
                diag(&format!(
                    "voice notification: {}",
                    crate::voice::signaling::describe(&event)
                ));
                match event {
                    VoiceEvent::IncomingCall { voice_chat, partner } => {
                        o.send(Ev::IncomingCall { voice_chat, partner })
                    }
                    VoiceEvent::Ended(chat) => o.send(Ev::VoiceChatEnded {
                        voice_chat: chat.voice_chat,
                    }),
                    _ => {}
                }
            }
        })
        .abort_handle(),
    );

    // An error on these streams is a burst we fell behind on, not the end
    // (that's the stream ending): keep listening, and ask again for what may
    // have been missed. Stopping here left every friend nameless.
    let (c, o) = (conn.clone(), out.clone());
    tasks.push(
        tokio::spawn(async move {
            while let Some(item) = lists.next().await {
                if let Ok(list) = item {
                    friends_list(&c, &list, me, &o);
                }
            }
        })
        .abort_handle(),
    );
    let o = out.clone();
    tasks.push(
        tokio::spawn(async move {
            let mut asked_again = false;
            while let Some(item) = states.next().await {
                match item {
                    Ok(state) => {
                        let list = personas(&state);
                        diag(&format!(
                            "persona message: {} entries, {} with a name, {} with an avatar",
                            list.len(),
                            list.iter().filter(|p| p.name.is_some()).count(),
                            list.iter().filter(|p| p.avatar_hash.is_some()).count()
                        ));
                        o.send(Ev::Personas(list));
                    }
                    // Once per connection: asking for every friend again is a burst too.
                    Err(_) if !asked_again => {
                        diag("persona stream fell behind; asking again");
                        asked_again = true;
                        o.send(Ev::NeedPersonas);
                    }
                    Err(_) => {}
                }
            }
        })
        .abort_handle(),
    );
    // Incoming messages and typing; the stream ends when the connection does.
    let (o, i) = (out.clone(), inner.clone());
    tasks.push(
        tokio::spawn(async move {
            let mut seq: std::collections::HashMap<u64, u64> = Default::default();
            while let Some(item) = incoming.next().await {
                let Ok(n) = item else { continue };
                let friend = n.steamid_friend.unwrap_or_default();
                match n.chat_entry_type.unwrap_or_default() {
                    ENTRY_TYPING if !n.local_echo.unwrap_or(false) => {
                        let at = seq.entry(friend).or_default();
                        *at += 1;
                        let at = *at;
                        o.send(Ev::Typing {
                            friend,
                            typing: true,
                            seq: at,
                        });
                        let o = o.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(TYPING_FOR).await;
                            o.send(Ev::Typing {
                                friend,
                                typing: false,
                                seq: at,
                            });
                        });
                    }
                    ENTRY_MESSAGE => {
                        let mine = n.local_echo.unwrap_or(false);
                        // With Steam's markup (bbcode.rs reads it).
                        let text = n.message.clone().unwrap_or_default();
                        let line = Line {
                            from: account_id(if mine { me } else { friend }),
                            time: n.rtime32_server_timestamp.unwrap_or_else(now),
                            ordinal: n.ordinal.unwrap_or_default(),
                            text,
                        };
                        if !mine {
                            let at = seq.entry(friend).or_default();
                            *at += 1;
                            o.send(Ev::Typing {
                                friend,
                                typing: false,
                                seq: *at,
                            });
                        }
                        o.send(Ev::Incoming { friend, mine, line });
                    }
                    _ => {}
                }
            }
            let _ = i.send(Inner::Dropped);
        })
        .abort_handle(),
    );
    // Online, and the chats with something unread.
    let (c, o) = (conn, out.clone());
    tasks.push(
        tokio::spawn(async move {
            let _ = c
                .send(CMsgClientChangeStatus {
                    persona_state: Some(1),
                    need_persona_response: Some(true),
                    ..Default::default()
                })
                .await;
            let _ = c
                .send(CMsgClientRequestFriendData {
                    persona_state_requested: Some(PERSONA_FLAGS),
                    friends: vec![me],
                    ..Default::default()
                })
                .await;
            // Answered with CMsgClientEmoticonList (vaporchat's `getEmoticons`).
            let _ = c.send(CMsgClientGetEmoticonList::default()).await;
            fetch_groups(c.clone(), o.clone()).await;
            let req = CFriendsMessages_GetActiveMessageSessions_Request {
                only_sessions_with_messages: Some(true),
                ..Default::default()
            };
            if let Ok(res) = c.service_method(req).await {
                o.send(Ev::Sessions(
                    res.message_sessions
                        .iter()
                        .map(|s| {
                            (
                                s.accountid_friend.unwrap_or_default(),
                                s.last_message.unwrap_or_default(),
                                s.last_view.unwrap_or_default(),
                                s.unread_message_count.unwrap_or_default(),
                            )
                        })
                        .collect(),
                ));
            }
            // Own persona may not come: ready anyway after a few seconds.
            tokio::time::sleep(Duration::from_secs(5)).await;
            o.send(Ev::ReadyDeadline);
        })
        .abort_handle(),
    );
    tasks
}

fn friends_list(conn: &Arc<Connection>, list: &CMsgClientFriendsList, me: u64, out: &Out) {
    let friends: Vec<(u64, u32)> = list
        .friends
        .iter()
        .map(|f| {
            (
                f.ulfriendid.unwrap_or_default(),
                f.efriendrelationship.unwrap_or_default(),
            )
        })
        // Individual accounts only (groups are on the list too).
        .filter(|(id, _)| is_individual(*id) && *id != me)
        .collect();
    let ids: Vec<u64> = friends.iter().map(|f| f.0).collect();
    // What the other entries are (Steam id type, EFriendRelationship), for
    // learning how community group invites show; no ids in the log.
    let others: Vec<(u64, u32)> = list
        .friends
        .iter()
        .filter(|f| !is_individual(f.ulfriendid.unwrap_or_default()))
        .map(|f| {
            (
                (f.ulfriendid.unwrap_or_default() >> 52) & 0xF,
                f.efriendrelationship.unwrap_or_default(),
            )
        })
        .collect();
    if !others.is_empty() {
        diag(&format!("friends list: other entries (type, relationship): {others:?}"));
    }
    diag(&format!(
        "friends list: {} entries ({} individual), incremental: {}",
        list.friends.len(),
        ids.len(),
        list.bincremental.unwrap_or(false)
    ));
    out.send(Ev::FriendsList {
        friends,
        incremental: list.bincremental.unwrap_or(false),
    });
    // Their names, presence and games (once per list; Steam then pushes changes).
    if !ids.is_empty() {
        tokio::spawn(request_personas(conn.clone(), ids));
    }
}

async fn request_personas(conn: Arc<Connection>, ids: Vec<u64>) {
    diag(&format!("asking for {} personas", ids.len()));
    for chunk in ids.chunks(100) {
        let sent = conn
            .send(CMsgClientRequestFriendData {
                persona_state_requested: Some(PERSONA_FLAGS),
                friends: chunk.to_vec(),
                ..Default::default()
            })
            .await;
        if let Err(e) = sent {
            diag(&format!("asking for personas failed: {e}"));
        }
    }
}

/// A line about the Steam connection on standard error: counts and states
/// only, never names, ids or tokens.
pub(crate) fn diag(line: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    // Seconds within the minute, to the millisecond: enough to order a call's steps.
    godwit_core::verbose!("steam [{:06.3}]: {line}", (now.as_millis() % 60_000) as f64 / 1000.0);
}

/// The group chats, as [`Ev::Groups`]; a failure only logs (the list stays).
async fn fetch_groups(conn: Arc<Connection>, out: Out) {
    match crate::groups::fetch(&conn).await {
        Ok(groups) => out.send(Ev::Groups(groups)),
        Err(e) => diag(&format!("group chats: fetching failed: {e}")),
    }
}

/// Steam's emoticon list, recently used first, then newest, then by name
/// (as vaporchat orders its picker). Emoticons come named `:name:`.
fn emoticons(list: &CMsgClientEmoticonList) -> Ev {
    fn order<T>(items: &[T], key: impl Fn(&T) -> (u32, u32, String)) -> Vec<String> {
        let mut keyed: Vec<(u32, u32, String)> = items.iter().map(key).filter(|k| !k.2.is_empty()).collect();
        keyed.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
        keyed.dedup_by(|a, b| a.2 == b.2);
        keyed.into_iter().map(|k| k.2).collect()
    }
    Ev::Emoticons {
        emoticons: order(&list.emoticons, |e| {
            (
                e.time_last_used(),
                e.time_received(),
                e.name().trim_matches(':').to_owned(),
            )
        }),
        stickers: order(&list.stickers, |s| {
            (s.time_last_used(), s.time_received(), s.name().to_owned())
        }),
    }
}

/// Steam games' names from PICS, as Steam's own client looks them up over
/// its connection (no web request): the public app info of up to 50 games at
/// a time, read from each one's `appinfo/common/name`.
async fn app_names(conn: &Connection, apps: Vec<u32>) -> Vec<(u32, String)> {
    use steam_vent_proto_steam::steammessages_clientserver_appinfo::{
        CMsgClientPICSProductInfoRequest, CMsgClientPICSProductInfoResponse, cmsg_client_picsproduct_info_request,
    };
    let mut names = Vec::new();
    for chunk in apps.chunks(50) {
        let request = CMsgClientPICSProductInfoRequest {
            apps: chunk
                .iter()
                .map(|&appid| cmsg_client_picsproduct_info_request::AppInfo {
                    appid: Some(appid),
                    ..Default::default()
                })
                .collect(),
            meta_data_only: Some(false),
            single_response: Some(true),
            ..Default::default()
        };
        let asked = conn.job::<_, CMsgClientPICSProductInfoResponse>(request);
        match tokio::time::timeout(Duration::from_secs(15), asked).await {
            Ok(Ok(response)) => {
                names.extend(
                    response
                        .apps
                        .iter()
                        .filter_map(|a| Some((a.appid?, crate::vdf::app_name(a.buffer())?))),
                );
            }
            Ok(Err(e)) => diag(&format!("couldn't look up game names: {e}")),
            Err(_) => diag("Steam didn't answer a game name lookup"),
        }
    }
    names
}

fn personas(state: &CMsgClientPersonaState) -> Vec<Persona> {
    state
        .friends
        .iter()
        .map(|f| Persona {
            steam_id: f.friendid.unwrap_or_default(),
            name: f.player_name.clone(),
            state: f.persona_state,
            avatar_hash: f.avatar_hash.clone(),
            game_name: f.game_name.clone().filter(|g| !g.is_empty()),
            game_app: f.game_played_app_id.filter(|a| *a != 0),
        })
        .collect()
}

/// An individual account's 64-bit id: universe 1, type 1 (individual).
fn is_individual(id: u64) -> bool {
    (id >> 52) & 0xF == 1
}

/// The 32-bit account id inside a 64-bit Steam id.
pub fn account_id(steam_id: u64) -> u32 {
    (steam_id & 0xFFFF_FFFF) as u32
}

/// An individual account's 64-bit id from its 32-bit account id.
pub fn steam_id(account_id: u32) -> u64 {
    0x0110_0001_0000_0000 | u64::from(account_id)
}

pub(crate) fn now() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u32::try_from(d.as_secs()).unwrap_or(u32::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_ids_and_account_ids_convert() {
        let id = 76_561_197_960_287_930;
        assert!(is_individual(id));
        assert_eq!(steam_id(account_id(id)), id);
        // A group's id isn't an individual's.
        assert!(!is_individual(103_582_791_429_521_408));
    }

    #[test]
    fn dead_sign_ins_are_told_from_network_trouble() {
        assert!(describe(&ConnectionError::LoginError(LoginError::InvalidCredentials)).1);
        assert!(!describe(&ConnectionError::LoginError(LoginError::RateLimited)).1);
        assert!(!describe(&ConnectionError::Aborted).1);
    }

    #[test]
    fn work_from_a_replaced_connection_is_dropped_but_answers_arrive() {
        let (tx, rx) = mpsc::channel();
        let out = Out {
            events: godwit_core::backend::Events::new(tx, Arc::new(|| {})),
            current: Arc::new(AtomicU64::new(1)),
            generation: None,
        };
        let old = out.tagged(1);
        out.current.store(2, Ordering::Release);
        old.send(Ev::ReadyDeadline);
        old.send(Ev::SendFailed {
            nonce: "n".into(),
            friend: 1,
            error: "e".into(),
        });
        out.tagged(2).send(Ev::ReadyDeadline);
        let got: Vec<Ev> = rx.try_iter().collect();
        assert!(matches!(&got[..], [Ev::SendFailed { .. }, Ev::ReadyDeadline]));
    }
}
