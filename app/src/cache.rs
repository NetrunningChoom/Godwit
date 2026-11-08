// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! App wiring for the shared encrypted cache and the platform secret store.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;

use godwit_cache::EncryptedCache;
use godwit_core::cache::AccountSnapshot;
use godwit_core::ids::AccountId;
use godwit_secrets::{Secret, SecretStore};

pub struct Cache {
    backend: Arc<EncryptedCache>,
    queue: Arc<WriteQueue>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct WriteState {
    pending: HashMap<AccountId, Option<AccountSnapshot>>,
    completed_reads: VecDeque<(AccountId, Option<AccountSnapshot>)>,
    active: HashSet<AccountId>,
    /// Clear every entry, after the writes queued before it.
    clear: bool,
    closed: bool,
}

#[derive(Default)]
struct WriteQueue {
    state: Mutex<WriteState>,
    ready: Condvar,
    idle: Condvar,
}

impl Cache {
    /// Demo mode never reads credentials or persists snapshots.
    pub fn open() -> io::Result<Self> {
        let config = godwit_core::config_dir::get()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "config directory unavailable"))?;
        let (secrets, _) = godwit_secrets::open(config.join("secrets"));
        Self::with_store(config.join("message-cache"), secrets.as_ref())
    }

    fn with_store(dir: impl Into<std::path::PathBuf>, store: &dyn SecretStore) -> io::Result<Self> {
        let key = load_or_create_key(store)?;
        Self::new(EncryptedCache::new(dir, &key)?)
    }

    fn new(backend: EncryptedCache) -> io::Result<Self> {
        let backend = Arc::new(backend);
        let queue = Arc::<WriteQueue>::default();
        let worker_queue = queue.clone();
        let worker_backend = backend.clone();
        let worker = std::thread::Builder::new()
            .name("godwit-cache-writer".into())
            .spawn(move || cache_writer(worker_backend, worker_queue))?;
        Ok(Self {
            backend,
            queue,
            worker: Some(worker),
        })
    }

    pub fn load(&self, account: &AccountId) -> Option<AccountSnapshot> {
        let name = cache_name(account);
        match self.backend.get::<AccountSnapshot>(&name) {
            Ok(Some(snapshot)) if snapshot.belongs_to(account) => Some(snapshot),
            Ok(Some(_)) => {
                self.remove(account);
                None
            }
            Ok(None) => None,
            Err(_) => {
                // Alpha cache data is disposable; an obsolete or corrupt entry
                // is discarded and rebuilt from the server on the next sync.
                self.remove(account);
                None
            }
        }
    }

    /// Reads and decrypts a snapshot away from the UI thread. Results are
    /// drained by `take_loaded` during the normal app pump.
    pub fn request_load(&self, account: &AccountId) -> io::Result<()> {
        let backend = self.backend.clone();
        let queue = self.queue.clone();
        let account = account.clone();
        std::thread::Builder::new()
            .name("godwit-cache-reader".into())
            .spawn(move || {
                let key = cache_name(&account);
                let snapshot = match backend.get::<AccountSnapshot>(&key) {
                    Ok(Some(snapshot)) if snapshot.belongs_to(&account) => Some(snapshot),
                    Ok(None) => None,
                    Ok(Some(_)) | Err(_) => {
                        let _ = backend.remove(&key);
                        None
                    }
                };
                let mut state = queue.state.lock().unwrap_or_else(PoisonError::into_inner);
                state.completed_reads.push_back((account, snapshot));
            })?;
        Ok(())
    }

    /// Returns completed reads without waiting for disk or decryption.
    pub fn take_loaded(&self) -> Vec<(AccountId, Option<AccountSnapshot>)> {
        let mut state = self.queue.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.completed_reads.drain(..).collect()
    }

    /// Queues the newest snapshot; repeated updates to this account coalesce.
    pub fn save(&self, snapshot: &AccountSnapshot) {
        self.queue(snapshot.account.clone(), Some(snapshot.clone()));
    }

    /// Deletes every saved snapshot (Settings → Clear). Writes queued
    /// before it are dropped.
    pub fn clear(&self) {
        let mut state = self.queue.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.closed {
            state.pending.clear();
            state.clear = true;
            self.queue.ready.notify_one();
        }
    }

    /// The bytes it takes on disk.
    pub fn size(&self) -> u64 {
        self.backend.size().unwrap_or(0)
    }

    /// Queues deletion of one account's cache after logout.
    pub fn remove(&self, account: &AccountId) {
        self.queue(account.clone(), None);
    }

    fn queue(&self, account: AccountId, value: Option<AccountSnapshot>) {
        let mut state = self.queue.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.closed {
            state.pending.insert(account, value);
            self.queue.ready.notify_one();
        }
    }

    #[cfg(test)]
    fn flush(&self) {
        let mut state = self.queue.state.lock().unwrap_or_else(PoisonError::into_inner);
        while !state.pending.is_empty() || !state.active.is_empty() || state.clear {
            state = self.queue.idle.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        let mut state = self.queue.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.closed = true;
        self.queue.ready.notify_one();
        drop(state);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn cache_writer(backend: Arc<EncryptedCache>, queue: Arc<WriteQueue>) {
    loop {
        let jobs = {
            let mut state = queue.state.lock().unwrap_or_else(PoisonError::into_inner);
            while state.pending.is_empty() && !state.clear && !state.closed {
                state = queue.ready.wait(state).unwrap_or_else(PoisonError::into_inner);
            }
            // The flag stays set until the files are gone, so a `flush` in
            // between keeps waiting (taking it first let one return early).
            if state.clear {
                drop(state);
                if backend.clear().is_err() {
                    godwit_core::verbose!("note: clearing the encrypted cache failed");
                }
                queue.state.lock().unwrap_or_else(PoisonError::into_inner).clear = false;
                queue.idle.notify_all();
                continue;
            }
            if state.pending.is_empty() && state.closed {
                return;
            }
            let jobs = std::mem::take(&mut state.pending);
            state.active.extend(jobs.keys().cloned());
            jobs
        };

        for (account, snapshot) in jobs {
            let key = cache_name(&account);
            let result = match snapshot {
                Some(snapshot) => backend.set(&key, &snapshot),
                None => backend.remove(&key),
            };
            if result.is_err() {
                godwit_core::verbose!("note: an encrypted cache update failed");
            }
            let mut state = queue.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.active.remove(&account);
            queue.idle.notify_all();
        }
    }
}

fn cache_name(account: &AccountId) -> String {
    format!("recent/{account}")
}

/// The encrypted cache of downloaded pictures (images.rs), under a key
/// derived from the message cache's.
pub fn open_pictures() -> io::Result<EncryptedCache> {
    let config = godwit_core::config_dir::get()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "config directory unavailable"))?;
    let (secrets, _) = godwit_secrets::open(config.join("secrets"));
    let key = load_or_create_key(secrets.as_ref())?;
    EncryptedCache::derived(config.join("picture-cache"), &key, "Godwit picture cache")
}

