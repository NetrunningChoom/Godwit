// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The app's config directory (`godwit/` in the OS config location).
//!
//! Godwit starts with its own directory. Godwit's (`Godwit/`) is never read,
//! moved or changed: it may still be in use.

use std::path::PathBuf;

pub fn get() -> Option<PathBuf> {
    Some(
        directories::ProjectDirs::from("", "", "godwit")?
            .config_dir()
            .to_path_buf(),
    )
}
