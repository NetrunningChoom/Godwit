// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Godwit's core. It knows nothing about any UI framework or any one network:
//! the UI sends [`application::Action`]s, network backends send
//! [`application::Event`]s, and [`application::Application`] answers both with
//! [`application::Effect`]s. See docs/design.md.

pub mod account;
pub mod account_security;
pub mod application;
pub mod autocomplete;
pub mod autodelete;
pub mod cache;
pub mod call;
pub mod channel_settings;
pub mod config_dir;
pub mod contacts;
pub mod emoji_packs;
pub mod emoticons;
pub mod encoding;
pub mod folder_edit;
pub mod forum;
pub mod games;
pub mod gifs;
pub mod hooks;
pub mod ids;
pub mod log;
pub mod media;
pub mod members;
pub mod message;
pub mod net;
pub mod network;
pub mod own_profile;
pub mod poll;
pub mod preferences;
pub mod privacy;
pub mod privacy_overview;
pub mod public_address;
pub mod relationships;
pub mod report;
pub mod rich;
pub mod room;
pub mod room_sync;
pub mod search;
pub mod server;
pub mod server_settings;
pub mod stickers;
pub mod webhooks;

pub mod backend;

pub mod sessions;
