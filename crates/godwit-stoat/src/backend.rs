// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Network side: runs on a tokio runtime in a background thread. The UI sends
//! `Cmd`s and receives `Ev`s; every event triggers a repaint.

use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc as tmpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::api::*;

pub enum Cmd {
    Session(SessionCmd),
    AutoDelete(AutoDeleteCmd),
    Voice(VoiceCmd),
    Message(MessageCmd),
    People(PeopleCmd),
    Member(MemberCmd),
    Webhook(WebhookCmd),
    Channel(ChannelCmd),
    Role(RoleCmd),
    Emoji(EmojiCmd),
    Server(ServerCmd),
    Bot(BotCmd),
    Settings(SettingsCmd),
    Media(MediaCmd),
}

pub enum SessionCmd {
    Login {
        email: String,
        password: String,
    },
    Mfa {
        ticket: String,
        code: String,
    },
    UseSession(Session),
    /// Close the live connection (staying signed in), or open it again.
    SetOffline(bool),
    Logout,
}

pub enum AutoDeleteCmd {
    /// Adds auto-delete work to the paced queue (see autodelete.rs): deletions
    /// at most one request per `gap`, indexing at `autodelete::INDEX_GAP`.
    AutoDelete {
        jobs: Vec<crate::autodelete::Job>,
        gap: Duration,
    },
    /// Stops the auto-delete queue and drops what's waiting.
    AutoDeleteCancel,
    /// Wakes the account after this long ([`Ev::AutoDeleteTick`]), for its
    /// next auto-delete check; only while it has rules. A newer one replaces
    /// a pending one.
    AutoDeleteWake(Duration),
}

pub enum VoiceCmd {
    JoinVoice {
        generation: u64,
        channel: String,
        settings: Box<godwit_voice::call::VoiceSettings>,
    },
    Voice(godwit_voice::call::VoiceCmd),
    ListAudioDevices,
}

pub enum MessageCmd {
    FetchMessages {
        channel: String,
        before: Option<String>,
    },
    /// The 50 messages around `around`, for jumping to an old message.
    FetchMessagesNearby {
        channel: String,
        around: String,
    },
    /// `POST /channels/{id}/search` (official search and pinned sidebars); `key` identifies the request.
    SearchMessages {
        channel: String,
        key: String,
        body: Value,
    },
    FetchUnreads,
    /// `nonce` goes out as the `Idempotency-Key` (so a retry can't post
    /// twice) and comes back in `Ev::Sent` / `Ev::SendFailed`.
    Send {
        channel: String,
        content: String,
        replies: Vec<ReplyIntent>,
        files: Vec<Upload>,
        nonce: String,
    },
    Edit {
        channel: String,
        id: String,
        content: String,
    },
    Delete {
        channel: String,
        id: String,
    },
    React {
        channel: String,
        id: String,
        emoji: String,
        add: bool,
    },
    Ack {
        channel: String,
        message: String,
    },
    Pin {
        channel: String,
        id: String,
        pin: bool,
    },
    /// Marks every channel in a server read.
    AckServer {
        server: String,
    },
    ClearReactions {
        channel: String,
        id: String,
    },
    /// Removes everyone's reaction with one emoji.
    RemoveReaction {
        channel: String,
        id: String,
        emoji: String,
    },
    Typing {
        channel: String,
    },
}

pub enum PeopleCmd {
    /// `online_only` for large servers, as the official client does.
    FetchMembers {
        server: String,
        online_only: bool,
    },
    /// "Add a new friend" by `username#1234`.
    FriendRequest {
        request: Option<u64>,
        username: String,
    },
    Relationship {
        user: String,
        action: RelationshipAction,
    },
    FetchProfile {
        user: String,
    },
    FetchOwnProfile {
        user: String,
        after_save: bool,
    },
    OpenDm {
        user: String,
    },
}

pub enum MemberCmd {
    /// Edits a member (`DataMemberEdit`); the avatar is uploaded first.
    EditMember {
        server: String,
        user: String,
        body: Value,
        avatar: Option<Upload>,
    },
    KickMember {
        server: String,
        user: String,
    },
    /// Sets a member's whole role list (`DataMemberEdit.roles`), for server
    /// admin, which waits for it (answered with AdminDone).
    SetRoles {
        server: String,
        user: String,
        roles: Vec<String>,
    },
    BanUser {
        server: String,
        user: String,
        reason: String,
        delete_message_seconds: u64,
    },
    /// Removes someone from a group (answered with MemberSaved).
    RemoveRecipient {
        channel: String,
        user: String,
    },
    /// Makes someone the group's owner (answered with MemberSaved).
    TransferGroup {
        channel: String,
        user: String,
    },
}

pub enum WebhookCmd {
    /// A channel's webhooks; answers with `Webhooks`.
    FetchWebhooks {
        channel: String,
    },

    Edit(WebhookEditCmd),
}

pub enum WebhookEditCmd {
    CreateWebhook {
        channel: String,
        name: String,
    },
    /// `body` is a `DataEditWebhook`; the avatar is uploaded first.
    EditWebhook {
        id: String,
        body: Value,
        avatar: Option<Upload>,
    },
    DeleteWebhook {
        id: String,
    },
}

pub enum ChannelCmd {
    /// Creates a server channel; answers with `ChannelCreated`.
    CreateChannel {
        server: String,
        name: String,
        voice: bool,
        category: Option<String>,
    },

    /// Deletes a server channel (`DELETE /channels/{id}`); answers with
    /// `ChannelDeleted`. The channel leaves the lists by its ChannelDelete event.
    DeleteChannel {
        channel: String,
        request: u64,
    },
    /// Leaves a group (or closes a DM).
    LeaveChannel {
        channel: String,
        silent: bool,
    },
    Edit(ChannelEditCmd),
}

pub enum ChannelEditCmd {
    /// Edits a channel (`DataEditChannel`), uploading the icon first.
    EditChannel {
        channel: String,
        /// Echoed in `ChannelSaved`, so the page can tell its own answer.
        request: u64,
        body: Value,
        icon: Option<Upload>,
    },
    /// Sets a channel's default (`role` None) or role permissions; `body`
    /// is the `permissions` value (a number for groups, else allow/deny).
    SetChannelPermissions {
        channel: String,
        request: u64,
        role: Option<String>,
        permissions: Value,
    },
}

pub enum RoleCmd {
    /// Creates a role; answers with `RoleCreated`.
    CreateRole {
        server: String,
        name: String,
    },

    Edit(RoleEditCmd),
}

pub enum RoleEditCmd {
    /// Edits a role: `body` is a `DataEditRole` (sent if not empty), the
    /// icon is uploaded first, and `permissions` (allow, deny) are set after.
    EditRole {
        server: String,
        role: String,
        body: Value,
        icon: Option<Upload>,
        permissions: Option<(u64, u64)>,
    },
    /// The permissions everyone gets without a role.
    SetDefaultPermissions {
        server: String,
        permissions: u64,
    },
    DeleteRole {
        server: String,
        role: String,
    },
    /// Role ids, most important first.
    SetRoleRanks {
        server: String,
        ranks: Vec<String>,
    },
}

pub enum EmojiCmd {
    /// Uploads an image and makes it a server emoji.
    CreateEmoji {
        server: String,
        name: String,
        file: Upload,
    },
    RenameEmoji {
        id: String,
        name: String,
    },
    DeleteEmoji {
        id: String,
    },
}

pub enum ServerCmd {
    CreateServer {
        name: String,
    },
    CreateGroup {
        name: String,
        users: Vec<String>,
    },
    FetchInvite {
        code: String,
    },
    JoinInvite {
        code: String,
    },
    /// Server settings: invites and bans lists, and their removal.
    FetchServerInvites {
        server: String,
    },
    DeleteInvite {
        server: String,
        code: String,
    },
    FetchBans {
        server: String,
    },
    Unban {
        server: String,
        user: String,
    },
    SaveServerOverview {
        server: String,
        request: u64,
        body: Value,
        icon: Option<Upload>,
        banner: Option<Upload>,
    },
    EditServer {
        server: String,
        body: Value,
        icon: Option<Upload>,
        banner: Option<Upload>,
    },
    CreateInvite {
        channel: String,
    },
    LeaveServer {
        server: String,
        silent: bool,
    },
    /// The owner deletes the server (`DELETE /servers/{id}`, as for leaving).
    DeleteServer {
        server: String,
    },
}

pub enum BotCmd {
    /// Server Overview's Save: uploads a new icon/banner first, then `PATCH /servers/{id}`.
    /// A server's (`servers/{id}`) or bot's (`bots/{id}`) discover request;
    /// answers with `DiscoverStatus`.
    FetchDiscover {
        target: String,
    },
    /// Submits (true) or cancels a discover request, then refetches it.
    SetDiscover {
        target: String,
        request: bool,
    },
    /// A public bot, to add it (`PublicBot`).
    FetchPublicBot {
        id: String,
    },
    /// Our bots (`GET /bots/@me`).
    FetchBots,
    CreateBot {
        name: String,
    },
    /// `PATCH /bots/{id}` with a `DataEditBot` body (name, public, `remove: ["Token"]`).
    EditBot {
        id: String,
        body: Value,
    },
    DeleteBot {
        id: String,
    },
    /// Adds a bot to a server or group (`destination` is `{ "server": id }` or `{ "group": id }`).
    InviteBot {
        id: String,
        destination: Value,
    },
}

pub enum SettingsCmd {
    /// Read-only fetch of the synced settings we use (notifications, ordering, folders).
    FetchSyncedSettings,
    /// Our login email (Account → Reveal).
    FetchEmail,
    /// Whether the account has an authenticator and recovery codes.
    FetchMfaStatus,
    /// A confirmed account security action: gets an MFA ticket with `response`, then runs it.
    MfaAction {
        action: MfaAction,
        response: MfaResponse,
    },
    /// Finishes turning on the authenticator with a code from it.
    EnableTotp {
        code: String,
    },
    /// Changes username, email or password. `totp` gets an MFA ticket first
    /// for the endpoints that require one when 2FA is on.
    ChangeAccount {
        change: AccountChange,
        totp: Option<String>,
    },
    /// `POST /safety/report` with a `DataReportContent` body.
    Report(Value),
    /// The profile editor's Save: uploads a new avatar/banner first, then
    /// `PATCH /users/@me` with `body` plus their ids.
    /// Edits our profile, or (`user`) one of our bots'.
    SaveProfile {
        user: Option<String>,
        body: Value,
        avatar: Option<Upload>,
        banner: Option<Upload>,
    },
    /// `PATCH /users/@me` with a `DataEditUser` body (status, presence, …).
    EditMe(Value),
    /// Writes one synced settings key (its value is a JSON string), as the official client does.
    SetSyncedSetting {
        key: String,
        value: String,
    },
    FetchSessions,
    RenameSession {
        id: String,
        name: String,
    },
    /// Which second factors the account accepts, for the confirm dialog.
    FetchMfaMethods,
    /// Gets an MFA ticket with `response`, then logs out the target session(s).
    RevokeSessions {
        target: RevokeTarget,
        response: MfaResponse,
    },
}

pub enum MediaCmd {
    /// gifbox's categories.
    GifCategories,
    /// gifbox: trending ([`godwit_core::gifs::TRENDING`]) or a search, from
    /// `position` (the previous page's `next`).
    SearchGifs { query: String, position: Option<String> },
    /// The latest changelog (official What's New); `auto` when checking at startup.
    FetchChangelog { auto: bool },
}

/// Account security actions that need a validated MFA ticket.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MfaAction {
    /// Start setting up an authenticator: returns the secret.
    TotpSecret,
    RemoveTotp,
    GenerateRecovery,
    ViewRecovery,
    DisableAccount,
    DeleteAccount,
}

/// The official account edit dialogs' requests (passwords never logged).
#[derive(Clone)]
pub enum AccountChange {
    Username { username: String, password: String },
    Email { email: String, current_password: String },
    Password { password: String, current_password: String },
}

