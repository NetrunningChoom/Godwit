// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! The Discord network thread: QR sign-in, the gateway session (resumed when
//! it drops, identified again only when Discord won't resume, with backoff),
//! and HTTP requests, each answered with an event. The UI thread never waits
//! on any of it.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::mpsc as tmpsc;

use crate::api::{self, Api, Span, Upload};
use crate::gateway::{self, Gateway, Reconnects};
use crate::remote_auth::{self, Step};
use crate::voice::{CallCmd, CallEvent, session as call_session};
use godwit_voice::call::VoiceSettings;

pub use godwit_core::backend::Waker;

/// A request to Discord's HTTP API.
#[derive(Debug, Clone)]
pub enum Request {
    History {
        channel: String,
        before: Option<String>,
        around: Option<String>,
    },
    Send {
        guild: Option<String>,
        channel: String,
        text: String,
        nonce: String,
        reply: Option<String>,
        /// Files to upload first: (path, name).
        files: Vec<(std::path::PathBuf, String)>,
    },
    /// Forward `message`, then send `comment` (when not empty) after it.
    Forward {
        guild: Option<String>,
        channel: String,
        message: String,
        target: String,
        /// The destination's server, for the comment's reference-less send.
        target_guild: Option<String>,
        comment: String,
    },
    /// Post a poll (the whole message body, see `api::poll_body`).
    CreatePoll {
        channel: String,
        body: serde_json::Value,
    },
    /// Post a sticker (the whole message body, see `api::sticker_body`).
    SendSticker {
        channel: String,
        body: serde_json::Value,
    },
    EndPoll {
        channel: String,
        message: String,
    },
    Edit {
        channel: String,
        message: String,
        text: String,
    },
    Delete {
        channel: String,
        message: String,
    },
    React {
        channel: String,
        message: String,
        emoji: String,
        add: bool,
    },
    Reactors {
        channel: String,
        message: String,
        emoji: String,
    },
    Ack {
        channel: String,
        message: String,
    },
    /// "Mark unread": ack up to `message` with the mentions left after it.
    AckManual {
        channel: String,
        message: String,
        mention_count: u32,
    },
    AckBulk(Vec<(String, String)>),
    Typing {
        channel: String,
    },
    OpenDm {
        user: String,
    },
    CreateGroup {
        name: String,
        users: Vec<String>,
    },
    LeaveGroup {
        channel: String,
    },
    AddGroupRecipient {
        channel: String,
        user: String,
    },
    RemoveGroupRecipient {
        channel: String,
        user: String,
    },
    Profile {
        user: String,
        guild: Option<String>,
    },
    /// Our private note on `user`, asked when their profile opens.
    Note {
        user: String,
    },
    SetNote {
        user: String,
        note: String,
        /// What it was, to put back when saving fails.
        previous: String,
    },
    /// Our profile saved: account fields (with an avatar file to read), then
    /// the profile's own; either may be absent.
    SaveProfile {
        me: Option<Value>,
        avatar: Option<std::path::PathBuf>,
        profile: Option<Value>,
    },
    AddFriend {
        user: String,
    },
    AddFriendByName {
        request: u64,
        name: String,
    },
    RemoveFriend {
        user: String,
    },
    BlockUser {
        user: String,
    },
    /// Lifts a block (the same DELETE as unfriending).
    UnblockUser {
        user: String,
    },
    IgnoreUser {
        user: String,
    },
    UnignoreUser {
        user: String,
    },
    CustomStatus(Option<String>),
    /// The first-run privacy choice: data-use consents revoked, activity hidden.
    TightenPrivacy,
    /// The data-use consents, for Settings → Privacy.
    Consents,
    /// One consent granted or revoked.
    SetConsent {
        key: String,
        grant: bool,
    },
    /// Activities shared in the presence, or not.
    ShowActivity(bool),
    /// The signed-in devices, and signing one out (its `id_hash`).
    Sessions,
    LogOutSession(String),
    Status(String),
    Invite {
        code: String,
    },
    JoinInvite {
        code: String,
    },
    Pins {
        channel: String,
    },
    Vote {
        channel: String,
        message: String,
        answers: Vec<u64>,
    },
    MessageComponent(Value),
    Search {
        guild: Option<String>,
        channel: String,
        /// What was asked, echoed in the answer.
        query: Box<godwit_core::search::SearchQuery>,
        params: Box<crate::search::SearchParams>,
    },
    /// Accept or ignore a message request.
    AnswerRequest {
        channel: String,
        accept: bool,
    },
    /// A forum's archived posts (see `Api::search_threads`).
    ForumThreads {
        channel: String,
        sort: godwit_core::forum::ForumSort,
        tag: Option<String>,
        offset: usize,
    },
    CreateThread {
        channel: String,
        message: Option<String>,
        name: String,
        text: String,
        tags: Vec<String>,
    },
    ThreadMembership {
        channel: String,
        join: bool,
    },
    /// Modify Channel: the fields in `body` (name, topic, nsfw,
    /// rate_limit_per_user); answered with [`Answer::ChannelSaved`].
    ModifyChannel {
        channel: String,
        request: u64,
        body: Value,
    },
    /// Sets or (`remove`) deletes one permission overwrite of a channel;
    /// answered with [`Answer::OverwriteSaved`].
    ChannelOverwrite {
        channel: String,
        request: u64,
        overwrite: crate::permissions::Overwrite,
        remove: bool,
    },
    /// Delete Channel (also a category).
    DeleteChannel {
        channel: String,
        request: u64,
    },
    /// Our notification level for a server (0 all, 1 mentions, 2 nothing).
    GuildNotify {
        guild: String,
        level: u8,
    },
    ScheduledEvents {
        guild: String,
    },
    ScheduledEventInterest {
        guild: String,
        event: String,
        interested: bool,
    },
    /// A channel's (or DM's) override in our guild settings: the body of
    /// its entry in `channel_overrides` (see `account::notify`).
    ChannelNotify {
        /// None for DMs (`@me`).
        guild: Option<String>,
        channel: String,
        body: serde_json::Value,
        /// The override before the change, put back if the request fails.
        previous: serde_json::Value,
    },
    Pin {
        channel: String,
        message: String,
        pinned: bool,
    },
    GifCategories,
    /// GIFs for `query` (`godwit_core::gifs::TRENDING`: what's trending).
    Gifs {
        query: String,
    },
    /// Ring the others in our DM call; `stranger`s are rung only when
    /// Discord says they may be (as the official client and Yapfast ask).
    Ring {
        channel: String,
        stranger: bool,
    },
    /// Decline a call ringing `me`.
    StopRinging {
        channel: String,
        me: String,
    },
    /// A server settings or menu change; the answer names `guild`.
    Admin {
        guild: String,
        op: Admin,
    },
}

