// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A small parser for the Markdown subset Stoat messages use: bold, italics,
//! strikethrough, inline code, code blocks, quotes, headings, spoilers, links,
//! mentions and custom emoji.

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub spoiler: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Text,
    Link(String),
    User(String),
    Channel(String),
    Role(String),
    Emoji(String),
    /// `<t:unix>` / `<t:unix:style>`: seconds and the style letter (official RE_TIMESTAMP).
    Timestamp(i64, Option<char>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Inline {
    pub text: String,
    pub style: Style,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph(Vec<Inline>),
    Heading(u8, Vec<Inline>),
    Quote(Vec<Inline>),
    Code {
        lang: String,
        text: String,
    },
    /// A bulleted or numbered list (the official `ul` / `ol`).
    List(Vec<ListItem>),
    /// A GFM table: column alignments, the header row, then the body rows.
    Table {
        align: Vec<Align>,
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
}

/// A table column's alignment (from `:---`, `:---:`, `---:`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Align {
    Default,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    /// Nesting, from the indentation (two spaces a level).
    pub depth: u8,
    /// The item's number in a numbered list; None for a bullet.
    pub number: Option<u64>,
    pub inline: Vec<Inline>,
}

pub fn parse(src: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut quote: Vec<&str> = Vec::new();
    // List items so far: depth, the number written (if numbered), the text lines.
    let mut list: Vec<(u8, Option<u64>, Vec<&str>)> = Vec::new();
    let mut lines = src.lines();

    fn flush_list(blocks: &mut Vec<Block>, list: &mut Vec<(u8, Option<u64>, Vec<&str>)>) {
        if list.is_empty() {
            return;
        }
        // Numbered items count up from the first number at their depth, as in Markdown.
        let mut counters: Vec<Option<u64>> = Vec::new();
        let items = list
            .drain(..)
            .map(|(depth, written, text)| {
                let d = depth as usize;
                counters.truncate(d + 1);
                counters.resize(d + 1, None);
                let number = written.map(|w| {
                    let n = counters[d].map_or(w, |prev| prev + 1);
                    counters[d] = Some(n);
                    n
                });
                if written.is_none() {
                    counters[d] = None;
                }
                ListItem {
                    depth,
                    number,
                    inline: parse_inline(&text.join("\n")),
                }
            })
            .collect();
        blocks.push(Block::List(items));
    }

    fn flush(blocks: &mut Vec<Block>, para: &mut Vec<&str>, quote: &mut Vec<&str>) {
        if !para.is_empty() {
            blocks.push(Block::Paragraph(parse_inline(&para.join("\n"))));
            para.clear();
        }
        if !quote.is_empty() {
            blocks.push(Block::Quote(parse_inline(&quote.join("\n"))));
            quote.clear();
        }
    }

    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if let Some((depth, number, rest)) = list_item(line) {
            flush(&mut blocks, &mut para, &mut quote);
            list.push((depth, number, vec![rest]));
            continue;
        }
        // A plain line right after an item carries on that item; a blank one ends the list.
        if let Some(last) = list.last_mut() {
            if !trimmed.is_empty()
                && !trimmed.starts_with("```")
                && !trimmed.starts_with('>')
                && heading(trimmed).is_none()
            {
                last.2.push(trimmed);
                continue;
            }
            flush_list(&mut blocks, &mut list);
            if trimmed.is_empty() {
                continue;
            }
        }
        // A table: a row of cells, then the delimiter row (and its rows after).
        if trimmed.contains('|')
            && let Some(align) = lines.clone().next().and_then(table_delimiter)
            && let header = table_cells(trimmed)
            && header.len() == align.len()
        {
            flush(&mut blocks, &mut para, &mut quote);
            lines.next();
            let mut rows = Vec::new();
            while let Some(row) = lines.clone().next().map(str::trim).filter(|l| l.contains('|')) {
                lines.next();
                let mut cells: Vec<Vec<Inline>> = table_cells(row).iter().map(|c| parse_inline(c)).collect();
                // GFM: short rows are padded, long ones cut, to the header's width.
                cells.resize_with(align.len(), Vec::new);
                rows.push(cells);
            }
            blocks.push(Block::Table {
                align,
                header: header.iter().map(|c| parse_inline(c)).collect(),
                rows,
            });
            continue;
        }
        if let Some(lang) = trimmed.strip_prefix("```") {
            flush(&mut blocks, &mut para, &mut quote);
            // Single-line ```code``` is still a code block.
            if let Some(body) = lang.strip_suffix("```") {
                blocks.push(Block::Code {
                    lang: String::new(),
                    text: body.to_string(),
                });
                continue;
            }
            let mut body = Vec::new();
            for l in lines.by_ref() {
                if l.trim_start().starts_with("```") {
                    break;
                }
                body.push(l);
            }
            blocks.push(Block::Code {
                lang: lang.trim().to_string(),
                text: body.join("\n"),
            });
        } else if let Some(q) = trimmed
            .strip_prefix("> ")
            .or(if trimmed == ">" { Some("") } else { None })
        {
            if !para.is_empty() {
                blocks.push(Block::Paragraph(parse_inline(&para.join("\n"))));
                para.clear();
            }
            quote.push(q);
        } else if let Some((level, rest)) = heading(trimmed) {
            flush(&mut blocks, &mut para, &mut quote);
            blocks.push(Block::Heading(level, parse_inline(rest)));
        } else {
            if !quote.is_empty() {
                blocks.push(Block::Quote(parse_inline(&quote.join("\n"))));
                quote.clear();
            }
            para.push(line);
        }
    }
    flush(&mut blocks, &mut para, &mut quote);
    flush_list(&mut blocks, &mut list);
    blocks
}

