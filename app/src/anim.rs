// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Animated pictures without rebuilding rows. Each animation on screen has a
//! slot in one shared list of current frames (the `Anim` global in Slint);
//! pictures that animate show `Anim.frames[slot]`. A tick only swaps the
//! slots whose frame changed: no row is rebuilt and nothing is laid out again
//! (rebuilding rows re-laid out the whole timeline every frame, which lagged).
//!
//! Only slots in rows near the view tick (`set_live`), on one single-shot
//! timer set for the soonest next frame: an idle window with no animation on
//! screen has no timer at all. Large animations stream (stream.rs): their
//! worker runs only while they're live, and each frame shows when it's due.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use slint::{Image, Model, VecModel};

use crate::images::Animation;
use crate::stream::{Player, Source};

/// The fastest a frame changes (see images.rs, `MIN_DELAY_MS`).
const MIN_TICK: Duration = Duration::from_millis(20);

enum Slot {
    /// Every frame and its delay (ms); empty for a released slot.
    Frames {
        frames: Vec<(Image, u32)>,
        total: u32,
        /// The frame in the shared list now.
        shown: usize,
    },
    /// Decoded while live; `due` is when (clock ms) the next frame shows.
    Stream {
        source: std::sync::Arc<Source>,
        player: Option<Player>,
        due: u128,
    },
}

impl Slot {
    fn released() -> Self {
        Self::Frames {
            frames: Vec::new(),
            total: 1,
            shown: 0,
        }
    }
}

pub struct Animator {
    model: Rc<VecModel<Image>>,
    slots: Vec<Slot>,
    /// Which picture (address and size key) has which slot.
    keys: HashMap<String, usize>,
    free: Vec<usize>,
    live: HashSet<usize>,
    /// Every animation plays against this clock, so copies move together.
    clock: Instant,
    timer: slint::Timer,
    this: Weak<RefCell<Animator>>,
}

impl Animator {
    pub fn new() -> Rc<RefCell<Self>> {
        Rc::new_cyclic(|this| {
            RefCell::new(Self {
                model: Rc::new(VecModel::default()),
                slots: Vec::new(),
                keys: HashMap::new(),
                free: Vec::new(),
                live: HashSet::new(),
                clock: Instant::now(),
                timer: slint::Timer::default(),
                this: this.clone(),
            })
        })
    }

    /// The shared list, for the window's `Anim` global.
    pub fn model(&self) -> Rc<VecModel<Image>> {
        self.model.clone()
    }

    /// The slot for an animation (`key` names the picture and its size),
    /// given one if it has none; `still` shows until a streamed one plays.
    pub fn slot(&mut self, key: &str, animation: &Animation, still: &Image) -> usize {
        if let Some(&slot) = self.keys.get(key) {
            return slot;
        }
        let (slot, image) = match animation {
            Animation::Frames(frames, total) => {
                let total = (*total).max(1);
                let (shown, image) = self.frame_at(frames, total);
                (
                    Slot::Frames {
                        frames: frames.clone(),
                        total,
                        shown,
                    },
                    image,
                )
            }
            Animation::Stream(source) => (
                Slot::Stream {
                    source: source.clone(),
                    player: None,
                    due: 0,
                },
                still.clone(),
            ),
        };
        let index = match self.free.pop() {
            Some(i) => {
                self.slots[i] = slot;
                self.model.set_row_data(i, image);
                i
            }
            None => {
                self.slots.push(slot);
                self.model.push(image);
                self.slots.len() - 1
            }
        };
        self.keys.insert(key.to_owned(), index);
        index
    }

    /// Visible animations stay cached even when their rows need no rebuild.
    pub fn is_live(&self, key: &str) -> bool {
        self.keys.get(key).is_some_and(|slot| self.live.contains(slot))
    }

    /// The picture left the cache: its frames go, and the slot can be reused.
    pub fn release(&mut self, key: &str) {
        let Some(slot) = self.keys.remove(key) else { return };
        if let Some(s) = self.slots.get_mut(slot) {
            *s = Slot::released();
            self.model.set_row_data(slot, Image::default());
        }
        self.live.remove(&slot);
        self.free.push(slot);
    }

    /// The slots in rows near the view: only these tick.
    pub fn set_live(&mut self, live: HashSet<usize>) {
        if live == self.live {
            return;
        }
        // Streams leaving the view stop their workers (they keep their frame).
        for slot in self.live.difference(&live) {
            if let Some(Slot::Stream { player, .. }) = self.slots.get_mut(*slot) {
                *player = None;
            }
        }
        self.live = live;
        self.tick();
    }