#[derive(Debug, Clone)]
pub enum Admin {
    Invites,
    RevokeInvite(String),
    Bans,
    Unban(String),
    Kick {
        user: String,
        reason: String,
    },
    Ban {
        user: String,
        reason: String,
        delete_seconds: u32,
    },
    MemberRole {
        user: String,
        role: String,
        on: bool,
    },
    /// Timeout or nickname; `user` is "@me" for our own nickname.
    ModifyMember {
        user: String,
        body: Value,
    },
    CreateRole(String),
    ModifyRole {
        role: String,
        body: Value,
    },
    DeleteRole(String),
    CreateInvite {
        channel: String,
    },
    /// A channel or category: `kind` is Discord's channel type (0 text, 2
    /// voice, 4 category), `parent` the category a channel goes in.
    CreateChannel {
        name: String,
        kind: u64,
        parent: Option<String>,
    },
    /// Modify Guild Channel Positions: `[{id, position, parent_id?}]`.
    MoveChannels(Value),
    Leave,
    Delete,
    ModifyGuild(Value),
    /// The Emoji page's list.
    Emojis,
    /// Each change arrives as GUILD_EMOJIS_UPDATE.
    CreateEmoji {
        name: String,
        path: std::path::PathBuf,
    },
    RenameEmoji {
        id: String,
        name: String,
    },
    DeleteEmoji(String),
}

#[derive(Debug)]
pub enum Cmd {
    /// Start signing in by QR code.
    Link,
    PasswordLogin {
        login: String,
        password: Secret,
    },
    LoginCode {
        ticket: Secret,
        method: String,
        code: Secret,
    },
    SendLoginSms {
        ticket: Secret,
    },
    /// Connect with a kept sign-in.
    UseSession {
        token: String,
    },
    /// A gateway command (presence, subscriptions).
    Gateway {
        op: u64,
        data: Value,
    },
    Request(Request),
    /// Close the session (staying signed in), or open it again.
    Offline(bool),
    /// End the session at Discord and forget it.
    Logout,
    /// Start a call's media (devices open now; it connects once a
    /// [`CallCmd::Server`] follows), ending any call before it. `call` tags
    /// its [`Ev::Call`]s.
    Call {
        call: u64,
        settings: Box<VoiceSettings>,
        muted: bool,
        deafened: bool,
    },
    /// To the call under way.
    CallCmd(CallCmd),
}

/// What a request came back with.
#[derive(Debug)]
pub enum Answer {
    History {
        channel: String,
        before: Option<String>,
        around: Option<String>,
        result: Result<Vec<Value>, RequestError>,
    },
    Reactors {
        channel: String,
        message: String,
        emoji: String,
        result: Result<Value, RequestError>,
    },
    Sent {
        channel: String,
        nonce: String,
        result: Result<Value, RequestError>,
    },
    /// An HTTP request that changes something finished; failures are
    /// reported, successes arrive as gateway events.
    Done {
        what: &'static str,
        result: Result<(), RequestError>,
    },
    /// A channel override was sent; on failure `previous` goes back.
    ChannelNotify {
        guild: Option<String>,
        previous: Value,
        result: Result<(), RequestError>,
    },
    DmOpened(Result<Value, RequestError>),
    Profile {
        user: String,
        result: Result<Value, RequestError>,
    },
    ProfileSaved(Result<(), RequestError>),
    Note {
        user: String,
        result: Result<String, RequestError>,
    },
    NoteSaved {
        user: String,
        previous: String,
        result: Result<(), RequestError>,
    },
    Consents(Result<Value, RequestError>),
    Sessions(Result<Value, RequestError>),
    FriendRequest {
        request: u64,
        result: Result<(), RequestError>,
    },
    Invite {
        code: String,
        result: Result<Value, RequestError>,
    },
    GifCategories(Result<Value, RequestError>),
    Search {
        channel: String,
        query: Box<godwit_core::search::SearchQuery>,
        offset: usize,
        result: Result<Value, RequestError>,
    },
    ThreadCreated(Result<Value, RequestError>),
    ThreadMembership {
        channel: String,
        join: bool,
        result: Result<(), RequestError>,
    },
    ForumThreads {
        channel: String,
        sort: godwit_core::forum::ForumSort,
        tag: Option<String>,
        offset: usize,
        result: Result<Value, RequestError>,
    },
    /// A channel edit's answer: the channel as Discord now has it.
    ChannelSaved {
        channel: String,
        request: u64,
        result: Result<Value, RequestError>,
    },
    /// An overwrite set or removed (a 204: the change is what was asked).
    OverwriteSaved {
        channel: String,
        request: u64,
        overwrite: crate::permissions::Overwrite,
        remove: bool,
        result: Result<(), RequestError>,
    },
    ChannelDeleted {
        channel: String,
        request: u64,
        result: Result<(), RequestError>,
    },
    RequestAnswered {
        channel: String,
        accept: bool,
        result: Result<(), RequestError>,
    },
    /// A channel's pins (also after pinning or unpinning there).
    Pins {
        channel: String,
        result: Result<Vec<Value>, RequestError>,
    },
    Gifs {
        query: String,
        result: Result<Value, RequestError>,
    },
    Joined {
        code: String,
        result: Result<Value, RequestError>,
    },
    Admin {
        guild: String,
        result: Result<AdminAnswer, RequestError>,
    },
    ScheduledEvents {
        guild: String,
        result: Result<Value, RequestError>,
    },
    ScheduledEventInterest {
        guild: String,
        event: String,
        interested: bool,
        result: Result<(), RequestError>,
    },
}

