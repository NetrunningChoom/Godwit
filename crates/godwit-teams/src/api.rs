// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Teams' HTTP calls: the chat service (`{region}.ng.msg.teams.microsoft.com`,
//! named by `authz`) for conversations and messages, the chat service
//! aggregator for teams and their channels, and the middle tier for names.
//! Headers are the web client's, as purple-teams sends them.
//!
//! Requests go one at a time, at least [`SPACING`] apart; a 429 pauses
//! everything for its `Retry-After` and the request isn't replayed.

use std::sync::RwLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::header::{self, HeaderValue};
use reqwest::{Client, Method, RequestBuilder};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::auth;

/// The web client's user agent (purple-teams' `TEAMS_USER_AGENT`): the chat
/// service treats other agents as unknown clients.
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36 Edg/126.0.0.0 Teams/24165.1410.2974.6689/49";
/// The client version Trouter and the registrar are told (purple-teams' `TEAMS_CLIENTINFO_VERSION`).
pub const CLIENT_VERSION: &str = "49/25113001312";
const TEAMS: &str = "https://teams.microsoft.com";
/// Between any two requests of an account.
const SPACING: Duration = Duration::from_millis(200);
/// History pages are this long.
pub const PAGE: usize = 50;
/// Every kind of conversation (purple-teams' `targetType`).
const TARGETS: &str = "Passport|Skype|Lync|Thread|PSTN|Agent";
/// Largest answer read; anything bigger is refused, not buffered.
const MAX_BODY: usize = 16 * 1024 * 1024;

pub type PresenceInfo = (String, String, Option<String>);

/// The tokens requests need, refreshed by the backend.
#[derive(Clone, Default)]
pub struct Tokens {
    pub skype: String,
    /// For `api.spaces.skype.com`: the middle tier, Trouter's sign-in, the registrar.
    pub access: String,
    /// For the chat service aggregator, once fetched.
    pub csa: Option<String>,
    /// For `presence.teams.microsoft.com`, once fetched.
    pub presence: Option<String>,
}

pub struct Api {
    pub media: godwit_core::media::Media,
    http: Client,
    chat: String,
    region: Option<String>,
    tokens: RwLock<Tokens>,
    endpoint: String,
    /// When the next request may go.
    pace: Mutex<Instant>,
    followed: tokio::sync::watch::Sender<Vec<String>>,
    active: std::sync::atomic::AtomicBool,
}

/// A refusal, in the service's own words where it gives any.
#[derive(Debug)]
pub struct Refused {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.message.is_empty() {
            write!(f, "Teams answered {}.", self.status)
        } else {
            write!(f, "{} ({})", self.message, self.status)
        }
    }
}

impl std::error::Error for Refused {}

impl Refused {
    /// The Skype token is no longer accepted.
    pub fn unauthorized(&self) -> bool {
        self.status == 401
    }
}

/// HTTPS with ring, no retries, no cookies.
pub fn client() -> Result<Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut headers = header::HeaderMap::new();
    headers.insert(header::USER_AGENT, HeaderValue::from_static(USER_AGENT));
    Client::builder()
        .default_headers(headers)
        .retry(reqwest::retry::never())
        // Other crates turn reqwest's decoders on for the whole build, and
        // Trouter's deflate answers don't decode ("deflate decompression
        // error"): ask for plain answers.
        .no_gzip()
        .no_deflate()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(60))
        .build()
        .context("could not set up HTTPS")
}

/// Percent-encodes a path segment (conversation ids hold `:`, `@` and `;`).
pub use godwit_core::encoding::percent_encode as seg;

fn sensitive(value: String) -> Result<HeaderValue> {
    let mut v = HeaderValue::from_str(&value).map_err(|_| anyhow::anyhow!("a token isn't a valid header"))?;
    v.set_sensitive(true);
    Ok(v)
}

