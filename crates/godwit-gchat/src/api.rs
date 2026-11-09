// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Google Chat's web API, as chat.google.com calls it and maugclib
//! (mautrix-googlechat) documents it: protobuf `POST`s to
//! `/u/0/api/{endpoint}?c={n}&rt=b&alt=proto&key={API_KEY}`, carrying the
//! sign-in's cookies and the page's XSRF token, which `/u/0/mole/world`
//! hands out. Answers are base64 protobuf when asked for with
//! `X-Goog-Encode-Response-If-Executable`.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use protobuf::Message;
use reqwest::Client;
use reqwest::header::{self, HeaderValue};

use crate::proto;
use crate::session::Saved;

/// The name requests carry in the network layer's log.
const NET: &str = "gchat";

/// How the network layer treats an API endpoint: listing, getting and
/// catching up only read (guarded against repeating), the rest write.
fn class(endpoint: &str) -> godwit_core::net::Class {
    const READS: [&str; 5] = ["list_", "get_", "paginated_", "catch_up", "search"];
    if READS.iter().any(|r| endpoint.starts_with(r)) {
        godwit_core::net::Class::Read
    } else {
        godwit_core::net::Class::Write
    }
}

const BASE: &str = "https://chat.google.com/u/0";
/// Google's API key from the Hangouts web client, as published in hangups
/// (<https://github.com/tdryer/hangups/blob/master/hangups/client.py>) and
/// copied into maugclib (<https://github.com/mautrix/googlechat/blob/master/maugclib/client.py>):
/// without it Google answers 403 "Daily Limit for Unauthenticated Use Exceeded".
const API_KEY: &str = "AIzaSyD7InnYR3VKdb4j2rMUEbTCIr2VyEazl6k"; // not-a-secret: Google's Hangouts web-client key, public in hangups and maugclib
/// The web client's version, as maugclib sends it.
const CLIENT_VERSION: i64 = 2_440_378_181_258;
pub(crate) const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.1 Safari/605.1.15";

/// Chat refused a call as unauthorized (401/403).
#[derive(Debug)]
pub struct Unauthorized;

impl std::fmt::Display for Unauthorized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not authorized")
    }
}

impl std::error::Error for Unauthorized {}

/// Whether `e` means the sign-in is gone (not just unreachable).
pub fn signed_out(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Unauthorized>().is_some() || format!("{e:#}").contains("signed out")
}

/// A signed-in connection to the API.
pub struct Api {
    http: Client,
    cookies: HeaderValue,
    xsrf: Option<String>,
    request: u64,
    next_request: tokio::time::Instant,
}