#[derive(Debug)]
pub enum AdminAnswer {
    Invites(Value),
    Bans(Value),
    Emojis(Value),
    /// The new emoji, as Discord answered the create.
    EmojiCreated(Value),
    InviteCreated(String),
    Done,
}

#[derive(Debug)]
pub enum Ev {
    LinkStep(Step),
    /// Signed in: the token to keep.
    Linked(String),
    LinkFailed(String),
    PasswordResult(Result<Value, String>),
    LoginCaptcha(String),
    LoginCodeResult(Result<Value, String>),
    LoginSmsResult(Result<Value, String>),
    Ready(Box<Value>),
    Dispatch(String, Value),
    /// Discord refreshed the sign-in; keep this token instead.
    Token(String),
    /// The connection dropped (and is being made again) or came back.
    Connection(bool),
    /// The session can't go on: the sign-in was refused, or too many reconnects.
    Lost {
        reason: String,
        signed_out: bool,
    },
    Answer(Answer),
    /// From the call started with [`Cmd::Call`] `call`.
    Call {
        call: u64,
        event: CallEvent,
    },
}

type Out = godwit_core::backend::Events<Ev>;

pub type Handle = godwit_core::backend::Handle<Cmd>;

/// A password, MFA code or ticket: its Debug never shows the text, so a
/// logged command can't leak it.
#[derive(Clone)]
pub struct Secret(pub String);
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

fn login_error(error: &anyhow::Error) -> String {
    let text = error.to_string();
    if text.contains("Wrong email or password.") {
        return "Wrong email or password.".into();
    }
    if text.contains("Discord sent you an email to confirm this sign-in.") {
        return "Discord sent you an email to confirm this sign-in. Confirm it, then try again.".into();
    }
    "Discord couldn't complete sign-in. Try again.".into()
}

pub fn spawn(wake: Waker) -> std::io::Result<(Handle, mpsc::Receiver<Ev>)> {
    godwit_core::backend::spawn(
        "discord-net",
        tokio::runtime::Builder::new_current_thread(),
        move |tx| Out::new(tx, wake),
        run,
        |out, e| out.send(Ev::LinkFailed(format!("Couldn't start Discord's network thread: {e}"))),
    )
}

/// The presence identified with: the web client's, letting Discord apply
/// the status from the account's settings.
fn identify_presence() -> Value {
    json!({"status": "unknown", "since": 0, "activities": [], "afk": false})
}

enum Inner {
    Session(Result<(), anyhow::Error>),
}

struct State {
    api: Option<Arc<Api>>,
    token: Option<String>,
    session: Option<tokio::task::JoinHandle<()>>,
    gateway: Option<tmpsc::UnboundedSender<(u64, Value)>>,
    link: Option<tokio::task::AbortHandle>,
    offline: bool,
    /// The call under way.
    call: Option<tmpsc::UnboundedSender<CallCmd>>,
}

async fn run(mut cmds: tmpsc::UnboundedReceiver<Cmd>, out: Out) {
    let mut state = State {
        api: None,
        token: None,
        session: None,
        gateway: None,
        link: None,
        offline: false,
        call: None,
    };
    let (inner_tx, mut inner_rx) = tmpsc::unbounded_channel::<Inner>();
    loop {
        tokio::select! {
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { break };
                handle(cmd, &mut state, &out, &inner_tx);
            }
            Some(Inner::Session(result)) = inner_rx.recv() => {
                state.session = None;
                state.gateway = None;
                if let Err(e) = result {
                    out.send(Ev::Lost { reason: e.to_string(), signed_out: e.downcast_ref::<Unauthorized>().is_some() });
                }
            }
        }
    }
}