/// A table row's cells: split on `|` outside code spans (and not escaped),
/// without the outer pipes.
fn table_cells(row: &str) -> Vec<String> {
    let row = row.trim();
    let row = row.strip_prefix('|').unwrap_or(row);
    let row = if row.ends_with('|') && !row.ends_with("\\|") {
        &row[..row.len() - 1]
    } else {
        row
    };
    let (mut cells, mut cell, mut code, mut escaped) = (Vec::new(), String::new(), false, false);
    for c in row.chars() {
        match c {
            '|' if !code && !escaped => cells.push(std::mem::take(&mut cell).trim().to_string()),
            _ => {
                if c == '`' && !escaped {
                    code = !code;
                }
                // `\|` stands for a pipe inside a cell.
                if c == '|' && escaped {
                    cell.pop();
                }
                cell.push(c);
            }
        }
        escaped = c == '\\' && !escaped;
    }
    cells.push(cell.trim().to_string());
    cells
}

/// A GFM delimiter row (`| --- | :-: |`): each column's alignment.
fn table_delimiter(line: &str) -> Option<Vec<Align>> {
    let line = line.trim();
    if !line.contains('-') {
        return None;
    }
    table_cells(line)
        .iter()
        .map(|c| {
            let (left, right) = (c.starts_with(':'), c.ends_with(':'));
            let dashes = c.trim_matches(':');
            (!dashes.is_empty() && dashes.bytes().all(|b| b == b'-')).then_some(match (left, right) {
                (true, true) => Align::Center,
                (true, false) => Align::Left,
                (false, true) => Align::Right,
                _ => Align::Default,
            })
        })
        .collect()
}

/// A list item line: `- `, `* ` or `+ `, or `1. ` / `1) `; returns its depth,
/// its number (if numbered) and its text.
fn list_item(line: &str) -> Option<(u8, Option<u64>, &str)> {
    let t = line.trim_start_matches([' ', '\t']);
    let depth = ((line.len() - t.len()) / 2).min(8) as u8;
    if let Some(rest) = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .or_else(|| t.strip_prefix("+ "))
    {
        return Some((depth, None, rest));
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if (1..=9).contains(&digits) {
        let after = &t[digits..];
        if let Some(rest) = after.strip_prefix(". ").or_else(|| after.strip_prefix(") ")) {
            return Some((depth, Some(t[..digits].parse().ok()?), rest));
        }
    }
    None
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) {
        line[hashes..].strip_prefix(' ').map(|rest| (hashes as u8, rest))
    } else {
        None
    }
}

