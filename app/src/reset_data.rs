// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Removing Godwit's local profile without following links outside its folders.

use std::io;
use std::path::Path;

use godwit_secrets::SecretStore;

pub fn roots() -> Option<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)> {
    let project = directories::ProjectDirs::from("", "", "godwit")?;
    Some((
        godwit_core::config_dir::get()?,
        project.data_dir().to_path_buf(),
        project.cache_dir().to_path_buf(),
    ))
}

pub fn folders() -> Vec<(String, std::path::PathBuf)> {
    let Some((config, data, cache)) = roots() else {
        return Vec::new();
    };
    vec![
        ("Settings and configuration".into(), config.clone()),
        ("Godwit data".into(), data),
        ("Kept sessions".into(), config.join("secrets")),
        ("Message cache".into(), config.join("message-cache")),
        ("Media cache".into(), config.join("picture-cache")),
        ("Logs".into(), cache),
    ]
}

pub fn is_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
}

/// Deletes all known kept-session entries without retrieving their values.
pub fn clear_sessions(store: &dyn SecretStore, accounts: &[(String, u32)]) -> io::Result<()> {
    for (network, slot) in accounts {
        let name = if *slot == 0 {
            format!("{network}-session")
        } else {
            format!("{network}-session-{slot}")
        };
        store.delete(&name)?;
    }
    store.delete("message-cache-key")?;
    Ok(())
}

/// Removes only the supplied Godwit-owned roots. A symlink is unlinked as a
/// leaf, so recursive deletion cannot enter a user's target directory.
pub fn remove_tree(path: &Path) -> io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        return match std::fs::remove_dir(path) {
            Ok(()) => Ok(()),
            Err(error) if matches!(error.kind(), io::ErrorKind::NotADirectory | io::ErrorKind::InvalidInput) => {
                std::fs::remove_file(path)
            }
            Err(error) => Err(error),
        };
    }
    if !metadata.is_dir() {
        return std::fs::remove_file(path);
    }
    for entry in std::fs::read_dir(path)? {
        remove_tree(&entry?.path())?;
    }
    std::fs::remove_dir(path)
}

/// Removes Godwit's config, data, and cache directories after clearing sessions.
pub fn reset(
    config: &Path,
    data: &Path,
    cache: &Path,
    accounts: &[(String, u32)],
    store: &dyn SecretStore,
) -> io::Result<()> {
    clear_sessions(store, accounts)?;
    for root in [config, data, cache] {
        remove_tree(root)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use godwit_secrets::{Secret, SecretStore, testing::Memory};

    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "godwit-reset-{}-{}",
                std::process::id(),
                TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    static TEMP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    #[test]
    fn reset_deletes_roots_and_sessions_without_reading_them() {
        let temp = Temp::new();
        let config = temp.0.join("config");
        let data = temp.0.join("data");
        let cache = temp.0.join("cache");
        for root in [&config, &data, &cache] {
            std::fs::create_dir_all(root.join("nested")).unwrap();
            std::fs::write(root.join("nested/file"), b"private").unwrap();
        }
        let secrets = Memory::default();
        secrets.set("stoat-session-4", &Secret::new("opaque secret")).unwrap();
        reset(&config, &data, &cache, &[("stoat".into(), 4)], &secrets).unwrap();
        assert!(!config.exists() && !data.exists() && !cache.exists());
        assert!(secrets.get("stoat-session-4").unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn reset_unlinks_symlinks_without_entering_the_target() {
        use std::os::unix::fs::symlink;
        let temp = Temp::new();
        let outside = temp.0.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"keep").unwrap();
        let config = temp.0.join("config");
        std::fs::create_dir(&config).unwrap();
        symlink(&outside, config.join("link")).unwrap();
        reset(
            &config,
            &temp.0.join("data"),
            &temp.0.join("cache"),
            &[],
            &Memory::default(),
        )
        .unwrap();
        assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"keep");
    }
}
