// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Mouse-wheel acceleration: the longer the wheel keeps turning, the further
//! each notch scrolls, gently, as browsers and desktops do. A pause or a
//! change of direction starts over at one notch's distance. The speed is
//! capped, too: a free-spinning wheel flew through history faster than it
//! could load, starting pictures for every row it passed.
//!
//! Only notched wheels (line deltas) are touched. Touchpads send pixel deltas
//! with their own momentum, which stay as they are.

use std::time::{Duration, Instant};

/// Notches closer together than this keep a streak going.
const STREAK_GAP: Duration = Duration::from_millis(180);
/// How much each further notch in a streak adds…
const STEP: f32 = 0.12;
/// …up to this many times a notch's usual distance.
const MAX: f32 = 2.0;
/// The fastest the wheel scrolls, in logical pixels a second, counted over
/// `WINDOW`: past it, further turns in that window scroll no further.
const MAX_SPEED: f32 = 4000.0;
const WINDOW: Duration = Duration::from_millis(100);
/// Slint's distance for one notch (its winit backend's), in logical pixels.
pub const LINE: f32 = 60.0;

#[derive(Default)]
pub struct Wheel {
    last: Option<(Instant, bool)>,
    streak: u32,
    /// The current speed window: when it began, and the pixels scrolled in it.
    window: Option<(Instant, f32)>,
}

impl Wheel {
    /// The factor for a notch turned at `now` (`down`: its direction).
    pub fn factor(&mut self, now: Instant, down: bool) -> f32 {
        let going = self
            .last
            .is_some_and(|(at, dir)| dir == down && now.saturating_duration_since(at) < STREAK_GAP);
        self.streak = if going { self.streak.saturating_add(1) } else { 0 };
        self.last = Some((now, down));
        // u16 is plenty: past (MAX - 1) / STEP notches the factor is capped.
        let streak = f32::from(u16::try_from(self.streak).unwrap_or(u16::MAX));
        (1.0 + STEP * streak).min(MAX)
    }

    /// The pixels a notched turn of `lines` (x, y; negative: down/right, as
    /// winit gives them) scrolls at `now`: accelerated, then capped at
    /// [`MAX_SPEED`].
    pub fn scroll(&mut self, now: Instant, x: f32, y: f32) -> (f32, f32) {
        let factor = self.factor(now, y < 0.0);
        let (dx, dy) = (x * LINE * factor, y * LINE * factor);
        let (start, spent) = match self.window {
            Some((start, spent)) if now.saturating_duration_since(start) < WINDOW => (start, spent),
            _ => (now, 0.0),
        };
        let budget = (MAX_SPEED * WINDOW.as_secs_f32() - spent).max(0.0);
        let wanted = dx.abs().max(dy.abs());
        let scale = if wanted > budget && wanted > 0.0 {
            budget / wanted
        } else {
            1.0
        };
        self.window = Some((start, spent + wanted * scale));
        (dx * scale, dy * scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_spin_speeds_up_gently_and_a_pause_starts_over() {
        let mut wheel = Wheel::default();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        assert_eq!(wheel.factor(at(0), true), 1.0, "a single notch is as before");
        let spun: Vec<f32> = (1..=30).map(|i| wheel.factor(at(i * 50), true)).collect();
        assert!(spun.windows(2).all(|w| w[1] >= w[0]), "it only grows while spinning");
        assert!((spun[0] - 1.12).abs() < 1e-4, "gently");
        assert_eq!(spun[29], MAX, "and not without end");
        assert_eq!(wheel.factor(at(1800), true), 1.0, "a pause starts over");
        wheel.factor(at(1850), true);
        assert_eq!(wheel.factor(at(1900), false), 1.0, "so does turning back");
    }

    #[test]
    fn the_speed_is_capped() {
        let mut wheel = Wheel::default();
        let t0 = Instant::now();
        // Ten notches down within 50 ms: at most 400 px in that 100 ms.
        let total: f32 = (0..10)
            .map(|i| wheel.scroll(t0 + Duration::from_millis(i * 5), 0.0, -1.0).1)
            .sum();
        assert!((total + 400.0).abs() < 0.01, "{total}");
        // A new window scrolls again.
        let (_, next) = wheel.scroll(t0 + Duration::from_millis(150), 0.0, -1.0);
        assert!(next < 0.0);
    }
}
