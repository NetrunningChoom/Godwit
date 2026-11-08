// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Message search, whatever the network: the filters typed in the search box
//! (`from:`, `has:`, `before:`, ...) as the official Discord client takes
//! them, parsed here once; a network maps the resulting [`SearchQuery`] to
//! its own API and says which filters it understands
//! ([`crate::room::Capabilities::search_filters`]).

use serde::{Deserialize, Serialize};

use crate::ids::{RoomId, UserId};

/// One kind of filter a network's search can take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchFilter {
    From,
    Mentions,
    Has,
    Before,
    After,
    During,
    /// `in:#channel`: another channel of the server.
    In,
    Pinned,
    /// "This channel" / "whole server".
    Scope,
}

impl SearchFilter {
    const fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// The filters a network supports, as a set (so `Capabilities` stays `Copy`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SearchFilters(u16);

impl SearchFilters {
    pub const NONE: Self = Self(0);
    pub const ALL: Self = Self(0x1ff);
    /// Everything but the server-only `in:` and the scope.
    pub const DIRECT: Self = Self(0x1ff & !SearchFilter::In.bit() & !SearchFilter::Scope.bit());

    pub const fn has(self, filter: SearchFilter) -> bool {
        self.0 & filter.bit() != 0
    }

    /// The hint line the UI shows: the typed forms this set takes.
    pub fn hint(self) -> String {
        let mut words = Vec::new();
        for (filter, word) in [
            (SearchFilter::From, "from:"),
            (SearchFilter::Mentions, "mentions:"),
            (SearchFilter::Has, "has:"),
            (SearchFilter::Before, "before:"),
            (SearchFilter::After, "after:"),
            (SearchFilter::During, "during:"),
            (SearchFilter::In, "in:"),
            (SearchFilter::Pinned, "pinned:"),
        ] {
            if self.has(filter) {
                words.push(word);
            }
        }
        if words.is_empty() {
            String::new()
        } else {
            format!("Filters: {}", words.join(" "))
        }
    }
}

/// What a message has, for `has:`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SearchHas {
    Link,
    Embed,
    File,
    Video,
    Image,
    Sound,
    Sticker,
    Poll,
    Forward,
}

impl SearchHas {
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word.to_ascii_lowercase().as_str() {
            "link" => Self::Link,
            "embed" => Self::Embed,
            "file" => Self::File,
            "video" => Self::Video,
            "image" => Self::Image,
            "sound" => Self::Sound,
            "sticker" => Self::Sticker,
            "poll" => Self::Poll,
            "forward" => Self::Forward,
            _ => return None,
        })
    }
}

/// Where a search looks, for a channel of a server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchScope {
    #[default]
    Channel,
    Server,
}

/// A calendar day as typed (`YYYY-MM-DD`); the network turns it into its own
/// time, in the reader's local day.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SearchDate {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl SearchDate {
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('-');
        let year: i32 = parts.next()?.parse().ok()?;
        let month: u8 = parts.next()?.parse().ok()?;
        let day: u8 = parts.next()?.parse().ok()?;
        if parts.next().is_some() || !(1..=12).contains(&month) || day == 0 {
            return None;
        }
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let last = match month {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        (day <= last).then_some(Self { year, month, day })
    }
}

/// A search, resolved: names are ids, the filters are neutral.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: String,
    pub from: Option<UserId>,
    pub mentions: Option<UserId>,
    pub has: Vec<SearchHas>,
    /// Before the start of this day.
    pub before: Option<SearchDate>,
    /// After the end of this day.
    pub after: Option<SearchDate>,
    /// Within this day.
    pub during: Option<SearchDate>,
    /// `in:#channel`: only this channel of the server.
    pub in_room: Option<RoomId>,
    pub pinned: bool,
    pub scope: SearchScope,
}

impl SearchQuery {
    pub fn text_only(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
}

/// A typed search before names are looked up.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedSearch {
    pub text: String,
    pub from: Option<String>,
    pub mentions: Option<String>,
    pub has: Vec<SearchHas>,
    pub before: Option<SearchDate>,
    pub after: Option<SearchDate>,
    pub during: Option<SearchDate>,
    pub in_channel: Option<String>,
    pub pinned: bool,
    /// Filters with values that make no sense (a bad date, `has:cat`).
    pub errors: Vec<String>,
}

