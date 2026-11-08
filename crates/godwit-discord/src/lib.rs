// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Discord for Godwit: signing in by QR code (Discord's remote login, as its
//! web client's login page does), servers with their channels, DMs and group
//! DMs, friends, and messages, over Discord's gateway and HTTP API.
//!
//! Requests follow the web client's documented shapes and respect Discord's
//! rate limits. Nothing is sent without a user's action behind it, and there
//! is no polling. Nitro features are never offered.
//!
//! Large parts follow Yapfast's Discord crate (GPL-3.0-or-later,
//! <https://github.com/yapfast/yapfast>), ported to Godwit (AGPL-3.0-only).
//! Payload shapes come from <https://docs.discord.food>.

pub mod account;
pub mod api;
pub mod backend;
pub mod captcha;
pub mod client;
pub mod forum;
pub mod gateway;
pub mod limits;
pub mod markdown;
pub mod member_list;
pub mod member_search;
pub mod permissions;
pub mod remote_auth;
pub mod search;
pub mod session;
pub mod settings;
pub mod store;
pub mod voice;

// Sources, each fetched and checked to say what it is cited for (HTTP 200).
const APPEALS: &str = "https://discord.com/safety/360043712172-how-you-can-appeal-our-actions";

fn link(label: &str, url: &str) -> (String, String) {
    (label.to_owned(), url.to_owned())
}

/// Why Godwit doesn't report on Discord, with
/// sources. Every address was checked to load (HTTP 200, free to read); each
/// label carries the page's publication date.
pub fn report_notice() -> godwit_core::report::ReportNotice {
    godwit_core::report::ReportNotice {
        text: "Reporting isn't available in Godwit yet. To report something, use Discord's app or website.".to_owned(),
        links: vec![
            link("Discord's Terms of Service", "https://discord.com/terms"),
            link("Discord's Privacy Policy", "https://discord.com/privacy"),
            link("Discord's safety transparency reports", "https://discord.com/safety-transparency-reports"),
            link("How You Can Appeal Our Actions, Discord (16 Feb 2024)", APPEALS),
        ],
        privacy_overview: Some("Privacy overview in Settings".to_owned()),
        caveat: Some(godwit_core::privacy_overview::SOURCES_CAVEAT.to_owned()),
    }
}

/// Discord's section of Settings → Data & privacy. Every claim is attributed
/// to its source and says only what the source says; no source's date is
/// hidden, since things may have changed since. Discord's own pages come
/// first (its privacy policy sits in the official card above the overview),
/// then third-party sources; each says why it is listed.
pub fn privacy_section() -> godwit_core::privacy_overview::PrivacySection {
    use godwit_core::privacy_overview::{PrivacyParagraph, PrivacySection};
    let para = |text: &str, why: &str, sources: Vec<(String, String)>| PrivacyParagraph {
        text: text.to_owned(),
        why: why.to_owned(),
        sources,
    };
    PrivacySection {
        title: "Discord".to_owned(),
        intro: "Discord's own privacy documentation and its page on appeals.".to_owned(),
        paragraphs: vec![
            para(
                "Discord's own privacy policy: what it collects and keeps, who it shares it with, and that it describes end-to-end encryption for voice and video calls but not for text messages.",
                "Discord's own page, last updated 29 August 2025.",
                vec![link("Discord Privacy Policy (29 Aug 2025)", "https://discord.com/privacy")],
            ),
            para(
                "Discord's own help page describes how to appeal its actions on an account (document version dated 16 February 2024).",
                "Discord's own page: how its actions on an account can be appealed.",
                vec![link("How You Can Appeal Our Actions, Discord (16 Feb 2024)", APPEALS)],
            ),
        ],
        takeaway: "For private conversations, a network with end-to-end encryption is a better fit right now.".to_owned(),
        caveat: godwit_core::privacy_overview::SOURCES_CAVEAT.to_owned(),
    }
}

#[cfg(test)]
mod report_notice_tests {
    #[test]
    fn declines_reports_with_an_explanation() {
        let n = super::report_notice();
        assert!(n.text.starts_with("Reporting isn't available in Godwit yet. To report something, use Discord's app or website."));
        assert!(n.links.iter().all(|(_, u)| u.starts_with("https://")));
        // The account supplies it, and offers no report form.
        use godwit_core::account::NetworkAccount;
        let (a, _) = crate::account::DiscordAccount::demo(crate::store::test_ready(), Default::default());
        assert_eq!(a.report_notice().as_ref(), Some(&n));
    }

    #[test]
    fn notice_keeps_its_links_and_opens_the_overview() {
        let n = super::report_notice();
        assert!(
            n.links
                .iter()
                .any(|(_, u)| u == "https://discord.com/terms")
        );
        assert!(n.privacy_overview.is_some());
        assert!(n.caveat.is_some());
    }

    #[test]
    fn supplies_a_sourced_privacy_section() {
        use godwit_core::account::NetworkAccount;
        let s = super::privacy_section();
        assert!(s.is_sourced() && !s.caveat.is_empty());
        let (a, _) = crate::account::DiscordAccount::demo(crate::store::test_ready(), Default::default());
        assert_eq!(a.privacy_section().as_ref(), Some(&s));
    }
}
