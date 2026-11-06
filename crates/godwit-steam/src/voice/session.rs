// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! One Steam voice call with a friend, from ringing to hanging up.
//!
//! The order follows Steam's own web client as vaporchat documents it
//! (`client/voiceSession.ts`, MIT): media first (offer, Steam's answer, ICE
//! and Steam's "session connected"), then ring or answer, then bind the media
//! session to the voice chat. Steam renegotiates whenever someone joins or
//! leaves; we apply its offer and only acknowledge the version.
//!
//! Unlike vaporchat this doesn't retry a failed connection: the call ends
//! with the reason, and calling again starts over.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use godwit_voice::call::VoiceSettings;
use godwit_voice::peer::{Peer, PeerEvent};
use steam_vent::Connection;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::sdp::{self, PlanBToUnifiedPlan};
use super::signaling::{self, SessionInfo, VoiceEvent, VoiceStatus};
use crate::backend::{self, diag};

/// How long the media path may take to come up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a friend's phone rings before we give up (Steam's client gives up too).
const RING_TIMEOUT: Duration = Duration::from_secs(60);
/// Steam's clients are all Chromium (its web chat, the desktop client's
/// CEF): the voice server sees one of those (the same Chrome version as
/// `signaling::BROWSER_VERSION`).
const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
/// Hanging up or telling Steam our state shouldn't hold anything up for long.
const QUICK: Duration = Duration::from_secs(5);

pub enum SessionCmd {
    Leave,
    Muted(bool),
    Deafened(bool),
    Settings(Box<VoiceSettings>),
}

/// Someone in the call, as Steam reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Member {
    pub steam_id: u64,
    pub speaking: bool,
    pub muted: bool,
    pub deafened: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// Their phone rings.
    Ringing,
    Connected,
    Members(Vec<Member>),
    /// Over; `None` when we hung up or they did, else why it failed.
    Ended(Option<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Connecting,
    Ringing,
    Live,
}

struct Call {
    conn: Arc<Connection>,
    me: u64,
    partner: u64,
    /// Steam's id of a call we're answering.
    incoming: Option<u64>,
    /// A group room's voice chat (group, chat) instead of a 1:1 call:
    /// joined, not rung (vaporchat's `JoinVoiceChat` path).
    group: Option<(u64, u64)>,
    peer: Peer,
    plan_b: PlanBToUnifiedPlan,
    /// Remote ssrc → account id, from Steam's latest description.
    accounts: HashMap<u32, u32>,
    session: Option<SessionInfo>,
    ice_up: bool,
    voice_chat: Option<u64>,
    phase: Phase,
    members: Vec<Member>,
    muted: bool,
    deafened: bool,
}

enum Step {
    Cmd(Option<SessionCmd>),
    Signal(VoiceEvent),
    Peer(PeerEvent),
    Tick,
    Timeout,
}

/// Runs a call with `partner` (answering Steam's call `incoming` if set) until
/// it ends; every change goes to `emit`, the last one [`SessionEvent::Ended`].
pub async fn run(
    conn: Arc<Connection>,
    partner: u64,
    incoming: Option<u64>,
    settings: VoiceSettings,
    mut cmds: mpsc::UnboundedReceiver<SessionCmd>,
    emit: impl Fn(SessionEvent) + Send + 'static,
) {
    diag(&format!("call: starting with {partner} (answering: {incoming:?})"));
    let reason = call(conn, partner, incoming, None, &settings, &mut cmds, &emit).await;
    diag(&format!("call: ended ({})", reason.as_deref().unwrap_or("normally")));
    emit(SessionEvent::Ended(reason));
}

/// Runs a group room's voice chat until we leave it or it ends.
pub async fn run_group(
    conn: Arc<Connection>,
    group: u64,
    chat: u64,
    settings: VoiceSettings,
    mut cmds: mpsc::UnboundedReceiver<SessionCmd>,
    emit: impl Fn(SessionEvent) + Send + 'static,
) {
    diag(&format!("call: joining group {group} room {chat}"));
    let reason = call(conn, 0, None, Some((group, chat)), &settings, &mut cmds, &emit).await;
    diag(&format!("call: ended ({})", reason.as_deref().unwrap_or("normally")));
    emit(SessionEvent::Ended(reason));
}

