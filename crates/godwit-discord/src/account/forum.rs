// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A forum's post list: the open posts from the store (the guild's active
//! threads), the archived ones from the thread search (see `crate::forum`).

use std::sync::Arc;

use godwit_core::forum::{ForumPage, ForumSort};

use super::*;
use crate::backend::RequestError;
use crate::forum as f;

/// Archived posts kept as rooms at most (a few pages of "Load older posts").
const MAX_OPENED_ARCHIVED: usize = 100;

impl DiscordAccount {
    /// Drops the archived posts loaded as rooms, except the one being
    /// viewed: they exist only so a post of the list can be opened, and
    /// would otherwise stay in the store and the core's rooms for good.
    pub(super) fn forget_archived(&mut self) {
        let keep = self.viewing.clone();
        let gone: Vec<String> = self
            .store
            .opened_archived
            .iter()
            .filter(|id| Some(*id) != keep.as_ref())
            .cloned()
            .collect();
        for id in gone {
            self.store.opened_archived.remove(&id);
            self.store.remove_channel(&id);
        }
    }

    /// The forum's tags and rules and its open posts, from what's known.
    pub(super) fn forum_open(&self, room: &RoomId, channel: &str, out: &mut Vec<Event>) {
        let Some(forum) = self.store.channels.get(channel).filter(|c| c.forum()) else {
            return;
        };
        let moderate = self.store.can(channel, p::MANAGE_THREADS);
        let posts = self
            .store
            .channels
            .values()
            .filter(|t| t.thread() && !t.archived && t.parent.as_deref() == Some(channel))
            .filter(|t| self.store.can(&t.id, p::VIEW_CHANNEL))
            .filter_map(|t| Some(f::post(t, self.room_id(&t.id)?, None)))
            .collect();
        out.push(Event::Forum(Box::new(ForumPage {
            room: room.clone(),
            info: Some(f::info(forum, moderate)),
            posts,
            archived: false,
            replace_older: false,
            sort: ForumSort::default(),
            tag: None,
            next_offset: None,
        })));
    }

