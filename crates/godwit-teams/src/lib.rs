// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Microsoft Teams for Godwit (work and school accounts): signing in with a
//! device code, 1:1 and group chats and team channels, history, sending,
//! deleting, read state and typing, over the chat service the official
//! clients use ("chatsvc"), with news pushed over its Trouter connection.
//!
//! Microsoft doesn't document this API for other clients (its documented
//! one, Graph, can only push to a public web address, so a desktop client
//! would have to poll). The protocol is the one purple-teams
//! (<https://github.com/EionRobb/purple-teams>) speaks; no code is taken
//! from it. See `docs/teams.md`.

pub mod account;
pub mod api;
pub mod auth;
pub mod backend;
pub mod emoji;
pub mod html;
pub mod session;
pub mod trouter;

mod media;
