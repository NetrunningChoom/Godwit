// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Code block highlighting in the official client's look: its highlight.js
//! GitHub-dark colours (ui/styles.css) mapped onto syntect's scopes, ported
//! from Godwit. The result is rich text with `<font color>` runs for Slint's
//! StyledText. The block is dark in both themes, as officially.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::str::FromStr;
use std::sync::OnceLock;

use godwit_core::rich::escape;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Color, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// The official block text colour (`.hljs`).
const TEXT: u32 = 0xc9d1d9;
/// Highlighted blocks kept; the cache is simply cleared when it's full.
const CACHE: usize = 256;

fn rgb(hex: u32) -> Color {
    let [_, r, g, b] = hex.to_be_bytes();
    Color { r, g, b, a: 0xff }
}

/// The official hljs colours, by the syntect scopes that mean the same thing.
fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        let rules: [(&str, u32); 10] = [
            // .hljs-keyword, -type, -doctag, -template-*
            ("keyword, storage, storage.type, support.type, entity.name.type, variable.language", 0xff7b72),
            // .hljs-title, .function_, .class_
            ("entity.name.function, entity.name.class, entity.other.inherited-class, support.function", 0xd2a8ff),
            // .hljs-number, -literal, -attr, -variable, -operator, -meta
            (
                "constant, constant.numeric, constant.language, variable, entity.other.attribute-name, keyword.operator, meta.preprocessor",
                0x79c0ff,
            ),
            // .hljs-string, -regexp
            ("string, string.regexp, punctuation.definition.string", 0xa5d6ff),
            // .hljs-built_in, -symbol
            ("support.constant, support.class, constant.other.symbol", 0xffa657),
            // .hljs-comment
            ("comment, punctuation.definition.comment", 0x8b949e),
            // .hljs-name, -selector-tag, -quote
            ("entity.name.tag, markup.quote, entity.name.selector", 0x7ee787),
            // .hljs-section
            ("markup.heading, entity.name.section", 0x1f6feb),
            // .hljs-bullet
            ("markup.list punctuation.definition.list", 0xf2cc60),
            ("markup.inserted", 0xaff5b4),
        ];
        let mut scopes: Vec<ThemeItem> = rules
            .iter()
            .filter_map(|(sel, hex)| {
                Some(ThemeItem {
                    scope: ScopeSelectors::from_str(sel).ok()?,
                    style: StyleModifier { foreground: Some(rgb(*hex)), background: None, font_style: Some(FontStyle::empty()) },
                })
            })
            .collect();
        if let Ok(scope) = ScopeSelectors::from_str("markup.deleted") {
            scopes.push(ThemeItem {
                scope,
                style: StyleModifier { foreground: Some(rgb(0xffdcd7)), background: None, font_style: None },
            });
        }
        Theme {
            name: Some("Stoat (highlight.js GitHub dark)".into()),
            author: None,
            settings: ThemeSettings { foreground: Some(rgb(TEXT)), background: Some(rgb(0x0d1117)), ..Default::default() },
            scopes,
        }
    })
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// `piece` as rich text that keeps its spacing: spaces become no-break
/// spaces (rich text would drop leading ones) and tabs four of them.
fn literal(piece: &str) -> String {
    escape(&piece.replace('\t', "    ").replace(' ', "\u{a0}"))
}

/// The code as rich text, highlighted for `lang` (a name or file extension),
/// or plain if the language is unknown. Cached by content.
pub fn highlight(text: &str, lang: &str) -> String {
    thread_local! {
        static DONE: RefCell<HashMap<(String, String), String>> = RefCell::new(HashMap::new());
    }
    let key = (text.to_owned(), lang.to_owned());
    if let Some(hit) = DONE.with(|d| d.borrow().get(&key).cloned()) {
        return hit;
    }
    let out = highlight_uncached(text, lang);
    DONE.with(|d| {
        let mut d = d.borrow_mut();
        if d.len() >= CACHE {
            d.clear();
        }
        d.insert(key, out.clone());
    });
    out
}

fn highlight_uncached(text: &str, lang: &str) -> String {
    let set = syntaxes();
    let lang = lang.trim().to_lowercase();
    let syntax = (!lang.is_empty())
        .then(|| {
            set.find_syntax_by_token(&lang)
                .or_else(|| set.find_syntax_by_extension(&lang))
        })
        .flatten();
    let Some(syntax) = syntax else {
        return literal(text);
    };
    let mut out = String::new();
    let mut h = HighlightLines::new(syntax, theme());
    for line in LinesWithEndings::from(text) {
        let Ok(spans) = h.highlight_line(line, set) else {
            out.push_str(&literal(line));
            continue;
        };
        for (style, piece) in spans {
            let c = style.foreground;
            let body = literal(piece);
            if body.trim().is_empty() || (c.r, c.g, c.b) == (0xc9, 0xd1, 0xd9) {
                out.push_str(&body);
            } else {
                let _ = write!(out, "<font color=\"#{:02x}{:02x}{:02x}\">{body}</font>", c.r, c.g, c.b);
            }
        }
    }
    out.trim_end_matches("\\\n").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_get_the_official_colours() {
        let out = highlight("fn main() { let x = \"hi\"; }", "rust");
        assert!(out.contains("<font color=\"#ff7b72\">fn</font>"), "{out}");
        assert!(out.contains("#a5d6ff"), "strings are blue: {out}");
        assert_eq!(
            highlight("a  b", "nonsense-lang"),
            "a\u{a0}\u{a0}b",
            "unknown languages stay plain"
        );
    }

    #[test]
    fn indentation_and_lines_survive() {
        let out = highlight("if x:\n    y()\n", "py");
        assert!(out.contains("\\\n\u{a0}\u{a0}\u{a0}\u{a0}"), "{out:?}");
        assert!(
            slint::StyledText::from_markdown(&out).is_ok(),
            "parses as rich text: {out}"
        );
    }
}
