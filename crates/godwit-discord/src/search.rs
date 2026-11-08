// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Message search as Discord's web client asks for it: Godwit's neutral
//! [`SearchQuery`] mapped to `GET /guilds/{g}/messages/search` (or a DM's own
//! `/channels/{c}/messages/search`), and the answer's paging. Parameter names
//! are from docs.discord.food, Message resource, "Search Guild Messages";
//! Yapfast's `yapfast-discord/src/api.rs` `search` sends them the same way.

use chrono::{Duration, Local, NaiveDate, TimeZone};
use godwit_core::search::{SearchDate, SearchHas, SearchQuery, SearchScope};
use serde_json::Value;

use crate::api::{DISCORD_EPOCH_MS, encode};

/// Matches Discord returns per page.
pub const PAGE: usize = 25;
/// Discord takes no offset past this.
const MAX_OFFSET: usize = 9975;

/// The query string's parts, in Discord's words.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchParams {
    pub content: String,
    pub author_id: Option<String>,
    pub mentions: Option<String>,
    pub has: Vec<&'static str>,
    pub min_id: Option<String>,
    pub max_id: Option<String>,
    pub pinned: bool,
    pub channel_id: Option<String>,
    pub include_nsfw: bool,
    pub offset: usize,
}

impl SearchParams {
    /// Maps a neutral query. `channel` is where the search was opened (a
    /// server's search is narrowed to it unless the scope is the whole
    /// server), `in_channel` the raw id `in:#channel` named.
    pub fn from_query(
        q: &SearchQuery,
        guild: bool,
        channel: &str,
        in_channel: Option<String>,
        nsfw: bool,
        offset: usize,
    ) -> Self {
        let channel_id = if !guild {
            None
        } else if in_channel.is_some() {
            in_channel
        } else if q.scope == SearchScope::Channel {
            Some(channel.to_owned())
        } else {
            None
        };
        // after:D is past the end of D, during:D the day itself, before:D
        // before its start (the client's search bar does the same).
        let mins = [
            q.after.and_then(|d| day_start(d, 1)),
            q.during.and_then(|d| day_start(d, 0)),
        ];
        let maxs = [
            q.before.and_then(|d| day_start(d, 0)),
            q.during.and_then(|d| day_start(d, 1)),
        ];
        Self {
            content: q.text.clone(),
            author_id: q.from.as_ref().map(|u| u.raw.to_string()),
            mentions: q.mentions.as_ref().map(|u| u.raw.to_string()),
            has: q.has.iter().map(|h| has_word(*h)).collect(),
            min_id: mins.into_iter().flatten().max().map(snowflake_at),
            max_id: maxs.into_iter().flatten().min().map(snowflake_at),
            pinned: q.pinned,
            channel_id,
            include_nsfw: nsfw,
            offset,
        }
    }

    /// The request path with its query string.
    pub fn path(&self, guild: Option<&str>, channel: &str) -> String {
        let mut parts = Vec::new();
        if !self.content.is_empty() {
            parts.push(format!("content={}", encode(&self.content)));
        }
        if let Some(v) = &self.author_id {
            parts.push(format!("author_id={v}"));
        }
        if let Some(v) = &self.mentions {
            parts.push(format!("mentions={v}"));
        }
        parts.extend(self.has.iter().map(|h| format!("has={h}")));
        if let Some(v) = &self.min_id {
            parts.push(format!("min_id={v}"));
        }
        if let Some(v) = &self.max_id {
            parts.push(format!("max_id={v}"));
        }
        if self.pinned {
            parts.push("pinned=true".into());
        }
        if let Some(v) = &self.channel_id {
            parts.push(format!("channel_id={v}"));
        }
        if self.include_nsfw {
            parts.push("include_nsfw=true".into());
        }
        parts.push(format!("offset={}", self.offset.min(MAX_OFFSET)));
        let query = parts.join("&");
        match guild {
            Some(guild) => format!("/guilds/{guild}/messages/search?{query}"),
            None => format!("/channels/{channel}/messages/search?{query}"),
        }
    }
}

fn has_word(h: SearchHas) -> &'static str {
    match h {
        SearchHas::Link => "link",
        SearchHas::Embed => "embed",
        SearchHas::File => "file",
        SearchHas::Video => "video",
        SearchHas::Image => "image",
        SearchHas::Sound => "sound",
        SearchHas::Sticker => "sticker",
        SearchHas::Poll => "poll",
        SearchHas::Forward => "snapshot",
    }
}

/// The start (Unix ms) of the reader's local day `date` plus `days`.
fn day_start(date: SearchDate, days: i64) -> Option<i64> {
    let day = NaiveDate::from_ymd_opt(date.year, u32::from(date.month), u32::from(date.day))?
        .checked_add_signed(Duration::days(days))?;
    // A day whose midnight doesn't exist (a clock change) starts an hour on.
    let start = day.and_hms_opt(0, 0, 0)?;
    let local = Local
        .from_local_datetime(&start)
        .earliest()
        .or_else(|| Local.from_local_datetime(&(start + Duration::hours(1))).earliest())?;
    Some(local.timestamp_millis())
}

/// The snowflake of a moment: `(ms - epoch) << 22`, what `min_id` and
/// `max_id` take (before the epoch, 0).
pub fn snowflake_at(ms: i64) -> String {
    let since = u64::try_from(ms).unwrap_or(0).saturating_sub(DISCORD_EPOCH_MS);
    (since << 22).to_string()
}

