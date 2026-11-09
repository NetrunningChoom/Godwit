// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Look-and-feel choices a network keeps on the account and syncs between its
//! clients (see [`Network::syncs_preferences`](crate::network::Network::syncs_preferences)).
//! Godwit applies what the account says on sign-in and when it changes
//! elsewhere, and writes back what the user changes here.

/// The synced choices; `None` is "not said" (a partial update leaves the rest).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncedPreferences {
    /// Dark rather than light theme.
    pub dark: Option<bool>,
    /// Typed emoticons (":)") become emoji.
    pub convert_emoticons: Option<bool>,
}

impl SyncedPreferences {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// `self` with the choices `newer` says laid over it.
    pub fn merged(self, newer: Self) -> Self {
        Self {
            dark: newer.dark.or(self.dark),
            convert_emoticons: newer.convert_emoticons.or(self.convert_emoticons),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_choices_win_and_unsaid_ones_stay() {
        let old = SyncedPreferences {
            dark: Some(true),
            convert_emoticons: Some(false),
        };
        let new = SyncedPreferences {
            dark: Some(false),
            convert_emoticons: None,
        };
        let both = old.merged(new);
        assert_eq!(both.dark, Some(false));
        assert_eq!(both.convert_emoticons, Some(false));
        assert!(SyncedPreferences::default().is_empty());
        assert!(!both.is_empty());
    }
}
