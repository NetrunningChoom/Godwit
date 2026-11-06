// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Slack's message markup ("mrkdwn",
//! <https://api.slack.com/reference/surfaces/formatting>) to Godwit rich text
//! and plain text, and what the user typed back to Slack's text.
//!
//! `*bold*`, `_italic_`, `~strike~`, `` `code` ``, ```` ```blocks``` ````, `>`
//! quotes (sent as `&gt;`), `<@U…>` mentions, `<#C…|name>` channels,
//! `<!here>`-style broadcasts, `<url|label>` links and `:shortcode:` emoji
//! (as the emoji itself, so the shared emoji pictures show; custom ones stay
//! as their `:name:`). Never panics; anything odd stays text.

use godwit_core::rich;

/// Names for what mrkdwn refers to by id.
pub trait Names {
    /// A user's name, and the id to link (`T…:U…`).
    fn user(&self, id: &str) -> (String, String);
    /// A channel's name, and the room id to link, if known.
    fn channel(&self, id: &str) -> (String, Option<String>);
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Converted {
    pub rich: String,
    pub plain: String,
}

/// `&amp;`, `&lt;`, `&gt;`: the only entities Slack writes.
pub fn decode(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// `:name:` (and `:name::skin-tone-2:`) as the emoji, if it's a standard one.
pub fn shortcode(name: &str) -> Option<String> {
    let base = name.split("::").next().unwrap_or(name);
    let e = emojis::get_by_shortcode(base)?;
    let tone = name.split("::skin-tone-").nth(1).and_then(|t| t.parse::<u8>().ok());
    let toned = tone.and_then(|t| {
        let tone = match t {
            2 => emojis::SkinTone::Light,
            3 => emojis::SkinTone::MediumLight,
            4 => emojis::SkinTone::Medium,
            5 => emojis::SkinTone::MediumDark,
            6 => emojis::SkinTone::Dark,
            _ => return None,
        };
        e.with_skin_tone(tone)
    });
    Some(toned.unwrap_or(e).as_str().to_owned())
}

/// The shortcode Slack reacts with for an emoji (`👍` → `+1`).
pub fn reaction_name(emoji: &str) -> Option<String> {
    let e = emojis::get(emoji)
        .or_else(|| emojis::get(&format!("{emoji}\u{fe0f}")))
        .or_else(|| emojis::get(&emoji.replace('\u{fe0f}', "")))?;
    // Slack names the thumbs as `+1` / `-1`, as GitHub's shortcodes do.
    e.shortcode().map(str::to_owned)
}

/// Where an emphasis mark may open: after start, a space or punctuation,
/// before something that isn't a space.
fn opens(chars: &[char], i: usize) -> bool {
    let before =
        i == 0 || chars[i - 1].is_whitespace() || (chars[i - 1].is_ascii_punctuation() && chars[i - 1] != chars[i]);
    let after = chars.get(i + 1).is_some_and(|c| !c.is_whitespace() && *c != chars[i]);
    before && after
}

fn closes(chars: &[char], i: usize) -> bool {
    let before = i > 0 && !chars[i - 1].is_whitespace();
    let after = chars
        .get(i + 1)
        .is_none_or(|c| c.is_whitespace() || c.is_ascii_punctuation());
    before && after
}

#[derive(Default)]
struct Out {
    rich: String,
    plain: String,
}

impl Out {
    fn text(&mut self, s: &str) {
        self.rich.push_str(&rich::escape(s));
        self.plain.push_str(s);
    }
}

/// One `<…>` reference.
fn reference(inner: &str, names: &impl Names, out: &mut Out) {
    let (target, label) = match inner.split_once('|') {
        Some((t, l)) => (t, Some(decode(l))),
        None => (inner, None),
    };
    if let Some(user) = target.strip_prefix('@') {
        let (name, id) = names.user(user);
        let name = label.unwrap_or(name);
        out.rich
            .push_str(&format!("[{}](godwit://user/{id})", rich::escape(&format!("@{name}"))));
        out.plain.push_str(&format!("@{name}"));
    } else if let Some(channel) = target.strip_prefix('#') {
        let (name, room) = names.channel(channel);
        let name = label.unwrap_or(name);
        match room {
            Some(room) => out.rich.push_str(&format!(
                "[{}](godwit://room/{room})",
                rich::escape(&format!("#{name}"))
            )),
            None => out.rich.push_str(&rich::escape(&format!("#{name}"))),
        }
        out.plain.push_str(&format!("#{name}"));
    } else if let Some(special) = target.strip_prefix('!') {
        // `<!here>`, `<!channel>`, `<!everyone>`, `<!subteam^S…|@team>`, `<!date^…|text>`.
        let shown = label.unwrap_or_else(|| format!("@{}", special.split('^').next().unwrap_or(special)));
        out.text(&shown);
    } else {
        let url = decode(target);
        let lower = url.to_ascii_lowercase();
        if lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:") {
            let shown = label.unwrap_or_else(|| url.trim_start_matches("mailto:").to_owned());
            let href: String = url
                .chars()
                .map(|c| match c {
                    '(' => "%28".to_owned(),
                    ')' => "%29".to_owned(),
                    ' ' => "%20".to_owned(),
                    c => c.to_string(),
                })
                .collect();
            out.rich.push_str(&format!("[{}]({href})", rich::escape(&shown)));
            out.plain.push_str(&shown);
        } else {
            out.text(&label.unwrap_or(url));
        }
    }
}

/// One line (no code blocks, no quote marker).
fn line(text: &str, names: &impl Names, out: &mut Out) {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut plain = String::new();
    let flush = |plain: &mut String, out: &mut Out| {
        if !plain.is_empty() {
            out.text(&decode(plain));
            plain.clear();
        }
    };
    while i < chars.len() {
        let c = chars[i];
        let close_of = |mark: char, from: usize| (from..chars.len()).find(|&j| chars[j] == mark && closes(&chars, j));
        match c {
            '`' => {
                if let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == '`') {
                    flush(&mut plain, out);
                    let code: String = chars[i + 1..end].iter().collect();
                    let code = decode(&code);
                    out.rich.push_str(&rich::code_span(&code));
                    out.plain.push_str(&code);
                    i = end + 1;
                    continue;
                }
            }
            '<' => {
                if let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == '>') {
                    flush(&mut plain, out);
                    let inner: String = chars[i + 1..end].iter().collect();
                    reference(&inner, names, out);
                    i = end + 1;
                    continue;
                }
            }
            ':' => {
                // `:name:` or `:name::skin-tone-3:`.
                let mut end = (i + 1..chars.len()).find(|&j| chars[j] == ':');
                if let Some(e) = end
                    && chars.get(e + 1) == Some(&':')
                {
                    end = (e + 2..chars.len()).find(|&j| chars[j] == ':');
                }
                if let Some(end) = end {
                    let name: String = chars[i + 1..end].iter().collect();
                    if !name.is_empty()
                        && name.chars().all(|c| c.is_ascii_alphanumeric() || "_+-:".contains(c))
                        && let Some(emoji) = shortcode(&name)
                    {
                        flush(&mut plain, out);
                        out.text(&emoji);
                        i = end + 1;
                        continue;
                    }
                }
            }
            '*' | '_' | '~' if opens(&chars, i) => {
                if let Some(end) = close_of(c, i + 1).filter(|&e| e > i + 1) {
                    flush(&mut plain, out);
                    let mark = match c {
                        '*' => "**",
                        '_' => "*",
                        _ => "~~",
                    };
                    let inner: String = chars[i + 1..end].iter().collect();
                    let mut sub = Out::default();
                    line(&inner, names, &mut sub);
                    out.rich.push_str(mark);
                    out.rich.push_str(&sub.rich);
                    out.rich.push_str(mark);
                    out.plain.push_str(&sub.plain);
                    i = end + 1;
                    continue;
                }
            }
            _ => {}
        }
        plain.push(c);
        i += 1;
    }
    flush(&mut plain, out);
}

