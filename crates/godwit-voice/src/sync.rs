// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Locking without panics.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `m`, recovering the data if another thread panicked while holding
/// it. Mixer, loudness and frame state stay consistent enough to keep a call
/// running, which beats taking the whole audio path down with it.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