/// Milliseconds since 1970, as Teams' client message ids and read markers use.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Api {
    /// `endpoint`: this sign-in's push endpoint id (the presence service
    /// wants it, as `x-ms-endpoint-id`).
    pub fn new(http: Client, skype: &auth::Skype, access: String, endpoint: String) -> Self {
        let media = godwit_core::media::Media::new("teams");
        crate::media::configure(
            &media,
            &Tokens {
                skype: skype.token.clone(),
                access: access.clone(),
                csa: None,
                presence: None,
            },
        );
        Self {
            media,
            http,
            chat: skype.chat_service.clone(),
            region: skype.region.clone(),
            tokens: RwLock::new(Tokens {
                skype: skype.token.clone(),
                access,
                csa: None,
                presence: None,
            }),
            endpoint,
            pace: Mutex::new(Instant::now()),
            followed: tokio::sync::watch::Sender::new(Vec::new()),
            active: std::sync::atomic::AtomicBool::new(true),
        }
    }

    pub fn http(&self) -> &Client {
        &self.http
    }

    pub fn tokens(&self) -> Tokens {
        self.tokens
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set_tokens(&self, f: impl FnOnce(&mut Tokens)) {
        let mut tokens = self.tokens.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut tokens);
        crate::media::configure(&self.media, &tokens);
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn region(&self) -> Option<&str> {
        self.region.as_deref()
    }

    /// A chat service request with the web client's headers.
    fn chat(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let t = self.tokens();
        Ok(self
            .http
            .request(method, format!("{}{path}", self.chat))
            .header("x-skypetoken", sensitive(t.skype)?)
            .header(header::ACCEPT, "application/json; ver=1.0;")
            .header("behavioroverride", "redirectAs404")
            .header("x-stratus-caller", "skypeteams")
            .header("x-stratus-request", "abcd1234")
            .header(header::ORIGIN, TEAMS)
            .header(header::REFERER, "https://teams.microsoft.com/"))
    }

    /// Sends at the pace, reads JSON (an empty body is `null`).
    async fn send(&self, request: RequestBuilder) -> Result<Value> {
        {
            let mut next = self.pace.lock().await;
            tokio::time::sleep_until(*next).await;
            *next = Instant::now() + SPACING;
        }
        let answer = request.send().await.context("Can't reach Teams.")?;
        let status = answer.status().as_u16();
        if status == 429 {
            let wait = godwit_core::backend::retry_after(
                answer.headers().get(header::RETRY_AFTER).and_then(|v| v.to_str().ok()),
                Duration::from_secs(60),
            );
            *self.pace.lock().await = godwit_core::backend::checked_deadline(wait);
            bail!(Refused {
                status,
                message: "Teams asked Godwit to slow down; try again in a moment".into(),
            });
        }
        let bytes = answer.bytes().await.context("Teams' answer was cut off.")?;
        if bytes.len() > MAX_BODY {
            bail!("Teams' answer was too large.");
        }
        let body: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        if !(200..300).contains(&status) {
            let message = body["message"]
                .as_str()
                .or_else(|| body["errorMessage"].as_str())
                .or_else(|| body["error"]["message"].as_str())
                .or_else(|| body["errorCode"].as_str())
                .or_else(|| body["code"].as_str())
                .unwrap_or_default()
                .to_owned();
            bail!(Refused { status, message });
        }
        Ok(body)
    }

    /// `GET /v1/users/ME/properties`: who we are.
    pub async fn me(&self) -> Result<Value> {
        self.send(self.chat(Method::GET, "/v1/users/ME/properties")?).await
    }

    /// `GET /v1/users/ME/conversations`: chats and channels, newest first.
    pub async fn conversations(&self) -> Result<Value> {
        let path =
            format!("/v1/users/ME/conversations?view=msnp24Equivalent&pageSize=100&startTime=0&targetType={TARGETS}");
        self.send(self.chat(Method::GET, &path)?).await
    }

    /// A page of messages, newest first; `before` (an id, which is its
    /// arrival time in ms) pages back, as purple-teams' `endTime` window.
    pub async fn messages(&self, conversation: &str, before: Option<&str>) -> Result<Value> {
        let mut path = format!(
            "/v1/users/ME/conversations/{}/messages?view=msnp24Equivalent&pageSize={PAGE}&startTime=1",
            seg(conversation)
        );
        if let Some(end) = before.and_then(|b| b.parse::<i64>().ok()) {
            path.push_str(&format!("&endTime={}", end - 1));
        }
        let mut result = self.send(self.chat(Method::GET, &path)?).await?;
        // The web client's scoped conversation address (msteams-mcp
        // chatsvc-messaging.ts). Include the parent when the reply page omits
        // it, so opening a thread always retains its channel context.
        if before.is_none()
            && let Some((channel, root)) = conversation.split_once(";messageid=")
            && !result["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|m| m["id"] == root)
        {
            let path = format!("/v1/users/ME/conversations/{}/messages/{}", seg(channel), seg(root));
            let parent = self.send(self.chat(Method::GET, &path)?).await?;
            if let Some(messages) = result["messages"].as_array_mut() {
                messages.push(parent);
            }
        }
        Ok(result)
    }

    /// Sends an HTML message; `client_id` comes back on its echo.
    pub async fn send_message(&self, conversation: &str, html: &str, client_id: &str, my_name: &str) -> Result<Value> {
        let path = format!("/v1/users/ME/conversations/{}/messages", seg(conversation));
        let body = json!({
            "clientmessageid": client_id,
            "content": html,
            "messagetype": "RichText/Html",
            "contenttype": "text",
            "imdisplayname": my_name,
        });
        self.send(self.chat(Method::POST, &path)?.json(&body)).await
    }

    /// ASM's documented sharing/file flow, as purple-teams sends it. One
    /// file in a 1:1 chat; channel files require a separate OneDrive flow.
    pub async fn send_file(
        &self,
        conversation: &str,
        text: &str,
        client_id: &str,
        my_name: &str,
        recipient: &str,
        files: &[godwit_core::application::Outgoing],
    ) -> Result<Value> {
        let [file] = files else {
            bail!("Send one Teams file at a time.");
        };
        let bytes = godwit_core::media::read_upload(file)
            .await
            .map_err(anyhow::Error::msg)?;
        let size = bytes.len();
        let mut permissions = serde_json::Map::new();
        permissions.insert(recipient.to_owned(), json!(["read"]));
        let token = self.tokens().skype;
        let created = self
            .send(
                self.http
                    .post("https://api.asm.skype.com/v1/objects")
                    .header(header::AUTHORIZATION, sensitive(format!("skype_token {token}"))?)
                    .json(&json!({"type":"sharing/file","filename":file.name.as_ref(),"permissions":permissions})),
            )
            .await?;
        let id = created["id"].as_str().context("Teams gave no upload object")?;
        let object = format!("https://api.asm.skype.com/v1/objects/{}", seg(id));
        self.send(
            self.http
                .put(format!("{object}/content/original"))
                .header(header::AUTHORIZATION, sensitive(format!("skype_token {token}"))?)
                .header(header::CONTENT_TYPE, "multipart/form-data")
                .body(bytes),
        )
        .await?;
        // ASM processes uploads asynchronously. This is bounded to the one
        // user-initiated transfer, with backoff; never a background poll.
        let mut ready = false;
        for seconds in [1, 2, 4, 8, 16] {
            tokio::time::sleep(Duration::from_secs(seconds)).await;
            let status = self
                .send(
                    self.http
                        .get(format!("{object}/views/original/status"))
                        .header(header::COOKIE, sensitive(format!("skypetoken_asm={token}"))?),
                )
                .await?;
            if status["content_state"] == "ready" {
                ready = true;
                break;
            }
            if status["content_state"] == "failed" {
                bail!("Teams couldn't process the attachment.");
            }
        }
        if !ready {
            bail!("Teams hasn't finished processing the attachment. Try again later.");
        }
        let caption = crate::html::convert(text, &crate::html::Mentions::new()).plain;
        let content = crate::media::file_message(&object, &file.name, size as u64, &caption);
        let path = format!("/v1/users/ME/conversations/{}/messages", seg(conversation));
        let mut result = self
            .send(self.chat(Method::POST, &path)?.json(&json!({
                "clientmessageid":client_id,"content":content,"messagetype":"RichText/Media_GenericFile",
                "contenttype":"text","imdisplayname":my_name,
            })))
            .await?;
        result["godwit_content"] = Value::String(content);
        Ok(result)
    }

    /// Soft-deletes one of our messages, as the web client does.
    pub async fn delete_message(&self, conversation: &str, message: &str) -> Result<()> {
        let path = format!(
            "/v1/users/ME/conversations/{}/messages/{}?behavior=softDelete",
            seg(conversation),
            seg(message)
        );
        self.send(self.chat(Method::DELETE, &path)?).await.map(drop)
    }

    pub async fn typing(&self, conversation: &str) -> Result<()> {
        let path = format!("/v1/users/ME/conversations/{}/messages", seg(conversation));
        let body = json!({"content": "", "messagetype": "Control/Typing", "contenttype": "Application/Message"});
        self.send(self.chat(Method::POST, &path)?.json(&body)).await.map(drop)
    }

    /// Marks read up to `message`: `consumptionhorizon` is
    /// `{id};{now in ms};{client message id}` (purple-teams).
    pub async fn mark_read(&self, conversation: &str, message: &str, client_id: &str) -> Result<()> {
        let path = format!(
            "/v1/users/ME/conversations/{}/properties?name=consumptionhorizon",
            seg(conversation)
        );
        let body = json!({"consumptionhorizon": format!("{message};{};{client_id}", now_ms())});
        self.send(self.chat(Method::PUT, &path)?.json(&body)).await.map(drop)
    }

    /// The chat service aggregator's view: every team with its channels.
    pub async fn teams(&self) -> Result<Value> {
        let Some(csa) = self.tokens().csa else {
            bail!("no token for teams and channels yet");
        };
        let url = format!(
            "{TEAMS}/api/csa/{}/api/v3/teams/users/me?isPrefetch=false&enableMembershipSummary=true",
            auth::geo(self.region())
        );
        let request = self
            .http
            .get(url)
            .header(header::AUTHORIZATION, sensitive(format!("Bearer {csa}"))?)
            .header(header::ACCEPT, "application/json");
        self.send(request).await
    }

    /// The organisation's people directory: everyone whose address has a
    /// "." (all members; not guests), as purple-teams lists it
    /// (`teams_get_friend_list`: POST `users/searchV2` with the body ".").
    /// Ids (MRIs) and names, at most a page of them.
    pub async fn directory(&self) -> Result<Vec<(String, String)>> {
        let t = self.tokens();
        let url = format!(
            "{TEAMS}/api/mt/{}/beta/users/searchV2?includeDLs=true&includeBots=true&enableGuest=true&source=newChat&skypeTeamsInfo=true",
            auth::geo(self.region())
        );
        let request = self
            .http
            .post(url)
            .header(header::AUTHORIZATION, sensitive(format!("Bearer {}", t.access))?)
            .header("x-skypetoken", sensitive(t.skype)?)
            .header(header::ACCEPT, "application/json")
            .header(header::CONTENT_TYPE, "text/plain")
            .body(".");
        Ok(directory_entries(&self.send(request).await?))
    }

    /// People's presence now (`POST presence.teams.microsoft.com/v1/presence/getpresence/`
    /// with `[{"mri": …}]`, purple-teams' `teams_lookup_contact_statuses`):
    /// MRI and availability (`Available`, `Away`, `Busy`, …). Up to 650 at once.
    pub async fn presence(&self, mris: &[String], endpoint: &str) -> Result<Vec<PresenceInfo>> {
        let Some(token) = self.tokens().presence else {
            bail!("no presence token yet");
        };
        let body: Vec<Value> = mris.iter().take(650).map(|m| json!({"mri": m})).collect();
        // The headers purple-teams' teams_post_or_get sends for the presence host.
        let request = self
            .http
            .post("https://presence.teams.microsoft.com/v1/presence/getpresence/")
            .header(header::AUTHORIZATION, sensitive(format!("Bearer {token}"))?)
            .header(header::ACCEPT, "application/json")
            .header("x-ms-client-user-agent", "Teams-V2-Desktop")
            .header("x-ms-correlation-id", "1")
            .header("x-ms-client-version", CLIENT_VERSION)
            .header("x-ms-endpoint-id", endpoint)
            .json(&body);
        Ok(presences(&self.send(request).await?))
    }

    fn presence_request(&self, method: Method, path: &str, body: &Value) -> Result<RequestBuilder> {
        let token = self.tokens().presence.context("no Teams presence token yet")?;
        Ok(self
            .http
            .request(method, format!("https://presence.teams.microsoft.com{path}"))
            .header(header::AUTHORIZATION, sensitive(format!("Bearer {token}"))?)
            .header(header::ACCEPT, "application/json")
            .header("x-ms-client-user-agent", "Teams-V2-Desktop")
            // As purple-teams' teams_post_or_get for the presence host.
            .header("x-ms-correlation-id", "1")
            .header("x-ms-client-version", CLIENT_VERSION)
            .header("x-ms-endpoint-id", self.endpoint())
            .json(body))
    }

    pub fn follow_presence(&self, ids: &[String]) {
        self.followed.send_if_modified(|current| {
            let before = current.len();
            for id in ids {
                if current.len() >= 650 {
                    break;
                }
                if id.starts_with("8:") && !current.contains(id) {
                    current.push(id.clone());
                }
            }
            current.len() != before
        });
    }
    pub fn presence_list(&self) -> tokio::sync::watch::Receiver<Vec<String>> {
        self.followed.subscribe()
    }

    /// Presence endpoints and payloads follow purple-teams teams_messages.c:
    /// teams_set_endpoint_statusid, teams_set_global_statusid, teams_set_idle.
    pub async fn register_presence(&self) -> Result<()> {
        let body = json!({"id":self.endpoint(),"availability":"Available","activity":"Available","activityReporting":"Transport","deviceType":"Desktop"});
        self.send(self.presence_request(Method::PUT, "/v1/me/endpoints/", &body)?)
            .await
            .map(drop)
    }
    pub async fn report_activity(&self) -> Result<()> {
        let body =
            json!({"endpointId":self.endpoint(),"isActive":self.active.load(std::sync::atomic::Ordering::Relaxed)});
        self.send(self.presence_request(Method::POST, "/v1/me/reportmyactivity/", &body)?)
            .await
            .map(drop)
    }
    pub async fn set_presence(&self, presence: godwit_core::members::Presence) -> Result<()> {
        use godwit_core::members::Presence;
        let availability = match presence {
            Presence::Online => "Available",
            Presence::Idle => "Away",
            Presence::Busy => "Busy",
            Presence::Focus => "DoNotDisturb",
            Presence::Invisible | Presence::Offline => "Offline",
            Presence::Unknown => bail!("Choose a Teams availability"),
        };
        self.send(self.presence_request(
            Method::PUT,
            "/v1/me/forceavailability/",
            &json!({"availability":availability}),
        )?)
        .await?;
        self.active.store(
            !matches!(presence, Presence::Idle | Presence::Invisible | Presence::Offline),
            std::sync::atomic::Ordering::Relaxed,
        );
        Ok(())
    }
    pub async fn set_status(&self, status: Option<&str>) -> Result<()> {
        // purple-teams teams_contacts.c::teams_set_mood_message.
        self.send(self.presence_request(Method::PUT,"/v1/me/publishnote",&json!({"message":crate::html::escape_text(status.unwrap_or_default()),"expiry":"9999-12-31T00:00:00.000Z"}))?).await.map(drop)
    }
    pub async fn subscribe_presence(&self, surl: &str, add: &[String]) -> Result<()> {
        // purple-teams teams_send_subscription_batch: at most 100 per batch,
        // never purging, exactly as the reference sends it.
        let body = json!({"trouterUri":format!("{}/TeamsUnifiedPresenceService",surl.trim_end_matches('/')),"subscriptionsToAdd":add.iter().take(100).map(|id| json!({"mri":id})).collect::<Vec<_>>(),"subscriptionsToRemove":[],"shouldPurgePreviousSubscriptions":false});
        self.send(self.presence_request(
            Method::POST,
            &format!("/v1/pubsub/subscriptions/{}", seg(self.endpoint())),
            &body,
        )?)
        .await
        .map(drop)
    }

    /// Display names for people's ids (`8:orgid:…`), from the middle tier.
    pub async fn names(&self, mris: &[String]) -> Result<Vec<(String, String)>> {
        if mris.is_empty() {
            return Ok(Vec::new());
        }
        let t = self.tokens();
        // The web client's body starts with an empty entry.
        let body: Vec<&str> = std::iter::once("").chain(mris.iter().map(String::as_str)).collect();
        let url = format!(
            "{TEAMS}/api/mt/{}/beta/users/fetchShortProfile?isMailAddress=false&canBeSmtpAddress=false&enableGuest=true&includeIBBarredUsers=true&skypeTeamsInfo=true&includeBots=true",
            auth::geo(self.region())
        );
        let request = self
            .http
            .post(url)
            .header(header::AUTHORIZATION, sensitive(format!("Bearer {}", t.access))?)
            .header("x-skypetoken", sensitive(t.skype)?)
            .header(header::ACCEPT, "application/json")
            .json(&body);
        Ok(short_profiles(&self.send(request).await?))
    }
}

/// `{"value": [{"mri", "displayName"}, …]}` (or the bare list).
pub fn short_profiles(v: &Value) -> Vec<(String, String)> {
    let list = if v["value"].is_array() { &v["value"] } else { v };
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let mri = p["mri"].as_str().filter(|s| !s.is_empty())?;
            let name = p["displayName"].as_str().filter(|s| !s.is_empty())?;
            Some((mri.to_owned(), name.to_owned()))
        })
        .collect()
}