fn handle(cmd: Cmd, state: &mut State, out: &Out, inner: &tmpsc::UnboundedSender<Inner>) {
    match cmd {
        Cmd::PasswordLogin { login, password } => {
            let out = out.clone();
            tokio::spawn(async move {
                let steps = out.clone();
                let result = match Api::new(String::new()) {
                    Ok(_) => crate::captcha::solving(
                        |solved| {
                            let login = login.clone();
                            let password = password.clone();
                            async move {
                                let api = Api::new(String::new())?;
                                api.password_login_solved(
                                    &login,
                                    &password.0,
                                    solved.as_ref().map(|(c, s)| (c, s.as_str())),
                                )
                                .await
                            }
                        },
                        |page| {
                            steps.send(Ev::LoginCaptcha(page.to_owned()));
                        },
                    )
                    .await
                    .map_err(|error| login_error(&error)),
                    Err(_) => Err("Discord couldn't start sign-in. Try again.".to_owned()),
                };
                out.send(Ev::PasswordResult(result));
            });
        }
        Cmd::LoginCode { ticket, method, code } => {
            let out = out.clone();
            tokio::spawn(async move {
                let result = match Api::new(String::new()) {
                    Ok(api) => api
                        .login_mfa(&method, &code.0, &ticket.0)
                        .await
                        .map_err(|_| "Discord couldn't verify that code. Try again.".to_owned()),
                    Err(_) => Err("Discord couldn't start sign-in. Try again.".to_owned()),
                };
                out.send(Ev::LoginCodeResult(result));
            });
        }
        Cmd::SendLoginSms { ticket } => {
            let out = out.clone();
            tokio::spawn(async move {
                let result = match Api::new(String::new()) {
                    Ok(api) => api
                        .login_sms(&ticket.0)
                        .await
                        .map_err(|_| "Discord couldn't send a text message. Try again.".to_owned()),
                    Err(_) => Err("Discord couldn't start sign-in. Try again.".to_owned()),
                };
                out.send(Ev::LoginSmsResult(result));
            });
        }
        Cmd::Link => {
            if let Some(link) = state.link.take() {
                link.abort();
            }
            let out = out.clone();
            let task = tokio::spawn(async move {
                let steps = out.clone();
                match remote_auth::link(move |step| steps.send(Ev::LinkStep(step))).await {
                    Ok(token) => out.send(Ev::Linked(token)),
                    Err(e) => out.send(Ev::LinkFailed(e.to_string())),
                }
            });
            state.link = Some(task.abort_handle());
        }
        Cmd::UseSession { token } => {
            let api = match Api::new(token.clone()) {
                Ok(api) => Arc::new(api),
                Err(e) => {
                    return out.send(Ev::Lost {
                        reason: e.to_string(),
                        signed_out: false,
                    });
                }
            };
            state.api = Some(api.clone());
            state.token = Some(token);
            start_session(state, out, inner);
        }
        Cmd::Gateway { op, data } => {
            if let Some(gateway) = &state.gateway {
                let _ = gateway.send((op, data));
            }
        }
        Cmd::Request(request) => {
            let Some(api) = state.api.clone() else {
                return;
            };
            let out = out.clone();
            tokio::spawn(async move { out.send(Ev::Answer(answer(&api, request).await)) });
        }
        Cmd::Offline(offline) => {
            state.offline = offline;
            if offline {
                end_call(state);
                if let Some(session) = state.session.take() {
                    session.abort();
                }
                state.gateway = None;
                out.send(Ev::Connection(false));
            } else if state.session.is_none() && state.token.is_some() {
                start_session(state, out, inner);
            }
        }
        Cmd::Call {
            call,
            settings,
            muted,
            deafened,
        } => {
            end_call(state);
            let (tx, rx) = tmpsc::unbounded_channel();
            state.call = Some(tx);
            let out = out.clone();
            call_session::start(*settings, muted, deafened, rx, move |event| {
                out.send(Ev::Call { call, event })
            });
        }
        Cmd::CallCmd(cmd) => {
            if matches!(cmd, CallCmd::Leave) {
                end_call(state);
            } else if let Some(call) = &state.call {
                let _ = call.send(cmd);
            }
        }
        Cmd::Logout => {
            end_call(state);
            if let Some(session) = state.session.take() {
                session.abort();
            }
            state.gateway = None;
            if let Some(api) = state.api.take() {
                tokio::spawn(async move {
                    // Ends the session at Discord, so the token stops working.
                    if let Err(e) = api.logout().await {
                        eprintln!("discord: couldn't end the session at Discord: {e}");
                    }
                });
            }
            state.token = None;
            // An empty reason: the user signed out, nothing went wrong.
            out.send(Ev::Lost {
                reason: String::new(),
                signed_out: true,
            });
        }
    }
}

/// Leaves the call under way, if any: it closes its voice connection and
/// reports [`CallEvent::Ended`].
fn end_call(state: &mut State) {
    if let Some(call) = state.call.take() {
        let _ = call.send(CallCmd::Leave);
    }
}

/// Discord refused the sign-in itself.
#[derive(Debug)]
struct Unauthorized;

impl std::fmt::Display for Unauthorized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Discord no longer accepts this sign-in. Sign in again.")
    }
}

impl std::error::Error for Unauthorized {}

fn start_session(state: &mut State, out: &Out, inner: &tmpsc::UnboundedSender<Inner>) {
    let (Some(token), Some(api), false) = (state.token.clone(), state.api.clone(), state.offline) else {
        return;
    };
    let (gateway_tx, gateway_rx) = tmpsc::unbounded_channel();
    state.gateway = Some(gateway_tx);
    let (out, inner) = (out.clone(), inner.clone());
    state.session = Some(tokio::spawn(async move {
        let result = session(token, api, gateway_rx, &out).await;
        let _ = inner.send(Inner::Session(result));
    }));
}

