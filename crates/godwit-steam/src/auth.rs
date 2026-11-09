// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Signing in to Steam: its Authentication service, over HTTPS as Steam's
//! own web sign-in uses it (protobuf bodies, base64 in `input_protobuf_encoded`,
//! the result in the `x-eresult` header). The end is a refresh token, which
//! signs the client connection in (`Connection::access`) and is kept in the
//! secret store, so the password is never stored, and with a QR code never
//! even typed here.
//!
//! - QR: [`begin_qr`], show its `challenge_url` as a QR code, then [`poll`]
//!   until the Steam mobile app approves (a new URL may come meanwhile).
//! - Password: [`begin_password`], then a Steam Guard code with
//!   [`submit_code`] when asked (or approval in the app), then [`poll`].

use std::time::Duration;

use base64::Engine;
use base64::prelude::{BASE64_STANDARD, BASE64_URL_SAFE_NO_PAD};
use steam_vent_proto_common::protobuf::{EnumOrUnknown, Message, MessageField};
use steam_vent_proto_steam::enums::ESessionPersistence;
use steam_vent_proto_steam::steammessages_auth_steamclient::{
    CAuthentication_AllowedConfirmation, CAuthentication_BeginAuthSessionViaCredentials_Request,
    CAuthentication_BeginAuthSessionViaCredentials_Response, CAuthentication_BeginAuthSessionViaQR_Request,
    CAuthentication_BeginAuthSessionViaQR_Response, CAuthentication_DeviceDetails,
    CAuthentication_GetPasswordRSAPublicKey_Request, CAuthentication_GetPasswordRSAPublicKey_Response,
    CAuthentication_PollAuthSessionStatus_Request, CAuthentication_PollAuthSessionStatus_Response,
    CAuthentication_UpdateAuthSessionWithSteamGuardCode_Request, EAuthSessionGuardType, EAuthTokenPlatformType,
};

/// The name requests carry in the network layer's log.
const NET: &str = "steam";
const API: &str = "https://api.steampowered.com/IAuthenticationService";
/// Characters of a generated device name (as Windows names new computers).
const NAME_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
/// The slowest Steam asks us to poll at, at least: it says how often.
const MIN_INTERVAL: Duration = Duration::from_secs(2);

/// Why signing in failed, in words for the sign-in form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthError(pub String);

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AuthError {}

/// A way Steam lets this sign-in be confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Guard {
    /// Nothing needed.
    None,
    /// A code sent by email (`hint`: the address's domain, as Steam says it).
    EmailCode { hint: String },
    /// A code from the Steam mobile app.
    DeviceCode,
    /// Approve in the Steam mobile app.
    DeviceConfirmation,
    /// Click the link in an email.
    EmailConfirmation,
}

impl Guard {
    fn from_proto(c: &CAuthentication_AllowedConfirmation) -> Option<Self> {
        Some(match c.confirmation_type?.enum_value().ok()? {
            EAuthSessionGuardType::k_EAuthSessionGuardType_None => Self::None,
            EAuthSessionGuardType::k_EAuthSessionGuardType_EmailCode => Self::EmailCode {
                hint: c.associated_message.clone().unwrap_or_default(),
            },
            EAuthSessionGuardType::k_EAuthSessionGuardType_DeviceCode => Self::DeviceCode,
            EAuthSessionGuardType::k_EAuthSessionGuardType_DeviceConfirmation => Self::DeviceConfirmation,
            EAuthSessionGuardType::k_EAuthSessionGuardType_EmailConfirmation => Self::EmailConfirmation,
            _ => return None,
        })
    }

    /// Whether it's a code to type.
    pub fn wants_code(&self) -> bool {
        matches!(self, Self::EmailCode { .. } | Self::DeviceCode)
    }
}

/// A sign-in under way.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub client_id: u64,
    pub request_id: Vec<u8>,
    /// How often Steam wants to be asked how it's going.
    pub interval: Duration,
    /// QR: the URL to show as a QR code.
    pub challenge_url: Option<String>,
    /// Password: whose account it is.
    pub steam_id: Option<u64>,
    /// Password: how it may be confirmed (Steam's order of preference).
    pub guards: Vec<Guard>,
}

