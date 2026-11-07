// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Calls on the account's side: joining and leaving through the main
//! gateway's Voice State Update (op 4), handing the voice server Discord
//! assigns to the call's media (`voice::session`), who's in every voice
//! channel and DM call, and DM calls ringing (both ways), as Discord's
//! clients and Yapfast do them. Also the call's video: others' cameras as
//! the media reports them, and Go Live streams, watched through the main
//! gateway (op 20 and the STREAM_* dispatches) and then a connection of
//! their own in the media (`voice::stream`).

use std::collections::BTreeMap;
use std::sync::Arc;

use godwit_core::application::{AlertKind, Event};
use godwit_core::call::{CallMember, CallStream, CallVideo};
use godwit_core::ids::{RoomId, UserId};
use serde_json::{Value, json};

use super::DiscordAccount;
use crate::backend::{Cmd, Request};
use crate::store;
use crate::voice::states::Changed;
use crate::voice::stream::{self, Opening, StreamServer};
use crate::voice::video::{FeedKind, sid};
use crate::voice::{CallCmd, CallEvent, VoiceServer};

/// Our call, from asking to join until it ends.
pub(super) struct OurCall {
    /// Tags the call's [`crate::backend::Ev::Call`]s.
    tag: u64,
    room: RoomId,
    guild: Option<String>,
    channel: String,
    /// Our voice session (from our voice state), and the voice server's
    /// token and endpoint: the call connects once both are known.
    session: Option<String>,
    server: Option<(String, String)>,
    /// What the call was last told to connect to.
    sent: Option<VoiceServer>,
    connected: bool,
    /// We started a DM call nobody was in: ring the others once it's up.
    ring: bool,
    muted: bool,
    deafened: bool,
    /// Who sounds like speech: us, and the others by user id.
    speaking: (bool, Vec<String>),
    /// Who has their camera on, as the media heard from the voice server.
    cameras: Vec<String>,
    /// The streams we watch, by key.
    watching: BTreeMap<String, Watched>,
    /// Our camera is on (or starting).
    camera_on: bool,
    /// Our own Go Live stream, from asking for it until it ends.
    live: Option<Live>,
    /// Why watching someone's stream last stopped, by user id, until it's
    /// watched again or they stop streaming.
    stream_notes: BTreeMap<String, Arc<str>>,
    /// Marks us in a call for the gateway's QoS heartbeats while it lasts.
    _rtc: crate::client::InCall,
}

/// Our own Go Live stream: its key, and where Discord serves it so far.
struct Live {
    key: String,
    opening: Opening,
    sent: Option<StreamServer>,
    /// Discord was told (op 18): the screen was chosen and a frame encoded.
    announced: bool,
}

/// A Go Live stream we asked to watch.
struct Watched {
    user: String,
    /// Where Discord serves it, as it has said so far.
    opening: Opening,
    /// What the media was last told to connect to.
    sent: Option<StreamServer>,
    /// Who watches it, from STREAM_CREATE/STREAM_UPDATE's `viewer_ids`
    /// (the official client's ApplicationStreamingStore); None until told.
    viewers: Option<Vec<String>>,
}

impl DiscordAccount {
    /// Op 4: into `channel`, or out of voice with None. Sending it again in
    /// the same channel changes only how others see us muted or deafened.
    fn voice_state_update(&self, guild: Option<&str>, channel: Option<&str>, muted: bool, deafened: bool, video: bool) {
        self.net.send(Cmd::Gateway {
            op: crate::gateway::VOICE_STATE_UPDATE,
            // Discord's clients mute themselves while deafened.
            data: json!({
                "guild_id": guild,
                "channel_id": channel,
                "self_mute": muted || deafened,
                "self_deaf": deafened,
                "self_video": video,
            }),
        });
    }

    pub(super) fn join_call(&mut self, room: RoomId, out: &mut Vec<Event>) {
        let Some(channel) = self.channel_of(&room) else { return };
        let Some(c) = self.store.channels.get(&channel) else {
            return;
        };
        let allowed =
            c.private() || (c.kind == store::GUILD_VOICE && self.store.can(&channel, crate::permissions::CONNECT));
        if !allowed {
            out.push(Event::CallEnded {
                room,
                reason: Some("You can't join this voice channel.".into()),
            });
            return;
        }
        // Why the share and camera buttons are on or off, for the log:
        // numbers only (permission bits, counts), never names or tokens.
        eprintln!("note: discord: joining a call: {}", self.store.video_reason(&channel));
        let guild = c.guild.clone();
        let private = c.private();
        if let Some(previous) = self.call.take() {
            self.stop_watching_all(&previous);
            self.net.send(Cmd::CallCmd(CallCmd::Leave));
            if previous.guild != guild {
                self.voice_state_update(previous.guild.as_deref(), None, false, false, false);
            }
        }
        // Answering a call ringing us.
        if self.ringing_me(&channel) {
            self.ringing.remove(&channel);
            out.push(Event::AlertDone { room: room.clone() });
        }
        let alone = self.voice.in_channel(&channel).iter().all(|s| s.user == self.me());
        self.voice_state_update(guild.as_deref(), Some(&channel), false, false, false);
        self.next_call = self.next_call.wrapping_add(1);
        self.net.send(Cmd::Call {
            call: self.next_call,
            settings: Box::new(self.voice_settings.clone()),
            muted: false,
            deafened: false,
        });
        self.call = Some(OurCall {
            tag: self.next_call,
            room,
            guild,
            channel,
            session: None,
            server: None,
            sent: None,
            connected: false,
            ring: private && alone,
            muted: false,
            deafened: false,
            speaking: (false, Vec::new()),
            cameras: Vec::new(),
            watching: BTreeMap::new(),
            camera_on: false,
            live: None,
            stream_notes: BTreeMap::new(),
            _rtc: crate::client::InCall::start(),
        });
        self.call_visibility();
    }

    pub(super) fn leave_call(&mut self, out: &mut Vec<Event>) {
        let Some(call) = self.call.take() else { return };
        self.stop_watching_all(&call);
        self.net.send(Cmd::CallCmd(CallCmd::Leave));
        self.voice_state_update(call.guild.as_deref(), None, false, false, false);
        out.push(Event::CallEnded {
            room: call.room,
            reason: None,
        });
    }

    pub(super) fn set_call_muted(&mut self, muted: Option<bool>, deafened: Option<bool>) {
        let Some(call) = self.call.as_mut() else { return };
        if let Some(muted) = muted {
            call.muted = muted;
            self.net.send(Cmd::CallCmd(CallCmd::Muted(muted)));
        }
        if let Some(deafened) = deafened {
            call.deafened = deafened;
            self.net.send(Cmd::CallCmd(CallCmd::Deafened(deafened)));
        }
        let (guild, channel, muted, deafened) = (call.guild.clone(), call.channel.clone(), call.muted, call.deafened);
        let video = call.camera_on;
        self.voice_state_update(guild.as_deref(), Some(&channel), muted, deafened, video);
    }

