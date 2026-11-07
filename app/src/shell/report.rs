// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The report dialog: opened from a user's card, a message's menu or a
//! server's menu; the core gates the report and answers with its outcome.

use godwit_core::report::ReportTarget;

use super::*;

impl Shell {
    fn open_report(&mut self, ui: &AppWindow, target: ReportTarget) {
        let reasons: Vec<slint::SharedString> = target.reasons().iter().map(|r| r.label().into()).collect();
        ui.set_report_title(format!("Report {}", target.noun()).into());
        ui.set_report_reasons(ModelRc::new(VecModel::from(reasons)));
        ui.set_report_reason(-1);
        ui.set_report_detail("".into());
        ui.set_report_status(0);
        ui.set_report_notice("".into());
        ui.set_report_open(true);
        self.report = Some(target);
        self.report_submitted = false;
    }

    /// "Report user" on the profile card.
    pub fn report_profile(&mut self, ui: &AppWindow) {
        if let Some((user, Some(_))) = self.app.state().profile.clone() {
            self.open_report(ui, ReportTarget::User { user, message: None });
        }
    }

    /// "Report message" in a message's menu.
    pub fn report_message(&mut self, ui: &AppWindow, row: i32) {
        let room = self.app.state().selected.clone();
        if let (Some(message), Some(room)) = (self.message_at(row), room) {
            self.open_report(ui, ReportTarget::Message { message, room });
        }
    }

    /// "Report server" in the selected server's menu.
    pub fn report_server(&mut self, ui: &AppWindow) {
        if let Some(server) = self.app.state().selected_server.clone() {
            self.open_report(ui, ReportTarget::Server { server });
        }
    }

    pub fn submit_report(&mut self, ui: &AppWindow, reason: i32, detail: &str) {
        let Some(target) = self.report.clone() else {
            return;
        };
        let Some(reason) = usize::try_from(reason)
            .ok()
            .and_then(|i| target.reasons().get(i))
            .copied()
        else {
            return;
        };
        self.report_submitted = true;
        self.act(
            ui,
            Action::Report {
                target,
                reason,
                detail: detail.to_owned(),
            },
        );
        self.show_report(ui);
    }

    pub fn close_report(&mut self, ui: &AppWindow) {
        self.report = None;
        self.report_submitted = false;
        ui.set_report_open(false);
    }

    /// Follows the core's outcome of the report just sent.
    pub(super) fn show_report(&mut self, ui: &AppWindow) {
        if self.report.is_none() || !self.report_submitted {
            return;
        }
        match &self.app.state().report_outcome {
            None => ui.set_report_status(1),
            Some(Ok(())) => ui.set_report_status(2),
            Some(Err(e)) => {
                ui.set_report_notice(e.to_string().into());
                ui.set_report_status(3);
            }
        }
    }
}
