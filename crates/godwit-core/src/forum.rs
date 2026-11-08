// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A forum's post list: network-neutral. A forum is a room with no messages
//! of its own whose posts are threads (rooms under it); this is what its list
//! shows of them (tags, a preview, counts), how it sorts and filters, and
//! which older (archived) posts are still to load. Discord's forums are the
//! first; the rules (a required tag, moderated tags) are Discord's, and
//! another network with forums fills the same model.

use std::sync::Arc;

use crate::ids::RoomId;

/// A tag a forum's posts may take.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForumTag {
    pub id: Arc<str>,
    pub name: Arc<str>,
    /// Its emoji as text (a Unicode emoji); None when it has none, or a
    /// custom one that can't be drawn as text.
    pub emoji: Option<Arc<str>>,
    /// Only those who manage the forum's threads may give it.
    pub moderated: bool,
}

/// How a forum's posts are ordered. The official client's "Sort by".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ForumSort {
    /// "Recent activity": the post with the newest message first.
    #[default]
    LatestActivity,
    /// "Creation date": the newest post first.
    CreationDate,
}

impl ForumSort {
    pub const ALL: [Self; 2] = [Self::LatestActivity, Self::CreationDate];

    pub fn label(self) -> &'static str {
        match self {
            Self::LatestActivity => "Recent activity",
            Self::CreationDate => "Creation date",
        }
    }
}

/// What a forum offers when posting and filtering.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForumInfo {
    /// In the forum's own order.
    pub tags: Vec<ForumTag>,
    /// A post must take at least one tag.
    pub require_tag: bool,
    /// The sort the forum itself asks for.
    pub default_sort: ForumSort,
    /// We may give moderated tags (Discord: Manage Threads).
    pub moderate_tags: bool,
}

/// Most tags a post takes (docs.discord.food, Channel, `applied_tags`).
pub const MAX_POST_TAGS: usize = 5;

/// One post in a forum's list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumPost {
    /// The post's own room.
    pub room: RoomId,
    pub title: Arc<str>,
    /// Applied tags, by id.
    pub tags: Vec<Arc<str>>,
    /// The start of its first message, when the network says it.
    pub preview: Option<Arc<str>>,
    /// Messages in the post besides the first.
    pub replies: u32,
    /// Unix milliseconds; 0 when not known.
    pub created: i64,
    pub last_activity: i64,
    /// Closed: loaded by "Load older posts".
    pub archived: bool,
}

/// One answer for a forum's list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForumPage {
    pub room: RoomId,
    /// The forum's tags and rules, when the network sends them (the first
    /// page does).
    pub info: Option<ForumInfo>,
    pub posts: Vec<ForumPost>,
    /// These are older (archived) posts for `sort` and `tag`, not the open ones.
    pub archived: bool,
    /// The network let go of the older posts loaded before (so they no
    /// longer open): this page replaces them.
    pub replace_older: bool,
    pub sort: ForumSort,
    pub tag: Option<Arc<str>>,
    /// Where the next older page starts; None: that was all. Only for
    /// archived pages.
    pub next_offset: Option<usize>,
}

/// The forum list for the selected forum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Forum {
    pub room: RoomId,
    /// None until the first answer.
    pub info: Option<ForumInfo>,
    pub sort: ForumSort,
    /// The sort is ours, not the forum's default (so the default no longer
    /// overrides it).
    pub sort_chosen: bool,
    /// Show only posts with this tag.
    pub tag: Option<Arc<str>>,
    pub posts: Vec<ForumPost>,
    /// Where the next page of older posts starts; None once all are loaded.
    pub older: Option<usize>,
    pub loading_older: bool,
    pub failed: bool,
}

impl Forum {
    pub fn new(room: RoomId) -> Self {
        Self {
            room,
            info: None,
            sort: ForumSort::default(),
            sort_chosen: false,
            tag: None,
            posts: Vec::new(),
            older: Some(0),
            loading_older: false,
            failed: false,
        }
    }

