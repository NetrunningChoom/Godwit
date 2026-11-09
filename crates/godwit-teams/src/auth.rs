// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Signing in as the Teams client does: Microsoft's device-code flow under
//! the Teams client's public id, then the access token traded at Teams'
//! `authz` for a Skype token and the address of the user's chat service.
//!
//! Tokens give full access to the account: they're never logged, printed or
//! put in errors. Microsoft's error answers hold no secrets, so their
//! descriptions are shown.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::Value;

/// The name requests carry in the network layer's log.
const NET: &str = "teams";

/// The Teams work client's public client id (purple-teams' `TEAMS_WORK_OAUTH_CLIENT_ID`).
pub const CLIENT_ID: &str = "1fec8e78-bce4-4aaf-ab1b-5451cc387264";
/// The resource the device code is asked for; `authz` takes its token.
const RESOURCE: &str = "https://api.spaces.skype.com";
/// The same, as a v2 scope, for refreshing.
pub const SPACES_SCOPE: &str = "https://api.spaces.skype.com/.default openid profile offline_access";
/// The chat service aggregator (teams and their channels).
/// The presence service, as a v2 scope (purple-teams' `teams_oauth_refresh_services`).
pub const PRESENCE_SCOPE: &str = "https://presence.teams.microsoft.com/.default openid profile offline_access";
pub const CSA_SCOPE: &str = "https://chatsvcagg.teams.microsoft.com/.default openid profile offline_access";
/// Any work or school tenant.
pub const TENANT: &str = "organizations";
const LOGIN: &str = "https://login.microsoftonline.com";
const AUTHZ: &str = "https://teams.microsoft.com/api/authsvc/v1.0/authz";
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// What to show while the user signs in elsewhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCode {
    /// The short code typed at `url`; not a secret (it's useless without the device code).
    pub user_code: String,
    pub url: String,
    /// The secret polled with; never shown.
    pub(crate) device_code: String,
    pub interval: Duration,
    pub expires_in: Duration,
}

/// An access token for one resource, and the refresh token if Microsoft
/// rotated it.
#[derive(Clone)]
pub struct Granted {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Duration,
}