/// Friend and block actions from the official user menu.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RelationshipAction {
    /// Accept a request, or send one to someone we already know.
    AddFriend,
    /// Remove a friend, reject or cancel a request.
    RemoveFriend,
    Block,
    Unblock,
}

#[derive(Clone, PartialEq)]
pub enum RevokeTarget {
    One(String),
    /// Every session except this one.
    Others,
}

/// A file queued for sending: picked/dropped from disk, or pasted image bytes.
#[derive(Clone)]
pub struct Upload {
    pub name: String,
    pub size: u64,
    pub source: UploadSource,
}

#[derive(Clone)]
pub enum UploadSource {
    Path(std::path::PathBuf),
    Bytes(std::sync::Arc<Vec<u8>>),
}

pub enum Ev {
    AutoDelete(AutoDeleteEv),
    /// Time for the next auto-delete check (see [`Cmd::AutoDeleteWake`]).
    AutoDeleteTick,
    Config(RootConfig),
    MfaRequired {
        ticket: String,
    },
    LoggedIn(Session),
    /// The message sent as `nonce`, as the server stored it.
    Sent {
        channel: String,
        nonce: String,
        message: Message,
    },
    SendFailed {
        channel: String,
        nonce: String,
        error: String,
    },
    LoggedOut,
    Connecting,
    Ready(Ready),
    /// A raw websocket event other than Ready; parsed by the store.
    Event(Value),
    Messages {
        channel: String,
        before: Option<String>,
        data: MessagesWithUsers,
    },
    MessagesFailed {
        channel: String,
    },
    MessagesNearby {
        channel: String,
        around: String,
        data: MessagesWithUsers,
    },
    SearchResults {
        key: String,
        result: Result<MessagesWithUsers, String>,
    },
    /// A pin or unpin finished (the pinned list is worth loading again).
    Pinned {
        channel: String,
        result: Result<(), String>,
    },
    Members {
        data: AllMembers,
    },
    Unreads(Vec<ChannelUnread>),
    /// The `/sync/settings/fetch` response: key → `[timestamp, json]`.
    SyncedSettings(Value),
    Profile {
        user: String,
        profile: UserProfile,
    },
    OwnProfile {
        after_save: bool,
        result: Result<UserProfile, String>,
    },
    DmOpened(Channel),

    Voice {
        generation: u64,
        channel: String,
        event: godwit_voice::call::VoiceEvent,
    },
    AudioDevices(godwit_voice::call::AudioDevices),
    Sessions(Vec<SessionInfo>),
    SessionRenamed(SessionInfo),
    MfaMethods(Vec<String>),
    /// The confirm dialog's request failed; shown in the dialog, not as a toast.
    MfaFailed(String),
    /// A server we created or joined, to add and open.
    ServerJoined(ServerWithChannels),
    GifCategories(Result<Vec<GifCategory>, String>),
    Gifs {
        query: String,
        more: bool,
        page: Result<GifPage, String>,
    },
    /// What an invite leads to, or why it couldn't be read.
    InvitePreview {
        code: String,
        preview: Result<InvitePreview, String>,
    },
    /// Joined through an invite (the server and its channels), or why not.
    InviteJoined {
        code: String,
        result: Result<ServerWithChannels, String>,
    },
    /// A create/join dialog's request failed; shown in that dialog.
    FormError(String),
    /// A user whose relationship to us changed (also arrives as a UserRelationship event).
    UserChanged(User),
    /// A user we edited, as the server returned it.
    UserSaved(User),
    Email(String),
    /// Whether an authenticator and recovery codes are set up.
    MfaStatus {
        totp: bool,
        recovery: bool,
    },
    /// Result of an `MfaAction` (the secret or recovery codes where there are some).
    MfaActionDone {
        action: MfaAction,
        result: Result<Value, String>,
    },
    TotpEnabled(Result<(), String>),
    /// Result of an account change dialog.
    AccountChanged(Result<(), String>),
    ServerInvites {
        server: String,
        invites: Vec<Value>,
    },
    Bans {
        server: String,
        data: Value,
    },
    /// Result of server Overview's Save.
    ServerSaved(Result<(), String>),
    ServerOverviewSaved {
        server: String,
        request: u64,
        result: Result<crate::api::Server, String>,
    },
    ChannelCreated {
        channel: Result<Value, String>,
        category: Option<String>,
    },
    PublicBot {
        id: String,
        username: String,
    },
    Changelog {
        changelog: crate::api::Changelog,
        auto: bool,
    },
    /// A member edit, kick or ban finished (the change arrives by event).
    MemberSaved(Result<(), String>),
    /// The request (None: there is none, so one can be made) or why it
    /// couldn't be fetched.
    DiscoverStatus {
        target: String,
        status: Result<Option<Value>, String>,
    },
    /// Webhook objects (they carry their secret token: never log them).
    Webhooks {
        channel: String,
        list: Vec<Value>,
    },
    /// A webhook was created or edited (the webhook) or deleted (`id` only).
    WebhookSaved {
        result: Result<Value, String>,
        deleted: Option<String>,
    },
    /// A channel edit or permission change finished; an edit answers with
    /// the channel as the server now has it.
    ChannelSaved {
        channel: String,
        request: u64,
        result: Result<Option<crate::api::Channel>, String>,
    },
    ChannelDeleted {
        channel: String,
        request: u64,
        result: Result<(), String>,
    },
    RoleCreated {
        server: String,
        id: String,
    },
    /// A role edit, permission change, deletion or reorder finished.
    RoleSaved {
        server: String,
        result: Result<(), String>,
    },
    /// A server settings change finished: invite deleted, unban, kick, ban,
    /// or (failing only: success removes the server) its deletion.
    AdminDone {
        server: String,
        result: Result<(), String>,
    },
    /// A custom emoji was created, renamed (the emoji) or deleted (null).
    EmojiSaved(Result<Value, String>),
    /// `{ bots, users }` from `/bots/@me`. Holds bot tokens: never log it.
    Bots(Value),
    /// A bot request failed, or "Bot added.".
    BotNotice(String),
    /// Result of a report.
    Reported(Result<(), String>),
    /// Result of the profile editor's Save.
    ProfileSaved {
        user: Option<String>,
        result: Result<(), String>,
    },
    /// Result of "Add a new friend".
    FriendRequest {
        request: Option<u64>,
        result: Result<(), String>,
    },
    /// Invite code, or why it couldn't be made.
    Invite(Result<String, String>),
    Error(String),
}

pub use godwit_core::backend::Waker;

pub type Handle = godwit_core::backend::Handle<Cmd>;

pub fn spawn(wake: Waker) -> std::io::Result<(Handle, mpsc::Receiver<Ev>)> {
    godwit_core::backend::spawn(
        "stoat-net",
        {
            let mut builder = tokio::runtime::Builder::new_multi_thread();
            // Two workers are plenty for a chat client's network traffic; each
            // extra thread costs memory (stack, allocator arena).
            builder.worker_threads(2);
            builder
        },
        move |tx| Emitter {
            events: godwit_core::backend::Events::new(tx, wake),
        },
        run,
        |out, e| out.error("starting the network runtime", e.into()),
    )
}

/// Auto-delete queue progress.
#[derive(Debug, Clone)]
pub enum AutoDeleteEv {
    /// A page of history read: everything from `lo` ("" is the start) to `hi`
    /// is seen, and these are our messages in it (id, kind letter).
    Indexed {
        channel: String,
        lo: String,
        hi: String,
        ours: Vec<(String, char)>,
    },
    /// A message is gone (deleted now, or already gone).
    Deleted { rule: u64, channel: String, id: String },
    /// The index listed a message that turned out not to be ours: it's
    /// left alone and dropped from the index.
    NotOurs { channel: String, id: String },
    /// A purge dealt with everything in `channel` up to `hi`.
    Swept { rule: u64, channel: String, hi: String },
    /// A job couldn't be finished; it's tried again later.
    Failed {
        rule: Option<u64>,
        channel: String,
        error: String,
    },
    /// The queue is empty (finished or cancelled).
    Idle,
}

/// Messages to the auto-delete queue.
enum AutoDeleteMsg {
    Jobs(Rest, Vec<crate::autodelete::Job>, Duration),
    Cancel,
}

#[derive(Clone)]
struct Emitter {
    events: godwit_core::backend::Events<Ev>,
}

impl Emitter {
    fn emit(&self, ev: Ev) {
        self.events.emit(ev);
    }

    fn error(&self, context: &str, err: anyhow::Error) {
        let msg = format!("Stoat, {context}: {}", crate::errors::describe(&err));
        eprintln!("error: {msg}");
        self.emit(Ev::Error(msg));
    }
}

#[derive(Clone)]
struct Rest {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl Rest {
    /// A request to another of Stoat's services (gifbox), signed in as here.
    fn absolute(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut r = self.http.request(method, url);
        if let Some(t) = &self.token {
            r = r.header("X-Session-Token", t);
        }
        r
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut r = self.http.request(method, format!("{}{}", self.base, path));
        if let Some(t) = &self.token {
            r = r.header("X-Session-Token", t);
        }
        r
    }

    async fn send<T: serde::de::DeserializeOwned>(&self, rb: reqwest::RequestBuilder) -> Result<T> {
        let res = rb.send().await?;
        let status = res.status();
        if !status.is_success() {
            let body = res.text().await.unwrap_or_default();
            return Err(crate::errors::ApiError::new(status.as_u16(), &body).into());
        }
        Ok(res.json().await?)
    }

