// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors

use super::*;

/// Work for a network backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// The account's privacy switches (Settings → Privacy).
    LoadPrivacy,
    SetPrivacy {
        key: Arc<str>,
        on: bool,
    },
    /// The account's signed-in devices (Settings → Sessions), and signing one out.
    LoadSessions,
    LogOutSession(Arc<str>),
    LogOutSessionWithProof {
        target: crate::privacy::SessionTarget,
        proof: crate::privacy::SessionProof,
    },
    RenameSession {
        session: Arc<str>,
        name: Arc<str>,
    },
    /// Auto-delete's page for this account (networks that have it).
    AutoDelete(crate::autodelete::AutoDeleteCommand),
    StartScreenShare {
        quality: crate::call::ScreenShareQuality,
    },
    ServerAdmin {
        server: ServerId,
        op: crate::server_settings::ServerAdmin,
    },
    /// Marks a whole chat read, up to its newest message as the network knows it.
    MarkRoomRead {
        room: RoomId,
    },
    LoadChannelOverview {
        room: RoomId,
        request: u64,
    },
    SaveChannelOverview {
        room: RoomId,
        request: u64,
        edit: Box<crate::channel_settings::ChannelOverviewEdit>,
    },
    /// Deletes a server channel; answered with [`Event::ChannelOverview`]
    /// (`Ok(None)` once it is gone).
    DeleteChannel {
        room: RoomId,
        request: u64,
    },
    /// Sets (Some((allow, deny))) or removes (None) a channel override;
    /// answered with [`Event::ChannelOverview`] holding the channel as it is now.
    SetChannelOverride {
        room: RoomId,
        request: u64,
        target: crate::channel_settings::OverrideTarget,
        change: Option<(u64, u64)>,
    },
    LoadServerOverview {
        server: ServerId,
        request: u64,
    },
    SaveServerOverview {
        server: ServerId,
        request: u64,
        /// Boxed: two image picks with paths would make every command large.
        edit: Box<crate::server_settings::ServerOverviewEdit>,
    },
    LoadOwnProfile,
    SaveOwnProfile(ProfileEdit),
    /// The room on screen, or none of this account's. Networks use it to
    /// mark live messages read and to keep that room's history fresh.
    Focus {
        room: Option<RoomId>,
    },
    /// `nonce` comes back in [`Event::MessageConfirmed`].
    SendMessage {
        room: RoomId,
        text: String,
        nonce: Arc<str>,
        reply_to: Option<MessageId>,
        files: Vec<Outgoing>,
    },
    /// Send a sticker (a network's id from [`crate::stickers::Sticker`]) with
    /// no text; the message arrives like any other.
    SendSticker {
        room: RoomId,
        sticker: Arc<str>,
        reply_to: Option<MessageId>,
    },
    /// Open the direct chat with `user`, creating it if needed; answered
    /// with [`Event::RoomOpened`].
    OpenDirect {
        user: UserId,
    },
    /// Create a named group conversation; answered by opening the new room.
    CreateGroup {
        name: String,
        users: Vec<UserId>,
    },
    /// Report `target`; answered with [`Event::Reported`].
    Report {
        target: crate::report::ReportTarget,
        reason: crate::report::ReportReason,
        detail: String,
    },
    /// Send a friend request (or accept one); the network's own update of
    /// the relationship follows.
    AddFriend {
        user: UserId,
    },
    /// Star or unstar a person in the network's own favourites; the
    /// network's [`Event::Favourites`](super::Event::Favourites) follows.
    SetFavourite {
        user: UserId,
        favourite: bool,
    },
    /// Unfriend, cancel our request, or decline theirs.
    RemoveFriend {
        user: UserId,
    },
    /// Block a user; the network's own update of the relationship follows.
    BlockUser {
        user: UserId,
    },
    /// Lift our block on a user.
    UnblockUser {
        user: UserId,
    },
    /// Set our private note on `user` (empty removes it); the network's
    /// own update follows as a new [`Event::Profile`].
    SetNote {
        user: UserId,
        note: String,
    },
    /// Remove a member from a group conversation.
    RemoveGroupMember {
        room: RoomId,
        user: UserId,
    },
    /// Transfer ownership of a group conversation.
    TransferGroupOwnership {
        room: RoomId,
        user: UserId,
    },
    /// The GIF categories; answered with [`Event::GifCategories`].
    LoadGifCategories,
    /// GIFs for `query` ([`crate::gifs::TRENDING`]: trending), from
    /// `position` (a page's `next`); answered with [`Event::Gifs`] or
    /// [`Event::GifsFailed`].
    SearchGifs {
        query: String,
        position: Option<String>,
    },
    /// What invite `code` leads to; answered with [`Event::InviteLoaded`].
    LoadInvite {
        code: Arc<str>,
    },
    /// Join through invite `code`; answered with [`Event::InviteJoined`] (and
    /// the server arriving), or [`Event::InviteLoaded`] with None.
    JoinInvite {
        code: Arc<str>,
    },
    /// A friend request by name; answered with [`Event::FriendRequestSent`].
    SendFriendRequest {
        request: u64,
        name: String,
    },
    /// `user`'s profile as seen from `room`; answered with [`Event::Profile`].
    LoadProfile {
        user: UserId,
        room: Option<RoomId>,
    },
    /// The member list for `room`, answered with [`Event::Members`] (and
    /// again whenever it changes, while it's the room asked about).
    LoadMembers {
        room: RoomId,
    },
    /// Members of `room`'s server whose names start with `query`, for networks
    /// that don't hold every member (`Capabilities::member_search`); answered
    /// with [`Event::MemberSearch`]. Only ever sent for what the user typed.
    SearchMembers {
        room: RoomId,
        query: String,
    },
    /// The open member list's rows `first` to `last` are on screen (see
    /// [`Action::MemberRows`]); networks without ranges ignore it.
    MemberRows {
        room: RoomId,
        first: u32,
        last: u32,
    },
    /// Vote for, or take back, a poll answer (the network keeps which are ours);
    /// the poll's message comes back as [`Event::MessageUpdated`].
    VotePoll {
        room: RoomId,
        message: MessageId,
        answer: u64,
    },
    /// Post a poll in `room` (already checked against its `polls` limits and
    /// the allowed durations); it arrives as a message of the room.
    CreatePoll {
        room: RoomId,
        question: String,
        answers: Vec<String>,
        hours: u32,
        multi: bool,
    },
    /// End our poll in `message` now; it comes back as [`Event::MessageUpdated`].
    EndPoll {
        room: RoomId,
        message: MessageId,
    },
    /// Messages of `room` matching `query`, from the `offset`th match;
    /// answered with [`Event::SearchResults`].
    SearchMessages {
        room: RoomId,
        query: crate::search::SearchQuery,
        offset: usize,
    },
    /// Refresh the network's own subscribed-thread index; with `more`,
    /// fetch its next (older) page instead.
    LoadThreads {
        server: ServerId,
        more: bool,
    },
    /// Start a thread in `room` (see [`Action::CreateThread`]); the network's
    /// new room arrives, then [`Event::RoomOpened`] for it.
    CreateThread {
        room: RoomId,
        name: String,
        message: Option<MessageId>,
        text: String,
        /// A forum post's tags, by id (already checked against the forum's).
        tags: Vec<Arc<str>>,
    },
    /// A forum's posts: with no `offset` its tags, rules and open posts;
    /// with one, the older (archived) posts from there in `sort` order,
    /// narrowed to `tag`. Answered with [`Event::Forum`].
    LoadForum {
        room: RoomId,
        sort: crate::forum::ForumSort,
        tag: Option<Arc<str>>,
        offset: Option<usize>,
    },
    /// These people's game libraries (networks with
    /// [`Network::has_game_libraries`]); answered with an
    /// [`Event::GameLibrary`] for each.
    LoadGameLibraries {
        users: Vec<UserId>,
    },
    /// `room`'s pinned messages; answered with [`Event::Pins`].
    LoadPins {
        room: RoomId,
    },
    /// Pin or unpin `message`; answered with [`Event::Pins`] (the list again)
    /// or an error.
    SetPinned {
        room: RoomId,
        message: MessageId,
        pinned: bool,
    },
    /// Answered with [`Event::CallConnected`] or [`Event::CallEnded`].
    JoinCall {
        room: RoomId,
    },
    /// Turn down the call ringing us in `room`; answered with
    /// [`Event::AlertDone`]. Networks without ringing ignore it.
    DeclineCall {
        room: RoomId,
    },
    LeaveCall,
    /// The call's video is on screen (see `Action::SetCallShown`): fetch and
    /// decode it only while true.
    SetCallShown(bool),
    SetMuted(bool),
    SetDeafened(bool),
    SetCamera(bool),
    SetScreenShare(bool),
    EditMessage {
        room: RoomId,
        message: MessageId,
        text: String,
    },
    DeleteMessage {
        room: RoomId,
        message: MessageId,
    },
    React {
        room: RoomId,
        message: MessageId,
        emoji: Arc<str>,
        add: bool,
    },
    /// Remove one emoji's reactions (Some) or all reactions (None) as a moderator.
    RemoveReactions {
        room: RoomId,
        message: MessageId,
        emoji: Option<Arc<str>>,
    },
    /// We're typing in `room` (sent as the draft changes; networks throttle it).
    Typing {
        room: RoomId,
    },
    /// The messages around `message`; answered with [`Event::HistoryAround`].
    LoadAround {
        room: RoomId,
        message: MessageId,
    },
    /// The page before `before` (the newest page when `None`).
    LoadHistory {
        room: RoomId,
        before: Option<MessageId>,
    },
    MarkRead {
        room: RoomId,
        last: MessageId,
    },
    /// A server's rooms we could join; answered with [`Event::Directory`].
    LoadDirectory {
        server: ServerId,
    },
    /// Answered with [`Event::RoomUpdated`] once joined.
    JoinRoom {
        room: RoomId,
    },
    /// Join `room` (accept), or decline the invite; answered with
    /// [`Event::RoomUpdated`] or [`Event::RoomRemoved`].
    AnswerInvite {
        room: RoomId,
        accept: bool,
    },
    /// Set (or clear) our custom status text; answered with
    /// [`Event::OwnPresence`].
    SetStatusText(Option<String>),
    /// Set our presence; answered with [`Event::OwnPresence`].
    SetPresence(Presence),
    /// Ask our other devices to verify this one; answered with
    /// [`Event::Verification`] as it goes.
    StartVerification,
    AnswerVerification {
        matches: bool,
    },
    CancelVerification,
    AcceptVerification,
    /// Verify this device with a recovery key; answered with
    /// [`Event::DeviceTrust`], or an [`Event::Error`] if the key is wrong.
    UnlockWithKey {
        key: Secret,
    },
    /// A room's own notification level or mute (only sent where the room's
    /// `Capabilities::notify_settings` is on); the account answers with
    /// [`Event::RoomUpdated`] carrying the new `Room::notify`.
    SetRoomNotify {
        room: RoomId,
        change: crate::room::RoomNotifyChange,
    },
    /// Forward `message` of `room` to `to` (same account; only sent where
    /// the rooms' `forward_from` / `forward_to` capabilities are on), then
    /// `comment`, when not empty, as a message of its own.
    ForwardMessage {
        room: RoomId,
        message: MessageId,
        to: RoomId,
        comment: String,
    },
    /// Everything after `last_read` is unread again (None: the whole room).
    MarkUnread {
        room: RoomId,
        last_read: Option<MessageId>,
    },
}
