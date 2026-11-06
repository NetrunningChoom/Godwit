// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Secrets (network tokens, store passwords, the message cache key) behind
//! one interface, so nothing else in Godwit touches an OS API and every
//! platform can have its own backend (see docs/design.md, "Encryption at
//! rest"). Backends: the OS keychain ([`KeyringStore`]: Secret Service on
//! Linux, Credential Manager on Windows, Keychain on macOS), and owner-only
//! files ([`FileStore`]: the stated fallback, Android for now, and what tests
//! and the demo use). [`open`] picks the best one there is.

#[cfg(any(target_os = "windows", test))]
mod chunked;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A secret value. Its debug output is redacted, so no log can print it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value itself, for handing to the network or a store.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// 32 random bytes, hex-encoded: a store password or key.
    pub fn random() -> io::Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Self(bytes.iter().map(|b| format!("{b:02x}")).collect()))
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// Where secrets are kept. Names are short ASCII keys like `stoat-session`.
pub trait SecretStore {
    fn get(&self, name: &str) -> io::Result<Option<Secret>>;
    fn set(&self, name: &str, secret: &Secret) -> io::Result<()>;
    fn delete(&self, name: &str) -> io::Result<()>;
}

/// The OS keychain, through keyring-core with the platform's store. Secrets
/// are entries of the service `godwit`, named as given.
pub struct KeyringStore;

const SERVICE: &str = "godwit";

impl KeyringStore {
    /// The keychain, if this platform has one that answers.
    pub fn open() -> io::Result<Self> {
        static READY: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
        READY
            .get_or_init(set_platform_store)
            .clone()
            .map_err(io::Error::other)?;
        Ok(Self)
    }

    fn entry(name: &str) -> io::Result<keyring_core::Entry> {
        keyring_core::Entry::new(SERVICE, name).map_err(io::Error::other)
    }
}

/// Makes the platform's keychain keyring-core's default store (once).
fn set_platform_store() -> Result<(), String> {
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android"))))]
    let store = zbus_secret_service_keyring_store::Store::new().map_err(|e| e.to_string())?;
    #[cfg(target_os = "windows")]
    let store = windows_native_keyring_store::Store::new().map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    let store = apple_native_keyring_store::keychain::Store::new().map_err(|e| e.to_string())?;
    #[cfg(any(
        target_os = "windows",
        target_os = "macos",
        all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android")))
    ))]
    {
        keyring_core::set_default_store(store);
        Ok(())
    }
    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        all(unix, not(any(target_os = "macos", target_os = "ios", target_os = "android")))
    )))]
    Err("no keychain on this platform yet".to_owned())
}

/// Credential Manager entries as bytes, for [`chunked`]: `set_password`
/// would store UTF-16, halving what fits in one credential.
#[cfg(target_os = "windows")]
struct Credentials;

#[cfg(target_os = "windows")]
impl chunked::Raw for Credentials {
    fn get(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        match KeyringStore::entry(name)?.get_secret() {
            Ok(value) => Ok(Some(value)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(io::Error::other(e)),
        }
    }

    fn set(&self, name: &str, value: &[u8]) -> io::Result<()> {
        KeyringStore::entry(name)?.set_secret(value).map_err(io::Error::other)
    }

    fn delete(&self, name: &str) -> io::Result<()> {
        match KeyringStore::entry(name)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(io::Error::other(e)),
        }
    }
}

/// Windows: UTF-8 bytes, split when longer than one credential holds.
#[cfg(target_os = "windows")]
impl SecretStore for KeyringStore {
    fn get(&self, name: &str) -> io::Result<Option<Secret>> {
        let Some(bytes) = chunked::get(&Credentials, name)? else {
            return Ok(None);
        };
        String::from_utf8(bytes)
            .map(|value| Some(Secret(value)))
            .map_err(|_| io::Error::other(format!("{name}: not UTF-8")))
    }

    fn set(&self, name: &str, secret: &Secret) -> io::Result<()> {
        chunked::set(&Credentials, name, secret.0.as_bytes(), chunked::LIMIT)
    }

    fn delete(&self, name: &str) -> io::Result<()> {
        chunked::delete(&Credentials, name)
    }
}

#[cfg(not(target_os = "windows"))]
impl SecretStore for KeyringStore {
    fn get(&self, name: &str) -> io::Result<Option<Secret>> {
        match Self::entry(name)?.get_password() {
            Ok(value) => Ok(Some(Secret(value))),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(io::Error::other(e)),
        }
    }

    fn set(&self, name: &str, secret: &Secret) -> io::Result<()> {
        Self::entry(name)?.set_password(&secret.0).map_err(io::Error::other)
    }

    fn delete(&self, name: &str) -> io::Result<()> {
        match Self::entry(name)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(io::Error::other(e)),
        }
    }
}

/// Which kind of store [`open`] gave, for the UI to say when secrets are
/// only protected by file permissions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Keychain,
    Files,
}