    async fn send_empty(&self, rb: reqwest::RequestBuilder) -> Result<()> {
        let res = rb.send().await?;
        let status = res.status();
        if !status.is_success() {
            let body = res.text().await.unwrap_or_default();
            return Err(crate::errors::ApiError::new(status.as_u16(), &body).into());
        }
        Ok(())
    }
}

/// Percent-encodes a query parameter (as `encodeURIComponent`).
use crate::api::encode_uri_component as encode_query;

/// The official client joins calls on whichever voice node answers first:
/// one request to each node, first reply wins. None if there are no nodes or
/// none answered within a few seconds.
async fn pick_voice_node(http: &reqwest::Client, nodes: &[VoiceNode]) -> Option<String> {
    if nodes.is_empty() {
        return None;
    }
    let probes = nodes.iter().map(|n| {
        let url = n.public_url.replacen("wss", "https", 1);
        let name = n.name.clone();
        let http = http.clone();
        Box::pin(async move {
            // Any HTTP response counts: we only want the round trip.
            http.get(url).timeout(Duration::from_secs(5)).send().await.map(|_| name)
        })
    });
    futures_util::future::select_ok(probes).await.ok().map(|(name, _)| name)
}

/// Session name shown in Session settings, e.g. "Godwit (Linux)"; the official
/// client picks the session icon by matching the OS name in it.
fn friendly_name() -> String {
    let os = match std::env::consts::OS {
        "linux" => "Linux",
        "windows" => "Windows",
        "macos" => "macOS",
        other => other,
    };
    format!("Godwit ({os})")
}

async fn run(mut cmds: tmpsc::UnboundedReceiver<Cmd>, out: Emitter) {
    let http = match reqwest::Client::builder()
        // Generic on purpose: no version, nothing about this machine.
        .user_agent("Godwit")
        .build()
    {
        Ok(http) => http,
        Err(e) => return out.error("starting the HTTP client", e.into()),
    };
    let rest = Rest {
        http,
        base: DEFAULT_API.to_string(),
        token: None,
    };

    let config: RootConfig = loop {
        match rest.send(rest.req(reqwest::Method::GET, "/")).await {
            Ok(c) => break c,
            Err(e) => {
                out.error("fetching server config", e);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };
    out.emit(Ev::Config(config.clone()));

    let mut backend = Backend {
        rest,
        config,
        out,
        ws_tx: None,
        ws_task: None,
        voice_node: Default::default(),
        voice_tx: None,
        voice_join_cancel: None,
        autodelete_tx: None,
        autodelete_wake: None,
    };
    while let Some(cmd) = cmds.recv().await {
        match cmd {
            Cmd::Session(cmd) => backend.session(cmd).await,
            Cmd::AutoDelete(cmd) => backend.autodelete(cmd).await,
            Cmd::Voice(cmd) => backend.voice(cmd).await,
            Cmd::Message(cmd) => backend.message(cmd).await,
            Cmd::People(cmd) => backend.people(cmd).await,
            Cmd::Member(cmd) => backend.member(cmd).await,
            Cmd::Webhook(cmd) => backend.webhook(cmd).await,
            Cmd::Channel(cmd) => backend.channel(cmd).await,
            Cmd::Role(cmd) => backend.role(cmd).await,
            Cmd::Emoji(cmd) => backend.emoji(cmd).await,
            Cmd::Server(cmd) => backend.server(cmd).await,
            Cmd::Bot(cmd) => backend.bot(cmd).await,
            Cmd::Settings(cmd) => backend.settings(cmd).await,
            Cmd::Media(cmd) => backend.media(cmd).await,
        }
    }
}

struct Backend {
    rest: Rest,
    config: RootConfig,
    out: Emitter,
    // Sender into the currently running websocket task, if any.
    ws_tx: Option<tmpsc::UnboundedSender<Value>>,
    ws_task: Option<tokio::task::JoinHandle<()>>,
    // The voice node picked for this session (see `pick_voice_node`).
    voice_node: std::sync::Arc<tokio::sync::Mutex<Option<String>>>,
    // The current call, if any.
    voice_tx: Option<tmpsc::UnboundedSender<godwit_voice::call::VoiceCmd>>,
    // Dropping this cancels node discovery and the pending REST join only.
    // Established calls still leave cooperatively so media cleanup runs.
    voice_join_cancel: Option<tokio::sync::oneshot::Sender<()>>,
    // The auto-delete queue, started on first use.
    autodelete_tx: Option<tmpsc::UnboundedSender<AutoDeleteMsg>>,
    // The pending auto-delete wake-up, if any.
    autodelete_wake: Option<tokio::task::JoinHandle<()>>,
}

impl Backend {
    async fn session(&mut self, cmd: SessionCmd) {
        let Self {
            rest,
            config,
            out,
            ws_tx,
            ws_task,
            voice_tx,
            voice_join_cancel,
            autodelete_tx,
            ..
        } = self;
        match cmd {
            SessionCmd::Login { email, password } => {
                let body = json!({ "email": email, "password": password, "friendly_name": friendly_name() });
                let rb = rest.req(reqwest::Method::POST, "/auth/session/login").json(&body);
                handle_login(rest, rb, out).await;
            }
            SessionCmd::Mfa { ticket, code } => {
                let body = json!({
                    "mfa_ticket": ticket,
                    "mfa_response": { "totp_code": code },
                    "friendly_name": friendly_name(),
                });
                let rb = rest.req(reqwest::Method::POST, "/auth/session/login").json(&body);
                handle_login(rest, rb, out).await;
            }
            SessionCmd::UseSession(session) => {
                rest.token = Some(session.token.clone());
                if let Some(t) = ws_task.take() {
                    t.abort();
                }
                let (tx, rx) = tmpsc::unbounded_channel();
                *ws_tx = Some(tx);
                *ws_task = Some(tokio::spawn(websocket_loop(
                    config.ws.clone(),
                    session.token,
                    rx,
                    out.clone(),
                )));
            }
            SessionCmd::SetOffline(offline) => {
                if let Some(t) = ws_task.take() {
                    t.abort();
                }
                *ws_tx = None;
                if offline {
                    // Offline means nothing live: a call ends too.
                    voice_join_cancel.take();
                    if let Some(tx) = voice_tx.take() {
                        let _ = tx.send(godwit_voice::call::VoiceCmd::Leave);
                    }
                } else if let Some(token) = rest.token.clone() {
                    let (tx, rx) = tmpsc::unbounded_channel();
                    *ws_tx = Some(tx);
                    *ws_task = Some(tokio::spawn(websocket_loop(config.ws.clone(), token, rx, out.clone())));
                }
            }
            SessionCmd::Logout => {
                // Signing out ends any call first: the backend can outlive the
                // sign-in (kept as the spare login account), and a call left running
                // would keep the microphone, camera or screen share going.
                voice_join_cancel.take();
                if let Some(tx) = voice_tx.take() {
                    let _ = tx.send(godwit_voice::call::VoiceCmd::Leave);
                }
                if let Some(t) = ws_task.take() {
                    t.abort();
                }
                // Its requests carry the old session.
                if let Some(tx) = &autodelete_tx {
                    let _ = tx.send(AutoDeleteMsg::Cancel);
                }
                *ws_tx = None;
                let rb = rest.req(reqwest::Method::POST, "/auth/session/logout");
                let _ = rest.send_empty(rb).await;
                rest.token = None;
                out.emit(Ev::LoggedOut);
            }
        }
    }
    async fn autodelete(&mut self, cmd: AutoDeleteCmd) {
        let Self {
            rest,
            out,
            autodelete_tx,
            autodelete_wake,
            ..
        } = self;
        match cmd {
            AutoDeleteCmd::AutoDelete { jobs, gap } => {
                let tx = autodelete_tx.get_or_insert_with(|| {
                    let (tx, rx) = tmpsc::unbounded_channel();
                    tokio::spawn(autodelete_queue(rx, out.clone()));
                    tx
                });
                let _ = tx.send(AutoDeleteMsg::Jobs(rest.clone(), jobs, gap));
            }
            AutoDeleteCmd::AutoDeleteCancel => {
                if let Some(tx) = &autodelete_tx {
                    let _ = tx.send(AutoDeleteMsg::Cancel);
                }
            }
            AutoDeleteCmd::AutoDeleteWake(after) => {
                if let Some(pending) = autodelete_wake.take() {
                    pending.abort();
                }
                let out = out.clone();
                *autodelete_wake = Some(tokio::spawn(async move {
                    tokio::time::sleep(after).await;
                    out.emit(Ev::AutoDeleteTick);
                }));
            }
        }
    }
    async fn voice(&mut self, cmd: VoiceCmd) {
        let Self {
            rest,
            config,
            out,
            voice_node,
            voice_tx,
            voice_join_cancel,
            ..
        } = self;
        match cmd {
            VoiceCmd::JoinVoice {
                channel,
                generation,
                settings,
            } => {
                use godwit_voice::call::{self, VoiceCmd};
                // Only one call at a time: leave the current one first.
                if let Some(tx) = voice_tx.take() {
                    let _ = tx.send(VoiceCmd::Leave);
                }
                voice_join_cancel.take();
                let (cancel, cancelled) = tokio::sync::oneshot::channel();
                *voice_join_cancel = Some(cancel);
                let (tx, rx) = tmpsc::unbounded_channel();
                *voice_tx = Some(tx);
                let rest = rest.clone();
                let out = out.clone();
                let nodes = config
                    .features
                    .livekit
                    .as_ref()
                    .map(|l| l.nodes.clone())
                    .unwrap_or_default();
                let voice_node = voice_node.clone();
                tokio::spawn(async move {
                    #[derive(serde::Deserialize)]
                    struct Joined {
                        token: String,
                        url: String,
                    }
                    let result: Result<Joined, _> = tokio::select! {
                        biased;
                        _ = cancelled => return,
                        result = async {
                    // Server calls must name a node; pick it once per session.
                    let node = {
                        let mut cached = voice_node.lock().await;
                        if cached.is_none() {
                            *cached = pick_voice_node(&rest.http, &nodes).await;
                        }
                        cached.clone()
                    };
                    // force_disconnect moves the call here from any other device, as the official client does.
                    let rb = rest
                        .req(reqwest::Method::POST, &format!("/channels/{channel}/join_call"))
                        .json(&json!({ "node": node, "force_disconnect": true }));
                            rest.send(rb).await
                        } => result,
                    };
                    let joined = match result {
                        Ok(j) => j,
                        Err(e) => {
                            out.emit(Ev::Voice {
                                generation,
                                channel,
                                event: call::VoiceEvent::Disconnected { reason: None },
                            });
                            out.error("joining voice", e);
                            return;
                        }
                    };
                    let emitter = out.clone();
                    let event_channel = channel.clone();
                    let emit: std::sync::Arc<dyn Fn(call::VoiceEvent) + Send + Sync> =
                        std::sync::Arc::new(move |event| {
                            emitter.emit(Ev::Voice {
                                generation,
                                channel: event_channel.clone(),
                                event,
                            })
                        });
                    call::run(channel, joined.url, joined.token, *settings, rx, emit).await;
                });
            }
            VoiceCmd::Voice(cmd) => {
                let leaving = matches!(cmd, godwit_voice::call::VoiceCmd::Leave);
                if let Some(tx) = &voice_tx {
                    let _ = tx.send(cmd);
                }
                if leaving {
                    voice_join_cancel.take();
                    *voice_tx = None;
                }
            }
            VoiceCmd::ListAudioDevices => {
                let out = out.clone();
                tokio::task::spawn_blocking(move || match godwit_voice::call::list_devices() {
                    Ok(d) => out.emit(Ev::AudioDevices(d)),
                    Err(e) => out.error("listing audio devices", e),
                });
            }
        }
    }
    async fn message(&mut self, cmd: MessageCmd) {
        let Self {
            rest,
            config,
            out,
            ws_tx,
            ..
        } = self;
        match cmd {
            MessageCmd::FetchMessages { channel, before } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let mut q = vec![("limit", "50".to_string()), ("include_users", "true".to_string())];
                    if let Some(b) = &before {
                        q.push(("before", b.clone()));
                    }
                    let rb = rest
                        .req(reqwest::Method::GET, &format!("/channels/{channel}/messages"))
                        .query(&q);
                    match rest.send::<MessagesWithUsers>(rb).await {
                        Ok(data) => out.emit(Ev::Messages { channel, before, data }),
                        Err(e) => {
                            out.emit(Ev::MessagesFailed { channel });
                            out.error("loading messages", e);
                        }
                    }
                });
            }
            MessageCmd::FetchMessagesNearby { channel, around } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let q = [("limit", "50"), ("include_users", "true"), ("nearby", around.as_str())];
                    let rb = rest
                        .req(reqwest::Method::GET, &format!("/channels/{channel}/messages"))
                        .query(&q);
                    match rest.send::<MessagesWithUsers>(rb).await {
                        Ok(data) => out.emit(Ev::MessagesNearby { channel, around, data }),
                        Err(e) => out.error("loading messages", e),
                    }
                });
            }
            MessageCmd::SearchMessages { channel, key, mut body } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    body["include_users"] = json!(true);
                    let rb = rest
                        .req(reqwest::Method::POST, &format!("/channels/{channel}/search"))
                        .json(&body);
                    let result = rest
                        .send::<MessagesWithUsers>(rb)
                        .await
                        .map_err(|e| crate::errors::describe(&e));
                    out.emit(Ev::SearchResults { key, result });
                });
            }
            MessageCmd::FetchUnreads => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    match rest
                        .send::<Vec<ChannelUnread>>(rest.req(reqwest::Method::GET, "/sync/unreads"))
                        .await
                    {
                        Ok(u) => out.emit(Ev::Unreads(u)),
                        Err(e) => out.error("loading unread state", e),
                    }
                });
            }
            MessageCmd::Send {
                channel,
                content,
                replies,
                files,
                nonce,
            } => {
                let rest = rest.clone();
                let out = out.clone();
                let autumn = config.features.autumn.url.clone();
                tokio::spawn(async move {
                    let failed = |error: String| Ev::SendFailed {
                        channel: channel.clone(),
                        nonce: nonce.clone(),
                        error,
                    };
                    // Upload attachments one at a time, then send the message referencing them.
                    let mut ids = Vec::new();
                    for f in files {
                        match upload(&rest, &autumn, f.clone(), "attachments").await {
                            Ok(id) => ids.push(id),
                            Err(e) => {
                                out.emit(failed(format!("uploading {}: {e:#}", f.name)));
                                return;
                            }
                        }
                    }
                    let mut body = json!({ "content": content, "replies": replies });
                    if !ids.is_empty() {
                        body["attachments"] = json!(ids);
                    }
                    let rb = rest
                        .req(reqwest::Method::POST, &format!("/channels/{channel}/messages"))
                        .header("Idempotency-Key", nonce.as_str())
                        .json(&body);
                    // The message also arrives over the websocket; the UI
                    // matches the two by id.
                    match rest.send::<Message>(rb).await {
                        Ok(message) => out.emit(Ev::Sent {
                            channel: channel.clone(),
                            nonce: nonce.clone(),
                            message,
                        }),
                        Err(e) => out.emit(failed(format!("sending message: {e:#}"))),
                    }
                });
            }
            MessageCmd::Edit { channel, id, content } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let rb = rest
                        .req(reqwest::Method::PATCH, &format!("/channels/{channel}/messages/{id}"))
                        .json(&json!({ "content": content }));
                    if let Err(e) = rest.send::<Value>(rb).await {
                        out.error("editing message", e);
                    }
                });
            }
            MessageCmd::Delete { channel, id } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let rb = rest.req(reqwest::Method::DELETE, &format!("/channels/{channel}/messages/{id}"));
                    if let Err(e) = rest.send_empty(rb).await {
                        out.error("deleting message", e);
                    }
                });
            }
            MessageCmd::AckServer { server } => {
                let rest = rest.clone();
                tokio::spawn(async move {
                    let _ = rest
                        .send_empty(rest.req(reqwest::Method::PUT, &format!("/servers/{server}/ack")))
                        .await;
                });
            }
            MessageCmd::Pin { channel, id, pin } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let method = if pin {
                        reqwest::Method::POST
                    } else {
                        reqwest::Method::DELETE
                    };
                    // The change arrives back as a MessageUpdate event; the pins
                    // panel loads its list again either way.
                    let result = rest
                        .send_empty(rest.req(method, &format!("/channels/{channel}/messages/{id}/pin")))
                        .await
                        .map_err(|e| crate::errors::describe(&e));
                    out.emit(Ev::Pinned { channel, result });
                });
            }
            MessageCmd::ClearReactions { channel, id } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let rb = rest.req(
                        reqwest::Method::DELETE,
                        &format!("/channels/{channel}/messages/{id}/reactions"),
                    );
                    if let Err(e) = rest.send_empty(rb).await {
                        out.error("removing reactions", e);
                    }
                });
            }
            MessageCmd::RemoveReaction { channel, id, emoji } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let path = format!("/channels/{channel}/messages/{id}/reactions/{emoji}");
                    if let Err(e) = rest
                        .send_empty(rest.req(reqwest::Method::DELETE, &path).query(&[("remove_all", true)]))
                        .await
                    {
                        out.error("removing reaction", e);
                    }
                });
            }
            MessageCmd::React {
                channel,
                id,
                emoji,
                add,
            } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    // Unicode emoji are percent-encoded by the URL parser.
                    let path = format!("/channels/{channel}/messages/{id}/reactions/{emoji}");
                    let method = if add {
                        reqwest::Method::PUT
                    } else {
                        reqwest::Method::DELETE
                    };
                    godwit_core::verbose!("reaction: sending method={method} emoji={emoji}");
                    // Match the official SDK's Message.unreact: explicitly
                    // remove only our reaction, never the whole emoji.
                    let request = rest.req(method, &path);
                    let request = if add {
                        request
                    } else {
                        request.query(&[("remove_all", false)])
                    };
                    // The change arrives back as a MessageReact/MessageUnreact event.
                    match rest.send_empty(request).await {
                        Ok(()) => godwit_core::verbose!("reaction: request succeeded emoji={emoji} add={add}"),
                        Err(e) => out.error("updating reaction", e),
                    }
                });
            }
            MessageCmd::Ack { channel, message } => {
                let rest = rest.clone();
                tokio::spawn(async move {
                    let rb = rest.req(reqwest::Method::PUT, &format!("/channels/{channel}/ack/{message}"));
                    let _ = rest.send_empty(rb).await;
                });
            }
            MessageCmd::Typing { channel } => {
                if let Some(tx) = &ws_tx {
                    let _ = tx.send(json!({ "type": "BeginTyping", "channel": channel }));
                }
            }
        }
    }
    async fn people(&mut self, cmd: PeopleCmd) {
        let Self { rest, out, .. } = self;
        match cmd {
            PeopleCmd::FetchMembers { server, online_only } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let rb = rest
                        .req(reqwest::Method::GET, &format!("/servers/{server}/members"))
                        .query(&[("exclude_offline", online_only)]);
                    match rest.send::<AllMembers>(rb).await {
                        Ok(data) => out.emit(Ev::Members { data }),
                        Err(e) => out.error("loading members", e),
                    }
                });
            }
            PeopleCmd::FetchProfile { user } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    match rest
                        .send::<UserProfile>(rest.req(reqwest::Method::GET, &format!("/users/{user}/profile")))
                        .await
                    {
                        Ok(profile) => out.emit(Ev::Profile { user, profile }),
                        Err(e) => out.error("loading profile", e),
                    }
                });
            }
            PeopleCmd::FetchOwnProfile { user, after_save } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let result = rest
                        .send::<UserProfile>(rest.req(reqwest::Method::GET, &format!("/users/{user}/profile")))
                        .await
                        .map_err(|error| crate::errors::describe(&error));
                    out.emit(Ev::OwnProfile { after_save, result });
                });
            }
            PeopleCmd::OpenDm { user } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    match rest
                        .send::<Channel>(rest.req(reqwest::Method::GET, &format!("/users/{user}/dm")))
                        .await
                    {
                        Ok(channel) => out.emit(Ev::DmOpened(channel)),
                        Err(e) => {
                            let msg = format!("Stoat, opening a direct message: {}", crate::errors::describe_dm(&e));
                            eprintln!("error: {msg}");
                            out.emit(Ev::Error(msg));
                        }
                    }
                });
            }
            PeopleCmd::FriendRequest { request, username } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let rb = rest
                        .req(reqwest::Method::POST, "/users/friend")
                        .json(&json!({ "username": username }));
                    match rest.send::<User>(rb).await {
                        Ok(u) => {
                            out.emit(Ev::UserChanged(u));
                            out.emit(Ev::FriendRequest {
                                request,
                                result: Ok(()),
                            });
                        }
                        Err(e) => out.emit(Ev::FriendRequest {
                            request,
                            result: Err(crate::errors::describe(&e)),
                        }),
                    }
                });
            }
            PeopleCmd::Relationship { user, action } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let (method, what) = match action {
                        RelationshipAction::AddFriend => (reqwest::Method::PUT, "friend"),
                        RelationshipAction::RemoveFriend => (reqwest::Method::DELETE, "friend"),
                        RelationshipAction::Block => (reqwest::Method::PUT, "block"),
                        RelationshipAction::Unblock => (reqwest::Method::DELETE, "block"),
                    };
                    match rest
                        .send::<User>(rest.req(method, &format!("/users/{user}/{what}")))
                        .await
                    {
                        Ok(u) => out.emit(Ev::UserChanged(u)),
                        Err(e) => out.error("updating relationship", e),
                    }
                });
            }
        }
    }
    async fn member(&mut self, cmd: MemberCmd) {
        let Self { rest, config, out, .. } = self;

        let rest = rest.clone();
        let out = out.clone();
        let autumn = config.features.autumn.url.clone();
        // Kicks and bans come from server admin, which waits for them.
        let admin = match &cmd {
            MemberCmd::KickMember { server, .. }
            | MemberCmd::BanUser { server, .. }
            | MemberCmd::SetRoles { server, .. } => Some(server.clone()),
            MemberCmd::EditMember { .. } | MemberCmd::RemoveRecipient { .. } | MemberCmd::TransferGroup { .. } => None,
        };
        tokio::spawn(async move {
            let result: Result<()> = async {
                match cmd {
                    MemberCmd::EditMember {
                        server,
                        user,
                        mut body,
                        avatar,
                    } => {
                        if let Some(f) = avatar {
                            body["avatar"] = json!(upload(&rest, &autumn, f, "avatars").await?);
                        }
                        rest.send::<Value>(
                            rest.req(reqwest::Method::PATCH, &format!("/servers/{server}/members/{user}"))
                                .json(&body),
                        )
                        .await?;
                    }
                    MemberCmd::SetRoles { server, user, roles } => {
                        rest.send::<Value>(
                            rest.req(reqwest::Method::PATCH, &format!("/servers/{server}/members/{user}"))
                                .json(&json!({ "roles": roles })),
                        )
                        .await?;
                    }
                    MemberCmd::KickMember { server, user } => {
                        rest.send_empty(
                            rest.req(reqwest::Method::DELETE, &format!("/servers/{server}/members/{user}")),
                        )
                        .await?;
                    }
                    MemberCmd::BanUser {
                        server,
                        user,
                        reason,
                        delete_message_seconds,
                    } => {
                        let body = json!({ "reason": reason, "delete_message_seconds": delete_message_seconds });
                        rest.send::<Value>(
                            rest.req(reqwest::Method::PUT, &format!("/servers/{server}/bans/{user}"))
                                .json(&body),
                        )
                        .await?;
                    }
                    MemberCmd::RemoveRecipient { channel, user } => {
                        rest.send_empty(rest.req(
                            reqwest::Method::DELETE,
                            &format!("/channels/{channel}/recipients/{user}"),
                        ))
                        .await?;
                    }
                    MemberCmd::TransferGroup { channel, user } => {
                        rest.send::<Value>(
                            rest.req(reqwest::Method::PATCH, &format!("/channels/{channel}"))
                                .json(&json!({ "owner": user })),
                        )
                        .await?;
                    }
                }
                Ok(())
            }
            .await;
            let result = result.map_err(|e| crate::errors::describe(&e));
            match admin {
                Some(server) => out.emit(Ev::AdminDone { server, result }),
                None => out.emit(Ev::MemberSaved(result)),
            }
        });
    }
    async fn webhook(&mut self, cmd: WebhookCmd) {
        let Self { rest, config, out, .. } = self;
        match cmd {
            WebhookCmd::FetchWebhooks { channel } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    match rest
                        .send::<Vec<Value>>(rest.req(reqwest::Method::GET, &format!("/channels/{channel}/webhooks")))
                        .await
                    {
                        Ok(list) => out.emit(Ev::Webhooks { channel, list }),
                        Err(e) => out.error("loading webhooks", e),
                    }
                });
            }
            WebhookCmd::Edit(cmd) => {
                let rest = rest.clone();
                let out = out.clone();
                let autumn = config.features.autumn.url.clone();
                tokio::spawn(async move {
                    let mut deleted = None;
                    let result: Result<Value> = async {
                        Ok(match cmd {
                            WebhookEditCmd::CreateWebhook { channel, name } => {
                                rest.send::<Value>(
                                    rest.req(reqwest::Method::POST, &format!("/channels/{channel}/webhooks"))
                                        .json(&json!({ "name": name })),
                                )
                                .await?
                            }
                            WebhookEditCmd::EditWebhook { id, mut body, avatar } => {
                                if let Some(f) = avatar {
                                    body["avatar"] = json!(upload(&rest, &autumn, f, "avatars").await?);
                                }
                                rest.send::<Value>(
                                    rest.req(reqwest::Method::PATCH, &format!("/webhooks/{id}")).json(&body),
                                )
                                .await?
                            }
                            WebhookEditCmd::DeleteWebhook { id } => {
                                rest.send_empty(rest.req(reqwest::Method::DELETE, &format!("/webhooks/{id}")))
                                    .await?;
                                deleted = Some(id);
                                Value::Null
                            }
                        })
                    }
                    .await;
                    out.emit(Ev::WebhookSaved {
                        result: result.map_err(|e| crate::errors::describe(&e)),
                        deleted,
                    });
                });
            }
        }
    }
    async fn channel(&mut self, cmd: ChannelCmd) {
        let Self { rest, config, out, .. } = self;
        match cmd {
            ChannelCmd::CreateChannel {
                server,
                name,
                voice,
                category,
            } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let body = json!({ "type": if voice { "Voice" } else { "Text" }, "name": name });
                    let channel = rest
                        .send::<Value>(
                            rest.req(reqwest::Method::POST, &format!("/servers/{server}/channels"))
                                .json(&body),
                        )
                        .await;
                    out.emit(Ev::ChannelCreated {
                        channel: channel.map_err(|e| crate::errors::describe(&e)),
                        category,
                    });
                });
            }
            ChannelCmd::Edit(cmd) => {
                let rest = rest.clone();
                let out = out.clone();
                let autumn = config.features.autumn.url.clone();
                tokio::spawn(async move {
                    // The change itself arrives as ChannelUpdate.
                    let (channel, request) = match &cmd {
                        ChannelEditCmd::EditChannel { channel, request, .. }
                        | ChannelEditCmd::SetChannelPermissions { channel, request, .. } => (channel.clone(), *request),
                    };
                    let result: Result<Option<crate::api::Channel>> = async {
                        match cmd {
                            ChannelEditCmd::EditChannel {
                                channel,
                                mut body,
                                icon,
                                ..
                            } => {
                                if let Some(f) = icon {
                                    body["icon"] = json!(upload(&rest, &autumn, f, "icons").await?);
                                }
                                let updated = rest
                                    .send::<crate::api::Channel>(
                                        rest.req(reqwest::Method::PATCH, &format!("/channels/{channel}"))
                                            .json(&body),
                                    )
                                    .await?;
                                Ok(Some(updated))
                            }
                            ChannelEditCmd::SetChannelPermissions {
                                channel,
                                role,
                                permissions,
                                ..
                            } => {
                                let target = role.as_deref().unwrap_or("default");
                                let rb = rest
                                    .req(
                                        reqwest::Method::PUT,
                                        &format!("/channels/{channel}/permissions/{target}"),
                                    )
                                    .json(&json!({ "permissions": permissions }));
                                let updated = rest.send::<crate::api::Channel>(rb).await?;
                                Ok(Some(updated))
                            }
                        }
                    }
                    .await;
                    out.emit(Ev::ChannelSaved {
                        channel,
                        request,
                        result: result.map_err(|e| crate::errors::describe(&e)),
                    });
                });
            }
            ChannelCmd::DeleteChannel { channel, request } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let rb = rest.req(reqwest::Method::DELETE, &format!("/channels/{channel}"));
                    let result = rest.send_empty(rb).await.map_err(|e| crate::errors::describe(&e));
                    out.emit(Ev::ChannelDeleted {
                        channel,
                        request,
                        result,
                    });
                });
            }
            ChannelCmd::LeaveChannel { channel, silent } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let rb = rest
                        .req(reqwest::Method::DELETE, &format!("/channels/{channel}"))
                        .query(&[("leave_silently", silent)]);
                    if let Err(e) = rest.send_empty(rb).await {
                        out.error("leaving group", e);
                    }
                });
            }
        }
    }
    async fn role(&mut self, cmd: RoleCmd) {
        let Self { rest, config, out, .. } = self;
        match cmd {
            RoleCmd::CreateRole { server, name } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    #[derive(serde::Deserialize)]
                    struct NewRole {
                        id: String,
                    }
                    let rb = rest
                        .req(reqwest::Method::POST, &format!("/servers/{server}/roles"))
                        .json(&json!({ "name": name }));
                    match rest.send::<NewRole>(rb).await {
                        Ok(r) => out.emit(Ev::RoleCreated { server, id: r.id }),
                        Err(e) => out.emit(Ev::RoleSaved {
                            server,
                            result: Err(crate::errors::describe(&e)),
                        }),
                    }
                });
            }
            RoleCmd::Edit(cmd) => {
                let rest = rest.clone();
                let out = out.clone();
                let autumn = config.features.autumn.url.clone();
                let server = match &cmd {
                    RoleEditCmd::EditRole { server, .. }
                    | RoleEditCmd::SetDefaultPermissions { server, .. }
                    | RoleEditCmd::DeleteRole { server, .. }
                    | RoleEditCmd::SetRoleRanks { server, .. } => server.clone(),
                };
                tokio::spawn(async move {
                    // The changes arrive as ServerRoleUpdate / ServerUpdate / ServerRoleDelete events.
                    let result: Result<()> = async {
                        match cmd {
                            RoleEditCmd::EditRole {
                                server,
                                role,
                                mut body,
                                icon,
                                permissions,
                            } => {
                                if let Some(f) = icon {
                                    body["icon"] = json!(upload(&rest, &autumn, f, "icons").await?);
                                }
                                if body.as_object().is_some_and(|o| !o.is_empty()) {
                                    rest.send::<Value>(
                                        rest.req(reqwest::Method::PATCH, &format!("/servers/{server}/roles/{role}"))
                                            .json(&body),
                                    )
                                    .await?;
                                }
                                if let Some((allow, deny)) = permissions {
                                    let body = json!({ "permissions": { "allow": allow, "deny": deny } });
                                    rest.send::<Value>(
                                        rest.req(
                                            reqwest::Method::PUT,
                                            &format!("/servers/{server}/permissions/{role}"),
                                        )
                                        .json(&body),
                                    )
                                    .await?;
                                }
                            }
                            RoleEditCmd::SetDefaultPermissions { server, permissions } => {
                                let rb = rest
                                    .req(reqwest::Method::PUT, &format!("/servers/{server}/permissions/default"))
                                    .json(&json!({ "permissions": permissions }));
                                rest.send::<Value>(rb).await?;
                            }
                            RoleEditCmd::DeleteRole { server, role } => {
                                rest.send_empty(
                                    rest.req(reqwest::Method::DELETE, &format!("/servers/{server}/roles/{role}")),
                                )
                                .await?;
                            }
                            RoleEditCmd::SetRoleRanks { server, ranks } => {
                                let rb = rest
                                    .req(reqwest::Method::PATCH, &format!("/servers/{server}/roles/ranks"))
                                    .json(&json!({ "ranks": ranks }));
                                rest.send::<Value>(rb).await?;
                            }
                        }
                        Ok(())
                    }
                    .await;
                    out.emit(Ev::RoleSaved {
                        server,
                        result: result.map_err(|e| crate::errors::describe(&e)),
                    });
                });
            }
        }
    }
    async fn emoji(&mut self, cmd: EmojiCmd) {
        let Self { rest, config, out, .. } = self;

        let rest = rest.clone();
        let out = out.clone();
        let autumn = config.features.autumn.url.clone();
        tokio::spawn(async move {
            let result: Result<Value> = async {
                Ok(match cmd {
                    EmojiCmd::CreateEmoji { server, name, file } => {
                        let id = upload(&rest, &autumn, file, "emojis").await?;
                        let body = json!({ "name": name, "parent": { "type": "Server", "id": server } });
                        rest.send::<Value>(
                            rest.req(reqwest::Method::PUT, &format!("/custom/emoji/{id}"))
                                .json(&body),
                        )
                        .await?
                    }
                    EmojiCmd::RenameEmoji { id, name } => {
                        rest.send::<Value>(
                            rest.req(reqwest::Method::PATCH, &format!("/custom/emoji/{id}"))
                                .json(&json!({ "name": name })),
                        )
                        .await?
                    }
                    EmojiCmd::DeleteEmoji { id } => {
                        rest.send_empty(rest.req(reqwest::Method::DELETE, &format!("/custom/emoji/{id}")))
                            .await?;
                        Value::Null
                    }
                })
            }
            .await;
            out.emit(Ev::EmojiSaved(result.map_err(|e| crate::errors::describe(&e))));
        });
    }
    async fn server(&mut self, cmd: ServerCmd) {
        let Self { rest, config, out, .. } = self;
        match cmd {
            ServerCmd::CreateServer { name } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let rb = rest
                        .req(reqwest::Method::POST, "/servers/create")
                        .json(&json!({ "name": name }));
                    match rest.send::<ServerWithChannels>(rb).await {
                        Ok(s) => out.emit(Ev::ServerJoined(s)),
                        Err(e) => out.emit(Ev::FormError(crate::errors::describe(&e))),
                    }
                });
            }
            ServerCmd::CreateGroup { name, users } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let rb = rest
                        .req(reqwest::Method::POST, "/channels/create")
                        .json(&json!({ "name": name, "users": users }));
                    match rest.send::<Channel>(rb).await {
                        Ok(c) => out.emit(Ev::DmOpened(c)),
                        Err(e) => out.emit(Ev::FormError(crate::errors::describe(&e))),
                    }
                });
            }
            ServerCmd::FetchInvite { code } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    match rest
                        .send::<InvitePreview>(rest.req(reqwest::Method::GET, &format!("/invites/{code}")))
                        .await
                    {
                        Ok(preview) => out.emit(Ev::InvitePreview {
                            code,
                            preview: Ok(preview),
                        }),
                        Err(e) => out.emit(Ev::InvitePreview {
                            code,
                            preview: Err(crate::errors::describe(&e)),
                        }),
                    }
                });
            }
            ServerCmd::JoinInvite { code } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    match rest
                        .send::<ServerWithChannels>(rest.req(reqwest::Method::POST, &format!("/invites/{code}")))
                        .await
                    {
                        Ok(server) => out.emit(Ev::InviteJoined {
                            code,
                            result: Ok(server),
                        }),
                        Err(e) => out.emit(Ev::InviteJoined {
                            code,
                            result: Err(crate::errors::describe(&e)),
                        }),
                    }
                });
            }
            ServerCmd::FetchServerInvites { server } => {
                let (rest, out) = (rest.clone(), out.clone());
                tokio::spawn(async move { refetch_invites(&rest, &out, server).await });
            }
            ServerCmd::DeleteInvite { server, code } => {
                let (rest, out) = (rest.clone(), out.clone());
                tokio::spawn(async move {
                    let result = rest
                        .send_empty(rest.req(reqwest::Method::DELETE, &format!("/invites/{code}")))
                        .await
                        .map_err(|e| crate::errors::describe(&e));
                    out.emit(Ev::AdminDone {
                        server: server.clone(),
                        result,
                    });
                    refetch_invites(&rest, &out, server).await;
                });
            }
            ServerCmd::FetchBans { server } => {
                let (rest, out) = (rest.clone(), out.clone());
                tokio::spawn(async move { refetch_bans(&rest, &out, server).await });
            }
            ServerCmd::Unban { server, user } => {
                let (rest, out) = (rest.clone(), out.clone());
                tokio::spawn(async move {
                    let result = rest
                        .send_empty(rest.req(reqwest::Method::DELETE, &format!("/servers/{server}/bans/{user}")))
                        .await
                        .map_err(|e| crate::errors::describe(&e));
                    out.emit(Ev::AdminDone {
                        server: server.clone(),
                        result,
                    });
                    refetch_bans(&rest, &out, server).await;
                });
            }
            ServerCmd::SaveServerOverview {
                server,
                request,
                mut body,
                icon,
                banner,
            } => {
                let (rest, out) = (rest.clone(), out.clone());
                let autumn = config.features.autumn.url.clone();
                tokio::spawn(async move {
                    let result: Result<crate::api::Server> = async {
                        if let Some(file) = icon {
                            body["icon"] = json!(upload(&rest, &autumn, file, "icons").await?);
                        }
                        if let Some(file) = banner {
                            body["banner"] = json!(upload(&rest, &autumn, file, "banners").await?);
                        }
                        rest.send(
                            rest.req(reqwest::Method::PATCH, &format!("/servers/{server}"))
                                .json(&body),
                        )
                        .await
                    }
                    .await;
                    out.emit(Ev::ServerOverviewSaved {
                        server,
                        request,
                        result: result.map_err(|e| crate::errors::describe(&e)),
                    });
                });
            }
            ServerCmd::EditServer {
                server,
                mut body,
                icon,
                banner,
            } => {
                let rest = rest.clone();
                let out = out.clone();
                let autumn = config.features.autumn.url.clone();
                tokio::spawn(async move {
                    let result: Result<()> = async {
                        if let Some(f) = icon {
                            body["icon"] = json!(upload(&rest, &autumn, f, "icons").await?);
                        }
                        if let Some(f) = banner {
                            body["banner"] = json!(upload(&rest, &autumn, f, "banners").await?);
                        }
                        rest.send::<Value>(
                            rest.req(reqwest::Method::PATCH, &format!("/servers/{server}"))
                                .json(&body),
                        )
                        .await?;
                        Ok(())
                    }
                    .await;
                    out.emit(Ev::ServerSaved(result.map_err(|e| crate::errors::describe(&e))));
                });
            }
            ServerCmd::CreateInvite { channel } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let rb = rest.req(reqwest::Method::POST, &format!("/channels/{channel}/invites"));
                    let result = rest
                        .send::<CreatedInvite>(rb)
                        .await
                        .map(|i| i.code)
                        .map_err(|e| crate::errors::describe(&e));
                    out.emit(Ev::Invite(result));
                });
            }
            ServerCmd::DeleteServer { server } => {
                let (rest, out) = (rest.clone(), out.clone());
                tokio::spawn(async move {
                    // The owner's leave deletes it; it disappears via ServerDelete.
                    if let Err(e) = rest
                        .send_empty(rest.req(reqwest::Method::DELETE, &format!("/servers/{server}")))
                        .await
                    {
                        out.emit(Ev::AdminDone {
                            server,
                            result: Err(crate::errors::describe(&e)),
                        });
                    }
                });
            }
            ServerCmd::LeaveServer { server, silent } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    // The server disappears via the ServerMemberLeave/ServerDelete event.
                    let rb = rest
                        .req(reqwest::Method::DELETE, &format!("/servers/{server}"))
                        .query(&[("leave_silently", silent)]);
                    if let Err(e) = rest.send_empty(rb).await {
                        out.error("leaving server", e);
                    }
                });
            }
        }
    }
    async fn bot(&mut self, cmd: BotCmd) {
        let Self { rest, out, .. } = self;
        match cmd {
            BotCmd::FetchPublicBot { id } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    match rest
                        .send::<Value>(rest.req(reqwest::Method::GET, &format!("/bots/{id}/invite")))
                        .await
                    {
                        Ok(v) => {
                            let username = v.get("username").and_then(Value::as_str).unwrap_or_default().to_owned();
                            out.emit(Ev::PublicBot { id, username });
                        }
                        Err(e) => out.error("loading bot", e),
                    }
                });
            }
            BotCmd::FetchDiscover { target } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let status = fetch_discover(&rest, &target).await;
                    out.emit(Ev::DiscoverStatus { target, status });
                });
            }
            BotCmd::SetDiscover { target, request } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let method = if request {
                        reqwest::Method::PUT
                    } else {
                        reqwest::Method::DELETE
                    };
                    let status = match rest.send_empty(rest.req(method, &format!("/{target}/discover"))).await {
                        Ok(()) => fetch_discover(&rest, &target).await,
                        Err(e) => Err(crate::errors::describe(&e)),
                    };
                    out.emit(Ev::DiscoverStatus { target, status });
                });
            }
            BotCmd::InviteBot { id, destination } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let rb = rest
                        .req(reqwest::Method::POST, &format!("/bots/{id}/invite"))
                        .json(&destination);
                    match rest.send_empty(rb).await {
                        Ok(()) => out.emit(Ev::BotNotice("Bot added.".into())),
                        Err(e) => out.emit(Ev::BotNotice(crate::errors::describe(&e))),
                    }
                });
            }
            BotCmd::FetchBots | BotCmd::CreateBot { .. } | BotCmd::EditBot { .. } | BotCmd::DeleteBot { .. } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    // Any change is followed by one refetch of the list.
                    let change: Result<()> = match cmd {
                        BotCmd::CreateBot { name } => {
                            rest.send_empty(
                                rest.req(reqwest::Method::POST, "/bots/create")
                                    .json(&json!({ "name": name })),
                            )
                            .await
                        }
                        BotCmd::EditBot { id, body } => {
                            rest.send_empty(rest.req(reqwest::Method::PATCH, &format!("/bots/{id}")).json(&body))
                                .await
                        }
                        BotCmd::DeleteBot { id } => {
                            rest.send_empty(rest.req(reqwest::Method::DELETE, &format!("/bots/{id}")))
                                .await
                        }
                        _ => Ok(()),
                    };
                    if let Err(e) = change {
                        out.emit(Ev::BotNotice(crate::errors::describe(&e)));
                        return;
                    }
                    match rest.send::<Value>(rest.req(reqwest::Method::GET, "/bots/@me")).await {
                        Ok(v) => out.emit(Ev::Bots(v)),
                        Err(e) => out.emit(Ev::BotNotice(crate::errors::describe(&e))),
                    }
                });
            }
        }
    }
    async fn settings(&mut self, cmd: SettingsCmd) {
        let Self { rest, config, out, .. } = self;
        match cmd {
            SettingsCmd::Report(body) => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let rb = rest.req(reqwest::Method::POST, "/safety/report").json(&body);
                    out.emit(Ev::Reported(
                        rest.send_empty(rb).await.map_err(|e| crate::errors::describe(&e)),
                    ));
                });
            }
            SettingsCmd::FetchEmail => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    match rest
                        .send::<Value>(rest.req(reqwest::Method::GET, "/auth/account/"))
                        .await
                    {
                        Ok(v) => out.emit(Ev::Email(
                            v.get("email").and_then(Value::as_str).unwrap_or_default().to_string(),
                        )),
                        Err(e) => out.error("loading your email", e),
                    }
                });
            }
            SettingsCmd::FetchMfaStatus => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    match rest.send::<Value>(rest.req(reqwest::Method::GET, "/auth/mfa/")).await {
                        Ok(v) => out.emit(Ev::MfaStatus {
                            totp: v.get("totp_mfa").and_then(Value::as_bool).unwrap_or(false),
                            recovery: v.get("recovery_active").and_then(Value::as_bool).unwrap_or(false),
                        }),
                        Err(e) => out.error("loading two-factor status", e),
                    }
                });
            }
            SettingsCmd::MfaAction { action, response } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let result: Result<Value> = async {
                        let rb = rest.req(reqwest::Method::PUT, "/auth/mfa/ticket").json(&response);
                        let ticket = rest.send::<MfaTicket>(rb).await?.token;
                        let (method, path) = match action {
                            MfaAction::TotpSecret => (reqwest::Method::POST, "/auth/mfa/totp"),
                            MfaAction::RemoveTotp => (reqwest::Method::DELETE, "/auth/mfa/totp"),
                            MfaAction::GenerateRecovery => (reqwest::Method::PATCH, "/auth/mfa/recovery"),
                            MfaAction::ViewRecovery => (reqwest::Method::POST, "/auth/mfa/recovery"),
                            MfaAction::DisableAccount => (reqwest::Method::POST, "/auth/account/disable"),
                            MfaAction::DeleteAccount => (reqwest::Method::POST, "/auth/account/delete"),
                        };
                        let res = rest.req(method, path).header("X-MFA-Ticket", ticket).send().await?;
                        let status = res.status();
                        let body = res.text().await.unwrap_or_default();
                        if !status.is_success() {
                            return Err(crate::errors::ApiError::new(status.as_u16(), &body).into());
                        }
                        Ok(serde_json::from_str(&body).unwrap_or(Value::Null))
                    }
                    .await;
                    let result = result.map_err(|e| {
                        if crate::errors::auth_rejected(&e) {
                            "That didn't match. Check it and try again.".to_string()
                        } else {
                            crate::errors::describe(&e)
                        }
                    });
                    out.emit(Ev::MfaActionDone { action, result });
                });
            }
            SettingsCmd::EnableTotp { code } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let rb = rest
                        .req(reqwest::Method::PUT, "/auth/mfa/totp")
                        .json(&json!({ "totp_code": code }));
                    out.emit(Ev::TotpEnabled(rest.send_empty(rb).await.map_err(|e| {
                        if crate::errors::auth_rejected(&e) {
                            "That code didn't match.".to_string()
                        } else {
                            crate::errors::describe(&e)
                        }
                    })));
                });
            }
            SettingsCmd::ChangeAccount { change, totp } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let result: Result<()> = async {
                        let ticket = match totp {
                            Some(code) => {
                                let rb = rest
                                    .req(reqwest::Method::PUT, "/auth/mfa/ticket")
                                    .json(&json!({ "totp_code": code }));
                                Some(rest.send::<MfaTicket>(rb).await?.token)
                            }
                            None => None,
                        };
                        let (path, body) = match change {
                            AccountChange::Username { username, password } => (
                                "/users/@me/username",
                                json!({ "username": username, "password": password }),
                            ),
                            AccountChange::Email {
                                email,
                                current_password,
                            } => (
                                "/auth/account/change/email",
                                json!({ "email": email, "current_password": current_password }),
                            ),
                            AccountChange::Password {
                                password,
                                current_password,
                            } => (
                                "/auth/account/change/password",
                                json!({ "password": password, "current_password": current_password }),
                            ),
                        };
                        let mut rb = rest.req(reqwest::Method::PATCH, path).json(&body);
                        if let Some(t) = ticket {
                            rb = rb.header("X-MFA-Ticket", t);
                        }
                        rest.send_empty(rb).await
                    }
                    .await;
                    // Never echo request bodies; the server's error text is enough.
                    let result = result.map_err(|e| {
                        if crate::errors::auth_rejected(&e) {
                            "That password or code didn't match.".to_string()
                        } else {
                            crate::errors::describe(&e)
                        }
                    });
                    out.emit(Ev::AccountChanged(result));
                });
            }
            SettingsCmd::SaveProfile {
                user,
                mut body,
                avatar,
                banner,
            } => {
                let rest = rest.clone();
                let out = out.clone();
                let autumn = config.features.autumn.url.clone();
                tokio::spawn(async move {
                    let result: Result<()> = async {
                        if let Some(f) = avatar {
                            body["avatar"] = json!(upload(&rest, &autumn, f, "avatars").await?);
                        }
                        if let Some(f) = banner {
                            body["profile"]["background"] = json!(upload(&rest, &autumn, f, "backgrounds").await?);
                        }
                        let path = format!("/users/{}", user.as_deref().unwrap_or("@me"));
                        // Bots get no UserUpdate for this, so apply the returned user.
                        out.emit(Ev::UserSaved(
                            rest.send::<User>(rest.req(reqwest::Method::PATCH, &path).json(&body))
                                .await?,
                        ));
                        Ok(())
                    }
                    .await;
                    out.emit(Ev::ProfileSaved {
                        user,
                        result: result.map_err(|e| crate::errors::describe(&e)),
                    });
                });
            }
            SettingsCmd::EditMe(body) => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    // The change comes back as a UserUpdate event.
                    if let Err(e) = rest
                        .send::<Value>(rest.req(reqwest::Method::PATCH, "/users/@me").json(&body))
                        .await
                    {
                        out.error("updating your status", e);
                    }
                });
            }
            SettingsCmd::SetSyncedSetting { key, value } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let ts = chrono::Utc::now().timestamp_millis();
                    let rb = rest
                        .req(reqwest::Method::POST, "/sync/settings/set")
                        .query(&[("timestamp", ts)])
                        .json(&json!({ key: value }));
                    if let Err(e) = rest.send_empty(rb).await {
                        out.error("saving settings", e);
                    }
                });
            }
            SettingsCmd::FetchSyncedSettings => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let keys = json!({ "keys": ["notifications", "ordering", "server-folders"] });
                    let rb = rest.req(reqwest::Method::POST, "/sync/settings/fetch").json(&keys);
                    match rest.send::<Value>(rb).await {
                        Ok(v) => out.emit(Ev::SyncedSettings(v)),
                        Err(e) => out.error("loading synced settings", e),
                    }
                });
            }
            SettingsCmd::FetchSessions => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    match rest
                        .send::<Vec<SessionInfo>>(rest.req(reqwest::Method::GET, "/auth/session/all"))
                        .await
                    {
                        Ok(s) => out.emit(Ev::Sessions(s)),
                        Err(e) => out.error("loading sessions", e),
                    }
                });
            }
            SettingsCmd::RenameSession { id, name } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let rb = rest
                        .req(reqwest::Method::PATCH, &format!("/auth/session/{id}"))
                        .json(&json!({ "friendly_name": name }));
                    match rest.send::<SessionInfo>(rb).await {
                        Ok(s) => out.emit(Ev::SessionRenamed(s)),
                        Err(e) => out.error("renaming session", e),
                    }
                });
            }
            SettingsCmd::FetchMfaMethods => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    match rest
                        .send::<Vec<String>>(rest.req(reqwest::Method::GET, "/auth/mfa/methods"))
                        .await
                    {
                        Ok(m) => out.emit(Ev::MfaMethods(m)),
                        Err(e) => out.emit(Ev::MfaFailed(crate::errors::describe(&e))),
                    }
                });
            }
            SettingsCmd::RevokeSessions { target, response } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    // Logging out sessions needs a validated MFA ticket, as in the official client.
                    let rb = rest.req(reqwest::Method::PUT, "/auth/mfa/ticket").json(&response);
                    let ticket = match rest.send::<MfaTicket>(rb).await {
                        Ok(t) => t,
                        Err(e) => {
                            let msg = if crate::errors::auth_rejected(&e) {
                                "That didn't match. Check it and try again.".into()
                            } else {
                                crate::errors::describe(&e)
                            };
                            out.emit(Ev::MfaFailed(msg));
                            return;
                        }
                    };
                    let rb = match &target {
                        RevokeTarget::One(id) => rest.req(reqwest::Method::DELETE, &format!("/auth/session/{id}")),
                        RevokeTarget::Others => rest
                            .req(reqwest::Method::DELETE, "/auth/session/all")
                            .query(&[("revoke_self", "false")]),
                    };
                    if let Err(e) = rest.send_empty(rb.header("X-MFA-Ticket", ticket.token)).await {
                        out.emit(Ev::MfaFailed(crate::errors::describe(&e)));
                        return;
                    }
                    // One refresh so the list shows what's left.
                    match rest
                        .send::<Vec<SessionInfo>>(rest.req(reqwest::Method::GET, "/auth/session/all"))
                        .await
                    {
                        Ok(s) => out.emit(Ev::Sessions(s)),
                        Err(e) => out.error("loading sessions", e),
                    }
                });
            }
        }
    }
    async fn media(&mut self, cmd: MediaCmd) {
        let Self { rest, out, .. } = self;
        match cmd {
            MediaCmd::GifCategories => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    let rb = rest.absolute(reqwest::Method::GET, &format!("{GIFBOX}/categories?locale=en_US"));
                    let result = rest.send::<Vec<GifCategory>>(rb).await;
                    out.emit(Ev::GifCategories(result.map_err(|e| crate::errors::describe(&e))));
                });
            }
            MediaCmd::SearchGifs { query, position } => {
                let rest = rest.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    // As the official picker; gifbox allows few requests (5
                    // trending, 10 searches per window), so the UI waits for
                    // typing to pause and nothing here retries.
                    let mut url = if query == godwit_core::gifs::TRENDING {
                        format!("{GIFBOX}/trending?locale=en_US")
                    } else {
                        format!("{GIFBOX}/search?locale=en_US&query={}", encode_query(&query))
                    };
                    if let Some(p) = &position {
                        url.push_str(&format!("&position={}", encode_query(p)));
                    }
                    let result = rest.send::<GifPage>(rest.absolute(reqwest::Method::GET, &url)).await;
                    out.emit(Ev::Gifs {
                        query,
                        more: position.is_some(),
                        page: result.map_err(|e| crate::errors::describe(&e)),
                    });
                });
            }
            MediaCmd::FetchChangelog { auto } => {
                let http = rest.http.clone();
                let out = out.clone();
                tokio::spawn(async move {
                    // Public and unauthenticated; quietly nothing on failure, as officially.
                    let rb = http
                        .get(crate::api::CHANGELOG_URL)
                        .header(reqwest::header::ACCEPT, "application/json");
                    let result = async {
                        rb.send()
                            .await?
                            .error_for_status()?
                            .json::<crate::api::Changelog>()
                            .await
                    }
                    .await;
                    match result {
                        Ok(changelog) => out.emit(Ev::Changelog { changelog, auto }),
                        Err(e) => godwit_core::verbose!("error: fetching the changelog: {e}"),
                    }
                });
            }
        }
    }
}