    /// Our camera on or off: the voice state says so first (`self_video`,
    /// as the official client), then the call's media starts or stops it.
    /// The core hears once the camera really started ([`CallEvent::OwnCamera`]).
    pub(super) fn set_call_camera(&mut self, on: bool, out: &mut Vec<Event>) {
        let device = self.voice_settings.camera.clone();
        let Some(call) = self.call.as_mut().filter(|c| c.connected) else {
            if let Some(room) = self.call.as_ref().map(|c| c.room.clone()) {
                out.push(Event::CallCamera { room, active: false });
            }
            return;
        };
        if call.camera_on == on {
            return;
        }
        call.camera_on = on;
        let (guild, channel, muted, deafened) = (call.guild.clone(), call.channel.clone(), call.muted, call.deafened);
        if on {
            eprintln!(
                "note: discord: camera on: {}",
                self.store.tier_tripwire(guild.as_deref())
            );
        }
        self.voice_state_update(guild.as_deref(), Some(&channel), muted, deafened, on);
        self.net.send(Cmd::CallCmd(CallCmd::Camera { on, device }));
    }

    pub(super) fn decline_call(&mut self, room: RoomId, out: &mut Vec<Event>) {
        let Some(channel) = self.channel_of(&room) else { return };
        if self.ringing_me(&channel) {
            self.ringing.remove(&channel);
            self.net.send(Cmd::Request(Request::StopRinging {
                channel,
                me: self.me().to_owned(),
            }));
        }
        out.push(Event::AlertDone { room });
    }

    fn ringing_me(&self, channel: &str) -> bool {
        self.ringing
            .get(channel)
            .is_some_and(|users| users.iter().any(|u| u == self.me()))
    }

    /// READY: our gateway session, and everyone in voice.
    pub(super) fn voice_ready(&mut self, d: &Value, out: &mut Vec<Event>) {
        let session = d["session_id"].as_str().map(str::to_owned);
        // A new gateway session (not a resume) loses the old one's voice.
        if self.session_id.is_some()
            && self.session_id != session
            && let Some(call) = self.call.take()
        {
            self.net.send(Cmd::CallCmd(CallCmd::Leave));
            out.push(Event::CallEnded {
                room: call.room,
                reason: Some("Discord call: the connection to Discord was lost. Join again.".into()),
            });
        }
        self.session_id = session;
        self.ringing.clear();
        let changed = self.voice.ready(d);
        self.occupants_changed(changed, out);
    }

    /// After the store learns names from READY_SUPPLEMENTAL, publish its
    /// voice snapshot. Discord sends it without any additional request.
    pub(super) fn voice_supplemental(&mut self, d: &Value, out: &mut Vec<Event>) {
        let changed = self.voice.supplemental(d);
        self.occupants_changed(changed, out);
    }

    /// The voice parts of a gateway event; the rest of `dispatch` still runs.
    pub(super) fn voice_dispatch(&mut self, event: &str, d: &Value, out: &mut Vec<Event>) {
        if matches!(event, "PASSIVE_UPDATE_V1" | "PASSIVE_UPDATE_V2")
            && let Some(guild) = d["guild_id"].as_str()
        {
            let field = if event == "PASSIVE_UPDATE_V2" {
                "updated_members"
            } else {
                "members"
            };
            for member in d[field].as_array().into_iter().flatten() {
                self.store.put_member(guild, member);
            }
        }
        let changed = match event {
            "VOICE_STATE_UPDATE" => {
                let guild = d["guild_id"].as_str();
                if let Some(guild) = guild
                    && d["member"].is_object()
                {
                    let mut member = d["member"].clone();
                    member["user_id"] = d["user_id"].clone();
                    self.store.put_member(guild, &member);
                }
                let changed = self.voice.apply(guild, d);
                if d["user_id"].as_str() == Some(self.me()) {
                    self.own_voice_state(d, out);
                }
                changed
            }
            "VOICE_SERVER_UPDATE" => {
                self.voice_server(d);
                Changed::new()
            }
            "STREAM_CREATE" | "STREAM_UPDATE" | "STREAM_SERVER_UPDATE" => {
                if self.stream_viewers(d) {
                    self.send_streams(out);
                }
                self.stream_server(event, d);
                return;
            }
            "STREAM_DELETE" => {
                self.stream_deleted(d, out);
                return;
            }
            "CALL_CREATE" | "CALL_UPDATE" => {
                let mut changed = Changed::new();
                for state in d["voice_states"].as_array().into_iter().flatten() {
                    changed.extend(self.voice.apply(None, state));
                }
                self.call_ringing(d, out);
                changed
            }
            "CALL_DELETE" => {
                let Some(channel) = d["channel_id"].as_str() else {
                    return;
                };
                if self.ringing.remove(channel).is_some()
                    && let Some(room) = self.room_id(channel)
                {
                    out.push(Event::AlertDone { room });
                }
                self.voice.end_call(channel)
            }
            "GUILD_CREATE" => match d["id"].as_str() {
                Some(id) if d["unavailable"] != true => self.voice.replace_guild(id, &d["voice_states"]),
                _ => Changed::new(),
            },
            "GUILD_DELETE" => match d["id"].as_str() {
                Some(id) if d["unavailable"] != true => self.voice.replace_guild(id, &Value::Null),
                _ => Changed::new(),
            },
            // Large servers report voice in these until subscribed.
            "PASSIVE_UPDATE_V1" => match d["guild_id"].as_str() {
                Some(guild) => self.voice.replace_guild(guild, &d["voice_states"]),
                None => Changed::new(),
            },
            "PASSIVE_UPDATE_V2" => {
                let Some(guild) = d["guild_id"].as_str() else { return };
                let mut changed = Changed::new();
                for state in d["updated_voice_states"].as_array().into_iter().flatten() {
                    changed.extend(self.voice.apply(Some(guild), state));
                }
                for user in d["removed_voice_states"].as_array().into_iter().flatten() {
                    if let Some(user) = user.as_str() {
                        changed.extend(self.voice.remove(Some(guild), user));
                    }
                }
                changed
            }
            _ => return,
        };
        self.occupants_changed(changed, out);
    }

