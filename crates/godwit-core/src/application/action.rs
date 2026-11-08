// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors

use super::*;

/// Something the user did.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// An account's privacy switches: load them, or flip one.
    LoadPrivacy(AccountId),
    SetPrivacy {
        account: AccountId,
        key: Arc<str>,
        on: bool,
    },
    /// An account's signed-in devices: load them, or sign one out.
    LoadSessions(AccountId),
    LogOutSession {
        account: AccountId,
        session: Arc<str>,
    },
    LogOutSessionWithProof {
        account: AccountId,
        target: crate::privacy::SessionTarget,
        proof: crate::privacy::SessionProof,
    },
    RenameSession {
        account: AccountId,
        session: Arc<str>,
        name: Arc<str>,
    },
    /// Auto-delete for an account: open its page, or change its rules.
    AutoDelete(AccountId, crate::autodelete::AutoDeleteCommand),
    StartScreenShare {
        room: RoomId,
        quality: crate::call::ScreenShareQuality,
    },
    /// A server channel's settings page (see [`crate::channel_settings`]).
    OpenChannelSettings(RoomId),
    CloseChannelSettings,
    SaveChannelOverview(crate::channel_settings::ChannelOverviewEdit),
    DeleteChannel,
    OpenServerSettings(ServerId),
    CloseServerSettings,
    CloseInviteLink,
    SaveServerOverview(crate::server_settings::ServerOverviewEdit),
    /// Switches the open settings to a page, loading it the first time.
    ServerSettingsPage(crate::server_settings::ServerPage),
    /// An admin change or load, from server settings or a member's card.
    ServerAdmin {
        server: ServerId,
        op: crate::server_settings::ServerAdmin,
    },
    LoadOwnProfile(AccountId),
    SaveOwnProfile {
        account: AccountId,
        edit: ProfileEdit,
    },
    /// Show a server's rooms, or (None) the direct messages.
    SelectServer(Option<ServerId>),
    SelectRoom(RoomId),
    /// Home's own arrangement (kept by the app between starts): a chat
    /// pinned to the top, or hidden in the collapsed section at the bottom.
    PinRoom {
        room: RoomId,
        pinned: bool,
    },
    HideRoom {
        room: RoomId,
        hidden: bool,
    },
    /// Opens or closes Home's hidden section.
    ToggleHiddenRooms,
    /// Marks a chat unread here (until it's opened), whatever its network says.
    MarkRoomUnread(RoomId),
    /// Marks a chat read, at its network too.
    MarkRoomRead(RoomId),
    /// The kept pins and hidden chats, at start.
    RestoreRoomPrefs {
        pinned: Vec<RoomId>,
        hidden: Vec<RoomId>,
    },
    /// The composer's text for the selected room changed.
    SetDraft(String),
    /// Send the selected room's draft. `now_ms` stamps the pending message
    /// until the server confirms it.
    Send {
        now_ms: i64,
    },
    /// The timeline was scrolled to its top.
    LoadEarlier,
    /// "These are the same person."
    MergeContacts {
        into: ContactId,
        from: ContactId,
    },
    /// Close the "switch network" suggestion; `never` stops it for that person.
    DismissNudge {
        never: bool,
    },
    /// The user read the errors shown.
    ClearErrors,
    /// Answer this message in the selected room (the composer shows it).
    ReplyTo(MessageId),
    /// Edit one of our messages: the composer takes its text.
    StartEdit(MessageId),
    /// Edit our latest message in the selected room (arrow-up in an empty
    /// composer, as officially).
    EditLast,
    /// Show a message of the selected room (a reply's original): loads the
    /// messages around it first if it isn't loaded.
    JumpTo(MessageId),
    /// Back to the newest messages after a jump into older history.
    JumpToPresent,
    /// Vote for (or take back) answer `answer` of the poll in `message` of
    /// the selected room.
    VotePoll {
        message: MessageId,
        answer: u64,
    },
    /// Search the selected room's messages (a server channel searches its
    /// channel); empty opens the search with nothing asked yet.
    Search(String),
    CloseSearch,
    /// Start a thread in the selected room: from `message`, or (a forum) a
    /// new post whose first message is `text`. The thread opens once made.
    CreateThread {
        name: String,
        message: Option<MessageId>,
        text: String,
    },
    /// Show the selected room's pinned messages (loading them).
    OpenPins,
    ClosePins,
    /// Report something to the network, with the reason chosen and the
    /// reporter's own words (may be empty). Answered with [`Event::Reported`].
    Report {
        target: crate::report::ReportTarget,
        reason: crate::report::ReportReason,
        detail: String,
    },
    /// Pin or unpin a message of the selected room.
    SetPinned {
        message: MessageId,
        pinned: bool,
    },
    /// Drop the reply or edit in progress (an edit also drops its text).
    CancelCompose,
    /// Delete a message in the selected room (the UI confirms first).
    Delete(MessageId),
    /// Mark the selected room unread from this message on.
    MarkUnread(MessageId),
    /// Add our reaction with `emoji`, or take it back if it's there.
    ToggleReaction {
        message: MessageId,
        emoji: Arc<str>,
    },
    /// Remove one emoji's reactions (Some) or all reactions (None) as a moderator.
    RemoveReactions {
        message: MessageId,
        emoji: Option<Arc<str>>,
    },
    /// Files to send with the selected room's next message.
    AddFiles(Vec<Outgoing>),
    /// Drop one of the files waiting to be sent.
    RemoveFile(usize),
    /// Show the friends list instead of a room.
    ShowFriends,
    /// Show People: everyone as one contact across networks.
    ShowPeople,
    /// Show Active now: friends in calls and in games, across networks.
    ShowActive,
    /// Recent subscribed threads in the selected Slack workspace.
    ShowThreads,
    /// The Threads page scrolled near its end: fetch the next page.
    LoadMoreThreads,
    /// "These two aren't the same person": stop suggesting the link.
    NotSame {
        a: ContactId,
        b: ContactId,
    },
    /// "Same person as…" from a profile: People, linking them.
    LinkFrom(UserId),
    /// Turn "switch to a better network" suggestions for a person on or off.
    SetNudges {
        contact: ContactId,
        on: bool,
    },
    /// Join a room from a server's directory.
    JoinRoom(RoomId),
    /// Join a room we're invited to, or turn the invite down.
    AnswerInvite {
        room: RoomId,
        accept: bool,
    },
    /// Set our presence on an account (Online, Idle, …, Invisible).
    SetPresence {
        account: AccountId,
        presence: Presence,
    },
    /// Set our custom status text there (None or blank clears it).
    SetStatusText {
        account: AccountId,
        text: Option<String>,
    },
    /// Verify this device by comparing emoji with another of ours.
    StartVerification(AccountId),
    /// Accept a verification another of our devices asked for.
    AcceptVerification,
    /// The emoji match (or don't) the other device's.
    AnswerVerification {
        matches: bool,
    },
    /// Close the verification (cancelling it if it's still going).
    CloseVerification,
    /// Verify this device with the network's recovery key.
    UnlockWithKey {
        account: AccountId,
        key: Secret,
    },
    /// "This account isn't them": take it out of its contact.
    SplitIdentity(UserId),
    /// Talk to this person on `network` (None: the most private one shared).
    SetPreferred {
        contact: ContactId,
        network: Option<Network>,
    },
    /// Talk to this person through exactly `user` (one of their identities),
    /// which also picks its network.
    PreferIdentity {
        contact: ContactId,
        user: UserId,
    },
    /// Open (or start) the direct chat with someone.
    OpenDirect(UserId),
    /// Create a named group conversation with these account's friends.
    CreateGroup {
        account: AccountId,
        name: String,
        users: Vec<UserId>,
    },
    /// Talk to this person on `user`'s network from now on, and open that
    /// chat (the switcher in a merged person's chat).
    TalkOn(UserId),
    /// Our own name for a person (None: theirs again). Local only.
    SetNickname {
        contact: ContactId,
        nickname: Option<String>,
    },
    /// Star or unstar a person (all of `users`, their accounts) on the
    /// networks that keep favourites themselves. Godwit's local stars are
    /// kept by the app; this only reaches the networks.
    SetFavourite {
        users: Vec<UserId>,
        favourite: bool,
    },
    /// Send `user` a friend request, or accept theirs.
    AddFriend(UserId),
    /// Unfriend `user`, cancel our request, or turn theirs down.
    RemoveFriend(UserId),
    /// Block `user` (their messages collapse and they can't reach us), when
    /// the shown profile allows it.
    BlockUser(UserId),
    /// Lift our block on `user`.
    UnblockUser(UserId),
    /// Remove a member from a group conversation, when the shown profile allows it.
    RemoveGroupMember {
        room: RoomId,
        user: UserId,
    },
    /// Make a group member the owner, when the shown profile allows it.
    TransferGroupOwnership {
        room: RoomId,
        user: UserId,
    },
    /// An invite card came on screen: fetch what it leads to (once).
    LoadInvite {
        account: AccountId,
        code: Arc<str>,
    },
    /// An invite card's Join.
    JoinInvite {
        account: AccountId,
        code: Arc<str>,
    },
    /// The friends list's "Add friend": a request to someone by their name
    /// on the network (`name#1234` on Stoat), from the first account whose
    /// network has friends.
    SendFriendRequest {
        account: AccountId,
        name: String,
    },
    /// Show or hide the member list.
    ToggleMembers,
    /// The member list's rows `first` to `last` are on screen: networks that
    /// stream lists in ranges (Discord, Fluxer) ask for the ones around them.
    MemberRows {
        first: u32,
        last: u32,
    },
    /// Open someone's profile card (as seen from the selected room).
    OpenProfile(UserId),
    CloseProfile,
    /// Join a room's call (leaving any other first).
    JoinCall(RoomId),
    /// Turn down the call ringing us in a room (it stops ringing for us).
    DeclineCall(RoomId),
    LeaveCall,
    /// Whether the call's video is on screen: its room is open, or the
    /// pop-out shows it over another room. Video is fetched only then.
    SetCallShown(bool),
    ToggleMute,
    /// Our mute and deafen as last saved (at start).
    RestoreVoiceFlags {
        muted: bool,
        deafened: bool,
    },
    ToggleDeafen,
    SetCamera(bool),
    SetScreenShare(bool),
    /// Watch (or stop watching) someone's screen share in our call, where
    /// the network asks for it ([`crate::call::Call::streams`]).
    WatchStream {
        user: UserId,
        watch: bool,
    },
    /// The GIF picker opened (in the selected room): its categories load,
    /// once per session.
    OpenGifs,
    /// Show GIFs for `query` ("" for the categories, [`crate::gifs::TRENDING`]
    /// for trending); `more` loads the next page of the current one.
    SearchGifs {
        query: String,
        more: bool,
    },
    /// Send a picked GIF's link in the selected room (keeping a reply).
    SendGif {
        url: Arc<str>,
        now_ms: i64,
    },
}
