// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Editable account profile data. Network adapters own their field limits
//! and request formats; this model is shared by settings on every network.

use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnProfile {
    /// The name currently displayed, including the network's fallback.
    pub name: Arc<str>,
    pub display_name: Arc<str>,
    pub pronouns: Arc<str>,
    /// Original markup, rather than the rendered profile card's rich text.
    pub bio: Arc<str>,
    pub avatar_url: Option<Arc<str>>,
    pub banner_url: Option<Arc<str>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ProfileImage {
    #[default]
    Keep,
    Remove,
    File {
        path: PathBuf,
        size: u64,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileEdit {
    /// None leaves a field untouched; an empty string clears it.
    pub display_name: Option<Arc<str>>,
    pub pronouns: Option<Arc<str>>,
    pub bio: Option<Arc<str>>,
    pub avatar: ProfileImage,
    pub banner: ProfileImage,
}

impl ProfileEdit {
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none()
            && self.pronouns.is_none()
            && self.bio.is_none()
            && self.avatar == ProfileImage::Keep
            && self.banner == ProfileImage::Keep
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnProfileState {
    pub profile: Option<OwnProfile>,
    pub loading: bool,
    pub saving: bool,
    pub saved: bool,
    pub error: Option<Arc<str>>,
}
