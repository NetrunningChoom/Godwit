// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Calls: the mic test, devices, screen sharing, streams' sound, incoming calls and rejoining.

use super::*;

impl Shell {
    pub fn play_call_sound(&mut self, ui: &AppWindow, index: i32) {
        let Some(call) = self.app.state().call.as_ref() else {
            return;
        };
        if call.status != godwit_core::call::CallStatus::Connected {
            return;
        }
        let Some(sound) = usize::try_from(index)
            .ok()
            .and_then(|i| call.sounds.get(i))
            .map(|s| s.id.clone())
        else {
            return;
        };
        self.act(ui, Action::PlaySound { sound });
    }

    pub fn load_call_sounds(&mut self, ui: &AppWindow) {
        if self
            .app
            .state()
            .call
            .as_ref()
            .is_some_and(|call| call.status == godwit_core::call::CallStatus::Connected)
        {
            self.act(ui, Action::LoadCallSounds);
        }
    }

    /// Starts or stops the test call (Settings → Voice), with the saved
    /// devices and processing; its level shows 20 times a second.
    pub fn mic_test(&mut self, ui: &AppWindow, on: bool) {
        self.mic_test = None;
        ui.set_mic_testing(false);
        ui.set_mic_level(0.0);
        if !on {
            return;
        }
        let test = match godwit_voice::mictest::MicTest::start(&self.settings.voice) {
            Ok(test) => test,
            Err(e) => {
                eprintln!("error: test call: {e:#}");
                self.errors.push(format!("The test call couldn't start: {e:#}").into());
                ui.set_problems_unseen(i32::try_from(self.errors.unseen()).unwrap_or(i32::MAX));
                return;
            }
        };
        use slint::ComponentHandle;
        let timer = slint::Timer::default();
        let weak = ui.as_weak();
        let shown = std::cell::Cell::new(0.0f32);
        let reader = test_level_reader(&test);
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(50),
            move || {
                let l = reader();
                // Falls back slowly, as meters do, so short sounds are seen.
                let v = l.max(shown.get() * 0.85);
                shown.set(v);
                if let Some(ui) = weak.upgrade() {
                    ui.set_mic_level(v);
                }
            },
        );
        self.mic_test = Some((test, timer));
        ui.set_mic_testing(true);
    }

    /// A stream's volume or mute changed in the call: kept per person (as
    /// Settings keeps screen-share volumes) and given to the calls at once.
    /// Someone in our call: their volume here (0..2) and whether they're
    /// muted for us only. Kept between starts, for every network's calls.
    /// Sets someone's volume and mute. While a slider is still dragged
    /// (`done` false) the change only goes to the calls, so it's heard
    /// at once; saving and redrawing wait for the release.
    pub fn user_sound(&mut self, ui: &AppWindow, index: i32, volume: f32, muted: bool, done: bool) {
        let Some(user) = usize::try_from(index).ok().and_then(|i| {
            self.app
                .state()
                .call
                .as_ref()?
                .members
                .get(i)
                .map(|m| m.user.raw.to_string())
        }) else {
            return;
        };
        let voice = &mut self.settings.voice;
        voice.user_volumes.insert(user.clone(), volume.clamp(0.0, 2.0));
        voice.muted_users.retain(|u| *u != user);
        if muted {
            voice.muted_users.push(user);
        }
        self.push_voice_settings();
        if done {
            self.save_settings();
            self.refresh(ui);
        }
    }

    /// Sends the voice settings to every account's calls.
    pub(super) fn push_voice_settings(&mut self) {
        // A stream's sound plays only while it's watched: a saved "unmuted"
        // otherwise let it play to someone who never opened the stream.
        let mut voice = self.settings.voice.clone();
        if let Some(call) = self.app.state().call.as_ref() {
            for stream in call.streams.iter().filter(|s| !s.watching) {
                voice.screen_share_mutes.insert(stream.user.raw.to_string(), true);
            }
        }
        if self.pushed_voice.as_ref() == Some(&voice) {
            return;
        }
        self.pushed_voice = Some(voice.clone());
        for account in &mut self.live {
            account.set_voice_settings(voice.clone());
        }
    }

    /// As [`Self::user_sound`], for a stream's sound.
    pub fn stream_sound(&mut self, ui: &AppWindow, index: i32, volume: f32, muted: bool, done: bool) {
        let Some(user) = usize::try_from(index).ok().and_then(|i| {
            self.app
                .state()
                .call
                .as_ref()?
                .streams
                .get(i)
                .map(|s| s.user.raw.to_string())
        }) else {
            return;
        };
        self.settings
            .voice
            .screen_share_volumes
            .insert(user.clone(), volume.clamp(0.0, 2.0));
        self.settings.voice.screen_share_mutes.insert(user, muted);
        let voice = self.settings.voice.clone();
        for account in &mut self.live {
            account.set_voice_settings(voice.clone());
        }
        if done {
            self.save_settings();
            self.refresh(ui);
        }
    }

    /// Lists the devices off the UI thread (WASAPI/ALSA enumeration and
    /// camera probing can take a moment); they arrive with a later poll.
    pub(super) fn list_devices(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel();
        let wake = self.inline_wake.clone();
        let spawned =
            std::thread::Builder::new().name("list-devices".into()).spawn(
                move || match godwit_voice::call::list_devices() {
                    Ok(devices) => {
                        let _ = tx.send(devices);
                        wake();
                    }
                    Err(e) => eprintln!("error: listing audio devices: {e}"),
                },
            );
        match spawned {
            Ok(_) => self.devices_rx = Some(rx),
            Err(e) => eprintln!("error: listing audio devices: {e}"),
        }
    }

    pub(super) fn audio_devices(&self) -> Option<&godwit_voice::call::AudioDevices> {
        if self.demo.is_some() {
            static DEMO_DEVICES: std::sync::LazyLock<godwit_voice::call::AudioDevices> =
                std::sync::LazyLock::new(|| godwit_voice::call::AudioDevices {
                    inputs: vec!["Demo microphone".into()],
                    outputs: vec!["Demo speakers".into()],
                    cameras: vec!["Demo camera".into(), "Demo camera 2".into()],
                });
            Some(&DEMO_DEVICES)
        } else {
            self.devices.as_ref()
        }
    }

    pub fn set_screen_share(&mut self, ui: &AppWindow, active: bool) {
        if !active {
            self.cancel_screen_share(ui);
            self.act(ui, Action::SetScreenShare(false));
            return;
        }
        let Some(dialog) = crate::voice_ui::ShareDialog::new(self.app.state()) else {
            return;
        };
        #[cfg(windows)]
        let mut dialog = dialog;
        // Windows always asks: there's no system picker for the screen or window.
        if self.settings.voice.screen_quality_ask || godwit_voice::screen::LISTS_SOURCES {
            #[cfg(windows)]
            {
                let wake = self.inline_wake.clone();
                dialog.list_sources(move || wake());
                ui.set_share_pick_source(true);
                set_share_sources(ui, &dialog.sources);
                ui.set_share_source_tab(0);
                ui.set_share_source_index(0);
            }
            ui.set_share_qualities(ModelRc::new(VecModel::from(
                dialog
                    .qualities
                    .iter()
                    .map(|q| SharedString::from(q.label()))
                    .collect::<Vec<_>>(),
            )));
            ui.set_share_quality_index(dialog.index(self.settings.voice.screen_quality));
            ui.set_share_remember(false);
            self.share_dialog = Some(dialog);
            self.show_share_dialog(ui);
        } else {
            self.act(
                ui,
                Action::StartScreenShare {
                    room: dialog.room,
                    quality: crate::voice_ui::core_quality(self.settings.voice.screen_quality),
                },
            );
        }
    }
    pub fn cancel_screen_share(&mut self, ui: &AppWindow) {
        self.share_dialog = None;
        ui.set_share_dialog_open(false);
    }
    pub fn confirm_screen_share(&mut self, ui: &AppWindow) {
        let Some(dialog) = self.share_dialog.take().filter(|d| d.current(self.app.state())) else {
            ui.set_share_dialog_open(false);
            return;
        };
        ui.set_share_dialog_open(false);
        let Some(quality) = dialog.choice(ui.get_share_quality_index()) else {
            return;
        };
        // Windows: what to capture, from Godwit's own list.
        #[cfg(windows)]
        {
            let list = if ui.get_share_source_tab() == 0 {
                &dialog.sources.0
            } else {
                &dialog.sources.1
            };
            let Some(source) = usize::try_from(ui.get_share_source_index())
                .ok()
                .and_then(|i| list.get(i))
            else {
                return;
            };
            godwit_voice::screen::pick(Some(source.clone()));
        }
        // Official ScreenShareSettings: save the choice only with Don't ask again.
        if ui.get_share_remember() {
            self.settings.voice.screen_quality = quality;
            self.settings.voice.screen_quality_ask = false;
            self.save_settings();
        }
        self.act(
            ui,
            Action::StartScreenShare {
                room: dialog.room,
                quality: crate::voice_ui::core_quality(quality),
            },
        );
    }
    pub(super) fn show_share_dialog(&mut self, ui: &AppWindow) {
        if self.share_dialog.as_ref().is_some_and(|d| !d.current(self.app.state())) {
            self.share_dialog = None;
        }
        #[cfg(windows)]
        if let Some(dialog) = self.share_dialog.as_mut()
            && let Some(sources) = dialog.sources_rx.as_ref().and_then(|rx| rx.try_recv().ok())
        {
            dialog.sources = sources;
            dialog.sources_rx = None;
            set_share_sources(ui, &dialog.sources);
        }
        ui.set_share_dialog_open(self.share_dialog.is_some());
    }

    /// The audio device lists, once they've arrived.
    pub(super) fn show_devices(&mut self, ui: &AppWindow) {
        if let Some(devices) = self.devices_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.devices = Some(devices);
            self.devices_rx = None;
            self.devices_shown = false;
        }
        if self.devices_shown {
            return;
        }
        let Some(d) = self.audio_devices() else { return };
        let (inputs, input_index) = crate::voice_ui::device_list(&d.inputs, &self.settings.voice.input_device);
        let (outputs, output_index) = crate::voice_ui::device_list(&d.outputs, &self.settings.voice.output_device);
        let (cameras, camera_index) = crate::voice_ui::device_list(&d.cameras, &self.settings.voice.camera);
        ui.set_inputs(inputs);
        ui.set_input_index(input_index);
        ui.set_outputs(outputs);
        ui.set_output_index(output_index);
        ui.set_cameras(cameras);
        ui.set_camera_index(camera_index);
        self.devices_shown = true;
    }

    /// The identity in the open chat's network switcher, row `row`.
    pub fn talk_on_at(&self, row: i32) -> Option<UserId> {
        self.presenter.talk_on_rows.get(usize::try_from(row).ok()?).cloned()
    }

    pub(super) fn drop_incoming(&mut self, room: &RoomId) {
        if self.incoming_call.as_ref().is_some_and(|(r, _, _)| r == room) {
            self.incoming_call = None;
        }
    }

    pub(super) fn show_incoming_call(&mut self, ui: &AppWindow) {
        // Joining a ringing call from its chat (not the popup) answers it too:
        // the ringtone and popup go.
        if let Some(room) = self.app.state().call.as_ref().map(|c| c.room.clone()) {
            self.alerts.done(&room);
            self.drop_incoming(&room);
        }
        let Some((room, name, _)) = &self.incoming_call else {
            ui.set_incoming_call_open(false);
            return;
        };
        // The caller's picture: the chat's (a DM's icon is the other person).
        let icon = self.app.state().room(room).and_then(|r| r.icon_url.clone());
        let picture = icon.and_then(|url| self.images.get(&url, crate::images::Fit::Circle(160)));
        ui.set_incoming_call_has_avatar(picture.is_some());
        if let Some(picture) = picture {
            ui.set_incoming_call_avatar(picture);
        }
        let initial: String = name
            .chars()
            .next()
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_default();
        ui.set_incoming_call_initials(initial.into());
        ui.set_incoming_call_name(name.as_str().into());
        // Already in a call: answering this one leaves it (one call at a time).
        let switches = self.app.state().call.as_ref().is_some_and(|c| c.room != *room);
        let network = room.account.network.name();
        ui.set_incoming_call_body(
            if switches {
                format!("Incoming voice call · {network} · answering ends your current call")
            } else {
                format!("Incoming voice call · {network}")
            }
            .into(),
        );
        ui.set_incoming_call_switches(switches);
        ui.set_incoming_call_open(true);
    }

    /// The incoming-call popup answered: accept opens the chat and joins
    /// its call (which answers it), decline turns it down on the network,
    /// and dismissing only hides the popup and stops the ringtone (the call
    /// can still be joined from the chat while it rings).
    pub fn incoming_call(&mut self, ui: &AppWindow, answer: i32) {
        let Some((room, _, _)) = self.incoming_call.take() else {
            return;
        };
        self.alerts.done(&room);
        match answer {
            1 => {
                self.act(ui, Action::SelectRoom(room.clone()));
                self.act(ui, Action::JoinCall(room));
            }
            2 => self.act(ui, Action::DeclineCall(room)),
            _ => {}
        }
        self.show_incoming_call(ui);
    }

    /// Keep `last_call` on the call we're connected to (none: cleared), and
    /// show the rejoin offer while one waits. The saved call isn't touched
    /// while the offer is open, so it survives another crash before answering.
    pub(super) fn remember_call(&mut self, ui: &AppWindow) {
        let state = self.app.state();
        // In a call again (this one or another): nothing to offer.
        if state.call.is_some() {
            self.rejoin = None;
        }
        let offer = self.rejoin.as_ref().and_then(|id| state.room(id));
        ui.set_rejoin_open(offer.is_some() && state.call.is_none());
        ui.set_rejoin_room(offer.map(|r| r.name.to_string()).unwrap_or_default().into());
        if self.rejoin.is_some() {
            return;
        }
        let now = state
            .call
            .as_ref()
            .filter(|c| c.status == CallStatus::Connected)
            .map(|c| settings::room_key(&c.room));
        if now != self.settings.last_call {
            self.settings.last_call = now;
            self.save_settings();
        }
    }

    /// The rejoin offer answered: join the call again, or let it go.
    pub fn rejoin_call(&mut self, ui: &AppWindow, join: bool) {
        let Some(room) = self.rejoin.take() else { return };
        if join {
            self.act(ui, Action::SelectRoom(room.clone()));
            self.act(ui, Action::JoinCall(room));
        }
        self.refresh(ui);
    }

    /// Godwit is closing normally: the call we're in is offered next time
    /// only if the user wants that after a normal quit too.
    pub fn closing(&mut self) {
        if self.settings.rejoin_offer != settings::RejoinOffer::EveryQuit && self.settings.last_call.take().is_some() {
            self.save_settings();
        }
    }

    /// The call's room, to open from the call bar.
    pub fn call_room(&self) -> Option<RoomId> {
        self.app.state().call.as_ref().map(|c| c.room.clone())
    }
}

/// The share dialog's lists of screens and windows.
#[cfg(windows)]
fn set_share_sources(ui: &AppWindow, sources: &(Vec<godwit_voice::screen::Source>, Vec<godwit_voice::screen::Source>)) {
    let titles = |list: &[godwit_voice::screen::Source]| {
        ModelRc::new(VecModel::from(
            list.iter()
                .map(|s| SharedString::from(s.title.as_str()))
                .collect::<Vec<_>>(),
        ))
    };
    ui.set_share_screens(titles(&sources.0));
    ui.set_share_windows(titles(&sources.1));
}
