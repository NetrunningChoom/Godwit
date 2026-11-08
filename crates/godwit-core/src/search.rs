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

    pub const fn word(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Embed => "embed",
            Self::File => "file",
            Self::Video => "video",
            Self::Image => "image",
            Self::Sound => "sound",
            Self::Sticker => "sticker",
            Self::Poll => "poll",
            Self::Forward => "forward",
        }
    }
}

/// Which content kinds a network supports after `has:`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SearchHasSet(u16);

impl SearchHasSet {
    pub const NONE: Self = Self(0);

    pub const fn new(values: &[SearchHas]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < values.len() {
            bits |= 1 << values[i] as u16;
            i += 1;
        }
        Self(bits)
    }

    pub const fn has(self, value: SearchHas) -> bool {
        self.0 & (1 << value as u16) != 0
    }

    pub fn words(self) -> Vec<&'static str> {
        [
            SearchHas::Link,
            SearchHas::Embed,
            SearchHas::File,
            SearchHas::Video,
            SearchHas::Image,
            SearchHas::Sound,
            SearchHas::Sticker,
            SearchHas::Poll,
            SearchHas::Forward,
        ]
        .into_iter()
        .filter(|value| self.has(*value))
        .map(SearchHas::word)
        .collect()
    }
}

/// One completion for a search box. `query` replaces the unfinished final
/// token and includes a trailing space so the user can keep typing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub label: String,
    pub detail: String,
    pub query: String,
}

/// Suggest a filter, a locally known member, a channel, or a `has:` value.
/// Candidates are supplied by the caller so this logic remains network-neutral
/// and never performs a lookup while the user types.
pub fn suggestions(
    input: &str,
    supported: SearchFilters,
    people: &[(String, String)],
    channels: &[(String, String)],
    has_values: &[&str],
) -> Vec<Suggestion> {
    let mut split = 0;
    let mut quoted = false;
    for (i, c) in input.char_indices() {
        if c == '"' {
            quoted = !quoted;
        } else if c.is_whitespace() && !quoted {
            split = i + c.len_utf8();
        }
    }
    let prefix = &input[split..];
    let before = &input[..split];
    let (key, value) = prefix.split_once(':').unwrap_or((prefix, ""));
    let filter = match key.to_ascii_lowercase().as_str() {
        "from" => Some(SearchFilter::From),
        "mentions" => Some(SearchFilter::Mentions),
        "has" => Some(SearchFilter::Has),
        "before" => Some(SearchFilter::Before),
        "after" => Some(SearchFilter::After),
        "during" => Some(SearchFilter::During),
        "in" => Some(SearchFilter::In),
        "pinned" => Some(SearchFilter::Pinned),
        _ => None,
    };
    let mut out = Vec::new();
    if let Some(filter) = filter.filter(|_| prefix.contains(':')) {
        if !supported.has(filter) {
            return out;
        }
        let needle = value.trim_matches('"').trim_start_matches(['@', '#']).to_lowercase();
        let mut add = |label: String, detail: String, insertion: String| {
            let quoted = if insertion.chars().any(char::is_whitespace) {
                format!("\"{insertion}\"")
            } else {
                insertion
            };
            out.push(Suggestion {
                label,
                detail,
                query: format!("{before}{key}:{quoted} "),
            });
        };
        match filter {
            SearchFilter::From | SearchFilter::Mentions => {
                let mut matching: Vec<&String> = people
                    .iter()
                    .map(|(_, name)| name)
                    .filter(|name| name.to_lowercase().contains(&needle))
                    .collect();
                matching.sort_by_key(|name| !name.to_lowercase().starts_with(&needle));
                for name in matching.into_iter().take(8) {
                    add(name.clone(), String::new(), name.clone());
                }
            }
            SearchFilter::In => {
                let mut matching: Vec<&String> = channels
                    .iter()
                    .map(|(_, name)| name)
                    .filter(|name| name.to_lowercase().contains(&needle))
                    .collect();
                matching.sort_by_key(|name| !name.to_lowercase().starts_with(&needle));
                for name in matching.into_iter().take(8) {
                    add(format!("#{name}"), "Channel".into(), name.clone());
                }
            }
            SearchFilter::Has => {
                for word in has_values {
                    if word.starts_with(&needle) {
                        add((*word).into(), "Message contains".into(), (*word).into());
                    }
                }
            }
            _ => {}
        }
        return out;
    }

    let filters = [
        (SearchFilter::From, "from:", "Author"),
        (SearchFilter::Mentions, "mentions:", "Mentioned user"),
        (SearchFilter::In, "in:", "Channel"),
        (SearchFilter::Has, "has:", "Message content"),
        (SearchFilter::Before, "before:", "Before date"),
        (SearchFilter::After, "after:", "After date"),
        (SearchFilter::During, "during:", "On date"),
        (SearchFilter::Pinned, "pinned:", "Pinned messages"),
    ];
    let needle = key.to_lowercase();
    for (filter, token, detail) in filters {
        if supported.has(filter) && token.trim_end_matches(':').starts_with(&needle) {
            out.push(Suggestion {
                label: token.into(),
                detail: detail.into(),
                query: format!("{before}{token}"),
            });
        }
    }
    out
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
    fn suggestions_offer_filters_members_channels_and_has_values() {
        let people = vec![("u1".into(), "Alice Example".into()), ("u2".into(), "Bob".into())];
        let channels = vec![("c1".into(), "general".into()), ("c2".into(), "help desk".into())];
        let filters = SearchFilters::ALL;
        let has_values = [
            "link", "embed", "file", "video", "image", "sound", "sticker", "poll", "forward",
        ];
        let filters_shown = suggestions("", filters, &people, &channels, &has_values);
        assert!(filters_shown.iter().any(|s| s.label == "from:"));
        assert!(filters_shown.iter().any(|s| s.label == "has:"));

        let authors = suggestions("from:ali", filters, &people, &channels, &has_values);
        assert_eq!(authors.len(), 1);
        assert_eq!(authors[0].label, "Alice Example");
        assert_eq!(authors[0].query, "from:\"Alice Example\" ");
        assert_eq!(
            suggestions("from:\"Alice Ex", filters, &people, &channels, &has_values)[0].label,
            "Alice Example"
        );

        let channels_found = suggestions("in:hel", filters, &people, &channels, &has_values);
        assert_eq!(channels_found[0].label, "#help desk");
        assert_eq!(channels_found[0].query, "in:\"help desk\" ");

        let kinds = suggestions("has:vi", filters, &people, &channels, &has_values);
        assert_eq!(kinds.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["video"]);
    }

    #[test]
    fn suggestions_respect_capabilities_and_replace_only_the_final_token() {
        let people = vec![("u1".into(), "Alice".into())];
        let filters = SearchFilters::DIRECT;
        assert!(suggestions("in:", filters, &people, &[], &[]).is_empty());
        let result = suggestions("some words fro", filters, &people, &[], &[]);
        assert_eq!(result[0].query, "some words from:");
    }

    #[test]
    fn content_suggestion_values_follow_the_neutral_capability_set() {
        let values = SearchHasSet::new(&[SearchHas::Poll, SearchHas::Link]);
        assert!(values.has(SearchHas::Poll));
        assert!(!values.has(SearchHas::Image));
        assert_eq!(values.words(), ["link", "poll"]);
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
