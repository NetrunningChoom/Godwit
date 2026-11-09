// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The timeline's messages: selecting and copying, links, attachments, reactions, the emoji picker and pasting files.

use super::*;

fn autocomplete_candidates(
    state: &godwit_core::application::AppState,
    room: &RoomId,
    kind: godwit_core::autocomplete::TriggerKind,
    specials: Vec<godwit_core::autocomplete::MentionSpecial>,
) -> Vec<godwit_core::autocomplete::Candidate> {
    use godwit_core::autocomplete::{Candidate, CandidateId, TriggerKind};
    use std::sync::Arc;
    let mut out = Vec::new();
    if kind == TriggerKind::Person {
        if let Some(current) = state.room(room)
            && let godwit_core::room::RoomKind::Direct { with } = &current.kind
        {
            let name = state
                .profiles
                .get(with)
                .map(|profile| profile.name.as_ref())
                .or_else(|| {
                    state
                        .friends
                        .iter()
                        .find(|person| &person.id == with)
                        .map(|person| person.name.as_ref())
                })
                .unwrap_or(current.name.as_ref());
            add_person_candidate(&mut out, with, name);
        }
        if let Some(groups) = state.members.get(room) {
            for member in groups.iter().flat_map(|g| &g.members) {
                add_person_candidate(&mut out, &member.user, &member.name);
            }
        }
        for person in state.friends.iter().filter(|p| p.id.account == room.account) {
            add_person_candidate(&mut out, &person.id, &person.name);
        }
        if let Some(people) = state.people_directory.get(&room.account) {
            for person in people {
                add_person_candidate(&mut out, &person.id, &person.name);
            }
        }
        out.extend(
            specials
                .into_iter()
                .filter(|s| s.label.starts_with('@'))
                .map(|special| Candidate {
                    id: CandidateId::MentionSpecial(special.id.into()),
                    name: special.label.into(),
                    detail: special.detail.into(),
                }),
        );
        if let Some(timeline) = state.timeline(room) {
            for message in timeline.messages.iter().rev().take(100) {
                add_person_candidate(&mut out, &message.author, &message.author_name);
            }
        }
    } else if kind == TriggerKind::MentionSpecial {
        out.extend(
            specials
                .into_iter()
                .filter(|s| s.label.starts_with('%'))
                .map(|special| Candidate {
                    id: CandidateId::MentionSpecial(special.id.into()),
                    name: special.label.into(),
                    detail: special.detail.into(),
                }),
        );
    } else if kind == TriggerKind::Channel {
        if let Some(server_id) = state.room(room).and_then(|r| match &r.kind {
            godwit_core::room::RoomKind::Channel { server } => Some(server),
            _ => None,
        }) && let Some(server) = state.server(server_id)
        {
            for id in server.categories.iter().flat_map(|c| &c.rooms) {
                if let Some(channel) = state.room(id) {
                    out.push(Candidate {
                        id: CandidateId::Channel(id.clone()),
                        name: channel.name.clone(),
                        detail: Arc::from("Channel"),
                    });
                }
            }
        }
    } else {
        for group in state.emoji_for(room) {
            for emoji in &group.emoji {
                let raw = emoji.reaction.rsplit(':').next().unwrap_or(emoji.reaction.as_ref());
                out.push(Candidate {
                    id: CandidateId::Emoji(Arc::from(raw)),
                    name: emoji.name.clone(),
                    detail: Arc::from("Custom emoji"),
                });
            }
        }
        for emoji in emojis::iter().filter(|e| crate::emoji::drawn(e)) {
            if let Some(code) = emoji.shortcode() {
                out.push(Candidate {
                    id: CandidateId::Emoji(Arc::from(emoji.as_str())),
                    name: Arc::from(code),
                    detail: Arc::from(emoji.as_str()),
                });
            }
        }
    }
    out
}

