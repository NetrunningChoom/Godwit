// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Right-click menus: off-screen pointer dispatch into the sidebar.

use std::cell::RefCell;
use std::rc::Rc;

use godwit::ui::{AppWindow, MessageItem, RoomItem, Screen};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, Rgb8Pixel, VecModel};

struct Offscreen(Rc<MinimalSoftwareWindow>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn click(window: &MinimalSoftwareWindow, x: f32, y: f32, button: PointerEventButton) {
    let position = LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed { position, button });
    window.dispatch_event(WindowEvent::PointerReleased { position, button });
}

#[test]
fn right_click_opens_a_rooms_menu() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window.clone())))?;
    let ui = AppWindow::new()?;
    ui.set_screen(Screen::Main);
    let row = RoomItem {
        name: "Notes".into(),
        kind: 0,
        ..Default::default()
    };
    ui.set_rooms(ModelRc::new(VecModel::from(vec![row.clone(), row])));
    let picked = Rc::new(RefCell::new(Vec::<(i32, i32)>::new()));
    let seen = picked.clone();
    ui.on_room_menu(move |i, a| seen.borrow_mut().push((i, a)));
    let selected = Rc::new(RefCell::new(Vec::<i32>::new()));
    let seen = selected.clone();
    ui.on_select_room(move |i| seen.borrow_mut().push(i));
    ui.show()?;
    window.set_size(PhysicalSize::new(1000, 700));
    let mut pixels = vec![Rgb8Pixel::default(); 1000 * 700];
    let mut draw = || {
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, 1000);
        });
    };
    draw();
    // Find the first room row with a left click.
    let x = 160.0;
    let mut row_y = None;
    for y in (0..700).step_by(4) {
        click(&window, x, y as f32, PointerEventButton::Left);
        draw();
        if !selected.borrow().is_empty() {
            row_y = Some(y as f32);
            break;
        }
    }
    let row_y = row_y.ok_or("no room row found")?;
    println!("row at y={row_y}, selected {:?}", selected.borrow());
    // Right-click it, then left-click just inside the menu's first row.
    click(&window, x, row_y, PointerEventButton::Right);
    draw();
    for dy in (8..60).step_by(4) {
        click(&window, x + 30.0, row_y + dy as f32, PointerEventButton::Left);
        draw();
        if !picked.borrow().is_empty() {
            break;
        }
    }
    println!("menu picks {:?}", picked.borrow());
    assert_eq!(
        picked.borrow().first().map(|p| p.1),
        Some(0),
        "the menu's first row is pin"
    );

    // Pinned rows dragged: the first one down two places, let go there.
    let pinned = |above: i32, below: i32| RoomItem {
        name: "Pinned".into(),
        pinned: true,
        pins_above: above,
        pins_below: below,
        ..Default::default()
    };
    ui.set_rooms(ModelRc::new(VecModel::from(vec![
        pinned(0, 2),
        pinned(1, 1),
        pinned(2, 0),
    ])));
    let moved = Rc::new(RefCell::new(Vec::<(i32, i32)>::new()));
    let seen = moved.clone();
    ui.on_move_pin(move |i, by| seen.borrow_mut().push((i, by)));
    selected.borrow_mut().clear();
    draw();
    let button = PointerEventButton::Left;
    let start = LogicalPosition::new(x, row_y);
    window.dispatch_event(WindowEvent::PointerMoved { position: start });
    window.dispatch_event(WindowEvent::PointerPressed {
        position: start,
        button,
    });
    let end = LogicalPosition::new(x, row_y + 96.0);
    for step in 1..=8 {
        let position = LogicalPosition::new(x, row_y + 12.0 * step as f32);
        window.dispatch_event(WindowEvent::PointerMoved { position });
        draw();
    }
    window.dispatch_event(WindowEvent::PointerReleased { position: end, button });
    draw();
    assert_eq!(*moved.borrow(), vec![(0, 2)], "dropped two places down");
    assert!(selected.borrow().is_empty(), "a drag isn't also a click");

    // A message's menu: its first row (with nothing else allowed) is "Copy text".
    let copied = Rc::new(RefCell::new(Vec::<i32>::new()));
    let seen = copied.clone();
    ui.on_copy_text(move |i| seen.borrow_mut().push(i));
    ui.set_has_room(true);
    let message = MessageItem {
        author: "Someone".into(),
        time: "12:00".into(),
        first_in_group: true,
        ..Default::default()
    };
    ui.set_messages(ModelRc::new(VecModel::from(vec![message])));
    draw();
    'found: for y in (60..600).rev().step_by(8) {
        click(&window, 600.0, y as f32, PointerEventButton::Right);
        draw();
        for dy in (6..40).step_by(6) {
            click(&window, 630.0, (y + dy) as f32, PointerEventButton::Left);
            draw();
            if !copied.borrow().is_empty() {
                println!("message menu at y={y}");
                break 'found;
            }
        }
    }
    assert_eq!(
        copied.borrow().first(),
        Some(&0),
        "a message's menu opens on right-click"
    );
    Ok(())
}
