// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Message text laid out by Godwit itself, with parley (the layout engine
//! Slint's own text uses, with the same fonts and line height): Slint's text
//! elements can't select across messages or put pictures inside a line, and
//! the official client does both (a browser selection; emoji images inline).
//!
//! A message block's rich text becomes a [`Paragraph`] (text, styles, links
//! and emoji); laid out at the timeline's width it gives the runs the UI draws
//! at their positions, and it answers where a point is for selecting.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use parley::{
    Alignment, AlignmentOptions, Cursor, FontContext, FontFamily, FontFamilyName, FontStyle, FontWeight, GenericFamily,
    InlineBox, InlineBoxKind, Layout, LayoutContext, LineHeight, PositionedLayoutItem, Selection, StyleProperty,
};

/// What a stretch of text looks like beyond its font.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Brush {
    /// Index into the paragraph's links (+1; 0 for none).
    pub link: u16,
    pub colour: Option<[u8; 3]>,
    pub strike: bool,
    pub code: bool,
    /// Drawn in the theme's faint colour (a header's time).
    pub faint: bool,
    /// A link drawn in the text's colour, not the link colour (the author).
    pub quiet: bool,
}

/// A styled stretch of text, by byte range.
#[derive(Clone, Debug, Default, PartialEq)]
struct Span {
    range: Range<usize>,
    bold: bool,
    /// Bold at 600, not 700 (a header's name, as Slint draws it).
    semi: bool,
    italic: bool,
    /// A font size of its own, else the block's.
    size: Option<f32>,
    brush: Brush,
}

/// An emoji drawn as a picture inside the line.
#[derive(Clone, Debug, PartialEq)]
pub struct Emoji {
    /// Where it sits in the paragraph's text (its characters aren't there).
    pub index: usize,
    /// What it copies as (the emoji, or `:name:`).
    pub text: Arc<str>,
    pub url: Option<Arc<str>>,
}

/// One block's text, ready to lay out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Paragraph {
    pub text: String,
    spans: Vec<Span>,
    pub links: Vec<Arc<str>>,
    pub emoji: Vec<Emoji>,
    /// A header's gap between author and time (its byte offset): it copies
    /// as " — ".
    pub gap: Option<usize>,
}

/// The link a header's author name carries: their profile.
pub const AUTHOR_LINK: &str = "godwit://author";
/// Between a header's author and time: an en space, about the official
/// client's 8 px at 14 px (and never in a name in practice).
const GAP: &str = "\u{2002}";

/// The block's base look.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Base {
    pub size: f32,
    pub bold: bool,
    pub mono: bool,
}

/// Emoji in text are this much larger than the font, as officially (1.375em).
const EMOJI_EM: f32 = 1.375;

impl Paragraph {
    /// Plain text, as is.
    pub fn plain(text: &str, emoji_url: &dyn Fn(&str) -> Option<String>) -> Self {
        let mut p = Self::default();
        p.push_text(text, Span::default(), emoji_url);
        p
    }

    /// A message group's header: the author's name (a link to their
    /// profile, in their role colour) and the time, smaller and faint, as
    /// the official client; one line of text so it selects with the rest.
    /// Emoji in the name are the pack's pictures, as in message text.
    pub fn header(
        author: &str,
        colour: Option<[u8; 3]>,
        time: &str,
        emoji_url: &dyn Fn(&str) -> Option<String>,
    ) -> Self {
        let mut p = Self {
            links: vec![AUTHOR_LINK.into()],
            ..Self::default()
        };
        p.push_text(
            author,
            Span {
                bold: true,
                semi: true,
                brush: Brush {
                    link: 1,
                    colour,
                    quiet: true,
                    ..Brush::default()
                },
                ..Span::default()
            },
            emoji_url,
        );
        p.gap = Some(p.text.len());
        p.text.push_str(GAP);
        p.push_span(
            time,
            Span {
                size: Some(11.0),
                brush: Brush {
                    faint: true,
                    ..Brush::default()
                },
                ..Span::default()
            },
        );
        p
    }