impl Api {
    pub fn new(saved: &Saved) -> Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut cookies = HeaderValue::from_str(&crate::auth::cookie_header(saved)).context("unusable cookies")?;
        cookies.set_sensitive(true);
        let mut headers = header::HeaderMap::new();
        headers.insert(header::USER_AGENT, HeaderValue::from_static(USER_AGENT));
        let http = Client::builder()
            .default_headers(headers)
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()
            .context("could not set up HTTPS")?;
        Ok(Self {
            http,
            cookies,
            xsrf: None,
            request: 0,
            next_request: tokio::time::Instant::now(),
        })
    }

    /// Fetches the page's XSRF token (`SMqcke` in its `WIZ_global_data`);
    /// fails as signed out when Google sends its sign-in page instead.
    pub async fn refresh(&mut self) -> Result<()> {
        // The query the web client loads its roster frame with (maugclib);
        // `hs` is the Gmail shell's build, without which the page answers 400.
        let hs = r#"["h_hs",null,null,[1,0],null,null,"gmail.pinto-server_20230730.06_p0",1,null,[15,38,36,35,26,30,41,18,24,11,21,14,6],null,null,"3Mu86PSulM4.en..es5",0,null,null,[0]]"#;
        let url = format!(
            "{BASE}/mole/world?origin=https%3A%2F%2Fmail.google.com&shell=9&hl=en&wfi=gtn-roster-iframe-id&hs={}",
            godwit_core::encoding::percent_encode(hs)
        );
        let request = self
            .http
            .get(url)
            .header(header::COOKIE, self.cookies.clone())
            .header(header::REFERER, "https://mail.google.com/");
        let body = godwit_core::net::stream(NET, godwit_core::net::Class::Auth, request)
            .await
            .context("loading Chat")?
            .error_for_status()
            .context("loading Chat")?
            .text()
            .await
            .context("loading Chat")?;
        let wiz = wiz_data(&body).ok_or_else(|| anyhow!("Chat's page changed (no WIZ_global_data)"))?;
        if wiz.get("qwAQke").and_then(|v| v.as_str()) == Some("AccountsSignInUi") {
            bail!("signed out");
        }
        let token = wiz
            .get("SMqcke")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("no XSRF token"))?;
        self.xsrf = Some(token.to_string());
        Ok(())
    }

    /// One API call.
    /// One API call. Refused as unauthorized (the page's XSRF token
    /// expires), the token is fetched again once and the call retried; if
    /// that says signed out, it fails as signed out.
    pub async fn call<Rq: Message, Rs: Message>(&mut self, endpoint: &str, request: &Rq) -> Result<Rs> {
        match self.call_once(endpoint, request).await {
            Err(e) if e.downcast_ref::<Unauthorized>().is_some() => {
                self.refresh().await?;
                self.call_once(endpoint, request).await
            }
            other => other,
        }
    }

    async fn call_once<Rq: Message, Rs: Message>(&mut self, endpoint: &str, request: &Rq) -> Result<Rs> {
        tokio::time::sleep_until(self.next_request).await;
        self.next_request = tokio::time::Instant::now() + Duration::from_millis(300);
        self.request += 1;
        let url = format!("{BASE}/api/{endpoint}?c={}&rt=b&alt=proto&key={API_KEY}", self.request);
        let mut req = self
            .http
            .post(url)
            .header(header::COOKIE, self.cookies.clone())
            .header(header::CONTENT_TYPE, "application/x-protobuf")
            .header("x-goog-encode-response-if-executable", "base64")
            .body(request.write_to_bytes()?);
        if let Some(xsrf) = &self.xsrf {
            req = req.header("x-framework-xsrf-token", xsrf);
        }
        // `c` counts requests: it doesn't make a repeated call a new one.
        let answer = godwit_core::net::fetch_with(NET, class(endpoint), req, &["c"])
            .await
            .with_context(|| endpoint.to_string())?;
        let status = answer.status;
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let wait = godwit_core::backend::retry_after(answer.header(header::RETRY_AFTER), Duration::from_secs(60));
            self.next_request = godwit_core::backend::checked_deadline(wait);
            bail!("{endpoint}: rate limited; try again later");
        }
        let body = answer.body;
        if matches!(status.as_u16(), 401 | 403) {
            return Err(anyhow::Error::new(Unauthorized).context(format!("{endpoint}: HTTP {status}")));
        }
        if !status.is_success() {
            bail!("{endpoint}: HTTP {status}");
        }
        let raw = base64::engine::general_purpose::STANDARD
            .decode(body.trim_ascii())
            .unwrap_or_else(|_| body.to_vec());
        Rs::parse_from_bytes(&raw).with_context(|| format!("{endpoint}: unexpected answer"))
    }

    fn header() -> proto::RequestHeader {
        let mut h = proto::RequestHeader::new();
        h.set_client_type(proto::request_header::ClientType::WEB);
        h.set_client_version(CLIENT_VERSION);
        h
    }

    /// Who's signed in.
    pub async fn self_status(&mut self) -> Result<proto::GetSelfUserStatusResponse> {
        let mut req = proto::GetSelfUserStatusRequest::new();
        req.request_header = Some(Self::header()).into();
        self.call("get_self_user_status", &req).await
    }

    /// The web-client setters documented by purple-googlechat's
    /// googlechat_conversation.c::googlechat_set_status, using the protobuf
    /// definitions shared with maugclib. Never change other profile fields.
    pub async fn set_own_presence(&mut self, presence: godwit_core::members::Presence) -> Result<proto::UserStatus> {
        use godwit_core::members::Presence;
        if !matches!(presence, Presence::Online | Presence::Idle | Presence::Focus) {
            bail!("Choose a Google Chat availability");
        }
        let mut shared = proto::SetPresenceSharedRequest::new();
        shared.request_header = Some(Self::header()).into();
        shared.set_presence_shared(presence != Presence::Idle);
        let _: proto::SetPresenceSharedResponse = self.call("set_presence_shared", &shared).await?;
        let mut dnd = proto::SetDndDurationRequest::new();
        dnd.request_header = Some(Self::header()).into();
        dnd.set_current_dnd_state(if presence == Presence::Focus {
            proto::set_dnd_duration_request::State::DND
        } else {
            proto::set_dnd_duration_request::State::AVAILABLE
        });
        // The timestamp field is absolute. Use the duration alternative for
        // the reference client's 48-hour DND interval, rather than a 1970 date.
        dnd.set_new_dnd_duration_usec(if presence == Presence::Focus {
            172_800_000_000
        } else {
            0
        });
        let _: proto::SetDndDurationResponse = self.call("set_dnd_duration", &dnd).await?;
        self.self_status()
            .await?
            .user_status
            .into_option()
            .context("Google Chat returned no own status")
    }
    pub async fn set_custom_status(&mut self, text: Option<&str>) -> Result<proto::UserStatus> {
        let mut request = proto::SetCustomStatusRequest::new();
        request.request_header = Some(Self::header()).into();
        if let Some(text) = text.filter(|s| !s.is_empty()) {
            let mut custom = proto::CustomStatus::new();
            custom.set_status_text(text.to_owned());
            request.custom_status = Some(custom).into();
            request.set_custom_status_remaining_duration_usec(172_800_000_000);
        } else {
            request.set_custom_status_remaining_duration_usec(0);
        }
        let _: proto::SetCustomStatusResponse = self.call("set_custom_status", &request).await?;
        self.self_status()
            .await?
            .user_status
            .into_option()
            .context("Google Chat returned no own status")
    }

    /// DMs and spaces, newest first: up to `limit` of them.
    pub async fn world(&mut self, limit: i32) -> Result<Vec<proto::WorldItemLite>> {
        let mut req = proto::PaginatedWorldRequest::new();
        req.request_header = Some(Self::header()).into();
        req.set_fetch_from_user_spaces(true);
        req.fetch_options
            .push(proto::paginated_world_request::FetchOptions::EXCLUDE_GROUP_LITE.into());
        let mut section = proto::WorldSectionRequest::new();
        section.set_page_size(limit);
        req.world_section_requests.push(section);
        let resp: proto::PaginatedWorldResponse = self.call("paginated_world", &req).await?;
        let mut items = resp.world_items;
        items.sort_by_key(|i| std::cmp::Reverse(i.sort_timestamp()));
        Ok(items)
    }
}