/// The auto-delete queue: one job at a time, one request at a time, spaced
/// by the pacer (the job's gap, the rate-limit headers, 429s, back-off on
/// network trouble). New jobs join the queue (skipping ones already waiting
/// for the same thing); a cancel drops everything.
async fn autodelete_queue(mut rx: tmpsc::UnboundedReceiver<AutoDeleteMsg>, out: Emitter) {
    use std::collections::VecDeque;
    let mut queue: VecDeque<(crate::autodelete::Job, Duration)> = VecDeque::new();
    let mut rest: Option<Rest> = None;
    // When the next request may go: shared by all jobs, so the gap and any
    // rate-limit wait carry over from one to the next.
    let mut next = tokio::time::Instant::now();
    loop {
        // Idle: wait for work.
        if queue.is_empty() {
            match rx.recv().await {
                Some(AutoDeleteMsg::Jobs(r, jobs, gap)) => {
                    rest = Some(r);
                    autodelete_add(&mut queue, jobs, gap);
                }
                Some(AutoDeleteMsg::Cancel) => {
                    out.emit(Ev::AutoDelete(AutoDeleteEv::Idle));
                    continue;
                }
                None => return,
            }
        }
        let (Some((job, gap)), Some(r)) = (queue.pop_front(), rest.clone()) else {
            continue;
        };
        // A job's failures don't count against the next one.
        let mut pacer = crate::autodelete::Pacer::new(gap);
        let mut ctl = Control {
            rx: &mut rx,
            queue: &mut queue,
            rest: &mut rest,
            cancelled: false,
        };
        let result = autodelete_job(&r, &job, &mut pacer, &mut next, &mut ctl, &out).await;
        if ctl.cancelled {
            queue.clear();
        } else if let Err(error) = result {
            let (rule, channel) = match &job {
                crate::autodelete::Job::Index { channel, .. } => (None, channel.clone()),
                crate::autodelete::Job::Delete { rule, channel, .. }
                | crate::autodelete::Job::Purge { rule, channel, .. } => (Some(*rule), channel.clone()),
            };
            out.emit(Ev::AutoDelete(AutoDeleteEv::Failed {
                rule,
                channel,
                error: error.to_string(),
            }));
        }
        if queue.is_empty() {
            out.emit(Ev::AutoDelete(AutoDeleteEv::Idle));
        }
    }
}