/// Signed in: what connects the client and is kept.
#[derive(Clone, PartialEq, Eq)]
pub struct Tokens {
    pub refresh_token: String,
    pub account_name: String,
    pub steam_id: u64,
}

impl std::fmt::Debug for Tokens {
    // Never print the token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("account_name", &self.account_name)
            .field("steam_id", &self.steam_id)
            .finish_non_exhaustive()
    }
}

/// How a sign-in stands after a [`poll`].
#[derive(Debug, Clone, PartialEq)]
pub enum Poll {
    /// Not yet. QR codes are renewed now and then: show the new one.
    Waiting {
        new_challenge_url: Option<String>,
        new_client_id: Option<u64>,
        /// Someone scanned the QR code (waiting for their approval).
        scanned: bool,
    },
    Done(Tokens),
}

/// The client used for signing in.
pub fn client() -> Result<reqwest::Client, AuthError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| AuthError("Couldn't start a secure connection.".into()))
}

/// Starts a sign-in by QR code.
/// A device name for one sign-in, as it shows in the account's list of
/// signed-in devices: random and Windows-style (`DESKTOP-` and seven
/// letters or digits), so it says neither "Godwit" nor the computer's real
/// name (which often holds a person's). Steam's own client sends the
/// computer's name; steam-vent a fixed one.
pub fn device_name() -> String {
    let mut bytes = [0u8; 7];
    if getrandom::fill(&mut bytes).is_err() {
        // No randomness: still nothing identifying.
        return "DESKTOP-PC".into();
    }
    let tail: String = bytes
        .iter()
        .map(|b| char::from(NAME_CHARS[usize::from(*b) % NAME_CHARS.len()]))
        .collect();
    format!("DESKTOP-{tail}")
}

pub async fn begin_qr(http: &reqwest::Client, device_name: &str) -> Result<Session, AuthError> {
    let res: CAuthentication_BeginAuthSessionViaQR_Response =
        call(http, "BeginAuthSessionViaQR", &qr_request(device_name), false).await?;
    Ok(Session {
        client_id: res.client_id.unwrap_or_default(),
        request_id: res.request_id.clone().unwrap_or_default(),
        interval: interval(res.interval),
        challenge_url: res.challenge_url.clone(),
        steam_id: None,
        guards: res.allowed_confirmations.iter().filter_map(Guard::from_proto).collect(),
    })
}

fn qr_request(device_name: &str) -> CAuthentication_BeginAuthSessionViaQR_Request {
    CAuthentication_BeginAuthSessionViaQR_Request {
        device_friendly_name: Some(device_name.into()),
        platform_type: Some(EnumOrUnknown::new(
            EAuthTokenPlatformType::k_EAuthTokenPlatformType_SteamClient,
        )),
        device_details: MessageField::some(device(device_name)),
        website_id: Some("Client".into()),
        ..Default::default()
    }
}

/// Starts a sign-in with a password. `guard_data` is what an earlier sign-in
/// on this device was given (it can spare the Steam Guard code).
pub async fn begin_password(
    http: &reqwest::Client,
    device_name: &str,
    account: &str,
    password: &str,
    guard_data: Option<&str>,
) -> Result<Session, AuthError> {
    let key: CAuthentication_GetPasswordRSAPublicKey_Response = call(
        http,
        "GetPasswordRSAPublicKey",
        &CAuthentication_GetPasswordRSAPublicKey_Request {
            account_name: Some(account.into()),
            ..Default::default()
        },
        true,
    )
    .await?;
    let encrypted = encrypt_password(&key, password)?;
    let req = CAuthentication_BeginAuthSessionViaCredentials_Request {
        account_name: Some(account.into()),
        encrypted_password: Some(encrypted),
        encryption_timestamp: key.timestamp,
        persistence: Some(EnumOrUnknown::new(
            ESessionPersistence::k_ESessionPersistence_Persistent,
        )),
        website_id: Some("Client".into()),
        device_details: MessageField::some(device(device_name)),
        guard_data: guard_data.map(str::to_owned),
        ..Default::default()
    };
    let res: CAuthentication_BeginAuthSessionViaCredentials_Response =
        call(http, "BeginAuthSessionViaCredentials", &req, false).await?;
    Ok(Session {
        client_id: res.client_id.unwrap_or_default(),
        request_id: res.request_id.clone().unwrap_or_default(),
        interval: interval(res.interval),
        challenge_url: None,
        steam_id: res.steamid,
        guards: res.allowed_confirmations.iter().filter_map(Guard::from_proto).collect(),
    })
}

