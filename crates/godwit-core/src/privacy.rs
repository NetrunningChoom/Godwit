// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! An account's privacy switches, as its network offers them (Discord's
//! data-use consents and activity sharing): the Settings page lists them and
//! flips one at a time.

use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivacyItem {
    /// The network's own name for it (what `SetPrivacy` names).
    pub key: Arc<str>,
    pub label: Arc<str>,
    pub detail: Arc<str>,
    pub on: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrivacyView {
    pub items: Vec<PrivacyItem>,
    pub loading: bool,
    pub error: Option<Arc<str>>,
}

/// A device signed in to the account (Settings → Sessions).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// The network's id for it (what signing it out names).
    pub id: Arc<str>,
    /// "Windows · Discord Client", as the official Devices page names it.
    pub device: Arc<str>,
    /// Where it was last used from, roughly, if the network says.
    pub location: Arc<str>,
    /// When it was last used (ms since the epoch), if known.
    pub last_used_ms: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionsView {
    pub sessions: Vec<Session>,
    pub loading: bool,
    pub error: Option<Arc<str>>,
}
