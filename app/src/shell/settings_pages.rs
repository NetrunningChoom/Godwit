// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Settings: the pages, problems, privacy and sessions, auto-delete, own profiles and the cache.

use super::*;
use slint::ComponentHandle;

impl Shell {
    /// Moves the core's new errors into the log; the rail's dot and Settings
    /// → Problems show them, instead of a pop-up.
    pub(super) fn log_errors(&mut self, ui: &AppWindow) {
        if self.app.state().errors.is_empty() {
            return;
        }
        for error in self.app.state().errors.clone() {
            eprintln!("error: {error}");
            self.errors.push(error);
        }
        let effects = self.app.dispatch(Action::ClearErrors);
        self.run(effects);
        self.show_problems(ui);
    }

    pub(super) fn show_problems(&self, ui: &AppWindow) {
        let items: Vec<ProblemItem> = self
            .errors
            .entries()
            .map(|e| ProblemItem {
                when: e.when.as_str().into(),
                text: e.text.as_ref().into(),
            })
            .collect();
        ui.set_problems(slint::ModelRc::new(slint::VecModel::from(items)));
        ui.set_problems_unseen(i32::try_from(self.errors.unseen()).unwrap_or(i32::MAX));
        ui.set_has_problem_log(self.errors.path().is_some());
    }

    /// Settings → Problems was opened: the rail's dot goes.
    pub fn problems_opened(&mut self, ui: &AppWindow) {
        self.errors.seen();
        self.show_problems(ui);
    }

    pub fn copy_problems(&self) {
        copy_to_clipboard(self.errors.text());
    }

    pub fn open_problem_log(&self) {
        if let Some(path) = self.errors.path()
            && let Err(e) = open::that_detached(path)
        {
            eprintln!("error: opening the problem log: {e}");
        }
    }

    pub fn clear_problems(&mut self, ui: &AppWindow) {
        self.errors.clear();
        self.show_problems(ui);
    }

