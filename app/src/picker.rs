// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The emoji picker's contents, as Godwit and the official picker: the
//! room's custom emoji by server, then the Unicode emoji by category (as
//! Yapfast groups them; the official picker has one "Default" list), in rows
//! of nine; a search filters both. Pictures load only for rows on screen.
//! Only emoji the pack draws are offered (`crate::emoji::drawn`).

use std::collections::HashSet;
use std::sync::Arc;

use godwit_core::message::EmojiGroup;
use slint::{Image, Model, ModelRc, VecModel};

use crate::images::{Images, PICKER_EMOJI};
use crate::ui::{EmojiCellItem, EmojiRowItem};

pub const COLUMNS: usize = 9;
/// Rows loaded beyond the last one on screen, and above the first: scrolling
/// (or jumping a category) finds them drawn (the render padding).
const AHEAD: usize = 12;
const BEHIND: usize = 4;
/// Rows the picker shows at once (its height over the row height).
const VISIBLE: usize = 11;

/// One emoji: what to show and what picking it does.
#[derive(Clone, Debug)]
pub struct Pick {
    pub name: Arc<str>,
    /// A Unicode emoji itself (shown while its picture loads); empty for custom ones.
    pub glyph: Arc<str>,
    pub url: Arc<str>,
    /// Sent when reacting.
    pub reaction: Arc<str>,
    /// Written into the composer.
    pub code: Arc<str>,
}

pub enum Row {
    Header(Arc<str>),
    Cells(Vec<Pick>),
}

fn unicode(e: &'static emojis::Emoji) -> Pick {
    let url = crate::emoji::url_of(e);
    Pick {
        name: e.shortcode().unwrap_or(e.name()).into(),
        glyph: e.as_str().into(),
        url: url.into(),
        reaction: e.as_str().into(),
        code: e.as_str().into(),
    }
}

fn chunked(rows: &mut Vec<Row>, cells: Vec<Pick>) {
    for chunk in cells.chunks(COLUMNS) {
        rows.push(Row::Cells(chunk.to_vec()));
    }
}

/// Which Unicode emoji a network has: all, those up to an Emoji version,
/// or its own set (Teams).
#[derive(Clone, Copy, Default)]
pub enum Allowed {
    #[default]
    All,
    UpTo(u32, u32),
    Only(&'static std::collections::HashSet<String>),
}

impl Allowed {
    fn has(self, e: &emojis::Emoji) -> bool {
        match self {
            Self::All => true,
            Self::UpTo(major, minor) => e.unicode_version() <= emojis::UnicodeVersion::new(major, minor),
            Self::Only(set) => set.contains(&e.as_str().replace('\u{fe0f}', "")),
        }
    }
}

/// The rows for `query` (empty: everything), with `custom` first; only the
/// Unicode emoji the network has (`allowed`).
pub fn rows(custom: &[&EmojiGroup], query: &str, allowed: Allowed) -> Vec<Row> {
    let needle = normalize(query);
    let known = |e: &&'static emojis::Emoji| allowed.has(e) && crate::emoji::drawn(e);
    let mut rows = Vec::new();
    let customs = |g: &EmojiGroup| -> Vec<Pick> {
        g.emoji
            .iter()
            .filter(|e| needle.is_empty() || match_rank(std::iter::once(&*e.name), &needle).is_some())
            .map(|e| Pick {
                name: e.name.clone(),
                glyph: "".into(),
                url: e.image_url.clone(),
                reaction: e.reaction.clone(),
                code: e.code.clone(),
            })
            .collect()
    };
    if needle.is_empty() {
        for g in custom {
            rows.push(Row::Header(g.title.clone()));
            chunked(&mut rows, customs(g));
        }
        for group in emojis::Group::iter() {
            let cells: Vec<Pick> = group.emojis().filter(known).map(unicode).collect();
            if cells.is_empty() {
                continue;
            }
            rows.push(Row::Header(group_title(group).into()));
            chunked(&mut rows, cells);
        }
    } else {
        let mut cells: Vec<Pick> = custom.iter().flat_map(|g| customs(g)).collect();
        // Best matches first (exact, then prefix, then anywhere); the
        // order within a rank stays the standard one.
        let mut found: Vec<(u8, &'static emojis::Emoji)> = emojis::iter()
            .filter(known)
            .filter_map(|e| match_rank(e.shortcodes().chain(std::iter::once(e.name())), &needle).map(|r| (r, e)))
            .collect();
        found.sort_by_key(|(r, _)| *r);
        cells.extend(found.into_iter().map(|(_, e)| unicode(e)));
        chunked(&mut rows, cells);
    }
    rows
}

/// A search term as the official picker compares it: case-insensitive, and
/// ":thumbs up:", "thumbs up" and "thumbs_up" are the same (for-web's
/// `EmojiPicker.tsx` matches names and shorthands case-insensitively; the
/// colons and spaces are forgiven here for typed names).
fn normalize(text: &str) -> String {
    text.trim().trim_matches(':').to_lowercase().replace(' ', "_")
}

/// How well `needle` (already normalized) matches any of the names: 0 exact,
/// 1 prefix, 2 contained, None for no match.
fn match_rank<'a>(names: impl Iterator<Item = &'a str>, needle: &str) -> Option<u8> {
    names
        .filter_map(|n| {
            let n = normalize(n);
            if n == needle {
                Some(0)
            } else if n.starts_with(needle) {
                Some(1)
            } else if n.contains(needle) {
                Some(2)
            } else {
                None
            }
        })
        .min()
}

/// Each row's nearest emoji row above and below (-1 for none), so the arrow
/// keys step over headings (and over empty sections).
fn neighbours(rows: &[Row]) -> Vec<(i32, i32)> {
    let cells: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r, Row::Cells(_)))
        .map(|(i, _)| i)
        .collect();
    let at = |i: usize| i32::try_from(i).unwrap_or(-1);
    (0..rows.len())
        .map(|i| {
            let above = cells.iter().rev().find(|&&c| c < i).map_or(-1, |&c| at(c));
            let below = cells.iter().find(|&&c| c > i).map_or(-1, |&c| at(c));
            (above, below)
        })
        .collect()
}