fn add_person_candidate(out: &mut Vec<godwit_core::autocomplete::Candidate>, id: &UserId, name: &str) {
    use godwit_core::autocomplete::{Candidate, CandidateId};
    let candidate = Candidate {
        id: CandidateId::Person(id.clone()),
        name: name.into(),
        detail: "Person".into(),
    };
    if !out.iter().any(|c| c.id == candidate.id) {
        out.push(candidate);
    }
}

impl Shell {
    /// Rebuilds suggestions only when the active token changes; cursor moves
    /// within the same token update its replacement span without rescanning.
    pub fn autocomplete_changed(&mut self, ui: &AppWindow, cursor: i32) {
        use godwit_core::autocomplete::{self, CandidateId};
        let text = ui.get_draft();
        let Some(trigger) = autocomplete::trigger_at(text.as_str(), usize::try_from(cursor).unwrap_or(usize::MAX))
        else {
            self.autocomplete.clear();
            self.autocomplete_trigger = None;
            ui.set_autocomplete_rows(ModelRc::default());
            return;
        };
        let Some(room) = self.app.state().selected.clone() else {
            return;
        };
        let changed = self.autocomplete_trigger.as_ref().is_none_or(|old| {
            old.kind != trigger.kind || old.query != trigger.query || self.autocomplete_room.as_ref() != Some(&room)
        });
        if changed {
            let specials = self
                .account(&room.account)
                .map(|a| a.mention_specials(&room))
                .unwrap_or_default();
            let candidates = autocomplete_candidates(self.app.state(), &room, trigger.kind, specials);
            self.autocomplete = autocomplete::matches(&candidates, &trigger.query, 8)
                .into_iter()
                .cloned()
                .collect();
            self.autocomplete_room = Some(room);
        }
        self.autocomplete_trigger = Some(trigger);
        let rows = self
            .autocomplete
            .iter()
            .map(|c| {
                let (kind, id) = match &c.id {
                    CandidateId::Person(id) => (0, id.raw.to_string()),
                    CandidateId::Channel(id) => (1, id.raw.to_string()),
                    CandidateId::Emoji(id) => (2, id.to_string()),
                    CandidateId::MentionSpecial(id) => (3, id.to_string()),
                };
                crate::ui::AutocompleteItem {
                    kind,
                    id: id.into(),
                    name: c.name.to_string().into(),
                    detail: c.detail.to_string().into(),
                }
            })
            .collect::<Vec<_>>();
        ui.set_autocomplete_rows(ModelRc::new(VecModel::from(rows)));
    }

    pub fn autocomplete_pick(&mut self, ui: &AppWindow, row: i32) {
        use godwit_core::autocomplete::{self, CandidateId};
        if row == -2 {
            self.autocomplete.clear();
            self.autocomplete_trigger = None;
            self.autocomplete_room = None;
            ui.set_autocomplete_rows(ModelRc::default());
            return;
        }
        let Some(index) = usize::try_from(row).ok() else { return };
        let (Some(candidate), Some(trigger), Some(room)) = (
            self.autocomplete.get(index),
            self.autocomplete_trigger.as_ref(),
            self.autocomplete_room.as_ref(),
        ) else {
            return;
        };
        let (kind, id, replacement) = match &candidate.id {
            CandidateId::Person(id) => (trigger.kind, id.raw.as_ref(), None),
            CandidateId::Channel(id) => (trigger.kind, id.raw.as_ref(), None),
            CandidateId::Emoji(id) if !id.is_ascii() => (trigger.kind, id.as_ref(), Some(id.as_ref())),
            CandidateId::Emoji(id) => (trigger.kind, id.as_ref(), None),
            CandidateId::MentionSpecial(id) => (
                godwit_core::autocomplete::TriggerKind::MentionSpecial,
                id.as_ref(),
                None,
            ),
        };
        let rendered = replacement.map(str::to_owned).unwrap_or_else(|| {
            self.account(&room.account)
                .map(|a| a.autocomplete_text(kind, id, &candidate.name))
                .unwrap_or_else(|| candidate.name.to_string())
        });
        let (draft, caret) = autocomplete::insert(ui.get_draft().as_str(), trigger, &rendered);
        ui.set_draft(draft.as_str().into());
        self.act(ui, Action::SetDraft(draft));
        ui.set_composer_inserted_at(i32::try_from(caret).unwrap_or(i32::MAX));
        ui.set_composer_insert_serial(ui.get_composer_insert_serial().wrapping_add(1));
        self.autocomplete.clear();
        self.autocomplete_trigger = None;
        self.autocomplete_room = None;
        ui.set_autocomplete_rows(ModelRc::default());
    }
    /// Copies a message's text (as written) or its link to the clipboard.
    pub fn copy(&self, row: i32, link: bool) {
        let Some(id) = self.message_at(row) else { return };
        let Some(room) = self.app.state().selected.clone() else {
            return;
        };
        let text = if link {
            let Some(link) = self.account(&room.account).and_then(|a| a.message_link(&room, &id)) else {
                return;
            };
            link
        } else {
            match self.app.state().selected_message(&id) {
                Some(m) => m.source.to_string(),
                None => return,
            }
        };
        copy_to_clipboard(text);
    }

