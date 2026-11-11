// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Local voice form choices; no device access or account I/O.
use godwit_core::application::AppState;
use godwit_core::call::{CallStatus, ScreenShareQuality};
use godwit_core::ids::RoomId;
use godwit_voice::dsp::NoiseSuppression;
use godwit_voice::screen::ScreenQuality;
use slint::{ModelRc, SharedString, VecModel};

pub fn device_list(names: &[String], chosen: &Option<String>) -> (ModelRc<SharedString>, i32) {
    let mut labels: Vec<SharedString> = vec!["System default".into()];
    labels.extend(names.iter().map(|n| n.as_str().into()));
    let index = match chosen {
        None => 0,
        Some(name) => match names.iter().position(|n| n == name) {
            Some(i) => i + 1,
            None => {
                labels.push(format!("{name} (unavailable)").into());
                labels.len() - 1
            }
        },
    };
    (ModelRc::new(VecModel::from(labels)), i32::try_from(index).unwrap_or(0))
}

pub fn device_choice(names: &[String], current: &Option<String>, index: i32) -> Option<String> {
    if index == 0 {
        return None;
    }
    usize::try_from(index)
        .ok()
        .and_then(|i| i.checked_sub(1))
        .and_then(|i| names.get(i))
        .cloned()
        .or_else(|| current.clone())
}

/// The noise suppression choice's row in the lists that offer it (Settings,
/// the call controls' microphone menu).
pub fn noise_index(mode: NoiseSuppression) -> i32 {
    match mode {
        NoiseSuppression::Disabled => 0,
        NoiseSuppression::WebRtc => 1,
        NoiseSuppression::Enhanced => 2,
    }
}

pub fn noise_at(index: i32) -> NoiseSuppression {
    match index {
        0 => NoiseSuppression::Disabled,
        1 => NoiseSuppression::WebRtc,
        _ => NoiseSuppression::Enhanced,
    }
}

pub fn quality_index(quality: ScreenQuality) -> i32 {
    ScreenQuality::ALL.iter().position(|q| *q == quality).unwrap_or(0) as i32
}
pub fn quality_at(index: i32) -> ScreenQuality {
    usize::try_from(index)
        .ok()
        .and_then(|i| ScreenQuality::ALL.get(i))
        .copied()
        .unwrap_or(ScreenQuality::Low)
}
pub fn core_quality(quality: ScreenQuality) -> ScreenShareQuality {
    match quality {
        ScreenQuality::Low => ScreenShareQuality::Low,
        ScreenQuality::High => ScreenShareQuality::High,
        ScreenQuality::Text => ScreenShareQuality::Text,
    }
}

pub struct ShareDialog {
    pub room: RoomId,
    pub qualities: Vec<ScreenQuality>,
    /// Windows: the screens and the apps' windows, once listed.
    #[cfg(windows)]
    pub sources: (Vec<godwit_voice::screen::Source>, Vec<godwit_voice::screen::Source>),
    /// Windows: the listing on its way. Asking windows for their titles
    /// waits on each app, so a hung one can't freeze the dialog.
    #[cfg(windows)]
    pub sources_rx:
        Option<std::sync::mpsc::Receiver<(Vec<godwit_voice::screen::Source>, Vec<godwit_voice::screen::Source>)>>,
}
impl ShareDialog {
    pub fn new(state: &AppState) -> Option<Self> {
        let call = state.call.as_ref().filter(|c| c.status == CallStatus::Connected)?;
        let room = state.room(&call.room).filter(|r| r.capabilities.video)?;
        Some(Self {
            room: call.room.clone(),
            qualities: ScreenQuality::ALL
                .into_iter()
                .filter(|q| q.allowed(room.capabilities.video_resolution))
                .collect(),
            #[cfg(windows)]
            sources: (Vec::new(), Vec::new()),
            #[cfg(windows)]
            sources_rx: None,
        })
    }

    /// Windows: starts listing what can be shared; `wake` runs when it's in.
    #[cfg(windows)]
    pub fn list_sources(&mut self, wake: impl Fn() + Send + 'static) {
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("share-sources".into()).spawn(move || {
            let _ = tx.send(godwit_voice::screen::sources());
            wake();
        });
        match spawned {
            Ok(_) => self.sources_rx = Some(rx),
            Err(e) => eprintln!("error: listing screens and windows: {e}"),
        }
    }
    pub fn current(&self, state: &AppState) -> bool {
        state
            .call
            .as_ref()
            .is_some_and(|c| c.room == self.room && c.status == CallStatus::Connected)
            && state.room(&self.room).is_some_and(|r| r.capabilities.video)
    }
    pub fn index(&self, quality: ScreenQuality) -> i32 {
        self.qualities.iter().position(|q| *q == quality).unwrap_or(0) as i32
    }
    pub fn choice(&self, index: i32) -> Option<ScreenQuality> {
        usize::try_from(index).ok().and_then(|i| self.qualities.get(i)).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;
    #[test]
    fn noise_rows_round_trip() {
        for mode in [
            NoiseSuppression::Disabled,
            NoiseSuppression::WebRtc,
            NoiseSuppression::Enhanced,
        ] {
            assert_eq!(noise_at(noise_index(mode)), mode);
        }
        assert_eq!(noise_at(-1), NoiseSuppression::Enhanced);
        assert_eq!(noise_at(9), NoiseSuppression::Enhanced);
    }
    #[test]
    fn missing_devices_stay_selected_until_the_user_changes_them() {
        let names = vec!["Present".into()];
        let chosen = Some("Unplugged".into());
        let (list, index) = device_list(&names, &chosen);
        assert_eq!(index, 2);
        assert_eq!(list.row_data(2).as_deref(), Some("Unplugged (unavailable)"));
        assert_eq!(device_choice(&names, &chosen, index), chosen);
        assert_eq!(device_choice(&names, &chosen, 0), None);
        assert_eq!(device_choice(&names, &chosen, 1).as_deref(), Some("Present"));
        assert_eq!(device_choice(&names, &chosen, -1), chosen);
    }
}
