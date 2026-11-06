// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Steam method names for the generated voice messages, so steam-vent can send
//! them (`service_method`) and route incoming ones (`on_notification`). Names
//! from the services in `proto/voice.proto`.

use steam_vent_proto_common::protobuf::Message;
use steam_vent_proto_common::{RpcMessage, RpcMethod};

use super::proto::*;

macro_rules! rpc_message {
    ($($ty:ty),* $(,)?) => {$(
        impl RpcMessage for $ty {
            fn parse(reader: &mut dyn std::io::Read) -> steam_vent_proto_common::protobuf::Result<Self> {
                <Self as Message>::parse_from_reader(reader)
            }
            fn write(&self, writer: &mut dyn std::io::Write) -> steam_vent_proto_common::protobuf::Result<()> {
                self.write_to_writer(writer)
            }
            fn encode_size(&self) -> usize {
                self.compute_size() as usize
            }
        }
    )*};
}

macro_rules! rpc_method {
    ($($ty:ty => $name:literal, $response:ty;)*) => {$(
        impl RpcMethod for $ty {
            const METHOD_NAME: &'static str = $name;
            type Response = $response;
        }
    )*};
}

rpc_message!(
    CVoiceChat_RequestOneOnOneChat_Request,
    CVoiceChat_RequestOneOnOneChat_Response,
    CVoiceChat_OneOnOneChatRequested_Notification,
    CVoiceChat_AnswerOneOnOneChat_Request,
    CVoiceChat_AnswerOneOnOneChat_Response,
    CVoiceChat_OneOnOneChatRequestResponse_Notification,
    CVoiceChat_EndOneOnOneChat_Request,
    CVoiceChat_EndOneOnOneChat_Response,
    CVoiceChat_UserJoinedVoiceChat_Notification,
    CVoiceChat_UserVoiceStatus_Notification,
    CVoiceChatClient_UserVoiceStatus_Notification,
    CVoiceChat_AllMembersStatus_Notification,
    CVoiceChat_UpdateVoiceChatWebRTCData_Request,
    CVoiceChat_UpdateVoiceChatWebRTCData_Response,
    CVoiceChat_UserLeftVoiceChat_Notification,
    CVoiceChat_VoiceChatEnded_Notification,
    CWebRTCClient_InitiateWebRTCConnection_Request,
    CWebRTCClient_InitiateWebRTCConnection_Response,
    CWebRTC_WebRTCSessionConnected_Notification,
    CWebRTC_WebRTCUpdateRemoteDescription_Notification,
    CWebRTCClient_AcknowledgeUpdatedRemoteDescription_Request,
    CWebRTCClient_AcknowledgeUpdatedRemoteDescription_Response,
    CChatRoomClient_ShouldRejoinVoiceChat_Notification,
);

rpc_method! {
    // Calls to Steam.
    CVoiceChat_RequestOneOnOneChat_Request => "VoiceChat.RequestOneOnOneChat#1", CVoiceChat_RequestOneOnOneChat_Response;
    CVoiceChat_AnswerOneOnOneChat_Request => "VoiceChat.AnswerOneOnOneChat#1", CVoiceChat_AnswerOneOnOneChat_Response;
    CVoiceChat_EndOneOnOneChat_Request => "VoiceChat.EndOneOnOneChat#1", CVoiceChat_EndOneOnOneChat_Response;
    CVoiceChat_UpdateVoiceChatWebRTCData_Request => "VoiceChat.UpdateVoiceChatWebRTCData#1", CVoiceChat_UpdateVoiceChatWebRTCData_Response;
    CVoiceChat_UserVoiceStatus_Notification => "VoiceChat.NotifyUserVoiceStatus#1", ();
    CWebRTCClient_InitiateWebRTCConnection_Request => "WebRTCClient.InitiateWebRTCConnection#1", CWebRTCClient_InitiateWebRTCConnection_Response;
    CWebRTCClient_AcknowledgeUpdatedRemoteDescription_Request => "WebRTCClient.AcknowledgeUpdatedRemoteDescription#1", CWebRTCClient_AcknowledgeUpdatedRemoteDescription_Response;
    // Notifications from Steam.
    CVoiceChat_OneOnOneChatRequested_Notification => "VoiceChatClient.NotifyOneOnOneChatRequested#1", ();
    CVoiceChat_OneOnOneChatRequestResponse_Notification => "VoiceChatClient.NotifyOneOnOneChatResponse#1", ();
    CVoiceChat_UserJoinedVoiceChat_Notification => "VoiceChatClient.NotifyUserJoinedVoiceChat#1", ();
    CVoiceChat_UserLeftVoiceChat_Notification => "VoiceChatClient.NotifyUserLeftVoiceChat#1", ();
    CVoiceChat_VoiceChatEnded_Notification => "VoiceChatClient.NotifyVoiceChatEnded#1", ();
    CVoiceChatClient_UserVoiceStatus_Notification => "VoiceChatClient.NotifyUserVoiceStatus#1", ();
    CVoiceChat_AllMembersStatus_Notification => "VoiceChatClient.NotifyAllUsersVoiceStatus#1", ();
    CWebRTC_WebRTCSessionConnected_Notification => "WebRTCClientNotifications.NotifyWebRTCSessionConnected#1", ();
    CWebRTC_WebRTCUpdateRemoteDescription_Notification => "WebRTCClientNotifications.NotifyWebRTCUpdateRemoteDescription#1", ();
    CChatRoomClient_ShouldRejoinVoiceChat_Notification => "ChatRoomClient.NotifyShouldRejoinChatRoomVoiceChat#1", ();
}