    /// Godwit rich text (see `Markup::Rich`), plus the code highlighter's
    /// `<font color>` spans.
    pub fn rich(rich: &str, emoji_url: &dyn Fn(&str) -> Option<String>) -> Self {
        use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
        let mut p = Self::default();
        let mut style = Span::default();
        let mut stack: Vec<Span> = Vec::new();
        let mut lists: Vec<Option<u64>> = Vec::new();
        let mut link_start: Vec<(usize, Arc<str>)> = Vec::new();
        // The custom emoji link being read already has its picture: its name
        // can come in several text events (`\:name\:` is split at escapes).
        let mut emoji_placed = false;
        for event in Parser::new_ext(rich, Options::ENABLE_STRIKETHROUGH) {
            match event {
                Event::Start(Tag::Paragraph) => {
                    if !p.text.is_empty() && !p.text.ends_with('\n') {
                        p.text.push('\n');
                    }
                }
                Event::Start(Tag::List(start)) => {
                    if !p.text.is_empty() && !p.text.ends_with('\n') {
                        p.text.push('\n');
                    }
                    lists.push(start);
                }
                Event::End(TagEnd::List(_)) => {
                    lists.pop();
                }
                Event::Start(Tag::Item) => {
                    if !p.text.is_empty() && !p.text.ends_with('\n') {
                        p.text.push('\n');
                    }
                    let depth = lists.len().saturating_sub(1);
                    p.text.push_str(&"    ".repeat(depth));
                    match lists.last_mut() {
                        Some(Some(n)) => {
                            p.text.push_str(&format!("{n}. "));
                            *n += 1;
                        }
                        _ => p.text.push_str("• "),
                    }
                }
                Event::Start(Tag::Emphasis) => {
                    stack.push(style.clone());
                    style.italic = true;
                }
                Event::Start(Tag::Strong) => {
                    stack.push(style.clone());
                    style.bold = true;
                }
                Event::Start(Tag::Strikethrough) => {
                    stack.push(style.clone());
                    style.brush.strike = true;
                }
                Event::Start(Tag::Link { dest_url, .. }) => {
                    stack.push(style.clone());
                    let url: Arc<str> = dest_url.as_ref().into();
                    // A custom emoji: its picture, inline.
                    if let Some(image) = url.strip_prefix(godwit_core::rich::EMOJI) {
                        link_start.push((usize::MAX, percent_decode(image).into()));
                        emoji_placed = false;
                        continue;
                    }
                    p.links.push(url.clone());
                    style.brush.link = u16::try_from(p.links.len()).unwrap_or(0);
                    link_start.push((p.text.len(), url));
                }
                Event::End(TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough) => {
                    style = stack.pop().unwrap_or_default();
                }
                Event::End(TagEnd::Link) => {
                    style = stack.pop().unwrap_or_default();
                    link_start.pop();
                }
                Event::Text(t) => match link_start.last() {
                    // The custom emoji's name: what it copies as.
                    Some((usize::MAX, image)) => match p.emoji.last_mut().filter(|_| emoji_placed) {
                        Some(e) => e.text = format!("{}{t}", e.text).into(),
                        None => {
                            let image = image.clone();
                            p.emoji.push(Emoji {
                                index: p.text.len(),
                                text: t.as_ref().into(),
                                url: Some(image),
                            });
                            emoji_placed = true;
                        }
                    },
                    _ => p.push_text(&t, style.clone(), emoji_url),
                },
                Event::Code(t) => {
                    let mut code = style.clone();
                    code.brush.code = true;
                    p.push_span(&t, code);
                }
                Event::SoftBreak => p.push_span(" ", style.clone()),
                Event::HardBreak => p.text.push('\n'),
                Event::InlineHtml(html) | Event::Html(html) => {
                    let html = html.trim();
                    if html == "</font>" {
                        style = stack.pop().unwrap_or_default();
                    } else if let Some(colour) = html
                        .strip_prefix("<font color=\"#")
                        .and_then(|rest| rest.strip_suffix("\">"))
                        .and_then(parse_hex)
                    {
                        stack.push(style.clone());
                        style.brush.colour = Some(colour);
                    }
                }
                _ => {}
            }
        }
        while p.text.ends_with('\n') {
            p.text.pop();
        }
        p
    }

