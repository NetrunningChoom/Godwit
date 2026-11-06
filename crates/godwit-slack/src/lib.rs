// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Slack for Godwit: signing in through Slack's own page (in the player
//! helper's window, `godwit-player --slack-login`), every workspace of the
//! sign-in as a server with its channels, DMs and group DMs, history,
//! sending, editing, deleting, reactions, read state and typing, over the
//! Web API (<https://api.slack.com/methods>) and the websocket the web
//! client uses (`rtm.connect`).
//!
//! The session is the web client's: a per-workspace `xoxc-` token and the
//! `d` cookie, as wee-slack and other third-party clients use. See
//! `docs/slack.md`.

pub mod account;
pub mod api;
pub mod backend;
pub mod mrkdwn;
pub mod rtm;
pub mod session;

mod media;
