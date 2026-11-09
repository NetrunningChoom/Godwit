// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Search, pins and threads.

use super::*;

fn search_completion_query<'a>(draft: Option<&'a str>, committed: &'a str) -> &'a str {
    draft.unwrap_or(committed)
}

impl Shell {
    /// Refresh the local search completions as the field changes. Member and
    /// channel candidates are state already loaded for this account; typing
    /// never triggers a network lookup.
    pub fn search_suggestions(&mut self, ui: &AppWindow, input: &str) {
        self.search_draft = Some(input.to_owned());
        self.update_search_suggestions(ui, input);
    }

    /// Asks the network for members matching the `from:`/`mentions:` name
    /// being typed (the caller debounces); core skips what can't be asked.
    pub fn look_up_search_people(&mut self, ui: &AppWindow) {
        let (Some(draft), Some(search)) = (self.search_draft.as_deref(), self.app.state().search.as_ref()) else {
            return;
        };
        let supported = self
            .app
            .state()
            .room(&search.room)
            .map(|r| r.capabilities.search_filters)
            .unwrap_or_default();
        let Some(query) = godwit_core::search::people_prefix(draft, supported) else {
            return;
        };
        let room = search.room.clone();
        self.act(ui, Action::LookUpPeople { room, query });
    }

    fn update_search_suggestions(&self, ui: &AppWindow, input: &str) {
        let Some(search) = self.app.state().search.as_ref() else {
            return;
        };
        let state = self.app.state();
        let capabilities = state.room(&search.room).map(|r| r.capabilities);
        let filters = capabilities.map(|c| c.search_filters).unwrap_or_default();
        let has_values = capabilities.map(|c| c.search_has.words()).unwrap_or_default();
        let mut people = Vec::new();
        if let Some(groups) = state.members.get(&search.room) {
            people.extend(
                groups
                    .iter()
                    .flat_map(|g| &g.members)
                    .map(|m| (m.user.to_string(), m.name.to_string())),
            );
        }
        if let Some((room, _, found)) = &state.member_found
            && room == &search.room
        {
            for member in found.iter() {
                if !people.iter().any(|(id, _)| id == &member.user.to_string()) {
                    people.push((member.user.to_string(), member.name.to_string()));
                }
            }
        }
        if let Some((room, _, found)) = &state.people_found
            && room == &search.room
        {
            for member in found.iter() {
                if !people.iter().any(|(id, _)| id == &member.user.to_string()) {
                    people.push((member.user.to_string(), member.name.to_string()));
                }
            }
        }
        for profile in state
            .profiles
            .values()
            .filter(|p| p.user.account == search.room.account)
        {
            if !people.iter().any(|(id, _)| id == &profile.user.to_string()) {
                people.push((profile.user.to_string(), profile.name.to_string()));
            }
        }
        let channels: Vec<(String, String)> = match state.room(&search.room).map(|r| &r.kind) {
            Some(godwit_core::room::RoomKind::Channel { server }) => state
                .rooms
                .iter()
                .filter(|r| {
                    r.id.account == search.room.account
                        && matches!(&r.kind, godwit_core::room::RoomKind::Channel { server: s } if s == server)
                })
                .map(|r| (r.id.to_string(), r.name.to_string()))
                .collect(),
            _ => Vec::new(),
        };
        let suggestions = godwit_core::search::suggestions(input, filters, &people, &channels, &has_values)
            .into_iter()
            .map(|s| crate::ui::SearchSuggestionItem {
                label: s.label.into(),
                detail: s.detail.into(),
                query: s.query.into(),
            })
            .collect::<Vec<_>>();
        ui.set_search_suggestions(ModelRc::new(VecModel::from(suggestions)));
    }

    pub fn search_submitted(&mut self) {
        self.search_draft = None;
    }

    /// A message as the pins and search lists show it.
    pub(super) fn list_item(m: &godwit_core::message::Message) -> crate::ui::PinItem {
        crate::ui::PinItem {
            author: m.author_name.to_string().into(),
            text: m.snippet.as_deref().unwrap_or(&m.source).to_string().into(),
            time: chrono::DateTime::from_timestamp_millis(m.sent_ms)
                .map(|t| t.with_timezone(&chrono::Local).format("%-d %b %Y").to_string())
                .unwrap_or_default()
                .into(),
        }
    }