    /// Takes an answer in; a stale one (for a sort or filter no longer
    /// shown) is dropped.
    pub fn apply(&mut self, page: ForumPage) {
        if page.room != self.room {
            return;
        }
        if let Some(info) = page.info {
            if !self.sort_chosen {
                self.sort = info.default_sort;
            }
            self.info = Some(info);
        }
        self.failed = false;
        if page.archived {
            if page.sort != self.sort || page.tag != self.tag {
                return;
            }
            self.loading_older = false;
            self.older = page.next_offset;
            if page.replace_older {
                self.posts.retain(|p| !p.archived);
            }
        } else {
            // A fresh open list replaces the old one; older posts stay.
            self.posts.retain(|p| p.archived);
        }
        for post in page.posts {
            match self.posts.iter_mut().find(|p| p.room == post.room) {
                Some(have) => {
                    // An open post wins over its archived copy.
                    if !(post.archived && !have.archived) {
                        *have = post;
                    }
                }
                None => self.posts.push(post),
            }
        }
    }

    /// A load failed. `archived`: it was for older posts in `sort` with
    /// `tag`; one for a sort or filter no longer shown is dropped (false).
    pub fn fail(&mut self, archived: bool, sort: ForumSort, tag: Option<Arc<str>>) -> bool {
        if archived && (sort != self.sort || tag != self.tag) {
            return false;
        }
        self.failed = true;
        self.loading_older = false;
        true
    }

    /// Another order; the older posts load again in it.
    pub fn set_sort(&mut self, sort: ForumSort) {
        self.sort = sort;
        self.sort_chosen = true;
        self.reset_older();
    }

    /// Another filter; the older posts load again with it.
    pub fn set_tag(&mut self, tag: Option<Arc<str>>) {
        self.tag = tag;
        self.reset_older();
    }

    fn reset_older(&mut self) {
        self.posts.retain(|p| !p.archived);
        self.older = Some(0);
        self.loading_older = false;
    }

    /// The posts as listed: filtered by the tag, in the sort's order.
    pub fn visible(&self) -> Vec<&ForumPost> {
        let mut list: Vec<&ForumPost> = self
            .posts
            .iter()
            .filter(|p| self.tag.as_ref().is_none_or(|t| p.tags.contains(t)))
            .collect();
        let key = |p: &ForumPost| match self.sort {
            ForumSort::LatestActivity => p.last_activity.max(p.created),
            ForumSort::CreationDate => p.created,
        };
        list.sort_by_key(|p| std::cmp::Reverse(key(p)));
        list
    }