impl Api {
    /// A conversation's newest topics (each with its first replies),
    /// oldest first.
    pub async fn topics(&mut self, group: &str, page: i32, before: Option<i64>) -> Result<proto::ListTopicsResponse> {
        let req = Self::topics_request(group, page, before)?;
        let mut resp: proto::ListTopicsResponse = self.call("list_topics", &req).await?;
        resp.topics.sort_by_key(|t| t.sort_time());
        Ok(resp)
    }

    fn topics_request(group: &str, page: i32, before: Option<i64>) -> Result<proto::ListTopicsRequest> {
        let mut req = proto::ListTopicsRequest::new();
        req.request_header = Some(Self::header()).into();
        req.group_id = Some(group_id(group).ok_or_else(|| anyhow!("not a Chat conversation"))?).into();
        req.set_page_size_for_topics(page);
        req.set_page_size_for_replies(page);
        if let Some(timestamp) = before {
            // As Chat's web client scrolling back: the oldest sort time shown
            // goes in the boundary, with sort option 2. The `*_not_older_than`
            // revisions are not a page cursor; they stay unset, as maugclib.
            let mut boundary = proto::ListTopicsBoundary::new();
            boundary.set_sort_time(timestamp);
            req.boundary = Some(boundary).into();
            req.set_sort_option(2);
        }
        Ok(req)
    }