fn load_or_create_key(store: &dyn SecretStore) -> io::Result<[u8; 32]> {
    if let Some(secret) = store.get("message-cache-key")? {
        if let Some(key) = decode_key(secret.expose()) {
            return Ok(key);
        }
        // Alpha cache entries have no migration requirement. A malformed old
        // key invalidates them; rotate the key and let normal sync rebuild.
        store.delete("message-cache-key")?;
    }
    let secret = Secret::random()?;
    let key = decode_key(secret.expose())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "generated cache key was invalid"))?;
    store.set("message-cache-key", &secret)?;
    Ok(key)
}

fn decode_key(encoded: &str) -> Option<[u8; 32]> {
    if encoded.len() != 64 {
        return None;
    }
    let mut key = [0u8; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(encoded.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use godwit_secrets::testing::Memory as MemorySecrets;

    struct Temp(std::path::PathBuf);

    impl Temp {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("godwit-cache-ui-{stamp}"));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn generated_secret_has_the_expected_key_shape() {
        let secret = Secret::random().unwrap();
        assert!(decode_key(secret.expose()).is_some());
        assert!(decode_key("bad").is_none());
    }

    #[test]
    fn cache_persists_account_snapshots_with_the_secret_store_key() {
        let temp = Temp::new();
        let secrets = MemorySecrets::default();
        let account = AccountId::new(godwit_core::network::Network::Stoat, "user");
        let snapshot = AccountSnapshot {
            account: account.clone(),
            rooms: Vec::new(),
            servers: Vec::new(),
            timelines: Vec::new(),
            profiles: Vec::new(),
        };
        let cache = Cache::with_store(temp.0.join("cache"), &secrets).unwrap();
        cache.save(&snapshot);
        cache.flush();
        assert_eq!(cache.load(&account), Some(snapshot.clone()));

        let reopened = Cache::with_store(temp.0.join("cache"), &secrets).unwrap();
        assert_eq!(reopened.load(&account), Some(snapshot));
        reopened.remove(&account);
        reopened.flush();
        assert_eq!(reopened.load(&account), None);
    }

    #[test]
    fn clearing_deletes_every_snapshot() {
        let temp = Temp::new();
        let secrets = MemorySecrets::default();
        let cache = Cache::with_store(temp.0.join("cache"), &secrets).unwrap();
        let accounts: Vec<AccountId> = ["a", "b"]
            .iter()
            .map(|u| AccountId::new(godwit_core::network::Network::Stoat, *u))
            .collect();
        for account in &accounts {
            cache.save(&AccountSnapshot {
                account: account.clone(),
                rooms: Vec::new(),
                servers: Vec::new(),
                timelines: Vec::new(),
                profiles: Vec::new(),
            });
        }
        cache.flush();
        assert!(cache.size() > 0);
        cache.clear();
        cache.flush();
        assert_eq!(cache.size(), 0);
        assert!(accounts.iter().all(|a| cache.load(a).is_none()));
    }

    #[test]
    fn cache_reads_complete_off_thread_and_are_drained_by_the_pump() {
        let temp = Temp::new();
        let secrets = MemorySecrets::default();
        let account = AccountId::new(godwit_core::network::Network::Stoat, "reader");
        let snapshot = AccountSnapshot {
            account: account.clone(),
            rooms: Vec::new(),
            servers: Vec::new(),
            timelines: Vec::new(),
            profiles: Vec::new(),
        };
        let cache = Cache::with_store(temp.0.join("cache"), &secrets).unwrap();
        cache.save(&snapshot);
        cache.flush();
        cache.request_load(&account).unwrap();
        let mut result = Vec::new();
        for _ in 0..200 {
            result = cache.take_loaded();
            if !result.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(result, vec![(account, Some(snapshot))]);
    }
}
