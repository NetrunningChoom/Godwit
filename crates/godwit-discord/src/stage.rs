// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Stage channels: the voice-state changes that raise and lower a hand, let
//! someone speak or send them back to the audience. Sources: docs.discord.food,
//! resources/voice (Modify Current User Voice State, Modify User Voice State)
//! and resources/stage-instance.

use godwit_core::call::StageRequest;
use serde_json::{Value, json};

use crate::permissions::{MUTE_MEMBERS, REQUEST_TO_SPEAK};

/// The permission a request needs: asking to speak takes Request to Speak,
/// letting someone speak (ourselves included) takes Mute Members. Lowering
/// our hand and stepping down to the audience are always allowed, and
/// moving someone else down takes Mute Members.
pub fn needed(request: StageRequest, own: bool) -> u64 {
    match request {
        StageRequest::RequestToSpeak => REQUEST_TO_SPEAK,
        StageRequest::InviteToSpeak => MUTE_MEMBERS,
        StageRequest::LowerHand => 0,
        StageRequest::MoveToAudience if own => 0,
        StageRequest::MoveToAudience => MUTE_MEMBERS,
    }
}

/// The PATCH body: to `/voice-states/@me` for ourselves (a hand is a
/// `request_to_speak_timestamp`, now or null) and to `/voice-states/{user}`
/// for others (only `suppress`). `now` is an ISO 8601 time.
pub fn body(channel: &str, request: StageRequest, own: bool, now: &str) -> Option<Value> {
    Some(match (request, own) {
        (StageRequest::RequestToSpeak, true) => {
            json!({"channel_id": channel, "request_to_speak_timestamp": now})
        }
        (StageRequest::LowerHand, true) => {
            json!({"channel_id": channel, "request_to_speak_timestamp": null})
        }
        (StageRequest::InviteToSpeak, _) => json!({"channel_id": channel, "suppress": false}),
        (StageRequest::MoveToAudience, _) => json!({"channel_id": channel, "suppress": true}),
        // Only we can raise or lower our own hand.
        (StageRequest::RequestToSpeak | StageRequest::LowerHand, false) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_hand_bodies() {
        assert_eq!(
            body("5", StageRequest::RequestToSpeak, true, "2026-01-01T00:00:00Z"),
            Some(json!({"channel_id": "5", "request_to_speak_timestamp": "2026-01-01T00:00:00Z"}))
        );
        assert_eq!(
            body("5", StageRequest::LowerHand, true, "x"),
            Some(json!({"channel_id": "5", "request_to_speak_timestamp": null}))
        );
        assert_eq!(body("5", StageRequest::RequestToSpeak, false, "x"), None);
    }

    #[test]
    fn invite_and_move_bodies() {
        assert_eq!(
            body("5", StageRequest::InviteToSpeak, false, "x"),
            Some(json!({"channel_id": "5", "suppress": false}))
        );
        assert_eq!(
            body("5", StageRequest::MoveToAudience, true, "x"),
            Some(json!({"channel_id": "5", "suppress": true}))
        );
    }

    #[test]
    fn permissions_follow_the_docs() {
        assert_eq!(needed(StageRequest::RequestToSpeak, true), REQUEST_TO_SPEAK);
        assert_eq!(needed(StageRequest::InviteToSpeak, false), MUTE_MEMBERS);
        assert_eq!(needed(StageRequest::MoveToAudience, true), 0);
        assert_eq!(needed(StageRequest::MoveToAudience, false), MUTE_MEMBERS);
        assert_eq!(needed(StageRequest::LowerHand, true), 0);
    }
}