/// Slack's text to rich and plain text.
pub fn convert(text: &str, names: &impl Names) -> Converted {
    let mut out = Out::default();
    let mut first = true;
    let fences = text.matches("```").count();
    for (k, part) in text.split("```").enumerate() {
        // Odd parts are inside a code block if a fence closes them (an
        // unclosed one stays text).
        if k % 2 == 1 && k < fences {
            let code = decode(part.trim_matches('\n'));
            if !out.rich.is_empty() {
                out.rich.push_str("\n\n");
                out.plain.push('\n');
            }
            out.rich.push_str(&rich::code_block("", &code));
            out.plain.push_str(&code);
            out.rich.push_str("\n\n");
            first = true;
            continue;
        }
        let mut quote: Vec<Out> = Vec::new();
        for l in part.split('\n') {
            if let Some(q) = l.strip_prefix("&gt;").or_else(|| l.strip_prefix('>')) {
                let mut o = Out::default();
                line(q.strip_prefix(' ').unwrap_or(q), names, &mut o);
                quote.push(o);
                continue;
            }
            if !quote.is_empty() {
                end_quote(&mut out, &mut quote);
                first = true;
            }
            if !first {
                out.rich.push_str("\\\n");
                out.plain.push('\n');
            }
            first = false;
            line(l, names, &mut out);
        }
        if !quote.is_empty() {
            end_quote(&mut out, &mut quote);
            first = true;
        }
    }
    Converted {
        rich: out.rich.trim().to_owned(),
        plain: out.plain.trim().to_owned(),
    }
}