/// Splits on spaces, keeping `"quoted values"` (`from:"Some Name"`) whole.
fn tokens(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in input.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Parses the typed search; a filter the network doesn't take stays text.
pub fn parse(input: &str, supported: SearchFilters) -> ParsedSearch {
    let mut p = ParsedSearch::default();
    let mut words = Vec::new();
    for token in tokens(input) {
        let Some((key, value)) = token.split_once(':') else {
            words.push(token);
            continue;
        };
        let value = value.trim_matches('"');
        let key = key.to_ascii_lowercase();
        let filter = match key.as_str() {
            "from" => SearchFilter::From,
            "mentions" => SearchFilter::Mentions,
            "has" => SearchFilter::Has,
            "before" => SearchFilter::Before,
            "after" => SearchFilter::After,
            "during" => SearchFilter::During,
            "in" => SearchFilter::In,
            "pinned" => SearchFilter::Pinned,
            _ => SearchFilter::Scope,
        };
        if filter == SearchFilter::Scope || !supported.has(filter) || value.is_empty() {
            words.push(token);
            continue;
        }
        let name = |v: &str| v.trim_start_matches(['@', '#']).to_owned();
        match filter {
            SearchFilter::From => p.from = Some(name(value)),
            SearchFilter::Mentions => p.mentions = Some(name(value)),
            SearchFilter::In => p.in_channel = Some(name(value)),
            SearchFilter::Has => match SearchHas::parse(value) {
                Some(h) => {
                    if !p.has.contains(&h) {
                        p.has.push(h);
                    }
                }
                None => p.errors.push(format!("has:{value} isn't something to look for")),
            },
            SearchFilter::Pinned => p.pinned = value.eq_ignore_ascii_case("true"),
            SearchFilter::Before | SearchFilter::After | SearchFilter::During => match SearchDate::parse(value) {
                Some(d) => match filter {
                    SearchFilter::Before => p.before = Some(d),
                    SearchFilter::After => p.after = Some(d),
                    _ => p.during = Some(d),
                },
                None => p.errors.push(format!("{key}:{value} needs a date like 2026-11-09")),
            },
            SearchFilter::Scope => {}
        }
    }
    p.text = words.join(" ");
    // Dates that leave no day to look in are not sent.
    if let (Some(b), Some(a)) = (p.before, p.after)
        && b <= a
    {
        p.errors.push("before: is earlier than after:".into());
    }
    if let Some(d) = p.during {
        if p.before.is_some_and(|b| b <= d) {
            p.errors.push("during: is not before the before: day".into());
        }
        if p.after.is_some_and(|a| a >= d) {
            p.errors.push("during: is not after the after: day".into());
        }
    }
    p
}

/// Looks `name` up among `(id, shown name)` pairs: the same name ignoring
/// case, else the only one that starts with it.
pub fn find_named<T: Clone + PartialEq>(name: &str, known: &[(T, String)]) -> Option<T> {
    let name = name.to_lowercase();
    if let Some((id, _)) = known.iter().find(|(_, n)| n.to_lowercase() == name) {
        return Some(id.clone());
    }
    // One person listed under several names (a rename) is still one match.
    let mut starts = known.iter().filter(|(_, n)| n.to_lowercase().starts_with(&name));
    let first = &starts.next()?.0;
    starts.all(|(id, _)| id == first).then(|| first.clone())
}

/// Resolves a parsed search against the people and channels we know; names
/// that match nobody come back as errors and nothing should be sent.
pub fn resolve(
    p: ParsedSearch,
    scope: SearchScope,
    people: &[(UserId, String)],
    channels: &[(RoomId, String)],
) -> Result<SearchQuery, Vec<String>> {
    let mut errors = p.errors;
    let mut person = |label: &str, name: Option<&String>| {
        let name = name?;
        let found = find_named(name, people);
        if found.is_none() {
            errors.push(format!("{label}:{name} isn't someone we know here"));
        }
        found
    };
    let from = person("from", p.from.as_ref());
    let mentions = person("mentions", p.mentions.as_ref());
    let in_room = match &p.in_channel {
        Some(name) => {
            let found = find_named(name, channels);
            if found.is_none() {
                errors.push(format!("in:#{name} isn't a channel here"));
            }
            found
        }
        None => None,
    };
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(SearchQuery {
        text: p.text,
        from,
        mentions,
        has: p.has,
        before: p.before,
        after: p.after,
        during: p.during,
        in_room,
        pinned: p.pinned,
        scope,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::AccountId;
    use crate::network::Network;

    fn acct() -> AccountId {
        AccountId::new(Network::Discord, "me")
    }

    #[test]
    fn every_filter_parses() {
        let p = parse(
            "lunch from:@Robin mentions:sam has:link has:image has:link before:2026-11-09 \
             after:2026-01-31 during:2026-06-15 in:#general pinned:true",
            SearchFilters::ALL,
        );
        assert_eq!(p.text, "lunch");
        assert_eq!(p.from.as_deref(), Some("Robin"));
        assert_eq!(p.mentions.as_deref(), Some("sam"));
        assert_eq!(p.has, [SearchHas::Link, SearchHas::Image]);
        assert_eq!(
            p.before,
            Some(SearchDate {
                year: 2026,
                month: 11,
                day: 9
            })
        );
        assert_eq!(
            p.after,
            Some(SearchDate {
                year: 2026,
                month: 1,
                day: 31
            })
        );
        assert_eq!(
            p.during,
            Some(SearchDate {
                year: 2026,
                month: 6,
                day: 15
            })
        );
        assert_eq!(p.in_channel.as_deref(), Some("general"));
        assert!(p.pinned && p.errors.is_empty());
        for word in ["video", "sound", "sticker", "poll", "forward", "embed", "file"] {
            assert!(SearchHas::parse(word).is_some(), "{word}");
        }
    }

    #[test]
    fn quotes_keep_names_whole_and_bad_values_are_errors() {
        let p = parse(
            r#"from:"Some Name" x has:cat before:2026-02-30 after:soon"#,
            SearchFilters::ALL,
        );
        assert_eq!(p.from.as_deref(), Some("Some Name"));
        assert_eq!(p.text, "x");
        assert_eq!(p.errors.len(), 3);
        assert!(SearchDate::parse("2023-02-29").is_none());
    }

    #[test]
    fn contradicting_dates_are_errors_and_renames_are_one_person() {
        let p = parse("before:2026-05-01 after:2026-05-01", SearchFilters::ALL);
        assert_eq!(p.errors, ["before: is earlier than after:"]);
        assert!(
            parse("after:2026-05-01 before:2026-05-03", SearchFilters::ALL)
                .errors
                .is_empty()
        );
        assert_eq!(
            parse("during:2026-05-01 before:2026-05-01", SearchFilters::ALL)
                .errors
                .len(),
            1
        );
        assert_eq!(
            parse("during:2026-05-01 after:2026-05-01", SearchFilters::ALL)
                .errors
                .len(),
            1
        );
        let robin = UserId::new(acct(), "1");
        let known = [(robin.clone(), "Robin".to_owned()), (robin.clone(), "Robby".to_owned())];
        assert_eq!(find_named("rob", &known), Some(robin));
    }

    #[test]
    fn unsupported_filters_stay_text() {
        let p = parse("from:robin hi", SearchFilters::NONE);
        assert_eq!(p.text, "from:robin hi");
        assert!(p.from.is_none());
        assert!(SearchFilters::NONE.hint().is_empty());
        assert!(SearchFilters::ALL.hint().contains("has:"));
    }

    #[test]
    fn names_resolve_ignoring_case_and_unknown_ones_are_errors() {
        let robin = UserId::new(acct(), "1");
        let robert = UserId::new(acct(), "2");
        let people = [(robin.clone(), "Robin".to_owned()), (robert, "Robert".to_owned())];
        let general = RoomId::new(acct(), "9");
        let channels = [(general.clone(), "general".to_owned())];
        let ok = resolve(
            parse("from:ROBIN in:general x", SearchFilters::ALL),
            SearchScope::Server,
            &people,
            &channels,
        );
        let q = ok.expect("resolves");
        assert_eq!(
            (q.from, q.in_room, q.scope),
            (Some(robin), Some(general), SearchScope::Server)
        );
        // "rob" starts two names: not guessed.
        let err = resolve(
            parse("from:rob mentions:nobody", SearchFilters::ALL),
            SearchScope::Channel,
            &people,
            &channels,
        );
        assert_eq!(err.expect_err("unresolved").len(), 2);
    }
}
