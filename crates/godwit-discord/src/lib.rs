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
pub(crate) mod soundboard;
pub mod store;
pub mod voice;

fn link(label: &str, url: &str) -> (String, String) {
    (label.to_owned(), url.to_owned())
}

/// Shown where the report option would be: Discord's report form isn't built
/// yet, so it points to Discord's own app.
pub fn report_notice() -> godwit_core::report::ReportNotice {
    godwit_core::report::ReportNotice {
        text: "Reporting isn't available in Godwit yet. To report something, use Discord's app or website.".to_owned(),
        links: Vec::new(),
        privacy_overview: None,
        caveat: None,
    }
}

/// Discord's section of Settings → Data & privacy: links to Discord's own
/// privacy documentation only.
pub fn privacy_section() -> godwit_core::privacy_overview::PrivacySection {
    use godwit_core::privacy_overview::{PrivacyParagraph, PrivacySection};
    let para = |text: &str, why: &str, sources: Vec<(String, String)>| PrivacyParagraph {
        text: text.to_owned(),
        why: why.to_owned(),
        sources,
    };
    PrivacySection {
        title: "Discord".to_owned(),
        intro: "Discord's own privacy documentation.".to_owned(),
        paragraphs: vec![para(
            "",
            "Discord's own page.",
            vec![link(
                "Discord Privacy Policy (29 Aug 2025)",
                "https://discord.com/privacy",
            )],
        )],
        takeaway: "For private conversations, a network with end-to-end encryption is a better fit right now."
            .to_owned(),
        caveat: godwit_core::privacy_overview::SOURCES_CAVEAT.to_owned(),
    }
}

#[cfg(test)]
mod report_notice_tests {
    #[test]
    fn points_reports_to_discords_app() {
        let n = super::report_notice();
        assert!(n.text.starts_with("Reporting isn't available in Godwit yet"));
        assert!(n.links.is_empty());
        // The account supplies it, and offers no report form.
        use godwit_core::account::NetworkAccount;
        let (a, _) = crate::account::DiscordAccount::demo(crate::store::test_ready(), Default::default());
        assert_eq!(a.report_notice().as_ref(), Some(&n));
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