#[allow(clippy::too_many_arguments)]
async fn call(
    conn: Arc<Connection>,
    partner: u64,
    incoming: Option<u64>,
    group: Option<(u64, u64)>,
    settings: &VoiceSettings,
    cmds: &mut mpsc::UnboundedReceiver<SessionCmd>,
    emit: &(impl Fn(SessionEvent) + Send),
) -> Option<String> {
    // Listen before asking for anything, so no notification is missed.
    let mut signals = signaling::events(&conn);
    let me = conn.steam_id().steam64();

    if let Some(voice_chat) = incoming
        && let Err(e) = signaling::answer_one_on_one(&conn, voice_chat, partner, true).await
    {
        return Some(format!("Couldn't answer the call: {e}"));
    }

    let (peer_tx, mut peer_rx) = mpsc::unbounded_channel();
    let peer = match Peer::new(settings, peer_tx) {
        Ok(peer) => peer,
        Err(e) => {
            // We already accepted: hang up rather than leave them waiting.
            if incoming.is_some() {
                let _ = tokio::time::timeout(QUICK, signaling::end_one_on_one(&conn, partner)).await;
            }
            return Some(format!("Couldn't start audio: {e:#}"));
        }
    };
    let mut call = Call {
        conn,
        me,
        partner,
        incoming,
        group,
        peer,
        plan_b: PlanBToUnifiedPlan::new(),
        accounts: HashMap::new(),
        session: None,
        ice_up: false,
        voice_chat: incoming,
        phase: Phase::Connecting,
        // Answering: the caller is in the call already, and Steam doesn't
        // say they joined (it did before we listened).
        members: std::iter::once(me)
            .chain(incoming.map(|_| partner))
            .map(|steam_id| Member {
                steam_id,
                speaking: false,
                muted: false,
                deafened: false,
            })
            .collect(),
        muted: false,
        deafened: false,
    };
    let reason = match call.run(&mut signals, &mut peer_rx, cmds, emit).await {
        Ok(()) => None,
        Err(reason) => reason,
    };
    // Leave or hang up on Steam's side too unless it already ended there.
    if let Some((group, chat)) = call.group {
        if call.voice_chat.is_some() {
            let _ = tokio::time::timeout(QUICK, signaling::leave_group(&call.conn, group, chat)).await;
        }
    } else if call.phase != Phase::Connecting || call.incoming.is_some() {
        let _ = tokio::time::timeout(QUICK, signaling::end_one_on_one(&call.conn, call.partner)).await;
    }
    reason
}

