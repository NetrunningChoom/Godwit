// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The report dialog: opened from a user's card, a message's menu or a
//! server's menu; the core gates the report and answers with its outcome.

use godwit_core::report::ReportTarget;

use super::*;

impl Shell {
    fn open_report(&mut self, ui: &AppWindow, target: ReportTarget) {
        // A network that declines reports explains why, with its sources,
        // instead of the form.
        let account = match &target {
            ReportTarget::User { user, .. } => user.account.clone(),
            ReportTarget::Message { room, .. } => room.account.clone(),
            ReportTarget::Server { server } => server.account.clone(),
        };
        let notice = self.account(&account).and_then(|a| a.report_notice());
        ui.set_report_unavailable(notice.as_ref().map_or("", |n| n.text.as_str()).into());
        let labels: Vec<slint::SharedString> = notice
            .iter()
            .flat_map(|n| n.links.iter().map(|(l, _)| l.as_str().into()))
            .collect();
        ui.set_report_links(ModelRc::new(VecModel::from(labels)));
        self.report_links = notice
            .map(|n| n.links.into_iter().map(|(_, u)| u).collect())
            .unwrap_or_default();
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

    /// Opens one of the notice's links in the browser.
    pub fn open_report_link(&mut self, i: i32) {
        if let Some(url) = usize::try_from(i).ok().and_then(|i| self.report_links.get(i)) {
            open_external(url);
        }
    }

    /// Keeps the report entries (card, message menu, server menu) showing
    /// for networks that decline reports, where the click explains why.
    pub(super) fn show_report_entries(&mut self, ui: &AppWindow) {
        let state = self.app.state();
        let declines =
            |account: &godwit_core::ids::AccountId| self.account(account).is_some_and(|a| a.report_notice().is_some());
        if let Some((user, Some(p))) = &state.profile
            && !p.me
            && declines(&user.account)
        {
            ui.set_card_can_report(true);
        }
        if let Some(room) = &state.selected
            && declines(&room.account)
        {
            ui.set_can_report(true);
        }
        if let Some(server) = &state.selected_server
            && declines(&server.account)
        {
            ui.set_server_can_report(true);
        }
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
