// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Discord's Markdown → Godwit rich text. Discord's dialect is Stoat's with
//! a few spellings changed, so its mentions are respelled for Stoat's parser
//! and converted by `godwit_stoat::rich`:
//!
//! | Discord | Stoat |
//! |---|---|
//! | `<@id>`, `<@!id>` | `<@id>` |
//! | `<@&id>` (role) | `<%id>` |
//! | `<#id>` | `<#id>` |
//! | `<:name:id>`, `<a:name:id>` | `:id:` |
//!
//! Stoat's ids are 26 characters, Discord's snowflakes fewer digits; they're
//! zero-padded to 26 here (a snowflake never starts with 0) and unpadded
//! before they're looked up. Nothing changes inside code.

use godwit_stoat::rich::{self, Resolve};

/// Names for a message's mentions, by Discord id.
pub trait Names {
    fn user(&self, id: &str) -> String;
    fn channel(&self, id: &str) -> String;
    fn role(&self, id: &str) -> String;
    /// A custom emoji's name, as the message spelled it.
    fn emoji(&self, id: &str) -> String;
    fn emoji_url(&self, id: &str) -> Option<String>;
}

const ID_LEN: usize = 26;

fn pad(id: &str) -> String {
    format!("{id:0>ID_LEN$}")
}

fn unpad(id: &str) -> &str {
    id.trim_start_matches('0')
}

fn snowflake(s: &str) -> bool {
    !s.is_empty() && s.len() < ID_LEN && s.bytes().all(|b| b.is_ascii_digit()) && !s.starts_with('0')
}

/// Respells Discord's mentions and custom emoji as Stoat's, leaving code alone.
/// Returns the text and the custom emoji's names by id (from `<:name:id>`).
pub fn respell(src: &str) -> (String, Vec<(String, String)>) {
    let mut out = String::with_capacity(src.len() + 16);
    let mut emoji = Vec::new();
    let mut rest = src;
    let mut fence = false;
    while !rest.is_empty() {
        // Code blocks and spans pass through unchanged.
        if rest.starts_with("```") {
            fence = !fence;
            out.push_str("```");
            rest = &rest[3..];
            continue;
        }
        if fence {
            let next = rest.find("```").unwrap_or(rest.len());
            out.push_str(&rest[..next]);
            rest = &rest[next..];
            continue;
        }
        if let Some(after) = rest.strip_prefix('`') {
            match after.find('`') {
                Some(end) => {
                    out.push_str(&rest[..end + 2]);
                    rest = &rest[end + 2..];
                }
                None => {
                    out.push('`');
                    rest = after;
                }
            }
            continue;
        }
        if rest.starts_with('<')
            && let Some(end) = rest.find('>')
        {
            let inner = &rest[1..end];
            let spelled = if let Some(id) = inner.strip_prefix("@&").filter(|id| snowflake(id)) {
                Some(format!("<%{}>", pad(id)))
            } else if let Some(id) = inner
                .strip_prefix("@!")
                .or_else(|| inner.strip_prefix('@'))
                .filter(|id| snowflake(id))
            {
                Some(format!("<@{}>", pad(id)))
            } else if let Some(id) = inner.strip_prefix('#').filter(|id| snowflake(id)) {
                Some(format!("<#{}>", pad(id)))
            } else {
                let custom = inner.strip_prefix("a:").or_else(|| inner.strip_prefix(':'));
                custom
                    .and_then(|c| c.rsplit_once(':'))
                    .filter(|(_, id)| snowflake(id))
                    .map(|(name, id)| {
                        emoji.push((id.to_owned(), name.to_owned()));
                        format!(":{}:", pad(id))
                    })
            };
            if let Some(spelled) = spelled {
                out.push_str(&spelled);
                rest = &rest[end + 1..];
                continue;
            }
        }
        let mut chars = rest.chars();
        if let Some(c) = chars.next() {
            out.push(c);
        }
        rest = chars.as_str();
    }
    (out, emoji)
}

struct Unpadded<'a, N> {
    names: &'a N,
}

impl<N: Names> Resolve for Unpadded<'_, N> {
    fn user(&self, id: &str) -> String {
        self.names.user(unpad(id))
    }
    fn channel(&self, id: &str) -> String {
        self.names.channel(unpad(id))
    }
    fn role(&self, id: &str) -> String {
        self.names.role(unpad(id))
    }
    fn emoji(&self, id: &str) -> String {
        self.names.emoji(unpad(id))
    }
    fn emoji_url(&self, id: &str) -> Option<String> {
        self.names.emoji_url(unpad(id))
    }
    fn timestamp(&self, secs: i64, style: Option<char>) -> String {
        timestamp(secs, style, chrono::Utc::now().timestamp(), &chrono::Local)
    }
}