impl Call {
    /// Ends with `Err(reason)` (`None`: a normal end).
    async fn run(
        &mut self,
        signals: &mut BoxStream<'static, VoiceEvent>,
        peer_rx: &mut mpsc::UnboundedReceiver<PeerEvent>,
        cmds: &mut mpsc::UnboundedReceiver<SessionCmd>,
        emit: &(impl Fn(SessionEvent) + Send),
    ) -> Result<(), Option<String>> {
        self.start_media().await?;
        let mut deadline = Instant::now() + CONNECT_TIMEOUT;
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let mut ticks: u64 = 0;
        loop {
            if self.phase == Phase::Connecting && self.ice_up && self.session.is_some() {
                if let Some((group, chat)) = self.group {
                    // Steam only accepts the join once the media path is up.
                    let voice_chat = signaling::join_group(&self.conn, group, chat)
                        .await
                        .map_err(|e| Some(format!("Couldn't join the voice chat: {e}")))?;
                    diag(&format!("call: joined group voice chat {voice_chat}"));
                    self.voice_chat = Some(voice_chat);
                    self.bind(emit).await?;
                } else if self.incoming.is_some() {
                    self.bind(emit).await?;
                } else {
                    let voice_chat = signaling::request_one_on_one(&self.conn, self.partner)
                        .await
                        .map_err(|e| Some(format!("Couldn't call: {e}")))?;
                    diag(&format!("call: ringing, voice chat {voice_chat}"));
                    self.voice_chat = Some(voice_chat);
                    self.phase = Phase::Ringing;
                    deadline = Instant::now() + RING_TIMEOUT;
                    emit(SessionEvent::Ringing);
                }
            }
            let waiting = self.phase != Phase::Live;
            let step = tokio::select! {
                cmd = cmds.recv() => Step::Cmd(cmd),
                Some(signal) = signals.next() => Step::Signal(signal),
                Some(event) = peer_rx.recv() => Step::Peer(event),
                _ = tick.tick() => Step::Tick,
                _ = tokio::time::sleep_until(deadline), if waiting => Step::Timeout,
            };
            match step {
                Step::Cmd(None | Some(SessionCmd::Leave)) => {
                    diag("call: we hung up");
                    return Ok(());
                }
                Step::Cmd(Some(SessionCmd::Muted(muted))) => {
                    self.muted = muted;
                    self.peer.set_muted(muted);
                    self.send_status();
                    self.update_me(emit);
                }
                Step::Cmd(Some(SessionCmd::Deafened(deafened))) => {
                    self.deafened = deafened;
                    self.peer.set_deafened(deafened);
                    self.send_status();
                    self.update_me(emit);
                }
                Step::Cmd(Some(SessionCmd::Settings(settings))) => self.peer.update(&settings),
                Step::Signal(signal) => self.signal(signal, emit).await?,
                Step::Peer(PeerEvent::Connected) => {
                    diag("call: media path (ICE) connected");
                    self.ice_up = true;
                }
                Step::Peer(PeerEvent::Lost) => {
                    diag("call: media path (ICE) failed or closed");
                    return Err(Some(if self.phase == Phase::Live {
                        "Lost the connection to Steam's voice server.".to_owned()
                    } else {
                        "Couldn't connect to Steam's voice server.".to_owned()
                    }));
                }
                Step::Peer(PeerEvent::Track { track, streams, mid }) => {
                    diag(&format!(
                        "call: remote track, streams {streams:?}, mid {mid:?}, ssrc map {:?}",
                        self.accounts
                    ));
                    let account = streams
                        .iter()
                        .find_map(|s| sdp::account_id_from_stream_id(s))
                        .or_else(|| {
                            let ssrc = self.plan_b.ssrc_for_mid(mid.as_deref()?)?;
                            self.accounts.get(&ssrc).copied()
                        });
                    match account {
                        Some(account) => self.peer.play(track, backend::steam_id(account).to_string()),
                        None => godwit_core::verbose!("warning: Steam voice: a track without a known speaker"),
                    }
                }
                Step::Tick => {
                    self.update_speaking(emit);
                    // Media counters every 2 s while connecting/live, for diagnosing calls.
                    ticks += 1;
                    if ticks.is_multiple_of(20) {
                        diag(&format!("call: {:?}: {}", self.phase, self.peer.stats().await));
                    }
                }
                Step::Timeout => {
                    diag(&format!("call: timed out in {:?}", self.phase));
                    return Err(Some(match self.phase {
                        Phase::Ringing => "No answer.".to_owned(),
                        _ => "Couldn't connect to Steam's voice server in time.".to_owned(),
                    }));
                }
            }
        }
    }

    /// Our offer to Steam and its answer.
    async fn start_media(&mut self) -> Result<(), Option<String>> {
        let failed = |e: String| Some(format!("Couldn't start the call: {e}"));
        let offer = self.peer.offer().await.map_err(|e| failed(format!("{e:#}")))?;
        let offer = sdp::munge_opus_fmtp(&offer);
        self.peer
            .set_local_offer(&offer)
            .await
            .map_err(|e| failed(format!("{e:#}")))?;
        diag(&format!("call: our offer:\n{}", redact(&offer)));
        diag("call: offer sent");
        let answer = signaling::initiate(&self.conn, &offer)
            .await
            .map_err(|e| failed(e.to_string()))?;
        diag(&format!("call: Steam's answer:\n{}", redact(&answer)));
        let answer = self.plan_b.convert(&answer);
        self.peer
            .set_remote_answer(&answer)
            .await
            .map_err(|e| failed(format!("{e:#}")))
    }

