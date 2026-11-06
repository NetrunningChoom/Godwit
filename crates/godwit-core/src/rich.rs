// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Helpers for Godwit rich text (see [`crate::message::Markup::Rich`]):
//! writing it safely from network crates and revealing spoilers in the UI.

/// The URL prefix of a spoiler's link; the hidden text follows, percent-encoded.
pub const SPOILER: &str = "godwit://spoiler/";
/// The URL prefix of a custom emoji's link: its picture's address follows,
/// percent-encoded; the link's text (`:name:`) is what it copies as.
pub const EMOJI: &str = "godwit://emoji/";

/// A custom emoji in rich text: shown as its picture, copied as `:name:`.
pub fn emoji(name: &str, image_url: &str) -> String {
    format!(
        "[{}]({EMOJI}{})",
        escape(&format!(":{name}:")),
        percent_encode(image_url)
    )
}

/// Escapes text so none of it reads as markup; newlines become hard breaks.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\\n"),
            c if c.is_ascii_punctuation() => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

/// An inline code span holding `text` exactly, whatever backticks it has.
pub fn code_span(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let (mut longest, mut run) = (0, 0);
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{text}{pad}{fence}")
}

/// A block of rich text, as UIs lay it out: the CommonMark subset UIs render
/// inline (see `Markup::Rich`) has no quotes, code blocks or headings, so
/// those are separate blocks around plain runs of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// Inline rich text (paragraphs, lists).
    Text(String),
    /// Inline rich text inside a quote.
    Quote(String),
    /// Inline rich text as a heading, level 1–6.
    Heading(u8, String),
    /// Code, exactly as written.
    Code { lang: String, text: String },
}

/// A quote block in rich text: `inner` (rich) with every line marked.
pub fn quote(inner: &str) -> String {
    inner.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n")
}

/// A heading in rich text; `inner` is rich and on one line.
pub fn heading(level: u8, inner: &str) -> String {
    format!(
        "{} {}",
        "#".repeat(usize::from(level.clamp(1, 6))),
        inner.replace('\n', " ")
    )
}

/// A fenced code block holding `text` exactly.
pub fn code_block(lang: &str, text: &str) -> String {
    let (mut longest, mut run) = (0, 0);
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}{lang}\n{text}\n{fence}")
}

/// Splits rich text into blocks. Plain text never starts a line with `>`,
/// `#` or a backtick fence, because [`escape`] escapes them, so only the
/// markers written by [`quote`], [`heading`] and [`code_block`] split it.
pub fn blocks(rich: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut lines = rich.lines().peekable();
    let mut text: Vec<&str> = Vec::new();
    let flush = |text: &mut Vec<&str>, out: &mut Vec<Block>| {
        let joined = text.join("\n");
        if !joined.trim().is_empty() {
            out.push(Block::Text(joined.trim_matches('\n').to_owned()));
        }
        text.clear();
    };
    while let Some(line) = lines.next() {
        if let Some(rest) = line.strip_prefix("```") {
            flush(&mut text, &mut out);
            let ticks = 3 + rest.chars().take_while(|c| *c == '`').count();
            let fence = "`".repeat(ticks);
            let lang = rest.trim_start_matches('`').trim().to_owned();
            let mut code: Vec<&str> = Vec::new();
            for l in lines.by_ref() {
                if l == fence {
                    break;
                }
                code.push(l);
            }
            out.push(Block::Code {
                lang,
                text: code.join("\n"),
            });
        } else if line.starts_with("> ") || line == ">" {
            flush(&mut text, &mut out);
            let mut quoted = vec![line.strip_prefix("> ").unwrap_or("")];
            while let Some(next) = lines.peek().filter(|l| l.starts_with("> ") || **l == ">") {
                quoted.push(next.strip_prefix("> ").unwrap_or(""));
                lines.next();
            }
            out.push(Block::Quote(quoted.join("\n")));
        } else if let Some((hashes, rest)) = line.split_once(' ')
            && !hashes.is_empty()
            && hashes.len() <= 6
            && hashes.bytes().all(|b| b == b'#')
        {
            flush(&mut text, &mut out);
            out.push(Block::Heading(u8::try_from(hashes.len()).unwrap_or(6), rest.to_owned()));
        } else {
            text.push(line);
        }
    }
    flush(&mut text, &mut out);
    out
}

/// A spoiler: blocks on screen, the text only in the link.
pub fn spoiler(text: &str) -> String {
    let hidden: String = text.chars().map(|c| if c.is_whitespace() { c } else { '█' }).collect();
    format!("[{}]({SPOILER}{})", escape(&hidden), percent_encode(text))
}

/// Replaces every spoiler in `rich` with its text (the user clicked one).
pub fn reveal_spoilers(rich: &str) -> String {
    let mut out = String::with_capacity(rich.len());
    let mut rest = rich;
    while let Some(at) = rest.find(&format!("]({SPOILER}")) {
        let Some(open) = rest[..at].rfind('[') else { break };
        let url_start = at + 2 + SPOILER.len();
        let Some(close) = rest[url_start..].find(')') else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&escape(&percent_decode(&rest[url_start..url_start + close])));
        rest = &rest[url_start + close + 1..];
    }
    out.push_str(rest);
    out
}

pub use crate::encoding::percent_encode;

pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spoilers_round_trip() {
        let rich = format!("before {} after", spoiler("a *b* ü"));
        assert!(!rich.contains("*b*"));
        assert_eq!(reveal_spoilers(&rich), format!("before {} after", escape("a *b* ü")));
    }

    #[test]
    fn blocks_split_on_written_markers_only() {
        let rich = [
            "intro".to_owned(),
            String::new(),
            quote("said\\\nthis"),
            heading(2, "Title"),
            code_block("rs", "fn x() {}\n```inner```"),
            escape("> not a quote # nor a heading"),
        ]
        .join("\n");
        assert_eq!(
            blocks(&rich),
            vec![
                Block::Text("intro".into()),
                Block::Quote("said\\\nthis".into()),
                Block::Heading(2, "Title".into()),
                Block::Code {
                    lang: "rs".into(),
                    text: "fn x() {}\n```inner```".into()
                },
                Block::Text(escape("> not a quote # nor a heading")),
            ]
        );
    }

    #[test]
    fn percent_coding_round_trips() {
        for s in ["", "plain", "a b/c?d=e&f", "üñí ✓", "100%"] {
            assert_eq!(percent_decode(&percent_encode(s)), s);
        }
    }

    #[test]
    fn code_spans_fence_their_backticks() {
        assert_eq!(code_span("a"), "`a`");
        assert_eq!(code_span("x`y"), "``x`y``");
        assert_eq!(code_span("`x"), "`` `x ``");
    }
}
