// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! One text selection across the timeline, as a browser selects in the
//! official client: press in a message's text and drag over as many
//! messages as you like, double-click for a word, triple-click for the whole
//! message (a browser's paragraph); Ctrl+C copies it all, with
//! who wrote what when it spans messages. Positions come from each block's
//! layout (textlayout.rs).

use std::ops::Range;
use std::sync::Arc;

use godwit_core::ids::MessageId;

use crate::textlayout;

/// A place by row: (row, block, byte offset).
type At = (usize, usize, usize);
use crate::view::RowKey;

/// A place in the timeline's text: a message's block, and a byte offset in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Point {
    pub message: MessageId,
    pub block: usize,
    pub index: usize,
}

/// A highlight rectangle: in which row and block, and where in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Rect {
    pub row: usize,
    pub block: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Default)]
pub struct ChatSelection {
    anchor: Option<Point>,
    focus: Option<Point>,
    /// Where the button went down, until it's released.
    pressed: Option<(usize, usize, f32, f32)>,
    /// The pointer moved while pressed: a drag, not a click.
    dragged: bool,
    /// The last double-click: a press soon after, in the same row, is a
    /// triple-click.
    doubled: Option<(usize, std::time::Instant)>,
}

/// How soon after a double-click a press counts as the third click.
const TRIPLE: std::time::Duration = std::time::Duration::from_millis(500);

impl ChatSelection {
    /// Whether anything is selected.
    pub fn is_empty(&self) -> bool {
        match (&self.anchor, &self.focus) {
            (Some(a), Some(f)) => a == f,
            _ => true,
        }
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.focus = None;
    }

    /// The button went down at (x, y) in row `row`'s block `block`; with
    /// `extend` (Shift) the selection grows to there instead.
    pub fn down(&mut self, rows: &[RowKey], row: usize, block: usize, x: f32, y: f32, extend: bool) {
        self.pressed = Some((row, block, x, y));
        self.dragged = false;
        let tripled = self
            .doubled
            .take()
            .is_some_and(|(r, at)| r == row && at.elapsed() < TRIPLE);
        if tripled && !extend && self.whole_message(rows, row, block) {
            // Kept on release, as a drag's selection is.
            self.dragged = true;
            return;
        }
        let Some(point) = point(rows, row, block, x, y) else {
            return;
        };
        if extend && self.anchor.is_some() {
            self.focus = Some(point);
        } else {
            self.anchor = Some(point.clone());
            self.focus = Some(point);
        }
    }

    /// The pressed pointer is over (x, y) in row `row`'s block `block`.
    pub fn drag(&mut self, rows: &[RowKey], row: usize, block: usize, x: f32, y: f32) {
        let Some((r, b, px, py)) = self.pressed else { return };
        // A few pixels of wobble is still a click.
        if (r, b) == (row, block) && (x - px).abs() < 3.0 && (y - py).abs() < 3.0 && !self.dragged {
            return;
        }
        self.dragged = true;
        if let Some(point) = point(rows, row, block, x, y) {
            self.focus = Some(point);
        }
    }

    /// The button came up. A click (no drag) on a link returns it to open,
    /// and a click anywhere else lets go of the selection.
    pub fn up(&mut self, rows: &[RowKey], row: usize, block: usize, x: f32, y: f32) -> Option<Arc<str>> {
        let pressed = self.pressed.take();
        if self.dragged || pressed.is_none() {
            return None;
        }
        let link = layout(rows, row, block).and_then(|(laid, _)| laid.link_at(x, y));
        if self.is_empty() || link.is_some() {
            self.clear();
        }
        link
    }

    /// A double-click: the word there.
    pub fn word(&mut self, rows: &[RowKey], row: usize, block: usize, x: f32, y: f32) {
        let Some((laid, message)) = layout(rows, row, block) else {
            return;
        };
        let range = laid.word_at(x, y);
        self.doubled = Some((row, std::time::Instant::now()));
        self.anchor = Some(Point {
            message: message.clone(),
            block,
            index: range.start,
        });
        self.focus = Some(Point {
            message,
            block,
            index: range.end,
        });
        self.dragged = true;
    }

