// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The `POST /safety/report` body, as the official client builds it
//! (stoatchat/for-web, packages/client/components/modal/modals/ReportContent.tsx).

use godwit_core::report::{ReportReason, ReportTarget};
use serde_json::{Value, json};

/// The API's name for a reason (`ContentReportReason`/`UserReportReason`).
fn reason_name(reason: ReportReason) -> &'static str {
    match reason {
        ReportReason::Illegal => "Illegal",
        ReportReason::IllegalGoods => "IllegalGoods",
        ReportReason::IllegalExtortion => "IllegalExtortion",
        ReportReason::IllegalPornography => "IllegalPornography",
        ReportReason::IllegalHacking => "IllegalHacking",
        ReportReason::ExtremeViolence => "ExtremeViolence",
        ReportReason::PromotesHarm => "PromotesHarm",
        ReportReason::UnsolicitedSpam => "UnsolicitedSpam",
        ReportReason::Raid => "Raid",
        ReportReason::SpamAbuse => "SpamAbuse",
        ReportReason::ScamsFraud => "ScamsFraud",
        ReportReason::Malware => "Malware",
        ReportReason::Harassment => "Harassment",
        ReportReason::InappropriateProfile => "InappropriateProfile",
        ReportReason::Impersonation => "Impersonation",
        ReportReason::BanEvasion => "BanEvasion",
        ReportReason::Underage => "Underage",
        ReportReason::Other => "NoneSpecified",
    }
}

/// The request body for reporting `target`.
pub fn body(target: &ReportTarget, reason: ReportReason, detail: &str) -> Value {
    let report_reason = reason_name(reason);
    let content = match target {
        ReportTarget::User { user, message } => {
            let mut c = json!({ "type": "User", "id": &*user.raw, "report_reason": report_reason });
            if let (Some(message), Some(map)) = (message, c.as_object_mut()) {
                map.insert("message_id".into(), json!(&*message.0));
            }
            c
        }
        ReportTarget::Message { message, .. } => {
            json!({ "type": "Message", "id": &*message.0, "report_reason": report_reason })
        }
        ReportTarget::Server { server } => {
            json!({ "type": "Server", "id": &*server.raw, "report_reason": report_reason })
        }
    };
    json!({ "content": content, "additional_context": detail })
}

#[cfg(test)]
mod tests {
    use super::*;
    use godwit_core::ids::{AccountId, MessageId, RoomId, ServerId, UserId};
    use godwit_core::network::Network;

    fn account() -> AccountId {
        AccountId::new(Network::Stoat, "me")
    }

    #[test]
    fn user_report_with_a_message() {
        let target = ReportTarget::User {
            user: UserId::new(account(), "U1"),
            message: Some(MessageId("M1".into())),
        };
        assert_eq!(
            body(&target, ReportReason::Impersonation, "pretends to be staff"),
            json!({
                "content": { "type": "User", "id": "U1", "report_reason": "Impersonation", "message_id": "M1" },
                "additional_context": "pretends to be staff"
            })
        );
    }

    #[test]
    fn user_report_without_a_message_has_no_message_id() {
        let target = ReportTarget::User {
            user: UserId::new(account(), "U1"),
            message: None,
        };
        let b = body(&target, ReportReason::Other, "");
        assert_eq!(b["content"]["report_reason"], "NoneSpecified");
        assert!(b["content"].get("message_id").is_none());
    }

    #[test]
    fn message_and_server_reports() {
        let m = ReportTarget::Message {
            message: MessageId("M2".into()),
            room: RoomId::new(account(), "C1"),
        };
        assert_eq!(
            body(&m, ReportReason::ScamsFraud, "x")["content"],
            json!({ "type": "Message", "id": "M2", "report_reason": "ScamsFraud" })
        );
        let s = ReportTarget::Server {
            server: ServerId::new(account(), "S1"),
        };
        assert_eq!(
            body(&s, ReportReason::Raid, "")["content"],
            json!({ "type": "Server", "id": "S1", "report_reason": "Raid" })
        );
    }
}