    /// Text in `style`, with its emoji as pictures where there's one for them.
    fn push_text(&mut self, text: &str, style: Span, emoji_url: &dyn Fn(&str) -> Option<String>) {
        use unicode_segmentation::UnicodeSegmentation;
        let mut plain = String::new();
        for g in text.graphemes(true) {
            // The shared lookup (crate::emoji), not inside code.
            let canonical = (!style.brush.code).then(|| crate::emoji::canonical(g)).flatten();
            let url = canonical.and_then(|e| emoji_url(e.as_str()).or_else(|| emoji_url(g)));
            match url {
                Some(url) => {
                    self.push_span(&std::mem::take(&mut plain), style.clone());
                    self.emoji.push(Emoji {
                        index: self.text.len(),
                        text: g.into(),
                        url: Some(url.into()),
                    });
                }
                None => plain.push_str(g),
            }
        }
        self.push_span(&plain, style);
    }

    fn push_span(&mut self, text: &str, style: Span) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        if style.bold || style.italic || style.size.is_some() || style.brush != Brush::default() {
            self.spans.push(Span {
                range: start..self.text.len(),
                ..style
            });
        }
    }

    /// The text of `range` (byte offsets into the laid-out text), with the
    /// emoji inside it, as it's copied.
    pub fn copy(&self, range: Range<usize>) -> String {
        if let Some(gap) = self.gap.filter(|g| range.start <= *g && *g + GAP.len() <= range.end) {
            let mut out = self.copy(range.start..gap);
            out.push_str(" — ");
            out.push_str(&self.copy(gap + GAP.len()..range.end));
            return out;
        }
        let mut out = String::new();
        let mut at = range.start;
        for e in self.emoji.iter().filter(|e| {
            range.contains(&e.index) || e.index == range.end && range.end > range.start && e.index == self.text.len()
        }) {
            if let Some(t) = self.text.get(at..e.index) {
                out.push_str(t);
            }
            out.push_str(&e.text);
            at = e.index;
        }
        if let Some(t) = self.text.get(at..range.end) {
            out.push_str(t);
        }
        out
    }
}

fn parse_hex(hex: &str) -> Option<[u8; 3]> {
    let v = u32::from_str_radix(hex, 16).ok().filter(|_| hex.len() == 6)?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(v) = text.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A paragraph laid out at a width.
pub struct Laid {
    pub paragraph: Paragraph,
    pub base: Base,
    pub layout: Layout<Brush>,
}

/// A piece of a laid-out paragraph to draw.
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    Text {
        x: f32,
        /// Top of the run's own line box (where a Slint Text of this style
        /// puts its baseline at the paragraph's).
        y: f32,
        width: f32,
        text: String,
        /// The run's font size.
        size: f32,
        bold: bool,
        italic: bool,
        brush: Brush,
    },
    Emoji {
        x: f32,
        y: f32,
        size: f32,
        index: usize,
    },
}

struct Fonts {
    fonts: FontContext,
    layouts: LayoutContext<Brush>,
    /// Inter's line height per font size, as Slint uses for every run.
    ratio: f32,
}

thread_local! {
    static FONTS: RefCell<Option<Fonts>> = const { RefCell::new(None) };
}

fn with_fonts<T>(f: impl FnOnce(&mut Fonts) -> T) -> T {
    FONTS.with(|cell| {
        let mut cell = cell.borrow_mut();
        let fonts = cell.get_or_insert_with(|| {
            let mut fonts = FontContext::new();
            // The fonts Slint has: the system's and the bundled Inter.
            for data in [
                &include_bytes!("../../assets/fonts/Inter-Regular.ttf")[..],
                &include_bytes!("../../assets/fonts/Inter-SemiBold.ttf")[..],
            ] {
                fonts
                    .collection
                    .register_fonts(parley::fontique::Blob::new(Arc::new(data)), None);
            }
            let mut f = Fonts {
                fonts,
                layouts: LayoutContext::new(),
                ratio: 1.2,
            };
            f.ratio = natural_ratio(&mut f).unwrap_or(1.2);
            f
        });
        f(fonts)
    })
}