    /// Opens the emoji picker for timeline row `row`'s reactions, or for the
    /// composer when `row` is negative.
    pub fn open_picker(&mut self, ui: &AppWindow, row: i32) {
        let target = if row < 0 { None } else { self.message_at(row) };
        if row >= 0 && target.is_none() {
            return;
        }
        self.picker_for = Some(target);
        self.picker_room = self.app.state().selected.clone();
        self.picker_search("");
        ui.set_picker_open(true);
    }

    pub fn picker_search(&mut self, query: &str) {
        let state = self.app.state();
        let custom = state.selected.as_ref().map(|r| state.emoji_for(r)).unwrap_or_default();
        let network = state.selected.as_ref().map(|r| r.account.network);
        // A network with its own emoji set (Teams' emoticon table) offers
        // only that; others are limited by Emoji version, if at all.
        let set = network.and_then(|n| (godwit_core::hooks::of(n).emoji_set)());
        let allowed = match (set, network.and_then(|n| n.emoji_version())) {
            (Some(set), _) => picker::Allowed::Only(set),
            (None, Some((a, b))) => picker::Allowed::UpTo(a, b),
            (None, None) => picker::Allowed::All,
        };
        let rows = picker::rows(&custom, query, allowed);
        self.picker.set(rows, &mut self.images);
    }

    pub fn picker_scrolled(&mut self, first: i32) {
        self.picker.show(usize::try_from(first).unwrap_or(0), &mut self.images);
    }

    pub fn close_picker(&mut self, ui: &AppWindow) {
        self.picker_for = None;
        self.picker_room = None;
        ui.set_picker_open(false);
    }

    /// An emoji was picked: react with it, or write it into the draft.
    pub fn picker_picked(&mut self, ui: &AppWindow, row: i32, col: i32) {
        let (Ok(row), Ok(col)) = (usize::try_from(row), usize::try_from(col)) else {
            return;
        };
        let Some(pick) = self.picker.at(row, col).cloned() else {
            return;
        };
        let target = self.picker_for.take();
        ui.set_picker_open(false);
        if self.picker_room.take() != self.app.state().selected {
            return;
        }
        match target {
            Some(Some(message)) => self.act(
                ui,
                Action::ToggleReaction {
                    message,
                    emoji: pick.reaction,
                },
            ),
            Some(None) => {
                let (draft, caret) = insert_at_selection(
                    ui.get_draft().as_str(),
                    ui.get_composer_anchor(),
                    ui.get_composer_cursor(),
                    &pick.code,
                );
                ui.set_draft(draft.as_str().into());
                self.act(ui, Action::SetDraft(draft));
                ui.set_composer_inserted_at(i32::try_from(caret).unwrap_or(i32::MAX));
                ui.set_composer_insert_serial(ui.get_composer_insert_serial().wrapping_add(1));
            }
            None => {}
        }
    }