    /// A topic's root and replies, as maugclib's proto_list_messages.
    pub async fn thread_messages(&mut self, group: &str, topic: &str, page: i32) -> Result<Vec<proto::Message>> {
        let request = Self::thread_messages_request(group, topic, page)?;
        let response: proto::ListMessagesResponse = self.call("list_messages", &request).await?;
        Ok(response.messages)
    }

    fn thread_messages_request(group: &str, topic: &str, page: i32) -> Result<proto::ListMessagesRequest> {
        let mut request = proto::ListMessagesRequest::new();
        request.request_header = Some(Self::header()).into();
        request.parent_id = Target {
            group: group.into(),
            topic: topic.into(),
            message: String::new(),
            created: 0,
        }
        .id()?
        .parent_id;
        request.set_page_size(page);
        Ok(request)
    }
    pub async fn history(&mut self, room: &str, page: i32, before: Option<i64>) -> Result<Vec<proto::Message>> {
        let (group, topic) = room_parts(room);
        if let Some(topic) = topic {
            self.thread_messages(group, topic, page).await
        } else {
            Ok(self
                .topics(group, page, before)
                .await?
                .topics
                .into_iter()
                .flat_map(|t| t.replies)
                .collect())
        }
    }

    pub async fn topic_history(
        &mut self,
        room: &str,
        page: i32,
        before: Option<i64>,
    ) -> Result<(Vec<proto::Message>, Option<i64>, bool)> {
        let (group, topic) = room_parts(room);
        if let Some(topic) = topic {
            let messages = self.thread_messages(group, topic, page).await?;
            let cursor = messages.iter().map(proto::Message::create_time).min();
            let reached_start = messages.len() < page as usize;
            return Ok((messages, cursor, reached_start));
        }
        let response = self.topics(group, page, before).await?;
        let cursor = response.topics.iter().map(proto::Topic::sort_time).min();
        let reached_start = response.contains_first_topic();
        let messages = response.topics.into_iter().flat_map(|topic| topic.replies).collect();
        Ok((messages, cursor, reached_start))
    }
    pub async fn mark_read(&mut self, group: &str, timestamp: i64) -> Result<()> {
        // maugclib/client.py::update_read_timestamp, in microseconds.
        let mut request = proto::MarkGroupReadstateRequest::new();
        request.request_header = Some(Self::header()).into();
        request.id = Some(group_id(group).context("not a Google Chat conversation")?).into();
        request.set_last_read_time(timestamp);
        let _: proto::MarkGroupReadstateResponse = self.call("mark_group_readstate", &request).await?;
        Ok(())
    }

    /// Sends `text` as a new topic (Chat's unthreaded message), with our
    /// `local_id` so its echo can be matched.
    pub async fn send(
        &mut self,
        group: &str,
        text: &str,
        local_id: &str,
        files: &[godwit_core::application::Outgoing],
        reply_to: Option<&Target>,
    ) -> Result<proto::Message> {
        let (group, thread) = room_parts(group);
        let mut req = proto::CreateTopicRequest::new();
        // A quote reply, as maugclib's send_message: the message replied to,
        // and when it was written.
        if let Some(target) = reply_to {
            let mut reply = proto::SendReplyTarget::new();
            reply.id = Some(target.id()?).into();
            reply.set_create_time(target.created);
            let mut info = proto::MessageInfo::new();
            info.set_accept_format_annotations(true);
            info.reply_to = Some(reply).into();
            req.message_info = Some(info).into();
        }
        req.request_header = Some(Self::header()).into();
        req.group_id = Some(group_id(group).ok_or_else(|| anyhow!("not a Chat conversation"))?).into();
        req.set_text_body(text.to_owned());
        req.set_local_id(local_id.to_owned());
        // As maugclib: the current history mode, and plain text only.
        req.set_history_v2(true);
        if files.len() > 10 {
            bail!("Attach at most ten files at once.");
        }
        for file in files {
            let upload = self.upload(group, file).await?;
            let mut annotation = proto::Annotation::new();
            annotation.set_type(proto::AnnotationType::UPLOAD_METADATA);
            annotation.set_chip_render_type(proto::annotation::ChipRenderType::RENDER);
            annotation.set_upload_metadata(upload);
            req.annotations.push(annotation);
        }
        if let Some(topic) = thread {
            let mut reply = proto::CreateMessageRequest::new();
            reply.request_header = req.request_header;
            reply.parent_id = Target {
                group: group.into(),
                topic: topic.into(),
                message: String::new(),
                created: 0,
            }
            .id()?
            .parent_id;
            reply.set_text_body(text.to_owned());
            reply.set_local_id(local_id.to_owned());
            reply.annotations = req.annotations;
            reply.message_info = req.message_info;
            let response: proto::CreateMessageResponse = self.call("create_message", &reply).await?;
            response
                .message
                .into_option()
                .context("Google Chat returned no sent reply")
        } else {
            let response: proto::CreateTopicResponse = self.call("create_topic", &req).await?;
            response
                .topic
                .replies
                .first()
                .cloned()
                .context("Google Chat returned no sent message")
        }
    }