const INTER: &[FontFamilyName<'static>] = &[
    FontFamilyName::Named(std::borrow::Cow::Borrowed("Inter")),
    FontFamilyName::Generic(GenericFamily::SansSerif),
    FontFamilyName::Generic(GenericFamily::SystemUi),
];
const MONO: &[FontFamilyName<'static>] = &[
    FontFamilyName::Named(std::borrow::Cow::Borrowed("monospace")),
    FontFamilyName::Generic(GenericFamily::SansSerif),
    FontFamilyName::Generic(GenericFamily::SystemUi),
];

/// Inter's natural line height over its size (Slint's `line_height_ratio`).
fn natural_ratio(f: &mut Fonts) -> Option<f32> {
    let mut b = f.layouts.ranged_builder(&mut f.fonts, "x", 1.0, false);
    b.push_default(FontFamily::List(std::borrow::Cow::Borrowed(INTER)));
    b.push_default(StyleProperty::FontSize(100.0));
    let mut layout: Layout<Brush> = b.build("x");
    layout.break_all_lines(None);
    let line = layout.lines().next()?;
    Some(line.metrics().line_height / 100.0)
}

/// Lays `paragraph` out within `width`.
pub fn lay_out(paragraph: Paragraph, base: Base, width: f32) -> Laid {
    let layout = with_fonts(|f| {
        let ratio = f.ratio;
        let text = paragraph.text.as_str();
        let mut b = f.layouts.ranged_builder(&mut f.fonts, text, 1.0, false);
        b.push_default(FontFamily::List(std::borrow::Cow::Borrowed(if base.mono {
            MONO
        } else {
            INTER
        })));
        b.push_default(StyleProperty::FontSize(base.size));
        b.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(ratio)));
        b.push_default(StyleProperty::OverflowWrap(parley::OverflowWrap::Anywhere));
        if base.bold {
            b.push_default(StyleProperty::FontWeight(FontWeight::BOLD));
        }
        for span in &paragraph.spans {
            if span.bold {
                let weight = if span.semi {
                    FontWeight::SEMI_BOLD
                } else {
                    FontWeight::BOLD
                };
                b.push(StyleProperty::FontWeight(weight), span.range.clone());
            }
            if span.italic {
                b.push(StyleProperty::FontStyle(FontStyle::Italic), span.range.clone());
            }
            if let Some(size) = span.size {
                b.push(StyleProperty::FontSize(size), span.range.clone());
            }
            if span.brush.code {
                b.push(FontFamily::List(std::borrow::Cow::Borrowed(MONO)), span.range.clone());
            }
            b.push(StyleProperty::Brush(span.brush), span.range.clone());
        }
        let size = (base.size * EMOJI_EM).round();
        for (i, e) in paragraph.emoji.iter().enumerate() {
            b.push_inline_box(InlineBox {
                id: i as u64,
                kind: InlineBoxKind::InFlow,
                index: e.index,
                width: size,
                height: size,
            });
        }
        let mut layout = b.build(text);
        layout.break_all_lines(Some(width.max(1.0)));
        layout.align(Alignment::Start, AlignmentOptions::default());
        layout
    });
    Laid {
        paragraph,
        base,
        layout,
    }
}

impl Laid {
    pub fn height(&self) -> f32 {
        self.layout.height()
    }