    /// Settings opened: show the saved values and ask for the devices.
    pub fn settings_opened(&mut self, ui: &AppWindow) {
        let editable = self
            .app
            .state()
            .accounts
            .iter()
            .filter(|a| self.account(&a.id).is_some_and(|a| a.can_edit_profile()))
            .map(|a| a.id.clone())
            .collect();
        self.profile_editor.populate(ui, self.app.state(), editable);
        // Back on the account page it was left on (or the first account's).
        if ui.get_settings_section() == 10 || ui.get_settings_section() == 0 {
            let index = ui.get_settings_account().max(0);
            if usize::try_from(index).is_ok_and(|i| i < self.app.state().accounts.len()) {
                self.open_settings_account(ui, index);
            } else {
                ui.set_settings_section(1);
            }
        }
        // Devices are this computer's: listed here, so they show with any
        // network signed in, or none (it was the Stoat thread's job, and
        // without a Stoat account the lists stayed empty).
        self.list_devices();
        self.devices_shown = false;
        let s = &self.settings;
        ui.set_notifications(s.notifications);
        ui.set_sounds(s.sounds);
        ui.set_muzzle(s.muzzle);
        let labels = |l: &[&str]| -> slint::ModelRc<slint::SharedString> {
            std::rc::Rc::new(slint::VecModel::from(l.iter().map(|&x| x.into()).collect::<Vec<_>>())).into()
        };
        let nearest = |choices: &[u64], secs: u64| {
            choices
                .iter()
                .enumerate()
                .min_by_key(|(_, c)| c.abs_diff(secs))
                .map_or(0, |(i, _)| i32::try_from(i).unwrap_or(0))
        };
        ui.set_muzzle_pauses(labels(&crate::settings::MUZZLE_PAUSE_LABELS));
        ui.set_muzzle_pause_index(nearest(&crate::settings::MUZZLE_PAUSES, s.muzzle_pause_secs));
        ui.set_muzzle_maxes(labels(&crate::settings::MUZZLE_MAX_LABELS));
        ui.set_muzzle_max_index(nearest(&crate::settings::MUZZLE_MAXES, s.muzzle_max_secs));
        ui.set_dnd(s.dnd);
        ui.set_dark(s.dark);
        ui.set_accent_index(
            theme::ACCENTS
                .iter()
                .position(|a| *a == s.accent())
                .map_or(0, |i| i32::try_from(i).unwrap_or(0)),
        );
        ui.set_style_index(i32::try_from(s.style).unwrap_or(0));
        ui.set_emoji_packs(
            std::rc::Rc::new(slint::VecModel::from(
                godwit_core::emoji_packs::PACKS
                    .iter()
                    .map(|(_, name)| (*name).into())
                    .collect::<Vec<slint::SharedString>>(),
            ))
            .into(),
        );
        ui.set_emoji_pack_index(
            godwit_core::emoji_packs::PACKS
                .iter()
                .position(|(id, _)| *id == crate::emoji::pack())
                .and_then(|i| i32::try_from(i).ok())
                .unwrap_or(0),
        );
        ui.set_scales(
            std::rc::Rc::new(slint::VecModel::from(
                crate::scale::LABELS
                    .iter()
                    .map(|&l| l.into())
                    .collect::<Vec<slint::SharedString>>(),
            ))
            .into(),
        );
        ui.set_scale_index(i32::try_from(crate::scale::index_of(s.ui_scale)).unwrap_or(2));
        let tones: Vec<slint::SharedString> = godwit_voice::sounds::Tone::ALL
            .iter()
            .map(|(_, n)| (*n).into())
            .collect();
        ui.set_tones(slint::ModelRc::new(slint::VecModel::from(tones)));
        ui.set_rejoin_offers(labels(&crate::settings::RejoinOffer::ALL.map(|(_, label)| label)));
        ui.set_rejoin_offer_index(i32::try_from(s.rejoin_offer.index()).unwrap_or(0));
        ui.set_tone_index(i32::try_from(s.message_tone).unwrap_or(0));
        ui.set_noise_index(match s.voice.noise_suppression {
            NoiseSuppression::Disabled => 0,
            NoiseSuppression::WebRtc => 1,
            NoiseSuppression::Enhanced => 2,
        });
        ui.set_echo_cancellation(s.voice.echo_cancellation);
        ui.set_auto_gain(s.voice.auto_gain_control);
        ui.set_screen_quality_index(crate::voice_ui::quality_index(s.voice.screen_quality));
        ui.set_screen_quality_ask(s.voice.screen_quality_ask);
        ui.set_memory(
            settings::memory_in_use()
                .map_or_else(|| "unknown".into(), view::human_size)
                .into(),
        );
        self.show_cache_size(ui);
        self.show_devices(ui);
    }

    /// The Discord account still to be asked the first-run privacy question
    /// (signed in, not the demo's, not asked before).
    pub(super) fn privacy_pending(&self) -> Option<AccountId> {
        self.live
            .iter()
            .filter(|a| a.asks_privacy())
            .filter_map(|a| a.id().cloned())
            .find(|id| !self.settings.privacy_asked(id))
    }

    pub(super) fn show_discord_privacy(&self, ui: &AppWindow) {
        ui.set_discord_privacy_open(self.privacy_pending().is_some());
    }

    /// The first-run privacy question answered: turn Discord's data use off,
    /// or leave it as it is. Asked once per account either way.
    pub fn discord_privacy(&mut self, ui: &AppWindow, tighten: bool) {
        let Some(id) = self.privacy_pending() else { return };
        if tighten && let Some(d) = self.live.iter_mut().find(|d| d.id() == Some(&id)) {
            d.tighten_privacy();
        }
        self.settings.mark_privacy_asked(&id);
        self.save_settings();
        self.show_discord_privacy(ui);
    }

