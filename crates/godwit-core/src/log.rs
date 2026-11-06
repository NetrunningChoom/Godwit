// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! How much Godwit writes to standard error. Release builds write only what
//! really went wrong; routine diagnostics (connections, lookups, timings)
//! only in debug builds, or with `--verbose` / `GODWIT_VERBOSE=1`. Those may
//! name people, rooms or files, so they stay out of a release's output.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static FORCED: AtomicBool = AtomicBool::new(false);

/// Turns the diagnostics on (the `--verbose` flag).
pub fn set_verbose(on: bool) {
    FORCED.store(on, Ordering::Relaxed);
}

/// Whether diagnostics are written.
pub fn verbose() -> bool {
    static ENV: OnceLock<bool> = OnceLock::new();
    cfg!(debug_assertions)
        || FORCED.load(Ordering::Relaxed)
        || *ENV.get_or_init(|| std::env::var_os("GODWIT_VERBOSE").is_some_and(|v| v != "0" && !v.is_empty()))
}

/// `eprintln!`, only when diagnostics are on (see [`verbose`]).
#[macro_export]
macro_rules! verbose {
    ($($arg:tt)*) => {
        if $crate::log::verbose() {
            eprintln!($($arg)*);
        }
    };
}