    /// Ctrl+V in the composer: a picture on the clipboard (a screenshot, an
    /// image copied in a browser) is attached as `image.png`, as Discord's
    /// and Stoat's own clients name pasted pictures; copied files attach as
    /// they are. Returns false when there's nothing to attach, so the text
    /// pastes as usual.
    pub fn paste_files(&mut self, ui: &AppWindow) -> bool {
        let Ok(mut clipboard) = arboard::Clipboard::new() else {
            return false;
        };
        if let Ok(paths) = clipboard.get().file_list() {
            let files: Vec<PathBuf> = paths.into_iter().filter(|p| p.is_file()).collect();
            if !files.is_empty() {
                self.add_files(ui, files);
                return true;
            }
        }
        let Ok(image) = clipboard.get_image() else {
            return false;
        };
        let (Ok(w), Ok(h)) = (u32::try_from(image.width), u32::try_from(image.height)) else {
            return false;
        };
        let Some(buffer) = image::RgbaImage::from_raw(w, h, image.bytes.into_owned()) else {
            return false;
        };
        // A folder of its own per paste: the file keeps its plain name.
        let dir = std::env::temp_dir().join(format!(
            "godwit-paste-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis())
        ));
        let path = dir.join("image.png");
        let saved = std::fs::create_dir_all(&dir).map_err(|e| e.to_string()).and_then(|()| {
            buffer
                .save_with_format(&path, image::ImageFormat::Png)
                .map_err(|e| e.to_string())
        });
        match saved {
            Ok(()) => {
                self.add_files(ui, vec![path]);
                true
            }
            Err(e) => {
                eprintln!("error: keeping a pasted picture: {e}");
                false
            }
        }
    }

    /// Files picked in the dialog, to send with the next message.
    pub fn add_files(&mut self, ui: &AppWindow, paths: Vec<PathBuf>) {
        let files: Vec<Outgoing> = paths
            .into_iter()
            .filter_map(|path| {
                let meta = std::fs::metadata(&path).ok().filter(std::fs::Metadata::is_file)?;
                let name = path.file_name()?.to_string_lossy().into_owned();
                Some(Outgoing {
                    path,
                    name: name.into(),
                    size: meta.len(),
                    spoiler: false,
                })
            })
            .collect();
        if !files.is_empty() {
            self.act(ui, Action::AddFiles(files));
        }
    }

    /// The message shown at timeline row `row`.
    pub fn message_at(&self, row: i32) -> Option<MessageId> {
        let row = usize::try_from(row).ok()?;
        self.presenter.rows.get(row).map(|k| k.id().clone())
    }

    /// A link in a message was clicked.
    /// The timeline's text width changed: its text is laid out again.
    pub fn text_width(&mut self, ui: &AppWindow, width: f32) {
        if width > 0.0 && (width - self.presenter.text_width).abs() >= 1.0 {
            self.presenter.text_width = width;
            self.refresh(ui);
        }
    }

    pub fn select_down(&mut self, ui: &AppWindow, row: i32, block: i32, x: f32, y: f32, extend: bool) {
        if row == view::CARD_ROW {
            // One selection at a time, as in a browser.
            self.selection.clear();
            if let Ok(block) = usize::try_from(block) {
                self.card_selection.down(&self.presenter.card_laid, block, x, y);
            }
            self.show_selection(ui);
            return;
        }
        self.card_selection.clear();
        let (Ok(row), Ok(block)) = (usize::try_from(row), usize::try_from(block)) else {
            return;
        };
        self.selection.down(&self.presenter.rows, row, block, x, y, extend);
        self.show_selection(ui);
    }

    pub fn select_drag(&mut self, ui: &AppWindow, row: i32, block: i32, x: f32, y: f32) {
        if row == view::CARD_ROW {
            if let Ok(block) = usize::try_from(block) {
                self.card_selection.drag(&self.presenter.card_laid, block, x, y);
            }
            self.show_selection(ui);
            return;
        }
        let (Ok(row), Ok(block)) = (usize::try_from(row), usize::try_from(block)) else {
            return;
        };
        self.selection.drag(&self.presenter.rows, row, block, x, y);
        self.show_selection(ui);
    }

