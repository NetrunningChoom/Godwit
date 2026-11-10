// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Audio attachments and voice messages playing in the timeline. One plays at
//! a time. The file is fetched and read on a thread of its own, played by
//! `godwit_voice::voice_message` (decoded only while it plays), and dropped as
//! soon as another starts. A timer ticks only while sound is playing.

use super::*;
use crate::ui::VoicePlayback;
use godwit_voice::voice_message::{Clip, Player};
use slint::ComponentHandle;

/// Playback speeds the toggle cycles through.
const SPEEDS: [(f32, &str); 3] = [(1.0, "1x"), (1.5, "1.5x"), (2.0, "2x")];
/// How often the progress moves while playing.
const TICK: std::time::Duration = std::time::Duration::from_millis(250);

type Loaded = (u64, Result<Clip, String>);

pub(super) struct VoiceMessages {
    /// The playing (or loading) attachment: `{message id}:{index}`, "" for none.
    key: String,
    /// Tells a late fetch from the current one.
    generation: u64,
    player: Option<Player>,
    playing: bool,
    speed: usize,
    volume: f32,
    muted: bool,
    timer: slint::Timer,
    loaded_tx: std::sync::mpsc::Sender<Loaded>,
    loaded: std::sync::mpsc::Receiver<Loaded>,
}

impl VoiceMessages {
    pub(super) fn new() -> Self {
        let (loaded_tx, loaded) = std::sync::mpsc::channel();
        Self {
            key: String::new(),
            generation: 0,
            player: None,
            playing: false,
            speed: 0,
            volume: 1.0,
            muted: false,
            timer: slint::Timer::default(),
            loaded_tx,
            loaded,
        }
    }

    /// Stops whatever plays and frees its recording.
    fn stop(&mut self) {
        self.generation += 1;
        self.key.clear();
        self.player = None;
        self.playing = false;
        self.timer.stop();
    }
}

/// Where a position along the bar (0 to 1, as the pointer gives it) lands in
/// a recording of `duration_ms`; anything unusable clamps to the ends.
fn seek_target_ms(at: f32, duration_ms: u64) -> u64 {
    let at = if at.is_finite() { at.clamp(0.0, 1.0) } else { 0.0 };
    ((f64::from(at) * duration_ms as f64) as u64).min(duration_ms)
}

/// The loudness the player gets: muted is silent, the slider keeps its place.
fn effective_volume(volume: f32, muted: bool) -> f32 {
    if muted { 0.0 } else { volume }
}

/// Splits a key into the message and the attachment's place in it.
fn parse_key(key: &str) -> Option<(&str, usize)> {
    let (id, index) = key.rsplit_once(':')?;
    Some((id, index.parse().ok()?))
}

impl Shell {
    /// A player's play button (or its bar, when that one isn't playing).
    pub fn voice_toggle(&mut self, ui: &AppWindow, key: String) {
        let vm = &mut self.voice_messages;
        if vm.key == key {
            match &vm.player {
                Some(p) if p.finished() => {
                    p.seek_ms(0);
                    p.play();
                    vm.playing = true;
                }
                Some(p) if vm.playing => {
                    p.pause();
                    vm.playing = false;
                }
                Some(p) => {
                    p.play();
                    vm.playing = true;
                }
                // Still loading: cancels.
                None => vm.stop(),
            }
            self.voice_sync(ui);
            return;
        }
        vm.stop();
        let Some(url) = parse_key(&key).and_then(|(id, index)| {
            let m = self.app.state().selected_message(&MessageId(id.into()))?;
            Some(m.attachments.get(index)?.url.clone())
        }) else {
            return self.voice_sync(ui);
        };
        let fetch: Box<dyn FnOnce() -> Result<Vec<u8>, String> + Send> =
            if let Some((_, fetch)) = self.media.iter().find(|(scheme, _)| url.starts_with(scheme.as_str())) {
                let fetch = fetch.clone();
                // The whole file, not a thumbnail.
                let whole = url.split('#').next().unwrap_or(&url).to_owned();
                Box::new(move || fetch(&whole).map_err(|e| e.to_string()))
            } else if url.starts_with("https://") {
                let url = url.to_string();
                Box::new(move || crate::images::fetch_public_media(&url))
            } else {
                return self.voice_sync(ui);
            };
        let vm = &mut self.voice_messages;
        vm.key = key;
        let generation = vm.generation;
        let (tx, wake) = (vm.loaded_tx.clone(), self.inline_wake.clone());
        let spawned = std::thread::Builder::new()
            .name("voice-message-fetch".into())
            .spawn(move || {
                let clip = fetch().and_then(|bytes| Clip::load(bytes).map_err(|e| format!("{e:#}")));
                let _ = tx.send((generation, clip));
                wake();
            });
        if let Err(error) = spawned {
            eprintln!("error: playing a voice message: {error}");
            self.voice_messages.stop();
        }
        self.voice_sync(ui);
    }

    /// A press or drag on the bar of the playing one: `at` is 0 to 1 along it.
    /// The whole recording is in memory, so a seek is a position change.
    pub fn voice_seek(&mut self, ui: &AppWindow, key: String, at: f32) {
        let vm = &mut self.voice_messages;
        if vm.key != key {
            return;
        }
        let Some(p) = &vm.player else { return };
        p.seek_ms(seek_target_ms(at, p.duration_ms()));
        // Seeking a finished one plays on from there.
        if !vm.playing && p.finished() {
            p.play();
            vm.playing = true;
        }
        self.voice_sync(ui);
    }