fn end_quote(out: &mut Out, quote: &mut Vec<Out>) {
    let inner: Vec<String> = quote.iter().map(|q| q.rich.clone()).collect();
    if !out.rich.is_empty() {
        out.rich.push_str("\n\n");
        out.plain.push('\n');
    }
    out.rich.push_str(&rich::quote(&inner.join("\\\n")));
    out.rich.push_str("\n\n");
    out.plain
        .push_str(&quote.iter().map(|q| q.plain.clone()).collect::<Vec<_>>().join("\n"));
    out.plain.push('\n');
    quote.clear();
}

/// What the user typed, as Slack's text: `&`, `<` and `>` escaped (Slack
/// reads its own markup, `*bold*` and the rest, from the rest).
pub fn from_text(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct N;
    impl Names for N {
        fn user(&self, id: &str) -> (String, String) {
            ("Ada".into(), format!("T1:{id}"))
        }
        fn channel(&self, id: &str) -> (String, Option<String>) {
            ("general".into(), Some(format!("T1:{id}")))
        }
    }

    fn conv(s: &str) -> Converted {
        convert(s, &N)
    }

    #[test]
    fn emphasis_code_and_entities() {
        let c = conv("*bold* _it_ ~gone~ `a &lt; b` &amp; 2*3");
        assert_eq!(c.rich, "**bold** *it* ~~gone~~ `a < b` \\& 2\\*3");
        assert_eq!(c.plain, "bold it gone a < b & 2*3");
    }

    #[test]
    fn references() {
        let c = conv("hi <@U2> in <#C3> see <https://a.b/c(d)|this> <!here>");
        assert_eq!(
            c.rich,
            "hi [\\@Ada](godwit://user/T1:U2) in [\\#general](godwit://room/T1:C3) see [this](https://a.b/c%28d%29) \\@here"
        );
        assert_eq!(conv("<javascript:alert(1)|x>").rich, "x");
    }

    #[test]
    fn emoji_become_characters() {
        assert_eq!(
            conv(":+1: :wave::skin-tone-3: :partyparrot:").plain,
            "👍 👋🏼 :partyparrot:"
        );
        assert_eq!(reaction_name("👍").as_deref(), Some("+1"));
    }

    #[test]
    fn blocks_quotes_and_breaks() {
        let c = conv("one\ntwo\n&gt; quoted\n```let x = 1;```\nafter");
        assert!(c.rich.starts_with("one\\\ntwo\n\n> quoted"), "{}", c.rich);
        assert!(c.rich.contains("```\nlet x = 1;\n```"), "{}", c.rich);
        assert!(c.rich.ends_with("after"), "{}", c.rich);
    }

    #[test]
    fn odd_input_never_panics() {
        for s in [
            "<",
            "```",
            "*",
            ":",
            "<@",
            "_a",
            "~~~",
            "```a",
            "&gt;",
            ":a::skin-tone-x:",
        ] {
            let _ = conv(s);
        }
    }

    #[test]
    fn sent_text_is_escaped() {
        assert_eq!(from_text("a < b & *c*"), "a &lt; b &amp; *c*");
    }
}