    pub fn select_up(&mut self, ui: &AppWindow, row: i32, block: i32, x: f32, y: f32) {
        if row == view::CARD_ROW {
            let link = usize::try_from(block)
                .ok()
                .and_then(|b| self.card_selection.up(&self.presenter.card_laid, b, x, y));
            self.show_selection(ui);
            // A link in their bio opens as one in a message would.
            if let Some(url) = link {
                self.link(ui, view::CARD_ROW, &url);
            }
            return;
        }
        let (Ok(r), Ok(b)) = (usize::try_from(row), usize::try_from(block)) else {
            return;
        };
        let link = self.selection.up(&self.presenter.rows, r, b, x, y);
        self.show_selection(ui);
        if let Some(url) = link {
            self.link(ui, row, &url);
        }
    }

    pub fn select_word(&mut self, ui: &AppWindow, row: i32, block: i32, x: f32, y: f32) {
        if row == view::CARD_ROW {
            if let Ok(block) = usize::try_from(block) {
                self.card_selection.word(&self.presenter.card_laid, block, x, y);
            }
            self.show_selection(ui);
            return;
        }
        let (Ok(row), Ok(block)) = (usize::try_from(row), usize::try_from(block)) else {
            return;
        };
        self.selection.word(&self.presenter.rows, row, block, x, y);
        self.show_selection(ui);
    }

    /// The pointer moved over text: a hand over links.
    pub fn select_hover(&mut self, ui: &AppWindow, row: i32, block: i32, x: f32, y: f32) {
        use slint::ComponentHandle;
        let link = if row == view::CARD_ROW {
            usize::try_from(block)
                .ok()
                .and_then(|b| crate::selection::BlockSelection::link_at(&self.presenter.card_laid, b, x, y))
        } else {
            let (Ok(r), Ok(b)) = (usize::try_from(row), usize::try_from(block)) else {
                return;
            };
            self.selection.link_at(&self.presenter.rows, r, b, x, y)
        };
        let select = ui.global::<crate::ui::Select>();
        let (row, block) = if link.is_some() { (row, block) } else { (-1, -1) };
        if select.get_link_row() != row || select.get_link_block() != block {
            select.set_link_row(row);
            select.set_link_block(block);
        }
        // What the right-click menu offers to open or copy: web links only
        // (mentions, spoilers and the like are Godwit's own).
        let url = link.as_deref().filter(|u| is_web_url(u)).unwrap_or_default();
        if select.get_link_url() != url {
            select.set_link_url(url.into());
        }
    }

    /// Ctrl+C with text selected in the timeline.
    pub fn select_copy(&mut self) {
        let text = if self.card_selection.is_empty() {
            self.selection.text(&self.presenter.rows)
        } else {
            self.card_selection.text(&self.presenter.card_laid)
        };
        if let Some(text) = text {
            copy_to_clipboard(text);
        }
    }

    pub fn select_clear(&mut self, ui: &AppWindow) {
        self.selection.clear();
        self.card_selection.clear();
        self.show_selection(ui);
    }

    pub(super) fn show_selection(&mut self, ui: &AppWindow) {
        use slint::ComponentHandle;
        let select = ui.global::<crate::ui::Select>();
        let rects: Vec<crate::ui::SelRect> = self
            .selection
            .rects(&self.presenter.rows)
            .into_iter()
            .map(|r| crate::ui::SelRect {
                row: i32::try_from(r.row).unwrap_or(-1),
                block: i32::try_from(r.block).unwrap_or(-1),
                x: r.x,
                y: r.y,
                w: r.w,
                h: r.h,
            })
            .chain(
                self.card_selection
                    .rects(&self.presenter.card_laid, 0)
                    .into_iter()
                    .map(|r| crate::ui::SelRect {
                        row: view::CARD_ROW,
                        block: i32::try_from(r.block).unwrap_or(-1),
                        x: r.x,
                        y: r.y,
                        w: r.w,
                        h: r.h,
                    }),
            )
            .collect();
        let active = !rects.is_empty();
        if active || select.get_active() {
            select.set_rects(slint::ModelRc::new(VecModel::from(rects)));
        }
        select.set_active(active);
    }