    /// Our own voice state: the session id the call connects with, or that
    /// we left the call's channel some other way.
    fn own_voice_state(&mut self, d: &Value, out: &mut Vec<Event>) {
        let Some(call) = self.call.as_mut() else { return };
        if d["guild_id"].as_str() != call.guild.as_deref() {
            return;
        }
        // Use the session from VOICE_STATE_UPDATE, as the voice handshake
        // requires. It must not be assumed equal to READY's gateway id.
        // Once established, a different voice session means another client.
        let ours = call
            .session
            .as_deref()
            .is_none_or(|id| d["session_id"].as_str() == Some(id));
        let reason = match d["channel_id"].as_str() {
            // Another of our clients joined voice: Discord moves voice there.
            Some(_) if !ours => "you joined voice on another device.",
            Some(channel) if channel == call.channel => {
                call.session = d["session_id"].as_str().map(str::to_owned);
                self.connect_call();
                // A stream's server may have come before our voice session.
                let keys: Vec<String> = self.call.iter().flat_map(|c| c.watching.keys().cloned()).collect();
                for key in keys {
                    self.connect_stream(&key);
                }
                return;
            }
            Some(_) => "Discord moved you to another channel. Join it there to carry on.",
            // A DM call's voice states carry no server: only ours count.
            None if ours => "you were disconnected from the call.",
            None => return,
        };
        let Some(call) = self.call.take() else { return };
        self.net.send(Cmd::CallCmd(CallCmd::Leave));
        if ours && d["channel_id"].is_string() {
            // Moved elsewhere: leave rather than sit there unheard.
            self.voice_state_update(call.guild.as_deref(), None, false, false, false);
        }
        out.push(Event::CallEnded {
            room: call.room,
            reason: Some(format!("Discord call: {reason}").into()),
        });
    }

    /// VOICE_SERVER_UPDATE for our call: where to connect, or where Discord
    /// moved it. The endpoint is null while Discord is still allocating one.
    fn voice_server(&mut self, d: &Value) {
        let Some(call) = self.call.as_mut() else { return };
        let ours = match &call.guild {
            Some(guild) => d["guild_id"].as_str() == Some(guild.as_str()),
            None => d["guild_id"].is_null() && d["channel_id"].as_str() == Some(call.channel.as_str()),
        };
        if let (true, Some(token), Some(endpoint)) = (ours, d["token"].as_str(), d["endpoint"].as_str()) {
            call.server = Some((token.to_owned(), endpoint.to_owned()));
            self.connect_call();
        }
    }

    /// Hands the voice server to the call once both halves are known, and
    /// again whenever it changes.
    fn connect_call(&mut self) {
        let me = self.me().to_owned();
        let Some(call) = self.call.as_mut() else { return };
        let (Some(session), Some((token, endpoint))) = (&call.session, &call.server) else {
            return;
        };
        let server = VoiceServer {
            endpoint: endpoint.clone(),
            token: token.clone(),
            session_id: session.clone(),
            user_id: me,
            // A call identifies with its channel where a server's would its server.
            server_id: call.guild.clone().unwrap_or_else(|| call.channel.clone()),
            channel_id: call.channel.clone(),
        };
        if call.sent.as_ref() != Some(&server) {
            call.sent = Some(server.clone());
            self.net.send(Cmd::CallCmd(CallCmd::Server(server)));
        }
    }

    /// CALL_CREATE/UPDATE's `ringing`: a call ringing us starts or stops.
    fn call_ringing(&mut self, d: &Value, out: &mut Vec<Event>) {
        let Some(channel) = d["channel_id"].as_str() else {
            return;
        };
        let was = self.ringing_me(channel);
        let ringing: Vec<String> = d["ringing"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|u| u.as_str().map(str::to_owned))
            .collect();
        self.ringing.insert(channel.to_owned(), ringing);
        let now = self.ringing_me(channel) && self.call.as_ref().is_none_or(|c| c.channel != channel);
        let Some(room) = self.room_id(channel) else { return };
        if now && !was {
            let caller = self
                .voice
                .in_channel(channel)
                .first()
                .map(|s| self.store.display_name(&s.user, None));
            let title = caller.unwrap_or_else(|| self.room(channel).map(|r| r.name.to_string()).unwrap_or_default());
            out.push(Event::Alert {
                from: None,
                room,
                kind: AlertKind::Call,
                title: title.into(),
                body: "Calling you on Discord".into(),
            });
        } else if was && !now {
            out.push(Event::AlertDone { room });
        }
    }

    /// From the call's media.
    pub(super) fn on_call(&mut self, tag: u64, event: CallEvent, out: &mut Vec<Event>) {
        let Some(call) = self.call.as_mut().filter(|c| c.tag == tag) else {
            return;
        };
        match event {
            CallEvent::Connected => {
                call.connected = true;
                out.push(Event::CallConnected {
                    room: call.room.clone(),
                });
                // Starting a DM call rings the others, once Discord has the
                // call (it has by now: it gave us a voice server).
                if std::mem::take(&mut call.ring) {
                    let channel = call.channel.clone();
                    let stranger = self.store.channels.get(&channel).is_some_and(|c| {
                        c.kind == store::DM
                            && c.recipients
                                .first()
                                .is_none_or(|u| self.store.relationships.get(u) != Some(&store::FRIEND))
                    });
                    self.net.send(Cmd::Request(Request::Ring { channel, stranger }));
                }
                self.send_members(out);
                self.send_streams(out);
            }
            CallEvent::Latency(milliseconds) => out.push(Event::CallLatency {
                room: call.room.clone(),
                milliseconds: Some(milliseconds),
            }),
            CallEvent::Speaking { me, users } => {
                call.speaking = (me, users);
                self.send_members(out);
            }
            CallEvent::Cameras(users) => {
                call.cameras = users;
                self.send_videos(out);
            }
            CallEvent::ScreenPicked => self.screen_picked(),
            CallEvent::OwnStream { active, error } => {
                let room = call.room.clone();
                if !active && let Some(live) = call.live.take().filter(|l| l.announced) {
                    // It ended on our side: Discord still counts it as live.
                    self.net.send(Cmd::Gateway {
                        op: crate::gateway::STREAM_DELETE,
                        data: json!({"stream_key": live.key}),
                    });
                }
                if let Some(error) = error {
                    out.push(Event::Error(format!("Discord stream: {error}").into()));
                }
                out.push(Event::CallScreenShare { room, active });
            }
            CallEvent::OwnCamera { active, error } => {
                let stopped = call.camera_on && !active;
                call.camera_on = active;
                let room = call.room.clone();
                if stopped {
                    // It stopped on its own: others must stop seeing it as on.
                    let (guild, channel, muted, deafened) =
                        (call.guild.clone(), call.channel.clone(), call.muted, call.deafened);
                    self.voice_state_update(guild.as_deref(), Some(&channel), muted, deafened, false);
                }
                if let Some(error) = error {
                    out.push(Event::Error(format!("Discord camera: {error}").into()));
                }
                out.push(Event::CallCamera { room, active });
            }
            CallEvent::StreamEnded { key, reason } => {
                if let Some(watched) = call.watching.remove(&key) {
                    // Discord still counts us as watching a stream whose
                    // connection ended on our side.
                    self.net.send(Cmd::Gateway {
                        op: crate::gateway::STREAM_DELETE,
                        data: json!({"stream_key": key}),
                    });
                    if let Some(reason) = reason {
                        call.stream_notes
                            .insert(watched.user, stream::shown_reason(&reason).into());
                        out.push(Event::Error(format!("Discord stream: {reason}").into()));
                    }
                    self.send_streams(out);
                    self.send_videos(out);
                }
            }
            // Only wakes the UI to draw the frame.
            CallEvent::Frame => {}
            CallEvent::Ended(reason) => {
                let Some(call) = self.call.take() else { return };
                self.stop_watching_all(&call);
                // Out of voice, unless another of our clients has it now.
                let here = self
                    .voice
                    .of(call.guild.as_deref(), self.me())
                    .is_some_and(|s| s.session.is_none() || s.session == call.session);
                if here {
                    self.voice_state_update(call.guild.as_deref(), None, false, false, false);
                }
                out.push(Event::CallEnded {
                    room: call.room,
                    reason: reason.map(|r| format!("Discord call: {r}").into()),
                });
            }
        }
    }

