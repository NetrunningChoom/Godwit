// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! In-memory secrets for tests: never opens a file or the OS keychain.
use crate::{Secret, SecretStore};
use std::{
    collections::HashMap,
    io,
    sync::{Mutex, MutexGuard, PoisonError},
};

#[derive(Default)]
pub struct Memory(Mutex<HashMap<String, Secret>>);
impl Memory {
    fn entries(&self) -> MutexGuard<'_, HashMap<String, Secret>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
impl SecretStore for Memory {
    fn get(&self, name: &str) -> io::Result<Option<Secret>> {
        Ok(self.entries().get(name).cloned())
    }
    fn set(&self, name: &str, secret: &Secret) -> io::Result<()> {
        self.entries().insert(name.to_owned(), secret.clone());
        Ok(())
    }
    fn delete(&self, name: &str) -> io::Result<()> {
        self.entries().remove(name);
        Ok(())
    }
}
