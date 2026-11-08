// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Searching a server's members by name, as the web client does for its
//! @mention and member searches: gateway op 8 REQUEST_GUILD_MEMBERS with a
//! `query`, answered by GUILD_MEMBERS_CHUNK carrying the same `nonce`
//! (Yapfast: `yapfast-discord/src/service/member_search.rs`, `gateway.rs`
//! `search_members`; docs.discord.food, Gateway: Request Guild Members).
//!
//! At most one search is in flight; a newer query waits (only the latest is
//! kept) and goes out when the answer arrives, whose now-stale result is
//! dropped. Gateway sends are paced by the connection's own send budget.

use std::time::{Duration, Instant};

use godwit_core::ids::RoomId;
use serde_json::{Value, json};

/// The gateway opcode (docs.discord.food, Gateway Opcodes: 8).
pub const REQUEST_GUILD_MEMBERS: u64 = 8;
/// The most members one search asks for (Yapfast's `FOUND`).
pub const FOUND: u64 = 10;
/// A search unanswered this long is given up, so a dropped request (Discord
/// can rate-limit a request without a chunk) doesn't block the next.
const PATIENCE: Duration = Duration::from_secs(15);

/// A search to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Search {
    pub room: RoomId,
    pub guild: String,
    pub query: String,
    pub nonce: String,
}

impl Search {
    /// The op 8 payload.
    pub fn payload(&self) -> Value {
        // The web client limits the query's length; the docs give no number,
        // so 100 characters is a safe cut.
        let query: String = self.query.chars().take(100).collect();
        json!({
            "guild_id": [self.guild],
            "query": query,
            "limit": FOUND,
            "presences": false,
            "nonce": self.nonce,
        })
    }
}

/// The result of an answered search.
#[derive(Debug, PartialEq, Eq)]
pub struct Answered {
    pub room: RoomId,
    pub query: String,
    /// A newer query is waiting, so this answer isn't wanted any more.
    pub stale: bool,
    /// The waiting query, to send now.
    pub next: Option<Search>,
}

#[derive(Default)]
pub struct SearchGate {
    counter: u64,
    flying: Option<(Search, Instant)>,
    waiting: Option<(RoomId, String, String)>,
}

impl SearchGate {
    /// The user's query for `guild`: Some when it should be sent now.
    pub fn ask(&mut self, now: Instant, room: &RoomId, guild: &str, query: &str) -> Option<Search> {
        if let Some((flying, since)) = &self.flying {
            if now.duration_since(*since) < PATIENCE {
                if flying.guild != guild || flying.query != query {
                    self.waiting = Some((room.clone(), guild.to_owned(), query.to_owned()));
                } else {
                    self.waiting = None;
                }
                return None;
            }
            self.flying = None;
        }
        self.waiting = None;
        Some(self.start(now, room.clone(), guild.to_owned(), query.to_owned()))
    }

    fn start(&mut self, now: Instant, room: RoomId, guild: String, query: String) -> Search {
        self.counter += 1;
        let search = Search {
            room,
            guild,
            query,
            nonce: format!("godwit-search-{}", self.counter),
        };
        self.flying = Some((search.clone(), now));
        search
    }

    /// The chunk with `nonce` arrived: what it answers, if it's ours.
    pub fn answer(&mut self, now: Instant, nonce: &str) -> Option<Answered> {
        if self.flying.as_ref().is_none_or(|(s, _)| s.nonce != nonce) {
            return None;
        }
        let (done, _) = self.flying.take()?;
        let waiting = self.waiting.take();
        Some(Answered {
            stale: waiting.is_some(),
            next: waiting.map(|(room, guild, query)| self.start(now, room, guild, query)),
            room: done.room,
            query: done.query,
        })
    }
}

/// A GUILD_MEMBERS_CHUNK's guild, nonce and members.
pub fn parse_chunk(d: &Value) -> Option<(&str, Option<&str>, &[Value])> {
    Some((
        d["guild_id"].as_str()?,
        d["nonce"].as_str(),
        d["members"].as_array().map_or(&[][..], Vec::as_slice),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use godwit_core::ids::AccountId;
    use godwit_core::network::Network;

    fn room() -> RoomId {
        RoomId::new(AccountId::new(Network::Discord, "me"), "c1")
    }

    #[test]
    fn payload_is_op_8_with_a_nonce_and_no_presences() {
        let mut gate = SearchGate::default();
        let s = gate.ask(Instant::now(), &room(), "g1", "ali").unwrap();
        assert_eq!(
            s.payload(),
            json!({"guild_id": ["g1"], "query": "ali", "limit": 10, "presences": false, "nonce": s.nonce})
        );
    }

    #[test]
    fn one_in_flight_and_only_the_latest_waits() {
        let (mut gate, now) = (SearchGate::default(), Instant::now());
        let first = gate.ask(now, &room(), "g1", "a").unwrap();
        assert!(gate.ask(now, &room(), "g1", "al").is_none());
        assert!(gate.ask(now, &room(), "g1", "ali").is_none());
        let done = gate.answer(now, &first.nonce).unwrap();
        assert!(done.stale, "a newer query superseded it");
        let next = done.next.unwrap();
        assert_eq!(next.query, "ali");
        let done = gate.answer(now, &next.nonce).unwrap();
        assert!(!done.stale && done.next.is_none());
    }

    #[test]
    fn answers_match_by_nonce_only() {
        let (mut gate, now) = (SearchGate::default(), Instant::now());
        let s = gate.ask(now, &room(), "g1", "a").unwrap();
        assert!(gate.answer(now, "other").is_none());
        assert!(gate.answer(now, &s.nonce).is_some());
        assert!(gate.answer(now, &s.nonce).is_none(), "answered once");
    }

    #[test]
    fn a_search_never_answered_is_given_up() {
        let (mut gate, now) = (SearchGate::default(), Instant::now());
        gate.ask(now, &room(), "g1", "a").unwrap();
        assert!(gate.ask(now + Duration::from_secs(5), &room(), "g1", "b").is_none());
        assert!(gate.ask(now + PATIENCE, &room(), "g1", "b").is_some());
    }

    #[test]
    fn chunks_parse() {
        let d = json!({"guild_id": "g1", "nonce": "n", "members": [{"user": {"id": "1"}}]});
        let (g, n, m) = parse_chunk(&d).unwrap();
        assert_eq!((g, n, m.len()), ("g1", Some("n"), 1));
        assert!(parse_chunk(&json!({})).is_none());
    }
}
