// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The interface scale chosen in Settings, on top of the display's own.
//!
//! Slint draws at the window's scale factor, which the platform sets from
//! the display (2 on a HiDPI screen). Godwit multiplies it by the user's
//! choice: it tells the window the new factor, then resizes it to the same
//! physical size at that factor, so the layout follows at once (no restart).
//! When the display's factor changes (the window moved to another screen),
//! the platform overwrites ours; the next refresh sees that and applies the
//! choice again on top of the new one.

use slint::ComponentHandle;
use slint::platform::WindowEvent;

use crate::ui::AppWindow;

/// Settings' choices, as fractions of the display's own scale.
pub const CHOICES: [f32; 8] = [0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0];
pub const LABELS: [&str; 8] = ["80%", "90%", "100%", "110%", "125%", "150%", "175%", "200%"];

/// The row of `CHOICES` nearest `scale`.
pub fn index_of(scale: f32) -> usize {
    CHOICES
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| (*a - scale).abs().total_cmp(&(*b - scale).abs()))
        .map_or(2, |(i, _)| i)
}

#[derive(Default)]
pub struct Scale {
    /// The display's factor (before ours), once the window had one.
    display: Option<f32>,
    /// The factor we last gave the window.
    applied: Option<f32>,
}

impl Scale {
    /// Make the window draw at the display's factor × `choice`. Cheap when
    /// nothing changed, so it runs on every refresh.
    pub fn apply(&mut self, ui: &AppWindow, choice: f32) {
        let window = ui.window();
        // No window yet (before the event loop made it): nothing to scale.
        if window.size().width == 0 {
            return;
        }
        let now = window.scale_factor();
        // Ours is still in place: only the choice may have changed. Otherwise
        // the platform set the display's factor (at start or on a move).
        if self.applied.is_none_or(|a| (a - now).abs() > 0.001) {
            self.display = Some(now);
        }
        let Some(display) = self.display else { return };
        let want = display * choice.clamp(0.5, 3.0);
        if (want - now).abs() <= 0.001 {
            self.applied = Some(want);
            return;
        }
        let physical = window.size();
        window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: want });
        window.dispatch_event(WindowEvent::Resized {
            size: physical.to_logical(want),
        });
        self.applied = Some(want);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nearest_choice_is_picked() {
        assert_eq!(index_of(1.0), 2);
        assert_eq!(index_of(1.24), 4);
        assert_eq!(index_of(9.0), CHOICES.len() - 1);
        assert_eq!(CHOICES.len(), LABELS.len());
    }
}
