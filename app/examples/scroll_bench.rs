// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Times what scrolling costs on the CPU: each frame's update (the presenter,
//! Slint's bindings and layout) plus its software paint, for a long text
//! channel and a channel of pictures. Off-screen with made-up data: no window
//! opens, nothing touches the network. Used to check that size-oriented build
//! settings don't slow the UI (docs/measurements.md, Binary size).
//!
//! cargo run --release --example scroll_bench -- [rounds]

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use godwit::demo::{msg, pictures, room, world};
use godwit::images::Images;
use godwit::present::Presenter;
use godwit::theme;
use godwit::ui::{AppWindow, Screen};
use godwit_core::account::MediaFetch;
use godwit_core::application::{Action, Application, Event};
use godwit_core::contacts::Contacts;
use godwit_core::message::{Attachment, AttachmentKind, Message};
use godwit_core::room::RoomKind;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, PhysicalSize, Rgb8Pixel};

const W: u32 = 1920;
const H: u32 = 1080;

struct Offscreen(Rc<MinimalSoftwareWindow>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

/// A made-up 800×450 picture for `demo://photo/N`, as PNG bytes.
fn photo(url: &str) -> Option<Vec<u8>> {
    let n: u32 = url.rsplit('/').next()?.parse().ok()?;
    let img = image::RgbImage::from_fn(800, 450, |x, y| {
        image::Rgb([
            ((x + n * 37) % 256) as u8,
            ((y + n * 53) % 256) as u8,
            ((x ^ y ^ n) % 256) as u8,
        ])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

struct Bench {
    window: Rc<MinimalSoftwareWindow>,
    ui: AppWindow,
    presenter: Presenter,
    images: Images,
    rows: Rc<Cell<Option<(usize, usize)>>>,
    buffer: Vec<Rgb8Pixel>,
}

impl Bench {
    /// One frame as the app does it: show the state, then paint what changed.
    fn frame(&mut self, app: &Application) -> Duration {
        let start = Instant::now();
        self.presenter.window = self.rows.get();
        self.presenter.images_changed |= self.images.poll();
        self.presenter.show(&self.ui, app.state(), true, &mut self.images);
        self.ui.set_screen(Screen::Main);
        slint::platform::update_timers_and_animations();
        let buffer = &mut self.buffer;
        self.window.draw_if_needed(|r| {
            r.render(buffer, W as usize);
        });
        start.elapsed()
    }

    /// Lets pictures decode (off the timed path) and the view settle.
    fn settle(&mut self, app: &Application) {
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(10));
            self.window.request_redraw();
            self.frame(app);
        }
    }

    /// Scrolls down and back up `rounds` times; returns each frame's time.
    fn scroll(&mut self, app: &Application, rounds: usize) -> Vec<Duration> {
        let mut times = Vec::new();
        for _ in 0..rounds {
            for dy in [-120.0f32, 120.0] {
                for _ in 0..20 {
                    self.window.dispatch_event(WindowEvent::PointerScrolled {
                        position: LogicalPosition::new(900.0, 500.0),
                        delta_x: 0.0,
                        delta_y: dy,
                    });
                    times.push(self.frame(app));
                }
            }
        }
        times
    }
}

fn report(name: &str, mut times: Vec<Duration>) {
    times.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let mean = times.iter().map(|d| ms(*d)).sum::<f64>() / times.len().max(1) as f64;
    let at = |q: f64| times.get(((times.len() as f64 * q) as usize).min(times.len().saturating_sub(1)));
    println!(
        "{name}: {} frames, mean {mean:.3} ms, median {:.3} ms, p95 {:.3} ms",
        times.len(),
        at(0.5).map(|d| ms(*d)).unwrap_or(0.0),
        at(0.95).map(|d| ms(*d)).unwrap_or(0.0),
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rounds = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(10);
    // As on screen: the last frame stays in the buffer, so only what changed is drawn.
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window.clone())))?;
    let ui = AppWindow::new()?;
    theme::apply(&ui, theme::DEFAULT_ACCENT, 0, true);
    let fetch: MediaFetch = Arc::new(photo);
    let mut images = Images::new(Arc::new(|| {}), vec![("demo://".to_string(), fetch)], None);
    pictures(&mut images);
    let presenter = Presenter::new(&ui);
    let rows: Rc<Cell<Option<(usize, usize)>>> = Rc::default();
    let seen = rows.clone();
    ui.on_visible_rows(move |top, bottom| {
        if let (Ok(t), Ok(b)) = (usize::try_from(top), usize::try_from(bottom)) {
            seen.set(Some((t.min(b), t.max(b))));
        }
    });
    let (mut app, _general, _dm) = world(Contacts::default());
    window.set_size(PhysicalSize::new(W, H));
    let mut bench = Bench {
        window,
        ui,
        presenter,
        images,
        rows,
        buffer: vec![Rgb8Pixel { r: 0, g: 0, b: 0 }; (W * H) as usize],
    };

    // A long text channel: 300 messages of mixed length and markup.
    let text = room("text", "text", RoomKind::Group, false, 0);
    let text_id = text.id.clone();
    app.apply(Event::RoomUpdated(text));
    app.dispatch(Action::SelectRoom(text_id.clone()));
    let lines = [
        "short one",
        "a **bold** claim with `code` and a [link](https://example.org) in it",
        "a longer message that wraps over a few lines on a normal window, so the layout has some real work to do for every row it shows",
        "> a quote\nand a reply under it",
        "```rust\nfn main() { println!(\"hi\"); }\n```",
    ];
    let messages: Vec<Message> = (0..300)
        .map(|i| {
            msg(
                &format!("t{i}"),
                if i % 3 == 0 { "u1" } else { "u2" },
                "Someone",
                i as i64,
                lines[i % lines.len()],
            )
        })
        .collect();
    app.apply(Event::History {
        room: text_id,
        before: None,
        messages,
        reached_start: true,
    });
    bench.settle(&app);
    let warm = bench.scroll(&app, 1);
    drop(warm);
    report("text channel", bench.scroll(&app, rounds));

    // A channel of 60 pictures.
    let pics = room("pics", "pictures", RoomKind::Group, false, 0);
    let pics_id = pics.id.clone();
    app.apply(Event::RoomUpdated(pics));
    app.dispatch(Action::SelectRoom(pics_id.clone()));
    let messages: Vec<Message> = (0..60)
        .map(|i| {
            let mut m = msg(&format!("p{i}"), "u1", "Pictures", i as i64, "");
            m.attachments = vec![Attachment {
                name: format!("{i}.png").into(),
                url: format!("demo://photo/{i}").into(),
                kind: AttachmentKind::Image,
                size: 0,
                dimensions: Some((800, 450)),
                original_url: None,
            }];
            m
        })
        .collect();
    app.apply(Event::History {
        room: pics_id,
        before: None,
        messages,
        reached_start: true,
    });
    bench.settle(&app);
    // Pictures decode on worker threads; let them land between rounds, untimed.
    let mut times = Vec::new();
    for _ in 0..rounds {
        times.extend(bench.scroll(&app, 1));
        bench.settle(&app);
    }
    report("picture channel", times);
    Ok(())
}
