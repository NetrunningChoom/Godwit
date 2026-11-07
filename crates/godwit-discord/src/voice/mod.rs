// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Discord voice calls, with DAVE end-to-end encryption.
//!
//! Joining goes as in Discord's clients: the main gateway's Voice State
//! Update (op 4) asks to join, Discord answers with our voice state (its
//! session id) and VOICE_SERVER_UPDATE (a token and an endpoint), and the
//! call connects to that voice server (`connection`): its gateway, UDP with
//! "rtpsize" AEAD transport encryption (`rtp`), and DAVE's MLS group for
//! end-to-end encryption of every Opus frame (`dave`). `session` runs the
//! media on its own thread over Godwit's shared audio chain
//! (`godwit_voice::raw`); `states` tracks who's in which channel.
//!
//! Video is received only, so far: others' cameras in a call, and Go Live
//! streams watched (`stream`, a voice connection of their own). Both go
//! through `video` (feeds and their decoder thread) and `depacketize`
//! (RTP to frames, NACK/PLI), DAVE-decrypted like sound. Sending a camera
//! or a stream is a later step: `connection::Kind`, the codec list and op 12
//! are where it plugs in.
//!
//! Large parts follow Yapfast (GPL-3.0-or-later), ported under AGPL-3.0-only.

pub mod connection;
pub mod dave;
pub mod depacketize;
pub mod golive;
pub mod outgoing;
pub mod packetize;
pub mod rtp;
pub mod session;
pub mod states;
pub mod stream;
pub mod video;

use godwit_voice::call::VoiceSettings;

/// Where Discord said to connect for a call.
#[derive(Clone, PartialEq, Eq)]
pub struct VoiceServer {
    pub endpoint: String,
    pub token: String,
    /// Our main gateway session's id, which the voice state names.
    pub session_id: String,
    pub user_id: String,
    /// The server, or for a DM or group DM call its channel, as identify
    /// names it.
    pub server_id: String,
    pub channel_id: String,
}

impl std::fmt::Debug for VoiceServer {
    // The token stays out of logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceServer")
            .field("endpoint", &self.endpoint)
            .field("server_id", &self.server_id)
            .field("channel_id", &self.channel_id)
            .finish_non_exhaustive()
    }
}

/// To a call under way.
#[derive(Debug)]
pub enum CallCmd {
    /// Connect to this voice server: the first, or where Discord moved the call.
    Server(VoiceServer),
    Muted(bool),
    Deafened(bool),
    /// New volumes (devices stay as they were for this call).
    Settings(Box<VoiceSettings>),
    /// Watch a Go Live stream in the call, at the server Discord named.
    /// Sent again with a new server when Discord moves it.
    Watch(stream::StreamServer),
    /// Stop watching the stream with this key.
    Unwatch(String),
    /// Whether the call is on screen: video is asked for and decoded only then.
    Visible(bool),
    /// Our camera on (from `device`, None the first) or off.
    Camera {
        on: bool,
        device: Option<String>,
    },
    /// Start capturing our screen for Go Live (the system's picker opens
    /// where there is one); `ScreenPicked` follows once a frame is encoded.
    PrepareLive,
    /// Send our Go Live stream to this server (where Discord put it, or
    /// moved it), or stop it with None.
    GoLive(Option<stream::StreamServer>),
    Leave,
}

/// From a call.
#[derive(Debug, PartialEq, Eq)]
pub enum CallEvent {
    /// Connected to the voice server; audio flows.
    Connected,
    /// Voice gateway heartbeat round trip in milliseconds.
    Latency(u16),
    /// Who sounds like speech now: us (while audible), and the others by user id.
    Speaking { me: bool, users: Vec<String> },
    /// Who has their camera on, by user id, as the voice server announced.
    Cameras(Vec<String>),
    /// A stream we watched ended or couldn't start; why, unless we stopped it.
    StreamEnded { key: String, reason: Option<String> },
    /// A video frame was shown: only wakes the UI to draw it.
    Frame,
    /// Our camera went on or off; why, if it stopped on its own.
    OwnCamera { active: bool, error: Option<String> },
    /// The screen for our stream was chosen and its first frame encoded:
    /// Discord may now be told we're live.
    ScreenPicked,
    /// Our Go Live stream started or ended; why, if it ended on its own.
    OwnStream { active: bool, error: Option<String> },
    /// The call ended; why, unless we left.
    Ended(Option<String>),
}

/// A line in the log, as the rest of the Discord crate writes them. Never
/// with a token or key in it.
pub(crate) fn note(message: &str) {
    godwit_core::verbose!("discord: call: {message}");
}