fn is_ulid(s: &str) -> bool {
    s.len() == 26 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Delimiters that toggle a style, longest first so `**` wins over `*`.
const MARKERS: &[&str] = &["**", "__", "~~", "||", "*", "_"];

pub fn parse_inline(src: &str) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    let mut style = Style::default();
    let mut buf = String::new();
    let mut i = 0;

    let push = |out: &mut Vec<Inline>, text: String, style: Style, kind: Kind| {
        if text.is_empty() {
            return;
        }
        // Merge adjacent plain runs with the same style.
        if kind == Kind::Text
            && let Some(last) = out.last_mut()
            && last.kind == Kind::Text
            && last.style == style
        {
            last.text.push_str(&text);
            return;
        }
        out.push(Inline { text, style, kind });
    };

    while i < src.len() {
        let rest = &src[i..];
        let Some(c) = rest.chars().next() else { break };

        // Backslash escapes the next character.
        if c == '\\'
            && let Some(next) = rest[1..].chars().next()
            && next.is_ascii_punctuation()
        {
            buf.push(next);
            i += 1 + next.len_utf8();
            continue;
        }

        // Inline code: taken verbatim.
        if c == '`'
            && let Some(end) = rest[1..].find('`')
            && end > 0
        {
            push(&mut out, std::mem::take(&mut buf), style, Kind::Text);
            let code = Style { code: true, ..style };
            push(&mut out, rest[1..1 + end].to_string(), code, Kind::Text);
            i += end + 2;
            continue;
        }

        // Mentions: <@user>, <#channel>, <%role>.
        if c == '<'
            && let Some(end) = rest.find('>')
            && end > 2
        {
            let inner = &rest[1..end];
            let kind = match (&inner[..1], &inner[1..]) {
                ("@", id) if is_ulid(id) => Some(Kind::User(id.to_string())),
                ("#", id) if is_ulid(id) => Some(Kind::Channel(id.to_string())),
                ("%", id) if is_ulid(id) => Some(Kind::Role(id.to_string())),
                _ => timestamp(inner),
            };
            if let Some(kind) = kind {
                push(&mut out, std::mem::take(&mut buf), style, Kind::Text);
                push(&mut out, rest[..=end].to_string(), style, kind);
                i += end + 1;
                continue;
            }
        }

        // Custom emoji: :ULID:
        if c == ':'
            && rest.len() >= 28
            && rest.is_char_boundary(27)
            && is_ulid(&rest[1..27])
            && rest.as_bytes()[27] == b':'
        {
            push(&mut out, std::mem::take(&mut buf), style, Kind::Text);
            push(
                &mut out,
                rest[..28].to_string(),
                style,
                Kind::Emoji(rest[1..27].to_string()),
            );
            i += 28;
            continue;
        }

        // Unicode shortcodes like :smile: become the emoji itself.
        if c == ':'
            && let Some(end) = rest[1..].find(':')
            && end > 0
            && rest[1..1 + end]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-'))
            && let Some(e) = emojis::get_by_shortcode(&rest[1..1 + end])
        {
            buf.push_str(e.as_str());
            i += end + 2;
            continue;
        }

        // Masked links: [text](https://…)
        if c == '['
            && let Some(close) = rest.find("](")
            && let Some(end) = rest[close + 2..].find(')')
        {
            let label = &rest[1..close];
            let url = &rest[close + 2..close + 2 + end];
            if !label.is_empty() && !label.contains('\n') && (url.starts_with("http://") || url.starts_with("https://"))
            {
                push(&mut out, std::mem::take(&mut buf), style, Kind::Text);
                push(&mut out, label.to_string(), style, Kind::Link(url.to_string()));
                i += close + 3 + end;
                continue;
            }
        }

        // Bare links.
        if (rest.starts_with("https://") || rest.starts_with("http://")) && prev_is_boundary(src, i) {
            let mut end = rest
                .find(|ch: char| ch.is_whitespace() || ch == '<' || ch == '>')
                .unwrap_or(rest.len());
            // Trailing punctuation usually ends the sentence, not the URL.
            while end > 0 && rest[..end].ends_with(['.', ',', ')', '!', '?', ';', ':', '\'', '"']) {
                end -= 1;
            }
            push(&mut out, std::mem::take(&mut buf), style, Kind::Text);
            push(
                &mut out,
                rest[..end].to_string(),
                style,
                Kind::Link(rest[..end].to_string()),
            );
            i += end;
            continue;
        }

        // Style delimiters.
        if let Some(m) = MARKERS.iter().find(|m| rest.starts_with(**m)) {
            let flag = marker_flag(&mut style, m);
            let opening = !*flag;
            // `_` only counts at word boundaries, so snake_case stays intact.
            let boundary_ok = !m.starts_with('_')
                || if opening {
                    prev_is_boundary(src, i)
                } else {
                    next_is_boundary(src, i + m.len())
                };
            // Only open when a closing delimiter follows.
            let closes_later = !opening || rest[m.len()..].contains(*m);
            let non_empty = !opening || !rest[m.len()..].starts_with(char::is_whitespace);
            if boundary_ok && closes_later && non_empty {
                push(&mut out, std::mem::take(&mut buf), style, Kind::Text);
                *marker_flag(&mut style, m) = opening;
                i += m.len();
                continue;
            }
        }

        buf.push(c);
        i += c.len_utf8();
    }
    push(&mut out, buf, style, Kind::Text);
    out
}