/// The password encrypted with the account's RSA key, as Steam wants it.
fn encrypt_password(
    key: &CAuthentication_GetPasswordRSAPublicKey_Response,
    password: &str,
) -> Result<String, AuthError> {
    let bad = || AuthError("Steam sent an unusable key; try again.".into());
    let n =
        rsa::BigUint::parse_bytes(key.publickey_mod.as_deref().unwrap_or_default().as_bytes(), 16).ok_or_else(bad)?;
    let e =
        rsa::BigUint::parse_bytes(key.publickey_exp.as_deref().unwrap_or_default().as_bytes(), 16).ok_or_else(bad)?;
    let key = rsa::RsaPublicKey::new(n, e).map_err(|_| bad())?;
    let encrypted = steam_vent_crypto::encrypt_with_key_pkcs1(&key, password.as_bytes()).map_err(|_| bad())?;
    Ok(BASE64_STANDARD.encode(encrypted))
}

/// A Steam Guard code for a password sign-in.
pub async fn submit_code(http: &reqwest::Client, session: &Session, code: &str, kind: &Guard) -> Result<(), AuthError> {
    let code_type = match kind {
        Guard::EmailCode { .. } => EAuthSessionGuardType::k_EAuthSessionGuardType_EmailCode,
        _ => EAuthSessionGuardType::k_EAuthSessionGuardType_DeviceCode,
    };
    let req = CAuthentication_UpdateAuthSessionWithSteamGuardCode_Request {
        client_id: Some(session.client_id),
        steamid: session.steam_id,
        code: Some(code.trim().to_owned()),
        code_type: Some(EnumOrUnknown::new(code_type)),
        ..Default::default()
    };
    // The answer has nothing in it but the result.
    let _: steam_vent_proto_steam::steammessages_auth_steamclient::CAuthentication_UpdateAuthSessionWithSteamGuardCode_Response =
        call(http, "UpdateAuthSessionWithSteamGuardCode", &req, false).await?;
    Ok(())
}

/// Asks once how the sign-in stands (call every `session.interval`).
pub async fn poll(http: &reqwest::Client, session: &Session) -> Result<Poll, AuthError> {
    let req = CAuthentication_PollAuthSessionStatus_Request {
        client_id: Some(session.client_id),
        request_id: Some(session.request_id.clone()),
        ..Default::default()
    };
    let res: CAuthentication_PollAuthSessionStatus_Response = call(http, "PollAuthSessionStatus", &req, false).await?;
    Ok(read_poll(res))
}

fn read_poll(res: CAuthentication_PollAuthSessionStatus_Response) -> Poll {
    if let Some(token) = res.refresh_token.clone().filter(|t| !t.is_empty())
        && let Some(steam_id) = token_steam_id(&token)
    {
        return Poll::Done(Tokens {
            refresh_token: token,
            account_name: res.account_name.clone().unwrap_or_default(),
            steam_id,
        });
    }
    Poll::Waiting {
        new_challenge_url: res.new_challenge_url.clone(),
        new_client_id: res.new_client_id,
        scanned: res.had_remote_interaction.unwrap_or(false),
    }
}

