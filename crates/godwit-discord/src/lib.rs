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
pub mod gateway;
pub mod limits;
pub mod markdown;
pub mod member_list;
pub mod permissions;
pub mod remote_auth;
pub mod search;
pub mod session;
pub mod settings;
pub mod store;
pub mod voice;