impl std::fmt::Debug for Granted {
    // Never print the tokens.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Granted")
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

/// One poll of the device code.
#[derive(Debug)]
pub enum Poll {
    Pending,
    /// Asked to poll less often.
    SlowDown,
    Granted(Granted),
}

/// What `authz` gives for an access token.
#[derive(Clone)]
pub struct Skype {
    pub token: String,
    pub expires_in: Duration,
    /// The chat service's base address, without a trailing slash.
    pub chat_service: String,
    /// The user's region (`emea`, `amer`, `apac`, or a country), if said.
    pub region: Option<String>,
}

impl std::fmt::Debug for Skype {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Skype")
            .field("chat_service", &self.chat_service)
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

/// Signing in again is needed: the refresh token is dead or the code expired.
#[derive(Debug)]
pub struct Reauth(pub String);

impl std::fmt::Display for Reauth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Reauth {}

/// `application/x-www-form-urlencoded`, by hand (reqwest's `form` isn't on).
pub(crate) use godwit_core::encoding::form;

async fn post_form(http: &Client, url: &str, pairs: &[(&str, &str)]) -> Result<(u16, Value)> {
    let request = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(form(pairs));
    let answer = godwit_core::net::stream(NET, godwit_core::net::Class::Auth, request)
        .await
        .context("Can't reach Microsoft's sign-in service.")?;
    let status = answer.status().as_u16();
    // A 200's body holds tokens: only serde's message goes in an error.
    let body: Value = answer
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("Microsoft's sign-in answer couldn't be read ({status})."))?;
    Ok((status, body))
}

/// Microsoft's own words for a refusal (no secrets in these).
fn refusal(status: u16, body: &Value) -> String {
    let error = body["error"].as_str().unwrap_or("refused");
    // The description's first line; later ones are trace and correlation ids.
    let description = body["error_description"]
        .as_str()
        .and_then(|d| d.lines().next())
        .unwrap_or_default();
    if description.is_empty() {
        format!("Microsoft refused the sign-in: {error} ({status}).")
    } else {
        format!("Microsoft refused the sign-in: {description}")
    }
}

fn seconds(v: &Value, default: u64) -> Duration {
    let n = v
        .as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(default);
    Duration::from_secs(n)
}

fn granted(body: &Value) -> Result<Granted> {
    let access_token = body["access_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .context("Microsoft's answer had no access token.")?
        .to_owned();
    Ok(Granted {
        access_token,
        refresh_token: body["refresh_token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(str::to_owned),
        // Azure AD's tokens last an hour; shorter is the safe guess.
        expires_in: seconds(&body["expires_in"], 3600),
    })
}

/// Starts signing in: `POST {login}/organizations/oauth2/devicecode`.
pub async fn device_code(http: &Client) -> Result<DeviceCode> {
    let url = format!("{LOGIN}/{TENANT}/oauth2/devicecode");
    let (status, body) = post_form(http, &url, &[("client_id", CLIENT_ID), ("resource", RESOURCE)]).await?;
    if status != 200 {
        bail!(refusal(status, &body));
    }
    let text = |k: &str| body[k].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    Ok(DeviceCode {
        user_code: text("user_code").context("Microsoft's answer had no code.")?,
        // The v1 endpoint says `verification_url`, v2 `verification_uri`.
        url: text("verification_url")
            .or_else(|| text("verification_uri"))
            .unwrap_or_else(|| "https://microsoft.com/devicelogin".into()),
        device_code: text("device_code").context("Microsoft's answer had no device code.")?,
        // Never faster than a second, whatever it says.
        interval: seconds(&body["interval"], 5).max(Duration::from_secs(1)),
        expires_in: seconds(&body["expires_in"], 900),
    })
}

/// One poll: `POST {login}/organizations/oauth2/token` with the device-code grant.
pub async fn poll(http: &Client, code: &DeviceCode) -> Result<Poll> {
    let url = format!("{LOGIN}/{TENANT}/oauth2/token");
    let (status, body) = post_form(
        http,
        &url,
        &[
            ("client_id", CLIENT_ID),
            ("grant_type", DEVICE_CODE_GRANT),
            ("code", &code.device_code),
        ],
    )
    .await?;
    if status == 200 {
        return granted(&body).map(Poll::Granted);
    }
    match body["error"].as_str() {
        Some("authorization_pending") => Ok(Poll::Pending),
        Some("slow_down") => Ok(Poll::SlowDown),
        Some("expired_token" | "code_expired") => {
            Err(Reauth("The code expired before the sign-in was finished. Try again.".into()).into())
        }
        Some("authorization_declined") => Err(Reauth("The sign-in was declined.".into()).into()),
        _ => bail!(refusal(status, &body)),
    }
}

/// A new access token for `scope`: `POST {login}/{tenant}/oauth2/v2.0/token`
/// with the refresh-token grant. A dead refresh token is [`Reauth`].
pub async fn refresh(http: &Client, tenant: &str, refresh_token: &str, scope: &str) -> Result<Granted> {
    let url = format!("{LOGIN}/{tenant}/oauth2/v2.0/token");
    let (status, body) = post_form(
        http,
        &url,
        &[
            ("client_id", CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("scope", scope),
        ],
    )
    .await?;
    if status == 200 {
        return granted(&body);
    }
    match body["error"].as_str() {
        Some("invalid_grant" | "interaction_required") => Err(Reauth(refusal(status, &body)).into()),
        _ => bail!(refusal(status, &body)),
    }
}

/// Trades an `api.spaces.skype.com` access token for a Skype token and the
/// chat service's address: `POST teams.microsoft.com/api/authsvc/v1.0/authz`.
pub async fn authz(http: &Client, access_token: &str) -> Result<Skype> {
    let request = http
        .post(AUTHZ)
        .bearer_auth(access_token)
        .header(reqwest::header::ACCEPT, "application/json; ver=1.0")
        // Front ends may refuse a body-less POST without a length.
        .header(reqwest::header::CONTENT_LENGTH, "0");
    let answer = godwit_core::net::stream(NET, godwit_core::net::Class::Auth, request)
        .await
        .context("Can't reach Teams.")?;
    let status = answer.status().as_u16();
    let body: Value = answer
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("Teams' sign-in answer couldn't be read ({status})."))?;
    parse_authz(status, &body)
}

fn parse_authz(status: u16, body: &Value) -> Result<Skype> {
    if status != 200 {
        // {"errorCode":"UserLicenseNotPresentForbidden","message":"…"} (purple-teams).
        let message = body["message"].as_str().unwrap_or("Teams refused the sign-in.");
        if status == 401 || status == 403 {
            bail!(Reauth(format!("{message} ({status})")));
        }
        bail!("{message} ({status})");
    }
    // `tokens.skypeToken` now; `skypeToken.skypetoken` in older answers.
    let (token, expires) = if let Some(t) = body["tokens"]["skypeToken"].as_str() {
        (t, &body["tokens"]["expiresIn"])
    } else {
        (
            body["skypeToken"]["skypetoken"].as_str().unwrap_or_default(),
            &body["skypeToken"]["expiresIn"],
        )
    };
    if token.is_empty() {
        bail!("Teams' answer had no Skype token.");
    }
    let region = body["region"].as_str().filter(|r| !r.is_empty()).map(str::to_owned);
    let chat_service = match body["regionGtms"]["chatService"].as_str().filter(|s| !s.is_empty()) {
        Some(url) => url.trim_end_matches('/').to_owned(),
        None => match &region {
            Some(r) => format!("https://{r}.ng.msg.teams.microsoft.com"),
            None => bail!("Teams didn't say where its chat service is."),
        },
    };
    if !chat_service.starts_with("https://") {
        bail!("Teams named a chat service that isn't HTTPS.");
    }
    Ok(Skype {
        token: token.to_owned(),
        expires_in: seconds(expires, 3600),
        chat_service,
        region,
    })
}

/// The `api/csa/{geo}` and `api/mt/{geo}` part for a region: `amer` for the
/// Americas, `apac` for Asia-Pacific, `emea` otherwise.
pub fn geo(region: Option<&str>) -> &'static str {
    match region.map(str::to_ascii_lowercase).as_deref() {
        Some("us" | "ca" | "br" | "amer") => "amer",
        Some("apac" | "au" | "jp" | "in" | "kr" | "sg") => "apac",
        _ => "emea",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn forms_encode_scopes_and_urls() {
        assert_eq!(
            form(&[("scope", "https://a.b/.default openid"), ("x", "a&b=c")]),
            "scope=https%3A%2F%2Fa.b%2F.default+openid&x=a%26b%3Dc"
        );
    }

    #[test]
    fn authz_takes_the_chat_service_or_builds_it_from_the_region() {
        let s = parse_authz(
            200,
            &json!({"tokens": {"skypeToken": "fake", "expiresIn": 86_399},
                    "region": "emea", "regionGtms": {"chatService": "https://emea.ng.msg.teams.microsoft.com/"}}),
        )
        .unwrap();
        assert_eq!(s.chat_service, "https://emea.ng.msg.teams.microsoft.com");
        assert_eq!(s.expires_in, Duration::from_secs(86_399));
        assert!(!format!("{s:?}").contains("fake"));
        let s = parse_authz(200, &json!({"skypeToken": {"skypetoken": "fake"}, "region": "apac"})).unwrap();
        assert_eq!(s.chat_service, "https://apac.ng.msg.teams.microsoft.com");
    }

    #[test]
    fn authz_refuses_plain_http_and_missing_tokens() {
        assert!(
            parse_authz(
                200,
                &json!({"tokens": {"skypeToken": "t"}, "regionGtms": {"chatService": "http://x"}})
            )
            .is_err()
        );
        assert!(parse_authz(200, &json!({"region": "emea"})).is_err());
        let e = parse_authz(403, &json!({"message": "Teams is disabled in user licenses"})).unwrap_err();
        assert!(e.downcast_ref::<Reauth>().is_some());
    }

    #[test]
    fn refusals_keep_only_the_first_line() {
        let body = json!({"error": "invalid_grant", "error_description": "AADSTS70000: Bad grant.\r\nTrace ID: 1\r\nCorrelation ID: 2"});
        assert_eq!(
            refusal(400, &body),
            "Microsoft refused the sign-in: AADSTS70000: Bad grant."
        );
    }

    #[test]
    fn regions_map_to_geos() {
        assert_eq!(geo(Some("US")), "amer");
        assert_eq!(geo(Some("au")), "apac");
        assert_eq!(geo(Some("de")), "emea");
        assert_eq!(geo(None), "emea");
    }
}
