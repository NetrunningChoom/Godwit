// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors

use godwit::shell::Shell;
use godwit::ui::AppWindow;
use godwit_core::application::Action;
use godwit_core::ids::RoomId;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, Model};
use std::rc::Rc;
use std::sync::Arc;

struct Offscreen(Rc<MinimalSoftwareWindow>);
impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

#[test]
fn demo_voice_preferences_and_share_confirmation_follow_the_current_call() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window)))?;
    let ui = AppWindow::new()?;
    ui.window().set_size(slint::PhysicalSize::new(640, 600));
    let mut shell = Shell::new(&ui, Arc::new(|| {}), true)?;
    let room = RoomId::new(godwit::demo::account(), "voice");
    shell.act(&ui, Action::SelectRoom(room.clone()));
    shell.act(&ui, Action::JoinCall(room.clone()));
    assert!(ui.get_call_can_video());

    ui.set_settings_section(2);
    shell.settings_opened(&ui);
    assert_eq!(
        ui.get_cameras().row_count(),
        3,
        "default and two invented cameras; no device enumeration"
    );
    ui.set_camera_index(2);
    ui.set_screen_quality_index(2);
    ui.set_screen_quality_ask(true);
    shell.settings_changed(&ui);
    shell.settings_opened(&ui);
    assert_eq!(ui.get_camera_index(), 2);
    assert_eq!(ui.get_screen_quality_index(), 2);

    shell.set_screen_share(&ui, true);
    assert!(ui.get_share_dialog_open());
    assert!(!ui.get_call_screen_share());
    assert_eq!(ui.get_share_qualities().row_count(), 1, "this demo room permits 720p");
    shell.cancel_screen_share(&ui);
    shell.confirm_screen_share(&ui);
    assert!(!ui.get_call_screen_share());

    shell.set_screen_share(&ui, true);
    ui.set_share_quality_index(0);
    shell.confirm_screen_share(&ui);
    assert!(ui.get_call_screen_share());
    shell.settings_opened(&ui);
    assert_eq!(
        ui.get_screen_quality_index(),
        2,
        "one-off quality must not replace the preference"
    );
    assert!(ui.get_screen_quality_ask());
    shell.set_screen_share(&ui, false);

    shell.set_screen_share(&ui, true);
    ui.set_share_remember(true);
    shell.confirm_screen_share(&ui);
    shell.settings_opened(&ui);
    assert_eq!(ui.get_screen_quality_index(), 0);
    assert!(!ui.get_screen_quality_ask());
    shell.set_screen_share(&ui, false);
    shell.set_screen_share(&ui, true);
    assert!(
        !ui.get_share_dialog_open(),
        "remembered choices bypass only the quality dialog"
    );
    assert!(ui.get_call_screen_share());
    shell.set_screen_share(&ui, false);

    ui.set_screen_quality_ask(true);
    shell.settings_changed(&ui);
    shell.set_screen_share(&ui, true);
    shell.act(&ui, Action::LeaveCall);
    assert!(!ui.get_share_dialog_open());
    shell.act(&ui, Action::JoinCall(room));
    shell.confirm_screen_share(&ui);
    assert!(
        !ui.get_call_screen_share(),
        "an old confirmation cannot start sharing after rejoining the same room"
    );
    shell.set_screen_share(&ui, true);
    ui.set_share_quality_index(99);
    shell.confirm_screen_share(&ui);
    assert!(!ui.get_call_screen_share());
    Ok(())
}
