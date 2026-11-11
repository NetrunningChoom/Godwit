// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Network-neutral composer suggestions and trigger handling.

use std::sync::Arc;

use crate::ids::{RoomId, UserId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerKind {
    Person,
    Channel,
    Emoji,
    MentionSpecial,
}

/// A network-specific mention offered from state already held in memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionSpecial {
    pub id: String,
    pub label: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub kind: TriggerKind,
    pub start: usize,
    pub end: usize,
    pub query: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CandidateId {
    Person(UserId),
    Channel(RoomId),
    Emoji(Arc<str>),
    MentionSpecial(Arc<str>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub id: CandidateId,
    pub name: Arc<str>,
    /// Text visible in the suggestion row.
    pub detail: Arc<str>,
}

/// Finds the active trigger ending at a UTF-8 byte cursor. `@` in an email
/// address is ignored; triggers otherwise begin at a word boundary.
pub fn trigger_at(text: &str, cursor: usize) -> Option<Trigger> {
    if !text.is_char_boundary(cursor) || cursor > text.len() {
        return None;
    }
    let before = &text[..cursor];
    // Match the current token after any punctuation boundary, so `(@al` and
    // `hi, #gen` work while an email's @ remains part of its token.
    let start = before
        .char_indices()
        .rev()
        .find_map(|(i, c)| {
            (c.is_whitespace()
                || (c.is_ascii_punctuation() && !matches!(c, '_' | '-' | '.' | '+' | '@' | '#' | ':' | '%')))
            .then_some(i + c.len_utf8())
        })
        .unwrap_or(0);
    let word = &before[start..];
    let (kind, prefix) = match word.chars().next()? {
        '@' => (TriggerKind::Person, '@'),
        '%' => (TriggerKind::MentionSpecial, '%'),
        '#' => (TriggerKind::Channel, '#'),
        ':' if word.chars().count() >= 3 => (TriggerKind::Emoji, ':'),
        _ => return None,
    };
    if kind == TriggerKind::Person && start > 0 {
        let previous = before[..start].chars().next_back()?;
        if previous.is_alphanumeric() || previous == '_' || previous == '.' || previous == '-' || previous == '+' {
            return None;
        }
    }
    if !word.starts_with(prefix) || word[1..].chars().any(char::is_whitespace) {
        return None;
    }
    // Match only the prefix before the caret, but replace the complete token
    // so typing in its middle does not leave a stale suffix behind.
    let end = text[cursor..]
        .char_indices()
        .find_map(|(offset, c)| (!c.is_alphanumeric() && !matches!(c, '_' | '-' | '.')).then_some(cursor + offset))
        .unwrap_or(text.len());
    Some(Trigger {
        kind,
        start,
        end,
        query: word[1..].to_owned(),
    })
}

/// Returns at most eight matches, ordered by exact, prefix, then substring match.
/// Input order breaks ties, allowing callers to put current members first.
pub fn matches<'a>(candidates: &'a [Candidate], query: &str, limit: usize) -> Vec<&'a Candidate> {
    let q = query.to_lowercase();
    let mut found: Vec<(u8, usize, &Candidate)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let name = match &c.id {
                CandidateId::MentionSpecial(_) => c
                    .name
                    .strip_prefix('@')
                    .or_else(|| c.name.strip_prefix('%'))
                    .unwrap_or(&c.name)
                    .to_lowercase(),
                _ => c.name.to_lowercase(),
            };
            let rank = if name == q {
                0
            } else if name.starts_with(&q) {
                1
            } else if name.contains(&q) {
                2
            } else {
                return None;
            };
            Some((rank, i, c))
        })
        .collect();
    found.sort_by_key(|(rank, index, _)| (*rank, *index));
    found.into_iter().take(limit.min(8)).map(|(_, _, c)| c).collect()
}

/// Replaces the trigger span and returns the new text and UTF-8 cursor offset.
pub fn insert(text: &str, trigger: &Trigger, replacement: &str) -> (String, usize) {
    let end = trigger.end.min(text.len());
    let start = trigger.start.min(end);
    let mut result = String::with_capacity(text.len() + replacement.len());
    result.push_str(&text[..start]);
    result.push_str(replacement);
    let cursor = result.len();
    result.push_str(&text[end..]);
    (result, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_at_cursor_and_rejects_emails() {
        assert_eq!(
            trigger_at("@al hi", 3).map(|t| (t.start, t.query)),
            Some((0, "al".into()))
        );
        assert_eq!(trigger_at("hi @al!", 6).map(|t| t.start), Some(3));
        assert_eq!(trigger_at("@alex", 3).map(|t| (t.query, t.end)), Some(("al".into(), 5)));
        assert_eq!(trigger_at("hi, (@al", 8).map(|t| t.start), Some(5));
        assert_eq!(trigger_at("hi!@al", 6).map(|t| t.start), Some(3));
        assert_eq!(trigger_at("mail me@host", 12), None);
        assert_eq!(trigger_at("hi #gen", 7).map(|t| t.kind), Some(TriggerKind::Channel));
        assert_eq!(trigger_at("hi :smile", 9).map(|t| t.kind), Some(TriggerKind::Emoji));
        assert_eq!(
            trigger_at("hi %team", 8).map(|t| t.kind),
            Some(TriggerKind::MentionSpecial)
        );
        assert_eq!(trigger_at("hi :x", 5), None);
    }

    #[test]
    fn ranking_and_limit_are_stable() {
        let candidates = [
            "alphabet", "al", "pAl", "also", "alfa", "algae", "alias", "alps", "alone",
        ]
        .map(|n| Candidate {
            id: CandidateId::Emoji(n.into()),
            name: n.into(),
            detail: Arc::from(""),
        });
        let got = matches(&candidates, "al", 20);
        assert_eq!(got.len(), 8);
        assert_eq!(got[0].name.as_ref(), "al");
        assert_eq!(got[1].name.as_ref(), "alphabet");
        assert_eq!(matches(&candidates, "pha", 8)[0].name.as_ref(), "alphabet");
    }

    #[test]
    fn insertion_replaces_only_the_trigger_word() {
        let t = trigger_at("hi @al there", 6).expect("trigger in test fixture");
        assert_eq!(insert("hi @al there", &t, "<@42>"), ("hi <@42> there".into(), 8));
        let t = trigger_at("@alex", 3).expect("middle of trigger");
        assert_eq!(insert("@alex", &t, "@Ada"), ("@Ada".into(), 4));
    }

    #[test]
    fn bare_at_offers_every_person_and_a_partial_name_narrows_them() {
        let people = ["Ada", "Alan", "Grace"].map(|n| Candidate {
            id: CandidateId::MentionSpecial(n.into()),
            name: n.into(),
            detail: Arc::from("Person"),
        });
        let bare = trigger_at("@", 1).expect("a lone @ is a trigger");
        assert_eq!((bare.kind, bare.query.as_str()), (TriggerKind::Person, ""));
        assert_eq!(matches(&people, &bare.query, 8).len(), 3);
        let partial = trigger_at("hello @a", 8).expect("@ after a space");
        let names: Vec<_> = matches(&people, &partial.query, 8)
            .iter()
            .map(|c| c.name.to_string())
            .collect();
        assert_eq!(names, ["Ada", "Alan", "Grace"]);
    }
}
