// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Forum channels as Discord's web client shows them: the tags a channel
//! offers (`available_tags`), what a post (a thread) carries (`applied_tags`,
//! `message_count`), and the search that lists a forum's archived posts,
//! `GET /channels/{forum}/threads/search?archived=true...` (docs.discord.food,
//! Channel resource, "Search Threads"; Yapfast's
//! `yapfast-discord/src/api.rs` `search_threads` and `forum.rs` send and read
//! the same). Mapped to `godwit_core::forum`; the default layout (list or
//! gallery) is ignored, Godwit lists posts only.

use std::collections::HashMap;
use std::sync::Arc;

use godwit_core::forum::{ForumInfo, ForumPost, ForumSort, ForumTag};
use godwit_core::ids::RoomId;
use serde_json::Value;

use crate::api::snowflake_ms;
use crate::store::Channel;

/// Threads a search page holds at most (the API's own limit).
pub const THREAD_PAGE: usize = 25;
/// A forum's flag that each post takes a tag (`1 << 4`, "REQUIRE_TAG").
const REQUIRE_TAG: u64 = 1 << 4;
/// `default_sort_order` 1: by creation date (0: latest activity).
const CREATION_DATE: u64 = 1;
/// Discord won't take an offset past this.
const MAX_OFFSET: usize = 9975;
/// How much of a post's first message the list shows.
const PREVIEW_CHARS: usize = 160;

/// A channel's `available_tags`. The emoji is the tag's Unicode one; a custom
/// emoji (`emoji_id`) has no text form, so such a tag shows its name only.
pub fn tags(channel: &Value) -> Vec<ForumTag> {
    channel["available_tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tag| {
            Some(ForumTag {
                id: tag["id"].as_str()?.into(),
                name: tag["name"].as_str().unwrap_or_default().into(),
                emoji: match (tag["emoji_id"].as_str(), tag["emoji_name"].as_str()) {
                    (None, Some(name)) if !name.is_empty() => Some(name.into()),
                    _ => None,
                },
                moderated: tag["moderated"].as_bool().unwrap_or(false),
            })
        })
        .collect()
}

/// What the forum offers; `moderate`: we have Manage Threads there.
pub fn info(c: &Channel, moderate: bool) -> ForumInfo {
    ForumInfo {
        tags: c.tags.clone(),
        require_tag: c.flags & REQUIRE_TAG != 0,
        default_sort: if c.default_sort == CREATION_DATE {
            ForumSort::CreationDate
        } else {
            ForumSort::LatestActivity
        },
        moderate_tags: moderate,
    }
}

/// A post's list entry from its thread channel, and its first message when
/// known (the search answers with them).
pub fn post(thread: &Channel, room: RoomId, first: Option<&Value>) -> ForumPost {
    ForumPost {
        room,
        title: thread.name.as_str().into(),
        tags: thread.applied_tags.iter().map(|t| Arc::from(t.as_str())).collect(),
        preview: first.and_then(|m| preview(m["content"].as_str()?)),
        replies: thread.message_count,
        // A post's id is its creation moment, as every snowflake.
        created: snowflake_ms(&thread.id),
        last_activity: snowflake_ms(thread.last_message_id.as_deref().unwrap_or(&thread.id)),
        archived: thread.archived,
    }
}

/// The start of a message, on one line.
fn preview(content: &str) -> Option<Arc<str>> {
    let line = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return None;
    }
    let mut cut: String = line.chars().take(PREVIEW_CHARS).collect();
    if cut.len() < line.len() {
        cut.push('…');
    }
    Some(cut.into())
}

/// The search's `sort_by` for a sort order.
pub fn sort_by(sort: ForumSort) -> &'static str {
    match sort {
        ForumSort::LatestActivity => "last_message_time",
        ForumSort::CreationDate => "creation_time",
    }
}

/// What a thread search answered.
#[derive(Debug, PartialEq)]
pub enum Page<'a> {
    /// 202 with `retry_after`: the forum isn't indexed yet (seconds to wait).
    Indexing(u64),
    Found {
        threads: &'a [Value],
        /// Each thread's first message, by thread id (thread-only channels).
        first: HashMap<&'a str, &'a Value>,
        /// Where the next page starts; None: that was all.
        next_offset: Option<usize>,
    },
}