    /// What to draw, by line.
    pub fn pieces(&self) -> Vec<Piece> {
        let mut out = Vec::new();
        // Glyph runs of one font run come in order: count their glyphs.
        let mut last_run: Option<Range<usize>> = None;
        let mut glyphs_before = 0usize;
        for line in self.layout.lines() {
            for item in line.items() {
                match item {
                    PositionedLayoutItem::GlyphRun(run) => {
                        let style = run.style();
                        // This glyph run's own text: parley keeps one font run
                        // for text that only differs by brush (a link, a
                        // colour), so the run's range is all of it, and every
                        // piece drew the whole line.
                        let whole = run.run().text_range();
                        if last_run.as_ref() != Some(&whole) {
                            last_run = Some(whole);
                            glyphs_before = 0;
                        }
                        let count = run.glyphs().count();
                        let range = glyph_text(run.run(), glyphs_before, count);
                        glyphs_before += count;
                        let Some(text) = self.paragraph.text.get(range.clone()) else {
                            continue;
                        };
                        let text = text.trim_end_matches('\n');
                        if text.trim().is_empty() {
                            continue;
                        }
                        let bold = self
                            .paragraph
                            .spans
                            .iter()
                            .any(|s| s.bold && s.range.start <= range.start && range.start < s.range.end)
                            || self.base.bold;
                        let italic = self
                            .paragraph
                            .spans
                            .iter()
                            .any(|s| s.italic && s.range.start <= range.start && range.start < s.range.end);
                        let mono = self.base.mono || style.brush.code;
                        let size = run.run().font_size();
                        let tops = baseline_offsets(Base { size, ..self.base });
                        let offset = if mono { tops.1 } else { tops.0 };
                        out.push(Piece::Text {
                            x: run.offset(),
                            y: run.baseline() - offset,
                            width: run.advance(),
                            text: text.to_owned(),
                            size,
                            bold,
                            italic,
                            brush: style.brush,
                        });
                    }
                    PositionedLayoutItem::InlineBox(b) => out.push(Piece::Emoji {
                        x: b.x,
                        y: b.y,
                        size: b.width,
                        index: b.id as usize,
                    }),
                }
            }
        }
        out
    }

    /// The byte offset nearest a point.
    pub fn index_at(&self, x: f32, y: f32) -> usize {
        Cursor::from_point(&self.layout, x, y).index()
    }

    /// The word (or emoji) at a point.
    pub fn word_at(&self, x: f32, y: f32) -> Range<usize> {
        Selection::word_from_point(&self.layout, x, y).text_range()
    }

    /// Rectangles covering `range`: (x, y, width, height).
    pub fn rects(&self, range: Range<usize>) -> Vec<(f32, f32, f32, f32)> {
        if range.is_empty() {
            return Vec::new();
        }
        let anchor = Cursor::from_byte_index(&self.layout, range.start, parley::Affinity::Downstream);
        let focus = Cursor::from_byte_index(&self.layout, range.end, parley::Affinity::Upstream);
        let mut out: Vec<(f32, f32, f32, f32)> = Selection::new(anchor, focus)
            .geometry(&self.layout)
            .into_iter()
            .map(|(b, _)| (b.x0 as f32, b.y0 as f32, (b.x1 - b.x0) as f32, (b.y1 - b.y0) as f32))
            .collect();
        // Emoji pictures inside the range are selected too.
        for line in self.layout.lines() {
            for item in line.items() {
                if let PositionedLayoutItem::InlineBox(b) = item
                    && let Some(e) = self.paragraph.emoji.get(b.id as usize)
                    && range.start <= e.index
                    && e.index < range.end.max(range.start + 1)
                {
                    out.push((b.x, b.y, b.width, b.height));
                }
            }
        }
        out
    }

    /// The link at a point, if any.
    pub fn link_at(&self, x: f32, y: f32) -> Option<Arc<str>> {
        for line in self.layout.lines() {
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(run) = item {
                    let m = line.metrics();
                    let inside = x >= run.offset()
                        && x < run.offset() + run.advance()
                        && y >= m.block_min_coord
                        && y < m.block_max_coord;
                    let link = run.style().brush.link;
                    if inside && link > 0 {
                        return self.paragraph.links.get(usize::from(link) - 1).cloned();
                    }
                }
            }
        }
        None
    }
}

// Each message's laid-out blocks, kept for selecting (timeline rows are
// rebuilt from scratch, so this is the one copy).
thread_local! {
    static KEPT: RefCell<HashMap<godwit_core::ids::MessageId, std::rc::Rc<Vec<Laid>>>> =
        RefCell::new(HashMap::new());
}

