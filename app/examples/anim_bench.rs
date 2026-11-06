// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Measures what playing an animated picture costs the software renderer:
//! how much of the window each frame repaints, and how long that takes.
//! Off-screen with made-up data: no window opens, nothing touches the network.
//!
//! cargo run --release --example anim_bench -- [width height seconds]

use std::rc::Rc;
use std::time::{Duration, Instant};

use godwit::demo::{pictures, world};
use godwit::images::{Images, PREVIEW};
use godwit::present::Presenter;
use godwit::theme;
use godwit::ui::{Anim, AppWindow, Screen};
use godwit_core::contacts::Contacts;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, Rgb8Pixel};

struct Offscreen(Rc<MinimalSoftwareWindow>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arg = |i: usize, default: u32| std::env::args().nth(i).and_then(|a| a.parse().ok()).unwrap_or(default);
    let (w, h, seconds) = (arg(1, 1920), arg(2, 1080), arg(3, 3));
    // As on screen: the last frame stays in the buffer, so only what changed
    // needs drawing (softbuffer reports a buffer age of 1).
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window.clone())))?;

    let ui = AppWindow::new()?;
    theme::apply(&ui, theme::DEFAULT_ACCENT, 0, true);
    let mut images = Images::offline();
    pictures(&mut images);
    // The demo's photo, as a 30 fps animation (400 × 225, 30 frames).
    let (fw, fh) = (400u32, 225u32);
    let frames: Vec<(Vec<u8>, u32)> = (0..30u32)
        .map(|f| {
            let rgba = (0..fh)
                .flat_map(|y| {
                    (0..fw).flat_map(move |x| {
                        let v = ((x + y + f * 8) % 256) as u8;
                        [v, 255 - v, 128, 255]
                    })
                })
                .collect();
            (rgba, 33)
        })
        .collect();
    images.put_animated("preview://photo", PREVIEW, fw, fh, &frames);
    let animator = godwit::anim::Animator::new();
    images.set_animator(animator.clone());
    ui.global::<Anim>()
        .set_frames(slint::ModelRc::from(animator.borrow().model()));

    let (app, _, _) = world(Contacts::default());
    let mut presenter = Presenter::new(&ui);
    ui.set_screen(Screen::Main);
    ui.show()?;
    window.set_size(PhysicalSize::new(w, h));
    let mut buffer = vec![Rgb8Pixel { r: 0, g: 0, b: 0 }; (w * h) as usize];
    let draw = |buffer: &mut [Rgb8Pixel]| {
        let mut area = None;
        let start = Instant::now();
        window.draw_if_needed(|renderer| {
            let region = renderer.render(buffer, w as usize);
            area = Some(
                region
                    .iter()
                    .map(|(_, size)| u64::from(size.width) * u64::from(size.height))
                    .sum::<u64>(),
            );
        });
        area.map(|a| (a, start.elapsed()))
    };
    // Settle: layout, then the rows' pictures.
    for _ in 0..4 {
        presenter.show(&ui, app.state(), true, &mut images);
        // The shell learns the visible rows from the timeline; the demo
        // channel fits on screen, so every row counts as near the view.
        presenter.window = Some((0, presenter.rows.len()));
        let live = godwit::view::live_slots(&presenter.rows, presenter.window);
        animator.borrow_mut().set_live(live);
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        let _ = draw(&mut buffer);
    }
    let live = godwit::view::live_slots(&presenter.rows, presenter.window);
    println!("window {w}×{h}, live animations: {}", live.len());
    if live.is_empty() {
        return Err("no animation near the view".into());
    }

    let end = Instant::now() + Duration::from_secs(u64::from(seconds));
    let (mut frames, mut total_area, mut total_time, mut worst) = (0u32, 0u64, Duration::ZERO, Duration::ZERO);
    while Instant::now() < end {
        std::thread::sleep(slint::platform::duration_until_next_timer_update().unwrap_or(Duration::from_millis(5)));
        slint::platform::update_timers_and_animations();
        if let Some((area, took)) = draw(&mut buffer) {
            frames += 1;
            total_area += area;
            total_time += took;
            worst = worst.max(took);
        }
    }
    // What every inline player frame used to cost: a timeline refresh with
    // moving pictures marked changed, then the redraw.
    let (mut refresh, mut refresh_draw, mut refresh_area) = (Duration::ZERO, Duration::ZERO, 0u64);
    const REFRESHES: u32 = 30;
    for _ in 0..REFRESHES {
        let start = Instant::now();
        presenter.frames_changed = true;
        presenter.show(&ui, app.state(), true, &mut images);
        refresh += start.elapsed();
        if let Some((area, took)) = draw(&mut buffer) {
            refresh_draw += took;
            refresh_area += area;
        }
    }
    println!(
        "frame-driven refresh: {:.2} ms sync + {:.2} ms redraw, {:.1}% of the window repainted",
        refresh.as_secs_f64() * 1000.0 / f64::from(REFRESHES),
        refresh_draw.as_secs_f64() * 1000.0 / f64::from(REFRESHES),
        refresh_area as f64 / f64::from(REFRESHES) / f64::from(w * h) * 100.0,
    );
    let frames_f = f64::from(frames.max(1));
    println!(
        "{frames} redraws in {seconds} s ({:.1}/s); per redraw: {:.0} px repainted ({:.1}% of the window), {:.2} ms average, {:.2} ms worst",
        f64::from(frames) / f64::from(seconds),
        total_area as f64 / frames_f,
        total_area as f64 / frames_f / f64::from(w * h) * 100.0,
        total_time.as_secs_f64() * 1000.0 / frames_f,
        worst.as_secs_f64() * 1000.0,
    );
    Ok(())
}