/// Queues jobs (deletions at `gap`, indexing at the slower indexing gap),
/// skipping ones the queue already has.
fn autodelete_add(
    queue: &mut std::collections::VecDeque<(crate::autodelete::Job, Duration)>,
    jobs: Vec<crate::autodelete::Job>,
    gap: Duration,
) {
    for job in jobs {
        let gap = match job {
            crate::autodelete::Job::Index { .. } => crate::autodelete::INDEX_GAP.max(gap),
            crate::autodelete::Job::Delete { .. } | crate::autodelete::Job::Purge { .. } => gap,
        };
        if !queue.iter().any(|(q, _)| *q == job) {
            queue.push_back((job, gap));
        }
    }
}

/// What a running auto-delete job listens to while it waits.
struct Control<'a> {
    rx: &'a mut tmpsc::UnboundedReceiver<AutoDeleteMsg>,
    queue: &'a mut std::collections::VecDeque<(crate::autodelete::Job, Duration)>,
    rest: &'a mut Option<Rest>,
    cancelled: bool,
}

impl Control<'_> {
    /// Waits `d`, taking in new jobs meanwhile; false if cancelled.
    async fn wait(&mut self, d: Duration) -> bool {
        let until = tokio::time::Instant::now() + d;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(until) => return !self.cancelled,
                msg = self.rx.recv() => match msg {
                    Some(AutoDeleteMsg::Jobs(r, jobs, gap)) => {
                        *self.rest = Some(r);
                        autodelete_add(self.queue, jobs, gap);
                    }
                    Some(AutoDeleteMsg::Cancel) | None => {
                        self.cancelled = true;
                        return false;
                    }
                },
            }
        }
    }
}