    /// The people in `channel`'s voice, as the core shows them.
    fn members_of(&self, channel: &str) -> Vec<CallMember> {
        let Some(account) = self.account.clone() else {
            return Vec::new();
        };
        let guild = self.store.channels.get(channel).and_then(|c| c.guild.clone());
        let speaking = self.call.as_ref().filter(|c| c.channel == channel).map(|c| &c.speaking);
        let me = self.me();
        self.voice
            .in_channel(channel)
            .into_iter()
            .map(|s| CallMember {
                user: godwit_core::ids::UserId::new(account.clone(), s.user.as_str()),
                name: self.store.display_name(&s.user, guild.as_deref()).into(),
                avatar_url: Some(self.avatar(&s.user, guild.as_deref()).into()),
                speaking: speaking.is_some_and(
                    |(us, others)| {
                        if s.user == me { *us } else { others.contains(&s.user) }
                    },
                ),
                muted: s.muted(),
                deafened: s.deafened(),
            })
            .collect()
    }

    /// Starts or stops watching `user`'s stream in our call, as the official
    /// client's "Watch Stream": op 20, and op 19 to stop. Watching needs
    /// being in the call, as there.
    pub(super) fn watch_call_stream(&mut self, room: &RoomId, user: &UserId, watch: bool, out: &mut Vec<Event>) {
        let me = self.me().to_owned();
        let Some(call) = self.call.as_mut().filter(|c| c.room == *room && c.connected) else {
            return;
        };
        let user = user.raw.to_string();
        let key = stream::key(call.guild.as_deref(), &call.channel, &user);
        if watch {
            let streaming = self
                .voice
                .in_channel(&call.channel)
                .iter()
                .any(|s| s.user == user && s.self_stream);
            if user == me || !streaming || call.watching.contains_key(&key) {
                return;
            }
            call.stream_notes.remove(&user);
            call.watching.insert(
                key.clone(),
                Watched {
                    user,
                    opening: Opening::default(),
                    sent: None,
                    viewers: None,
                },
            );
            self.net.send(Cmd::Gateway {
                op: crate::gateway::STREAM_WATCH,
                data: json!({"stream_key": key}),
            });
        } else {
            if call.watching.remove(&key).is_none() {
                return;
            }
            self.net.send(Cmd::CallCmd(CallCmd::Unwatch(key.clone())));
            self.net.send(Cmd::Gateway {
                op: crate::gateway::STREAM_DELETE,
                data: json!({"stream_key": key}),
            });
        }
        self.send_streams(out);
        self.send_videos(out);
    }

    /// Ends every stream watched in `call`, as leaving it does.
    fn stop_watching_all(&self, call: &OurCall) {
        for key in call.watching.keys() {
            self.net.send(Cmd::Gateway {
                op: crate::gateway::STREAM_DELETE,
                data: json!({"stream_key": key}),
            });
        }
    }

    /// Our screen as a Go Live stream in the call (op 18, as the official
    /// client's "Share Your Screen"), or ended (op 19). Discord answers with
    /// STREAM_CREATE and STREAM_SERVER_UPDATE: where to send it.
    pub(super) fn set_call_live(&mut self, on: bool, out: &mut Vec<Event>) {
        let me = self.me().to_owned();
        let Some(call) = self.call.as_mut().filter(|c| c.connected) else {
            if let Some(room) = self.call.as_ref().map(|c| c.room.clone()) {
                out.push(Event::CallScreenShare { room, active: false });
            }
            return;
        };
        if call.live.is_some() == on {
            return;
        }
        let key = stream::key(call.guild.as_deref(), &call.channel, &me);
        if on {
            // The screen first (the system's picker), as Yapfast: Discord
            // hears of the stream at `screen_picked`, so a cancelled picker
            // shares nothing.
            self.net.send(Cmd::CallCmd(CallCmd::PrepareLive));
            call.live = Some(Live {
                key,
                opening: Opening::default(),
                sent: None,
                announced: false,
            });
        } else {
            if call.live.take().is_some_and(|l| l.announced) {
                self.net.send(Cmd::Gateway {
                    op: crate::gateway::STREAM_DELETE,
                    data: json!({"stream_key": key}),
                });
            }
            self.net.send(Cmd::CallCmd(CallCmd::GoLive(None)));
        }
    }

    /// The screen for our stream was chosen and is encoding: go live (op
    /// 18, as the official client's "Share Your Screen"), and unpause it
    /// straight away, as Yapfast and Discord-video-stream do: viewers of a
    /// paused stream get no video.
    fn screen_picked(&mut self) {
        let Some(call) = self.call.as_mut() else { return };
        let Some(live) = call.live.as_mut().filter(|l| !l.announced) else {
            return;
        };
        live.announced = true;
        let key = live.key.clone();
        let guild = call.guild.clone();
        eprintln!(
            "note: discord: going live: {}",
            self.store.tier_tripwire(guild.as_deref())
        );
        let Some(call) = self.call.as_mut() else { return };
        self.net.send(Cmd::Gateway {
            op: crate::gateway::STREAM_CREATE,
            data: json!({
                "type": if call.guild.is_some() { "guild" } else { "call" },
                "guild_id": call.guild,
                "channel_id": call.channel,
                "preferred_region": null,
            }),
        });
        self.net.send(Cmd::Gateway {
            op: crate::gateway::STREAM_SET_PAUSED,
            data: json!({"stream_key": key, "paused": false}),
        });
    }

    /// STREAM_CREATE/UPDATE (the stream's RTC server) and
    /// STREAM_SERVER_UPDATE (its endpoint and token) for a stream we watch:
    /// where to connect, or where Discord moved it.
    /// Takes a watched stream's `viewer_ids`; true when they changed.
    fn stream_viewers(&mut self, d: &Value) -> bool {
        let (Some(key), Some(ids)) = (d["stream_key"].as_str(), d["viewer_ids"].as_array()) else {
            return false;
        };
        let Some(watched) = self.call.as_mut().and_then(|c| c.watching.get_mut(key)) else {
            return false;
        };
        let ids: Vec<String> = ids.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect();
        if watched.viewers.as_ref() == Some(&ids) {
            return false;
        }
        watched.viewers = Some(ids);
        true
    }

