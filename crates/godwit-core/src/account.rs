// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! What every network's account offers the app, whatever the network.
//!
//! An account turns the core's [`Command`]s into network requests and the
//! network's news into [`Event`]s. Signing in differs too much per network
//! (passwords and codes on Stoat, a homeserver and maybe single sign-on on
//! Matrix, a linked phone on Signal) to share one shape, so each network's
//! sign-in stays on its own type; everything after it goes through here.

use std::sync::Arc;

use crate::application::{Command, Event};
use crate::ids::{AccountId, MessageId, RoomId, UserId};
use crate::network::Network;

/// Downloads one of a network's own media addresses (Matrix's `mxc://`,
/// which need a signed-in download). Blocking: picture loader threads and
/// downloads call it, never the UI thread.
pub type MediaFetch = Arc<dyn Fn(&str) -> Result<Vec<u8>, crate::media::MediaError> + Send + Sync>;

pub trait NetworkAccount {
    fn network(&self) -> Network;
    /// Text inserted when a composer suggestion is picked. Names are supplied
    /// by core; the account owns the network-specific wire spelling.
    fn autocomplete_text(&self, kind: crate::autocomplete::TriggerKind, _id: &str, name: &str) -> String {
        match kind {
            crate::autocomplete::TriggerKind::Person => format!("@{name}"),
            crate::autocomplete::TriggerKind::Channel => format!("#{name}"),
            crate::autocomplete::TriggerKind::Emoji => format!(":{name}:"),
            crate::autocomplete::TriggerKind::MentionSpecial => name.to_owned(),
        }
    }
    /// Extra `@` suggestions this account can derive without making a request.
    fn mention_specials(&self, _room: &RoomId) -> Vec<crate::autocomplete::MentionSpecial> {
        Vec::new()
    }
    /// Who we're signed in as, once we are.
    fn id(&self) -> Option<&AccountId>;
    /// Which account commands for this one go to. Usually [`id`](Self::id), but a
    /// network may still take commands while it isn't ready to be shown (Steam
    /// reconnecting): it then answers them itself, with a failure if need be,
    /// instead of the command vanishing.
    fn route_id(&self) -> Option<&AccountId> {
        self.id()
    }
    /// Whether [`set_offline`](Self::set_offline) can take this network's
    /// connection down.
    fn can_go_offline(&self) -> bool {
        false
    }
    /// Takes the account's connection down (`true`) or brings it back,
    /// staying signed in: nothing comes through while it's down. The app
    /// stops sending it commands meanwhile. Returns what changed.
    fn set_offline(&mut self, _offline: bool) -> Vec<Event> {
        Vec::new()
    }
    /// A kept sign-in that hasn't connected yet (or is retrying while its
    /// network is unreachable): the rail shows it greyed out until it is.
    fn connecting(&self) -> bool {
        false
    }
    /// Everything the network sent since the last call. Never blocks.
    fn poll(&mut self) -> Vec<Event>;
    /// Carries out a command; local state changes come back as events.
    fn run(&mut self, command: Command) -> Vec<Event>;
    /// A message's link in the network's own app, if it has one.
    fn message_link(&self, room: &RoomId, message: &MessageId) -> Option<String>;
    /// Set when the network doesn't take reports from Godwit: the report
    /// entries then show this explanation and its links instead of a form.
    fn report_notice(&self) -> Option<crate::report::ReportNotice> {
        None
    }
    /// The network's section of Settings → Data & privacy: plain facts about
    /// privacy and moderation, each with its sources. None shows only the
    /// policy link.
    fn privacy_section(&self) -> Option<crate::privacy_overview::PrivacySection> {
        None
    }
    /// Whether this network accepts pasted server invites.
    fn supports_invites(&self) -> bool {
        false
    }
    /// Whether this account can create a server from a name, as in Stoat's
    /// official `CreateOrJoinServer` / `CreateServer` modals.
    fn supports_create_server(&self) -> bool {
        false
    }
    fn can_edit_profile(&self) -> bool {
        false
    }
    /// Parse a pasted invite without making any network requests.
    fn parse_invite(&self, _input: &str) -> Option<String> {
        None
    }
    /// Starts (`watch`) or stops watching `user`'s screen share in our call
    /// in `room`, on networks where watching is asked for (Discord's Go
    /// Live); the stream shows in [`Event::CallVideos`] and
    /// [`Event::CallStreams`]. A method rather than a [`Command`], so
    /// networks without it needn't know.
    fn watch_stream(&mut self, _room: &RoomId, _user: &UserId, _watch: bool) -> Vec<Event> {
        Vec::new()
    }
    fn log_out(&mut self);
    /// The address scheme this network's media uses (`"mxc://"`) and how to
    /// fetch it, for networks whose media isn't plain web links.
    fn media(&self) -> Option<(String, MediaFetch)> {
        None
    }
}