/// Auto-delete keeps status and causes structured while requests and jobs
/// propagate failures; only its UI event needs text.
#[derive(Debug)]
enum AutoDeleteError {
    Network(reqwest::Error),
    RateLimited,
    Http { operation: &'static str, status: u16 },
    Decode(serde_json::Error),
}
impl std::fmt::Display for AutoDeleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(e) => write!(f, "network: {e}"),
            Self::RateLimited => f.write_str("still rate limited after several tries"),
            Self::Http { operation, status } => write!(f, "{operation}: HTTP {status}"),
            Self::Decode(e) => write!(f, "reading messages: {e}"),
        }
    }
}
impl std::error::Error for AutoDeleteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Network(e) => Some(e),
            Self::Decode(e) => Some(e),
            Self::RateLimited | Self::Http { .. } => None,
        }
    }
}

/// Sends a request through the pacer: waits for its turn, retries on 429 and
/// on network trouble (with back-off). Ok(None) if cancelled while waiting.
async fn autodelete_request(
    make: &(dyn Fn() -> reqwest::RequestBuilder + Sync),
    pacer: &mut crate::autodelete::Pacer,
    next: &mut tokio::time::Instant,
    ctl: &mut Control<'_>,
) -> std::result::Result<Option<(u16, String)>, AutoDeleteError> {
    use crate::autodelete::Verdict;
    loop {
        let now = tokio::time::Instant::now();
        if *next > now && !ctl.wait(*next - now).await {
            return Ok(None);
        }
        match make().send().await {
            Err(e) => {
                let d = pacer.failed();
                if pacer.exhausted() {
                    return Err(AutoDeleteError::Network(e));
                }
                *next = tokio::time::Instant::now() + d;
            }
            Ok(res) => {
                let status = res.status().as_u16();
                let header = |name: &str| {
                    res.headers()
                        .get(name)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<f64>().ok())
                };
                let remaining = header("x-ratelimit-remaining").map(|v| v as u32);
                let reset_after = header("x-ratelimit-reset-after").map(|v| v as u64);
                let body = res.text().await.unwrap_or_default();
                let retry_after = serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|v| v.get("retry_after").and_then(Value::as_f64))
                    .map(|v| v as u64);
                match pacer.answered(status, remaining, reset_after, retry_after) {
                    Verdict::Retry(d) => {
                        if pacer.exhausted() {
                            return Err(AutoDeleteError::RateLimited);
                        }
                        *next = tokio::time::Instant::now() + d;
                    }
                    Verdict::Next(d) => {
                        *next = tokio::time::Instant::now() + d;
                        return Ok(Some((status, body)));
                    }
                }
            }
        }
    }
}

