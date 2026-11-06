// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Steam's voice signaling over the client connection: the calls the official
//! client makes, and the notifications it listens to, as one stream of
//! [`VoiceEvent`]s. Media is not here: it runs over WebRTC directly between us
//! and Steam's voice servers (ICE-lite, no STUN/TURN needed).
//!
//! The order of calls (from vaporchat's `client/voiceSession.ts`, MIT):
//! 1. 1:1 callee only: [`answer_one_on_one`] with `accept = true`.
//! 2. Create a WebRTC offer, [`munge_opus_fmtp`](super::sdp::munge_opus_fmtp)
//!    it, set it as the local description and send it with [`initiate`];
//!    apply the answer through [`PlanBToUnifiedPlan`](super::sdp::PlanBToUnifiedPlan).
//! 3. Wait for both ICE connected and [`VoiceEvent::SessionConnected`].
//! 4. Group: [`join_group`]. 1:1 caller: [`request_one_on_one`], then wait for
//!    [`VoiceEvent::CallAnswered`].
//! 5. [`update_webrtc_data`], then [`send_voice_status`].
//! 6. On [`VoiceEvent::RemoteDescription`] (people joining or leaving): convert,
//!    apply the offer, answer locally (Steam never sees the answer) and
//!    [`acknowledge`] the version.

use futures_util::stream::{self, BoxStream, StreamExt};
use steam_vent::{Connection, ConnectionTrait, NetworkError};
use steam_vent_proto_steam::steammessages_chat_steamclient::{
    CChatRoom_JoinVoiceChat_Request, CChatRoom_LeaveVoiceChat_Request,
};

use super::proto::*;
use super::sdp;

/// Steam's relay won't forward a sender reporting no or an odd sample rate.
const SAMPLE_RATE: i32 = 48_000;
/// The browser we report with our offer, as Steam's web client reports its
/// own; it matches libwebrtc (Chromium's) and the user agent in session.rs.
pub const BROWSER_NAME: &str = "Chrome";
pub const BROWSER_VERSION: &str = "126.0.0.0";

#[derive(Debug)]
pub enum VoiceError {
    Network(NetworkError),
    /// Steam answered something other than an SDP answer to our offer.
    BadAnswer(String),
}

impl std::fmt::Display for VoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(e) => write!(f, "{e}"),
            Self::BadAnswer(what) => write!(f, "Steam sent {what} instead of an answer"),
        }
    }
}

impl std::error::Error for VoiceError {}

impl From<NetworkError> for VoiceError {
    fn from(e: NetworkError) -> Self {
        Self::Network(e)
    }
}

pub type Result<T> = std::result::Result<T, VoiceError>;

/// Where Steam's voice server sees our media session, from
/// `NotifyWebRTCSessionConnected`; echoed back in later calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionInfo {
    /// The ssrc Steam assigned to what we send.
    pub ssrc: u32,
    pub client_ip: u32,
    pub client_port: u32,
    pub server_ip: u32,
    pub server_port: u32,
}

/// Someone's mute/deafen state in a voice chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VoiceStatus {
    pub voice_chat: u64,
    pub user: u64,
    pub mic_muted: bool,
    pub output_muted: bool,
    pub no_mic: bool,
}

/// Which chat a join/leave/end belongs to: a group voice channel, or a 1:1 call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChatRef {
    pub voice_chat: u64,
    /// Group channel: chat group and chat (both 0 for a 1:1 call).
    pub chat_group: u64,
    pub chat: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceEvent {
    /// Our WebRTC session is up on Steam's side.
    SessionConnected(SessionInfo),
    /// Steam's new offer for our peer connection (participants changed).
    /// `description` is Steam's JSON; `accounts` maps remote ssrcs to account ids.
    RemoteDescription {
        description: String,
        version: u64,
        accounts: Vec<(u32, u32)>,
    },
    /// A friend is calling us.
    IncomingCall {
        voice_chat: u64,
        partner: u64,
    },
    /// The friend we called accepted or declined.
    CallAnswered {
        voice_chat: u64,
        partner: u64,
        accepted: bool,
    },
    UserJoined {
        chat: ChatRef,
        user: u64,
    },
    UserLeft {
        chat: ChatRef,
        user: u64,
    },
    Status(VoiceStatus),
    AllStatus {
        voice_chat: u64,
        users: Vec<VoiceStatus>,
    },
    /// The voice chat ended (also: a call hung up before we answered).
    Ended(ChatRef),
    /// Steam wants us to leave and rejoin this group voice channel.
    Rejoin {
        chat_group: u64,
        chat: u64,
    },
}

fn status(voice_chat: u64, user: u64, mic_muted: bool, output_muted: bool, no_mic: bool) -> VoiceStatus {
    VoiceStatus {
        voice_chat,
        user,
        mic_muted,
        output_muted,
        no_mic,
    }
}