    /// The message search dialog, from the core's search.
    pub(super) fn show_search(&self, ui: &AppWindow) {
        let Some(s) = self.app.state().search.as_ref() else {
            ui.set_search_open(false);
            return;
        };
        let items: Vec<crate::ui::PinItem> = s
            .messages
            .iter()
            .flatten()
            .enumerate()
            .map(|(i, m)| {
                let mut item = Self::list_item(m);
                // A hit from another channel names it, as the official
                // client's result header does.
                if let Some(other) = s.rooms.get(i).filter(|r| **r != s.room)
                    && let Some(r) = self.app.state().room(other)
                {
                    item.author = format!("{} · #{}", item.author, r.name).into();
                }
                item
            })
            .collect();
        ui.set_search_results(ModelRc::new(VecModel::from(items)));
        ui.set_search_busy(s.messages.is_none());
        ui.set_search_failed(s.failed);
        ui.set_search_asked(!s.query.is_empty());
        // Only the filters the room's network takes, and the scope toggle
        // only for a server channel (`in:` and the scope are its own).
        let caps = self
            .app
            .state()
            .room(&s.room)
            .map(|r| r.capabilities.search_filters)
            .unwrap_or_default();
        ui.set_search_hint(caps.hint().into());
        ui.set_search_can_scope(caps.has(godwit_core::search::SearchFilter::Scope));
        ui.set_search_scope_server(s.scope == godwit_core::search::SearchScope::Server);
        let mut notice = s.errors.clone();
        notice.extend(s.notice.clone());
        ui.set_search_notice(notice.join("\n").into());
        ui.set_search_total(
            match s.total {
                Some(1) => "1 result".to_owned(),
                Some(n) => format!("{n} results"),
                None => String::new(),
            }
            .into(),
        );
        ui.set_search_more(s.next_offset.is_some());
        ui.set_search_loading_more(s.loading_more);
        ui.set_search_open(true);
        let query = search_completion_query(self.search_draft.as_deref(), &s.query);
        self.update_search_suggestions(ui, query);
    }

    /// "Jump" on a search result: the dialog closes, the message shows.
    pub fn search_jump(&mut self, ui: &AppWindow, index: i32) {
        let hit = self.app.state().search.as_ref().and_then(|s| {
            let i = usize::try_from(index).ok()?;
            let id = s.messages.as_ref()?.get(i)?.id.clone();
            Some((id, s.rooms.get(i).unwrap_or(&s.room).clone()))
        });
        if let Some((id, room)) = hit {
            self.act(ui, Action::CloseSearch);
            // A hit in another channel: open that one first.
            if self.app.state().selected.as_ref() != Some(&room) {
                self.act(ui, Action::SelectRoom(room));
            }
            self.act(ui, Action::JumpTo(id));
        }
    }

    /// The pinned messages panel, from the core's list.
    pub(super) fn show_pins(&self, ui: &AppWindow) {
        let Some(p) = self.app.state().pins.as_ref() else {
            ui.set_pins_open(false);
            return;
        };
        let items: Vec<crate::ui::PinItem> = p.messages.iter().flatten().map(|m| Self::list_item(m)).collect();
        ui.set_pins(ModelRc::new(VecModel::from(items)));
        ui.set_pins_loading(p.messages.is_none() && !p.failed);
        ui.set_pins_failed(p.failed);
        ui.set_pins_open(true);
    }

    pub(super) fn pinned_at(&self, index: i32) -> Option<MessageId> {
        let p = self.app.state().pins.as_ref()?;
        p.messages
            .as_ref()?
            .get(usize::try_from(index).ok()?)
            .map(|m| m.id.clone())
    }

    /// "Jump" in the pins panel: the panel closes, the message shows.
    pub fn pin_jump(&mut self, ui: &AppWindow, index: i32) {
        let Some(id) = self.pinned_at(index) else { return };
        self.act(ui, Action::ClosePins);
        self.act(ui, Action::JumpTo(id));
    }

    pub fn pin_unpin(&mut self, ui: &AppWindow, index: i32) {
        if let Some(message) = self.pinned_at(index) {
            self.act(ui, Action::SetPinned { message, pinned: false });
        }
    }

