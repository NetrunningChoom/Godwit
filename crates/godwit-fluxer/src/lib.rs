// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Fluxer for Godwit (<https://fluxer.com>, docs at <https://docs.fluxer.com>):
//! signing in by email and password (its ALTCHA proof of work solved here),
//! servers with their channels, DMs and group DMs, and messages, over its
//! gateway and HTTP API.
//!
//! Fluxer's objects have Discord's shapes, so the state store, permissions
//! and Markdown conversion come from `godwit-discord`; its requests,
//! gateway and sign-in are Fluxer's own. Fluxer documents its API for
//! third-party clients; requests send `User-Agent: Godwit`, keep well below its
//! limits (a global denial signs the user out), and never poll.

pub mod account;
pub mod api;
pub mod backend;
pub mod captcha;
pub mod discovery;
pub mod gateway;
pub mod session;