    pub fn link(&mut self, ui: &AppWindow, row: i32, url: &str) {
        if url == crate::view::REVEAL_HIDDEN {
            // A collapsed run of hidden messages: show each message for this session.
            if let Some(run) = self.message_at(row).and_then(|id| self.presenter.hidden_runs.get(&id)) {
                let run = run.clone();
                self.presenter.revealed_runs.extend(run);
                self.refresh(ui);
            }
        } else if url.starts_with(rich::SPOILER) {
            if let Some(id) = self.message_at(row) {
                self.presenter.revealed.insert(id);
                self.refresh(ui);
            }
        } else if let Some(channel) = url.strip_prefix("godwit://room/") {
            let room = self
                .presenter
                .timeline_of
                .as_ref()
                .map(|r| RoomId::new(r.account.clone(), godwit_core::rich::percent_decode(channel).as_str()));
            if let Some(room) = room.filter(|r| self.app.state().room(r).is_some()) {
                self.act(ui, Action::SelectRoom(room));
            }
        } else if url == crate::textlayout::AUTHOR_LINK {
            // A header's name: the author's profile card.
            if let Some(user) = self.author_at(row) {
                self.act(ui, Action::OpenProfile(user));
            }
        } else if let Some(page) = url.strip_prefix("godwit://view/") {
            // An embed's picture (a GIF, a link's preview): the viewer, at it.
            if let Some(message) = self.message_at(row) {
                let at = self.app.state().selected_message(&message).and_then(|m| {
                    let images = m.attachments.iter().filter(|a| a.kind == AttachmentKind::Image).count();
                    let embeds: Vec<&godwit_core::message::Embed> =
                        m.embeds.iter().filter(|e| e.image_url.is_some()).collect();
                    embeds
                        .iter()
                        .position(|e| e.url.as_deref() == Some(page))
                        .map(|p| images + p)
                });
                match at {
                    Some(at) => self.open_viewer(ui, message, at),
                    None if page.starts_with("https://") || page.starts_with("http://") => {
                        ui.set_confirm_link(page.into())
                    }
                    None => {}
                }
            }
        } else if let Some(answer) = url.strip_prefix("godwit://vote/").and_then(|a| a.parse::<u64>().ok()) {
            // A poll's answer: vote for it (or take the vote back).
            if let Some(message) = self.message_at(row) {
                self.act(ui, Action::VotePoll { message, answer });
            }
        } else if let Some(user) = url.strip_prefix("godwit://user/") {
            // A mention: their profile card, as officially.
            if let Some(account) = self.presenter.timeline_of.as_ref().map(|r| r.account.clone()) {
                self.act(ui, Action::OpenProfile(UserId::new(account, user)));
            }
        } else if url.starts_with("http://") || url.starts_with("https://") {
            // Asked first, showing where it really goes: a link's text can
            // say something else (Discord's masked links).
            ui.set_confirm_link(url.into());
        }
    }

    pub fn activate_message_button(&mut self, ui: &AppWindow, row: i32, custom_id: &str) {
        let Some(room) = self.presenter.timeline_of.clone() else {
            return;
        };
        let Some(message) = self.message_at(row) else { return };
        self.act(
            ui,
            Action::ActivateMessageButton {
                room,
                message,
                custom_id: custom_id.into(),
            },
        );
    }

    /// "Open" in the link question.
    pub fn open_confirmed_link(&mut self, ui: &AppWindow) {
        let url = ui.get_confirm_link();
        ui.set_confirm_link("".into());
        if url.starts_with("http://") || url.starts_with("https://") {
            open_external(&url);
        }
    }

