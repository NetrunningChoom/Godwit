// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The confirmation prompt for a request a network holds back until the
//! person proves it's them; the core queues the prompts and gates the
//! answers, and this only shows the first and carries the typed code over.

use godwit_core::application::Secret;

use super::*;

impl Shell {
    pub(super) fn show_step_up(&mut self, ui: &AppWindow) {
        let Some(prompt) = self.app.state().step_up.first() else {
            if self.step_up_shown.take().is_some() {
                // Nothing typed stays behind once the prompt is gone.
                ui.set_step_up_code("".into());
                ui.set_step_up_error("".into());
                ui.set_step_up_open(false);
            }
            return;
        };
        let key = (prompt.account.clone(), prompt.id);
        if self.step_up_shown.as_ref() != Some(&key) {
            self.step_up_shown = Some(key);
            ui.set_step_up_code("".into());
            ui.set_step_up_method(0);
            let labels: Vec<slint::SharedString> = prompt.methods.iter().map(|m| m.label().into()).collect();
            ui.set_step_up_methods(ModelRc::new(VecModel::from(labels)));
        }
        ui.set_step_up_busy(prompt.busy);
        ui.set_step_up_error(prompt.error.as_deref().unwrap_or("").into());
        ui.set_step_up_open(true);
    }

    pub fn answer_step_up(&mut self, ui: &AppWindow, method: i32, code: &str) {
        let Some(method) = usize::try_from(method)
            .ok()
            .and_then(|i| self.app.state().step_up.first()?.methods.get(i).copied())
        else {
            return;
        };
        self.act(
            ui,
            Action::StepUpAnswer {
                method,
                code: Secret(code.to_owned()),
            },
        );
    }

    pub fn cancel_step_up(&mut self, ui: &AppWindow) {
        self.act(ui, Action::StepUpCancel);
    }
}
