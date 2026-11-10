// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Voice calls: the one we're in, and who is in each room's call.

use std::sync::Arc;

use crate::ids::{RoomId, UserId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallStatus {
    Connecting,
    Connected,
}

/// Someone in a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallMember {
    pub user: UserId,
    pub name: Arc<str>,
    pub avatar_url: Option<Arc<str>>,
    pub speaking: bool,
    pub muted: bool,
    /// Hears nothing (deafened themselves).
    pub deafened: bool,
    /// Their part on a stage (speaker, audience, hand raised); None outside
    /// stage rooms.
    pub stage: Option<StageRole>,
    /// Their camera is on (only where the network says so from outside the call).
    pub video: bool,
    /// Sharing their screen live.
    pub streaming: bool,
}

/// The line summing up who is in a voice channel, for the card its row
/// shows under the pointer: `1 person`, or `3 people · 1 live · 2 cameras`;
/// empty for nobody. Port of Yapfast 0cf06a66's `voice_roster` summary.
pub fn voice_summary(members: &[CallMember]) -> String {
    if members.is_empty() {
        return String::new();
    }
    let live = members.iter().filter(|m| m.streaming).count();
    let cameras = members.iter().filter(|m| m.video).count();
    let n = members.len();
    let mut parts = vec![format!("{n} {}", if n == 1 { "person" } else { "people" })];
    if live > 0 {
        parts.push(format!("{live} live"));
    }
    if cameras > 0 {
        parts.push(format!("{cameras} {}", if cameras == 1 { "camera" } else { "cameras" }));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod summary_tests {
    use super::*;
    use crate::ids::{AccountId, UserId};
    use crate::network::Network;

    fn member(name: &str, video: bool, streaming: bool) -> CallMember {
        CallMember {
            user: UserId::new(AccountId::new(Network::Stoat, "a"), name),
            name: name.into(),
            avatar_url: None,
            speaking: false,
            muted: false,
            deafened: false,
            stage: None,
            video,
            streaming,
        }
    }

    #[test]
    fn sums_up_who_is_in_a_voice_channel() {
        assert_eq!(voice_summary(&[]), "");
        assert_eq!(voice_summary(&[member("Ann", false, false)]), "1 person");
        let all = [
            member("Ann", true, true),
            member("Bob", false, true),
            member("Cat", true, false),
            member("Dan", false, false),
        ];
        assert_eq!(voice_summary(&all), "4 people · 2 live · 2 cameras");
        let cam = [member("Ann", true, false), member("Bob", false, false)];
        assert_eq!(voice_summary(&cam), "2 people · 1 camera");
    }
}

/// Where someone stands on a stage: speakers talk, the audience listens and
/// may raise a hand to be invited to speak.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageRole {
    Speaker,
    Audience,
    HandRaised,
}

/// What can be asked on a stage ([`crate::application::Action::Stage`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageRequest {
    /// Us, from the audience: ask to speak.
    RequestToSpeak,
    /// Us: take the raised hand back.
    LowerHand,
    /// A moderator lets someone speak.
    InviteToSpeak,
    /// A speaker steps down (us), or a moderator moves someone down.
    MoveToAudience,
}

/// The stage going on in a stage room's call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallStage {
    pub topic: Arc<str>,
    /// We may invite people to speak and move them to the audience.
    pub moderate: bool,
    /// We may ask to speak.
    pub may_request: bool,
}

/// The call we're in (one at a time, across every account).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub room: RoomId,
    pub status: CallStatus,
    /// Latest voice transport round trip (ms), when the network exposes it.
    pub latency_ms: Option<u16>,
    pub members: Vec<CallMember>,
    pub muted: bool,
    pub deafened: bool,
    pub camera: bool,
    pub screen_share: bool,
    pub videos: Vec<CallVideo>,
    /// Who shares their screen in the call for others to watch (Discord's
    /// Go Live), and whether we do. Networks that send every screen share
    /// to everyone (LiveKit) leave this empty and list them in `videos`.
    pub streams: Vec<CallStream>,
    /// The stage, in a stage room (see [`crate::room::Capabilities::stage`]).
    pub stage: Option<CallStage>,
    /// The soundboard currently available in this call.
    pub sounds: Vec<Sound>,
}

/// A network-neutral sound that can be played in a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sound {
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub emoji: Option<Arc<str>>,
    pub source: Option<Arc<str>>,
}

/// Someone sharing their screen in our call, which we may watch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallStream {
    pub user: UserId,
    pub watching: bool,
    /// Who watches this stream, us included, when the network says
    /// (Discord tells only a stream's viewers who its viewers are).
    pub viewers: Option<Vec<UserId>>,
    /// Why watching it stopped or failed, shown beside it until it's
    /// watched again ("No video arrived.", "The stream ended.").
    pub note: Option<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallVideo {
    pub sid: String,
    pub user: UserId,
    pub screen_share: bool,
}

/// Common screen-share presets; adapters apply the network's allowed limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenShareQuality {
    Low,
    High,
    Text,
}
