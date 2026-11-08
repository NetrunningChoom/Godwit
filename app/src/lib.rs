// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Godwit's app: the Slint UI over the core and the network backends.
//! `main.rs` runs it; `examples/preview.rs` renders it off-screen.

pub mod account_pages;
pub mod alerts;
pub mod anim;
pub mod autodelete_page;
pub mod cache;
pub mod channel_settings;
pub mod code;
pub mod cropper;
pub mod demo;
pub mod emoji;
pub mod errors;
pub mod games;
pub mod gifs;
pub mod images;
pub mod inline_player;
pub mod install;
pub mod local_recovery;
pub mod memory;
pub mod notifications;
pub mod picker;
pub mod player_window;
pub mod present;
pub mod profile_settings;
pub mod profiles;
pub mod qr;
pub mod registry;
pub mod sas;
pub mod scale;
pub mod selection;
pub mod server_settings;
pub mod settings;
pub mod shell;
pub mod stickers;
pub mod stream;
pub mod textlayout;
pub mod theme;
pub mod update;
pub mod view;
pub mod voice_ui;
pub mod wheel;
#[cfg(windows)]
pub mod win32;

/// The Slint UI, generated in its own crate (`crates/godwit-ui`) so it
/// compiles once and stays cached.
pub use godwit_ui as ui;