/// `searchV2`'s answer: `results` (else `value`/`values`), each its
/// `nodeProfileData` or itself, with `mri` and `displayName` (purple-teams'
/// `create_search_results`).
pub fn directory_entries(v: &Value) -> Vec<(String, String)> {
    let list = ["results", "value", "values"]
        .iter()
        .find_map(|k| v[*k].as_array())
        .cloned()
        .unwrap_or_default();
    list.iter()
        .filter_map(|r| {
            let c = if r["nodeProfileData"].is_object() {
                &r["nodeProfileData"]
            } else {
                r
            };
            let mri = c["mri"].as_str().filter(|m| m.starts_with("8:"))?;
            let name = c["displayName"].as_str().filter(|n| !n.is_empty())?;
            Some((mri.to_owned(), name.to_owned()))
        })
        .collect()
}

/// `getpresence`'s answer: `[{"mri", "presence": {"availability"}}]`.
pub fn presences(v: &Value) -> Vec<PresenceInfo> {
    v.get("presence")
        .unwrap_or(v)
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let mri = p["mri"].as_str()?;
            let availability = p["presence"]["availability"].as_str()?;
            // The custom status text (purple-teams reads presence.note.message).
            let note = p
                .pointer("/presence/note/message")
                .or_else(|| p.pointer("/note/message"))
                .and_then(|n| n.as_str())
                .filter(|n| !n.is_empty())
                .map(|n| n.to_owned());
            Some((mri.to_owned(), availability.to_owned(), note))
        })
        .collect()
}