    fn stream_server(&mut self, event: &str, d: &Value) {
        let Some(key) = d["stream_key"].as_str() else { return };
        let me = self.me().to_owned();
        if let Some(call) = self.call.as_mut()
            && let Some(session) = call.session.clone()
            && let Some(live) = call.live.as_mut().filter(|l| l.key == key)
        {
            live.opening.apply(event, d);
            if let Some(server) = live.opening.server(key, &me, &session)
                && live.sent.as_ref() != Some(&server)
            {
                live.sent = Some(server.clone());
                self.net.send(Cmd::CallCmd(CallCmd::GoLive(Some(server))));
            }
            return;
        }
        let Some(watched) = self.call.as_mut().and_then(|c| c.watching.get_mut(key)) else {
            return;
        };
        watched.opening.apply(event, d);
        let key = key.to_owned();
        self.connect_stream(&key);
    }

    /// Hands a watched stream's server to the media once it and our voice
    /// session are known, and again whenever it changes.
    fn connect_stream(&mut self, key: &str) {
        let me = self.me().to_owned();
        let Some(call) = self.call.as_mut() else { return };
        let Some(session) = call.session.clone() else { return };
        let Some(watched) = call.watching.get_mut(key) else {
            return;
        };
        let Some(server) = watched.opening.server(key, &me, &session) else {
            return;
        };
        if watched.sent.as_ref() != Some(&server) {
            watched.sent = Some(server.clone());
            self.net.send(Cmd::CallCmd(CallCmd::Watch(server)));
        }
    }

    /// STREAM_DELETE: a stream we watch ended (the streamer stopped, or
    /// Discord refused us), or ours.
    fn stream_deleted(&mut self, d: &Value, out: &mut Vec<Event>) {
        let Some(key) = d["stream_key"].as_str() else { return };
        let Some(call) = self.call.as_mut() else { return };
        // Our own stream, ended by Discord.
        if call.live.as_ref().is_some_and(|l| l.key == key) {
            call.live = None;
            let room = call.room.clone();
            self.net.send(Cmd::CallCmd(CallCmd::GoLive(None)));
            if let Some(reason) = stream::ended_because(d) {
                out.push(Event::Error(format!("Discord stream: {reason}").into()));
            }
            out.push(Event::CallScreenShare { room, active: false });
            return;
        }
        let Some(watched) = call.watching.remove(key) else {
            return;
        };
        self.net.send(Cmd::CallCmd(CallCmd::Unwatch(key.to_owned())));
        if let Some(reason) = stream::ended_because(d) {
            call.stream_notes.insert(watched.user, reason.into());
            out.push(Event::Error(format!("Discord stream: {reason}").into()));
        }
        self.send_streams(out);
        self.send_videos(out);
    }

    /// Tells the media whether the call is on screen: its video is asked
    /// for and decoded only then, as LiveKit calls watch only while shown.
    pub(super) fn call_visibility(&self) {
        if self.call.is_some() {
            self.net.send(Cmd::CallCmd(CallCmd::Visible(self.call_shown)));
        }
    }

    /// Who streams in our call, and which of them we watch.
    fn streams_of(&self, call: &OurCall) -> Vec<CallStream> {
        let Some(account) = self.account.clone() else {
            return Vec::new();
        };
        self.voice
            .in_channel(&call.channel)
            .into_iter()
            .filter(|s| s.self_stream && s.user != self.me())
            .map(|s| CallStream {
                user: UserId::new(account.clone(), s.user.as_str()),
                watching: call.watching.values().any(|w| w.user == s.user),
                // Only a stream's viewers hear who watches it (viewer_ids).
                viewers: call.watching.values().find(|w| w.user == s.user).and_then(|w| {
                    w.viewers
                        .as_ref()
                        .map(|ids| ids.iter().map(|id| UserId::new(account.clone(), id.as_str())).collect())
                }),
                note: call.stream_notes.get(&s.user).cloned(),
            })
            .collect()
    }

    fn send_streams(&self, out: &mut Vec<Event>) {
        if let Some(call) = &self.call {
            out.push(Event::CallStreams {
                room: call.room.clone(),
                streams: self.streams_of(call),
            });
        }
    }

    /// The call's video tiles: everyone's cameras, then the streams watched.
    fn send_videos(&self, out: &mut Vec<Event>) {
        let (Some(call), Some(account)) = (&self.call, self.account.clone()) else {
            return;
        };
        let parse = |user: &str| user.parse::<u64>().ok();
        let cameras = call.cameras.iter().filter_map(|user| {
            Some(CallVideo {
                sid: sid(FeedKind::Camera, parse(user)?),
                user: UserId::new(account.clone(), user.as_str()),
                screen_share: false,
            })
        });
        let streams = call.watching.values().filter_map(|watched| {
            Some(CallVideo {
                sid: sid(FeedKind::Screen, parse(&watched.user)?),
                user: UserId::new(account.clone(), watched.user.as_str()),
                screen_share: true,
            })
        });
        out.push(Event::CallVideos {
            room: call.room.clone(),
            videos: cameras.chain(streams).collect(),
        });
    }

    fn send_members(&self, out: &mut Vec<Event>) {
        if let Some(call) = &self.call {
            out.push(Event::CallMembers {
                room: call.room.clone(),
                members: self.members_of(&call.channel),
            });
        }
    }