/// What an answer says.
#[derive(Debug, PartialEq)]
pub enum Page<'a> {
    /// 202: still indexing this chat; ask again in this many seconds.
    Indexing(u64),
    Found {
        /// The groups of messages (each holds the match).
        groups: &'a [Value],
        total: Option<u64>,
        /// Where the next page starts.
        next_offset: Option<usize>,
    },
}

pub fn page(answer: &Value, offset: usize) -> Page<'_> {
    if answer["messages"].is_null() && !answer["retry_after"].is_null() {
        let secs = answer["retry_after"].as_f64().unwrap_or(5.0).ceil().max(1.0);
        return Page::Indexing(secs as u64);
    }
    let groups = answer["messages"].as_array().map_or(&[][..], Vec::as_slice);
    let total = answer["total_results"].as_u64();
    let next = offset + groups.len();
    let more = !groups.is_empty() && total.is_none_or(|t| (next as u64) < t) && next <= MAX_OFFSET;
    Page::Found {
        groups,
        total,
        next_offset: more.then_some(next),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use godwit_core::ids::{AccountId, UserId};
    use godwit_core::network::Network;
    use serde_json::json;

    fn user(raw: &str) -> UserId {
        UserId::new(AccountId::new(Network::Discord, "me"), raw)
    }

    #[test]
    fn a_day_becomes_snowflakes_in_the_local_day() {
        // 2015-01-01 00:00:00 UTC is the epoch: id 0; a second later 1000 << 22.
        assert_eq!(snowflake_at(1_420_070_400_000), "0");
        assert_eq!(snowflake_at(1_420_070_401_000), (1000u64 << 22).to_string());
        assert_eq!(snowflake_at(0), "0");
        let d = SearchDate {
            year: 2026,
            month: 11,
            day: 9,
        };
        let (a, b) = (day_start(d, 0).unwrap(), day_start(d, 1).unwrap());
        // A local day is a day (23 to 25 hours around clock changes).
        assert!((23 * 3_600_000..=25 * 3_600_000).contains(&(b - a)));
    }

    #[test]
    fn filters_become_query_params() {
        let q = SearchQuery {
            text: "lunch & more".into(),
            from: Some(user("11")),
            mentions: Some(user("22")),
            has: vec![SearchHas::Link, SearchHas::Forward],
            before: SearchDate::parse("2026-11-09"),
            after: SearchDate::parse("2026-01-01"),
            pinned: true,
            ..SearchQuery::default()
        };
        let p = SearchParams::from_query(&q, true, "99", None, false, 25);
        assert_eq!(p.channel_id.as_deref(), Some("99"), "a channel search is narrowed");
        let path = p.path(Some("5"), "99");
        assert!(path.starts_with("/guilds/5/messages/search?content=lunch%20%26%20more&author_id=11&mentions=22"));
        assert!(path.contains("&has=link&has=snapshot&min_id="));
        assert!(path.contains("&max_id=") && path.contains("&pinned=true&channel_id=99&offset=25"));
        assert!(p.min_id.is_some() && p.max_id.is_some());
        // The whole server has no channel; a DM has its own path and none.
        let wide = SearchQuery {
            scope: SearchScope::Server,
            ..SearchQuery::text_only("x")
        };
        let p = SearchParams::from_query(&wide, true, "99", None, true, 0);
        assert_eq!(
            p.path(Some("5"), "99"),
            "/guilds/5/messages/search?content=x&include_nsfw=true&offset=0"
        );
        let p = SearchParams::from_query(&wide, false, "99", None, false, 0);
        assert_eq!(p.path(None, "99"), "/channels/99/messages/search?content=x&offset=0");
        let inn = SearchParams::from_query(&wide, true, "99", Some("7".into()), false, 0);
        assert_eq!(inn.channel_id.as_deref(), Some("7"));
    }

    #[test]
    fn during_spans_one_day_and_after_starts_the_next() {
        let day = SearchDate::parse("2026-03-10");
        let during = SearchQuery {
            during: day,
            ..SearchQuery::default()
        };
        let p = SearchParams::from_query(&during, false, "1", None, false, 0);
        let (min, max): (u64, u64) = (p.min_id.unwrap().parse().unwrap(), p.max_id.unwrap().parse().unwrap());
        assert!(max > min);
        let after = SearchQuery {
            after: day,
            ..SearchQuery::default()
        };
        let p = SearchParams::from_query(&after, false, "1", None, false, 0);
        assert_eq!(
            p.min_id.unwrap().parse::<u64>().unwrap(),
            max,
            "after the day = the day's end"
        );
    }

    #[test]
    fn answers_page_and_indexing_is_told() {
        let full: Vec<Value> = (0..PAGE).map(|_| json!([{"id": "1"}])).collect();
        let answer = json!({"total_results": 60, "messages": full});
        let Page::Found { total, next_offset, .. } = page(&answer, 0) else {
            panic!("found")
        };
        assert_eq!((total, next_offset), (Some(60), Some(25)));
        let Page::Found { next_offset, .. } = page(&answer, 35) else {
            panic!("found")
        };
        assert_eq!(next_offset, None, "60 results end at the third page");
        assert_eq!(
            page(&json!({"retry_after": 2.3, "documents_indexed": 0}), 0),
            Page::Indexing(3)
        );
        let empty = json!({"total_results": 0, "messages": []});
        let Page::Found {
            groups, next_offset, ..
        } = page(&empty, 0)
        else {
            panic!("found")
        };
        assert!(groups.is_empty() && next_offset.is_none());
    }
}