    /// maugclib's resumable upload: start, then upload/finalize, then put
    /// the returned protobuf metadata on the message. Neither URL is logged.
    async fn upload(&self, group: &str, file: &godwit_core::application::Outgoing) -> Result<proto::UploadMetadata> {
        let bytes = godwit_core::media::read_upload(file)
            .await
            .map_err(anyhow::Error::msg)?;
        let group = group.split_once(':').context("not a Chat conversation")?.1;
        let mime = godwit_core::media::mime(&file.name);
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(120))
            .build()
            .context("could not prepare upload")?;
        let mut request = client
            .post("https://chat.google.com/uploads")
            .query(&[("group_id", group)])
            .header(header::COOKIE, self.cookies.clone())
            .header("x-goog-upload-protocol", "resumable")
            .header("x-goog-upload-command", "start")
            .header("x-goog-upload-content-length", bytes.len().to_string())
            .header("x-goog-upload-content-type", mime)
            .header("x-goog-upload-file-name", file.name.as_ref());
        if let Some(xsrf) = &self.xsrf {
            request = request.header("x-framework-xsrf-token", xsrf);
        }
        let answer = godwit_core::net::stream(NET, godwit_core::net::Class::Write, request)
            .await
            .map_err(|_| anyhow!("Could not start the upload."))?;
        if !answer.status().is_success() {
            bail!("Google Chat refused the upload ({}).", answer.status());
        }
        let url = answer
            .headers()
            .get("x-goog-upload-url")
            .and_then(|u| u.to_str().ok())
            .and_then(godwit_core::media::https)
            .context("Google Chat gave no upload URL")?;
        if url.host_str() != Some("chat.google.com") {
            bail!("Google Chat gave an unexpected upload host.");
        }
        let answer = client
            .put(url)
            .header(header::COOKIE, self.cookies.clone())
            .header("x-goog-upload-command", "upload, finalize")
            .header("x-goog-upload-protocol", "resumable")
            .header("x-goog-upload-offset", "0")
            .body(bytes);
        let answer = godwit_core::net::stream(NET, godwit_core::net::Class::Live, answer)
            .await
            .map_err(|_| anyhow!("Could not upload the attachment."))?;
        if !answer.status().is_success() {
            bail!("Google Chat refused the attachment ({}).", answer.status());
        }
        if answer.content_length().is_some_and(|len| len > 1024 * 1024) {
            bail!("Upload answer too large.");
        }
        let body = answer
            .bytes()
            .await
            .map_err(|_| anyhow!("Could not read upload metadata."))?;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(body.trim_ascii())
            .context("unusable upload metadata")?;
        proto::UploadMetadata::parse_from_bytes(&raw).context("unusable upload metadata")
    }

    /// Changes our message's text (maugclib's edit_message).
    pub async fn edit(&mut self, target: &Target, text: &str) -> Result<()> {
        let mut req = proto::EditMessageRequest::new();
        req.request_header = Some(Self::header()).into();
        req.message_id = Some(target.id()?).into();
        req.set_text_body(text.to_owned());
        let mut info = proto::MessageInfo::new();
        info.set_accept_format_annotations(true);
        req.message_info = Some(info).into();
        let _: proto::EditMessageResponse = self.call("edit_message", &req).await?;
        Ok(())
    }

    /// Deletes our message (maugclib's delete_message).
    pub async fn delete(&mut self, target: &Target) -> Result<()> {
        let mut req = proto::DeleteMessageRequest::new();
        req.request_header = Some(Self::header()).into();
        req.message_id = Some(target.id()?).into();
        let _: proto::DeleteMessageResponse = self.call("delete_message", &req).await?;
        Ok(())
    }

    /// Adds or removes our reaction (maugclib's update_reaction).
    pub async fn react(&mut self, target: &Target, emoji: &str, add: bool) -> Result<()> {
        let mut req = proto::UpdateReactionRequest::new();
        req.request_header = Some(Self::header()).into();
        req.message_id = Some(target.id()?).into();
        let mut e = proto::Emoji::new();
        e.set_unicode(emoji.to_owned());
        req.emoji = Some(e).into();
        req.set_type(if add {
            proto::update_reaction_request::ReactionUpdateType::ADD
        } else {
            proto::update_reaction_request::ReactionUpdateType::REMOVE
        });
        let _: proto::UpdateReactionResponse = self.call("update_reaction", &req).await?;
        Ok(())
    }

    /// People's presence now (maugclib's get_user_presence): active, away,
    /// or busy (Do not disturb).
    pub async fn presence(&mut self, ids: &[String]) -> Result<Vec<proto::UserPresence>> {
        let mut req = proto::GetUserPresenceRequest::new();
        req.request_header = Some(Self::header()).into();
        for id in ids {
            let mut user = proto::UserId::new();
            user.set_id(id.clone());
            req.user_ids.push(user);
        }
        req.set_include_active_until(false);
        req.set_include_user_status(true);
        let resp: proto::GetUserPresenceResponse = self.call("get_user_presence", &req).await?;
        Ok(resp.user_presences)
    }

    /// People's names and pictures.
    pub async fn users(&mut self, ids: &[String]) -> Result<Vec<proto::User>> {
        let mut req = proto::GetMembersRequest::new();
        req.request_header = Some(Self::header()).into();
        for id in ids {
            let mut user = proto::UserId::new();
            user.set_id(id.clone());
            let mut member = proto::MemberId::new();
            member.set_user_id(user);
            req.member_ids.push(member);
        }
        let resp: proto::GetMembersResponse = self.call("get_members", &req).await?;
        Ok(resp
            .members
            .iter()
            .filter(|m| m.has_user())
            .map(|m| m.user().clone())
            .collect())
    }
}

