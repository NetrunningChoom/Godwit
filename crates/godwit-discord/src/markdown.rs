// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Discord's Markdown → Godwit rich text. Discord's dialect is Stoat's with
//! a few spellings changed, so its mentions are respelled for Stoat's parser
//! and converted by `godwit_core::markdown_rich`:
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

use godwit_core::markdown_rich::{self as rich, Resolve};

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
        // @everyone and @here (docs.discord.food, Message Formatting): the
        // shared parser's broadcast spelling, so they show as mentions.
        if let Some(word) = ["everyone", "here"]
            .into_iter()
            .find(|w| rest.strip_prefix('@').is_some_and(|r| r.starts_with(w)))
        {
            out.push_str(&format!("<!{word}>"));
            rest = &rest[word.len() + 1..];
            continue;
        }
        // Stoat's @online means nothing on Discord: escaped, so the shared
        // parser leaves it as text.
        if rest.starts_with("@online") && !out.ends_with('\\') {
            out.push('\\');
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

/// The reader's locale for dates: `LC_ALL`, `LC_TIME` then `LANG` (POSIX
/// order), as a chrono locale; en-US when unset, "C" or unknown. Windows and
/// macOS processes mostly lack these variables and get en-US too.
/// Read once: the locale doesn't change while the app runs.
static SYSTEM_LOCALE: std::sync::OnceLock<chrono::Locale> = std::sync::OnceLock::new();

fn system_locale() -> chrono::Locale {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let raw = var("LC_ALL")
        .or_else(|| var("LC_TIME"))
        .or_else(|| var("LANG"))
        .unwrap_or_default();
    parse_locale(&raw).unwrap_or(chrono::Locale::en_US)
}

/// `de_DE.UTF-8@euro`, `de-DE` or a bare `de` as a chrono locale.
fn parse_locale(raw: &str) -> Option<chrono::Locale> {
    let tag = raw.split(['.', '@']).next()?.replace('-', "_");
    if matches!(tag.as_str(), "C" | "POSIX") {
        return None;
    }
    if let Ok(l) = chrono::Locale::try_from(tag.as_str()) {
        return Some(l);
    }
    // A bare language: its usual region (de -> de_DE, nl -> nl_NL), or the
    // first locale of that language.
    let lang = tag.split('_').next()?;
    chrono::Locale::try_from(format!("{lang}_{}", lang.to_uppercase()).as_str()).ok()
}

/// A `<t:…>` timestamp as Discord's client words it, in the reader's time
/// zone `tz` and the system locale. The official client formats with
/// `Intl.DateTimeFormat` in the user's locale (styles: t/T time, d numeric
/// date, D long date, f/F long date and time, s/S numeric date and time;
/// docs.discord.food, Reference, "Message Formatting" timestamp styles).
/// `now` and `tz` are parameters so tests are exact. `R` is computed when the
/// message is drawn and never ticks: a timer would be idle work for little.
pub fn timestamp<Tz: chrono::TimeZone>(secs: i64, style: Option<char>, now: i64, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    timestamp_in(secs, style, now, tz, *SYSTEM_LOCALE.get_or_init(system_locale))
}

/// [`timestamp`] in a given locale. Intl's patterns aren't in chrono's
/// locale data (only month and day names, numeric dates, 12 or 24 hours), so
/// the long date's word order and the "at" joiner come from a small table
/// of the common languages; others get the day-month-year order, with ", ".
pub fn timestamp_in<Tz: chrono::TimeZone>(
    secs: i64,
    style: Option<char>,
    now: i64,
    tz: &Tz,
    locale: chrono::Locale,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let Some(t) = tz.timestamp_opt(secs, 0).single() else {
        return format!("<t:{secs}>");
    };
    let loc = format!("{locale:?}");
    let (lang, region) = loc.split_once('_').unwrap_or((loc.as_str(), ""));
    let h12 = {
        let (x, p) = (
            t.format_localized("%X", locale).to_string(),
            t.format_localized("%p", locale).to_string(),
        );
        !p.is_empty() && x.contains(&p)
    };
    let clock = |seconds: bool| {
        let fmt = match (h12, seconds) {
            (true, false) => "%-I:%M %p",
            (true, true) => "%-I:%M:%S %p",
            (false, false) => "%H:%M",
            (false, true) => "%H:%M:%S",
        };
        t.format_localized(fmt, locale).to_string()
    };
    // Numeric date with two-digit day and month, in the locale's order.
    let numeric = || match lang {
        "en" if region == "US" || region == "PH" => t.format("%m/%d/%Y").to_string(),
        "de" | "ru" | "pl" | "tr" | "cs" | "fi" | "nb" | "nn" | "no" | "uk" | "ro" | "da" => {
            t.format("%d.%m.%Y").to_string()
        }
        "nl" => t.format("%-d-%-m-%Y").to_string(),
        "ja" | "zh" | "sv" | "lt" | "hu" | "ko" => t.format("%Y-%m-%d").to_string(),
        _ => t.format("%d/%m/%Y").to_string(),
    };
    let long = || match (lang, region) {
        ("en", "US" | "CA" | "PH") => t.format_localized("%B %-d, %Y", locale).to_string(),
        ("de" | "nb" | "nn" | "no" | "cs" | "fi", _) => t.format_localized("%-d. %B %Y", locale).to_string(),
        ("es" | "pt", _) => t.format_localized("%-d de %B de %Y", locale).to_string(),
        _ => t.format_localized("%-d %B %Y", locale).to_string(),
    };
    let joiner = match lang {
        "en" => " at ",
        "de" => " um ",
        "fr" => " à ",
        "nl" => " om ",
        _ => ", ",
    };
    match style {
        Some('t') => clock(false),
        Some('T') => clock(true),
        Some('d') => numeric(),
        Some('s') => format!("{}, {}", numeric(), clock(false)),
        Some('S') => format!("{}, {}", numeric(), clock(true)),
        Some('D') => long(),
        Some('F') => {
            // English, German... put a comma after the weekday, except the
            // day-first English regions (en-GB, en-AU).
            let comma = if lang == "en" && !matches!(region, "US" | "CA" | "PH") {
                ""
            } else {
                ","
            };
            format!(
                "{}{comma} {}{joiner}{}",
                t.format_localized("%A", locale),
                long(),
                clock(false)
            )
        }
        Some('R') => {
            let diff = now - secs;
            let r = godwit_core::timefmt::relative(diff.abs() * 1000);
            if diff >= 0 || r == "just now" {
                r
            } else {
                format!("in {}", r.trim_end_matches(" ago"))
            }
        }
        _ => format!("{}{joiner}{}", long(), clock(false)),
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
    fn everyone_and_here_become_broadcasts_outside_code() {
        let (out, _) = respell("@everyone @here `@here` @online");
        assert_eq!(out, "<!everyone> <!here> `@here` \\@online");
    }

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
    fn timestamp_styles_follow_the_locale() {
        use chrono::Locale;
        let tz = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
        let t = 1_618_928_430; // 2021-04-20 14:20:30 UTC, 16:20:30 at +2
        let f = |l, c| timestamp_in(t, c, t, &tz, l);
        // en-US as Intl words it (12-hour clock, month first).
        let us = |c| f(Locale::en_US, c);
        assert_eq!(us(Some('t')), "4:20 PM");
        assert_eq!(us(Some('T')), "4:20:30 PM");
        assert_eq!(us(Some('d')), "04/20/2021");
        assert_eq!(us(Some('s')), "04/20/2021, 4:20 PM");
        assert_eq!(us(Some('S')), "04/20/2021, 4:20:30 PM");
        assert_eq!(us(Some('D')), "April 20, 2021");
        assert_eq!(us(None), "April 20, 2021 at 4:20 PM");
        assert_eq!(us(Some('f')), "April 20, 2021 at 4:20 PM");
        assert_eq!(us(Some('F')), "Tuesday, April 20, 2021 at 4:20 PM");
        let gb = |c| f(Locale::en_GB, c);
        assert_eq!(gb(Some('t')), "16:20");
        assert_eq!(gb(Some('T')), "16:20:30");
        assert_eq!(gb(Some('d')), "20/04/2021");
        assert_eq!(gb(Some('S')), "20/04/2021, 16:20:30");
        assert_eq!(gb(Some('D')), "20 April 2021");
        assert_eq!(gb(Some('f')), "20 April 2021 at 16:20");
        assert_eq!(gb(Some('F')), "Tuesday 20 April 2021 at 16:20");
        let de = |c| f(Locale::de_DE, c);
        assert_eq!(de(Some('t')), "16:20");
        assert_eq!(de(Some('d')), "20.04.2021");
        assert_eq!(de(Some('s')), "20.04.2021, 16:20");
        assert_eq!(de(Some('D')), "20. April 2021");
        assert_eq!(de(Some('f')), "20. April 2021 um 16:20");
        assert_eq!(de(Some('F')), "Dienstag, 20. April 2021 um 16:20");
        assert_eq!(us(Some('R')), "just now");
        assert_eq!(
            timestamp_in(t, Some('R'), t + 2 * 86_400, &tz, Locale::en_US),
            "2 days ago"
        );
        assert_eq!(
            timestamp_in(t, Some('R'), t - 3 * 3600, &tz, Locale::en_US),
            "in 3 hours"
        );
    }

    #[test]
    fn locale_names_parse() {
        assert_eq!(parse_locale("de_DE.UTF-8"), Some(chrono::Locale::de_DE));
        assert_eq!(parse_locale("en-GB"), Some(chrono::Locale::en_GB));
        assert_eq!(parse_locale("nl"), Some(chrono::Locale::nl_NL));
        assert_eq!(parse_locale("C"), None);
        assert_eq!(parse_locale(""), None);
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