/// Emoji picked lately, newest first, shown as the picker's first section.
const RECENT_MAX: usize = 2 * COLUMNS;

/// `pick` goes to the front of `recent` (once), keeping at most `RECENT_MAX`.
fn remember(recent: &mut Vec<Pick>, pick: Pick) {
    recent.retain(|p| p.reaction != pick.reaction);
    recent.insert(0, pick);
    recent.truncate(RECENT_MAX);
}

/// A category's heading (Yapfast's names for the Unicode groups).
fn group_title(group: emojis::Group) -> &'static str {
    match group {
        emojis::Group::SmileysAndEmotion => "Smileys & emotion",
        emojis::Group::PeopleAndBody => "People & body",
        emojis::Group::AnimalsAndNature => "Animals & nature",
        emojis::Group::FoodAndDrink => "Food & drink",
        emojis::Group::TravelAndPlaces => "Travel & places",
        emojis::Group::Activities => "Activities",
        emojis::Group::Objects => "Objects",
        emojis::Group::Symbols => "Symbols",
        emojis::Group::Flags => "Flags",
    }
}

/// The picker's model and what's loaded in it.
#[derive(Default)]
pub struct Picker {
    pub rows: Vec<Row>,
    pub model: std::rc::Rc<VecModel<EmojiRowItem>>,
    /// Rows asked for (on screen or just ahead).
    wanted: HashSet<usize>,
    /// Rows whose pictures are all in.
    complete: HashSet<usize>,
    /// Each row's section: the row of the heading above it.
    sections: Vec<usize>,
    /// Each row's nearest emoji row above and below.
    around: Vec<(i32, i32)>,
    /// Picked this session (not kept across restarts), newest first.
    recent: Vec<Pick>,
}