    /// An account's page in Settings: its Profile tab when its profile can
    /// be edited here (the editor switched to it), else its Account tab.
    pub fn open_settings_account(&mut self, ui: &AppWindow, index: i32) {
        let Some(id) = usize::try_from(index)
            .ok()
            .and_then(|i| self.app.state().accounts.get(i))
            .map(|a| a.id.clone())
        else {
            return;
        };
        ui.set_settings_section(10);
        ui.set_settings_account(index);
        let account_pages = ui.global::<crate::ui::AccountPages>();
        account_pages.set_confirm_session(-1);
        account_pages.set_rename_session(-1);
        ui.set_settings_data_policy_url(id.network.privacy_url().into());
        ui.set_settings_data_policy_label(format!("Open {} privacy information", id.network.name()).into());
        // The network's own privacy overview, if it has one.
        let section = self.account(&id).and_then(|a| a.privacy_section());
        ui.set_settings_overview_title(section.as_ref().map_or("", |s| s.title.as_str()).into());
        ui.set_settings_overview_caveat(section.as_ref().map_or("", |s| s.caveat.as_str()).into());
        let paras: Vec<crate::ui::PrivPara> = section
            .iter()
            .flat_map(|s| &s.paragraphs)
            .map(|p| crate::ui::PrivPara {
                text: p.text.as_str().into(),
                sources: ModelRc::new(VecModel::from(
                    p.sources
                        .iter()
                        .map(|(l, _)| l.as_str().into())
                        .collect::<Vec<slint::SharedString>>(),
                )),
            })
            .collect();
        ui.set_settings_overview(ModelRc::new(VecModel::from(paras)));
        self.privacy_sources = section
            .iter()
            .flat_map(|s| &s.paragraphs)
            .map(|p| p.sources.iter().map(|(_, u)| u.clone()).collect())
            .collect();
        let row = self.profile_editor.row_of(&id);
        ui.set_settings_account_profile(row.is_some());
        let pages = id.network.account_pages();
        let stoat = pages.contains(&godwit_core::network::AccountPage::AutoDelete);
        ui.set_settings_account_autodelete(stoat);
        self.autodelete_page.account = stoat.then(|| id.clone());
        let discord = pages.contains(&godwit_core::network::AccountPage::PrivacyAndSessions);
        ui.set_settings_account_discord(discord);
        let sessions = pages.contains(&godwit_core::network::AccountPage::Sessions);
        ui.set_settings_account_sessions(sessions);
        ui.set_settings_account_rename_sessions(pages.contains(&godwit_core::network::AccountPage::RenameSessions));
        ui.set_settings_account_log_out_others(pages.contains(&godwit_core::network::AccountPage::LogOutOtherSessions));
        self.account_pages.account = (discord || sessions).then(|| id.clone());
        ui.set_settings_account_tab(if row.is_some() { 0 } else { 1 });
        if let Some(row) = row {
            self.select_profile_account(ui, i32::try_from(row).unwrap_or(0));
        }
    }

    /// Opens one source of the privacy overview in the browser.
    pub fn open_privacy_source(&mut self, para: i32, source: i32) {
        let url = usize::try_from(para)
            .ok()
            .zip(usize::try_from(source).ok())
            .and_then(|(p, s)| self.privacy_sources.get(p)?.get(s));
        if let Some(url) = url {
            open_external(url);
        }
    }

    /// Settings → Privacy opened: ask the account for its switches.
    pub fn open_privacy(&mut self, ui: &AppWindow) {
        if let Some(action) = self.account_pages.open_privacy() {
            self.act(ui, action);
        }
    }

    /// Settings → Sessions opened: ask the account for its devices.
    pub fn open_sessions(&mut self, ui: &AppWindow) {
        if let Some(action) = self.account_pages.open_sessions() {
            self.act(ui, action);
        }
    }

    pub fn toggle_privacy(&mut self, ui: &AppWindow, index: i32) {
        let action = usize::try_from(index)
            .ok()
            .and_then(|i| self.account_pages.toggle(self.app.state(), i));
        if let Some(action) = action {
            self.act(ui, action);
        }
    }

