// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The repeat guard: a request that keeps getting the same answer is a loop
//! somewhere in Godwit, so its next sends are spaced out instead of hitting
//! the server back to back.
//!
//! Only quiet time (no request with that key) or a different answer brings
//! the spacing back down; nothing the user does resets it, so a view that
//! re-asks on every frame can't talk its way past it either.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use super::Class;

/// Identical answers in a row before the spacing starts.
pub const TRIGGER: u32 = 3;
/// The longest a looping request is held back.
pub const MAX_HOLD: Duration = Duration::from_secs(60);
/// Each this long without the request takes the spacing down one step.
pub const QUIET: Duration = Duration::from_secs(60);
/// A pause this long between answer and next request isn't a loop: the
/// count of identical answers starts again (the spacing still only decays).
pub const WINDOW: Duration = Duration::from_secs(30);
/// Identical writes this close together are noted (never held back).
const WRITE_ECHO: Duration = Duration::from_secs(5);
/// The ceiling on guarded requests per network in [`BUDGET_SPAN`]: generous,
/// a last safety net, not pacing (each network paces itself).
pub const BUDGET: usize = 600;
pub const BUDGET_SPAN: Duration = Duration::from_secs(60);
/// Keys kept before idle ones are dropped.
const KEEP: usize = 4096;
const IDLE: Duration = Duration::from_secs(600);

#[derive(Debug)]
struct Entry {
    /// When the request was last let through (or is scheduled to be).
    sent: Instant,
    /// When its last answer came.
    done: Option<Instant>,
    answer: Option<u64>,
    /// Identical answers (or, for sign-ins, failures) in a row.
    same: u32,
    /// The spacing step (0: none).
    level: u32,
}

/// What the guard did with a request, for the log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Note {
    /// Identical answers (or failed sign-ins) in a row.
    pub repeats: u32,
    /// How long it was held before going out.
    pub held: Duration,
    /// The spacing step went up with this answer.
    pub stepped_up: bool,
    /// An identical write went out again within a few seconds.
    pub echo: bool,
}

#[derive(Debug, Default)]
pub struct Guard {
    keys: HashMap<u64, Entry>,
    budget: HashMap<&'static str, VecDeque<Instant>>,
}

/// The hold for spacing step `level`: 2 s, 4 s, 8 s … up to [`MAX_HOLD`].
pub fn hold_for(level: u32) -> Duration {
    if level == 0 {
        return Duration::ZERO;
    }
    let secs = 1u64.checked_shl(level.min(16)).unwrap_or(u64::MAX);
    Duration::from_secs(secs).min(MAX_HOLD)
}

impl Guard {
    /// How long a request with `key` must wait before going out; reserves
    /// its slot, so concurrent repeats queue up behind each other.
    pub fn before(&mut self, network: &'static str, key: u64, class: Class, now: Instant) -> Duration {
        self.prune(now);
        let mut wait = match class {
            Class::Live | Class::Write => Duration::ZERO,
            Class::Poll(floor) => self
                .keys
                .get(&key)
                .map_or(Duration::ZERO, |e| (e.sent + floor).saturating_duration_since(now)),
            Class::Read | Class::Auth => match self.keys.get_mut(&key) {
                Some(e) => {
                    if let Some(done) = e.done {
                        let quiet = now.saturating_duration_since(done);
                        let steps = u32::try_from(quiet.as_secs() / QUIET.as_secs()).unwrap_or(u32::MAX);
                        e.level = e.level.saturating_sub(steps);
                        if quiet >= WINDOW {
                            e.same = 0;
                        }
                    }
                    (e.sent + hold_for(e.level)).saturating_duration_since(now)
                }
                None => Duration::ZERO,
            },
        };
        if !matches!(class, Class::Live) {
            wait = wait.max(self.budget_wait(network, now + wait));
        }
        let at = now + wait;
        let entry = self.keys.entry(key).or_insert(Entry {
            sent: at,
            done: None,
            answer: None,
            same: 0,
            level: 0,
        });
        entry.sent = at;
        wait
    }

    /// Records the answer to a request with `key`: `answer` fingerprints it
    /// (None: no answer came), `ok` says whether it succeeded.
    pub fn after(&mut self, key: u64, class: Class, answer: Option<u64>, ok: bool, now: Instant) -> Note {
        let Some(e) = self.keys.get_mut(&key) else {
            return Note::default();
        };
        let previous_done = e.done;
        e.done = Some(now);
        let mut note = Note::default();
        match class {
            Class::Live | Class::Poll(_) => {}
            Class::Write => {
                let echo = answer.is_some()
                    && e.answer == answer
                    && previous_done.is_some_and(|d| now.saturating_duration_since(d) < WRITE_ECHO);
                note.echo = echo;
                e.answer = answer;
            }
            Class::Read => {
                if answer.is_some() && e.answer == answer {
                    e.same = e.same.saturating_add(1);
                } else {
                    // Something changed: not a loop (any more).
                    e.same = 1;
                    e.level = 0;
                    e.answer = answer;
                }
                if e.same >= TRIGGER {
                    e.level = e.level.saturating_add(1).min(16);
                    note.stepped_up = true;
                }
                note.repeats = e.same;
            }
            Class::Auth => {
                if ok {
                    e.same = 0;
                    e.level = 0;
                } else {
                    e.same = e.same.saturating_add(1);
                    if e.same >= TRIGGER {
                        e.level = e.level.saturating_add(1).min(16);
                        note.stepped_up = true;
                    }
                }
                e.answer = answer;
                note.repeats = e.same;
            }
        }
        note
    }