/// A search answer for the page asked from `offset`.
pub fn page(answer: &Value, offset: usize) -> Page<'_> {
    if answer["threads"].is_null() && !answer["retry_after"].is_null() {
        let secs = answer["retry_after"].as_f64().unwrap_or(5.0).ceil().max(1.0);
        return Page::Indexing(secs as u64);
    }
    let threads = answer["threads"].as_array().map_or(&[][..], Vec::as_slice);
    let first = answer["first_messages"]
        .as_array()
        .into_iter()
        .flatten()
        // A first message's channel is its thread.
        .filter_map(|m| Some((m["channel_id"].as_str()?, m)))
        .collect();
    let next = offset + threads.len();
    let more = !threads.is_empty() && answer["has_more"].as_bool().unwrap_or(false) && next <= MAX_OFFSET;
    Page::Found {
        threads,
        first,
        next_offset: more.then_some(next),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn forum() -> Value {
        json!({"id": "40", "type": 15, "name": "ideas", "flags": 16, "default_sort_order": 1,
        "available_tags": [
            {"id": "1", "name": "Bug", "emoji_name": "🐛", "emoji_id": null, "moderated": false},
            {"id": "2", "name": "Done", "moderated": true},
            {"id": "3", "name": "Custom", "emoji_id": "999", "emoji_name": "blob"}
        ]})
    }

    #[test]
    fn parses_tags_and_the_forums_rules() {
        let t = tags(&forum());
        assert_eq!(t.len(), 3);
        assert_eq!(
            (t[0].name.as_ref(), t[0].emoji.as_deref(), t[0].moderated),
            ("Bug", Some("🐛"), false)
        );
        assert!(t[1].moderated && t[1].emoji.is_none());
        assert!(t[2].emoji.is_none(), "a custom emoji has no text form");
        let c = Channel::from(&forum(), Some("10")).expect("a channel");
        let i = info(&c, false);
        assert!(i.require_tag && !i.moderate_tags);
        assert_eq!(i.default_sort, ForumSort::CreationDate);
        assert!(!info(&Channel::default(), true).require_tag);
    }

    #[test]
    fn a_post_carries_its_tags_replies_and_preview() {
        let thread = json!({"id": "1793000000000000000", "type": 11, "name": "Crash", "parent_id": "40",
            "applied_tags": ["1", "2"], "message_count": 7, "thread_metadata": {"archived": true}});
        let c = Channel::from(&thread, Some("10")).expect("a thread");
        let room = RoomId::new(
            godwit_core::ids::AccountId::new(godwit_core::network::Network::Discord, "me"),
            "x",
        );
        let first = json!({"content": "  It crashes\n on  start  "});
        let p = post(&c, room, Some(&first));
        assert_eq!(p.tags.iter().map(|t| &**t).collect::<Vec<_>>(), ["1", "2"]);
        assert_eq!((p.replies, p.archived), (7, true));
        assert_eq!(p.preview.as_deref(), Some("It crashes on start"));
        assert!(p.created > 0);
    }

    #[test]
    fn pages_continue_while_discord_has_more() {
        let a = json!({"threads": [{"id": "1"}, {"id": "2"}], "has_more": true, "total_results": 50,
            "first_messages": [{"channel_id": "1", "content": "hi"}]});
        let Page::Found {
            threads,
            first,
            next_offset,
        } = page(&a, 25)
        else {
            panic!("found")
        };
        assert_eq!((threads.len(), next_offset), (2, Some(27)));
        assert!(first.contains_key("1"));
        let done = json!({"threads": [{"id": "1"}], "has_more": false});
        assert!(matches!(page(&done, 0), Page::Found { next_offset: None, .. }));
        let empty = json!({"threads": [], "has_more": true});
        assert!(matches!(page(&empty, 0), Page::Found { next_offset: None, .. }));
        let indexing = json!({"message": "Index not yet available", "retry_after": 2});
        assert_eq!(page(&indexing, 0), Page::Indexing(2));
    }

    #[test]
    fn sorts_map_to_the_searchs_names() {
        assert_eq!(sort_by(ForumSort::LatestActivity), "last_message_time");
        assert_eq!(sort_by(ForumSort::CreationDate), "creation_time");
    }
}