/// The account a token belongs to: its JWT's subject.
pub fn token_steam_id(token: &str) -> Option<u64> {
    #[derive(serde::Deserialize)]
    struct Claims {
        sub: String,
    }
    let payload = BASE64_URL_SAFE_NO_PAD.decode(token.split('.').nth(1)?).ok()?;
    serde_json::from_slice::<Claims>(&payload).ok()?.sub.parse().ok()
}

/// When a token stops working (seconds since the epoch), from its JWT.
pub fn token_expiry(token: &str) -> Option<u64> {
    #[derive(serde::Deserialize)]
    struct Claims {
        exp: u64,
    }
    let payload = BASE64_URL_SAFE_NO_PAD.decode(token.split('.').nth(1)?).ok()?;
    Some(serde_json::from_slice::<Claims>(&payload).ok()?.exp)
}

fn interval(secs: Option<f32>) -> Duration {
    secs.filter(|s| s.is_finite() && *s > 0.0)
        .map_or(Duration::from_secs(5), Duration::from_secs_f32)
        .max(MIN_INTERVAL)
}

fn device(device_name: &str) -> CAuthentication_DeviceDetails {
    CAuthentication_DeviceDetails {
        device_friendly_name: Some(device_name.into()),
        platform_type: Some(EnumOrUnknown::new(
            EAuthTokenPlatformType::k_EAuthTokenPlatformType_SteamClient,
        )),
        os_type: Some(os_type()),
        ..Default::default()
    }
}

/// Steam's EOSType for this system, as its client reports it.
fn os_type() -> i32 {
    if cfg!(windows) {
        16 // Windows 10 and later
    } else if cfg!(target_os = "macos") {
        -102 // macOS, version unknown
    } else {
        -203 // Linux, version unknown
    }
}

/// One call: the request as base64 protobuf, the answer's protobuf back.
async fn call<Req: Message, Res: Message>(
    http: &reqwest::Client,
    method: &str,
    req: &Req,
    get: bool,
) -> Result<Res, AuthError> {
    let encoded = req
        .write_to_bytes()
        .map(|b| BASE64_STANDARD.encode(b))
        .map_err(|_| AuthError("Couldn't prepare the sign-in.".into()))?;
    let url = format!("{API}/{method}/v1");
    let params = [("input_protobuf_encoded", encoded)];
    let request = if get {
        http.get(&url).query(&params)
    } else {
        http.post(&url).form(&params)
    };
    // Status polls repeat by design (sign-in by QR); the rest sign in.
    let class = if method.starts_with("Poll") {
        godwit_core::net::Class::Poll(std::time::Duration::from_secs(1))
    } else {
        godwit_core::net::Class::Auth
    };
    let response = godwit_core::net::stream(NET, class, request)
        .await
        .map_err(|_| AuthError("Couldn't reach Steam. Check your connection.".into()))?;
    let result = response
        .headers()
        .get("x-eresult")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(if response.status().is_success() { 1 } else { 2 });
    if result != 1 {
        return Err(AuthError(eresult_text(result)));
    }
    let body = response
        .bytes()
        .await
        .map_err(|_| AuthError("Steam's answer was cut off; try again.".into()))?;
    Res::parse_from_bytes(&body).map_err(|_| AuthError("Steam's answer couldn't be read.".into()))
}