    fn budget_wait(&mut self, network: &'static str, at: Instant) -> Duration {
        let sent = self.budget.entry(network).or_default();
        while sent
            .front()
            .is_some_and(|t| at.saturating_duration_since(*t) >= BUDGET_SPAN)
        {
            sent.pop_front();
        }
        let wait = if sent.len() >= BUDGET {
            sent.front().map_or(Duration::ZERO, |oldest| {
                (*oldest + BUDGET_SPAN).saturating_duration_since(at)
            })
        } else {
            Duration::ZERO
        };
        sent.push_back(at + wait);
        wait
    }

    fn prune(&mut self, now: Instant) {
        if self.keys.len() > KEEP {
            self.keys
                .retain(|_, e| now.saturating_duration_since(e.done.unwrap_or(e.sent)) < IDLE);
        }
    }

    #[cfg(test)]
    fn level(&self, key: u64) -> u32 {
        self.keys.get(&key).map_or(0, |e| e.level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(g: &mut Guard, class: Class, answer: u64, now: Instant) -> (Duration, Note) {
        let wait = g.before("test", 1, class, now);
        let note = g.after(1, class, Some(answer), true, now + wait);
        (wait, note)
    }

    #[test]
    fn identical_answers_space_out_reads() {
        let mut g = Guard::default();
        let t = Instant::now();
        assert_eq!(round(&mut g, Class::Read, 7, t).0, Duration::ZERO);
        assert_eq!(round(&mut g, Class::Read, 7, t).0, Duration::ZERO);
        let (_, note) = round(&mut g, Class::Read, 7, t);
        assert!(note.stepped_up);
        assert_eq!(note.repeats, 3);
        let (wait, _) = round(&mut g, Class::Read, 7, t);
        assert_eq!(wait, Duration::from_secs(2));
        let (wait, _) = round(&mut g, Class::Read, 7, t + Duration::from_secs(2));
        assert_eq!(wait, Duration::from_secs(4));
    }

    #[test]
    fn hold_caps_at_a_minute() {
        assert_eq!(hold_for(1), Duration::from_secs(2));
        assert_eq!(hold_for(5), Duration::from_secs(32));
        assert_eq!(hold_for(6), MAX_HOLD);
        assert_eq!(hold_for(40), MAX_HOLD);
    }

    #[test]
    fn a_different_answer_ends_the_loop() {
        let mut g = Guard::default();
        let t = Instant::now();
        for _ in 0..4 {
            round(&mut g, Class::Read, 7, t);
        }
        assert!(g.level(1) > 0);
        g.before("test", 1, Class::Read, t + Duration::from_secs(10));
        g.after(1, Class::Read, Some(8), true, t + Duration::from_secs(10));
        assert_eq!(g.level(1), 0);
    }

    #[test]
    fn only_quiet_time_brings_the_spacing_down_step_by_step() {
        let mut g = Guard::default();
        let t = Instant::now();
        let mut now = t;
        for _ in 0..6 {
            let (wait, _) = round(&mut g, Class::Read, 7, now);
            now += wait;
        }
        let level = g.level(1);
        assert!(level >= 3);
        // One quiet minute: one step down, not back to zero.
        let later = now + QUIET;
        g.before("test", 1, Class::Read, later);
        assert_eq!(g.level(1), level - 1);
    }

    #[test]
    fn live_and_write_are_never_held() {
        let mut g = Guard::default();
        let t = Instant::now();
        for _ in 0..10 {
            assert_eq!(round(&mut g, Class::Live, 7, t).0, Duration::ZERO);
        }
        let mut g = Guard::default();
        round(&mut g, Class::Write, 7, t);
        let (wait, note) = round(&mut g, Class::Write, 7, t);
        assert_eq!(wait, Duration::ZERO);
        assert!(note.echo);
    }

    #[test]
    fn polls_keep_their_floor_only() {
        let mut g = Guard::default();
        let t = Instant::now();
        let floor = Duration::from_secs(5);
        assert_eq!(round(&mut g, Class::Poll(floor), 7, t).0, Duration::ZERO);
        assert_eq!(
            round(&mut g, Class::Poll(floor), 7, t + Duration::from_secs(1)).0,
            Duration::from_secs(4)
        );
        for i in 0..10 {
            let (wait, note) = round(&mut g, Class::Poll(floor), 7, t + floor * (i + 2));
            assert_eq!(wait, Duration::ZERO);
            assert!(!note.stepped_up);
        }
    }

    #[test]
    fn failed_sign_ins_space_out() {
        let mut g = Guard::default();
        let t = Instant::now();
        for _ in 0..3 {
            g.before("test", 1, Class::Auth, t);
            g.after(1, Class::Auth, Some(1), false, t);
        }
        assert_eq!(g.before("test", 1, Class::Auth, t), Duration::from_secs(2));
        g.after(1, Class::Auth, Some(2), true, t);
        assert_eq!(g.level(1), 0);
    }

    #[test]
    fn the_budget_caps_a_network() {
        let mut g = Guard::default();
        let t = Instant::now();
        for key in 0..BUDGET as u64 {
            assert_eq!(g.before("test", key, Class::Write, t), Duration::ZERO);
        }
        assert_eq!(g.before("test", u64::MAX, Class::Write, t), BUDGET_SPAN);
        assert_eq!(g.before("other", u64::MAX, Class::Write, t), Duration::ZERO);
    }
}
