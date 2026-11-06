// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Stoat's API errors in plain words. The server answers failures with
//! `{"type": "…"}` (the `Error` schema in `https://stoat.chat/api/openapi.json`);
//! these sentences say what happened and, where there is one, what to do.
//! The status code stays at the end in brackets: useful when reporting a bug,
//! and what the sign-in flow checks for (HTTP 401).

use std::fmt;

/// A failed REST request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    /// The error's `type`, when the body has one.
    pub kind: Option<String>,
    /// `MissingPermission` / `MissingUserPermission`: which one.
    pub permission: Option<String>,
}

impl ApiError {
    pub fn new(status: u16, body: &str) -> Self {
        let json: Option<serde_json::Value> = serde_json::from_str(body).ok();
        let field = |name: &str| {
            json.as_ref()
                .and_then(|v| v.get(name))
                .and_then(serde_json::Value::as_str)
                // Only short identifiers: the body is the server's, not ours.
                .filter(|s| s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric()))
                .map(str::to_owned)
        };
        Self {
            status,
            kind: field("type"),
            permission: field("permission"),
        }
    }

    fn sentence(&self) -> String {
        let s = match self.kind.as_deref() {
            Some("MissingUserPermission") if self.permission.as_deref() == Some("SendMessage") => {
                "Stoat only lets friends message each other: send a friend request first"
            }
            Some("MissingUserPermission") => "they don't allow that",
            Some("MissingPermission") => {
                return match &self.permission {
                    Some(p) => format!("you need the {} permission here", spaced(p)),
                    None => "you don't have permission for that here".into(),
                };
            }
            Some("NotFriends") => "you aren't friends",
            Some("UnknownUser") => "no one on Stoat goes by that name",
            Some("AlreadyFriends") => "you're friends already",
            Some("AlreadySentRequest") => "you've sent them a request already",
            Some("Blocked") => "you've blocked them",
            Some("BlockedByOther") => "they've blocked you",
            Some("TooManyPendingFriendRequests") => "too many friend requests are waiting for an answer",
            Some("InvalidUsername") => "that isn't a username: it looks like name#1234",
            Some("UsernameTaken") => "that username is taken",
            Some("InvalidCredentials") => "wrong email or password",
            Some("InvalidSession" | "NotAuthenticated" | "InvalidToken") => "signed out: sign in again",
            Some("UnverifiedAccount") => "the account's email isn't verified yet",
            Some("LockedOut") => "too many tries: the account is locked for a while",
            Some("CaptchaFailed") => "Stoat wants a captcha, which Godwit can't show yet",
            Some("UnknownChannel") => "that channel is gone",
            Some("UnknownMessage") => "that message is gone",
            Some("UnknownServer") => "that server is gone",
            Some("UnknownAttachment") => "that file is gone",
            Some("CannotEditMessage") => "only your own messages can be edited",
            Some("CannotDeleteMessage") => "that message can't be deleted",
            Some("EmptyMessage") => "the message is empty",
            Some("TooManyAttachments") => "too many files for one message",
            Some("TooManyEmbeds") => "too many embeds for one message",
            Some("TooManyReplies") => "too many replies for one message",
            Some("PayloadTooLarge") => "the message is too long",
            Some("FileTooLarge") => "the file is too large",
            Some("FileTooSmall") => "the file is empty",
            Some("FileTypeNotAllowed") => "that kind of file isn't allowed here",
            Some("InSlowmode") => "slow mode is on: wait a moment before sending again",
            Some("Banned") => "you're banned from that server",
            Some("TooManyServers") => "you're in as many servers as Stoat allows",
            Some("AlreadyInServer") => "you're in that server already",
            Some("CantCreateServers") => "this account can't create servers",
            Some("InvalidInvite" | "MissingInvite") => "the invite isn't valid (any more)",
            Some("GroupTooLarge") => "the group is full",
            Some("AlreadyInGroup") => "they're in the group already",
            Some("NotInGroup") => "they aren't in the group",
            Some("CannotJoinCall" | "NotAVoiceChannel") => "that isn't a call you can join",
            Some("AlreadyConnected") => "you're in that call already",
            Some("LiveKitUnavailable" | "VosoUnavailable" | "UnknownNode") => {
                "voice isn't available on Stoat right now"
            }
            Some("FeatureDisabled") => "that's switched off on this Stoat server",
            Some("NoEmbedData") => "no preview for that link",
            Some("NotFound") => "it isn't there (any more)",
            Some("DatabaseError" | "InternalError" | "OperationFailed" | "ProxyError" | "RenderFail") => {
                "Stoat had a problem on its side: try again later"
            }
            Some(other) => return format!("the server said {}", spaced(other)),
            None if self.status == 429 => "Stoat asked to slow down: try again in a moment",
            None if self.status >= 500 => "Stoat had a problem on its side: try again later",
            None if self.status == 404 => "it isn't there (any more)",
            None => "the request was refused",
        };
        s.to_owned()
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (HTTP {})", self.sentence(), self.status)
    }
}

