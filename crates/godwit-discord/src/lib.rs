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

/// Why Godwit doesn't report on Discord, with
/// sources. Every address was checked to load (HTTP 200).
pub fn report_notice() -> godwit_core::report::ReportNotice {
    let link = |label: &str, url: &str| (label.to_owned(), url.to_owned());
    godwit_core::report::ReportNotice {
        text: "Reporting isn't available in Godwit yet. To report something, use Discord's app or website.".to_owned(),
        links: vec![
            link("Discord's Terms of Service", "https://discord.com/terms"),
            link("Discord's Privacy Policy", "https://discord.com/privacy"),
            link("Discord's safety transparency reports", "https://discord.com/safety-transparency-reports"),
        ],
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
}
