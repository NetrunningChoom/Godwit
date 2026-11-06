// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Google Chat for Godwit, as its web client (chat.google.com) uses it:
//! signing in on Google's own page in the player helper's window, then the
//! web client's cookies, with every API call signed as the page signs it
//! (`SAPISIDHASH`). Works for personal Google accounts and Workspace alike;
//! the official Chat API covers Workspace only and has no push without a
//! Cloud Pub/Sub project. See `docs/gchat.md`.
//!
//! `session` (the sign-in), `api` (the web API) with `proto` (its messages,
//! from maugclib), `backend` (the network thread) and `account` (the core
//! bridge). The push channel comes next.

pub mod account;
mod annotations;
pub mod api;
pub mod auth;
pub mod backend;
mod channel;
mod media;
mod pblite;
pub mod proto;
pub mod session;
