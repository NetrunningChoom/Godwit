// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! What a network supplies that only its crate knows (data tables, client
//! headers), registered once at startup so the app and core can ask by
//! network without naming a network crate.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use crate::network::Network;

/// Request headers, as name and value.
pub type Headers = Vec<(&'static str, String)>;

/// A network's hooks. Each defaults to "nothing special".
#[derive(Clone, Copy)]
pub struct Hooks {
    /// Headers for fetching a picture at this URL, when it's the network's
    /// own and its official client sends particular ones; `None` otherwise.
    pub image_headers: fn(&str) -> Option<Headers>,
    /// The only emoji the network's clients know (by character), when it has
    /// a set of its own rather than an Emoji version.
    pub emoji_set: fn() -> Option<&'static HashSet<String>>,
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            image_headers: |_| None,
            emoji_set: || None,
        }
    }
}

fn registry() -> &'static Mutex<HashMap<Network, Hooks>> {
    static REGISTRY: OnceLock<Mutex<HashMap<Network, Hooks>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

fn with<T>(f: impl FnOnce(&mut HashMap<Network, Hooks>) -> T) -> T {
    let mut map = registry().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut map)
}

/// Registers `network`'s hooks (at startup; a later call replaces them).
pub fn register(network: Network, hooks: Hooks) {
    with(|m| m.insert(network, hooks));
}

/// `network`'s hooks, or the defaults.
pub fn of(network: Network) -> Hooks {
    with(|m| m.get(&network).copied()).unwrap_or_default()
}

/// The headers some network wants for a picture at `url`, if any.
pub fn image_headers(url: &str) -> Option<Headers> {
    let all: Vec<Hooks> = with(|m| m.values().copied().collect());
    all.iter().find_map(|h| (h.image_headers)(url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unregistered_networks_have_nothing_special() {
        let h = of(Network::Fluxer);
        assert!((h.image_headers)("https://example.com/a.png").is_none());
        assert!((h.emoji_set)().is_none());
    }

    #[test]
    fn registered_hooks_are_found_by_url() {
        register(
            Network::Fluxer,
            Hooks {
                image_headers: |url| url.contains("fluxer.test").then(|| vec![("x-test", "1".to_owned())]),
                ..Hooks::default()
            },
        );
        assert_eq!(image_headers("https://cdn.fluxer.test/a.png").map(|h| h.len()), Some(1));
        assert!(image_headers("https://elsewhere.test/a.png").is_none());
    }
}
