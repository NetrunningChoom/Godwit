// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! JSON sign-ins in the secret store, with network-owned schemas and names.
use godwit_secrets::{Secret, SecretStore};
use serde::{Serialize, de::DeserializeOwned};
use std::{marker::PhantomData, sync::OnceLock};

type Store = dyn SecretStore + Send + Sync;

pub struct Sessions<T> {
    name: &'static str,
    scan_slots: u32,
    limit: Option<u32>,
    save_error: &'static str,
    clear_error: &'static str,
    store: OnceLock<Box<Store>>,
    data: PhantomData<fn() -> T>,
}
impl<T: Serialize + DeserializeOwned> Sessions<T> {
    /// `limit` bounds all access for demo/test isolation when the network
    /// used that rule; `scan_slots` only bounds discovery of kept accounts.
    pub const fn new(
        name: &'static str,
        scan_slots: u32,
        limit: Option<u32>,
        save_error: &'static str,
        clear_error: &'static str,
    ) -> Self {
        Self {
            name,
            scan_slots,
            limit,
            save_error,
            clear_error,
            store: OnceLock::new(),
            data: PhantomData,
        }
    }
    fn store(&self) -> Option<&Store> {
        if let Some(store) = self.store.get() {
            return Some(store.as_ref());
        }
        let dir = crate::config_dir::get()?.join("secrets");
        Some(self.store.get_or_init(|| godwit_secrets::open(dir).0).as_ref())
    }
    /// Slot zero keeps the original single-account name so sign-ins carry over.
    pub fn name(&self, slot: u32) -> String {
        if slot == 0 {
            self.name.to_owned()
        } else {
            format!("{}-{slot}", self.name)
        }
    }
    /// Read through an injected store, without opening the OS keychain.
    pub fn read_from(&self, store: &dyn SecretStore, slot: u32) -> Option<T> {
        let secret = store.get(&self.name(slot)).ok()??;
        serde_json::from_str(secret.expose()).ok()
    }
    /// Serialize compact JSON with the network's unchanged field order/schema.
    pub fn write_to(&self, store: &dyn SecretStore, slot: u32, saved: &T) -> bool {
        let Ok(text) = serde_json::to_string(saved) else {
            return false;
        };
        store.set(&self.name(slot), &Secret::new(text)).is_ok()
    }
    fn kept(&self, slot: u32) -> bool {
        self.limit.is_none_or(|limit| slot < limit)
    }
    pub fn slots(&self) -> Vec<u32> {
        let Some(store) = self.store() else { return Vec::new() };
        (0..self.scan_slots)
            .filter(|&slot| self.read_from(store, slot).is_some())
            .collect()
    }
    pub fn load(&self, slot: u32) -> Option<T> {
        if !self.kept(slot) {
            return None;
        }
        self.read_from(self.store()?, slot)
    }
    pub fn save(&self, slot: u32, saved: &T) {
        if !self.kept(slot) {
            return;
        }
        if !self.store().is_some_and(|store| self.write_to(store, slot, saved)) {
            // Store errors can hold platform details: don't log secrets.
            eprintln!("{}", self.save_error);
        }
    }
    pub fn clear(&self, slot: u32) {
        if !self.kept(slot) {
            return;
        }
        if let Some(store) = self.store()
            && store.delete(&self.name(slot)).is_err()
        {
            eprintln!("{}", self.clear_error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_demo_slots_do_not_open_a_store_and_unbounded_slots_stay_valid() {
        let bounded = Sessions::<String>::new("test", 8, Some(8), "unused", "unused");
        assert!(bounded.kept(7));
        assert!(!bounded.kept(8));
        assert!(bounded.load(8).is_none());
        bounded.save(8, &"fake".into());
        bounded.clear(8);
        assert!(bounded.store.get().is_none());
        let unbounded = Sessions::<String>::new("test", 8, None, "unused", "unused");
        assert!(unbounded.kept(u32::MAX));
        assert_eq!(unbounded.name(u32::MAX), "test-4294967295");
    }
}
