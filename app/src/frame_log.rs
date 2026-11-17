// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Opt-in (`GODWIT_FRAME_LOG=1`) timing of the UI thread, to stderr: stalls
//! of the event loop (a missed frame, whatever caused it), our own callbacks
//! that took long, and timeline size changes (a page loaded) to line them up
//! with. A stall with no slow callback beside it was Slint's layout or paint.
//! Only times and counts are written, never content.

use std::cell::Cell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Callbacks at least this long are logged.
const SLOW: Duration = Duration::from_millis(4);
/// A gap this long between ticks of the watch timer is a stall.
const STALL: Duration = Duration::from_millis(34);
const TICK: Duration = Duration::from_millis(8);

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("GODWIT_FRAME_LOG").is_some_and(|v| v != "0"))
}

fn since_start() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// A callback queued from `at` ran for `took`.
pub fn work(at: &std::panic::Location<'_>, took: Duration) {
    if took >= SLOW {
        eprintln!(
            "[frame {:9.3}] work {:6.1} ms  {}:{}",
            since_start(),
            took.as_secs_f64() * 1000.0,
            at.file(),
            at.line()
        );
    }
}

/// The timeline's row count changed.
pub fn rows(before: usize, after: usize) {
    if enabled() && before != after {
        eprintln!("[frame {:9.3}] timeline rows {before} -> {after}", since_start());
    }
}

/// Starts the stall watch when logging is on; keep the timer alive.
pub fn watch() -> Option<slint::Timer> {
    if !enabled() {
        return None;
    }
    since_start();
    let timer = slint::Timer::default();
    let last = Cell::new(Instant::now());
    timer.start(slint::TimerMode::Repeated, TICK, move || {
        let now = Instant::now();
        let gap = now.duration_since(last.replace(now));
        if gap >= STALL {
            eprintln!(
                "[frame {:9.3}] STALL {:6.1} ms",
                since_start(),
                gap.as_secs_f64() * 1000.0
            );
        }
    });
    Some(timer)
}