    /// The tags a new post may carry out of `wanted`: the forum's own, and
    /// moderated ones only for those who may; None when the forum needs a
    /// tag and none is left.
    pub fn post_tags(&self, wanted: &[Arc<str>]) -> Option<Vec<Arc<str>>> {
        let Some(info) = &self.info else {
            return Some(Vec::new());
        };
        let mut tags: Vec<Arc<str>> = Vec::new();
        for id in wanted {
            let allowed = info
                .tags
                .iter()
                .any(|t| t.id == *id && (!t.moderated || info.moderate_tags));
            if allowed && !tags.contains(id) && tags.len() < MAX_POST_TAGS {
                tags.push(id.clone());
            }
        }
        (!(info.require_tag && tags.is_empty())).then_some(tags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{AccountId, RoomId};
    use crate::network::Network;

    fn room(raw: &str) -> RoomId {
        RoomId::new(AccountId::new(Network::Discord, "me"), raw)
    }

    fn post(raw: &str, tags: &[&str], created: i64, last: i64, archived: bool) -> ForumPost {
        ForumPost {
            room: room(raw),
            title: raw.into(),
            tags: tags.iter().map(|t| Arc::<str>::from(*t)).collect(),
            preview: None,
            replies: 0,
            created,
            last_activity: last,
            archived,
        }
    }

    fn info(require: bool, moderate: bool) -> ForumInfo {
        let tag = |id: &str, moderated| ForumTag {
            id: id.into(),
            name: id.into(),
            emoji: None,
            moderated,
        };
        ForumInfo {
            tags: vec![tag("bug", false), tag("done", true)],
            require_tag: require,
            default_sort: ForumSort::CreationDate,
            moderate_tags: moderate,
        }
    }

    fn page(posts: Vec<ForumPost>, archived: bool, next: Option<usize>) -> ForumPage {
        ForumPage {
            room: room("f"),
            info: None,
            posts,
            archived,
            replace_older: false,
            sort: ForumSort::LatestActivity,
            tag: None,
            next_offset: next,
        }
    }

    #[test]
    fn sorts_by_activity_or_creation() {
        let mut f = Forum::new(room("f"));
        f.apply(page(
            vec![post("a", &[], 10, 50, false), post("b", &[], 20, 30, false)],
            false,
            None,
        ));
        let order = |f: &Forum| f.visible().iter().map(|p| p.title.to_string()).collect::<Vec<_>>();
        assert_eq!(order(&f), ["a", "b"]);
        f.set_sort(ForumSort::CreationDate);
        assert_eq!(order(&f), ["b", "a"]);
    }

    #[test]
    fn the_forums_default_sort_applies_until_we_choose() {
        let mut f = Forum::new(room("f"));
        let mut p = page(Vec::new(), false, None);
        p.info = Some(info(false, false));
        f.apply(p.clone());
        assert_eq!(f.sort, ForumSort::CreationDate);
        f.set_sort(ForumSort::LatestActivity);
        f.apply(p);
        assert_eq!(f.sort, ForumSort::LatestActivity);
    }

    #[test]
    fn filters_by_tag_and_pages_in_older_posts() {
        let mut f = Forum::new(room("f"));
        f.apply(page(
            vec![post("a", &["bug"], 1, 1, false), post("b", &[], 2, 2, false)],
            false,
            None,
        ));
        assert_eq!(f.older, Some(0));
        f.apply(page(vec![post("c", &["bug"], 0, 0, true)], true, Some(25)));
        assert_eq!(f.older, Some(25));
        f.set_tag(Some("bug".into()));
        assert_eq!(f.visible().len(), 1, "the filter drops b and the archived ones");
        assert_eq!(f.older, Some(0), "older posts load again with the filter");
        // An answer for the previous filter is dropped.
        f.apply(page(vec![post("d", &[], 0, 0, true)], true, None));
        assert_eq!(f.older, Some(0));
        f.apply(ForumPage {
            tag: Some("bug".into()),
            ..page(vec![post("c", &["bug"], 0, 0, true)], true, None)
        });
        assert_eq!(f.older, None);
        assert_eq!(f.visible().len(), 2);
    }

    #[test]
    fn a_required_tag_and_moderated_tags_gate_new_posts() {
        let mut f = Forum::new(room("f"));
        f.info = Some(info(true, false));
        assert_eq!(f.post_tags(&[]), None, "a tag is required");
        assert_eq!(f.post_tags(&["done".into()]), None, "moderated, and we can't");
        assert_eq!(f.post_tags(&["bug".into(), "nope".into()]), Some(vec!["bug".into()]));
        f.info = Some(info(true, true));
        assert_eq!(f.post_tags(&["done".into()]), Some(vec!["done".into()]));
        f.info = Some(info(false, false));
        assert_eq!(f.post_tags(&[]), Some(Vec::new()));
        let many: Vec<Arc<str>> = (0..9).map(|i| Arc::from(format!("t{i}"))).collect();
        f.info = Some(ForumInfo {
            tags: many
                .iter()
                .map(|id| ForumTag {
                    id: id.clone(),
                    name: id.clone(),
                    ..Default::default()
                })
                .collect(),
            ..info(false, false)
        });
        assert_eq!(f.post_tags(&many).map(|t| t.len()), Some(MAX_POST_TAGS));
    }

    #[test]
    fn a_page_can_replace_the_older_posts_the_network_let_go_of() {
        let mut f = Forum::new(room("f"));
        f.apply(page(vec![post("a", &[], 1, 1, false)], false, None));
        f.apply(page(vec![post("b", &[], 0, 0, true)], true, Some(1)));
        f.apply(ForumPage {
            replace_older: true,
            ..page(vec![post("c", &[], 0, 0, true)], true, Some(2))
        });
        let titles: Vec<_> = f.posts.iter().map(|p| p.title.to_string()).collect();
        assert_eq!(titles, ["a", "c"], "the open post stays, b is gone");
    }

    #[test]
    fn a_stale_page_or_failure_is_dropped() {
        let mut f = Forum::new(room("f"));
        f.loading_older = true;
        f.set_tag(Some("bug".into()));
        f.loading_older = true;
        // For the sort and filter before the change.
        f.apply(page(vec![post("c", &[], 0, 0, true)], true, None));
        assert!(f.posts.is_empty() && f.loading_older);
        assert!(!f.fail(true, ForumSort::LatestActivity, None));
        assert!(f.loading_older, "a stale failure leaves the load running");
        assert!(f.fail(true, ForumSort::LatestActivity, Some("bug".into())));
        assert!(f.failed && !f.loading_older);
    }
}