/// Rooms' worth of messages; past this the store starts over (rows rebuilt
/// later lay out again).
const KEEP: usize = 4000;

pub fn remember(id: &godwit_core::ids::MessageId, blocks: Vec<Laid>) {
    KEPT.with(|k| {
        let mut k = k.borrow_mut();
        if k.len() >= KEEP && !k.contains_key(id) {
            k.clear();
        }
        k.insert(id.clone(), std::rc::Rc::new(blocks));
    });
}

/// One block of a message's kept layouts.
pub struct LaidRef {
    blocks: std::rc::Rc<Vec<Laid>>,
    index: usize,
}

impl LaidRef {
    pub fn new(blocks: std::rc::Rc<Vec<Laid>>, index: usize) -> Option<Self> {
        (index < blocks.len()).then_some(Self { blocks, index })
    }
}

impl std::ops::Deref for LaidRef {
    type Target = Laid;
    fn deref(&self) -> &Laid {
        // `new` checked the index.
        &self.blocks[self.index]
    }
}

pub fn forget(id: &godwit_core::ids::MessageId) {
    KEPT.with(|k| k.borrow_mut().remove(id));
}

/// A message's laid-out blocks, if its row was built.
pub fn blocks(id: &godwit_core::ids::MessageId) -> Option<std::rc::Rc<Vec<Laid>>> {
    KEPT.with(|k| k.borrow().get(id).cloned())
}

/// The text of `count` glyphs from glyph `start` of `run` (in visual order,
/// as parley's glyph runs take them): the clusters they belong to.
fn glyph_text(run: &parley::Run<'_, Brush>, start: usize, count: usize) -> Range<usize> {
    let end = start + count;
    let (mut lo, mut hi) = (usize::MAX, 0);
    let mut seen = 0;
    for cluster in run.visual_clusters() {
        let n = cluster.glyphs().count();
        let (a, b) = (seen, seen + n);
        seen = b;
        // A cluster without glyphs (part of a ligature) belongs where it falls.
        let inside = if n == 0 {
            a > start && a <= end
        } else {
            b > start && a < end
        };
        if inside {
            let r = cluster.text_range();
            lo = lo.min(r.start);
            hi = hi.max(r.end);
        }
        if seen >= end && n > 0 {
            break;
        }
    }
    if lo > hi { 0..0 } else { lo..hi }
}