/// What a Steam result code means for whoever is signing in.
fn eresult_text(code: i32) -> String {
    match code {
        5 => "Wrong account name or password.".into(),
        9 | 27 => "This sign-in expired. Start again.".into(),
        65 | 88 => "That code isn't right. Check it and try again.".into(),
        84 => "Too many tries. Wait a while before signing in again.".into(),
        _ => format!("Steam refused the sign-in (code {code})."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(payload: &str) -> String {
        format!("x.{}.y", BASE64_URL_SAFE_NO_PAD.encode(payload))
    }

    #[test]
    fn tokens_name_their_account_and_expiry() {
        let token = jwt(r#"{"iss":"steam","sub":"76561190000000001","exp":1900000000}"#);
        assert_eq!(token_steam_id(&token), Some(76_561_190_000_000_001));
        assert_eq!(token_expiry(&token), Some(1_900_000_000));
        assert_eq!(token_steam_id("not a token"), None);
        // Debug output never shows the token.
        let t = Tokens {
            refresh_token: token,
            account_name: "someone".into(),
            steam_id: 1,
        };
        assert!(!format!("{t:?}").contains("eyJ") && !format!("{t:?}").contains("x."));
    }

    #[test]
    fn polls_wait_until_a_token_arrives() {
        let waiting = read_poll(CAuthentication_PollAuthSessionStatus_Response {
            new_challenge_url: Some("https://s.team/q/1/2".into()),
            had_remote_interaction: Some(true),
            ..Default::default()
        });
        assert_eq!(
            waiting,
            Poll::Waiting {
                new_challenge_url: Some("https://s.team/q/1/2".into()),
                new_client_id: None,
                scanned: true
            }
        );
        let token = jwt(r#"{"sub":"42","exp":1}"#);
        let done = read_poll(CAuthentication_PollAuthSessionStatus_Response {
            refresh_token: Some(token.clone()),
            account_name: Some("someone".into()),
            ..Default::default()
        });
        assert!(matches!(done, Poll::Done(t) if t.steam_id == 42 && t.refresh_token == token));
    }

    #[test]
    fn requests_name_a_random_desktop_as_a_steam_client() {
        let name = device_name();
        assert!(name.starts_with("DESKTOP-") && name.len() == 15, "{name}");
        assert!(name[8..].bytes().all(|b| NAME_CHARS.contains(&b)));
        assert_ne!(device_name(), device_name(), "one per sign-in");
        let req = qr_request(&name);
        let bytes = req.write_to_bytes().unwrap();
        let back = CAuthentication_BeginAuthSessionViaQR_Request::parse_from_bytes(&bytes).unwrap();
        assert_eq!(back.device_friendly_name.as_deref(), Some(name.as_str()));
        assert!(!format!("{back:?}").to_lowercase().contains("godwit"));
        assert_eq!(
            back.platform_type.unwrap().enum_value(),
            Ok(EAuthTokenPlatformType::k_EAuthTokenPlatformType_SteamClient)
        );
        assert_eq!(interval(Some(0.1)), MIN_INTERVAL, "never faster than allowed");
        assert_eq!(interval(None), Duration::from_secs(5));
    }

    #[test]
    fn passwords_are_encrypted_with_the_accounts_key() {
        // A small made-up test key (not Steam's): a 516-bit modulus, exponent 65537.
        let key = CAuthentication_GetPasswordRSAPublicKey_Response {
            publickey_mod: Some(
                "c5062b58d8539c765e2a1e8f2b2d1b5bd0a6c8b1d2e3f405162738495a6b7c8d9eafb0c1d2e3f405162738495a6b7c8d9eafb0c1d2e3f405162738495a6b7c8d9".into(),
            ),
            publickey_exp: Some("010001".into()),
            timestamp: Some(1),
            ..Default::default()
        };
        let encrypted = encrypt_password(&key, "hunter2").unwrap();
        let raw = BASE64_STANDARD.decode(encrypted).unwrap();
        assert_eq!(raw.len(), 65, "one RSA block (the modulus's size)");
        assert!(!raw.windows(7).any(|w| w == b"hunter2"));
        let broken = CAuthentication_GetPasswordRSAPublicKey_Response::default();
        assert!(encrypt_password(&broken, "x").is_err());
    }

    #[test]
    fn guards_say_whether_a_code_is_wanted() {
        let email = CAuthentication_AllowedConfirmation {
            confirmation_type: Some(EnumOrUnknown::new(
                EAuthSessionGuardType::k_EAuthSessionGuardType_EmailCode,
            )),
            associated_message: Some("example.com".into()),
            ..Default::default()
        };
        let guard = Guard::from_proto(&email).unwrap();
        assert!(guard.wants_code());
        assert_eq!(
            guard,
            Guard::EmailCode {
                hint: "example.com".into()
            }
        );
        assert!(!Guard::DeviceConfirmation.wants_code());
    }
}