/// A message the API acts on: its conversation, topic (Chat's thread) and
/// id, and when it was written (replies carry it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub group: String,
    pub topic: String,
    pub message: String,
    pub created: i64,
}

impl Target {
    /// The API's message id: the message within its topic in its group.
    pub fn id(&self) -> Result<proto::MessageId> {
        let mut topic = proto::TopicId::new();
        topic.group_id = Some(group_id(&self.group).ok_or_else(|| anyhow!("not a Chat conversation"))?).into();
        topic.set_topic_id(self.topic.clone());
        let mut parent = proto::MessageParentId::new();
        parent.set_topic_id(topic);
        let mut id = proto::MessageId::new();
        id.parent_id = Some(parent).into();
        id.set_message_id(self.message.clone());
        Ok(id)
    }
}

/// Local room addressing: base conversations and their topic view.
pub fn room_parts(raw: &str) -> (&str, Option<&str>) {
    match raw.split_once('#') {
        Some((group, topic)) => (group, Some(topic)),
        None => (raw, None),
    }
}

/// `dm:…` / `space:…` (maugclib's conversation ids) as the API's group.
pub fn group_id(raw: &str) -> Option<proto::GroupId> {
    let mut group = proto::GroupId::new();
    if let Some(id) = raw.strip_prefix("dm:") {
        let mut dm = proto::DmId::new();
        dm.set_dm_id(id.to_owned());
        group.set_dm_id(dm);
    } else {
        let mut space = proto::SpaceId::new();
        space.set_space_id(raw.strip_prefix("space:")?.to_owned());
        group.set_space_id(space);
    }
    Some(group)
}