    /// Binds our media session to the voice chat: the call is live.
    async fn bind(&mut self, emit: &(impl Fn(SessionEvent) + Send)) -> Result<(), Option<String>> {
        let (Some(voice_chat), Some(session)) = (self.voice_chat, self.session) else {
            return Ok(());
        };
        diag(&format!(
            "call: binding media to voice chat {voice_chat} (ssrc {})",
            session.ssrc
        ));
        signaling::update_webrtc_data(&self.conn, voice_chat, session, USER_AGENT)
            .await
            .map_err(|e| Some(format!("Couldn't join the call: {e}")))?;
        diag("call: live");
        self.phase = Phase::Live;
        emit(SessionEvent::Connected);
        self.send_status();
        emit(SessionEvent::Members(self.members.clone()));
        Ok(())
    }

    async fn signal(
        &mut self,
        signal: VoiceEvent,
        emit: &(impl Fn(SessionEvent) + Send),
    ) -> Result<(), Option<String>> {
        diag(&format!(
            "call: {} (our voice chat {:?}, phase {:?})",
            signaling::describe(&signal),
            self.voice_chat,
            self.phase
        ));
        let ours = |voice_chat: u64| self.voice_chat == Some(voice_chat);
        match signal {
            VoiceEvent::SessionConnected(info) => {
                self.plan_b.set_own_ssrc(info.ssrc);
                self.session = Some(info);
            }
            VoiceEvent::RemoteDescription {
                description,
                version,
                accounts,
            } => {
                self.accounts = accounts.into_iter().collect();
                let Some((kind, remote)) = sdp::from_json(&description) else {
                    return Ok(());
                };
                diag(&format!("call: Steam's {kind} v{version}:\n{}", redact(&remote)));
                let converted = self.plan_b.convert(&remote);
                diag(&format!("call: converted v{version}:\n{}", redact(&converted)));
                let applied = if kind == "offer" {
                    self.peer.apply_offer(&converted).await
                } else {
                    self.peer.set_remote_answer(&converted).await
                };
                if let Err(e) = &applied {
                    diag(&format!("call: applying Steam's {kind} v{version} failed: {e:#}"));
                }
                applied.map_err(|e| Some(format!("Steam's voice server sent something we couldn't use: {e:#}")))?;
                // Steam only learns that we applied it; the official client
                // ignores whether that worked.
                if let Some(session) = self.session {
                    let conn = self.conn.clone();
                    tokio::spawn(async move {
                        let _ = tokio::time::timeout(QUICK, signaling::acknowledge(&conn, session, version)).await;
                    });
                }
            }
            VoiceEvent::CallAnswered {
                voice_chat,
                partner,
                accepted,
            } if self.phase == Phase::Ringing && ours(voice_chat) && partner == self.partner => {
                if !accepted {
                    return Err(Some("They declined the call.".to_owned()));
                }
                self.bind(emit).await?;
            }
            // Both called at once: take their call, like the official client.
            VoiceEvent::IncomingCall { voice_chat, partner }
                if partner == self.partner && self.phase == Phase::Ringing =>
            {
                signaling::answer_one_on_one(&self.conn, voice_chat, partner, true)
                    .await
                    .map_err(|e| Some(format!("Couldn't answer the call: {e}")))?;
                self.voice_chat = Some(voice_chat);
                self.incoming = Some(voice_chat);
                self.bind(emit).await?;
            }
            VoiceEvent::UserJoined { chat, user } if ours(chat.voice_chat) => {
                if !self.members.iter().any(|m| m.steam_id == user) {
                    self.members.push(Member {
                        steam_id: user,
                        speaking: false,
                        muted: false,
                        deafened: false,
                    });
                    emit(SessionEvent::Members(self.members.clone()));
                }
            }
            VoiceEvent::UserLeft { chat, user } if ours(chat.voice_chat) && user != self.me => {
                self.members.retain(|m| m.steam_id != user);
                emit(SessionEvent::Members(self.members.clone()));
            }
            VoiceEvent::Status(status) if ours(status.voice_chat) => {
                if self.apply_status(status) {
                    emit(SessionEvent::Members(self.members.clone()));
                }
            }
            VoiceEvent::AllStatus { voice_chat, users } if ours(voice_chat) => {
                let mut changed = false;
                for status in users {
                    changed |= self.apply_status(status);
                }
                if changed {
                    emit(SessionEvent::Members(self.members.clone()));
                }
            }
            // They hung up (or the call was answered elsewhere): a normal end.
            VoiceEvent::Ended(chat) if ours(chat.voice_chat) => {
                diag(&format!("call: at the end: {}", self.peer.stats().await));
                self.phase = Phase::Connecting;
                self.incoming = None;
                return Err(None);
            }
            _ => {}
        }
        Ok(())
    }