/// Keeps a gateway session going: connects, passes events on, and after a
/// loss waits (with backoff) and connects again, until the sign-in is
/// refused or too many sessions were started.
async fn session(
    mut token: String,
    api: Arc<Api>,
    mut commands: tmpsc::UnboundedReceiver<(u64, Value)>,
    out: &Out,
) -> anyhow::Result<()> {
    api.learn_build_number().await;
    let mut reconnects = Reconnects::default();
    loop {
        let started = Instant::now();
        let error = match Gateway::connect(&token, identify_presence(), &mut reconnects).await {
            Ok((mut gateway, ready)) => {
                if let Some(new) = ready["auth_token"].as_str().filter(|t| !t.is_empty() && *t != token) {
                    token = new.to_owned();
                    api.relogin(&token);
                    out.send(Ev::Token(token.clone()));
                }
                out.send(Ev::Connection(true));
                out.send(Ev::Ready(Box::new(ready)));
                loop {
                    tokio::select! {
                        event = gateway.next() => match event {
                            Ok(d) => out.send(Ev::Dispatch(d.event, d.data)),
                            Err(e) => break e,
                        },
                        Some((op, data)) = commands.recv() => {
                            if let Err(e) = gateway.send(op, data) {
                                break e;
                            }
                        }
                    }
                }
            }
            Err(e) => e,
        };
        if error.downcast_ref::<gateway::TooManyIdentifies>().is_some() {
            return Err(error);
        }
        if error
            .downcast_ref::<gateway::Closed>()
            .is_some_and(gateway::Closed::is_final)
        {
            return Err(Unauthorized.into());
        }
        out.send(Ev::Connection(false));
        let wait = reconnects.wait(&error, Some(started.elapsed()));
        godwit_core::verbose!(
            "discord: connection lost ({error}); trying again in {}s",
            wait.as_secs()
        );
        tokio::time::sleep(wait).await;
    }
}

/// A request failure retains its source until the account bridge presents it.
#[derive(Debug)]
pub struct RequestError(anyhow::Error);

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}
impl RequestError {
    /// The thing asked about no longer exists (a 404).
    pub fn is_gone(&self) -> bool {
        gone(&self.0)
    }
}

impl std::error::Error for RequestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}
impl From<anyhow::Error> for RequestError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}
impl From<&str> for RequestError {
    fn from(message: &str) -> Self {
        Self(anyhow::anyhow!(message.to_owned()))
    }
}
impl From<RequestError> for String {
    fn from(error: RequestError) -> Self {
        error.to_string()
    }
}
impl From<RequestError> for Arc<str> {
    fn from(error: RequestError) -> Self {
        error.to_string().into()
    }
}