/// The best store there is: the OS keychain, else owner-only files in `dir`.
pub fn open(dir: impl Into<PathBuf>) -> (Box<dyn SecretStore + Send + Sync>, Kind) {
    match KeyringStore::open() {
        Ok(k) => (Box::new(k), Kind::Keychain),
        Err(e) => {
            eprintln!("note: no keychain ({e}); secrets are kept in owner-only files");
            (Box::new(FileStore::new(dir)), Kind::Files)
        }
    }
}

/// Secrets as owner-only files in a private folder (0700, files 0600 on
/// Unix): the fallback where there's no keychain. Anything running as the
/// same user can read them, which the UI says when this backend is used.
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// A name's file; only ASCII letters, digits and `-`/`_` are kept, so a
    /// name can't reach outside the folder.
    fn path(&self, name: &str) -> io::Result<PathBuf> {
        let clean: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if clean.is_empty() || clean != name {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad secret name"));
        }
        Ok(self.dir.join(clean))
    }

    fn private_dir(&self) -> io::Result<()> {
        not_a_link(&self.dir)?;
        fs::create_dir_all(&self.dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
}

/// Refuses a symbolic link where a secret file should be (it could point
/// the write somewhere else).
fn not_a_link(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            Err(io::Error::new(io::ErrorKind::InvalidInput, "secret file is a link"))
        }
        _ => Ok(()),
    }
}

impl SecretStore for FileStore {
    fn get(&self, name: &str) -> io::Result<Option<Secret>> {
        let path = self.path(name)?;
        not_a_link(&path)?;
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(Secret(text))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Written to a private temporary file, flushed, then moved over the
    /// old one, so a crash never leaves half a secret.
    fn set(&self, name: &str, secret: &Secret) -> io::Result<()> {
        self.private_dir()?;
        let path = self.path(name)?;
        not_a_link(&path)?;
        // A fresh, exclusively created file cannot follow a planted symlink or
        // reuse a stale temporary file with permissions broader than 0600.
        let tmp = self.dir.join(format!(".{name}-{}.tmp", Secret::random()?.expose()));
        let result = (|| {
            use std::io::Write;
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut f = options.open(&tmp)?;
            f.write_all(secret.0.as_bytes())?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp, &path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    fn delete(&self, name: &str) -> io::Result<()> {
        match fs::remove_file(self.path(name)?) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test's own directory, removed when the test ends, even by a
    /// failed assertion (some tests never removed theirs).
    struct TempDir(PathBuf);

    impl std::ops::Deref for TempDir {
        type Target = PathBuf;
        fn deref(&self) -> &PathBuf {
            &self.0
        }
    }

    impl AsRef<Path> for TempDir {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn store() -> (FileStore, TempDir) {
        let dir = std::env::temp_dir().join(format!(
            "godwit-secrets-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        (FileStore::new(&dir), TempDir(dir))
    }

    #[test]
    fn set_get_delete() {
        let (s, _dir) = store();
        assert_eq!(s.get("stoat-session").unwrap(), None);
        let secret = Secret::new("token-123");
        s.set("stoat-session", &secret).unwrap();
        assert_eq!(s.get("stoat-session").unwrap(), Some(secret));
        s.delete("stoat-session").unwrap();
        assert_eq!(s.get("stoat-session").unwrap(), None);
        s.delete("stoat-session").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn only_the_owner_can_read() {
        use std::os::unix::fs::PermissionsExt;
        let (s, dir) = store();
        s.set("key", &Secret::new("x")).unwrap();
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(dir.join("key")).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_linked_secret_files_and_directories() {
        use std::os::unix::fs::symlink;
        let (s, dir) = store();
        fs::create_dir_all(&dir).unwrap();
        let outside = dir.join("outside");
        fs::write(&outside, "untouched fake data").unwrap();
        symlink(&outside, dir.join("key")).unwrap();
        assert!(s.get("key").is_err());
        assert!(s.set("key", &Secret::new("fake secret")).is_err());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "untouched fake data");
        let linked = dir.join("linked");
        symlink(&dir, &linked).unwrap();
        assert!(
            FileStore::new(linked)
                .set("other", &Secret::new("fake secret"))
                .is_err()
        );
        assert!(!dir.join("other").exists());
    }

    #[cfg(unix)]
    #[test]
    fn replacing_a_secret_restores_private_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let (s, dir) = store();
        s.set("key", &Secret::new("fake old secret")).unwrap();
        fs::set_permissions(dir.join("key"), fs::Permissions::from_mode(0o644)).unwrap();
        s.set("key", &Secret::new("fake new secret")).unwrap();
        assert_eq!(
            fs::metadata(dir.join("key")).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            1,
            "no temporary secrets left behind"
        );
    }

    #[test]
    fn names_stay_inside_and_values_stay_hidden() {
        let (s, _dir) = store();
        assert!(s.set("../escape", &Secret::new("x")).is_err());
        assert!(s.get("a/b").is_err());
        assert_eq!(format!("{:?}", Secret::new("hunter2")), "Secret(…)");
        let (a, b) = (Secret::random().unwrap(), Secret::random().unwrap());
        assert_eq!(a.expose().len(), 64);
        assert_ne!(a, b);
    }
}