impl Picker {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            model: std::rc::Rc::default(),
            wanted: HashSet::new(),
            complete: HashSet::new(),
            sections: Vec::new(),
            around: Vec::new(),
            recent: Vec::new(),
        }
    }

    /// `rows` with the recently used emoji in front (no search active).
    pub fn with_recent(&self, mut rows: Vec<Row>, query: &str) -> Vec<Row> {
        if query.trim().is_empty() && !self.recent.is_empty() {
            let mut front = vec![Row::Header("Recently used".into())];
            chunked(&mut front, self.recent.clone());
            front.append(&mut rows);
            return front;
        }
        rows
    }

    /// `pick` was chosen: it leads the recently used.
    pub fn remember(&mut self, pick: Pick) {
        remember(&mut self.recent, pick);
    }

    /// New contents (opened, or the search changed).
    pub fn set(&mut self, rows: Vec<Row>, images: &mut Images) {
        self.rows = rows;
        self.wanted.clear();
        self.complete.clear();
        let mut heading = 0;
        self.sections = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                if matches!(r, Row::Header(_)) {
                    heading = i;
                }
                heading
            })
            .collect();
        let items: Vec<EmojiRowItem> = self
            .rows
            .iter()
            .zip(&self.sections)
            .enumerate()
            .map(|(i, (r, s))| item(r, None, *s, self.around_of(i)))
            .collect();
        self.around = neighbours(&self.rows);
        self.model.set_vec(items);
        // The category bar's icons are each section's first emoji: load
        // those rows now, before the screen's (asked last, so loaded first).
        let firsts: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r, Row::Header(_)))
            .map(|(i, _)| i + 1)
            .collect();
        for i in firsts {
            if self.wanted.insert(i) {
                self.fill(i, images);
            }
        }
        self.show(0, images);
    }

    fn around_of(&self, i: usize) -> (i32, i32) {
        self.around.get(i).copied().unwrap_or((-1, -1))
    }

    /// The view is scrolled so `first` is the top row.
    pub fn show(&mut self, first: usize, images: &mut Images) {
        let end = (first + VISIBLE + AHEAD).min(self.rows.len());
        let start = first.saturating_sub(BEHIND).min(end);
        // The loader takes the newest request first, so the rows on screen
        // are asked for last: padding first, from the farthest in.
        let on_screen = first.min(end)..(first + VISIBLE).min(end);
        let padding = (on_screen.end..end).rev().chain(start..on_screen.start);
        for i in padding.chain(on_screen.rev()) {
            if self.wanted.insert(i) {
                self.fill(i, images);
            }
        }
    }

    /// Pictures arrived: fill the rows still missing some.
    pub fn refresh(&mut self, images: &mut Images) {
        let pending: Vec<usize> = self.wanted.difference(&self.complete).copied().collect();
        for i in pending {
            self.fill(i, images);
        }
    }

    fn fill(&mut self, i: usize, images: &mut Images) {
        let Some(row) = self.rows.get(i) else { return };
        let section = self.sections.get(i).copied().unwrap_or(0);
        let around = self.around_of(i);
        let (it, complete) = match row {
            Row::Header(_) => (item(row, None, section, around), true),
            Row::Cells(cells) => {
                let pics: Vec<Option<Image>> = cells.iter().map(|c| images.get(&c.url, PICKER_EMOJI)).collect();
                let complete = pics.iter().all(Option::is_some);
                (item(row, Some(pics), section, around), complete)
            }
        };
        if complete {
            self.complete.insert(i);
        }
        if i < self.model.row_count() {
            self.model.set_row_data(i, it);
        }
    }

    pub fn at(&self, row: usize, col: usize) -> Option<&Pick> {
        match self.rows.get(row)? {
            Row::Cells(cells) => cells.get(col),
            Row::Header(_) => None,
        }
    }
}

