// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors

use super::*;

/// Something a network backend reports.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// An account's synced look-and-feel choices, as far as its network said them.
    Preferences {
        account: AccountId,
        prefs: crate::preferences::SyncedPreferences,
    },
    /// An account's privacy switches, as its network says them now.
    Privacy {
        account: AccountId,
        view: crate::privacy::PrivacyView,
    },
    /// An account's signed-in devices, as its network lists them now.
    Sessions {
        account: AccountId,
        view: crate::privacy::SessionsView,
    },
    /// A session revocation is being confirmed; keep the current list visible.
    SessionActionPending {
        account: AccountId,
    },
    /// Signing out a session failed; keep the list visible so it can be retried.
    SessionActionFailed {
        account: AccountId,
        error: Arc<str>,
    },
    AccountSecurity {
        account: AccountId,
        view: crate::account_security::SecurityView,
    },
    /// An account's auto-delete page, after any change.
    AutoDelete {
        account: AccountId,
        view: crate::autodelete::AutoDeleteView,
    },
    /// How a report went: the network took it, or why not.
    Reported {
        account: AccountId,
        result: Result<(), Arc<str>>,
    },
    ServerAdmin {
        server: ServerId,
        result: Result<crate::server_settings::ServerAdminResult, Arc<str>>,
    },
    /// The network holds a request back until the person confirms with one
    /// of `methods`; answered by [`Action::StepUpAnswer`] or
    /// [`Action::StepUpCancel`].
    StepUpRequired {
        account: AccountId,
        id: u64,
        methods: Vec<crate::step_up::StepUpMethod>,
    },
    /// How an answer to prompt `id` went: taken (the prompt closes and the
    /// request goes again), or why not (the prompt stays for another try).
    StepUpFinished {
        account: AccountId,
        id: u64,
        result: Result<(), Arc<str>>,
    },
    /// The server's scheduled events, or a load/update error.
    ServerEvents {
        server: ServerId,
        result: Result<Vec<crate::server::ServerEvent>, Arc<str>>,
        /// True for the response to an explicit page load, false for gateway updates.
        load_response: bool,
    },
    ModerationLog {
        server: ServerId,
        before: Option<Arc<str>>,
        result: Result<Vec<crate::server_settings::ModerationLogEntry>, Arc<str>>,
    },
    /// A channel's settings: loaded, saved or failed. `Ok(None)` means the
    /// channel was deleted.
    ChannelOverview {
        room: RoomId,
        request: u64,
        result: Result<Option<crate::channel_settings::ChannelOverview>, Arc<str>>,
    },
    /// A channel's overrides changed elsewhere (another admin, another
    /// client): the open page shows them.
    ChannelOverrides {
        room: RoomId,
        overrides: crate::channel_settings::ChannelOverrides,
    },
    Webhooks {
        room: RoomId,
        result: Result<Vec<crate::webhooks::Webhook>, Arc<str>>,
    },
    /// A webhook's URL to copy; it holds the webhook's secret.
    WebhookUrl {
        room: RoomId,
        result: Result<crate::webhooks::WebhookUrl, Arc<str>>,
    },
    WebhookChanged {
        room: RoomId,
        result: Result<Vec<crate::webhooks::Webhook>, Arc<str>>,
    },
    ServerOverview {
        server: ServerId,
        request: u64,
        result: Result<crate::server_settings::ServerOverview, Arc<str>>,
    },
    AccountIdentity {
        account: AccountId,
        display_name: Arc<str>,
        username: Arc<str>,
        avatar_url: Option<Arc<str>>,
    },
    OwnProfileLoaded {
        account: AccountId,
        result: Result<OwnProfile, Arc<str>>,
    },
    OwnProfileSaved {
        account: AccountId,
        result: Result<OwnProfile, Arc<str>>,
    },
    ServerProfileLoaded {
        server: ServerId,
        result: Result<OwnProfile, Arc<str>>,
    },
    ServerProfileSaved {
        server: ServerId,
        result: Result<OwnProfile, Arc<str>>,
    },
    /// Signed in and synced: the account's rooms and servers, in its order.
    AccountReady {
        account: AccountId,
        display_name: Arc<str>,
        /// The account's own name on its network (e.g. `name#1234`).
        username: Arc<str>,
        avatar_url: Option<Arc<str>>,
        rooms: Vec<Room>,
        servers: Vec<Server>,
    },
    /// The account is gone: `reason` says why ("" when the user logged out).
    AccountLost {
        account: AccountId,
        reason: Arc<str>,
    },
    /// New keys arrived (the device was verified): messages already shown
    /// may decrypt now, so loaded timelines are read again.
    KeysArrived {
        account: AccountId,
    },
    /// A server's directory: rooms we could join (ones we're in may be listed too).
    Directory {
        server: ServerId,
        rooms: Vec<DirectoryRoom>,
    },
    /// How verifying with another device is going.
    Verification {
        account: AccountId,
        step: Verify,
    },
    /// Our own presence on an account (set here or elsewhere).
    OwnPresence {
        account: AccountId,
        presence: Presence,
        /// Our custom status text, if set.
        status: Option<Arc<str>>,
    },
    /// The account's connection dropped (or came back); see `Account::offline`.
    Connection {
        account: AccountId,
        online: bool,
    },
    /// Whether this device still needs verifying (see `Account::unverified`).
    DeviceTrust {
        account: AccountId,
        unverified: bool,
    },
    /// A room appeared or changed.
    RoomUpdated(Room),
    RoomRemoved(RoomId),
    /// The account's servers, all of them, in its order.
    ServersChanged {
        account: AccountId,
        servers: Vec<Server>,
    },
    /// A newly created server is present in the account's server list; open it.
    ServerCreated(ServerId),
    /// A page of history, oldest first. `before` is what the page was asked
    /// to end before; `None` is the newest page, which replaces the timeline.
    History {
        room: RoomId,
        before: Option<MessageId>,
        messages: Vec<Message>,
        reached_start: bool,
    },
    /// The messages around `around` (oldest first): they replace the
    /// timeline, which is then away from the newest messages.
    HistoryAround {
        room: RoomId,
        around: MessageId,
        messages: Vec<Message>,
    },
    /// Loading history failed; scrolling up tries again.
    HistoryFailed {
        room: RoomId,
    },
    MessageAdded {
        room: RoomId,
        message: Message,
    },
    MessageUpdated {
        room: RoomId,
        message: Message,
    },
    /// Only a message's link previews changed: they're made after sending
    /// and can arrive without the rest of the message.
    EmbedsUpdated {
        room: RoomId,
        id: MessageId,
        embeds: Vec<crate::message::Embed>,
    },
    MessageDeleted {
        room: RoomId,
        id: MessageId,
    },
    /// A message's reactions, all of them (networks that send reactions
    /// on their own, like Matrix).
    ReactionsChanged {
        room: RoomId,
        id: MessageId,
        reactions: Vec<Reaction>,
    },
    /// People who used one reaction, keyed so stale answers cannot replace a newer popup.
    Reactors {
        room: RoomId,
        message: MessageId,
        emoji: Arc<str>,
        people: Vec<Person>,
    },
    /// New text for a message (networks that send only the edit, like
    /// Matrix); everything else about it stays.
    MessageEdited {
        room: RoomId,
        id: MessageId,
        text: Arc<str>,
        markup: Markup,
        source: Arc<str>,
    },
    /// The server accepted what we sent as `nonce`.
    MessageConfirmed {
        room: RoomId,
        nonce: Arc<str>,
        message: Message,
    },
    MessageFailed {
        room: RoomId,
        nonce: Arc<str>,
        error: Arc<str>,
    },
    /// Some people's presence on an account (online, away, …), for
    /// networks that report it apart from member lists (Slack, Google
    /// Chat): People, member lists and direct chats show it.
    Presences {
        account: AccountId,
        presence: Vec<(UserId, Presence)>,
    },
    /// What the account's friends are doing now ("Playing …", "Listening
    /// to …"): everyone the account knows to be doing something. Replaces
    /// the account's earlier list; Active now shows it.
    Activities {
        account: AccountId,
        activities: Vec<(UserId, Arc<str>)>,
    },
    /// A voice channel's session began at `since_ms` (Unix ms), or ended (None).
    VoiceSince {
        room: RoomId,
        since_ms: Option<i64>,
    },
    /// The account's company or workspace directory (Slack's member list):
    /// listed in People besides those we have direct chats with.
    PeopleDirectory {
        account: AccountId,
        people: Vec<Person>,
    },
    /// People's names and pictures became known: messages already loaded
    /// show them (instead of a placeholder).
    Authors {
        account: AccountId,
        people: Vec<Person>,
    },
    /// The account's friends, all of them (sorted as the network does).
    Friends {
        account: AccountId,
        people: Vec<Person>,
    },
    /// The people the account has blocked, all of them.
    Blocked {
        account: AccountId,
        users: Vec<UserId>,
    },
    /// The people the account ignores, all of them.
    Ignored {
        account: AccountId,
        users: Vec<UserId>,
    },
    /// The people the account's network keeps as favourites, all of them.
    Favourites {
        account: AccountId,
        users: Vec<UserId>,
    },
    /// Network-owned nicknames for this account, as a complete snapshot.
    Nicknames {
        account: AccountId,
        nicknames: Vec<(UserId, String)>,
    },
    /// The account's pending friend requests, both ways.
    FriendRequests {
        account: AccountId,
        incoming: Vec<Person>,
        outgoing: Vec<Person>,
    },
    /// What an invite leads to; None if it isn't valid (any more).
    InviteLoaded {
        account: AccountId,
        code: Arc<str>,
        preview: Option<crate::message::InvitePreview>,
    },
    /// The GIF service's categories.
    GifCategories {
        account: AccountId,
        categories: Vec<crate::gifs::GifCategory>,
    },
    /// A page of GIFs for `query`; `more` when it continues the last page.
    Gifs {
        account: AccountId,
        query: String,
        more: bool,
        results: Vec<crate::gifs::Gif>,
        next: Option<String>,
    },
    /// Loading GIFs for `query` failed.
    GifsFailed {
        account: AccountId,
        query: String,
        error: Arc<str>,
    },
    /// Joined through an invite: its server opens, as officially.
    InviteJoined {
        account: AccountId,
        code: Arc<str>,
        server: ServerId,
    },
    /// How a [`Command::SendFriendRequest`] went.
    FriendRequestSent {
        account: AccountId,
        request: u64,
        result: Result<(), Arc<str>>,
    },
    FriendRequestFormError(Arc<str>),
    /// A direct chat we asked for exists now: open it.
    RoomOpened(RoomId),
    /// Worth telling the user outside the window: a desktop notification and
    /// a sound, as the app's settings allow. Networks send it only for what
    /// their own client would notify about (mutes and levels applied).
    Alert {
        room: RoomId,
        kind: AlertKind,
        /// Who it's from, when a person: muzzle mode goes by them.
        from: Option<UserId>,
        title: Arc<str>,
        body: Arc<str>,
    },
    /// A call that was ringing (an [`AlertKind::Call`]) stopped.
    AlertDone {
        room: RoomId,
    },
    /// Our call in `room` is up.
    CallConnected {
        room: RoomId,
    },
    /// Latest voice transport round trip; `None` when the network can't measure it.
    CallLatency {
        room: RoomId,
        milliseconds: Option<u16>,
    },
    CallVideos {
        room: RoomId,
        videos: Vec<crate::call::CallVideo>,
    },
    /// Who shares their screen in our call in `room`, to watch.
    CallStreams {
        room: RoomId,
        streams: Vec<crate::call::CallStream>,
    },
    CallCamera {
        room: RoomId,
        active: bool,
    },
    CallScreenShare {
        room: RoomId,
        active: bool,
    },
    /// Our call in `room` ended; `reason` if it wasn't us leaving.
    CallEnded {
        room: RoomId,
        reason: Option<Arc<str>>,
    },
    /// Who is in our call now.
    CallMembers {
        room: RoomId,
        members: Vec<CallMember>,
    },
    /// The stage of our call changed (None: no stage going on).
    CallStage {
        room: RoomId,
        stage: Option<crate::call::CallStage>,
    },
    /// The sounds offered by the call's soundboard.
    CallSounds {
        room: RoomId,
        sounds: Vec<crate::call::Sound>,
    },
    /// A soundboard sound was played in a call by a participant. `url` is
    /// where the audio is served (None: nothing to play locally); the UI
    /// fetches it, so no network's host is named outside its crate.
    SoundPlayed {
        room: RoomId,
        user: UserId,
        sound: Arc<str>,
        url: Option<Arc<str>>,
        volume: f32,
    },
    /// A profile asked for with [`Command::LoadProfile`] (sent again as
    /// more of it arrives).
    Profile(Profile),
    /// The stickers the account may send, by server (all of them).
    Stickers {
        account: AccountId,
        groups: Vec<crate::stickers::StickerGroup>,
    },
    /// The account's custom emoji, all of them, by server.
    CustomEmoji {
        account: AccountId,
        groups: Vec<EmojiGroup>,
    },
    /// Who [`Command::SearchMembers`] found for `query` in `room`'s server.
    MemberSearch {
        room: RoomId,
        query: String,
        members: Vec<Member>,
    },
    /// The member list of `room`, grouped.
    Members {
        room: RoomId,
        groups: Vec<MemberGroup>,
    },
    /// Messages matching a search of `room`, newest first (None: it failed).
    SearchResults {
        room: RoomId,
        query: crate::search::SearchQuery,
        /// Where this page started.
        offset: usize,
        /// Where the next page starts; None: that was all.
        next_offset: Option<usize>,
        /// How many matches there are in all, if known.
        total: Option<u64>,
        /// A word to show with the results (such as "still indexing").
        notice: Option<String>,
        /// The room of each message when they aren't all `room`'s (a
        /// server-wide search); empty otherwise.
        rooms: Vec<RoomId>,
        messages: Option<Vec<Message>>,
    },
    /// A forum's posts, as asked with [`Command::LoadForum`].
    Forum(Box<crate::forum::ForumPage>),
    /// Loading a forum's posts failed.
    ForumFailed {
        room: RoomId,
        /// For older posts in `sort` with `tag` (else the open list).
        archived: bool,
        sort: crate::forum::ForumSort,
        tag: Option<Arc<str>>,
    },
    /// `room`'s pinned messages, the latest pinned first (None: loading failed).
    Pins {
        room: RoomId,
        messages: Option<Vec<Message>>,
    },
    /// `user`'s game library, as asked with [`Command::LoadGameLibraries`].
    GameLibrary {
        user: UserId,
        library: crate::games::Library,
    },
    /// Who is in `room`'s call, seen from outside it (empty: nobody).
    VoiceOccupants {
        room: RoomId,
        members: Vec<CallMember>,
    },
    /// Who is typing in `room` now (their names), apart from us.
    Typing {
        room: RoomId,
        names: Vec<Arc<str>>,
    },
    Error(Arc<str>),
}