/// Where a Slint Text of the block's style puts its first baseline: the run
/// is drawn at its paragraph baseline minus this (normal, monospace).
fn baseline_offsets(base: Base) -> (f32, f32) {
    thread_local! {
        static DONE: RefCell<HashMap<(u32, bool), (f32, f32)>> = RefCell::new(HashMap::new());
    }
    let key = (base.size.to_bits(), base.bold);
    if let Some(hit) = DONE.with(|d| d.borrow().get(&key).copied()) {
        return hit;
    }
    let one = |mono: bool| {
        let laid = lay_out(
            Paragraph {
                text: "x".into(),
                ..Paragraph::default()
            },
            Base { mono, ..base },
            f32::MAX,
        );
        laid.layout.lines().next().map_or(base.size, |l| l.metrics().baseline)
    };
    let out = (one(false), one(true));
    DONE.with(|d| d.borrow_mut().insert(key, out));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn rich_text_keeps_its_styles_links_and_lists() {
        let p = Paragraph::rich("**bold** *it* [site](https://x.example) `c`\\\nnext", &none);
        assert_eq!(p.text, "bold it site c\nnext");
        assert_eq!(p.links.len(), 1);
        assert!(p.spans.iter().any(|s| s.bold && &p.text[s.range.clone()] == "bold"));
        assert!(p.spans.iter().any(|s| s.brush.code && &p.text[s.range.clone()] == "c"));
        let list = Paragraph::rich("- one\n- two", &none);
        assert_eq!(list.text, "• one\n• two");
    }

    #[test]
    fn emoji_become_pictures_that_copy_as_themselves() {
        let url = |e: &str| Some(format!("pack/{e}"));
        let p = Paragraph::plain("hi 👌 there", &url);
        assert_eq!(p.text, "hi  there");
        assert_eq!(p.emoji.len(), 1);
        assert_eq!(p.copy(0..p.text.len()), "hi 👌 there");
        assert_eq!(p.copy(0..2), "hi");
    }

    #[test]
    fn a_header_is_one_line_that_copies_with_a_dash() {
        let p = Paragraph::header("Ann", Some([1, 2, 3]), "Today at 12:00", &|_| None);
        assert_eq!(p.copy(0..p.text.len()), "Ann — Today at 12:00");
        assert_eq!(p.copy(0..3), "Ann");
        let laid = lay_out(
            p,
            Base {
                size: 14.0,
                bold: false,
                mono: false,
            },
            400.0,
        );
        let sizes: Vec<f32> = laid
            .pieces()
            .iter()
            .filter_map(|p| match p {
                Piece::Text { size, .. } => Some(*size),
                Piece::Emoji { .. } => None,
            })
            .collect();
        assert_eq!(sizes, [14.0, 11.0], "the name, then the smaller time");
        assert_eq!(laid.link_at(2.0, 8.0).as_deref(), Some(AUTHOR_LINK));
        assert_eq!(laid.layout.lines().count(), 1);
    }

    #[test]
    fn styled_stretches_draw_their_own_text_once() {
        // A link (same font, another brush) splits the line into pieces;
        // each must be its own words, not the whole line again.
        let p = Paragraph::rich("found the [store page](https://example.com/) btw", &none);
        let laid = lay_out(
            p,
            Base {
                size: 14.0,
                bold: false,
                mono: false,
            },
            600.0,
        );
        let texts: Vec<String> = laid
            .pieces()
            .into_iter()
            .filter_map(|p| match p {
                Piece::Text { text, .. } => Some(text),
                Piece::Emoji { .. } => None,
            })
            .collect();
        assert_eq!(texts.concat(), "found the store page btw");
        assert!(texts.contains(&"store page".to_owned()), "{texts:?}");
    }

    #[test]
    fn digits_stay_text_and_keycaps_are_pictures() {
        let url = |e: &str| Some(format!("https://emoji.example/{e}"));
        assert!(
            Paragraph::plain("Player4 #1*", &url).emoji.is_empty(),
            "digits and symbols stay text"
        );
        assert_eq!(
            Paragraph::plain("4\u{fe0f}\u{20e3}", &url).emoji.len(),
            1,
            "a real keycap is an emoji"
        );
    }

    #[test]
    fn a_custom_emoji_is_one_picture_whatever_its_name_holds() {
        // `rich::emoji` escapes the name's colons, which splits its text.
        let src = format!(
            "hey {} you",
            godwit_core::rich::emoji("steamhappy", "https://example.com/e.png")
        );
        let p = Paragraph::rich(&src, &none);
        assert_eq!(p.emoji.len(), 1, "{:?}", p.emoji);
        assert_eq!(&*p.emoji[0].text, ":steamhappy:");
        assert_eq!(p.copy(0..p.text.len()), "hey :steamhappy: you");
    }

    #[test]
    fn highlighted_code_keeps_its_colours() {
        let p = Paragraph::rich("<font color=\"#ff7b72\">fn</font> main", &none);
        assert_eq!(p.text, "fn main");
        assert_eq!(p.spans[0].brush.colour, Some([0xff, 0x7b, 0x72]));
    }

    #[test]
    fn layout_wraps_and_finds_points() {
        let p = Paragraph::plain("one two three four five six seven eight nine ten", &none);
        let base = Base {
            size: 14.0,
            bold: false,
            mono: false,
        };
        let wide = lay_out(p.clone(), base, 2000.0);
        let narrow = lay_out(p, base, 80.0);
        assert!(narrow.height() > wide.height() * 2.0, "wraps");
        assert_eq!(wide.index_at(0.0, 1.0), 0);
        assert_eq!(wide.word_at(1.0, 1.0), 0..3);
        assert!(!wide.rects(0..3).is_empty());
        assert!(wide.pieces().iter().all(|p| matches!(p, Piece::Text { .. })));
    }
}