/// All voice notifications from Steam as one stream. Subscribe before starting
/// a session so nothing is missed; dropped (lagged) notifications are skipped.
pub fn events(conn: &Connection) -> BoxStream<'static, VoiceEvent> {
    fn on<T, F>(conn: &Connection, map: F) -> BoxStream<'static, VoiceEvent>
    where
        T: steam_vent::ServiceMethodRequest + 'static,
        F: Fn(T) -> VoiceEvent + Send + 'static,
    {
        conn.on_notification::<T>()
            .filter_map(move |n| std::future::ready(n.ok().map(&map)))
            .boxed()
    }
    stream::select_all([
        on(conn, |n: CWebRTC_WebRTCSessionConnected_Notification| {
            VoiceEvent::SessionConnected(SessionInfo {
                ssrc: n.ssrc(),
                client_ip: n.client_ip(),
                client_port: n.client_port(),
                server_ip: n.server_ip(),
                server_port: n.server_port(),
            })
        }),
        on(conn, |n: CWebRTC_WebRTCUpdateRemoteDescription_Notification| {
            VoiceEvent::RemoteDescription {
                description: n.remote_description().to_owned(),
                version: n.remote_description_version(),
                accounts: n
                    .ssrcs_to_accountids
                    .iter()
                    .map(|m| (m.ssrc(), m.accountid()))
                    .collect(),
            }
        }),
        on(conn, |n: CVoiceChat_OneOnOneChatRequested_Notification| {
            VoiceEvent::IncomingCall {
                voice_chat: n.voice_chatid(),
                partner: n.steamid_partner(),
            }
        }),
        on(conn, |n: CVoiceChat_OneOnOneChatRequestResponse_Notification| {
            VoiceEvent::CallAnswered {
                voice_chat: n.voicechat_id(),
                partner: n.steamid_partner(),
                accepted: n.accepted_request(),
            }
        }),
        on(conn, |n: CVoiceChat_UserJoinedVoiceChat_Notification| {
            VoiceEvent::UserJoined {
                chat: ChatRef {
                    voice_chat: n.voice_chatid(),
                    chat_group: n.chat_group_id(),
                    chat: n.chatid(),
                },
                user: n.user_steamid(),
            }
        }),
        on(conn, |n: CVoiceChat_UserLeftVoiceChat_Notification| {
            VoiceEvent::UserLeft {
                chat: ChatRef {
                    voice_chat: n.voice_chatid(),
                    chat_group: n.chat_group_id(),
                    chat: n.chatid(),
                },
                user: n.user_steamid(),
            }
        }),
        on(conn, |n: CVoiceChatClient_UserVoiceStatus_Notification| {
            VoiceEvent::Status(status(
                n.voice_chatid(),
                n.user_steamid(),
                n.user_muted_mic_locally(),
                n.user_muted_output_locally(),
                n.user_has_no_mic_for_session(),
            ))
        }),
        on(conn, |n: CVoiceChat_AllMembersStatus_Notification| {
            VoiceEvent::AllStatus {
                voice_chat: n.voice_chatid(),
                users: n
                    .users
                    .iter()
                    .map(|u| {
                        status(
                            n.voice_chatid(),
                            u.user_steamid(),
                            u.user_muted_mic_locally(),
                            u.user_muted_output_locally(),
                            u.user_has_no_mic_for_session(),
                        )
                    })
                    .collect(),
            }
        }),
        on(conn, |n: CVoiceChat_VoiceChatEnded_Notification| {
            VoiceEvent::Ended(ChatRef {
                voice_chat: n.voice_chatid(),
                chat_group: n.chat_group_id(),
                chat: n.chatid(),
            })
        }),
        on(conn, |n: CChatRoomClient_ShouldRejoinVoiceChat_Notification| {
            VoiceEvent::Rejoin {
                chat_group: n.chat_group_id(),
                chat: n.chat_id(),
            }
        }),
    ])
    .boxed()
}

/// Sends our offer (already munged and set as local description); returns
/// Steam's answer SDP, still in Plan B.
pub async fn initiate(conn: &Connection, offer_sdp: &str) -> Result<String> {
    let req = CWebRTCClient_InitiateWebRTCConnection_Request {
        sdp: Some(sdp::to_json("offer", offer_sdp)),
        browser_name: Some(BROWSER_NAME.to_owned()),
        browser_version: Some(BROWSER_VERSION.to_owned()),
        ..Default::default()
    };
    let res = conn.service_method(req).await?;
    match sdp::from_json(res.remote_description()) {
        Some((kind, answer)) if kind == "answer" => Ok(answer),
        Some((kind, _)) => Err(VoiceError::BadAnswer(format!("an {kind}"))),
        None => Err(VoiceError::BadAnswer("no description".to_owned())),
    }
}

/// Tells Steam we applied its description `version`. The official client
/// ignores the result; a failed acknowledgement must not end the call.
pub async fn acknowledge(conn: &Connection, session: SessionInfo, version: u64) -> Result<()> {
    let req = CWebRTCClient_AcknowledgeUpdatedRemoteDescription_Request {
        ip_webrtc_server: Some(session.server_ip),
        port_webrtc_server: Some(session.server_port),
        ip_webrtc_session_client: Some(session.client_ip),
        port_webrtc_session_client: Some(session.client_port),
        remote_description_version: Some(version),
        ..Default::default()
    };
    conn.service_method(req).await?;
    Ok(())
}