fn marker_flag<'a>(style: &'a mut Style, marker: &str) -> &'a mut bool {
    match marker {
        "**" | "__" => &mut style.bold,
        "~~" => &mut style.strike,
        "||" => &mut style.spoiler,
        _ => &mut style.italic,
    }
}

fn prev_is_boundary(src: &str, i: usize) -> bool {
    src[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric())
}

fn next_is_boundary(src: &str, i: usize) -> bool {
    src[i..].chars().next().is_none_or(|c| !c.is_alphanumeric())
}

/// True when the message is nothing but emoji (custom or unicode) and
/// whitespace, which the official client renders large.
/// The emoji of a paragraph made only of emoji (custom or Unicode, ignoring
/// spaces), as the official `isOnlyEmoji`; None otherwise. Each is either a
/// custom emoji id (`Ok`) or a Unicode emoji (`Err`).
/// Parses the inside of `<t:1700000000>` or `<t:1700000000:R>`.
fn timestamp(inner: &str) -> Option<Kind> {
    let rest = inner.strip_prefix("t:")?;
    let (secs, style) = match rest.split_once(':') {
        Some((s, f)) => (s, Some(f)),
        None => (rest, None),
    };
    if secs.is_empty() || !secs.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let style = match style {
        Some(f) if f.chars().count() == 1 && f.chars().all(|c| c.is_alphanumeric() || c == '_') => f.chars().next(),
        Some(_) => return None,
        None => None,
    };
    Some(Kind::Timestamp(secs.parse().ok()?, style))
}

