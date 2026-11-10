// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Calls: voice states, call members and their videos.

use super::*;

impl StoatAccount {
    /// Carries out a core command. Local state changes (a room read) come
    /// back as events.
    pub fn video_limit(&self) -> Option<[u32; 2]> {
        let limits = &self.config.as_ref()?.features.limits.as_ref()?.default;
        let limit = limits.video_resolution.unwrap_or([1280, 720]);
        (limits.video && !limit.contains(&1)).then_some(limit)
    }

    /// The call's videos to the core and the voice backend: everyone's
    /// cameras (at most 9 decoded), the screen shares as streams to watch,
    /// and the ones being watched as tiles.
    pub(super) fn sync_call_videos(&mut self, out: &mut Vec<Event>) {
        let Some(call) = self.call.as_mut() else { return };
        let Some(account) = self.account.clone() else { return };
        let Some(room) = self.rooms.room(&call.channel).map(|r| r.id.clone()) else {
            return;
        };
        // Receive only tracks shown in the current call, bounded to avoid
        // decoding every camera in a large room.
        let shown: Vec<&godwit_voice::call::VideoTrackInfo> = call
            .videos
            .iter()
            .filter(|v| !v.screen_share || call.watching.contains(&v.identity))
            .take(9)
            .collect();
        call.video_sids = shown.iter().map(|v| v.sid.clone()).collect();
        let tiles = shown
            .iter()
            .map(|v| godwit_core::call::CallVideo {
                sid: v.sid.clone(),
                user: UserId::new(account.clone(), v.identity.as_str()),
                screen_share: v.screen_share,
            })
            .collect();
        let mut streams: Vec<godwit_core::call::CallStream> = Vec::new();
        for v in call.videos.iter().filter(|v| v.screen_share) {
            if !streams.iter().any(|s| *s.user.raw == *v.identity) {
                streams.push(godwit_core::call::CallStream {
                    user: UserId::new(account.clone(), v.identity.as_str()),
                    watching: call.watching.contains(&v.identity),
                    viewers: None,
                    note: None,
                });
            }
        }
        self.net
            .send(Cmd::Voice(VoiceCommand::Voice(VoiceCmd::Watch(if self.call_shown {
                call.video_sids.clone()
            } else {
                Vec::new()
            }))));
        out.push(Event::CallVideos {
            room: room.clone(),
            videos: tiles,
        });
        out.push(Event::CallStreams { room, streams });
    }

    pub(super) fn handle_voice(&mut self, e: VoiceEvent, out: &mut Vec<Event>) {
        let Some(call) = self.call.as_mut() else { return };
        let Some(room) = self.account.clone().map(|a| RoomId::new(a, call.channel.as_str())) else {
            return;
        };
        match e {
            VoiceEvent::Connected { .. } => {
                out.push(Event::CallConnected { room });
                return;
            }
            VoiceEvent::Latency(milliseconds) => {
                out.push(Event::CallLatency { room, milliseconds });
                return;
            }
            VoiceEvent::Disconnected { reason } => {
                if let Some(r) = &reason {
                    eprintln!("error: voice: {r}");
                }
                self.call = None;
                out.push(Event::CallEnded {
                    room,
                    reason: reason.map(|r| format!("Voice: {r}").into()),
                });
                return;
            }
            VoiceEvent::Participants(p) => call.participants = p,
            VoiceEvent::Speaking(s) => call.speaking = s,
            VoiceEvent::Muted { identity, muted } => {
                call.muted.insert(identity, muted);
            }
            VoiceEvent::Videos(videos) => {
                call.videos = videos;
                // Someone stopped sharing: watching them ends too.
                let sharing: Vec<String> = call
                    .videos
                    .iter()
                    .filter(|v| v.screen_share)
                    .map(|v| v.identity.clone())
                    .collect();
                call.watching.retain(|who| sharing.contains(who));
                self.sync_call_videos(out);
                return;
            }
            VoiceEvent::Camera { active, error } => {
                out.push(Event::CallCamera { room, active });
                if let Some(error) = error {
                    out.push(Event::Error(error.into()));
                }
                return;
            }
            VoiceEvent::ScreenShare { active, error } => {
                out.push(Event::CallScreenShare { room, active });
                if let Some(error) = error {
                    out.push(Event::Error(error.into()));
                }
                return;
            }
            VoiceEvent::VideoFrame => {
                return;
            }
        }
        let Some(call) = self.call.as_ref() else { return };
        let server = self.store.channels.get(&call.channel).and_then(|c| c.server.clone());
        let members = call
            .participants
            .iter()
            .map(|user| {
                let mut m = self.call_member(user, server.as_deref());
                m.speaking = call.speaking.contains(user);
                m.muted = call.muted.get(user).copied().unwrap_or(false);
                // LiveKit doesn't say; Stoat's voice state does.
                m.deafened = self
                    .store
                    .voice
                    .get(&call.channel)
                    .and_then(|v| v.get(user))
                    .is_some_and(|v| !v.is_receiving);
                m
            })
            .collect();
        out.push(Event::CallMembers { room, members });
    }

    pub(super) fn call_member(&self, user: &str, server: Option<&str>) -> CallMember {
        let account = self
            .account
            .clone()
            .unwrap_or_else(|| AccountId::new(Network::Stoat, ""));
        CallMember {
            user: UserId::new(account, user),
            name: self.store.display_name(user, server).into(),
            avatar_url: Some(self.avatar_url(user, server).into()),
            speaking: false,
            muted: false,
            deafened: false,
            stage: None,
            video: false,
            streaming: false,
        }
    }

    /// Sends who is in each channel's call, where that changed.
    pub(super) fn sync_voice(&mut self, out: &mut Vec<Event>) {
        if self.account.is_none() {
            return;
        }
        let mut now: HashMap<String, Vec<CallMember>> = HashMap::new();
        for (channel, users) in &self.store.voice {
            let server = self.store.channels.get(channel).and_then(|c| c.server.clone());
            let members: Vec<CallMember> = users
                .values()
                .map(|v| CallMember {
                    muted: !v.is_publishing,
                    deafened: !v.is_receiving,
                    ..self.call_member(&v.id, server.as_deref())
                })
                .collect();
            if !members.is_empty() {
                now.insert(channel.clone(), members);
            }
        }
        for (channel, members) in &now {
            if self.occupants.get(channel) != Some(members)
                && let Some(room) = self.room_id(channel)
            {
                out.push(Event::VoiceOccupants {
                    room,
                    members: members.clone(),
                });
            }
        }
        for channel in self.occupants.keys() {
            if !now.contains_key(channel)
                && let Some(room) = self.room_id(channel)
            {
                out.push(Event::VoiceOccupants {
                    room,
                    members: Vec::new(),
                });
            }
        }
        self.occupants = now;
    }
}