async fn answer(api: &Api, request: Request) -> Answer {
    match request {
        Request::History {
            channel,
            before,
            around,
        } => {
            let span = match (&before, &around) {
                (Some(b), _) => Span::Before(b),
                (None, Some(a)) => Span::Around(a),
                _ => Span::Latest,
            };
            let result = api.messages(&channel, span).await.map_err(RequestError::from);
            Answer::History {
                channel,
                before,
                around,
                result,
            }
        }
        Request::Reactors {
            channel,
            message,
            emoji,
        } => Answer::Reactors {
            result: api
                .reaction_users(&channel, &message, &emoji)
                .await
                .map_err(RequestError::from),
            channel,
            message,
            emoji,
        },
        Request::Send {
            guild,
            channel,
            text: body,
            nonce,
            reply,
            files,
        } => {
            let result = async {
                let mut uploads: Vec<Upload> = Vec::new();
                for (path, name) in files {
                    let data = tokio::fs::read(&path)
                        .await
                        .map_err(|e| anyhow::anyhow!("couldn't read {name}: {e}"))?;
                    uploads.push(api.upload(&channel, &name, data).await?);
                }
                api.send(guild.as_deref(), &channel, &body, &nonce, reply.as_deref(), &uploads)
                    .await
            }
            .await
            .map_err(RequestError::from);
            Answer::Sent { channel, nonce, result }
        }
        Request::Forward {
            guild,
            channel,
            message,
            target,
            target_guild,
            comment,
        } => {
            let forwarded = api
                .forward(guild.as_deref(), &channel, &message, &target, &crate::api::nonce())
                .await;
            // The comment is its own message: its failure is worded as such.
            let (what, result) = match forwarded {
                Err(e) => ("forwarding a message", Err(e)),
                Ok(_) if comment.is_empty() => ("forwarding a message", Ok(())),
                Ok(_) => (
                    "sending the comment",
                    api.send(
                        target_guild.as_deref(),
                        &target,
                        &comment,
                        &crate::api::nonce(),
                        None,
                        &[],
                    )
                    .await
                    .map(drop),
                ),
            };
            Answer::Done {
                what,
                result: result.map_err(RequestError::from),
            }
        }
        Request::Edit {
            channel,
            message,
            text: body,
        } => Answer::Done {
            what: "editing a message",
            result: api
                .edit(&channel, &message, &body)
                .await
                .map(drop)
                .map_err(RequestError::from),
        },
        Request::Delete { channel, message } => Answer::Done {
            what: "deleting a message",
            result: api.delete(&channel, &message).await.map_err(RequestError::from),
        },
        Request::React {
            channel,
            message,
            emoji,
            add,
        } => Answer::Done {
            what: "reacting",
            result: api
                .react(&channel, &message, &emoji, add)
                .await
                .map_err(RequestError::from),
        },
        Request::Ack { channel, message } => Answer::Done {
            what: "marking read",
            result: api.ack(&channel, &message).await.map_err(RequestError::from),
        },
        Request::AckManual {
            channel,
            message,
            mention_count,
        } => Answer::Done {
            what: "marking unread",
            result: api
                .ack_manual(&channel, &message, mention_count)
                .await
                .map_err(RequestError::from),
        },
        Request::AckBulk(reads) => Answer::Done {
            what: "marking read",
            result: api.ack_bulk(&reads).await.map_err(RequestError::from),
        },
        Request::Typing { channel } => Answer::Done {
            what: "typing",
            result: api.typing(&channel).await.map_err(RequestError::from),
        },
        Request::OpenDm { user } => Answer::DmOpened(api.open_dm(&user).await.map_err(RequestError::from)),
        Request::CreateGroup { name, users } => {
            Answer::DmOpened(api.create_group(&name, &users).await.map_err(RequestError::from))
        }
        Request::LeaveGroup { channel } => Answer::Done {
            what: "Leaving group",
            result: api.leave_group(&channel).await.map_err(RequestError::from),
        },
        Request::AddGroupRecipient { channel, user } => Answer::Done {
            what: "Adding group recipient",
            result: api
                .add_group_recipient(&channel, &user)
                .await
                .map_err(RequestError::from),
        },
        Request::RemoveGroupRecipient { channel, user } => Answer::Done {
            what: "Removing group recipient",
            result: api
                .remove_group_recipient(&channel, &user)
                .await
                .map_err(RequestError::from),
        },
        Request::Profile { user, guild } => {
            let result = api.profile(&user, guild.as_deref()).await.map_err(RequestError::from);
            Answer::Profile { user, result }
        }
        Request::Note { user } => {
            let result = api.note(&user).await.map_err(RequestError::from);
            Answer::Note { user, result }
        }
        Request::SetNote { user, note, previous } => {
            let result = api.set_note(&user, &note).await.map_err(RequestError::from);
            Answer::NoteSaved { user, previous, result }
        }
        Request::SaveProfile { me, avatar, profile } => Answer::ProfileSaved(
            async {
                let mut me = me;
                if let Some(path) = avatar {
                    let uri = avatar_uri(&path).await?;
                    me.get_or_insert_with(|| serde_json::json!({}))["avatar"] = uri.into();
                }
                if let Some(me) = me {
                    api.edit_me(&me).await?;
                }
                if let Some(profile) = profile {
                    api.edit_my_profile(&profile).await?;
                }
                anyhow::Ok(())
            }
            .await
            .map_err(RequestError::from),
        ),
        Request::AddFriend { user } => Answer::Done {
            what: "adding a friend",
            result: api.add_friend(&user, "User Profile").await.map_err(RequestError::from),
        },
        Request::BlockUser { user } => Answer::Done {
            what: "blocking someone",
            result: api.block_user(&user, "User Profile").await.map_err(RequestError::from),
        },
        Request::UnblockUser { user } => Answer::Done {
            what: "unblocking someone",
            result: api
                .remove_relationship(&user, "User Profile")
                .await
                .map_err(RequestError::from),
        },
        Request::IgnoreUser { user } => Answer::Done {
            what: "ignoring someone",
            result: api.ignore_user(&user, true).await.map_err(RequestError::from),
        },
        Request::UnignoreUser { user } => Answer::Done {
            what: "unignoring someone",
            result: api.ignore_user(&user, false).await.map_err(RequestError::from),
        },
        Request::AddFriendByName { request, name } => Answer::FriendRequest {
            request,
            result: api.add_friend_by_username(&name).await.map_err(RequestError::from),
        },
        Request::RemoveFriend { user } => Answer::Done {
            what: "removing a friend",
            result: api
                .remove_relationship(&user, "User Profile")
                .await
                .map_err(RequestError::from),
        },
        Request::Consents => Answer::Consents(api.consents().await.map_err(RequestError::from)),
        Request::Sessions => Answer::Sessions(api.sessions().await.map_err(RequestError::from)),
        // Read the list back after signing one out, as the official page does.
        Request::LogOutSession(id) => Answer::Sessions(
            async {
                api.log_out_session(&id).await?;
                api.sessions().await
            }
            .await
            .map_err(RequestError::from),
        ),
        Request::SetConsent { key, grant } => Answer::Done {
            what: "changing a privacy setting",
            result: api.set_consent(&key, grant).await.map_err(RequestError::from),
        },
        Request::ShowActivity(shown) => Answer::Done {
            what: "changing activity sharing",
            result: api.set_show_activity(shown).await.map_err(RequestError::from),
        },
        Request::TightenPrivacy => Answer::Done {
            what: "changing privacy settings",
            result: async {
                api.revoke_consents().await?;
                api.hide_activity().await
            }
            .await
            .map_err(RequestError::from),
        },
        Request::Status(status) => Answer::Done {
            what: "setting the status",
            result: api.set_status(&status).await.map_err(RequestError::from),
        },
        Request::CustomStatus(status) => Answer::Done {
            what: "setting the status",
            result: api
                .set_custom_status(status.as_deref())
                .await
                .map_err(RequestError::from),
        },
        Request::Invite { code } => {
            let result = api.invite(&code).await.map_err(RequestError::from);
            Answer::Invite { code, result }
        }
        Request::Pins { channel } => {
            let result = api.pins(&channel).await.map_err(RequestError::from);
            Answer::Pins { channel, result }
        }
        Request::AnswerRequest { channel, accept } => {
            let result = api.answer_request(&channel, accept).await.map_err(RequestError::from);
            Answer::RequestAnswered {
                channel,
                accept,
                result,
            }
        }
        Request::CreateThread {
            channel,
            message,
            name,
            text: content,
            tags,
        } => Answer::ThreadCreated(
            api.create_thread(&channel, message.as_deref(), &name, &content, &tags)
                .await
                .map_err(RequestError::from),
        ),
        Request::ThreadMembership { channel, join } => Answer::ThreadMembership {
            result: api.thread_membership(&channel, join).await.map_err(RequestError::from),
            channel,
            join,
        },
        Request::ForumThreads {
            channel,
            sort,
            tag,
            offset,
        } => Answer::ForumThreads {
            result: api
                .search_threads(&channel, sort, tag.as_deref(), offset)
                .await
                .map_err(RequestError::from),
            channel,
            sort,
            tag,
            offset,
        },
        Request::ModifyChannel { channel, request, body } => Answer::ChannelSaved {
            result: api.modify_channel(&channel, &body).await.map_err(RequestError::from),
            channel,
            request,
        },
        Request::ChannelOverwrite {
            channel,
            request,
            overwrite,
            remove,
        } => Answer::OverwriteSaved {
            result: if remove {
                api.delete_overwrite(&channel, &overwrite.id).await
            } else {
                api.put_overwrite(&channel, &overwrite).await
            }
            .map_err(RequestError::from),
            channel,
            request,
            overwrite,
            remove,
        },
        Request::DeleteChannel { channel, request } => Answer::ChannelDeleted {
            result: api.delete_channel(&channel).await.map_err(RequestError::from),
            channel,
            request,
        },
        Request::GuildNotify { guild, level } => Answer::Done {
            what: "notification setting",
            result: api.guild_notify(&guild, level).await.map_err(RequestError::from),
        },
        Request::ScheduledEvents { guild } => Answer::ScheduledEvents {
            guild: guild.clone(),
            result: api.scheduled_events(&guild).await.map_err(RequestError::from),
        },
        Request::ScheduledEventInterest {
            guild,
            event,
            interested,
        } => Answer::ScheduledEventInterest {
            guild: guild.clone(),
            event: event.clone(),
            interested,
            result: api
                .scheduled_event_interest(&guild, &event, interested)
                .await
                .map_err(RequestError::from),
        },
        Request::ChannelNotify {
            guild,
            channel,
            body,
            previous,
        } => Answer::ChannelNotify {
            result: api
                .channel_notify(guild.as_deref(), &channel, &body)
                .await
                .map_err(RequestError::from),
            guild,
            previous,
        },
        Request::Search {
            guild,
            channel,
            query,
            params,
        } => {
            let result = api
                .search(guild.as_deref(), &channel, &params)
                .await
                .map_err(RequestError::from);
            Answer::Search {
                channel,
                query,
                offset: params.offset,
                result,
            }
        }
        Request::CreatePoll { channel, body } => Answer::Done {
            what: "creating a poll",
            result: api.create_poll(&channel, &body).await.map_err(RequestError::from),
        },
        Request::SendSticker { channel, body } => Answer::Done {
            what: "sending a sticker",
            result: api.send_sticker(&channel, &body).await.map_err(RequestError::from),
        },
        Request::EndPoll { channel, message } => Answer::Done {
            what: "ending a poll",
            result: api.end_poll(&channel, &message).await.map_err(RequestError::from),
        },
        Request::Vote {
            channel,
            message,
            answers,
        } => Answer::Done {
            what: "vote",
            result: api.vote(&channel, &message, &answers).await.map_err(RequestError::from),
        },
        Request::MessageComponent(body) => Answer::Done {
            what: "button",
            result: api.message_component(&body).await.map_err(RequestError::from),
        },
        Request::Pin {
            channel,
            message,
            pinned,
        } => {
            // The list again afterwards: what the panel shows is Discord's.
            let result = match api.pin(&channel, &message, pinned).await {
                Ok(()) => api.pins(&channel).await.map_err(RequestError::from),
                Err(e) => Err(e.into()),
            };
            Answer::Pins { channel, result }
        }
        Request::GifCategories => Answer::GifCategories(api.gifs_trending().await.map_err(RequestError::from)),
        Request::Gifs { query } => {
            let result = if query == godwit_core::gifs::TRENDING {
                api.gifs_trending_gifs().await
            } else {
                api.gifs_search(&query).await
            }
            .map_err(RequestError::from);
            Answer::Gifs { query, result }
        }
        Request::Ring { channel, stranger } => Answer::Done {
            what: "ringing",
            result: async {
                if stranger && !api.ringable(&channel).await? {
                    anyhow::bail!("Discord doesn't let you ring them; they'll see the call when they open the DM.");
                }
                api.ring(&channel).await
            }
            .await
            .map_err(RequestError::from),
        },
        Request::StopRinging { channel, me } => Answer::Done {
            what: "declining a call",
            result: api.stop_ringing(&channel, &me).await.map_err(RequestError::from),
        },
        Request::JoinInvite { code } => {
            let result = api.join_invite(&code).await.map_err(RequestError::from);
            Answer::Joined { code, result }
        }
        Request::Admin { guild, op } => {
            let result = admin(api, &guild, op).await.map_err(RequestError::from);
            Answer::Admin { guild, result }
        }
    }
}