    /// Who's in each changed channel, as seen from outside it, and our
    /// call's members if it's among them.
    fn occupants_changed(&mut self, changed: Changed, out: &mut Vec<Event>) {
        // A note about someone's stream lasts until they stop streaming.
        if let Some(call) = self.call.as_mut().filter(|c| changed.contains(&c.channel)) {
            let streaming: Vec<String> = self
                .voice
                .in_channel(&call.channel)
                .into_iter()
                .filter(|s| s.self_stream)
                .map(|s| s.user.clone())
                .collect();
            call.stream_notes.retain(|user, _| streaming.contains(user));
        }
        for channel in &changed {
            if let Some(room) = self.room_id(channel) {
                out.push(Event::VoiceOccupants {
                    room,
                    members: self.members_of(channel),
                });
            }
        }
        if self.call.as_ref().is_some_and(|c| changed.contains(&c.channel)) {
            self.send_members(out);
            self.send_streams(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use godwit_core::account::NetworkAccount;
    use godwit_core::application::Command;
    use serde_json::json;

    use super::*;
    use crate::account::Status;
    use crate::backend::{Ev, Handle};

    fn account() -> (DiscordAccount, tokio::sync::mpsc::UnboundedReceiver<Cmd>, Vec<Event>) {
        let (net, cmds) = Handle::detached();
        let (_, events) = mpsc::channel();
        let mut a = DiscordAccount::with(u32::MAX, net, events, Status::Connecting);
        let mut ready = store::test_ready();
        ready["session_id"] = json!("ours");
        // A voice channel everyone may connect to, with someone in it.
        ready["guilds"][0]["channels"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "15", "type": 2, "name": "Lounge", "position": 2, "parent_id": "12"}));
        let everyone = crate::permissions::VIEW_CHANNEL
            | crate::permissions::SEND_MESSAGES
            | crate::permissions::CONNECT
            | crate::permissions::STREAM;
        ready["guilds"][0]["roles"][0]["permissions"] = json!(everyone.to_string());
        ready["guilds"][0]["voice_states"] = json!([{"user_id": "200", "channel_id": "15", "self_mute": true}]);
        let mut out = Vec::new();
        a.handle(Ev::Ready(Box::new(ready)), &mut out);
        (a, cmds, out)
    }

    fn sent(cmds: &mut tokio::sync::mpsc::UnboundedReceiver<Cmd>) -> Vec<Cmd> {
        std::iter::from_fn(|| cmds.try_recv().ok()).collect()
    }

    fn room(a: &DiscordAccount, channel: &str) -> RoomId {
        a.room_id(channel).unwrap()
    }

    fn dispatch(a: &mut DiscordAccount, event: &str, d: Value) -> Vec<Event> {
        let mut out = Vec::new();
        a.handle(Ev::Dispatch(event.into(), d), &mut out);
        out
    }

    fn op4(cmd: &Cmd) -> Option<&Value> {
        match cmd {
            Cmd::Gateway { op: 4, data } => Some(data),
            _ => None,
        }
    }

    #[test]
    fn voice_channels_and_dms_can_call_and_show_whos_in_them() {
        let (a, _, out) = account();
        let occupants = out.iter().find_map(|e| match e {
            Event::VoiceOccupants { room, members } if &*room.raw == "15" => Some(members),
            _ => None,
        });
        let members = occupants.expect("who's in the voice channel");
        assert_eq!(members.len(), 1);
        assert!(members[0].muted && &*members[0].name == "zarg");
        assert!(a.room("15").unwrap().capabilities.voice);
        assert!(a.room("20").unwrap().capabilities.voice, "a DM call");
        assert!(!a.room("13").unwrap().capabilities.voice, "a text channel");
    }

    #[test]
    fn supplemental_restores_every_participant_and_capacity_without_requests() {
        let (mut a, mut cmds, _) = account();
        sent(&mut cmds);
        dispatch(
            &mut a,
            "CHANNEL_UPDATE",
            json!({
                "id":"15", "guild_id":"10", "type":2, "name":"Lounge", "user_limit":20
            }),
        );
        let states: Vec<Value> = (300..315)
            .map(|id| json!({"user_id":id.to_string(),"channel_id":"15"}))
            .collect();
        let members: Vec<Value> = (300..315)
            .map(|id| json!({"user_id":id.to_string(),"nick":format!("Guest {id}")}))
            .collect();
        let out = dispatch(
            &mut a,
            "READY_SUPPLEMENTAL",
            json!({
                "guilds":[{"id":"10", "voice_states":states}], "merged_members":[members]
            }),
        );
        assert_eq!(a.room("15").unwrap().capabilities.voice_user_limit, Some(20));
        assert!(a.room("15").unwrap().capabilities.video, "a voice channel allows video");
        let participants = out
            .iter()
            .find_map(|event| match event {
                Event::VoiceOccupants { room, members } if &*room.raw == "15" => Some(members),
                _ => None,
            })
            .unwrap();
        assert_eq!(participants.len(), 15);
        assert!(participants.iter().all(|p| p.name.starts_with("Guest ")));
        assert!(sent(&mut cmds).is_empty(), "no REST lookups or gateway requests");
        let out = dispatch(
            &mut a,
            "VOICE_STATE_UPDATE",
            json!({"guild_id":"10","user_id":"300","channel_id":null}),
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::VoiceOccupants { members, .. } if members.len() == 14))
        );
        dispatch(
            &mut a,
            "CHANNEL_UPDATE",
            json!({"id":"15","guild_id":"10","type":2,"name":"Lounge","user_limit":0}),
        );
        assert_eq!(a.room("15").unwrap().capabilities.voice_user_limit, Some(0));
        // READY and updates leave a default limit out: still a voice channel, unlimited.
        dispatch(
            &mut a,
            "CHANNEL_UPDATE",
            json!({"id":"15","guild_id":"10","type":2,"name":"Lounge"}),
        );
        assert_eq!(a.room("15").unwrap().capabilities.voice_user_limit, Some(0));
    }

    #[test]
    fn joining_uses_the_voice_session_not_the_gateway_session() {
        let (mut a, mut cmds, _) = account();
        a.run(Command::JoinCall { room: room(&a, "15") });
        sent(&mut cmds);
        dispatch(
            &mut a,
            "VOICE_SERVER_UPDATE",
            json!({"guild_id":"10","token":"test","endpoint":"voice.invalid"}),
        );
        let out = dispatch(
            &mut a,
            "VOICE_STATE_UPDATE",
            json!({
                "guild_id":"10","user_id":"100","channel_id":"15","session_id":"voice-session"
            }),
        );
        assert!(!out.iter().any(|e| matches!(e, Event::CallEnded { .. })));
        assert!(
            sent(&mut cmds)
                .iter()
                .any(|cmd| matches!(cmd, Cmd::CallCmd(CallCmd::Server(s)) if s.session_id == "voice-session"))
        );
        let tag = a.call.as_ref().unwrap().tag;
        a.on_call(tag, CallEvent::Ended(Some("test failure".into())), &mut Vec::new());
        assert!(
            sent(&mut cmds)
                .iter()
                .any(|cmd| op4(cmd).is_some_and(|d| d["channel_id"].is_null())),
            "a failed call leaves its own voice session"
        );
    }

    #[test]
    fn a_newly_available_guild_publishes_its_voice_participants_after_rooms() {
        let (mut a, mut cmds, _) = account();
        sent(&mut cmds);
        let out = dispatch(
            &mut a,
            "GUILD_CREATE",
            json!({
                "id":"20", "name":"New guild", "owner_id":"100",
                "roles":[{"id":"20", "permissions":(crate::permissions::VIEW_CHANNEL | crate::permissions::CONNECT).to_string()}],
                "channels":[{"id":"25","type":2,"name":"Lounge","user_limit":4}],
                "members":[{"user":{"id":"800","username":"guest"},"nick":"New participant"}],
                "voice_states":[{"user_id":"800","channel_id":"25"}]
            }),
        );
        assert!(a.room("25").is_some());
        assert!(
            out.iter()
                .any(|event| matches!(event, Event::VoiceOccupants { room, members }
            if &*room.raw == "25" && members.len() == 1 && &*members[0].name == "New participant"))
        );
        assert!(sent(&mut cmds).is_empty());
    }

    #[test]
    fn joining_connects_once_discord_names_the_session_and_server() {
        let (mut a, mut cmds, _) = account();
        let lounge = room(&a, "15");
        a.run(Command::JoinCall { room: lounge.clone() });
        let first = sent(&mut cmds);
        let join = first.iter().find_map(op4).expect("op 4");
        assert_eq!(join["guild_id"], "10");
        assert_eq!(join["channel_id"], "15");
        assert!(first.iter().any(|c| matches!(c, Cmd::Call { muted: false, .. })));

        // Both the voice state and server are required.
        dispatch(
            &mut a,
            "VOICE_SERVER_UPDATE",
            json!({"guild_id": "10", "token": "tok", "endpoint": "c-ams.discord.media:443"}),
        );
        assert!(sent(&mut cmds).is_empty(), "waits for our voice state");
        let out = dispatch(
            &mut a,
            "VOICE_STATE_UPDATE",
            json!({"guild_id": "10", "user_id": "100", "channel_id": "15", "session_id": "ours"}),
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::VoiceOccupants { members, .. } if members.len() == 2))
        );
        let Some(Cmd::CallCmd(CallCmd::Server(server))) = sent(&mut cmds).pop() else {
            panic!("the call connects")
        };
        assert_eq!(
            (
                server.server_id.as_str(),
                server.channel_id.as_str(),
                server.session_id.as_str()
            ),
            ("10", "15", "ours")
        );
        assert_eq!(server.endpoint, "c-ams.discord.media:443");

        // Discord moves the call to another voice server.
        dispatch(
            &mut a,
            "VOICE_SERVER_UPDATE",
            json!({"guild_id": "10", "token": "tok2", "endpoint": "c-fra.discord.media:443"}),
        );
        assert!(matches!(sent(&mut cmds).as_slice(), [Cmd::CallCmd(CallCmd::Server(s))] if s.token == "tok2"));

        let tag = a.call.as_ref().unwrap().tag;
        let mut out = Vec::new();
        a.on_call(tag, CallEvent::Connected, &mut out);
        assert!(matches!(&out[0], Event::CallConnected { room } if *room == lounge));
        let mut out = Vec::new();
        a.on_call(
            tag,
            CallEvent::Speaking {
                me: true,
                users: vec![],
            },
            &mut out,
        );
        let Some(Event::CallMembers { members, .. }) = out.pop() else {
            panic!("members")
        };
        assert!(members.iter().any(|m| &*m.user.raw == "100" && m.speaking));
        assert!(members.iter().any(|m| &*m.user.raw == "200" && !m.speaking));

        // Muting shows to others through op 4, and stops the microphone.
        a.run(Command::SetMuted(true));
        let muted = sent(&mut cmds);
        assert!(muted.iter().any(|c| matches!(c, Cmd::CallCmd(CallCmd::Muted(true)))));
        assert_eq!(muted.iter().find_map(op4).unwrap()["self_mute"], true);
        a.run(Command::SetDeafened(true));
        let deaf = sent(&mut cmds);
        let state = deaf.iter().find_map(op4).unwrap();
        assert_eq!(
            (state["self_mute"].clone(), state["self_deaf"].clone()),
            (json!(true), json!(true))
        );

        // The camera: op 4 says self_video, then the call's media starts it;
        // if it can't, others stop seeing it as on and the core hears why.
        a.run(Command::SetCamera(true));
        let on = sent(&mut cmds);
        assert_eq!(on.iter().find_map(op4).unwrap()["self_video"], true);
        assert!(
            on.iter()
                .any(|c| matches!(c, Cmd::CallCmd(CallCmd::Camera { on: true, .. })))
        );
        let mut out = Vec::new();
        a.on_call(
            tag,
            CallEvent::OwnCamera {
                active: false,
                error: Some("No camera found.".into()),
            },
            &mut out,
        );
        assert!(out.iter().any(|e| matches!(e, Event::CallCamera { active: false, .. })));
        assert_eq!(sent(&mut cmds).iter().find_map(op4).unwrap()["self_video"], false);

        let out = a.run(Command::LeaveCall);
        assert!(matches!(&out[..], [Event::CallEnded { reason: None, .. }]));
        let left = sent(&mut cmds);
        assert!(left.iter().any(|c| matches!(c, Cmd::CallCmd(CallCmd::Leave))));
        assert!(left.iter().find_map(op4).unwrap()["channel_id"].is_null());
    }

    fn gateway_op(cmds: &[Cmd], wanted: u64) -> Option<&Value> {
        cmds.iter().find_map(|c| match c {
            Cmd::Gateway { op, data } if *op == wanted => Some(data),
            _ => None,
        })
    }

    #[test]
    fn go_live_streams_are_listed_watched_and_shown() {
        let (mut a, mut cmds, _) = account();
        let lounge = room(&a, "15");
        a.run(Command::Focus {
            room: Some(lounge.clone()),
        });
        // The app says the call's video is on screen.
        a.run(Command::SetCallShown(true));
        a.run(Command::JoinCall { room: lounge.clone() });
        dispatch(
            &mut a,
            "VOICE_STATE_UPDATE",
            json!({"guild_id": "10", "user_id": "100", "channel_id": "15", "session_id": "ours"}),
        );
        let tag = a.call.as_ref().unwrap().tag;
        a.on_call(tag, CallEvent::Connected, &mut Vec::new());
        let joined = sent(&mut cmds);
        assert!(
            joined.iter().any(|c| matches!(c, Cmd::CallCmd(CallCmd::Visible(true)))),
            "the call is on screen"
        );

        // Zarg goes live.
        let out = dispatch(
            &mut a,
            "VOICE_STATE_UPDATE",
            json!({"guild_id": "10", "user_id": "200", "channel_id": "15", "self_stream": true}),
        );
        let streams = out.iter().find_map(|e| match e {
            Event::CallStreams { streams, .. } => Some(streams.clone()),
            _ => None,
        });
        let streams = streams.expect("who streams");
        assert_eq!(streams.len(), 1);
        assert!(&*streams[0].user.raw == "200" && !streams[0].watching);

        // Watching asks Discord, then connects where it says.
        let zarg = streams[0].user.clone();
        let out = a.watch_stream(&lounge, &zarg, true);
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::CallStreams { streams, .. } if streams[0].watching))
        );
        assert!(out.iter().any(|e| matches!(
            e,
            Event::CallVideos { videos, .. } if videos.len() == 1 && videos[0].screen_share && videos[0].sid == "discord:stream:200"
        )));
        let asked = sent(&mut cmds);
        assert_eq!(gateway_op(&asked, 20).unwrap()["stream_key"], "guild:10:15:200");
        dispatch(
            &mut a,
            "STREAM_CREATE",
            json!({"stream_key": "guild:10:15:200", "rtc_server_id": "900"}),
        );
        assert!(sent(&mut cmds).is_empty(), "waits for the endpoint");
        dispatch(
            &mut a,
            "STREAM_SERVER_UPDATE",
            json!({"stream_key": "guild:10:15:200", "token": "st", "endpoint": "c-ams.discord.media:443"}),
        );
        let Some(Cmd::CallCmd(CallCmd::Watch(server))) = sent(&mut cmds).pop() else {
            panic!("the stream connects")
        };
        assert_eq!(
            (
                server.rtc_server_id.as_str(),
                server.session_id.as_str(),
                server.user_id.as_str()
            ),
            ("900", "ours", "100")
        );

        // Who watches comes with STREAM_UPDATE's viewer_ids, us included.
        let out = dispatch(
            &mut a,
            "STREAM_UPDATE",
            json!({"stream_key": "guild:10:15:200", "viewer_ids": ["100", "300"]}),
        );
        let viewers = out.iter().find_map(|e| match e {
            Event::CallStreams { streams, .. } => streams[0].viewers.clone(),
            _ => None,
        });
        let viewers: Vec<String> = viewers.expect("viewers").iter().map(|u| u.raw.to_string()).collect();
        assert_eq!(viewers, ["100", "300"]);
        // The same list again sends nothing new.
        let again = dispatch(
            &mut a,
            "STREAM_UPDATE",
            json!({"stream_key": "guild:10:15:200", "viewer_ids": ["100", "300"]}),
        );
        assert!(!again.iter().any(|e| matches!(e, Event::CallStreams { .. })));

        // Cameras come in from the media, beside the stream.
        let mut out = Vec::new();
        a.on_call(tag, CallEvent::Cameras(vec!["200".into()]), &mut out);
        let Some(Event::CallVideos { videos, .. }) = out.pop() else {
            panic!("videos")
        };
        assert_eq!(
            videos
                .iter()
                .map(|v| (v.sid.as_str(), v.screen_share))
                .collect::<Vec<_>>(),
            [("discord:camera:200", false), ("discord:stream:200", true)]
        );

        // Off screen (no room nor pop-out shows it), video stops.
        a.run(Command::SetCallShown(false));
        assert!(matches!(
            sent(&mut cmds).as_slice(),
            [Cmd::CallCmd(CallCmd::Visible(false))]
        ));

        // Discord ends the stream: it goes, saying why.
        let out = dispatch(
            &mut a,
            "STREAM_DELETE",
            json!({"stream_key": "guild:10:15:200", "reason": "stream_full"}),
        );
        assert!(out.iter().any(|e| matches!(e, Event::Error(r) if r.contains("full"))));
        assert!(matches!(sent(&mut cmds).as_slice(), [Cmd::CallCmd(CallCmd::Unwatch(k))] if k == "guild:10:15:200"));

        // Watching again, then stopping: op 19, and the media lets it go.
        a.watch_stream(&lounge, &zarg, true);
        sent(&mut cmds);
        a.watch_stream(&lounge, &zarg, false);
        let stopped = sent(&mut cmds);
        assert!(stopped.iter().any(|c| matches!(c, Cmd::CallCmd(CallCmd::Unwatch(_)))));
        assert_eq!(gateway_op(&stopped, 19).unwrap()["stream_key"], "guild:10:15:200");
        // Nobody else's stream can be watched, nor our own.
        assert!(
            a.watch_stream(&lounge, &UserId::new(zarg.account.clone(), "100"), true)
                .is_empty()
        );

        // Leaving ends what's watched.
        a.watch_stream(&lounge, &zarg, true);
        sent(&mut cmds);
        a.run(Command::LeaveCall);
        assert!(gateway_op(&sent(&mut cmds), 19).is_some());
    }

    #[test]
    fn joining_on_another_device_ends_ours_without_kicking_it() {
        let (mut a, mut cmds, _) = account();
        a.run(Command::JoinCall { room: room(&a, "15") });
        dispatch(
            &mut a,
            "VOICE_STATE_UPDATE",
            json!({"guild_id":"10", "user_id":"100", "channel_id":"15", "session_id":"ours"}),
        );
        sent(&mut cmds);
        let out = dispatch(
            &mut a,
            "VOICE_STATE_UPDATE",
            json!({"guild_id": "10", "user_id": "100", "channel_id": "15", "session_id": "phone"}),
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::CallEnded { reason: Some(r), .. } if r.contains("another device")))
        );
        let after = sent(&mut cmds);
        assert!(after.iter().any(|c| matches!(c, Cmd::CallCmd(CallCmd::Leave))));
        assert!(after.iter().find_map(op4).is_none(), "the phone keeps its call");
    }

    #[test]
    fn a_failed_call_says_why_and_leaves_voice() {
        let (mut a, mut cmds, _) = account();
        a.run(Command::JoinCall { room: room(&a, "15") });
        dispatch(
            &mut a,
            "VOICE_STATE_UPDATE",
            json!({"guild_id": "10", "user_id": "100", "channel_id": "15", "session_id": "ours"}),
        );
        sent(&mut cmds);
        let tag = a.call.as_ref().unwrap().tag;
        let mut out = Vec::new();
        a.on_call(
            tag,
            CallEvent::Ended(Some("the voice server didn't answer".into())),
            &mut out,
        );
        assert!(matches!(&out[..], [Event::CallEnded { reason: Some(r), .. }] if r.starts_with("Discord call: ")));
        assert!(sent(&mut cmds).iter().find_map(op4).unwrap()["channel_id"].is_null());
        // A stale call's events are ignored.
        let mut out = Vec::new();
        a.on_call(tag, CallEvent::Connected, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn dm_calls_ring_both_ways() {
        let (mut a, mut cmds, _) = account();
        let dm = room(&a, "20");
        // Their call rings us.
        let out = dispatch(
            &mut a,
            "CALL_CREATE",
            json!({"channel_id": "20", "ringing": ["100"], "voice_states": [{"user_id": "200", "channel_id": "20"}]}),
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::Alert { kind: AlertKind::Call, title, .. } if &**title == "zarg"))
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::VoiceOccupants { room, .. } if *room == dm))
        );
        let out = a.run(Command::DeclineCall { room: dm.clone() });
        assert!(matches!(&out[..], [Event::AlertDone { .. }]));
        assert!(sent(&mut cmds).iter().any(
            |c| matches!(c, Cmd::Request(Request::StopRinging { channel, me }) if channel == "20" && me == "100")
        ));
        let out = dispatch(&mut a, "CALL_DELETE", json!({"channel_id": "20"}));
        assert!(
            out.iter()
                .any(|e| matches!(e, Event::VoiceOccupants { members, .. } if members.is_empty()))
        );

        // Ours rings them once it's up; a friend is rung straight away.
        a.run(Command::JoinCall { room: dm.clone() });
        let join = sent(&mut cmds);
        assert!(join.iter().find_map(op4).unwrap()["guild_id"].is_null());
        let tag = a.call.as_ref().unwrap().tag;
        a.on_call(tag, CallEvent::Connected, &mut Vec::new());
        assert!(
            sent(&mut cmds)
                .iter()
                .any(|c| matches!(c, Cmd::Request(Request::Ring { channel, stranger: false }) if channel == "20"))
        );
        // A DM call's server update names its channel.
        dispatch(
            &mut a,
            "VOICE_STATE_UPDATE",
            json!({"guild_id": null, "user_id": "100", "channel_id": "20", "session_id": "ours"}),
        );
        dispatch(
            &mut a,
            "VOICE_SERVER_UPDATE",
            json!({"guild_id": null, "channel_id": "20", "token": "t", "endpoint": "e"}),
        );
        assert!(matches!(
            sent(&mut cmds).as_slice(),
            [Cmd::CallCmd(CallCmd::Server(s))] if s.server_id == "20" && s.channel_id == "20"
        ));
    }
}
