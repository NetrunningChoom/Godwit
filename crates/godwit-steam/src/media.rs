// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Request policy for Steam's public avatars and images.

use godwit_core::hooks::Hooks;

pub fn hooks() -> Hooks {
    Hooks {
        // An empty header list explicitly opts out of the generic user agent,
        // matching Steam's sign-ins (auth.rs) and the previous image loader.
        image_headers: |url| anonymous_host(url).then(Vec::new),
        ..Hooks::default()
    }
}

/// Hosts that aren't told this is Godwit: Steam's (its avatars and images),
/// as its sign-ins don't name Godwit either (godwit-steam's `auth`).
fn anonymous_host(url: &str) -> bool {
    let host = url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        // Userinfo (`host@elsewhere`) would name another server: never ours.
        .filter(|authority| !authority.contains('@'))
        .and_then(|authority| authority.split(':').next())
        .unwrap_or_default()
        .to_ascii_lowercase();
    [
        "steamstatic.com",
        "steampowered.com",
        "steamcommunity.com",
        "steamusercontent.com",
    ]
    .iter()
    .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_hosts_are_not_told_its_godwit() {
        assert!(anonymous_host("https://avatars.steamstatic.com/abc_medium.jpg"));
        assert!(anonymous_host("https://steamcommunity.com/x"));
        assert!(!anonymous_host("https://cdn.stoatusercontent.com/x"));
        assert!(!anonymous_host("https://steamcommunity.com@127.0.0.1/x"));
        assert!(
            !anonymous_host("https://evilsteamstatic.com/x"),
            "only Steam's own domains"
        );
    }
}