pub fn emoji_only(inlines: &[Inline]) -> Option<Vec<Result<String, String>>> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut out = Vec::new();
    for i in inlines {
        match &i.kind {
            Kind::Emoji(id) => out.push(Ok(id.clone())),
            Kind::Text if !i.style.code && !i.style.spoiler => {
                for g in i.text.graphemes(true) {
                    if g.trim().is_empty() {
                        continue;
                    }
                    emojis::get(g)?;
                    out.push(Err(g.to_string()));
                }
            }
            _ => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod emoji_only_tests {
    use super::*;

    #[test]
    fn timestamps_parse() {
        let first = |s: &str| match parse(s).as_slice() {
            [Block::Paragraph(inl)] => inl
                .iter()
                .find(|i| matches!(i.kind, Kind::Timestamp(..)))
                .map(|i| i.kind.clone()),
            _ => None,
        };
        assert_eq!(
            first("at <t:1700000000:R>!"),
            Some(Kind::Timestamp(1_700_000_000, Some('R')))
        );
        assert_eq!(first("<t:1700000000>"), Some(Kind::Timestamp(1_700_000_000, None)));
        assert_eq!(first("<t:abc>"), None);
    }

    #[test]
    fn emoji_only_paragraphs() {
        let only = |s: &str| match parse(s).as_slice() {
            [Block::Paragraph(inl)] => emoji_only(inl).map(|v| v.len()),
            _ => None,
        };
        assert_eq!(only("😀"), Some(1));
        assert_eq!(only("😀 👍🏽  ❤️"), Some(3), "spaces don't count");
        assert_eq!(only("hi 😀"), None);
        assert_eq!(only("`😀`"), None, "code isn't emoji");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(v: &[Inline]) -> Vec<(&str, Style)> {
        v.iter().map(|i| (i.text.as_str(), i.style)).collect()
    }

    #[test]
    fn bold_italic() {
        let v = parse_inline("a **b** *c*");
        let b = Style {
            bold: true,
            ..Default::default()
        };
        let it = Style {
            italic: true,
            ..Default::default()
        };
        assert_eq!(
            texts(&v),
            vec![("a ", Style::default()), ("b", b), (" ", Style::default()), ("c", it)]
        );
    }

    #[test]
    fn unclosed_markers_are_literal() {
        assert_eq!(parse_inline("2 * 3 = 6")[0].text, "2 * 3 = 6");
        assert_eq!(parse_inline("snake_case_name")[0].text, "snake_case_name");
    }

    #[test]
    fn code_is_verbatim() {
        let v = parse_inline("`**not bold**`");
        assert_eq!(v.len(), 1);
        assert!(v[0].style.code && !v[0].style.bold);
        assert_eq!(v[0].text, "**not bold**");
    }

    #[test]
    fn mentions_links_emoji() {
        let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let v = parse_inline(&format!("hi <@{id}> see https://x.org/a. :{id}:"));
        assert!(v.iter().any(|i| i.kind == Kind::User(id.into())));
        assert!(v.iter().any(|i| i.kind == Kind::Link("https://x.org/a".into())));
        assert!(v.iter().any(|i| i.kind == Kind::Emoji(id.into())));
    }

    #[test]
    fn masked_link_and_spoiler() {
        let v = parse_inline("[docs](https://stoat.chat) ||secret||");
        assert_eq!(v[0].kind, Kind::Link("https://stoat.chat".into()));
        assert_eq!(v[0].text, "docs");
        assert!(v.iter().any(|i| i.text == "secret" && i.style.spoiler));
    }

    #[test]
    fn unicode_shortcodes() {
        assert_eq!(
            parse_inline("hi :wave: and :not_a_real_one:")[0].text,
            "hi 👋 and :not_a_real_one:"
        );
    }

    #[test]
    fn blocks() {
        let b = parse("# Title\nline one\nline two\n> quoted\n```rs\nfn x() {}\n```\nafter");
        assert!(matches!(&b[0], Block::Heading(1, _)));
        assert!(matches!(&b[1], Block::Paragraph(_)));
        assert!(matches!(&b[2], Block::Quote(_)));
        assert_eq!(
            b[3],
            Block::Code {
                lang: "rs".into(),
                text: "fn x() {}".into()
            }
        );
        assert!(matches!(&b[4], Block::Paragraph(_)));
    }

    #[test]
    fn lists() {
        let blocks = parse("intro\n- one\n- two\n  - inner\nmore of two? no: inner\n\n3. c\n7. d\n\nafter");
        assert!(matches!(&blocks[0], Block::Paragraph(_)));
        let Block::List(items) = &blocks[1] else {
            panic!("{blocks:?}")
        };
        assert_eq!(
            items.iter().map(|i| (i.depth, i.number)).collect::<Vec<_>>(),
            vec![(0, None), (0, None), (1, None)]
        );
        // A plain line carries on the item before it.
        assert!(items[2].inline.iter().any(|i| i.text.contains("more of two")));
        let Block::List(numbered) = &blocks[2] else {
            panic!("{blocks:?}")
        };
        // Counting up from the first number.
        assert_eq!(
            numbered.iter().map(|i| i.number).collect::<Vec<_>>(),
            vec![Some(3), Some(4)]
        );
        assert!(matches!(&blocks[3], Block::Paragraph(_)));
        // Emphasis isn't a bullet.
        assert!(matches!(parse("*not a list*").as_slice(), [Block::Paragraph(_)]));
    }

    #[test]
    fn tables_and_small_headings() {
        let blocks = parse("| a | `x|y` | c |\n|:--|:-:|--:|\n| 1 | 2 |\n| 4 \\| 5 | 6 | 7 | 8 |\nafter\n###### six");
        let Block::Table { align, header, rows } = &blocks[0] else {
            panic!("{blocks:?}")
        };
        assert_eq!(align, &vec![Align::Left, Align::Center, Align::Right]);
        // A pipe inside a code span stays in its cell.
        assert_eq!(header[1][0].text, "x|y");
        // Rows are padded or cut to the header's width; `\|` is a literal pipe.
        assert_eq!(rows.iter().map(|r| r.len()).collect::<Vec<_>>(), vec![3, 3]);
        assert_eq!(rows[1][0][0].text, "4 | 5");
        assert!(matches!(&blocks[1], Block::Paragraph(_)));
        assert!(matches!(&blocks[2], Block::Heading(6, _)));
        // A lone pipe line isn't a table.
        assert!(matches!(parse("a | b").as_slice(), [Block::Paragraph(_)]));
    }
}