    /// A page of archived posts: they become rooms (kept out of the sidebar)
    /// so a post opens like any other.
    pub(super) fn forum_older(
        &mut self,
        channel: &str,
        sort: ForumSort,
        tag: Option<String>,
        offset: usize,
        result: Result<Value, RequestError>,
        out: &mut Vec<Event>,
    ) {
        let Some(room) = self.room_id(channel) else { return };
        let shown_tag = tag.as_deref().map(Arc::<str>::from);
        let failed = |out: &mut Vec<Event>, why: String| {
            out.push(Event::Error(format!("Discord forum: {why}").into()));
            out.push(Event::ForumFailed {
                room: room.clone(),
                archived: true,
                sort,
                tag: shown_tag.clone(),
            });
        };
        let answer = match result {
            Ok(v) => v,
            Err(e) => return failed(out, e.to_string()),
        };
        let (threads, first, next_offset) = match f::page(&answer, offset) {
            f::Page::Indexing(secs) => return failed(out, format!("still indexing, try again in {secs}s")),
            f::Page::Found {
                threads,
                first,
                next_offset,
            } => (threads, first, next_offset),
        };
        // A list starting over (a new sort or filter, or the first load)
        // lets go of the posts of the last one, so they don't pile up.
        let mut replace_older = false;
        if offset == 0 {
            self.forget_archived();
        } else if self.store.opened_archived.len() + threads.len() > MAX_OPENED_ARCHIVED {
            // Over the cap: the earlier pages' rooms go, so the core's list
            // must drop them too (`replace_older`), or they'd list posts
            // that no longer open.
            self.forget_archived();
            replace_older = true;
        }
        let guild = self.store.channels.get(channel).and_then(|c| c.guild.clone());
        let mut posts = Vec::new();
        for thread in threads {
            let mut thread = thread.clone();
            if thread["guild_id"].is_null()
                && let Some(g) = &guild
            {
                thread["guild_id"] = Value::from(g.as_str());
            }
            let Some(id) = thread["id"].as_str().map(str::to_owned) else {
                continue;
            };
            self.store.put_channel(&thread);
            self.store.opened_archived.insert(id.clone());
            if let (Some(c), Some(r)) = (self.store.channels.get(&id), self.room_id(&id)) {
                posts.push(f::post(c, r, first.get(id.as_str()).copied()));
            }
        }
        self.sync_rooms(out);
        out.push(Event::Forum(Box::new(ForumPage {
            room,
            info: None,
            posts,
            archived: true,
            replace_older,
            sort,
            tag: shown_tag,
            next_offset,
        })));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The demo forum "40" in server "10": two open posts, three archived
    /// ones behind "Load older posts".
    fn demo() -> DiscordAccount {
        let mut ready = crate::store::test_ready();
        if let Some(g) = ready["guilds"].get_mut(0) {
            g["channels"].as_array_mut().expect("channels").push(json!({
                "id": "40", "type": 15, "name": "ideas", "position": 5, "flags": 16,
                "available_tags": [{"id": "1", "name": "Bug", "emoji_name": "🐛"}, {"id": "2", "name": "Done", "moderated": true}]
            }));
            g["threads"] = json!([
                {"id": "1793000000000000001", "type": 11, "guild_id": "10", "parent_id": "40", "name": "Crash",
                 "applied_tags": ["1"], "message_count": 4, "last_message_id": "1793000000000000009", "thread_metadata": {"archived": false}},
                {"id": "1793000000000000002", "type": 11, "guild_id": "10", "parent_id": "40", "name": "Idea",
                 "thread_metadata": {"archived": false}}
            ]);
        }
        let old = |id: &str, name: &str, tags: Value| {
            json!({"id": id, "type": 11, "guild_id": "10", "parent_id": "40", "name": name, "applied_tags": tags,
                   "message_count": 2, "last_message_id": id, "thread_metadata": {"archived": true}, "demo_first": format!("{name} text")})
        };
        let history = HashMap::from([(
            "archived:40".to_owned(),
            vec![
                old("1780000000000000001", "Old one", json!(["1"])),
                old("1780000000000000002", "Old two", json!([])),
                old("1780000000000000003", "Old three", json!(["1"])),
            ],
        )]);
        DiscordAccount::demo(ready, history).0
    }

    fn pages(out: &[Event]) -> Vec<&ForumPage> {
        out.iter()
            .filter_map(|e| match e {
                Event::Forum(p) => Some(&**p),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_demo_forum_lists_open_posts_with_tags_then_older_ones_on_request() {
        let mut a = demo();
        let room = a.room_id("40").expect("the forum");
        assert!(a.room("40").is_some_and(|r| r.capabilities.forum));
        let out = a.run(Command::LoadForum {
            room: room.clone(),
            sort: ForumSort::LatestActivity,
            tag: None,
            offset: None,
        });
        let page = pages(&out).remove(0);
        let info = page.info.as_ref().expect("the forum's rules");
        assert_eq!(
            (info.tags.len(), info.require_tag, info.moderate_tags),
            (2, true, false)
        );
        assert_eq!(page.posts.len(), 2);
        let crash = page.posts.iter().find(|p| &*p.title == "Crash").expect("a post");
        assert_eq!((crash.tags.len(), crash.replies), (1, 4));

        let out = a.run(Command::LoadForum {
            room: room.clone(),
            sort: ForumSort::LatestActivity,
            tag: None,
            offset: Some(0),
        });
        let page = pages(&out).remove(0);
        assert!(page.archived && page.next_offset.is_none());
        assert_eq!(page.posts.len(), 3);
        let one = page.posts.iter().find(|p| &*p.title == "Old one").expect("a post");
        assert_eq!(one.preview.as_deref(), Some("Old one text"));
        let opened = page.posts[0].room.raw.to_string();
        assert!(a.room(&opened).is_some(), "an archived post opens as a room");

        let out = a.run(Command::LoadForum {
            room,
            sort: ForumSort::CreationDate,
            tag: Some("1".into()),
            offset: Some(0),
        });
        assert_eq!(pages(&out).remove(0).posts.len(), 2, "the tag narrows the older posts");
    }
}