    /// Someone else's mute/deafen (ours is what we set). Returns whether it changed.
    fn apply_status(&mut self, status: VoiceStatus) -> bool {
        if status.user == self.me {
            return false;
        }
        // A status is someone in the call, even if we never saw them join.
        let added = !self.members.iter().any(|m| m.steam_id == status.user);
        if added {
            self.members.push(Member {
                steam_id: status.user,
                speaking: false,
                muted: false,
                deafened: false,
            });
        }
        let Some(member) = self.members.iter_mut().find(|m| m.steam_id == status.user) else {
            return false;
        };
        let before = *member;
        member.muted = status.mic_muted;
        member.deafened = status.output_muted;
        added || before != *member
    }

    fn update_me(&mut self, emit: &(impl Fn(SessionEvent) + Send)) {
        let (muted, deafened) = (self.muted, self.deafened);
        if let Some(me) = self.members.iter_mut().find(|m| m.steam_id == self.me) {
            me.muted = muted;
            me.deafened = deafened;
        }
        if self.phase == Phase::Live {
            emit(SessionEvent::Members(self.members.clone()));
        }
    }

    fn update_speaking(&mut self, emit: &(impl Fn(SessionEvent) + Send)) {
        if self.phase != Phase::Live {
            return;
        }
        let (me_speaking, others) = self.peer.speaking();
        let mut changed = false;
        for member in &mut self.members {
            let speaking = if member.steam_id == self.me {
                me_speaking && !self.muted
            } else {
                others.iter().any(|id| *id == member.steam_id.to_string())
            };
            changed |= member.speaking != speaking;
            member.speaking = speaking;
        }
        if changed {
            emit(SessionEvent::Members(self.members.clone()));
        }
    }

    /// Our mute/deafen for the others (a notification; doesn't hold the call up).
    fn send_status(&self) {
        let Some(voice_chat) = self.voice_chat.filter(|_| self.phase == Phase::Live) else {
            return;
        };
        let status = VoiceStatus {
            voice_chat,
            user: self.me,
            mic_muted: self.muted,
            output_muted: self.deafened,
            no_mic: false,
        };
        let conn = self.conn.clone();
        tokio::spawn(async move { signaling::send_voice_status(&conn, status).await });
    }
}

/// An SDP for the log without its secrets and addresses (ICE credentials,
/// DTLS fingerprint, candidates): codecs and extensions are what matter there.
fn redact(sdp: &str) -> String {
    sdp.lines()
        .map(|l| {
            let l = l.trim_end();
            if l.starts_with("a=ice-pwd")
                || l.starts_with("a=ice-ufrag")
                || l.starts_with("a=fingerprint")
                || l.starts_with("a=candidate")
                || l.starts_with("c=")
                || l.starts_with("o=")
            {
                format!("    {} [redacted]", l.split([':', ' ']).next().unwrap_or(""))
            } else {
                format!("    {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
