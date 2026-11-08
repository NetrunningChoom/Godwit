// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Checks dialog dismissal through real off-screen pointer dispatch: the
//! extracted components must update AppWindow's state and route callbacks.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use godwit::ui::{AppWindow, Screen};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, Rgb8Pixel};

struct Offscreen(Rc<MinimalSoftwareWindow>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

#[test]
fn extracted_dialogs_preserve_dismissal_and_parent_state() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window.clone())))?;
    let ui = AppWindow::new()?;
    ui.set_screen(Screen::Main);
    ui.show()?;
    window.set_size(PhysicalSize::new(800, 600));
    let clicks = Rc::new(RefCell::new(Vec::new()));
    let observed = clicks.clone();
    ui.on_quiet_notice(move |on| observed.borrow_mut().push(on));
    let destructive = Rc::new(Cell::new(0));
    let observed = destructive.clone();
    ui.on_delete(move |_| observed.set(observed.get() + 1));
    let observed = destructive.clone();
    ui.on_server_menu_leave(move || observed.set(observed.get() + 1));
    let observed = destructive.clone();
    ui.on_log_out_account(move |_| observed.set(observed.get() + 1));
    let observed = destructive.clone();
    ui.on_moderate_member(move |_, _, _| observed.set(observed.get() + 1));
    let mut pixels = vec![Rgb8Pixel::default(); 800 * 600];
    let mut dismiss = || {
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, 800);
        });
        let position = LogicalPosition::new(4.0, 4.0);
        let button = PointerEventButton::Left;
        window.dispatch_event(WindowEvent::PointerMoved { position });
        window.dispatch_event(WindowEvent::PointerPressed { position, button });
        window.dispatch_event(WindowEvent::PointerReleased { position, button });
    };

    ui.set_confirm_delete(0);
    dismiss();
    assert_eq!(ui.get_confirm_delete(), -1, "deletion cancellation reaches AppWindow");
    ui.set_confirm_leave(true);
    dismiss();
    assert!(!ui.get_confirm_leave());
    ui.set_confirm_log_out(0);
    dismiss();
    assert_eq!(ui.get_confirm_log_out(), -2);
    ui.set_confirm_moderate(2);
    dismiss();
    assert_eq!(ui.get_confirm_moderate(), 0);
    ui.set_creating_channel(true);
    dismiss();
    assert!(!ui.get_creating_channel());
    assert_eq!(destructive.get(), 0, "backdrop dismissal must never confirm an action");

    ui.set_in_call(true);
    ui.set_camera_confirm(true);
    dismiss();
    assert!(!ui.get_camera_confirm(), "camera cancellation reaches AppWindow");
    ui.set_quiet_notice_open(true);
    dismiss();
    assert_eq!(&*clicks.borrow(), &[false], "backdrop dismissal keeps quiet mode");
    Ok(())
}