    pub fn log_out_session(&mut self, ui: &AppWindow, index: i32) {
        let action = usize::try_from(index)
            .ok()
            .and_then(|i| self.account_pages.log_out(self.app.state(), i));
        if let Some(action) = action {
            self.act(ui, action);
        }
    }

    pub fn rename_session(&mut self, ui: &AppWindow, index: i32, name: &str) {
        let action = usize::try_from(index)
            .ok()
            .and_then(|i| self.account_pages.rename_session(self.app.state(), i, name));
        if let Some(action) = action {
            self.act(ui, action);
        }
    }

    pub fn log_out_session_with_proof(&mut self, ui: &AppWindow, index: i32, method: i32, response: &str) {
        let method = usize::try_from(method).ok();
        let action = method.and_then(|method| {
            if index == -1 {
                self.account_pages.log_out_others_with_proof(method, response)
            } else {
                usize::try_from(index).ok().and_then(|index| {
                    self.account_pages
                        .log_out_with_proof(self.app.state(), index, method, response)
                })
            }
        });
        if let Some(action) = action {
            self.act(ui, action);
        }
    }

    /// The Auto-delete tab opened: ask the account for its page.
    pub fn open_autodelete(&mut self, ui: &AppWindow) {
        if let Some(account) = self.autodelete_page.account.clone() {
            let action = self.autodelete_page.open(account);
            self.act(ui, action);
        }
    }

    pub(super) fn autodelete_view(&self) -> Option<godwit_core::autodelete::AutoDeleteView> {
        let account = self.autodelete_page.account.as_ref()?;
        self.app.state().autodelete.get(account).cloned()
    }

    /// "New rule" (-1) or a rule's "Edit".
    pub fn autodelete_edit(&mut self, ui: &AppWindow, index: i32) {
        let view = self.autodelete_view();
        self.autodelete_page
            .edit(ui, view.as_ref(), usize::try_from(index).ok());
    }

    pub fn autodelete_toggle_place(&mut self, ui: &AppWindow, index: i32) {
        if let Ok(index) = usize::try_from(index) {
            self.autodelete_page.toggle_place(ui, index);
        }
    }

    pub fn autodelete_toggle_expand(&mut self, ui: &AppWindow, index: i32) {
        if let Ok(index) = usize::try_from(index) {
            self.autodelete_page.toggle_expand(ui, index);
        }
    }

    pub fn autodelete_search(&mut self, ui: &AppWindow, query: String) {
        self.autodelete_page.search(ui, query);
    }

    /// "All DMs and groups" or "whose messages" changed in the editor.
    pub fn autodelete_scope_changed(&mut self, ui: &AppWindow) {
        self.autodelete_page.refresh_places(ui);
    }

    pub fn autodelete_next(&mut self, ui: &AppWindow) {
        self.autodelete_page.next(ui);
    }

    pub fn autodelete_save(&mut self, ui: &AppWindow) {
        if let Some(action) = self.autodelete_page.save(ui) {
            self.act(ui, action);
        }
    }

    /// A command about the rule in row `index`.
    pub fn autodelete_rule(
        &mut self,
        ui: &AppWindow,
        index: i32,
        command: impl FnOnce(u64) -> godwit_core::autodelete::AutoDeleteCommand,
    ) {
        let view = self.autodelete_view();
        let id = usize::try_from(index)
            .ok()
            .and_then(|i| crate::autodelete_page::AutoDeletePage::rule_id(view.as_ref(), i));
        if let Some(action) = id.and_then(|id| self.autodelete_page.act(command(id))) {
            self.act(ui, action);
        }
    }

    pub fn autodelete_command(&mut self, ui: &AppWindow, command: godwit_core::autodelete::AutoDeleteCommand) {
        if let Some(action) = self.autodelete_page.act(command) {
            self.act(ui, action);
        }
    }