/// The API's group as `dm:…` / `space:…`.
pub fn group_raw(group: &proto::GroupId) -> Option<String> {
    if group.has_dm_id() {
        Some(format!("dm:{}", group.dm_id().dm_id()))
    } else if group.has_space_id() {
        Some(format!("space:{}", group.space_id().space_id()))
    } else {
        None
    }
}

/// Chat's presence as Godwit's: Do not disturb while active is busy, an
/// inactive person is away, and hidden or unknown shows no dot.
pub fn presence(p: &proto::UserPresence) -> godwit_core::members::Presence {
    use godwit_core::members::Presence;
    match p.presence() {
        proto::Presence::ACTIVE if p.dnd_state() == proto::DndState_State::DND => Presence::Busy,
        proto::Presence::ACTIVE => Presence::Online,
        proto::Presence::INACTIVE => Presence::Idle,
        _ => Presence::Unknown,
    }
}

/// The page's `window.WIZ_global_data = {…};` object.
fn wiz_data(page: &str) -> Option<serde_json::Value> {
    let start = page.find(">window.WIZ_global_data = ")? + ">window.WIZ_global_data = ".len();
    let end = start + page[start..].find(";</script>")?;
    serde_json::from_str(&page[start..end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_names_its_message_within_its_topic_and_group() {
        let target = Target {
            group: "space:AAA".into(),
            topic: "t1".into(),
            message: "m1".into(),
            created: 5,
        };
        let id = target.id().unwrap();
        assert_eq!(id.message_id(), "m1");
        let topic = id.parent_id.topic_id();
        assert_eq!(topic.topic_id(), "t1");
        assert_eq!(group_raw(&topic.group_id).as_deref(), Some("space:AAA"));
    }

    #[test]
    fn chat_presence_maps_to_godwits() {
        use godwit_core::members::Presence;
        let p = |presence, dnd| {
            let mut u = proto::UserPresence::new();
            u.set_presence(presence);
            u.set_dnd_state(dnd);
            super::presence(&u)
        };
        assert_eq!(
            p(proto::Presence::ACTIVE, proto::DndState_State::AVAILABLE),
            Presence::Online
        );
        assert_eq!(p(proto::Presence::ACTIVE, proto::DndState_State::DND), Presence::Busy);
        assert_eq!(
            p(proto::Presence::INACTIVE, proto::DndState_State::AVAILABLE),
            Presence::Idle
        );
        assert_eq!(
            p(proto::Presence::SHARING_DISABLED, proto::DndState_State::AVAILABLE),
            Presence::Unknown
        );
    }

    #[test]
    fn conversation_ids_round_trip() {
        for raw in ["dm:abc", "space:AAAA"] {
            assert_eq!(group_raw(&group_id(raw).unwrap()).as_deref(), Some(raw));
        }
        assert!(group_id("room:x").is_none());
    }

    #[test]
    fn older_topic_pages_carry_the_boundary_not_revisions() {
        let newest = Api::topics_request("space:AAQA", 30, None).unwrap();
        assert!(newest.boundary.is_none());
        assert!(!newest.has_sort_option());
        let older = Api::topics_request("space:AAQA", 30, Some(123_456)).unwrap();
        assert_eq!(older.boundary.sort_time(), 123_456);
        assert_eq!(older.sort_option(), 2);
        assert!(older.group_not_older_than.is_none() && older.user_not_older_than.is_none());
    }

    #[test]
    fn thread_history_requests_expand_the_list_messages_prefix() {
        use protobuf::Message as _;
        let request = Api::thread_messages_request("space:AAQA", "topic", 60).unwrap();
        assert_eq!(request.page_size(), 60);
        assert_eq!(request.parent_id.topic_id().topic_id(), "topic");
        let bytes = request.write_to_bytes().unwrap();
        let decoded = proto::ListMessagesRequest::parse_from_bytes(&bytes).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn the_pages_globals_are_found() {
        let page = r#"<script nonce="x">window.WIZ_global_data = {"SMqcke":"tok","qwAQke":"Chat"};</script>"#;
        let wiz = wiz_data(page).unwrap();
        assert_eq!(wiz["SMqcke"], "tok");
        assert!(wiz_data("<html></html>").is_none());
    }
}