/// Joins a group's voice channel; returns the voice chat id.
pub async fn join_group(conn: &Connection, chat_group: u64, chat: u64) -> Result<u64> {
    let req = CChatRoom_JoinVoiceChat_Request {
        chat_group_id: Some(chat_group),
        chat_id: Some(chat),
        ..Default::default()
    };
    Ok(conn.service_method(req).await?.voice_chatid())
}

pub async fn leave_group(conn: &Connection, chat_group: u64, chat: u64) -> Result<()> {
    let req = CChatRoom_LeaveVoiceChat_Request {
        chat_group_id: Some(chat_group),
        chat_id: Some(chat),
        ..Default::default()
    };
    conn.service_method(req).await?;
    Ok(())
}

/// Rings a friend; returns the voice chat id. Their answer arrives as
/// [`VoiceEvent::CallAnswered`].
pub async fn request_one_on_one(conn: &Connection, partner: u64) -> Result<u64> {
    let req = CVoiceChat_RequestOneOnOneChat_Request {
        steamid_partner: Some(partner),
        ..Default::default()
    };
    Ok(conn.service_method(req).await?.voice_chatid())
}

pub async fn answer_one_on_one(conn: &Connection, voice_chat: u64, partner: u64, accept: bool) -> Result<()> {
    let req = CVoiceChat_AnswerOneOnOneChat_Request {
        voice_chatid: Some(voice_chat),
        steamid_partner: Some(partner),
        accepted_request: Some(accept),
        ..Default::default()
    };
    conn.service_method(req).await?;
    Ok(())
}

/// Hangs up a 1:1 call for both sides.
pub async fn end_one_on_one(conn: &Connection, partner: u64) -> Result<()> {
    let req = CVoiceChat_EndOneOnOneChat_Request {
        steamid_partner: Some(partner),
        ..Default::default()
    };
    conn.service_method(req).await?;
    Ok(())
}

/// Binds our media session to the voice chat. Steam only accepts this once
/// both ICE and [`VoiceEvent::SessionConnected`] are up.
pub async fn update_webrtc_data(
    conn: &Connection,
    voice_chat: u64,
    session: SessionInfo,
    user_agent: &str,
) -> Result<()> {
    let req = CVoiceChat_UpdateVoiceChatWebRTCData_Request {
        voice_chatid: Some(voice_chat),
        ip_webrtc_server: Some(session.server_ip),
        port_webrtc_server: Some(session.server_port),
        ip_webrtc_client: Some(session.client_ip),
        port_webrtc_client: Some(session.client_port),
        ssrc_my_sending_stream: Some(session.ssrc),
        user_agent: Some(user_agent.to_owned()),
        has_audio_worklets_support: Some(false),
        // Voice only, like vaporchat: no data channel, no video either way.
        has_webrtc_data_channel: Some(false),
        accepts_webrtc_video: Some(false),
        sends_webrtc_video: Some(false),
        ..Default::default()
    };
    conn.service_method(req).await?;
    Ok(())
}

/// Announces our mute/deafen state: a notification (no reply), as Steam's clients send it.
pub async fn send_voice_status(conn: &Connection, me: VoiceStatus) {
    let req = CVoiceChat_UserVoiceStatus_Notification {
        voice_chatid: Some(me.voice_chat),
        user_steamid: Some(me.user),
        user_muted_mic_locally: Some(me.mic_muted),
        user_muted_output_locally: Some(me.output_muted),
        user_has_no_mic_for_session: Some(me.no_mic),
        user_webaudio_sample_rate: Some(SAMPLE_RATE),
        user_sends_video: Some(false),
        ..Default::default()
    };
    if let Err(e) = conn.notify_service(req).await {
        crate::backend::diag(&format!("call: sending our voice status failed: {e}"));
    }
}

/// One line about a voice event, for the log (no message content in these).
pub fn describe(event: &VoiceEvent) -> String {
    match event {
        VoiceEvent::SessionConnected(s) => format!("session connected, ssrc {}", s.ssrc),
        VoiceEvent::RemoteDescription { version, accounts, .. } => {
            format!("new remote description v{version}, ssrc map {accounts:?}")
        }
        VoiceEvent::IncomingCall { voice_chat, partner } => format!("incoming call {voice_chat} from {partner}"),
        VoiceEvent::CallAnswered {
            voice_chat,
            partner,
            accepted,
        } => format!("call {voice_chat} answered by {partner}: accepted {accepted}"),
        VoiceEvent::UserJoined { chat, user } => format!("{user} joined voice chat {}", chat.voice_chat),
        VoiceEvent::UserLeft { chat, user } => format!("{user} left voice chat {}", chat.voice_chat),
        VoiceEvent::Status(s) => format!("status of {} in {}", s.user, s.voice_chat),
        VoiceEvent::AllStatus { voice_chat, users } => format!("status of {} users in {voice_chat}", users.len()),
        VoiceEvent::Ended(chat) => format!("voice chat {} ended", chat.voice_chat),
        VoiceEvent::Rejoin { chat_group, chat } => format!("rejoin {chat_group}/{chat}"),
    }
}