    pub fn select_profile_account(&mut self, ui: &AppWindow, row: i32) {
        if let Some(action) = self.profile_editor.select(ui, row) {
            self.act(ui, action);
        }
        self.show_own_profile(ui);
    }

    pub fn reload_own_profile(&mut self, ui: &AppWindow) {
        if !ui.get_edit_profile_busy()
            && let Some(action) = self.profile_editor.reload()
        {
            self.act(ui, action);
        }
    }

    pub fn reset_own_profile(&mut self, ui: &AppWindow) {
        self.profile_editor.reset(ui, self.app.state());
        self.show_own_profile(ui);
    }

    pub fn save_own_profile(&mut self, ui: &AppWindow) {
        if let Some(action) = self.profile_editor.save(ui) {
            self.act(ui, action);
        }
    }

    pub fn own_profile_edited(&mut self, ui: &AppWindow) {
        self.profile_editor.edited(ui);
    }

    pub fn remove_profile_image(&mut self, ui: &AppWindow, avatar: bool) {
        self.profile_editor.remove_image(ui, avatar);
        self.show_own_profile(ui);
    }

    pub fn profile_edit_account(&self) -> Option<AccountId> {
        self.profile_editor.account()
    }

    pub fn profile_image_picked(
        &mut self,
        ui: &AppWindow,
        account: &AccountId,
        avatar: bool,
        result: Result<godwit_core::own_profile::ProfileImage, String>,
    ) {
        self.profile_editor.picked(ui, account, avatar, result);
        self.show_own_profile(ui);
    }

    pub(super) fn show_own_profile(&mut self, ui: &AppWindow) {
        if ui.get_settings_open() && ui.get_settings_section() == 10 && ui.get_settings_account_profile() {
            self.profile_editor.show(ui, self.app.state(), &mut self.images);
        }
    }

    /// Settings → Message cache: what it takes, or why there's none.
    pub(super) fn show_cache_size(&self, ui: &AppWindow) {
        let text = match &self.cache {
            None => "Not in use".to_owned(),
            Some(_) if self.cache_cleared => "Cleared; saving again from the next start".to_owned(),
            Some(cache) => view::human_size(cache.size() + self.images.disk_size()),
        };
        ui.set_cache_size(text.into());
        ui.set_cache_clearable(self.cache.is_some() && !self.cache_cleared);
    }

    /// Settings → Cache → Clear: the saved messages and pictures are deleted
    /// from this device. Rooms open now stay until they're closed.
    pub fn clear_cache(&mut self, ui: &AppWindow) {
        if let Some(cache) = &self.cache {
            cache.clear();
            self.images.clear_disk();
            self.saved_cache.clear();
            self.cache_cleared = true;
        }
        self.show_cache_size(ui);
    }

