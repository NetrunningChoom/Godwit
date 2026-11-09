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

/// Whether `path` can only be one of Godwit's own folders: absolute, not a
/// filesystem root or the home folder, and named `godwit` itself or
/// `config`, `data` or `cache` directly inside one (Windows' `…\godwit\config`).
/// The roots come from the platform's standard folders, so this never refuses a
/// real one; it stops a wrong path (a bad environment variable, a future
/// change) from turning a reset into deleting someone's files.
pub fn is_godwit_root(path: &Path) -> bool {
    if !path.is_absolute() || path.parent().is_none() {
        return false;
    }
    if let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
        && home.starts_with(path)
    {
        return false;
    }
    let named = |p: Option<&Path>| {
        p.and_then(Path::file_name)
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("godwit"))
    };
    let leaf = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase());
    named(Some(path)) || (named(path.parent()) && matches!(leaf.as_deref(), Some("config" | "data" | "cache")))
}

/// Removes only the supplied Godwit-owned roots. A symlink is unlinked as a
/// leaf, so recursive deletion cannot enter a user's target directory, and a
/// folder on another filesystem (something mounted inside) is refused rather
/// than emptied.
pub fn remove_tree(path: &Path) -> io::Result<()> {
    remove_below(path, None)
}

fn remove_below(path: &Path, device: Option<u64>) -> io::Result<()> {
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
    let here = device_of(&metadata);
    if let (Some(root), Some(here)) = (device, here)
        && root != here
    {
        return Err(io::Error::other(format!(
            "{} is another filesystem mounted inside Godwit's folder; unmount it first",
            path.display()
        )));
    }
    for entry in std::fs::read_dir(path)? {
        remove_below(&entry?.path(), device.or(here))?;
    }
    std::fs::remove_dir(path)
}

#[cfg(unix)]
fn device_of(metadata: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.dev())
}

// Windows and others: a mount point inside the folder is a reparse point,
// which `symlink_metadata` reports as a link and is unlinked as a leaf.
#[cfg(not(unix))]
fn device_of(_: &std::fs::Metadata) -> Option<u64> {
    None
}

/// Removes Godwit's config, data, and cache directories after clearing sessions.
pub fn reset(
    config: &Path,
    data: &Path,
    cache: &Path,
    accounts: &[(String, u32)],
    store: &dyn SecretStore,
) -> io::Result<()> {
    for root in [config, data, cache] {
        if !is_godwit_root(root) {
            return Err(io::Error::other(format!(
                "{} doesn't look like a Godwit folder, so nothing was removed",
                root.display()
            )));
        }
    }
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
        let config = temp.0.join("config/godwit");
        let data = temp.0.join("data/godwit");
        let cache = temp.0.join("cache/godwit");
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
        let config = temp.0.join("config/godwit");
        std::fs::create_dir_all(&config).unwrap();
        symlink(&outside, config.join("link")).unwrap();
        reset(
            &config,
            &temp.0.join("data/godwit"),
            &temp.0.join("cache/godwit"),
            &[],
            &Memory::default(),
        )
        .unwrap();
        assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"keep");
    }

    #[test]
    fn only_godwit_folders_count_as_roots() {
        assert!(is_godwit_root(Path::new("/srv/u/.config/godwit")));
        assert!(is_godwit_root(Path::new("/srv/u/AppData/Roaming/godwit/config")));
        assert!(is_godwit_root(Path::new("/Users/u/Library/Caches/Godwit")));
        assert!(!is_godwit_root(Path::new("/")));
        assert!(!is_godwit_root(Path::new("godwit")));
        assert!(!is_godwit_root(Path::new("/srv/u/.config")));
        assert!(!is_godwit_root(Path::new("/srv/u/Documents")));
        assert!(!is_godwit_root(Path::new("/srv/u/godwit-notes")));
        assert!(!is_godwit_root(Path::new("/srv/u/godwit/Documents")));
        if let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) {
            assert!(!is_godwit_root(&home));
            if let Some(parent) = home.parent() {
                assert!(!is_godwit_root(parent));
            }
        }
    }

    #[test]
    fn a_wrong_root_stops_the_reset_before_anything_goes() {
        let temp = Temp::new();
        let config = temp.0.join("config/godwit");
        let wrong = temp.0.join("Documents");
        for root in [&config, &wrong] {
            std::fs::create_dir_all(root).unwrap();
            std::fs::write(root.join("file"), b"keep").unwrap();
        }
        let secrets = Memory::default();
        secrets.set("stoat-session", &Secret::new("opaque secret")).unwrap();
        let result = reset(
            &config,
            &wrong,
            &temp.0.join("cache/godwit"),
            &[("stoat".into(), 0)],
            &secrets,
        );
        assert!(result.is_err());
        assert!(config.join("file").exists() && wrong.join("file").exists());
        assert!(secrets.get("stoat-session").unwrap().is_some());
    }

    #[test]
    fn reset_twice_is_fine() {
        let temp = Temp::new();
        let roots = ["config/godwit", "data/godwit", "cache/godwit"].map(|r| temp.0.join(r));
        std::fs::create_dir_all(&roots[0]).unwrap();
        let secrets = Memory::default();
        for _ in 0..2 {
            reset(&roots[0], &roots[1], &roots[2], &[("discord".into(), 3)], &secrets).unwrap();
        }
        assert!(!roots[0].exists());
    }
}