fn item(row: &Row, pics: Option<Vec<Option<Image>>>, section: usize, around: (i32, i32)) -> EmojiRowItem {
    let section = i32::try_from(section).unwrap_or(0);
    match row {
        Row::Header(t) => EmojiRowItem {
            header: true,
            title: t.as_ref().into(),
            cells: ModelRc::default(),
            section,
            above: around.0,
            below: around.1,
        },
        Row::Cells(cells) => {
            let pics = pics.unwrap_or_default();
            let cells: Vec<EmojiCellItem> = cells
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let pic = pics.get(i).cloned().flatten();
                    EmojiCellItem {
                        name: format!(":{}:", c.name).into(),
                        glyph: c.glyph.as_ref().into(),
                        has_image: pic.is_some(),
                        image: pic.unwrap_or_else(crate::view::no_image),
                        anim: -1,
                    }
                })
                .collect();
            EmojiRowItem {
                header: false,
                title: "".into(),
                cells: ModelRc::new(VecModel::from(cells)),
                section,
                above: around.0,
                below: around.1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_hold_nine_and_search_matches_shortcodes() {
        let all = rows(&[], "", Allowed::All);
        // By category, Smileys first.
        assert!(matches!(&all[0], Row::Header(t) if &**t == "Smileys & emotion"));
        let headers = all.iter().filter(|r| matches!(r, Row::Header(_))).count();
        assert_eq!(headers, 9);
        assert!(
            all.iter()
                .all(|r| matches!(r, Row::Header(_)) || matches!(r, Row::Cells(c) if c.len() <= COLUMNS))
        );
        // 🫫 (Emoji 18.0) has no picture in any pack yet: not offered.
        let offered = |e: &str| {
            all.iter()
                .any(|r| matches!(r, Row::Cells(c) if c.iter().any(|p| &*p.reaction == e)))
        };
        assert!(offered("🫠") && !offered("🫫"));
        let found = rows(&[], "thumbsup", Allowed::All);
        let first = found.iter().find_map(|r| match r {
            Row::Cells(c) => c.first(),
            Row::Header(_) => None,
        });
        assert_eq!(first.map(|p| &*p.reaction), Some("👍"));
        assert!(rows(&[], "no-such-emoji-name", Allowed::All).is_empty());
    }

    fn pick(code: &str) -> Pick {
        Pick {
            name: code.into(),
            glyph: "".into(),
            url: "".into(),
            reaction: code.into(),
            code: code.into(),
        }
    }

    #[test]
    fn search_forgives_case_spaces_and_colons_and_ranks_matches() {
        let n = normalize(" :Thumbs Up: ");
        assert_eq!(n, "thumbs_up");
        assert_eq!(match_rank(["thumbs_up"].into_iter(), &n), Some(0));
        assert_eq!(match_rank(["thumbs_up_sign"].into_iter(), &n), Some(1));
        assert_eq!(match_rank(["big_thumbs_up"].into_iter(), &n), Some(2));
        assert_eq!(match_rank(["smile"].into_iter(), &n), None);
        // The best of several names counts.
        assert_eq!(match_rank(["big_thumbs_up", "thumbs_up"].into_iter(), &n), Some(0));
        // A full emoji name finds it, not only its shorthand.
        let found = rows(&[], "thumbs up", Allowed::All);
        assert!(
            found
                .iter()
                .any(|r| matches!(r, Row::Cells(c) if c.iter().any(|p| &*p.reaction == "👍")))
        );
    }

    #[test]
    fn neighbours_skip_headings() {
        let rows = vec![
            Row::Header("a".into()),
            Row::Cells(vec![pick("1")]),
            Row::Header("b".into()),
            Row::Header("empty".into()),
            Row::Cells(vec![pick("2")]),
        ];
        let n = neighbours(&rows);
        assert_eq!(n[1], (-1, 4));
        assert_eq!(n[4], (1, -1));
        assert_eq!(n[0], (-1, 1));
        assert_eq!(n[3], (1, 4));
    }

    #[test]
    fn recent_emoji_lead_once_and_are_capped() {
        let mut recent = Vec::new();
        remember(&mut recent, pick("a"));
        remember(&mut recent, pick("b"));
        remember(&mut recent, pick("a"));
        let order: Vec<&str> = recent.iter().map(|p| &*p.reaction).collect();
        assert_eq!(order, ["a", "b"]);
        for i in 0..40 {
            remember(&mut recent, pick(&i.to_string()));
        }
        assert_eq!(recent.len(), RECENT_MAX);
        assert_eq!(&*recent[0].reaction, "39");

        let mut picker = Picker::new();
        picker.remember(pick("x"));
        let with = picker.with_recent(Vec::new(), "");
        assert!(matches!(&with[0], Row::Header(t) if &**t == "Recently used"));
        // Not while searching.
        assert!(picker.with_recent(Vec::new(), "x").is_empty());
    }

    #[test]
    fn a_networks_emoji_version_limits_the_set() {
        let count = |version: Option<(u32, u32)>| -> usize {
            rows(&[], "", version.map_or(Allowed::All, |(a, b)| Allowed::UpTo(a, b)))
                .iter()
                .map(|r| match r {
                    Row::Cells(c) => c.len(),
                    Row::Header(_) => 0,
                })
                .sum()
        };
        let all = count(None);
        let old = count(Some((12, 0)));
        assert!(old > 0 && old < all);
        // 🫠 (melting face) is Emoji 14.0: not offered when limited to 12.0.
        let melting = |version: Option<(u32, u32)>| {
            rows(
                &[],
                "melting",
                version.map_or(Allowed::All, |(a, b)| Allowed::UpTo(a, b)),
            )
            .iter()
            .any(|r| matches!(r, Row::Cells(c) if c.iter().any(|p| &*p.reaction == "🫠")))
        };
        assert!(melting(None) && !melting(Some((12, 0))));
    }

    #[test]
    fn a_network_with_its_own_set_offers_only_those() {
        static SET: std::sync::OnceLock<std::collections::HashSet<String>> = std::sync::OnceLock::new();
        let set = SET.get_or_init(|| ["👍", "❤"].iter().map(|e| (*e).to_owned()).collect());
        let cells: Vec<String> = rows(&[], "", Allowed::Only(set))
            .iter()
            .flat_map(|r| match r {
                Row::Cells(c) => c.iter().map(|p| p.reaction.replace('\u{fe0f}', "")).collect(),
                Row::Header(_) => Vec::new(),
            })
            .collect();
        assert_eq!(cells.len(), 2);
        assert!(cells.contains(&"👍".to_owned()) && cells.contains(&"❤".to_owned()));
    }
}
