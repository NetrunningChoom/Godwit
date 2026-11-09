// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A channel's webhook list. Secrets intentionally have no core representation.

use crate::ids::RoomId;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Webhook {
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub avatar_url: Option<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebhookChange {
    Create {
        name: Arc<str>,
    },
    Rename {
        id: Arc<str>,
        name: Arc<str>,
    },
    Delete {
        id: Arc<str>,
    },
    /// Put the webhook's URL on the clipboard (networks whose webhook URL
    /// carries its secret keep that secret out of the list).
    CopyUrl {
        id: Arc<str>,
    },
}

/// A webhook's URL, which holds its secret: never shown by `Debug`, and only
/// travels from the network crate to the clipboard.
#[derive(Clone, PartialEq, Eq)]
pub struct WebhookUrl(Arc<str>);

impl WebhookUrl {
    pub fn new(url: impl Into<Arc<str>>) -> Self {
        Self(url.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for WebhookUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WebhookUrl(..)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebhooksState {
    pub room: RoomId,
    pub entries: Vec<Webhook>,
    pub loading: bool,
    pub busy: bool,
    pub error: Option<Arc<str>>,
}
