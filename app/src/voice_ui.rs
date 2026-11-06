// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Local voice form choices; no device access or account I/O.
use godwit_core::application::AppState;
use godwit_core::call::{CallStatus, ScreenShareQuality};
use godwit_core::ids::RoomId;
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
    /// Windows: the screens and the apps' windows listed when it opened.
    #[cfg(windows)]
    pub sources: (Vec<godwit_voice::screen::Source>, Vec<godwit_voice::screen::Source>),
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
            sources: godwit_voice::screen::sources(),
        })
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