    /// Something in Settings changed: apply it and save.
    pub fn settings_changed(&mut self, ui: &AppWindow) {
        let devices = self.audio_devices();
        let mut s = self.settings.clone();
        s.notifications = ui.get_notifications();
        s.sounds = ui.get_sounds();
        s.muzzle = ui.get_muzzle();
        let pick = |choices: &[u64], index: i32, old: u64| {
            usize::try_from(index)
                .ok()
                .and_then(|i| choices.get(i).copied())
                .unwrap_or(old)
        };
        s.muzzle_pause_secs = pick(
            &crate::settings::MUZZLE_PAUSES,
            ui.get_muzzle_pause_index(),
            s.muzzle_pause_secs,
        );
        s.muzzle_max_secs = pick(
            &crate::settings::MUZZLE_MAXES,
            ui.get_muzzle_max_index(),
            s.muzzle_max_secs,
        );
        s.dnd = ui.get_dnd();
        s.dark = ui.get_dark();
        if self.devices_shown
            && let Some(devices) = devices
        {
            s.voice.input_device =
                crate::voice_ui::device_choice(&devices.inputs, &s.voice.input_device, ui.get_input_index());
            s.voice.output_device =
                crate::voice_ui::device_choice(&devices.outputs, &s.voice.output_device, ui.get_output_index());
            s.voice.camera = crate::voice_ui::device_choice(&devices.cameras, &s.voice.camera, ui.get_camera_index());
        }
        let tone = usize::try_from(ui.get_tone_index()).unwrap_or(0);
        if tone != s.message_tone && tone < godwit_voice::sounds::Tone::ALL.len() {
            s.message_tone = tone;
            // A preview of the one picked.
            let (preview, _) = godwit_voice::sounds::Tone::ALL[tone];
            self.alerts.sound(&s, godwit_voice::sounds::Sound::MessageTone(preview));
        }
        s.voice.noise_suppression = match ui.get_noise_index() {
            0 => NoiseSuppression::Disabled,
            1 => NoiseSuppression::WebRtc,
            _ => NoiseSuppression::Enhanced,
        };
        if let Some(offer) = crate::settings::RejoinOffer::at(ui.get_rejoin_offer_index()) {
            s.rejoin_offer = offer;
        }
        s.voice.echo_cancellation = ui.get_echo_cancellation();
        s.voice.auto_gain_control = ui.get_auto_gain();
        s.voice.screen_quality = crate::voice_ui::quality_at(ui.get_screen_quality_index());
        s.voice.screen_quality_ask = ui.get_screen_quality_ask();
        let accent = usize::try_from(ui.get_accent_index())
            .ok()
            .and_then(|i| theme::ACCENTS.get(i));
        if let Some(accent) = accent {
            s.accent = if *accent == theme::DEFAULT_ACCENT {
                String::new()
            } else {
                (*accent).to_owned()
            };
        }
        s.style = usize::try_from(ui.get_style_index())
            .unwrap_or(0)
            .min(theme::STYLES.len() - 1);
        if let Some((id, _)) = usize::try_from(ui.get_emoji_pack_index())
            .ok()
            .and_then(|i| godwit_core::emoji_packs::PACKS.get(i))
            && s.emoji_pack != *id
        {
            s.emoji_pack = (*id).to_owned();
            crate::emoji::set_pack(id);
            // Every emoji picture changes: rebuild the rows and the picker.
            for row in &mut self.presenter.rows {
                row.invalidate();
            }
            // (The picker builds its rows, and their addresses, each time it opens.)
            self.presenter.images_changed = true;
        }
        if s.dark != self.settings.dark || s.accent != self.settings.accent || s.style != self.settings.style {
            theme::apply(ui, s.accent(), s.style, s.dark);
        }
        s.ui_scale = usize::try_from(ui.get_scale_index())
            .ok()
            .and_then(|i| crate::scale::CHOICES.get(i).copied())
            .unwrap_or(s.ui_scale);
        self.scale.apply(ui, s.ui_scale);
        if s.voice != self.settings.voice {
            // The next refresh sends them again with unwatched streams muted.
            self.pushed_voice = None;
            for account in &mut self.live {
                account.set_voice_settings(s.voice.clone());
            }
        }
        for account in &mut self.live {
            account.set_notifications(s.notifications || s.sounds);
        }
        self.settings = s;
        self.presenter.muzzle = self.settings.clone();
        if let Some(path) = &self.settings_path
            && let Err(e) = settings::save(path, &self.settings)
        {
            eprintln!("error: saving settings: {e}");
        }
    }

    pub(super) fn save_settings(&self) {
        if let Some(path) = &self.settings_path
            && let Err(e) = settings::save(path, &self.settings)
        {
            eprintln!("error: saving settings: {e}");
        }
    }

    /// A panel was resized (or reset): its width, kept for the next start.
    pub fn panels_resized(&mut self, sidebar: f32, members: f32) {
        self.settings.sidebar_width = sidebar;
        self.settings.members_width = members;
        self.save_settings();
    }
}
