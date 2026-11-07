// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Preserve malformed local preference files before starting from defaults.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Moves a corrupt file's bytes to a unique sibling before its original path
/// is reused. Exclusive creation prevents overwriting an earlier recovery.
pub fn preserve_corrupt(path: &Path) -> io::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().unwrap_or_default();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut source = fs::File::open(path)?;
    for _ in 0..100 {
        let mut recovered_name = OsString::from(name);
        recovered_name.push(format!(".corrupt-{stamp}-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
        let recovered = parent.join(recovered_name);
        let mut destination = match OpenOptions::new().write(true).create_new(true).open(&recovered) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let copied = io::copy(&mut source, &mut destination).and_then(|_| destination.sync_all());
        if let Err(error) = copied {
            drop(destination);
            let _ = fs::remove_file(&recovered);
            return Err(error);
        }
        drop(destination);
        fs::remove_file(path)?;
        return Ok(recovered);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not choose a recovery filename",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_corrupt_bytes_and_frees_original_path() {
        let directory = std::env::temp_dir().join(format!("godwit-recovery-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("settings.json");
        fs::write(&path, b"{broken").unwrap();

        let recovered = preserve_corrupt(&path).unwrap();

        assert!(!path.exists());
        assert_eq!(fs::read(recovered).unwrap(), b"{broken");
        let _ = fs::remove_dir_all(directory);
    }
}