    pub fn voice_cycle_speed(&mut self, ui: &AppWindow) {
        let vm = &mut self.voice_messages;
        vm.speed = (vm.speed + 1) % SPEEDS.len();
        if let Some(p) = &vm.player {
            p.set_speed(SPEEDS[vm.speed].0);
        }
        self.voice_sync(ui);
    }

    pub fn voice_set_volume(&mut self, ui: &AppWindow, volume: f32) {
        let vm = &mut self.voice_messages;
        vm.volume = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        // Moving the slider unmutes.
        vm.muted = false;
        if let Some(p) = &vm.player {
            p.set_volume(vm.volume);
        }
        self.voice_sync(ui);
    }

    pub fn voice_mute(&mut self, ui: &AppWindow, muted: bool) {
        let vm = &mut self.voice_messages;
        vm.muted = muted;
        if let Some(p) = &vm.player {
            p.set_volume(effective_volume(vm.volume, muted));
        }
        self.voice_sync(ui);
    }

    /// Takes in a finished fetch, notices the end of the recording and moves
    /// the progress. Cheap when nothing plays. True when something changed.
    pub(super) fn poll_voice_messages(&mut self, ui: &AppWindow) -> bool {
        let mut changed = false;
        while let Ok((generation, loaded)) = self.voice_messages.loaded.try_recv() {
            changed = true;
            let vm = &mut self.voice_messages;
            if generation != vm.generation {
                continue;
            }
            match loaded {
                Ok(clip) if vm.player.is_none() => {
                    let (tx, wake) = (vm.loaded_tx.clone(), self.inline_wake.clone());
                    let failed = move |error: String| {
                        let _ = tx.send((generation, Err(error)));
                        wake();
                    };
                    let device = self.settings.voice.output_device.clone();
                    match Player::start(
                        clip,
                        device,
                        SPEEDS[vm.speed].0,
                        effective_volume(vm.volume, vm.muted),
                        failed,
                    ) {
                        Ok(player) => {
                            vm.player = Some(player);
                            vm.playing = true;
                        }
                        Err(error) => {
                            eprintln!("error: playing a voice message: {error:#}");
                            vm.stop();
                        }
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!("error: playing a voice message: {error}");
                    vm.stop();
                }
            }
        }
        let vm = &mut self.voice_messages;
        if let Some(p) = &vm.player
            && vm.playing
            && p.finished()
        {
            // Quiet at the end: the stream stops until it is played again.
            p.pause();
            vm.playing = false;
            changed = true;
        }
        if vm.player.is_some() || changed {
            self.voice_sync(ui);
        }
        changed
    }

    /// Shows the state in the players and runs the tick only while playing.
    fn voice_sync(&mut self, ui: &AppWindow) {
        let vm = &mut self.voice_messages;
        let global = ui.global::<VoicePlayback>();
        let (state, progress, time) = match &vm.player {
            None if vm.key.is_empty() => (0, 0.0, String::new()),
            None => (1, 0.0, String::new()),
            Some(p) if p.finished() => (0, 0.0, String::new()),
            Some(p) => {
                let (at, total) = (p.position_ms(), p.duration_ms());
                (
                    if vm.playing { 2 } else { 3 },
                    if total == 0 {
                        0.0
                    } else {
                        (at as f32 / total as f32).min(1.0)
                    },
                    format!("{} / {}", crate::view::clock(at), crate::view::clock(total)),
                )
            }
        };
        global.set_key(vm.key.as_str().into());
        global.set_state(state);
        global.set_progress(progress);
        global.set_time(time.into());
        global.set_speed(SPEEDS[vm.speed].1.into());
        global.set_volume(vm.volume);
        global.set_muted(vm.muted);
        if state == 2 {
            let wake = self.wake.clone();
            // Already running: leave it be, so its beat stays steady.
            if !vm.timer.running() {
                vm.timer.start(slint::TimerMode::Repeated, TICK, move || wake());
            }
        } else {
            vm.timer.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{effective_volume, parse_key, seek_target_ms};

    #[test]
    fn keys_split_into_message_and_place() {
        assert_eq!(parse_key("12345:2"), Some(("12345", 2)));
        // A message id may hold colons; the index is after the last.
        assert_eq!(parse_key("a:b:0"), Some(("a:b", 0)));
        assert_eq!(parse_key("nothing"), None);
        assert_eq!(parse_key("x:y"), None);
    }

    #[test]
    fn seek_maps_the_bar_onto_the_recording() {
        assert_eq!(seek_target_ms(0.0, 10_000), 0);
        assert_eq!(seek_target_ms(0.5, 10_000), 5_000);
        assert_eq!(seek_target_ms(1.0, 10_000), 10_000);
        // A drag past the ends, or a bad value, stays inside.
        assert_eq!(seek_target_ms(-0.3, 10_000), 0);
        assert_eq!(seek_target_ms(1.7, 10_000), 10_000);
        assert_eq!(seek_target_ms(f32::NAN, 10_000), 0);
        assert_eq!(seek_target_ms(0.5, 0), 0);
    }

    #[test]
    fn mute_silences_without_losing_the_level() {
        assert_eq!(effective_volume(0.4, true), 0.0);
        assert_eq!(effective_volume(0.4, false), 0.4);
    }
}