impl std::error::Error for ApiError {}

/// `SendMessage` → `send message`.
fn spaced(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push(' ');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// Opening a direct message failed: Stoat refuses DMs between people who
/// aren't friends with `MissingPermission` / `SendMessage` (seen live), which
/// reads better as what to do about it.
pub fn describe_dm(err: &anyhow::Error) -> String {
    match err.downcast_ref::<ApiError>() {
        Some(e) if e.permission.as_deref() == Some("SendMessage") => format!(
            "Stoat only lets friends message each other: send a friend request first (HTTP {})",
            e.status
        ),
        _ => describe(err),
    }
}

/// Whether the server turned down a password, code or token (sign-in and
/// two-factor forms say "that didn't match" instead).
pub fn auth_rejected(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ApiError>()
        .is_some_and(|e| e.status == 401 || matches!(e.kind.as_deref(), Some("InvalidCredentials" | "InvalidToken")))
}

/// Any failure in words: an API error's sentence, "can't reach Stoat" for
/// connection trouble, or the error itself.
pub fn describe(err: &anyhow::Error) -> String {
    if let Some(api) = err.downcast_ref::<ApiError>() {
        return api.to_string();
    }
    if let Some(e) = err.chain().find_map(|e| e.downcast_ref::<reqwest::Error>()) {
        if e.is_connect() || e.is_timeout() {
            return "can't reach Stoat: check the connection".into();
        }
        if e.is_decode() {
            return "Stoat answered with something Godwit doesn't understand".into();
        }
    }
    format!("{err:#}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_read_as_sentences_and_keep_the_status() {
        let dm = ApiError::new(403, r#"{"type":"MissingUserPermission","permission":"SendMessage"}"#);
        assert_eq!(
            dm.to_string(),
            "Stoat only lets friends message each other: send a friend request first (HTTP 403)"
        );
        let perm = ApiError::new(403, r#"{"type":"MissingPermission","permission":"ManageMessages"}"#);
        assert_eq!(
            perm.to_string(),
            "you need the manage messages permission here (HTTP 403)"
        );
        assert!(ApiError::new(401, "").to_string().ends_with("(HTTP 401)"));
        assert_eq!(
            ApiError::new(400, r#"{"type":"SomethingNew"}"#).to_string(),
            "the server said something new (HTTP 400)"
        );
        assert!(
            ApiError::new(429, "not json")
                .to_string()
                .starts_with("Stoat asked to slow down")
        );
    }

    #[test]
    fn refused_dms_suggest_a_friend_request() {
        // The body Stoat sent when messaging a server member who isn't a friend.
        let body = r#"{"type":"MissingPermission","permission":"SendMessage","location":"crates/core/permissions/src/models/mod.rs:69:17"}"#;
        let e = anyhow::Error::new(ApiError::new(403, body));
        assert!(describe_dm(&e).starts_with("Stoat only lets friends message each other"));
        assert!(describe(&e).starts_with("you need the send message permission"));
    }

    #[test]
    fn rejected_codes_are_recognised_by_type_or_status() {
        let typed = anyhow::Error::new(ApiError::new(400, r#"{"type":"InvalidToken"}"#));
        assert!(auth_rejected(&typed));
        assert!(auth_rejected(&anyhow::Error::new(ApiError::new(401, ""))));
        assert!(!auth_rejected(&anyhow::Error::new(ApiError::new(403, ""))));
    }

    #[test]
    fn odd_bodies_never_reach_the_sentence() {
        let e = ApiError::new(400, r#"{"type":"<script>alert(1)</script>"}"#);
        assert_eq!(e.kind, None);
        let described = describe(&anyhow::Error::new(e));
        assert!(!described.contains('<'));
    }
}
