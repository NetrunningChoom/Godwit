// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The poll dialog: "Create Poll" in the composer's menu opens it; the core
//! holds the limits (the room's `polls` capability) and gates the post.

use godwit_core::poll::{POLL_DEFAULT_DURATION, POLL_DURATIONS};

use super::*;

/// What is typed in the dialog so far.
#[derive(Default)]
pub struct PollDraft {
    room: Option<RoomId>,
    question: String,
    answers: Vec<String>,
}

impl Shell {
    pub fn open_poll(&mut self, ui: &AppWindow) {
        let Some(room) = self.app.state().selected.clone() else {
            return;
        };
        self.poll = PollDraft {
            room: Some(room),
            question: String::new(),
            answers: vec![String::new(); 2],
        };
        let labels: Vec<SharedString> = POLL_DURATIONS.iter().map(|(_, l)| (*l).into()).collect();
        ui.set_poll_durations(ModelRc::new(VecModel::from(labels)));
        ui.set_poll_duration(POLL_DEFAULT_DURATION as i32);
        ui.set_poll_multi(false);
        ui.set_poll_open(true);
        self.poll_refresh(ui, true);
    }

    pub fn poll_question(&mut self, ui: &AppWindow, text: &str) {
        self.poll.question = text.to_owned();
        self.poll_refresh(ui, false);
    }

    pub fn poll_answer(&mut self, ui: &AppWindow, row: i32, text: &str) {
        if let Some(a) = usize::try_from(row).ok().and_then(|r| self.poll.answers.get_mut(r)) {
            *a = text.to_owned();
        }
        self.poll_refresh(ui, false);
    }

    pub fn poll_add_answer(&mut self, ui: &AppWindow) {
        let max = self.poll_limits().map_or(0, |l| l.max_answers);
        if self.poll.answers.len() < max {
            self.poll.answers.push(String::new());
            self.poll_refresh(ui, true);
        }
    }

    pub fn poll_remove_answer(&mut self, ui: &AppWindow, row: i32) {
        let row = usize::try_from(row).unwrap_or(usize::MAX);
        let min = self.poll_limits().map_or(2, |l| l.min_answers);
        if row < self.poll.answers.len() && self.poll.answers.len() > min {
            self.poll.answers.remove(row);
            self.poll_refresh(ui, true);
        }
    }

    pub fn poll_post(&mut self, ui: &AppWindow, duration: i32, multi: bool) {
        let draft = std::mem::take(&mut self.poll);
        ui.set_poll_open(false);
        let hours = usize::try_from(duration)
            .ok()
            .and_then(|d| POLL_DURATIONS.get(d))
            .map(|(h, _)| *h);
        // The dialog belongs to the room it was opened in; the action goes to
        // the selected one, so a switched chat doesn't take the poll.
        let Some(room) = draft.room else { return };
        if self.app.state().selected.as_ref() != Some(&room) {
            self.errors.push("The poll wasn't posted: the chat changed.".into());
            self.refresh(ui);
            return;
        }
        if let Some(hours) = hours {
            self.act(
                ui,
                Action::CreatePoll {
                    question: draft.question,
                    answers: draft.answers,
                    hours,
                    multi,
                },
            );
        }
    }

    pub fn end_poll(&mut self, ui: &AppWindow, row: i32) {
        if let Some(message) = self.message_at(row) {
            self.act(ui, Action::EndPoll(message));
        }
    }

    fn poll_limits(&self) -> Option<godwit_core::poll::PollLimits> {
        let room = self.poll.room.as_ref()?;
        self.app.state().room(room)?.capabilities.polls
    }

    /// Shows what the dialog can post; `rebuild` sets the answer fields again
    /// (an edit leaves them alone: a field keeps the text typed in it).
    fn poll_refresh(&self, ui: &AppWindow, rebuild: bool) {
        let limits = self.poll_limits();
        if rebuild {
            let rows: Vec<SharedString> = self.poll.answers.iter().map(|a| a.as_str().into()).collect();
            ui.set_poll_answers(ModelRc::new(VecModel::from(rows)));
        }
        let checked = limits.map(|l| l.check(&self.poll.question, &self.poll.answers));
        ui.set_poll_can_add(limits.is_some_and(|l| self.poll.answers.len() < l.max_answers));
        ui.set_poll_can_post(matches!(checked, Some(Ok(_))));
        ui.set_poll_hint(match checked {
            Some(Err(why)) => why.into(),
            _ => SharedString::new(),
        });
    }
}