    pub fn open_attachment(&mut self, ui: &AppWindow, row: i32, index: i32) {
        let Some(id) = self.message_at(row) else { return };
        let Ok(index) = usize::try_from(index) else { return };
        let url = self.app.state().selected_message(&id).and_then(|m| {
            let a = m.attachments.get(index)?;
            Some((a.url.clone(), a.name.clone(), a.kind))
        });
        let Some((url, name, kind)) = url else { return };
        // Pictures open in the viewer, as Yapfast's; other files as before.
        if kind == AttachmentKind::Image {
            // Its place among the message's pictures (image attachments come first).
            let at = self.app.state().selected_message(&id).map_or(0, |m| {
                m.attachments
                    .iter()
                    .take(index)
                    .filter(|a| a.kind == AttachmentKind::Image)
                    .count()
            });
            self.open_viewer(ui, id, at);
            return;
        }
        if let Some((scheme, fetch)) = self.media.iter().find(|(scheme, _)| url.starts_with(scheme.as_str())) {
            // The whole file, not the thumbnail a `#…` part asks for.
            let whole = url.split('#').next().unwrap_or(&url).to_owned();
            let network = self
                .media_networks
                .iter()
                .find(|(prefix, _)| prefix == scheme)
                .map_or("network", |(_, name)| name.as_str());
            save_and_open(fetch.clone(), network.to_owned(), whole, name.to_string(), kind);
        } else if url.starts_with("http://") || url.starts_with("https://") {
            // A file on the web leaves the app like any link: asked first,
            // showing where it goes.
            ui.set_confirm_link((&*url).into());
        }
    }

    /// An attachment's context menu: 0 opens it in the browser, 1 saves it,
    /// 2 copies its address. As the image viewer's buttons do, for any file.
    pub fn attachment_action(&mut self, row: i32, index: i32, action: i32) {
        let Some(id) = self.message_at(row) else { return };
        let Ok(index) = usize::try_from(index) else { return };
        let Some(a) = self
            .app
            .state()
            .selected_message(&id)
            .and_then(|m| m.attachments.get(index))
            .cloned()
        else {
            return;
        };
        let full = a.original_url.as_deref().unwrap_or(&a.url).to_owned();
        match action {
            0 => {
                if let Some(link) = attachment_web_link(&a) {
                    open_external(link);
                }
            }
            1 => self.save_or_open(full, a.name.to_string(), a.kind, false),
            _ => {
                if let Some(link) = attachment_web_link(&a) {
                    copy_to_clipboard(link.to_owned());
                }
            }
        }
    }

    pub fn react_row(&mut self, ui: &AppWindow, row: i32, reaction: i32) {
        let Some(id) = self.message_at(row) else { return };
        let emoji = self.app.state().selected_message(&id).and_then(|m| {
            m.reactions
                .get(usize::try_from(reaction).ok()?)
                .map(|r| (r.emoji.clone(), r.me))
        });
        if let Some((emoji, me)) = emoji {
            godwit_core::verbose!("reaction: clicked emoji={emoji} me={me} add={}", !me);
            self.act(ui, Action::ToggleReaction { message: id, emoji });
        }
    }

    pub fn remove_reactions_row(&mut self, ui: &AppWindow, row: i32, reaction: i32) {
        let Some(id) = self.message_at(row) else { return };
        let emoji = self.app.state().selected_message(&id).and_then(|m| {
            m.reactions
                .get(usize::try_from(reaction).ok()?)
                .map(|r| r.emoji.clone())
        });
        if let Some(emoji) = emoji {
            self.act(
                ui,
                Action::RemoveReactions {
                    message: id,
                    emoji: Some(emoji),
                },
            );
        }
    }

    pub fn remove_all_reactions_row(&mut self, ui: &AppWindow, row: i32) {
        if let Some(message) = self.message_at(row) {
            self.act(ui, Action::RemoveReactions { message, emoji: None });
        }
    }