    /// The frame playing now in an animation, and its picture.
    fn frame_at(&self, frames: &[(Image, u32)], total: u32) -> (usize, Image) {
        let mut at = (self.clock.elapsed().as_millis() % u128::from(total)) as u32;
        for (i, (image, delay)) in frames.iter().enumerate() {
            if at < *delay {
                return (i, image.clone());
            }
            at -= delay;
        }
        (0, frames.first().map(|(i, _)| i.clone()).unwrap_or_default())
    }

    /// Shows each live animation's current frame, and sets the timer for the
    /// soonest next one.
    fn tick(&mut self) {
        let now = self.clock.elapsed().as_millis();
        let mut soonest: Option<u64> = None;
        let mut soon = |ms: u64| soonest = Some(soonest.map_or(ms, |x: u64| x.min(ms)));
        let live: Vec<usize> = self.live.iter().copied().collect();
        for slot in live {
            let Some(s) = self.slots.get_mut(slot) else { continue };
            match s {
                Slot::Frames { frames, total, shown } => {
                    if frames.len() < 2 {
                        continue;
                    }
                    let mut at = (now % u128::from(*total)) as u32;
                    let (mut index, mut left) = (0, 0);
                    for (i, (_, delay)) in frames.iter().enumerate() {
                        if at < *delay {
                            index = i;
                            left = delay - at;
                            break;
                        }
                        at -= delay;
                    }
                    soon(u64::from(left));
                    if index != *shown {
                        *shown = index;
                        if slot < self.model.row_count() {
                            self.model.set_row_data(slot, frames[index].0.clone());
                        }
                    }
                }
                Slot::Stream { source, player, due } => {
                    if player.is_none() {
                        *player = Player::start(source);
                        *due = now;
                    }
                    let Some(p) = player else { continue };
                    if now < *due {
                        soon(u64::try_from(*due - now).unwrap_or(u64::MAX));
                        continue;
                    }
                    match p.next() {
                        Some((image, delay)) => {
                            // Late frames don't pile up: the next one is due a
                            // full delay after this one shows.
                            *due = now + u128::from(delay);
                            soon(u64::from(delay));
                            if slot < self.model.row_count() {
                                self.model.set_row_data(slot, image);
                            }
                        }
                        // Not decoded yet: look again shortly.
                        None => soon(MIN_TICK.as_millis() as u64),
                    }
                }
            }
        }
        match soonest {
            Some(ms) => {
                let this = self.this.clone();
                self.timer.start(
                    slint::TimerMode::SingleShot,
                    Duration::from_millis(ms).max(MIN_TICK),
                    move || {
                        if let Some(a) = this.upgrade()
                            && let Ok(mut a) = a.try_borrow_mut()
                        {
                            a.tick();
                        }
                    },
                );
            }
            None => self.timer.stop(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames() -> Animation {
        Animation::Frames(raw_frames(), 200)
    }

    fn raw_frames() -> Vec<(Image, u32)> {
        let px = |v: u8| {
            Image::from_rgba8(slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                &[v, v, v, 255],
                1,
                1,
            ))
        };
        vec![(px(1), 100), (px(2), 100)]
    }

    #[test]
    fn slots_are_shared_released_and_reused() {
        let anim = Animator::new();
        let mut a = anim.borrow_mut();
        let one = a.slot("a", &frames(), &Image::default());
        assert_eq!(a.slot("a", &frames(), &Image::default()), one, "one slot per picture");
        let two = a.slot("b", &frames(), &Image::default());
        assert_ne!(one, two);
        assert_eq!(a.model().row_count(), 2);
        a.release("a");
        assert_eq!(
            a.slot("c", &frames(), &Image::default()),
            one,
            "released slots are reused"
        );
        assert_eq!(a.model().row_count(), 2);
    }

    #[test]
    fn only_live_slots_tick() {
        let anim = Animator::new();
        let mut a = anim.borrow_mut();
        let slot = a.slot("a", &frames(), &Image::default());
        a.clock = Instant::now() - Duration::from_millis(150);
        a.set_live(HashSet::new());
        let shown = |a: &Animator| match &a.slots[slot] {
            Slot::Frames { shown, .. } => *shown,
            Slot::Stream { .. } => usize::MAX,
        };
        assert_eq!(shown(&a), 0, "not on screen: no change");
        a.set_live(HashSet::from([slot]));
        assert_eq!(shown(&a), 1);
    }
}
