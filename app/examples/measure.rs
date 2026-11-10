// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Measures Godwit's memory the way Yapfast does (docs/measurements.md):
//! private anonymous memory, which here includes the window's frame. It
//! draws the demo world off-screen at 1920×1080: no window opens, nothing
//! touches the network or any account.
//!
//! Two scenarios, as Yapfast's: idle (a channel, a DM, the channel again),
//! and an image-heavy channel (100 different pictures, scrolled up through
//! and back down, decoded by the real loader from made-up PNGs).
//!
//! cargo run --release --example measure

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use godwit::demo::{account, msg, pictures, room, world};
use godwit::images::Images;
use godwit::present::Presenter;
use godwit::theme;
use godwit::ui::{AppWindow, Screen};
use godwit_core::account::MediaFetch;
use godwit_core::application::{Action, Application, Event};
use godwit_core::contacts::Contacts;
use godwit_core::message::{Attachment, AttachmentKind};
use godwit_core::room::RoomKind;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, PhysicalSize, Rgb8Pixel};

const W: u32 = 1920;
const H: u32 = 1080;
const PICTURES: usize = 100;

struct Offscreen(Rc<MinimalSoftwareWindow>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

/// Private anonymous memory, in MiB.
fn anon() -> f64 {
    let text = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap_or_default();
    text.lines()
        .find(|l| l.starts_with("Pss_Anon:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|kb| kb.parse::<f64>().ok())
        .unwrap_or(0.0)
        / 1024.0
}

/// A made-up 1600×900 photo for `demo://photo/N`, as PNG bytes (each different).
fn photo(url: &str) -> Result<Vec<u8>, godwit_core::media::MediaError> {
    let n: u32 = url
        .rsplit('/')
        .next()
        .and_then(|part| part.parse().ok())
        .ok_or(godwit_core::media::MediaError::BadAddress)?;
    let img = image::RgbImage::from_fn(1600, 900, |x, y| {
        image::Rgb([
            ((x + n * 37) % 256) as u8,
            ((y + n * 53) % 256) as u8,
            ((x ^ y ^ n) % 256) as u8,
        ])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)
        .map_err(|_| godwit_core::media::MediaError::BodyRead)?;
    Ok(out.into_inner())
}

struct Run {
    window: Rc<MinimalSoftwareWindow>,
    ui: AppWindow,
    presenter: Presenter,
    images: Images,
    rows: Rc<Cell<Option<(usize, usize)>>>,
    buffer: Vec<Rgb8Pixel>,
    /// Every row gets pictures (as before the list was virtualized).
    all: bool,
}

impl Run {
    /// Shows `app`, lets pictures load and the view settle.
    fn draw(&mut self, app: &Application) {
        for _ in 0..40 {
            if !self.all {
                self.presenter.window = self.rows.get();
            } else {
                self.presenter.window = Some((0, usize::MAX / 2));
            }
            self.presenter.images_changed |= self.images.poll();
            self.presenter.show(&self.ui, app.state(), true, &mut self.images);
            self.ui.set_screen(Screen::Main);
            std::thread::sleep(std::time::Duration::from_millis(15));
            slint::platform::update_timers_and_animations();
            self.window.request_redraw();
            let buffer = &mut self.buffer;
            self.window.draw_if_needed(|r| {
                r.render(buffer, W as usize);
            });
        }
    }

    fn scroll(&mut self, app: &Application, dy: f32) {
        self.window.dispatch_event(WindowEvent::PointerScrolled {
            position: LogicalPosition::new(900.0, 500.0),
            delta_x: 0.0,
            delta_y: dy,
        });
        self.draw(app);
    }
}

fn measure(all: bool) -> Result<(f64, f64), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
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
    let (mut app, general, dm) = world(Contacts::default());
    window.set_size(PhysicalSize::new(W, H));
    let mut run = Run {
        window,
        ui,
        presenter,
        images,
        rows,
        buffer: vec![Rgb8Pixel { r: 0, g: 0, b: 0 }; (W * H) as usize],
        all,
    };

    // Idle: a busy channel, a direct chat, the channel again.
    run.draw(&app);
    app.dispatch(Action::SelectServer(None));
    app.dispatch(Action::SelectRoom(dm));
    run.draw(&app);
    app.dispatch(Action::SelectRoom(general));
    run.draw(&app);
    godwit::memory::settle_now();
    let idle = anon();

    // Image-heavy: a room of 100 pictures, up through all of them and back.
    let pics = room("pics", "pictures", RoomKind::Group, false, 0);
    let id = pics.id.clone();
    app.apply(Event::RoomUpdated(pics));
    app.dispatch(Action::SelectRoom(id.clone()));
    let messages = (0..PICTURES)
        .map(|i| {
            let mut m = msg(&format!("p{i}"), "u1", "Pictures", i as i64, "");
            m.attachments = vec![Attachment {
                name: format!("{i}.png").into(),
                url: format!("demo://photo/{i}").into(),
                kind: AttachmentKind::Image,
                size: 0,
                dimensions: Some((1600, 900)),
                original_url: None,
                spoiler: false,
                voice: None,
            }];
            m
        })
        .collect();
    let _ = account();
    app.apply(Event::History {
        room: id,
        before: None,
        messages,
        reached_start: true,
    });
    run.draw(&app);
    for _ in 0..60 {
        run.scroll(&app, 800.0);
    }
    for _ in 0..60 {
        run.scroll(&app, -800.0);
    }
    godwit::memory::settle_now();
    Ok((idle, anon()))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    godwit::memory::limit_pools();
    // `--all`: every row gets pictures, for comparison.
    let all = std::env::args().any(|a| a == "--all");
    let (idle, heavy) = measure(all)?;
    println!(
        "{}idle {idle:.1} MiB; image-heavy channel {heavy:.1} MiB (private anonymous)",
        if all { "[every row] " } else { "" }
    );
    Ok(())
}
