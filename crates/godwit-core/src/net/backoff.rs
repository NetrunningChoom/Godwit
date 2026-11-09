// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Reconnect pacing shared by every connection: waits grow with each
//! failed try, time without a network doesn't count as a try, a network
//! change ends a wait early, and a connection that held long enough starts
//! the count again.

use std::hash::{BuildHasher, RandomState};
use std::time::{Duration, Instant};

use super::watch;

#[derive(Debug)]
pub struct Backoff {
    base: Duration,
    max: Duration,
    /// A connection that held this long counts as healthy again.
    steady: Duration,
    attempts: u32,
    connected_at: Option<Instant>,
}

impl Backoff {
    /// Waits start at `base` and double up to `max`; a connection that held
    /// for `steady` resets them.
    pub fn new(base: Duration, max: Duration, steady: Duration) -> Self {
        Self {
            base,
            max,
            steady,
            attempts: 0,
            connected_at: None,
        }
    }

    /// Failed tries since the last steady connection.
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// The connection is up.
    pub fn connected(&mut self) {
        self.connected_at = Some(Instant::now());
    }

    /// Forget the failures (the user asked to connect).
    pub fn reset(&mut self) {
        self.attempts = 0;
        self.connected_at = None;
    }

    /// The wait before the next try, without waiting; counts the try.
    pub fn next_delay(&mut self) -> Duration {
        if self.connected_at.take().is_some_and(|at| at.elapsed() >= self.steady) {
            self.attempts = 0;
        }
        let step = self.attempts.min(16);
        self.attempts = self.attempts.saturating_add(1);
        let raw = self.base.saturating_mul(1u32 << step).min(self.max);
        jitter(raw)
    }

    /// Waits before the next try: first for a network if there's none (not
    /// counted as a try), then the next delay, cut short by a network change.
    pub async fn wait(&mut self, network: &str, what: &str) {
        if watch::until_online().await {
            // The network just came back: try at once, the failures were its absence.
            self.attempts = 0;
            super::log::connection(network, what, "network back, retrying");
            return;
        }
        let delay = self.next_delay();
        super::log::connection(
            network,
            what,
            &format!("retry {} in {:.1}s", self.attempts, delay.as_secs_f32()),
        );
        if watch::sleep_or_change(delay).await {
            super::log::connection(network, what, "network changed, retrying now");
        }
    }
}

/// ±20%, so many connections dropped together don't come back in step.
fn jitter(d: Duration) -> Duration {
    let r = RandomState::new().hash_one(Instant::now()) % 401;
    let factor = 0.8 + (r as f64) / 1000.0;
    d.mul_f64(factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_double_up_to_the_cap() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(8), Duration::from_secs(60));
        let d: Vec<_> = (0..6).map(|_| b.next_delay().as_secs_f64()).collect();
        for (got, want) in d.iter().zip([1.0, 2.0, 4.0, 8.0, 8.0, 8.0]) {
            assert!(*got >= want * 0.79 && *got <= want * 1.21, "{d:?}");
        }
    }

    #[test]
    fn a_short_connection_keeps_the_count() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(60), Duration::from_secs(60));
        b.next_delay();
        b.next_delay();
        b.connected();
        b.next_delay();
        assert_eq!(b.attempts(), 3);
    }

    #[test]
    fn a_steady_connection_resets_it() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(60), Duration::ZERO);
        b.next_delay();
        b.next_delay();
        b.connected();
        b.next_delay();
        assert_eq!(b.attempts(), 1);
    }
}
