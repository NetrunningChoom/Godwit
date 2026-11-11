// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Godwit's Stoat network.

pub mod account;
pub mod api;
pub mod autodelete;
pub mod backend;
mod channel_edit;
pub mod emoji;
pub mod errors;
pub use godwit_core::markdown;
pub mod permissions;
mod profile_edit;
pub mod report;
pub use godwit_core::markdown_rich as rich;
mod server_edit;
mod server_profile;
pub mod session_file;
pub mod store;
pub use godwit_core::timefmt;
