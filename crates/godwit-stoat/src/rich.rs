// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Stoat's Markdown → Godwit rich text (see `godwit_core::message::Markup::Rich`).
//!
//! The parser in [`crate::markdown`] understands Stoat's dialect; this turns
//! its output into the CommonMark subset every Godwit UI renders, with
//! mentions resolved to names. Anything the subset lacks (headings, quotes,
//! code blocks, tables) is approximated with what it has.

use std::fmt::Write as _;

use godwit_core::rich::{self, code_span, escape};

use crate::markdown::{self, Block, Inline, Kind, Style};

/// Names for the ids a message mentions.
pub trait Resolve {
    fn user(&self, id: &str) -> String;
    fn channel(&self, id: &str) -> String;
    fn role(&self, id: &str) -> String;
    fn emoji(&self, id: &str) -> String;
    /// A custom emoji's picture, when it can be shown (else its `:name:`).
    fn emoji_url(&self, _id: &str) -> Option<String> {
        None
    }
    /// `<t:…>` in the reader's time zone, in the official styles.
    fn timestamp(&self, secs: i64, style: Option<char>) -> String;
}

/// Converts one message's content.
pub fn to_rich(src: &str, names: &impl Resolve) -> String {
    let mut out = String::new();
    for block in markdown::parse(src) {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        match block {
            Block::Paragraph(inlines) => inline_run(&mut out, &inlines, names),
            // No headings in the subset: a bold line instead.
            Block::Heading(level, inlines) => {
                let mut inner = String::new();
                inline_run(&mut inner, &inlines, names);
                out.push_str(&rich::heading(level, &inner));
            }
            Block::Quote(inlines) => {
                let mut inner = String::new();
                inline_run(&mut inner, &inlines, names);
                out.push_str(&rich::quote(&inner));
            }
            Block::Code { lang, text } => out.push_str(&rich::code_block(&lang, &text)),
            Block::List(items) => {
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push('\n');
                    }
                    out.push_str(&"  ".repeat(usize::from(item.depth)));
                    match item.number {
                        Some(n) => {
                            let _ = write!(out, "{n}. ");
                        }
                        None => out.push_str("- "),
                    }
                    inline_run(&mut out, &item.inline, names);
                }
            }
            // No tables in the subset: one line per row, cells split by bars.
            Block::Table { header, rows, .. } => {
                let row = |out: &mut String, cells: &[Vec<Inline>]| {
                    for (i, cell) in cells.iter().enumerate() {
                        if i > 0 {
                            out.push_str(" \\| ");
                        }
                        inline_run(out, cell, names);
                    }
                };
                out.push_str("**");
                row(&mut out, &header);
                out.push_str("**");
                for r in &rows {
                    out.push_str("\\\n");
                    row(&mut out, r);
                }
            }
        }
    }
    out
}

/// The text of a message without markup, mentions as names and spoilers
/// hidden: for notifications.
pub fn to_plain(src: &str, names: &impl Resolve) -> String {
    let mut out = String::new();
    for block in markdown::parse(src) {
        let inlines: Vec<Inline> = match block {
            Block::Paragraph(i) | Block::Heading(_, i) | Block::Quote(i) => i,
            Block::Code { text, .. } => {
                push_line(&mut out, &text);
                continue;
            }
            Block::List(items) => items.into_iter().flat_map(|i| i.inline).collect(),
            Block::Table { header, rows, .. } => {
                header.into_iter().chain(rows.into_iter().flatten()).flatten().collect()
            }
        };
        let mut line = String::new();
        for i in &inlines {
            if i.style.spoiler {
                line.push_str("(spoiler)");
                continue;
            }
            match &i.kind {
                Kind::User(id) => line.push_str(&format!("@{}", names.user(id))),
                Kind::Channel(id) => line.push_str(&format!("#{}", names.channel(id))),
                Kind::Role(id) => line.push_str(&format!("@{}", names.role(id))),
                Kind::Emoji(id) => line.push_str(&format!(":{}:", names.emoji(id))),
                Kind::Timestamp(secs, style) => line.push_str(&names.timestamp(*secs, *style)),
                Kind::Text | Kind::Link(_) => line.push_str(&i.text),
            }
        }
        push_line(&mut out, &line);
    }
    out
}

fn push_line(out: &mut String, line: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(line);
}