/// One job. Indexing reads up to `pages` pages of 100, oldest first, and
/// reports each (the range it proves and our messages in it); deleting
/// deletes and reports message by message. Either way an interrupted job
/// loses nothing: what's reported is saved.
async fn autodelete_job(
    rest: &Rest,
    job: &crate::autodelete::Job,
    pacer: &mut crate::autodelete::Pacer,
    next: &mut tokio::time::Instant,
    ctl: &mut Control<'_>,
    out: &Emitter,
) -> std::result::Result<(), AutoDeleteError> {
    use crate::autodelete::{Job, PAGE, is_ours, kind_of};
    match job {
        Job::Index {
            channel,
            after,
            before,
            pages,
            me,
        } => {
            let mut after = after.clone();
            for _ in 0..*pages {
                let mut query = vec![
                    ("limit", PAGE.to_string()),
                    ("sort", "Oldest".into()),
                    ("before", before.clone()),
                ];
                if let Some(a) = &after {
                    query.push(("after", a.clone()));
                }
                let path = format!("/channels/{channel}/messages");
                let make = || rest.req(reqwest::Method::GET, &path).query(&query);
                let Some((status, body)) = autodelete_request(&make, pacer, next, ctl).await? else {
                    return Ok(());
                };
                if !(200..300).contains(&status) {
                    return Err(AutoDeleteError::Http {
                        operation: "reading messages",
                        status,
                    });
                }
                let page: Vec<Message> = serde_json::from_str(&body).map_err(AutoDeleteError::Decode)?;
                let ours = page
                    .iter()
                    .filter(|m| is_ours(m, me))
                    .map(|m| (m.id.clone(), kind_of(m).letter()))
                    .collect();
                let last = page.last().map(|m| m.id.clone());
                // A short page reaches the end of the range.
                let finished = page.len() < PAGE || last.is_none();
                let hi = if finished {
                    before.clone()
                } else {
                    last.clone().unwrap_or_default()
                };
                let lo = after.clone().unwrap_or_default();
                out.emit(Ev::AutoDelete(AutoDeleteEv::Indexed {
                    channel: channel.clone(),
                    lo,
                    hi,
                    ours,
                }));
                if finished {
                    break;
                }
                after = last;
            }
            Ok(())
        }
        // Our own messages only: each is fetched and checked to be ours before
        // it's deleted, whatever the index says (moderators could delete others').
        Job::Delete { rule, channel, ids, me } => {
            for id in ids {
                let path = format!("/channels/{channel}/messages/{id}");
                let make = || rest.req(reqwest::Method::GET, &path);
                let Some((status, body)) = autodelete_request(&make, pacer, next, ctl).await? else {
                    return Ok(());
                };
                match status {
                    200..=299 => {}
                    // Already gone.
                    404 => {
                        out.emit(Ev::AutoDelete(AutoDeleteEv::Deleted {
                            rule: *rule,
                            channel: channel.clone(),
                            id: id.clone(),
                        }));
                        continue;
                    }
                    _ => {
                        return Err(AutoDeleteError::Http {
                            operation: "checking a message",
                            status,
                        });
                    }
                }
                let ours =
                    serde_json::from_str::<Message>(&body).is_ok_and(|m| m.channel == *channel && is_ours(&m, me));
                if !ours {
                    out.emit(Ev::AutoDelete(AutoDeleteEv::NotOurs {
                        channel: channel.clone(),
                        id: id.clone(),
                    }));
                    continue;
                }
                if !autodelete_delete(rest, channel, id, pacer, next, ctl).await? {
                    return Ok(());
                }
                out.emit(Ev::AutoDelete(AutoDeleteEv::Deleted {
                    rule: *rule,
                    channel: channel.clone(),
                    id: id.clone(),
                }));
            }
            Ok(())
        }
        // A moderator purge: everyone's messages of the rule's kinds, swept page by page.
        Job::Purge {
            rule,
            channel,
            after,
            before,
            kinds,
            keep_pinned,
            pages,
        } => {
            let mut after = after.clone();
            for _ in 0..*pages {
                let mut query = vec![
                    ("limit", PAGE.to_string()),
                    ("sort", "Oldest".into()),
                    ("before", before.clone()),
                ];
                if let Some(a) = &after {
                    query.push(("after", a.clone()));
                }
                let path = format!("/channels/{channel}/messages");
                let make = || rest.req(reqwest::Method::GET, &path).query(&query);
                let Some((status, body)) = autodelete_request(&make, pacer, next, ctl).await? else {
                    return Ok(());
                };
                if !(200..300).contains(&status) {
                    return Err(AutoDeleteError::Http {
                        operation: "reading messages",
                        status,
                    });
                }
                let page: Vec<Message> = serde_json::from_str(&body).map_err(AutoDeleteError::Decode)?;
                for m in page
                    .iter()
                    .filter(|m| kinds.covers(kind_of(m)) && !(*keep_pinned && m.pinned == Some(true)))
                {
                    if !autodelete_delete(rest, channel, &m.id, pacer, next, ctl).await? {
                        return Ok(());
                    }
                    out.emit(Ev::AutoDelete(AutoDeleteEv::Deleted {
                        rule: *rule,
                        channel: channel.clone(),
                        id: m.id.clone(),
                    }));
                }
                let last = page.last().map(|m| m.id.clone());
                let finished = page.len() < PAGE || last.is_none();
                let hi = if finished {
                    before.clone()
                } else {
                    last.clone().unwrap_or_default()
                };
                out.emit(Ev::AutoDelete(AutoDeleteEv::Swept {
                    rule: *rule,
                    channel: channel.clone(),
                    hi,
                }));
                if finished {
                    break;
                }
                after = last;
            }
            Ok(())
        }
    }
}