    /// "Create thread" on a message: the dialog, named after the message.
    pub fn create_thread_from(&mut self, ui: &AppWindow, row: i32) {
        let Some(id) = self.message_at(row) else { return };
        let name: String = self
            .app
            .state()
            .selected_message(&id)
            .map(|m| m.snippet.as_deref().unwrap_or(&m.source).chars().take(40).collect())
            .unwrap_or_default();
        self.thread_from = Some(id);
        ui.set_thread_post(false);
        ui.set_thread_name(name.trim().to_owned().into());
        ui.set_thread_text("".into());
        ui.set_thread_open(true);
    }

    /// "New post" in a forum: the dialog, empty.
    pub fn new_post(&mut self, ui: &AppWindow) {
        self.thread_from = None;
        self.post_tags.clear();
        self.render_forum(ui);
        ui.set_thread_post(true);
        ui.set_thread_name("".into());
        ui.set_thread_text("".into());
        ui.set_thread_open(true);
    }

    pub fn thread_confirm(&mut self, ui: &AppWindow) {
        let (name, text) = (ui.get_thread_name().to_string(), ui.get_thread_text().to_string());
        ui.set_thread_open(false);
        let message = if ui.get_thread_post() {
            None
        } else {
            self.thread_from.take()
        };
        let tags = if message.is_none() {
            std::mem::take(&mut self.post_tags)
        } else {
            Vec::new()
        };
        self.act(
            ui,
            Action::CreateThread {
                name,
                message,
                text,
                tags,
            },
        );
    }

    /// A tag's chip text: its emoji, then its name.
    fn tag_label(t: &godwit_core::forum::ForumTag) -> SharedString {
        match &t.emoji {
            Some(e) => format!("{e} {}", t.name).into(),
            None => t.name.to_string().into(),
        }
    }

    /// The tags a new post may pick from: moderated ones only for those who
    /// may give them.
    fn postable_tags(&self) -> Vec<&godwit_core::forum::ForumTag> {
        let Some(info) = self.app.state().forum.as_ref().and_then(|f| f.info.as_ref()) else {
            return Vec::new();
        };
        info.tags
            .iter()
            .filter(|t| !t.moderated || info.moderate_tags)
            .collect()
    }

    /// A tag picked or unpicked in the new-post dialog.
    pub fn thread_toggle_tag(&mut self, ui: &AppWindow, index: i32) {
        let Some(id) = usize::try_from(index)
            .ok()
            .and_then(|i| self.postable_tags().get(i).map(|t| t.id.clone()))
        else {
            return;
        };
        match self.post_tags.iter().position(|t| *t == id) {
            Some(at) => {
                self.post_tags.remove(at);
            }
            // Discord takes at most five tags on a post.
            None if self.post_tags.len() < godwit_core::forum::MAX_POST_TAGS => self.post_tags.push(id),
            None => {}
        }
        self.render_forum(ui);
    }

    /// The forum's post list and the new-post dialog's tags, from the core.
    /// Only when the forum changed since last shown: a refresh for another
    /// reason (a message arriving) rebuilds nothing.
    pub(super) fn show_forum(&self, ui: &AppWindow) {
        let state = self.app.state().forum.as_ref();
        if self.forum_shown.borrow().as_ref() == state {
            return;
        }
        self.render_forum(ui);
    }

