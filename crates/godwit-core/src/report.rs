// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Reporting users, messages and servers to a network's safety team.

use crate::ids::{MessageId, RoomId, ServerId, UserId};

/// What a network shows in place of its report form when it declines
/// reports (its own wording and sources; core never writes them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportNotice {
    pub text: String,
    /// (label, address) pairs, opened in the browser.
    pub links: Vec<(String, String)>,
    /// Label of a button that opens the account's privacy overview in
    /// Settings (an internal target, not an address), if the network has one.
    pub privacy_overview: Option<String>,
    /// Shown under the links: how current the sources are.
    pub caveat: Option<String>,
}

/// What is being reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReportTarget {
    /// Someone's account, optionally with the message that shows why.
    User {
        user: UserId,
        message: Option<MessageId>,
    },
    Message {
        message: MessageId,
        room: RoomId,
    },
    Server {
        server: ServerId,
    },
}

impl ReportTarget {
    /// The reasons to offer: a user report has its own list.
    pub fn reasons(&self) -> &'static [ReportReason] {
        match self {
            Self::User { .. } => ReportReason::USER,
            _ => ReportReason::CONTENT,
        }
    }

    /// The word for what is reported, for dialog titles.
    pub fn noun(&self) -> &'static str {
        match self {
            Self::User { .. } => "user",
            Self::Message { .. } => "message",
            Self::Server { .. } => "server",
        }
    }
}

/// Why something is reported. The lists and labels are the official
/// client's ReportContent modal (stoatchat/for-web,
/// packages/client/components/modal/modals/ReportContent.tsx).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReportReason {
    Illegal,
    IllegalGoods,
    IllegalExtortion,
    IllegalPornography,
    IllegalHacking,
    ExtremeViolence,
    PromotesHarm,
    UnsolicitedSpam,
    Raid,
    SpamAbuse,
    ScamsFraud,
    Malware,
    Harassment,
    InappropriateProfile,
    Impersonation,
    BanEvasion,
    Underage,
    Other,
}

impl ReportReason {
    /// For messages and servers.
    pub const CONTENT: &'static [Self] = &[
        Self::Illegal,
        Self::IllegalGoods,
        Self::IllegalExtortion,
        Self::IllegalPornography,
        Self::IllegalHacking,
        Self::ExtremeViolence,
        Self::PromotesHarm,
        Self::UnsolicitedSpam,
        Self::Raid,
        Self::SpamAbuse,
        Self::ScamsFraud,
        Self::Malware,
        Self::Harassment,
        Self::Other,
    ];
    /// For users.
    pub const USER: &'static [Self] = &[
        Self::UnsolicitedSpam,
        Self::SpamAbuse,
        Self::InappropriateProfile,
        Self::Impersonation,
        Self::BanEvasion,
        Self::Underage,
        Self::Other,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Illegal => "Content breaks one or more laws",
            Self::IllegalGoods => "Drugs or illegal goods",
            Self::IllegalExtortion => "Extortion or blackmail",
            Self::IllegalPornography => "Revenge or underage pornography",
            Self::IllegalHacking => "Illegal hacking or cracking",
            Self::ExtremeViolence => "Extreme violence, gore or animal cruelty",
            Self::PromotesHarm => "Promotes harm",
            Self::UnsolicitedSpam => "Unsolicited advertising or spam",
            Self::Raid => "Raid or spam attack",
            Self::SpamAbuse => "Spam or similar platform abuse",
            Self::ScamsFraud => "Scams or fraud",
            Self::Malware => "Malware or phishing",
            Self::Harassment => "Harassment or cyberbullying",
            Self::InappropriateProfile => "User's profile has inappropriate content",
            Self::Impersonation => "Impersonation",
            Self::BanEvasion => "Ban evasion",
            Self::Underage => "Not of minimum age to use the platform",
            Self::Other => "Other",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_are_the_official_ones() {
        assert_eq!(ReportReason::CONTENT.len(), 14);
        assert_eq!(ReportReason::USER.len(), 7);
        assert!(!ReportReason::USER.contains(&ReportReason::Malware));
        assert!(!ReportReason::CONTENT.contains(&ReportReason::Underage));
        assert_eq!(ReportReason::Other.label(), "Other");
    }
}