    /// Selects all of row `row`'s message text: its blocks after the header
    /// (block 0, when it has one), or the header alone if that's where the
    /// click was. Whether there was anything to select.
    fn whole_message(&mut self, rows: &[RowKey], row: usize, block: usize) -> bool {
        let Some(key) = rows.get(row) else { return false };
        let Some(blocks) = textlayout::blocks(key.id()) else {
            return false;
        };
        let (first, last) = if block == 0 || blocks.len() == 1 {
            (block, block)
        } else {
            (1, blocks.len() - 1)
        };
        let Some(end) = blocks.get(last).map(|b| b.paragraph.text.len()) else {
            return false;
        };
        self.anchor = Some(Point {
            message: key.id().clone(),
            block: first,
            index: 0,
        });
        self.focus = Some(Point {
            message: key.id().clone(),
            block: last,
            index: end,
        });
        true
    }

    /// Whether (x, y) in a block is on a link (the pointer becomes a hand).
    pub fn on_link(&self, rows: &[RowKey], row: usize, block: usize, x: f32, y: f32) -> bool {
        layout(rows, row, block).is_some_and(|(laid, _)| laid.link_at(x, y).is_some())
    }

    /// The selection, start first, as rows and positions; None if empty or
    /// its messages are gone.
    fn span(&self, rows: &[RowKey]) -> Option<(At, At)> {
        let (a, f) = (self.anchor.as_ref()?, self.focus.as_ref()?);
        if a == f {
            return None;
        }
        let at = |p: &Point| Some((rows.iter().position(|r| r.id() == &p.message)?, p.block, p.index));
        let (a, f) = (at(a)?, at(f)?);
        Some(if a <= f { (a, f) } else { (f, a) })
    }

    /// What's selected of each block from start to end: (row, block, range).
    fn ranges(&self, rows: &[RowKey]) -> Vec<(usize, usize, Range<usize>)> {
        let Some((start, end)) = self.span(rows) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for row in start.0..=end.0 {
            let Some(blocks) = rows.get(row).and_then(|r| textlayout::blocks(r.id())) else {
                continue;
            };
            for (block, laid) in blocks.iter().enumerate() {
                let len = laid.paragraph.text.len();
                let from = if (row, block) == (start.0, start.1) {
                    start.2
                } else if (row, block) < (start.0, start.1) {
                    continue;
                } else {
                    0
                };
                let to = if (row, block) == (end.0, end.1) {
                    end.2
                } else if (row, block) > (end.0, end.1) {
                    continue;
                } else {
                    len
                };
                if from < to.min(len) {
                    out.push((row, block, from..to.min(len)));
                }
            }
        }
        out
    }

    /// The highlight rectangles.
    pub fn rects(&self, rows: &[RowKey]) -> Vec<Rect> {
        let mut out = Vec::new();
        for (row, block, range) in self.ranges(rows) {
            let Some(blocks) = rows.get(row).and_then(|r| textlayout::blocks(r.id())) else {
                continue;
            };
            let Some(laid) = blocks.get(block) else { continue };
            for (x, y, w, h) in laid.rects(range) {
                out.push(Rect { row, block, x, y, w, h });
            }
        }
        out
    }

    /// The selected text as it's copied, as a browser copies the official
    /// client's: a message's blocks on lines of their own, each message on a
    /// new line, a group's header (when it's selected) as "Name — time"
    /// after a blank line.
    pub fn text(&self, rows: &[RowKey]) -> Option<String> {
        let mut out = String::new();
        let mut last_row = None;
        for (row, block, range) in self.ranges(rows) {
            let Some(blocks) = rows.get(row).and_then(|key| textlayout::blocks(key.id())) else {
                continue;
            };
            let Some(laid) = blocks.get(block) else { continue };
            if !out.is_empty() {
                out.push('\n');
                if last_row != Some(row) && block == 0 {
                    out.push('\n');
                }
            }
            last_row = Some(row);
            out.push_str(&laid.paragraph.copy(range));
        }
        (!out.is_empty()).then_some(out)
    }
}