/// Deletes one message through the pacer. False if cancelled; a message
/// already gone counts as done.
async fn autodelete_delete(
    rest: &Rest,
    channel: &str,
    id: &str,
    pacer: &mut crate::autodelete::Pacer,
    next: &mut tokio::time::Instant,
    ctl: &mut Control<'_>,
) -> std::result::Result<bool, AutoDeleteError> {
    let path = format!("/channels/{channel}/messages/{id}");
    let make = || rest.req(reqwest::Method::DELETE, &path);
    let Some((status, _)) = autodelete_request(&make, pacer, next, ctl).await? else {
        return Ok(false);
    };
    match status {
        200..=299 | 404 => Ok(true),
        _ => Err(AutoDeleteError::Http {
            operation: "deleting a message",
            status,
        }),
    }
}

async fn refetch_invites(rest: &Rest, out: &Emitter, server: String) {
    match rest
        .send::<Vec<Value>>(rest.req(reqwest::Method::GET, &format!("/servers/{server}/invites")))
        .await
    {
        Ok(invites) => out.emit(Ev::ServerInvites { server, invites }),
        Err(e) => out.error("loading invites", e),
    }
}

async fn refetch_bans(rest: &Rest, out: &Emitter, server: String) {
    match rest
        .send::<Value>(rest.req(reqwest::Method::GET, &format!("/servers/{server}/bans")))
        .await
    {
        Ok(data) => out.emit(Ev::Bans { server, data }),
        Err(e) => out.error("loading bans", e),
    }
}

/// Uploads to the file server under `tag` ("attachments", "avatars", "backgrounds", …).
/// A discover request; a 404 means there is none yet (the official
/// Discoverable treats NotFound as "you can request").
async fn fetch_discover(rest: &Rest, target: &str) -> Result<Option<Value>, String> {
    let res = rest
        .req(reqwest::Method::GET, &format!("/{target}/discover"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    match res.status() {
        reqwest::StatusCode::NOT_FOUND => Ok(None),
        s if s.is_success() => res.json::<Value>().await.map(Some).map_err(|e| e.to_string()),
        s => Err(format!("HTTP {s}")),
    }
}

async fn upload(rest: &Rest, autumn: &str, f: Upload, tag: &str) -> Result<String> {
    let data = match f.source {
        UploadSource::Bytes(b) => b.as_ref().clone(),
        UploadSource::Path(p) => tokio::fs::read(&p)
            .await
            .with_context(|| format!("reading {}", p.display()))?,
    };
    let part = reqwest::multipart::Part::bytes(data).file_name(f.name);
    let form = reqwest::multipart::Form::new().part("file", part);
    let mut rb = rest.http.post(format!("{autumn}/{tag}")).multipart(form);
    if let Some(t) = &rest.token {
        rb = rb.header("X-Session-Token", t);
    }
    #[derive(serde::Deserialize)]
    struct Uploaded {
        id: String,
    }
    Ok(rest.send::<Uploaded>(rb).await?.id)
}

async fn handle_login(rest: &Rest, rb: reqwest::RequestBuilder, out: &Emitter) {
    match rest.send::<LoginResponse>(rb).await {
        Ok(LoginResponse::Success(session)) => out.emit(Ev::LoggedIn(session)),
        Ok(LoginResponse::Mfa {
            ticket,
            allowed_methods,
        }) => {
            if allowed_methods.iter().any(|m| m == "Totp") || allowed_methods.is_empty() {
                out.emit(Ev::MfaRequired { ticket });
            } else {
                out.emit(Ev::Error(format!(
                    "MFA method not supported yet (server allows: {})",
                    allowed_methods.join(", ")
                )));
            }
        }
        Ok(LoginResponse::Disabled) => out.emit(Ev::Error("account is disabled".into())),
        Err(e) => out.error("logging in", e),
    }
}

/// Connects, forwards events, and reconnects with backoff until aborted.
async fn websocket_loop(url: String, token: String, mut outgoing: tmpsc::UnboundedReceiver<Value>, out: Emitter) {
    let mut failures: u32 = 0;
    loop {
        out.emit(Ev::Connecting);
        // A panic in here would otherwise kill the task silently and leave the UI spinning.
        let attempt = std::panic::AssertUnwindSafe(websocket_once(&url, &token, &mut outgoing, &out));
        match futures_util::FutureExt::catch_unwind(attempt).await {
            Ok(Ok(())) => failures = 0,
            Ok(Err(e)) => {
                failures += 1;
                out.error("websocket", e);
            }
            Err(_) => {
                failures += 1;
                out.emit(Ev::Error(
                    "websocket: connection task panicked (see terminal output)".into(),
                ));
            }
        }
        // Back off 5s, 10s, 20s … up to 2 minutes so a broken connection doesn't hammer the server.
        let delay = if failures == 0 {
            2
        } else {
            (5u64 << (failures - 1).min(5)).min(120)
        };
        tokio::time::sleep(Duration::from_secs(delay)).await;
    }
}

async fn websocket_once(
    url: &str,
    token: &str,
    outgoing: &mut tmpsc::UnboundedReceiver<Value>,
    out: &Emitter,
) -> Result<()> {
    // Authenticate with a message rather than a URL parameter so the token
    // can never end up in an error message or log line.
    // The configured URL has no path ("wss://events.stoat.chat"); without the
    // slash the handshake's request line becomes "GET ?version=…", a 400.
    let full = format!("{}/?version=1&format=json", url.trim_end_matches('/'));
    let (ws, _) = tokio_tungstenite::connect_async(full.as_str())
        .await
        .context("connecting")?;
    let (mut sink, mut stream) = ws.split();
    let auth = json!({ "type": "Authenticate", "token": token }).to_string();
    sink.send(WsMessage::Text(auth.into())).await?;
    let mut ping = tokio::time::interval(Duration::from_secs(10));

    loop {
        tokio::select! {
            _ = ping.tick() => {
                let msg = json!({ "type": "Ping", "data": 0 }).to_string();
                sink.send(WsMessage::Text(msg.into())).await?;
            }
            Some(v) = outgoing.recv() => {
                sink.send(WsMessage::Text(v.to_string().into())).await?;
            }
            msg = stream.next() => {
                let Some(msg) = msg else { return Ok(()) };
                match msg? {
                    WsMessage::Text(text) => {
                        let v: Value = serde_json::from_str(&text)?;
                        dispatch(v, out)?;
                    }
                    WsMessage::Close(frame) => bail!("closed by server: {frame:?}"),
                    _ => {}
                }
            }
        }
    }
}

fn dispatch(v: Value, out: &Emitter) -> Result<()> {
    match v.get("type").and_then(Value::as_str) {
        Some("Bulk") => {
            if let Some(Value::Array(items)) = v.get("v") {
                for item in items {
                    dispatch(item.clone(), out)?;
                }
            }
        }
        Some("Ready") => out.emit(Ev::Ready(serde_json::from_value(v).context("parsing Ready")?)),
        Some("Error") => bail!("server error: {}", v.get("error").unwrap_or(&Value::Null)),
        Some("Pong" | "Authenticated") => {}
        _ => out.emit(Ev::Event(v)),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auto_delete_failures_keep_status_and_existing_presentation() {
        let error = AutoDeleteError::Http {
            operation: "checking a message",
            status: 403,
        };
        assert_eq!(error.to_string(), "checking a message: HTTP 403");
        assert!(matches!(error, AutoDeleteError::Http { status: 403, .. }));
        assert_eq!(
            AutoDeleteError::RateLimited.to_string(),
            "still rate limited after several tries"
        );
        if let Err(cause) = serde_json::from_str::<Value>("{") {
            let expected = format!("reading messages: {cause}");
            let error = AutoDeleteError::Decode(cause);
            assert_eq!(error.to_string(), expected);
            assert!(std::error::Error::source(&error).is_some());
        } else {
            panic!("invalid JSON must fail");
        }
    }
}