    /// Builds the forum's models now (also when the dialog's picks changed).
    fn render_forum(&self, ui: &AppWindow) {
        use godwit_core::forum::ForumSort;
        let Some(f) = self.app.state().forum.as_ref() else {
            *self.forum_shown.borrow_mut() = None;
            ui.set_forum_posts(ModelRc::default());
            return;
        };
        *self.forum_shown.borrow_mut() = Some(f.clone());
        let info = f.info.as_ref();
        let now = chrono::Utc::now().timestamp_millis();
        let tag_of = |id: &str| info.and_then(|i| i.tags.iter().find(|t| &*t.id == id));
        let posts: Vec<crate::ui::ForumPostItem> = f
            .visible()
            .into_iter()
            .map(|p| {
                let (word, at) = match f.sort {
                    ForumSort::LatestActivity => ("active", p.last_activity.max(p.created)),
                    ForumSort::CreationDate => ("started", p.created),
                };
                let replies = match p.replies {
                    1 => "1 reply".to_owned(),
                    n => format!("{n} replies"),
                };
                let when = if at > 0 {
                    format!(" · {word} {}", crate::notifications::ago(at, now))
                } else {
                    String::new()
                };
                let tags: Vec<SharedString> = p.tags.iter().filter_map(|id| tag_of(id)).map(Self::tag_label).collect();
                crate::ui::ForumPostItem {
                    title: p.title.to_string().into(),
                    preview: p.preview.as_deref().unwrap_or_default().into(),
                    meta: format!("{replies}{when}").into(),
                    tags: ModelRc::new(VecModel::from(tags)),
                }
            })
            .collect();
        ui.set_forum_posts(ModelRc::new(VecModel::from(posts)));
        let filter: Vec<crate::ui::ForumTagItem> = info
            .map(|i| {
                i.tags
                    .iter()
                    .map(|t| crate::ui::ForumTagItem {
                        label: Self::tag_label(t),
                        chosen: f.tag.as_ref() == Some(&t.id),
                    })
                    .collect()
            })
            .unwrap_or_default();
        ui.set_forum_filter_tags(ModelRc::new(VecModel::from(filter)));
        let labels: Vec<SharedString> = ForumSort::ALL.iter().map(|s| s.label().into()).collect();
        ui.set_forum_sort_labels(ModelRc::new(VecModel::from(labels)));
        ui.set_forum_sort_index(ForumSort::ALL.iter().position(|s| *s == f.sort).unwrap_or(0) as i32);
        ui.set_forum_loading(info.is_none() && !f.failed);
        ui.set_forum_failed(f.failed);
        ui.set_forum_has_older(info.is_some() && f.older.is_some());
        ui.set_forum_loading_older(f.loading_older);
        let dialog: Vec<crate::ui::ForumTagItem> = self
            .postable_tags()
            .into_iter()
            .map(|t| crate::ui::ForumTagItem {
                label: Self::tag_label(t),
                chosen: self.post_tags.contains(&t.id),
            })
            .collect();
        let required = info.is_some_and(|i| i.require_tag);
        ui.set_thread_tags(ModelRc::new(VecModel::from(dialog)));
        ui.set_thread_tags_required(required);
        ui.set_thread_tags_missing(required && self.post_tags.is_empty());
    }

    /// A post in the forum's list: it opens.
    pub fn forum_open_post(&mut self, ui: &AppWindow, index: i32) {
        let room = self
            .app
            .state()
            .forum
            .as_ref()
            .and_then(|f| f.visible().get(usize::try_from(index).ok()?).map(|p| p.room.clone()));
        if let Some(room) = room {
            self.act(ui, Action::SelectRoom(room));
        }
    }

    /// A tag chip in the forum's filter: filter by it, or clear it.
    pub fn forum_toggle_tag(&mut self, ui: &AppWindow, index: i32) {
        let Some(f) = self.app.state().forum.as_ref() else {
            return;
        };
        let Some(tag) = f
            .info
            .as_ref()
            .and_then(|i| i.tags.get(usize::try_from(index).ok()?))
            .map(|t| t.id.clone())
        else {
            return;
        };
        let next = (f.tag.as_ref() != Some(&tag)).then_some(tag);
        self.act(ui, Action::SetForumTag(next));
    }

    pub fn forum_set_sort(&mut self, ui: &AppWindow, index: i32) {
        if let Some(sort) = usize::try_from(index)
            .ok()
            .and_then(|i| godwit_core::forum::ForumSort::ALL.get(i).copied())
        {
            self.act(ui, Action::SetForumSort(sort));
        }
    }

    /// "Pin message" in a message's menu.
    pub fn pin_row(&mut self, ui: &AppWindow, row: i32, pinned: bool) {
        if let Some(message) = self.message_at(row) {
            self.act(ui, Action::SetPinned { message, pinned });
        }
    }
}

#[cfg(test)]
mod search_completion_tests {
    use super::search_completion_query;

    #[test]
    fn refresh_keeps_the_live_search_draft_until_submission() {
        assert_eq!(search_completion_query(Some("from:ali"), "from:alice "), "from:ali");
        assert_eq!(search_completion_query(None, "from:alice "), "from:alice ");
    }
}
