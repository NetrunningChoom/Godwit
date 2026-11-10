// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Up/Down in a multi-line draft: off-screen key dispatch into the composer.

use std::rc::Rc;

use godwit::ui::{AppWindow, Screen};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Key, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, Rgb8Pixel, SharedString};

struct Offscreen(Rc<MinimalSoftwareWindow>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn key(window: &MinimalSoftwareWindow, text: SharedString) {
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

#[test]
fn arrows_move_between_draft_lines() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window.clone())))?;
    let ui = AppWindow::new()?;
    ui.set_screen(Screen::Main);
    ui.set_has_room(true);
    ui.set_can_send(true);
    ui.set_draft("aaaa\naaaa\naaaa".into());
    ui.show()?;
    window.set_size(PhysicalSize::new(800, 600));
    let mut pixels = vec![Rgb8Pixel::default(); 800 * 600];
    let mut draw = || {
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, 800);
        });
    };
    draw();
    // Click somewhere in the composer, near the window's bottom.
    let mut focused = false;
    for y in (450..600).rev().step_by(6) {
        let position = LogicalPosition::new(450.0, y as f32);
        let button = PointerEventButton::Left;
        window.dispatch_event(WindowEvent::PointerMoved { position });
        window.dispatch_event(WindowEvent::PointerPressed { position, button });
        window.dispatch_event(WindowEvent::PointerReleased { position, button });
        draw();
        key(&window, "Q".into());
        if ui.get_draft().contains('Q') {
            println!("focused at y={y}: {:?}", ui.get_draft());
            focused = true;
            break;
        }
    }
    assert!(focused, "composer never took focus");
    let clean = ui.get_draft().replace('Q', "");
    ui.set_draft(clean.into());
    draw();
    window.dispatch_event(WindowEvent::KeyPressed {
        text: Key::Control.into(),
    });
    key(&window, Key::End.into());
    window.dispatch_event(WindowEvent::KeyReleased {
        text: Key::Control.into(),
    });
    draw();
    key(&window, Key::UpArrow.into());
    draw();
    key(&window, "X".into());
    draw();
    println!("after Up: {:?}", ui.get_draft());
    key(&window, Key::DownArrow.into());
    draw();
    key(&window, "Y".into());
    println!("after Down: {:?}", ui.get_draft());
    assert_eq!(ui.get_draft(), "aaaa\naaaaX\naaaaY");
    Ok(())
}

#[test]
fn sent_text_stays_cleared() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window.clone())))?;
    let ui = AppWindow::new()?;
    ui.set_screen(Screen::Main);
    ui.set_has_room(true);
    ui.set_can_send(true);
    let edits = Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let seen = edits.clone();
    ui.on_draft_edited(move |t| seen.borrow_mut().push(t.to_string()));
    let weak = ui.as_weak();
    ui.on_send(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_draft("".into());
        }
    });
    ui.show()?;
    window.set_size(PhysicalSize::new(800, 600));
    let mut pixels = vec![Rgb8Pixel::default(); 800 * 600];
    let mut draw = || {
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, 800);
        });
    };
    draw();
    let position = LogicalPosition::new(450.0, 551.0);
    let button = PointerEventButton::Left;
    window.dispatch_event(WindowEvent::PointerPressed { position, button });
    window.dispatch_event(WindowEvent::PointerReleased { position, button });
    draw();
    for c in ["h", "e", "l", "l", "o"] {
        key(&window, c.into());
    }
    draw();
    key(&window, "\n".into());
    draw();
    draw();
    println!("draft {:?}, edits {:?}", ui.get_draft(), edits.borrow());
    assert_eq!(ui.get_draft(), "");
    Ok(())
}

// Ctrl plus the key under A on the Russian and Thai layouts selects all
// (ported from Yapfast's `ctrl_shortcuts_work_on_cyrillic_and_thai_layouts`).
#[test]
fn ctrl_shortcuts_work_on_cyrillic_and_thai_layouts() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window.clone())))?;
    let ui = AppWindow::new()?;
    ui.set_screen(Screen::Main);
    ui.set_has_room(true);
    ui.set_can_send(true);
    ui.show()?;
    window.set_size(PhysicalSize::new(800, 600));
    let mut pixels = vec![Rgb8Pixel::default(); 800 * 600];
    let mut draw = || {
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, 800);
        });
    };
    draw();
    let position = LogicalPosition::new(450.0, 551.0);
    let button = PointerEventButton::Left;
    window.dispatch_event(WindowEvent::PointerPressed { position, button });
    window.dispatch_event(WindowEvent::PointerReleased { position, button });
    draw();
    let select_all = |letter: &str| {
        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        key(&window, letter.into());
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
    };
    for (typed, a, next) in [("ฟห", "ฟ", "ก"), ("фы", "ф", "в")] {
        for letter in typed.chars() {
            key(&window, letter.to_string().into());
        }
        draw();
        assert_eq!(ui.get_draft(), typed);
        select_all(a);
        draw();
        assert_eq!(ui.get_draft(), typed, "Ctrl and a letter types nothing");
        key(&window, next.into());
        draw();
        assert_eq!(ui.get_draft(), next);
        select_all(a);
        key(&window, Key::Backspace.into());
        draw();
        assert_eq!(ui.get_draft(), "");
    }
    Ok(())
}