    pub fn show_reactors_row(&mut self, ui: &AppWindow, row: i32, reaction: i32) {
        let Some(id) = self.message_at(row) else { return };
        let emoji = self.app.state().selected_message(&id).and_then(|m| {
            m.reactions
                .get(usize::try_from(reaction).ok()?)
                .map(|r| r.emoji.clone())
        });
        if let Some(emoji) = emoji {
            self.act(ui, Action::ShowReactors { message: id, emoji });
        }
    }

    pub fn close_reactors(&mut self, ui: &AppWindow) {
        self.act(ui, Action::CloseReactors);
        ui.set_reactors_open(false);
    }
}

#[cfg(test)]
mod autocomplete_tests {
    use super::*;
    use godwit_core::{
        application::{AppState, Person},
        ids::AccountId,
        members::{Member, MemberGroup, Presence},
        room::{Capabilities, Room, RoomEncryption, RoomKind, RoomNotify},
    };
    use std::sync::Arc;

    #[test]
    fn gathers_open_room_members_and_deduplicates_recent_authors() {
        let account = AccountId::new(godwit_core::network::Network::ALL[0], "me");
        let room_id = RoomId::new(account.clone(), "room");
        let person_id = UserId::new(account.clone(), "person");
        let mut state = AppState::default();
        state.rooms.push(Arc::new(Room {
            id: room_id.clone(),
            name: Arc::from("chat"),
            kind: RoomKind::Direct {
                with: person_id.clone(),
            },
            icon_url: None,
            capabilities: Capabilities::default(),
            encryption: RoomEncryption::Unknown,
            unread: false,
            mentions: 0,
            last_read: None,
            invited_by: None,
            presence: None,
            status: None,
            member_count: None,
            last_activity: 0,
            parent: None,
            joined_thread: false,
            notify: RoomNotify::default(),
        }));
        state.members.insert(
            room_id.clone(),
            Arc::new(vec![MemberGroup {
                title: Arc::from("Members"),
                members: vec![Member {
                    user: person_id.clone(),
                    name: Arc::from("Ada"),
                    avatar_url: None,
                    colour: None,
                    presence: Presence::Unknown,
                    status: None,
                }],
            }]),
        );
        state.friends.push(Person {
            id: person_id.clone(),
            name: Arc::from("Ada"),
            avatar_url: None,
            online: false,
        });
        let friend_id = UserId::new(account.clone(), "friend");
        state.friends.push(Person {
            id: friend_id.clone(),
            name: Arc::from("Bea"),
            avatar_url: None,
            online: false,
        });
        let candidates = autocomplete_candidates(
            &state,
            &room_id,
            godwit_core::autocomplete::TriggerKind::Person,
            Vec::new(),
        );
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].name.as_ref(), "Ada");
        assert_eq!(
            candidates[0].id,
            godwit_core::autocomplete::CandidateId::Person(person_id)
        );
        assert_eq!(
            candidates[1].id,
            godwit_core::autocomplete::CandidateId::Person(friend_id)
        );
    }

    #[test]
    fn special_mentions_follow_people_in_candidate_order() {
        let account = AccountId::new(godwit_core::network::Network::ALL[0], "me");
        let room_id = RoomId::new(account.clone(), "room");
        let person_id = UserId::new(account.clone(), "person");
        let mut state = AppState::default();
        state.friends.push(Person {
            id: person_id,
            name: Arc::from("Everyone"),
            avatar_url: None,
            online: false,
        });
        let candidates = autocomplete_candidates(
            &state,
            &room_id,
            godwit_core::autocomplete::TriggerKind::Person,
            vec![godwit_core::autocomplete::MentionSpecial {
                id: "everyone".into(),
                label: "@everyone".into(),
                detail: "Special".into(),
            }],
        );
        let matched = godwit_core::autocomplete::matches(&candidates, "everyone", 8);
        assert!(matches!(
            matched[0].id,
            godwit_core::autocomplete::CandidateId::Person(_)
        ));
        assert!(matches!(
            matched[1].id,
            godwit_core::autocomplete::CandidateId::MentionSpecial(_)
        ));
    }
}