/// A `<t:…>` timestamp as Discord's client words it, in the reader's time
/// zone `tz` (docs.discord.food, Reference, "Message Formatting" timestamp
/// styles). The docs' examples are en-US ("April 20, 2021 at 16:20"); the
/// official client follows the locale, and we use the en-GB order, which
/// matches the app's own dd/mm dates, with a 24-hour clock.
/// `now` and `tz` are parameters so tests are exact. `R` is computed when the
/// message is drawn and never ticks: a timer would be idle work for little.
pub fn timestamp<Tz: chrono::TimeZone>(secs: i64, style: Option<char>, now: i64, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let Some(t) = tz.timestamp_opt(secs, 0).single() else {
        return format!("<t:{secs}>");
    };
    match style {
        Some('t') => t.format("%H:%M").to_string(),
        Some('T') => t.format("%H:%M:%S").to_string(),
        Some('d') => t.format("%d/%m/%Y").to_string(),
        Some('s') => t.format("%d/%m/%Y, %H:%M").to_string(),
        Some('S') => t.format("%d/%m/%Y, %H:%M:%S").to_string(),
        Some('D') => t.format("%-d %B %Y").to_string(),
        Some('F') => t.format("%A, %-d %B %Y %H:%M").to_string(),
        Some('R') => {
            let diff = now - secs;
            let r = godwit_stoat::timefmt::relative(diff.abs() * 1000);
            if diff >= 0 || r == "just now" {
                r
            } else {
                format!("in {}", r.trim_end_matches(" ago"))
            }
        }
        _ => t.format("%-d %B %Y %H:%M").to_string(),
    }
}

/// A message's content as Godwit rich text.
pub fn to_rich(src: &str, names: &impl Names) -> String {
    let (text, _) = respell(src);
    rich::to_rich(&text, &Unpadded { names })
}

/// A message's content as plain text (notifications, replies' previews).
pub fn to_plain(src: &str, names: &impl Names) -> String {
    let (text, _) = respell(src);
    rich::to_plain(&text, &Unpadded { names })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_and_emoji_are_respelled_outside_code() {
        let (out, emoji) = respell("hi <@123> <@!45> <@&67> <#89> <:wave:111> <a:spin:222> `<@123>`");
        let p = |id: &str| format!("{id:0>26}");
        assert_eq!(
            out,
            format!(
                "hi <@{}> <@{}> <%{}> <#{}> :{}: :{}: `<@123>`",
                p("123"),
                p("45"),
                p("67"),
                p("89"),
                p("111"),
                p("222")
            )
        );
        assert_eq!(
            emoji,
            vec![("111".into(), "wave".into()), ("222".into(), "spin".into())]
        );
        let (fenced, _) = respell("```\n<@123>\n``` <@1>");
        assert!(fenced.starts_with("```\n<@123>\n```"));
        assert!(fenced.ends_with(&format!("<@{}>", p("1"))));
        assert_eq!(
            respell("<b>x</b> <@0123> a < b").0,
            "<b>x</b> <@0123> a < b",
            "not mentions stay"
        );
    }

    struct Fake;
    impl Names for Fake {
        fn user(&self, id: &str) -> String {
            format!("user{id}")
        }
        fn channel(&self, id: &str) -> String {
            format!("chan{id}")
        }
        fn role(&self, id: &str) -> String {
            format!("role{id}")
        }
        fn emoji(&self, id: &str) -> String {
            format!("emo{id}")
        }
        fn emoji_url(&self, _: &str) -> Option<String> {
            None
        }
    }

    #[test]
    fn plain_text_names_who_was_mentioned() {
        let plain = to_plain("hey <@123> in <#45>, **bold**", &Fake);
        assert!(plain.contains("user123"), "{plain}");
        assert!(plain.contains("chan45"), "{plain}");
        assert!(!plain.contains("000"), "{plain}");
    }

    #[test]
    fn timestamp_styles() {
        let tz = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
        let t = 1_618_928_430; // 2021-04-20 14:20:30 UTC, 16:20:30 at +2
        let f = |c| timestamp(t, c, t, &tz);
        assert_eq!(f(Some('t')), "16:20");
        assert_eq!(f(Some('T')), "16:20:30");
        assert_eq!(f(Some('d')), "20/04/2021");
        assert_eq!(f(Some('s')), "20/04/2021, 16:20");
        assert_eq!(f(Some('S')), "20/04/2021, 16:20:30");
        assert_eq!(f(Some('D')), "20 April 2021");
        assert_eq!(f(None), "20 April 2021 16:20");
        assert_eq!(f(Some('f')), "20 April 2021 16:20");
        assert_eq!(f(Some('F')), "Tuesday, 20 April 2021 16:20");
        assert_eq!(f(Some('R')), "just now");
        assert_eq!(timestamp(t, Some('R'), t + 2 * 86_400, &tz), "2 days ago");
        assert_eq!(timestamp(t, Some('R'), t - 3 * 3600, &tz), "in 3 hours");
    }

    #[test]
    fn timestamps_parse_in_every_style_and_bad_forms_stay_text() {
        for s in ["t", "T", "d", "D", "f", "F", "R", "s", "S"] {
            let out = to_plain(&format!("<t:1618928430:{s}>"), &Fake);
            assert!(!out.contains("<t:"), "{s}: {out}");
        }
        assert!(!to_plain("<t:1618928430>", &Fake).contains("<t:"));
        for bad in ["<t:abc>", "<t:>"] {
            assert!(to_plain(bad, &Fake).contains("t:"), "{bad}");
        }
    }
}
