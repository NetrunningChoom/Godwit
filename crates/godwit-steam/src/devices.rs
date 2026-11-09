// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The account's authorized devices: every sign-in Steam keeps a refresh
//! token for, as `Authentication.EnumerateTokens` lists them
//! (SteamDatabase/Protobufs steammessages_auth.steamclient.proto,
//! https://github.com/SteamDatabase/Protobufs). Read-only: revoking another
//! device's token (`RevokeRefreshToken`) takes a signature that no public
//! source explains.

use godwit_core::privacy::Session;
use steam_vent::{Connection, ConnectionTrait};
use steam_vent_proto_steam::steammessages_auth_steamclient::{
    CAuthentication_RefreshToken_Enumerate_Request, CAuthentication_RefreshToken_Enumerate_Response,
    EAuthTokenPlatformType,
    cauthentication_refresh_token_enumerate_response::{RefreshTokenDescription, TokenUsageEvent},
};

/// The devices signed in now, newest use first.
pub async fn list(conn: &Connection) -> Result<Vec<Session>, String> {
    let response = conn
        .service_method(CAuthentication_RefreshToken_Enumerate_Request::default())
        .await
        .map_err(|e| e.to_string())?;
    Ok(sessions(&response))
}

/// Maps Steam's token list to Godwit's sessions. The token this client signed
/// in with (`requesting_token`) is named as this device.
pub fn sessions(response: &CAuthentication_RefreshToken_Enumerate_Response) -> Vec<Session> {
    let current = response.requesting_token;
    let mut out: Vec<Session> = response
        .refresh_tokens
        .iter()
        .filter(|t| t.token_id.is_some())
        .map(|t| session(t, current == t.token_id))
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.last_used_ms));
    out
}

fn session(token: &RefreshTokenDescription, current: bool) -> Session {
    let kind = match token.platform_type() {
        EAuthTokenPlatformType::k_EAuthTokenPlatformType_SteamClient => "Steam client",
        EAuthTokenPlatformType::k_EAuthTokenPlatformType_WebBrowser => "Web browser",
        EAuthTokenPlatformType::k_EAuthTokenPlatformType_MobileApp => "Steam Mobile app",
        _ => "Device",
    };
    let name = token.token_description().trim();
    let mut device = if name.is_empty() {
        kind.to_owned()
    } else {
        format!("{name} · {kind}")
    };
    if current {
        device.push_str(" · This device");
    }
    let last = token.last_seen.as_ref();
    let seen = |e: Option<&TokenUsageEvent>| {
        e.map(TokenUsageEvent::time)
            .filter(|&t| t > 0)
            .map(|t| i64::from(t) * 1000)
    };
    Session {
        id: token.token_id().to_string().into(),
        device: device.into(),
        location: last.map(place).unwrap_or_default().into(),
        created_ms: seen(token.first_seen.as_ref()),
        last_used_ms: seen(last).or_else(|| token.time_updated.filter(|&t| t > 0).map(|t| i64::from(t) * 1000)),
    }
}

/// "City, State, Country", leaving out what Steam doesn't say.
fn place(event: &TokenUsageEvent) -> String {
    [event.city(), event.state(), event.country()]
        .into_iter()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use steam_vent_proto_common::protobuf::{EnumOrUnknown, MessageField};

    fn token(id: u64, name: &str, platform: EAuthTokenPlatformType, last: u32) -> RefreshTokenDescription {
        RefreshTokenDescription {
            token_id: Some(id),
            token_description: Some(name.to_owned()),
            platform_type: Some(EnumOrUnknown::new(platform)),
            last_seen: MessageField::some(TokenUsageEvent {
                time: Some(last),
                city: Some("Helsinki".into()),
                country: Some("FI".into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn tokens_become_sessions_newest_first_with_this_device_named() {
        let response = CAuthentication_RefreshToken_Enumerate_Response {
            refresh_tokens: vec![
                token(
                    1,
                    "DESKTOP",
                    EAuthTokenPlatformType::k_EAuthTokenPlatformType_SteamClient,
                    100,
                ),
                token(2, "", EAuthTokenPlatformType::k_EAuthTokenPlatformType_MobileApp, 200),
                RefreshTokenDescription::default(),
            ],
            requesting_token: Some(1),
            ..Default::default()
        };
        let list = sessions(&response);
        assert_eq!(list.len(), 2);
        assert_eq!(&*list[0].id, "2");
        assert_eq!(&*list[0].device, "Steam Mobile app");
        assert_eq!(list[0].last_used_ms, Some(200_000));
        assert_eq!(&*list[1].device, "DESKTOP · Steam client · This device");
        assert_eq!(&*list[1].location, "Helsinki, FI");
        assert_eq!(list[1].created_ms, None);
    }

    #[test]
    fn last_use_falls_back_to_the_update_time() {
        let t = RefreshTokenDescription {
            token_id: Some(5),
            time_updated: Some(7),
            ..Default::default()
        };
        let s = session(&t, false);
        assert_eq!(s.last_used_ms, Some(7000));
        assert_eq!(&*s.device, "Device");
        assert_eq!(&*s.location, "");
    }
}