/// Teams' availability as Godwit's presence; anything with "Idle" in it is
/// away, as purple-teams treats it.
pub fn presence_of(availability: &str) -> godwit_core::members::Presence {
    use godwit_core::members::Presence;
    match availability {
        "Available" => Presence::Online,
        "Busy" | "BusyIdle" | "InACall" | "InAMeeting" | "Presenting" => Presence::Busy,
        "DoNotDisturb" => Presence::Focus,
        "Away" | "BeRightBack" => Presence::Idle,
        "Offline" | "OffWork" => Presence::Offline,
        a if a.contains("Idle") => Presence::Idle,
        _ => Presence::Unknown,
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_and_presence_answers_are_read() {
        let found = directory_entries(&json!({"value": [
            {"nodeProfileData": {"mri": "8:orgid:a", "displayName": "Ada"}},
            {"mri": "8:orgid:b", "displayName": "Bea"},
            {"mri": "28:bot", "displayName": "Bot"},
        ]}));
        assert_eq!(
            found,
            [
                ("8:orgid:a".to_owned(), "Ada".to_owned()),
                ("8:orgid:b".to_owned(), "Bea".to_owned())
            ]
        );
        let p = presences(&json!([
            {"mri": "8:orgid:a", "presence": {"availability": "Away", "note": {"message": "Lunch"}}},
            {"mri": "8:orgid:b", "presence": {"availability": "Available"}}
        ]));
        assert_eq!(
            p,
            [
                ("8:orgid:a".to_owned(), "Away".to_owned(), Some("Lunch".to_owned())),
                ("8:orgid:b".to_owned(), "Available".to_owned(), None)
            ]
        );
        use godwit_core::members::Presence;
        assert_eq!(presence_of("Available"), Presence::Online);
        assert_eq!(presence_of("BusyIdle"), Presence::Busy);
        assert_eq!(presence_of("AvailableIdle"), Presence::Idle);
        assert_eq!(presence_of("DoNotDisturb"), Presence::Focus);
    }

    #[test]
    fn conversation_ids_are_encoded_as_one_segment() {
        assert_eq!(seg("19:abc@thread.v2"), "19%3Aabc%40thread.v2");
        assert_eq!(
            seg("19:a@thread.tacv2;messageid=1"),
            "19%3Aa%40thread.tacv2%3Bmessageid%3D1"
        );
    }

    #[test]
    fn short_profiles_skip_entries_without_a_name() {
        let v = json!({"value": [
            {"mri": "8:orgid:a", "displayName": "Ada"},
            {"mri": "8:orgid:b", "displayName": ""},
            {"displayName": "no id"}
        ]});
        assert_eq!(short_profiles(&v), [("8:orgid:a".to_owned(), "Ada".to_owned())]);
    }
}
