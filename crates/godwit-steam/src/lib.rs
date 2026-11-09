// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Steam for Godwit: signing in (a QR code for the Steam mobile app, or a
//! password and Steam Guard), friends with their presence and games, and
//! one-to-one chat, over Steam's client protocol (steam-vent). Valve offers
//! no chat API for other clients: this behaves like its own client, asks
//! only what a chat needs, and never automates anything.

pub mod account;
pub mod auth;
pub mod backend;
pub mod bbcode;
pub mod favourites;
pub mod groups;
pub mod library;
pub mod media;
pub mod nicknames;
pub mod session;
mod uploads;
mod vdf;
pub mod voice;