fn inline_run(out: &mut String, inlines: &[Inline], names: &impl Resolve) {
    for i in inlines {
        let text = match &i.kind {
            Kind::Text => None,
            Kind::Link(_) => None,
            Kind::User(id) => Some((format!("@{}", names.user(id)), format!("godwit://user/{id}"))),
            Kind::Channel(id) => Some((format!("#{}", names.channel(id)), format!("godwit://room/{id}"))),
            Kind::Role(id) => Some((format!("@{}", names.role(id)), format!("godwit://role/{id}"))),
            Kind::Emoji(id) => {
                match names.emoji_url(id) {
                    Some(url) => out.push_str(&rich::emoji(&names.emoji(id), &url)),
                    None => out.push_str(&escape(&format!(":{}:", names.emoji(id)))),
                }
                continue;
            }
            Kind::Timestamp(secs, style) => {
                styled(out, i.style, &escape(&names.timestamp(*secs, *style)));
                continue;
            }
        };
        if i.style.spoiler {
            // Hidden until clicked: the text travels in the link, not on screen.
            out.push_str(&rich::spoiler(&i.text));
            continue;
        }
        if i.style.code {
            out.push_str(&code_span(&i.text));
            continue;
        }
        match (&i.kind, text) {
            (_, Some((label, url))) => styled(out, i.style, &format!("[{}]({url})", escape(&label))),
            (Kind::Link(url), None) => {
                let label = if i.text.is_empty() {
                    url.as_str()
                } else {
                    i.text.as_str()
                };
                styled(
                    out,
                    i.style,
                    &format!("[{}](<{}>)", escape(label), url.replace(['<', '>', '\n'], "")),
                );
            }
            _ => styled(out, i.style, &escape(&i.text)),
        }
    }
}

/// Wraps already-escaped text in emphasis markers. Markers go inside any
/// leading or trailing spaces, as CommonMark needs them next to text.
fn styled(out: &mut String, style: Style, text: &str) {
    let core = text.trim();
    if core.is_empty() {
        out.push_str(text);
        return;
    }
    let lead = &text[..text.len() - text.trim_start().len()];
    let trail = &text[text.trim_end().len()..];
    let mut open = String::new();
    if style.bold {
        open.push_str("**");
    }
    if style.italic {
        open.push('*');
    }
    if style.strike {
        open.push_str("~~");
    }
    let close: String = open.chars().rev().collect();
    out.push_str(lead);
    out.push_str(&open);
    out.push_str(core);
    out.push_str(&close);
    out.push_str(trail);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Names;
    impl Resolve for Names {
        fn user(&self, id: &str) -> String {
            format!("user-{id}")
        }
        fn channel(&self, id: &str) -> String {
            format!("chan-{id}")
        }
        fn role(&self, id: &str) -> String {
            format!("role-{id}")
        }
        fn emoji(&self, id: &str) -> String {
            format!("emoji-{id}")
        }
        fn timestamp(&self, secs: i64, _: Option<char>) -> String {
            format!("T{secs}")
        }
    }

    fn rich(src: &str) -> String {
        to_rich(src, &Names)
    }

    #[test]
    fn plain_text_is_escaped() {
        assert_eq!(rich("price: 5$ [x] <b> #tag"), r"price\: 5\$ \[x\] \<b\> \#tag");
    }

    #[test]
    fn styles_and_line_breaks() {
        assert_eq!(rich("**bold** and *it*"), "**bold** and *it*");
        assert_eq!(rich("~~gone~~"), "~~gone~~");
        assert_eq!(rich("one\ntwo"), "one\\\ntwo");
    }

    #[test]
    fn mentions_become_links_with_names() {
        let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        assert_eq!(
            rich(&format!("hi <@{id}>")),
            format!("hi [\\@user\\-{id}](godwit://user/{id})")
        );
        assert_eq!(
            rich(&format!("see <#{id}>")),
            format!("see [\\#chan\\-{id}](godwit://room/{id})")
        );
    }

    #[test]
    fn code_keeps_backticks_and_markup_literal() {
        assert_eq!(rich("`a*b`"), "`a*b`");
        assert_eq!(rich("```\nfn x() {}\n*y*\n```"), "```\nfn x() {}\n*y*\n```");
    }

    #[test]
    fn spoilers_hide_their_text() {
        let out = rich("||secret word||");
        assert!(
            out.starts_with("[██████ ████](godwit://spoiler/secret%20word)"),
            "{out}"
        );
        assert!(!out.contains("secret word"));
    }

    #[test]
    fn plain_text_for_notifications() {
        let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        assert_eq!(
            to_plain(&format!("**hi** <@{id}> ||x||"), &Names),
            format!("hi @user-{id} (spoiler)")
        );
    }

    #[test]
    fn links_lists_and_headings() {
        assert_eq!(
            rich("[site](https://example.org/a_b)"),
            "[site](<https://example.org/a_b>)"
        );
        assert_eq!(rich("- one\n- two"), "- one\n- two");
        assert_eq!(rich("# Title"), "# Title");
        assert_eq!(rich("> quoted"), "> quoted");
    }
}
