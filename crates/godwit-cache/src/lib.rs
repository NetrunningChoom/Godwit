// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A small encrypted JSON cache shared by network adapters. Cache keys never
//! appear in filenames, and each value is authenticated before it is decoded.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::Serialize;
use serde::de::DeserializeOwned;

const MAGIC: &[u8; 7] = b"GWCACHE";
const NONCE_LEN: usize = 24;
/// Refuse unbounded cache reads and writes, including corrupted headers.
const MAX_ENTRY: usize = 16 * 1024 * 1024;
/// Raw entries (pictures) may be larger: a 20 MB upload is still a picture
/// worth keeping rather than downloading again.
const MAX_RAW_ENTRY: usize = 32 * 1024 * 1024;
/// A read entry counts as recently used again (for [`EncryptedCache::prune`])
/// once its time is older than this, so frequent reads don't each write.
const TOUCH_AFTER: std::time::Duration = std::time::Duration::from_secs(3600);

/// XChaCha20-Poly1305 cache, keyed by a secret held outside this crate.
pub struct EncryptedCache {
    dir: PathBuf,
    cipher: XChaCha20Poly1305,
    name_key: [u8; 32],
}

impl EncryptedCache {
    /// `key` must contain exactly 32 cryptographically random bytes.
    pub fn new(dir: impl Into<PathBuf>, key: &[u8]) -> io::Result<Self> {
        let key: &[u8; 32] = key
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "cache key must be 32 bytes"))?;
        let name_key = blake3::derive_key("Godwit encrypted cache filename key", key);
        Ok(Self {
            dir: dir.into(),
            cipher: XChaCha20Poly1305::new(key.into()),
            name_key,
        })
    }

    /// As [`Self::new`], with a key derived from `key` for one purpose
    /// (`context`), so one stored secret can serve several caches.
    pub fn derived(dir: impl Into<PathBuf>, key: &[u8], context: &str) -> io::Result<Self> {
        Self::new(dir, &blake3::derive_key(context, key))
    }

    /// Reads and authenticates a value. A missing entry returns `None`.
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> io::Result<Option<T>> {
        let Some(plain) = self.get_bytes(key)? else {
            return Ok(None);
        };
        if plain.len() > MAX_ENTRY {
            return Err(invalid("cache entry exceeds size limit"));
        }
        serde_json::from_slice(&plain)
            .map(Some)
            .map_err(|_| invalid("invalid cache value"))
    }

    /// Reads and authenticates raw bytes (see [`Self::set_bytes`]).
    pub fn get_bytes(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        let path = self.path(key);
        not_a_link(&path)?;
        let mut file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let size = file.metadata()?.len();
        if size > (MAX_RAW_ENTRY + NONCE_LEN + MAGIC.len() + 16) as u64 {
            return Err(invalid("cache entry exceeds size limit"));
        }
        let mut bytes = Vec::with_capacity(size as usize);
        file.read_to_end(&mut bytes)?;
        let header_len = MAGIC.len() + NONCE_LEN;
        if bytes.len() < header_len + 16 || &bytes[..MAGIC.len()] != MAGIC {
            return Err(invalid("invalid cache entry"));
        }
        let nonce_bytes: [u8; NONCE_LEN] = bytes[MAGIC.len()..header_len]
            .try_into()
            .map_err(|_| invalid("invalid cache nonce"))?;
        let nonce = XNonce::from(nonce_bytes);
        let aad = associated_data(key);
        let plain = self
            .cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &bytes[header_len..],
                    aad: &aad,
                },
            )
            .map_err(|_| invalid("cache entry authentication failed"))?;
        Ok(Some(plain))
    }

    /// Serializes and encrypts a value, then replaces its entry via a private
    /// temporary file. A missing entry after interruption is safe to refetch.
    pub fn set<T: Serialize>(&self, key: &str, value: &T) -> io::Result<()> {
        let mut plain = BoundedVec::default();
        serde_json::to_writer(&mut plain, value)
            .map_err(|e| io::Error::new(e.io_error_kind().unwrap_or(io::ErrorKind::InvalidData), e))?;
        self.write(key, &plain.0, true)
    }

    /// Encrypts and stores raw bytes (pictures). Not synced to disk first:
    /// losing one in a crash only means fetching it again.
    pub fn set_bytes(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > MAX_RAW_ENTRY {
            return Err(invalid("cache value exceeds size limit"));
        }
        self.write(key, bytes, false)
    }

    /// Marks an entry as just used, so [`Self::prune`] removes it last
    /// (least recently used first). At most once per `TOUCH_AFTER`.
    pub fn touch(&self, key: &str) -> io::Result<()> {
        let path = self.path(key);
        not_a_link(&path)?;
        let now = std::time::SystemTime::now();
        let modified = fs::symlink_metadata(&path)?.modified()?;
        if now.duration_since(modified).unwrap_or_default() < TOUCH_AFTER {
            return Ok(());
        }
        fs::File::options().write(true).open(&path)?.set_modified(now)
    }

    fn write(&self, key: &str, plain: &[u8], durable: bool) -> io::Result<()> {
        self.private_dir()?;
        let path = self.path(key);
        not_a_link(&path)?;
        let mut nonce_bytes = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce_bytes).map_err(|e| io::Error::other(e.to_string()))?;
        let aad = associated_data(key);
        let encrypted = self
            .cipher
            .encrypt(&XNonce::from(nonce_bytes), Payload { msg: plain, aad: &aad })
            .map_err(|_| io::Error::other("cache encryption failed"))?;

        let mut bytes = Vec::with_capacity(MAGIC.len() + NONCE_LEN + encrypted.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&nonce_bytes);
        bytes.extend_from_slice(&encrypted);
        let tmp = self.dir.join(format!(".{}.tmp", random_suffix()?));
        let result = (|| {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp)?;
            file.write_all(&bytes)?;
            if durable {
                file.sync_all()?;
            }
            drop(file);
            not_a_link(&path)?;
            // Windows rename does not replace an existing file. Losing a
            // cache entry during this tiny window is safe: it is refetched.
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }

    /// Removes one entry. It is safe to call when it is already absent.
    pub fn remove(&self, key: &str) -> io::Result<()> {
        let path = self.path(key);
        not_a_link(&path)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Removes every entry, and temporary files left by interrupted writes.
    /// Only this cache's own files are touched (64 hex digits, or
    /// `.<hex>.tmp`); anything else in the folder stays.
    pub fn clear(&self) -> io::Result<()> {
        for file in self.own_files()? {
            match fs::remove_file(&file) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Removes the entries used longest ago (written, or [`Self::touch`]ed)
    /// until the cache takes at most
    /// `budget` bytes. Returns what it takes afterwards.
    pub fn prune(&self, budget: u64) -> io::Result<u64> {
        let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = self
            .own_files()?
            .into_iter()
            .filter_map(|f| {
                let meta = fs::symlink_metadata(&f).ok()?;
                Some((meta.modified().ok()?, meta.len(), f))
            })
            .collect();
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        if total <= budget {
            return Ok(total);
        }
        files.sort_by_key(|f| f.0);
        for (_, len, file) in files {
            if total <= budget {
                break;
            }
            match fs::remove_file(&file) {
                Ok(()) => total = total.saturating_sub(len),
                Err(e) if e.kind() == io::ErrorKind::NotFound => total = total.saturating_sub(len),
                Err(e) => return Err(e),
            }
        }
        Ok(total)
    }

    /// The bytes the cache takes on disk.
    pub fn size(&self) -> io::Result<u64> {
        Ok(self
            .own_files()?
            .iter()
            .filter_map(|f| fs::symlink_metadata(f).ok())
            .map(|m| m.len())
            .sum())
    }

    fn own_files(&self) -> io::Result<Vec<PathBuf>> {
        not_a_link(&self.dir)?;
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let hex = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
        Ok(entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter(|e| {
                let name = e.file_name();
                let Some(name) = name.to_str() else { return false };
                (name.len() == 64 && hex(name))
                    || name
                        .strip_prefix('.')
                        .and_then(|n| n.strip_suffix(".tmp"))
                        .is_some_and(hex)
            })
            .map(|e| e.path())
            .collect())
    }

    fn path(&self, key: &str) -> PathBuf {
        let hash = blake3::keyed_hash(&self.name_key, key.as_bytes());
        self.dir.join(hash.to_hex().as_str())
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

fn associated_data(key: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(MAGIC.len() + key.len());
    aad.extend_from_slice(MAGIC);
    aad.extend_from_slice(key.as_bytes());
    aad
}

fn random_suffix() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn not_a_link(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(invalid("cache path is a symbolic link")),
        _ => Ok(()),
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[derive(Default)]
struct BoundedVec(Vec<u8>);

impl Write for BoundedVec {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_ENTRY {
            return Err(invalid("cache value exceeds size limit"));
        }
        self.0.try_reserve(bytes.len()).map_err(io::Error::other)?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Snapshot {
        text: String,
        unread: u32,
    }

    struct Temp(PathBuf);

    impl Temp {
        fn new() -> Self {
            let name = random_suffix().unwrap();
            let path = std::env::temp_dir().join(format!("godwit-cache-{name}"));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn encrypts_round_trips_and_removes_values() {
        let temp = Temp::new();
        let cache = EncryptedCache::new(&temp.0, &[7; 32]).unwrap();
        let value = Snapshot {
            text: "Stoat history".into(),
            unread: 4,
        };
        assert_eq!(cache.get::<Snapshot>("stoat/room-1").unwrap(), None);
        cache.set("stoat/room-1", &value).unwrap();
        assert_eq!(cache.get("stoat/room-1").unwrap(), Some(value));
        cache.set("stoat/room-1", &"updated").unwrap();
        assert_eq!(cache.get::<String>("stoat/room-1").unwrap().as_deref(), Some("updated"));
        let entry = fs::read_dir(&temp.0).unwrap().next().unwrap().unwrap().path();
        let disk = fs::read(entry).unwrap();
        assert!(!disk.windows(b"Stoat history".len()).any(|w| w == b"Stoat history"));
        cache.remove("stoat/room-1").unwrap();
        assert_eq!(cache.get::<Snapshot>("stoat/room-1").unwrap(), None);
    }

    #[test]
    fn raw_bytes_round_trip_and_pruning_drops_the_oldest() {
        let temp = Temp::new();
        let cache = EncryptedCache::derived(&temp.0, &[9; 32], "test pictures").unwrap();
        let picture = vec![0x47u8; 1000];
        cache.set_bytes("https://files.example/a", &picture).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        cache.set_bytes("https://files.example/b", &picture).unwrap();
        assert_eq!(
            cache.get_bytes("https://files.example/a").unwrap(),
            Some(picture.clone())
        );
        let one = cache.size().unwrap() / 2;
        assert_eq!(cache.prune(one + 10).unwrap(), one);
        assert_eq!(cache.get_bytes("https://files.example/a").unwrap(), None, "oldest went");
        assert_eq!(
            cache.get_bytes("https://files.example/b").unwrap(),
            Some(picture.clone())
        );

        // A used entry stays; the one unused since goes.
        cache.set_bytes("https://files.example/c", &picture).unwrap();
        let old = std::time::SystemTime::now() - 2 * TOUCH_AFTER;
        for key in ["https://files.example/b", "https://files.example/c"] {
            fs::File::options()
                .write(true)
                .open(cache.path(key))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        let older = old - TOUCH_AFTER;
        fs::File::options()
            .write(true)
            .open(cache.path("https://files.example/c"))
            .unwrap()
            .set_modified(older)
            .unwrap();
        cache.touch("https://files.example/c").unwrap();
        cache.prune(one + 10).unwrap();
        assert!(
            cache.get_bytes("https://files.example/c").unwrap().is_some(),
            "touched, kept"
        );
        assert!(cache.get_bytes("https://files.example/b").unwrap().is_none());
    }

    #[test]
    fn rejects_wrong_key_tampering_and_key_swaps() {
        let temp = Temp::new();
        let cache = EncryptedCache::new(&temp.0, &[3; 32]).unwrap();
        cache.set("room-a", &"secret text").unwrap();
        assert_eq!(
            EncryptedCache::new(&temp.0, &[4; 32])
                .unwrap()
                .get::<String>("room-a")
                .unwrap(),
            None
        );
        fs::copy(cache.path("room-a"), cache.path("room-b")).unwrap();
        assert!(cache.get::<String>("room-b").is_err());
        let path = cache.path("room-a");
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x80;
        fs::write(path, bytes).unwrap();
        assert!(cache.get::<String>("room-a").is_err());
    }

    #[test]
    fn enforces_key_and_entry_limits() {
        let bad_key = EncryptedCache::new("cache", &[0; 31]);
        assert_eq!(bad_key.err().map(|e| e.kind()), Some(io::ErrorKind::InvalidInput));
        let temp = Temp::new();
        let cache = EncryptedCache::new(&temp.0, &[0; 32]).unwrap();
        assert_eq!(
            cache.set("large", &"x".repeat(MAX_ENTRY)).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn clear_removes_only_the_caches_own_files() {
        let temp = Temp::new();
        let cache = EncryptedCache::new(&temp.0, &[7; 32]).unwrap();
        let value = Snapshot {
            text: "hello".into(),
            unread: 1,
        };
        cache.set("a", &value).unwrap();
        cache.set("b", &value).unwrap();
        fs::write(temp.0.join(".00ff.tmp"), b"x").unwrap();
        fs::write(temp.0.join("notes.txt"), b"keep").unwrap();
        assert!(cache.size().unwrap() > 0);
        cache.clear().unwrap();
        assert_eq!(cache.get::<Snapshot>("a").unwrap(), None);
        assert_eq!(cache.size().unwrap(), 0);
        assert!(temp.0.join("notes.txt").exists(), "unrelated files stay");
        assert!(!temp.0.join(".00ff.tmp").exists());
    }
}
