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
    Create { name: Arc<str> },
    Rename { id: Arc<str>, name: Arc<str> },
    Delete { id: Arc<str> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebhooksState {
    pub room: RoomId,
    pub entries: Vec<Webhook>,
    pub loading: bool,
    pub busy: bool,
    pub error: Option<Arc<str>>,
}
