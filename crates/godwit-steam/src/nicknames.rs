// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Steam's server-side friend nicknames. The RPC and client messages match
//! SteamKit2's `SteamFriends.SetPlayerNickname` and nickname-list callback.

use steam_vent::{Connection, ConnectionTrait};
use steam_vent_proto_common::protobuf::Message;
use steam_vent_proto_common::{RpcMessage, RpcMessageWithKind, RpcMethod};

#[allow(unknown_lints, clippy::all, dead_code, missing_docs, non_camel_case_types, non_snake_case)]
#[allow(non_upper_case_globals, trivial_casts, unused_results, unused_mut, unused_attributes)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[rustfmt::skip]
mod proto {
    include!(concat!(env!("OUT_DIR"), "/nicknames.rs"));
}
pub use proto::CMsgClientSetPlayerNickname;
pub use proto::CMsgClientSetPlayerNicknameResponse;
use proto::*;

macro_rules! rpc_message {
    ($($ty:ty),* $(,)?) => {$(
        impl RpcMessage for $ty {
            fn parse(reader: &mut dyn std::io::Read) -> steam_vent_proto_common::protobuf::Result<Self> {
                <Self as Message>::parse_from_reader(reader)
            }
            fn write(&self, writer: &mut dyn std::io::Write) -> steam_vent_proto_common::protobuf::Result<()> {
                self.write_to_writer(writer)
            }
            fn encode_size(&self) -> usize { self.compute_size() as usize }
        }
    )*};
}

rpc_message!(
    CPlayer_GetNicknameList_Request,
    CPlayer_GetNicknameList_Response,
    CMsgClientPlayerNicknameList,
    CMsgClientSetPlayerNickname,
    CMsgClientSetPlayerNicknameResponse,
);

impl RpcMethod for CPlayer_GetNicknameList_Request {
    const METHOD_NAME: &'static str = "Player.GetNicknameList#1";
    type Response = CPlayer_GetNicknameList_Response;
}

pub use proto::CMsgClientPlayerNicknameList as Changed;

impl RpcMessageWithKind for Changed {
    type KindEnum = steam_vent_proto_steam::enums_clientserver::EMsg;
    const KIND: Self::KindEnum = Self::KindEnum::k_EMsgClientPlayerNicknameList;
}

impl RpcMessageWithKind for CMsgClientSetPlayerNickname {
    type KindEnum = steam_vent_proto_steam::enums_clientserver::EMsg;
    const KIND: Self::KindEnum = Self::KindEnum::k_EMsgAMClientSetPlayerNickname;
}

impl RpcMessageWithKind for CMsgClientSetPlayerNicknameResponse {
    type KindEnum = steam_vent_proto_steam::enums_clientserver::EMsg;
    const KIND: Self::KindEnum = Self::KindEnum::k_EMsgAMClientSetPlayerNicknameResponse;
}

/// Fetch the full server-side nickname list.
pub async fn get(conn: &Connection) -> Result<Vec<(u32, String)>, String> {
    let response = conn
        .service_method(CPlayer_GetNicknameList_Request::default())
        .await
        .map_err(|e| e.to_string())?;
    Ok(from_response(&response))
}

/// Convert Steam's protobuf response to account ids and nickname strings.
pub fn from_response(response: &CPlayer_GetNicknameList_Response) -> Vec<(u32, String)> {
    entries(
        response
            .nicknames
            .iter()
            .map(|entry| (entry.accountid(), entry.nickname().to_owned())),
    )
}

/// Map Steam's account ids to the core's account-id form, ignoring malformed empty ids/names.
pub fn entries(items: impl IntoIterator<Item = (u32, String)>) -> Vec<(u32, String)> {
    items
        .into_iter()
        .filter(|(id, name)| *id != 0 && !name.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nickname_entries_and_skips_empty_values() {
        assert_eq!(
            entries([(1, "Alex".into()), (0, "bad".into()), (2, "".into())]),
            vec![(1, "Alex".into())]
        );
    }

    #[test]
    fn maps_service_response_fields() {
        let response = CPlayer_GetNicknameList_Response {
            nicknames: vec![
                cplayer_get_nickname_list_response::PlayerNickname {
                    accountid: Some(17),
                    nickname: Some("Alex".into()),
                    ..Default::default()
                },
                cplayer_get_nickname_list_response::PlayerNickname {
                    accountid: Some(0),
                    nickname: Some("bad".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(from_response(&response), vec![(17, "Alex".into())]);
    }

    #[test]
    fn method_name_matches_steam_service() {
        assert_eq!(CPlayer_GetNicknameList_Request::METHOD_NAME, "Player.GetNicknameList#1");
    }
}
