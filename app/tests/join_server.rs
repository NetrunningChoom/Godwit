// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors

use std::rc::Rc;
use std::sync::Arc;

use godwit::shell::Shell;
use godwit::ui::AppWindow;
use slint::ComponentHandle;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};

struct Offscreen(Rc<MinimalSoftwareWindow>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

#[test]
fn demo_invites_and_profile_settings_preserve_their_state() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window)))?;
    let ui = AppWindow::new()?;
    ui.window().set_size(slint::PhysicalSize::new(800, 600));
    // Demo never loads saved sessions or contacts, and has no backend thread.
    let mut shell = Shell::new(&ui, Arc::new(|| {}), true)?;
    shell.open_join_server(&ui);
    assert!(ui.get_join_server_open());
    assert!(!ui.get_join_server_ready());

    shell.inspect_invite(&ui, 0, "https://example.com/not-a-stoat-invite");
    assert!(ui.get_join_server_failed());
    assert!(!ui.get_join_server_ready());
    shell.accept_server_invite(&ui);
    assert!(ui.get_join_server_open());

    shell.inspect_invite(&ui, 0, "https://stt.gg/DemoJoin");
    assert!(!ui.get_join_server_failed());
    assert!(ui.get_join_server_ready());
    assert!(!ui.get_join_server_name().is_empty());
    shell.reset_join_server(&ui);
    assert!(!ui.get_join_server_ready(), "editing must invalidate the old preview");
    shell.accept_server_invite(&ui);
    assert!(ui.get_join_server_open());

    shell.inspect_invite(&ui, 0, "DemoJoin");
    shell.accept_server_invite(&ui);
    assert!(!ui.get_join_server_open(), "successful joining dismisses the dialog");

    shell.open_join_server(&ui);
    shell.inspect_invite(&ui, 0, "DemoMine");
    assert!(ui.get_join_server_ready());
    shell.accept_server_invite(&ui);
    assert!(!ui.get_join_server_open(), "an already joined server opens immediately");

    ui.set_settings_open(true);
    shell.settings_opened(&ui);
    // The first account's page opens on its Profile tab.
    shell.open_settings_account(&ui, 0);
    assert!(ui.get_settings_account_profile());
    assert!(ui.get_edit_profile_loaded());
    assert!(!ui.get_edit_profile_busy());
    ui.set_edit_profile_name("Changed name".into());
    ui.set_edit_profile_pronouns("she/her".into());
    ui.set_edit_profile_bio("**Hello**\nA second line.".into());
    shell.own_profile_edited(&ui);
    shell.save_own_profile(&ui);
    assert_eq!(ui.get_edit_profile_status().as_str(), "Profile saved.");
    assert_eq!(ui.get_account_name().as_str(), "Changed name");
    assert_eq!(ui.get_edit_profile_bio().as_str(), "**Hello**\nA second line.");
    ui.set_edit_profile_name("Unsaved draft".into());
    shell.own_profile_edited(&ui);
    shell.pump(&ui);
    assert_eq!(
        ui.get_edit_profile_name().as_str(),
        "Unsaved draft",
        "a pump cannot overwrite an unsaved form"
    );
    shell.reset_own_profile(&ui);
    assert_eq!(ui.get_edit_profile_name().as_str(), "Changed name");
    shell.remove_profile_image(&ui, true);
    shell.save_own_profile(&ui);
    assert!(!ui.get_edit_profile_has_avatar());
    Ok(())
}