async fn admin(api: &Api, guild: &str, op: Admin) -> anyhow::Result<AdminAnswer> {
    Ok(match op {
        Admin::Invites => AdminAnswer::Invites(api.guild_invites(guild).await?),
        Admin::RevokeInvite(code) => {
            api.revoke_invite(&code).await?;
            AdminAnswer::Invites(api.guild_invites(guild).await?)
        }
        Admin::Bans => AdminAnswer::Bans(api.bans(guild).await?),
        Admin::Unban(user) => {
            api.unban(guild, &user).await?;
            AdminAnswer::Bans(api.bans(guild).await?)
        }
        Admin::Kick { user, reason } => {
            api.kick(guild, &user, &reason).await?;
            AdminAnswer::Done
        }
        Admin::Ban {
            user,
            reason,
            delete_seconds,
        } => {
            api.ban(guild, &user, &reason, delete_seconds).await?;
            AdminAnswer::Done
        }
        // The member's change arrives as GUILD_MEMBER_UPDATE.
        Admin::MemberRole { user, role, on } => {
            api.set_member_role(guild, &user, &role, on).await?;
            AdminAnswer::Done
        }
        Admin::ModifyMember { user, body } => {
            api.modify_member(guild, &user, &body).await?;
            AdminAnswer::Done
        }
        // Role changes arrive as GUILD_ROLE_* events.
        Admin::CreateRole(name) => {
            api.create_role(guild, &json!({"name": name, "color": 0, "permissions": "0"}))
                .await?;
            AdminAnswer::Done
        }
        Admin::ModifyRole { role, body } => {
            api.modify_role(guild, &role, &body).await?;
            AdminAnswer::Done
        }
        Admin::DeleteRole(role) => {
            api.delete_role(guild, &role).await?;
            AdminAnswer::Done
        }
        Admin::CreateInvite { channel } => AdminAnswer::InviteCreated(api.create_invite(&channel).await?),
        // The new channel arrives as CHANNEL_CREATE.
        Admin::CreateChannel { name, kind, parent } => {
            let mut body = json!({"name": name, "type": kind, "permission_overwrites": []});
            if let Some(parent) = parent {
                body["parent_id"] = json!(parent);
            }
            api.create_channel(guild, &body).await?;
            AdminAnswer::Done
        }
        // Each moved channel arrives as CHANNEL_UPDATE.
        Admin::MoveChannels(moves) => {
            api.move_channels(guild, &moves).await?;
            AdminAnswer::Done
        }
        Admin::Leave => {
            api.leave_guild(guild).await?;
            AdminAnswer::Done
        }
        Admin::Delete => {
            api.delete_guild(guild).await?;
            AdminAnswer::Done
        }
        Admin::ModifyGuild(body) => {
            api.modify_guild(guild, &body).await?;
            AdminAnswer::Done
        }
        Admin::Emojis => AdminAnswer::Emojis(api.guild_emojis(guild).await?),
        Admin::CreateEmoji { name, path } => {
            if tokio::fs::metadata(&path).await?.len() > EMOJI_MAX_BYTES {
                anyhow::bail!("An emoji image is at most 256 KB.");
            }
            let image = emoji_uri(&tokio::fs::read(&path).await?)?;
            AdminAnswer::EmojiCreated(api.create_emoji(guild, &name, &image).await?)
        }
        Admin::RenameEmoji { id, name } => {
            api.rename_emoji(guild, &id, &name).await?;
            AdminAnswer::Done
        }
        Admin::DeleteEmoji(id) => {
            api.delete_emoji(guild, &id).await?;
            AdminAnswer::Done
        }
    })
}