/// The layout of row `row`'s block `block`, and its message.
fn layout(rows: &[RowKey], row: usize, block: usize) -> Option<(textlayout::LaidRef, MessageId)> {
    let key = rows.get(row)?;
    let blocks = textlayout::blocks(key.id())?;
    let laid = textlayout::LaidRef::new(blocks, block)?;
    Some((laid, key.id().clone()))
}

fn point(rows: &[RowKey], row: usize, block: usize, x: f32, y: f32) -> Option<Point> {
    let (laid, message) = layout(rows, row, block)?;
    Some(Point {
        message,
        block,
        index: laid.index_at(x, y),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use slint::VecModel;

    use super::*;
    use crate::images::Images;
    use crate::ui::MessageItem;
    use crate::view::{Marks, sync_timeline};

    /// Three messages: two by Ann a minute apart (one group), one by Bob.
    fn rows() -> Vec<RowKey> {
        let list: Vec<Arc<_>> = [
            ("m1", "u1", "Ann", 0, "first"),
            ("m2", "u1", "Ann", 1, "second"),
            ("m3", "u2", "Bob", 2, "third"),
        ]
        .into_iter()
        .map(|(id, who, name, at, text)| Arc::new(crate::demo::msg(id, who, name, at, text)))
        .collect();
        let (revealed, invites) = (HashSet::new(), HashMap::new());
        let marks = Marks {
            unread_from: None,
            jump: None,
            revealed: &revealed,
            blocked_runs: &Default::default(),
            all: None,
            players: None,
            invites: &invites,
            servers: &[],
            text_width: 600.0,
        };
        let model = VecModel::<MessageItem>::default();
        let mut window = None;
        sync_timeline(&model, &[], &list, &marks, &mut window, false, &mut Images::offline())
    }

    fn at(rows: &[RowKey], row: usize, block: usize, index: usize) -> Point {
        Point {
            message: rows[row].id().clone(),
            block,
            index,
        }
    }

    #[test]
    fn copying_takes_headers_only_when_they_are_selected() {
        let rows = rows();
        let mut s = ChatSelection {
            anchor: Some(at(&rows, 0, 1, 2)),
            focus: Some(at(&rows, 2, 1, 3)),
            ..ChatSelection::default()
        };
        let time = |m: usize| crate::view::time(rows[m].message.sent_ms);
        // From inside Ann's first message to inside Bob's: Ann's header isn't
        // selected, Bob's is (it lies between).
        assert_eq!(
            s.text(&rows).as_deref(),
            Some(format!("rst\nsecond\n\nBob — {}\nthi", time(2)).as_str())
        );

        // From Ann's name on: her header too.
        s.anchor = Some(at(&rows, 0, 0, 0));
        s.focus = Some(at(&rows, 0, 1, 5));
        assert_eq!(
            s.text(&rows).as_deref(),
            Some(format!("Ann — {}\nfirst", time(0)).as_str())
        );

        // Within one message, just its text.
        s.anchor = Some(at(&rows, 1, 1, 0));
        s.focus = Some(at(&rows, 1, 1, 3));
        assert_eq!(s.text(&rows).as_deref(), Some("sec"));
        assert!(!s.rects(&rows).is_empty());
    }

    #[test]
    fn a_third_click_selects_the_whole_message() {
        let rows = rows();
        let mut s = ChatSelection::default();
        s.word(&rows, 1, 1, 2.0, 5.0);
        s.down(&rows, 1, 1, 2.0, 5.0, false);
        let _ = s.up(&rows, 1, 1, 2.0, 5.0);
        assert_eq!(s.text(&rows).as_deref(), Some("second"));
        // Too late, or elsewhere: an ordinary click.
        s.doubled = Some((1, std::time::Instant::now() - TRIPLE * 2));
        s.down(&rows, 1, 1, 2.0, 5.0, false);
        let _ = s.up(&rows, 1, 1, 2.0, 5.0);
        assert!(s.is_empty());
    }
}

/// A selection over a few laid-out blocks outside the timeline (the profile
/// card's name, username, status and bio): the same pointer handling as the
/// timeline's, through the same `Select` global, under its own row.
#[derive(Default)]
pub struct BlockSelection {
    /// (block, byte offset) where it started, and where it is now.
    anchor: Option<(usize, usize)>,
    focus: Option<(usize, usize)>,
    /// Where the button went down: released there, it's a click.
    pressed: Option<(usize, usize)>,
}

impl BlockSelection {
    pub fn is_empty(&self) -> bool {
        match (self.anchor, self.focus) {
            (Some(a), Some(f)) => a == f,
            _ => true,
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn down(&mut self, blocks: &[textlayout::Laid], block: usize, x: f32, y: f32) {
        let Some(laid) = blocks.get(block) else { return };
        let at = (block, laid.index_at(x, y));
        self.anchor = Some(at);
        self.focus = Some(at);
        self.pressed = Some(at);
    }

    pub fn drag(&mut self, blocks: &[textlayout::Laid], block: usize, x: f32, y: f32) {
        if self.pressed.is_none() {
            return;
        }
        if let Some(laid) = blocks.get(block) {
            self.focus = Some((block, laid.index_at(x, y)));
        }
    }

    /// The button came up: a click (no drag) on a link gives its address.
    pub fn up(&mut self, blocks: &[textlayout::Laid], block: usize, x: f32, y: f32) -> Option<Arc<str>> {
        let pressed = self.pressed.take()?;
        let laid = blocks.get(block)?;
        let here = (block, laid.index_at(x, y));
        if pressed == here {
            self.anchor = None;
            self.focus = None;
            return laid.link_at(x, y);
        }
        self.focus = Some(here);
        None
    }

    pub fn word(&mut self, blocks: &[textlayout::Laid], block: usize, x: f32, y: f32) {
        let Some(laid) = blocks.get(block) else { return };
        let word = laid.word_at(x, y);
        self.anchor = Some((block, word.start));
        self.focus = Some((block, word.end));
    }

    pub fn on_link(blocks: &[textlayout::Laid], block: usize, x: f32, y: f32) -> bool {
        blocks.get(block).and_then(|l| l.link_at(x, y)).is_some()
    }

    /// Each selected block and its byte range, in order.
    fn ranges(&self, blocks: &[textlayout::Laid]) -> Vec<(usize, Range<usize>)> {
        let (Some(a), Some(f)) = (self.anchor, self.focus) else {
            return Vec::new();
        };
        let (start, end) = if a <= f { (a, f) } else { (f, a) };
        (start.0..=end.0)
            .filter_map(|b| {
                let len = blocks.get(b)?.paragraph.text.len();
                let from = if b == start.0 { start.1 } else { 0 };
                let to = if b == end.0 { end.1 } else { len };
                (from < to).then_some((b, from..to))
            })
            .collect()
    }

    /// The highlight rectangles, in `row` (the blocks' row in `Select`).
    pub fn rects(&self, blocks: &[textlayout::Laid], row: usize) -> Vec<Rect> {
        let mut out = Vec::new();
        for (block, range) in self.ranges(blocks) {
            let Some(laid) = blocks.get(block) else { continue };
            for (x, y, w, h) in laid.rects(range) {
                out.push(Rect { row, block, x, y, w, h });
            }
        }
        out
    }

    /// The selected text as it's copied: each block on its own line.
    pub fn text(&self, blocks: &[textlayout::Laid]) -> Option<String> {
        let parts: Vec<String> = self
            .ranges(blocks)
            .into_iter()
            .filter_map(|(b, range)| Some(blocks.get(b)?.paragraph.copy(range)))
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n"))
    }
}