/// How long typing notices are spaced (the web client sends one per ~8 s).
pub const TYPING_EVERY: Duration = Duration::from_secs(8);

/// Whether a refusal means the sign-in itself is gone.
pub fn refused_sign_in(message: &str) -> bool {
    message.contains("no longer accepts this sign-in")
}

/// Statuses a REST answer may carry that mean "doesn't exist any more".
pub fn gone(error: &anyhow::Error) -> bool {
    api::status(error) == Some(reqwest::StatusCode::NOT_FOUND)
}

/// An avatar file as the data URI Discord takes: PNG, JPEG or WebP, at most
/// 10 MB. A GIF (an animated avatar) is a Nitro feature, so it's refused.
async fn avatar_uri(path: &std::path::Path) -> anyhow::Result<String> {
    use base64::Engine;
    const MOST: usize = 10 << 20;
    let bytes = tokio::fs::read(path).await?;
    if bytes.len() > MOST {
        anyhow::bail!("The picture is over 10 MB.");
    }
    let mime = match bytes.as_slice() {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xff, 0xd8, 0xff, ..] => "image/jpeg",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        [b'G', b'I', b'F', ..] => anyhow::bail!("Animated avatars need Nitro on Discord; choose a PNG, JPEG or WebP."),
        _ => anyhow::bail!("Choose a PNG, JPEG or WebP picture."),
    };
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// The most Discord takes for an emoji image (docs.discord.food, Create Guild Emoji).
pub const EMOJI_MAX_BYTES: u64 = 256 << 10;

/// An emoji image as the data URI Discord takes: PNG, JPEG, WebP or GIF (a
/// GIF animates; uploading is limited by the server's slots, not Nitro), at
/// most 256 KiB.
pub fn emoji_uri(bytes: &[u8]) -> anyhow::Result<String> {
    use base64::Engine;
    if bytes.is_empty() || bytes.len() as u64 > EMOJI_MAX_BYTES {
        anyhow::bail!("An emoji image is at most 256 KB.");
    }
    let mime = match bytes {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xff, 0xd8, 0xff, ..] => "image/jpeg",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        [b'G', b'I', b'F', ..] => "image/gif",
        _ => anyhow::bail!("Choose a PNG, JPEG, GIF or WebP image."),
    };
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_errors_keep_the_source_and_existing_presentation() {
        let cause = std::io::Error::other("request failed");
        let error = RequestError::from(anyhow::Error::new(cause).context("uploading"));
        assert_eq!(error.to_string(), "uploading");
        assert!(std::error::Error::source(&error).is_some());
        let text: Arc<str> = error.into();
        assert_eq!(&*text, "uploading");
    }
}
