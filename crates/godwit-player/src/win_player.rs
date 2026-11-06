// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The players on Windows: WebView2 (Edge's engine, part of Windows 11)
//! through wry, as the sign-in windows are.
//!
//! Inline (`run_helper`): WebView2 has no off-screen mode, so the page lives
//! in a borderless window far off the screen, left out of the taskbar and
//! never activated. Chromium would stop drawing a window it thinks nobody
//! sees, so its occlusion tracking and background throttling are switched
//! off for this browser process only. Frames come from `CapturePreview`
//! (JPEG, decoded here) and go to the app in the Linux helper's wire format;
//! clicks go in through the DevTools protocol's `Input.dispatchMouseEvent`.
//!
//! Pop-out (`run_window`): the same page in a real fullscreen window.
//!
//! The page is served as `https://stoat.chat/…` (intercepted, never fetched),
//! as WebKit's base URI is on Linux: embeds such as Twitch check `parent`
//! against the page's origin, and YouTube wants a referrer.

use std::io::{self, BufRead, Write};
use std::time::{Duration, Instant};

use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_JPEG, COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN,
    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL, ICoreWebView2, ICoreWebView2_2,
};
use webview2_com::{
    AcceleratorKeyPressedEventHandler, CallDevToolsProtocolMethodCompletedHandler, CapturePreviewCompletedHandler,
    ContainsFullScreenElementChangedEventHandler, WebResourceRequestedEventHandler,
};
use windows::Win32::System::Com::{IStream, STREAM_SEEK_SET};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::core::{HSTRING, Interface};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Fullscreen, Window, WindowId};
use wry::{WebViewBuilderExtWindows, WebViewExtWindows};

use crate::{Kind, autoplay, flat, gif_page, iframe_page, valid_player_url, video_page};

/// Where the page is served from (see the module comment).
const PAGE_URL: &str = "https://stoat.chat/__godwit_player";

/// About 30 frames a second at most, as the Linux helper's 33 ms timer.
const FRAME_EVERY: Duration = Duration::from_millis(33);

/// While the picture stays the same (a paused video, a still page), four
/// captures a second: each is a JPEG encoded by WebView2, so idle players
/// cost little on weak machines. Any change or input goes back to full speed.
const STILL_EVERY: Duration = Duration::from_millis(250);

/// Unchanged captures in a row before slowing to [`STILL_EVERY`] (half a
/// second at full speed).
const STILL_AFTER: u32 = 15;

/// wry's own defaults (it replaces them when given any), plus what keeps an
/// unseen player drawing and lets a muted GIF or a clicked video play.
const INLINE_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion \
     --disable-backgrounding-occluded-windows --disable-renderer-backgrounding \
     --disable-background-timer-throttling --autoplay-policy=no-user-gesture-required";
const WINDOW_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
     --autoplay-policy=no-user-gesture-required";

enum Event {
    /// A line from the app (`q`, `s w h`, `d/u/m x y`).
    Command(String),
    /// A captured JPEG (None: the capture failed).
    Captured(Option<Vec<u8>>),
    /// The page asked for fullscreen.
    Fullscreen,
    /// A link the page opened in a new window.
    Link(String),
    /// Escape or F11 in the pop-out window.
    Key(u32),
}

const VK_ESCAPE: u32 = 0x1B;
const VK_F11: u32 = 0x7A;

/// Serves `page` at `PAGE_URL` to this view, then goes there.
fn serve_page(web: &wry::WebView, page: String) -> windows::core::Result<()> {
    let core: ICoreWebView2 = web.webview();
    // SAFETY: COM calls on the UI thread, on interfaces wry keeps alive.
    unsafe {
        let env = core.cast::<ICoreWebView2_2>()?.Environment()?;
        core.AddWebResourceRequestedFilter(
            &HSTRING::from(format!("{PAGE_URL}*")),
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
        )?;
        let body = page.into_bytes();
        let mut token = 0i64;
        core.add_WebResourceRequested(
            &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let stream = SHCreateMemStream(Some(&body));
                let response = env.CreateWebResourceResponse(
                    stream.as_ref(),
                    200,
                    &HSTRING::from("OK"),
                    &HSTRING::from("Content-Type: text/html; charset=utf-8"),
                )?;
                args.SetResponse(&response)
            })),
            &mut token,
        )?;
    }
    web.load_url(PAGE_URL).map_err(|_| windows::core::Error::from_win32())
}

/// Tells the app when the page wants fullscreen.
fn watch_fullscreen(web: &wry::WebView, proxy: EventLoopProxy<Event>) -> windows::core::Result<()> {
    let core: ICoreWebView2 = web.webview();
    let mut token = 0i64;
    // SAFETY: as in `serve_page`.
    unsafe {
        core.add_ContainsFullScreenElementChanged(
            &ContainsFullScreenElementChangedEventHandler::create(Box::new(move |sender, _| {
                let mut full = windows::core::BOOL::default();
                if let Some(sender) = sender {
                    sender.ContainsFullScreenElement(&mut full)?;
                }
                if full.as_bool() {
                    let _ = proxy.send_event(Event::Fullscreen);
                }
                Ok(())
            })),
            &mut token,
        )
    }
}

/// Reads a whole memory stream.
fn read_stream(stream: &IStream) -> Option<Vec<u8>> {
    // SAFETY: a stream this process made; the buffer outlives each Read.
    unsafe {
        stream.Seek(0, STREAM_SEEK_SET, None).ok()?;
        let mut out = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            let hr = stream.Read(chunk.as_mut_ptr().cast(), chunk.len() as u32, Some(&mut read));
            if hr.is_err() {
                return None;
            }
            if read == 0 {
                break;
            }
            out.extend_from_slice(chunk.get(..read as usize)?);
        }
        Some(out)
    }
}

/// Starts one capture; its JPEG arrives as `Event::Captured`.
fn capture(web: &wry::WebView, proxy: EventLoopProxy<Event>) -> bool {
    let core: ICoreWebView2 = web.webview();
    // SAFETY: as in `serve_page`; the stream moves into the handler.
    unsafe {
        let Some(stream) = SHCreateMemStream(None) else {
            return false;
        };
        let target = stream.clone();
        core.CapturePreview(
            COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_JPEG,
            &stream,
            &CapturePreviewCompletedHandler::create(Box::new(move |result| {
                let jpeg = result.ok().and_then(|()| read_stream(&target));
                let _ = proxy.send_event(Event::Captured(jpeg));
                Ok(())
            })),
        )
        .is_ok()
    }
}

/// A left-button event at frame pixel (x, y), through DevTools.
fn pointer(web: &wry::WebView, x: f64, y: f64, scale: f64, op: &str, pressed: bool) {
    let kind = match op {
        "d" => "mousePressed",
        "u" => "mouseReleased",
        _ => "mouseMoved",
    };
    let moving = op == "m";
    let params = serde_json::json!({
        "type": kind,
        // DevTools wants CSS pixels; the frame is in device pixels.
        "x": x / scale,
        "y": y / scale,
        "button": if moving && !pressed { "none" } else { "left" },
        "buttons": if pressed { 1 } else { 0 },
        "clickCount": if moving { 0 } else { 1 },
    });
    let core: ICoreWebView2 = web.webview();
    // SAFETY: as in `serve_page`.
    unsafe {
        let _ = core.CallDevToolsProtocolMethod(
            &HSTRING::from("Input.dispatchMouseEvent"),
            &HSTRING::from(params.to_string()),
            &CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(()))),
        );
    }
}

/// JPEG → BGRA (the wire format: Cairo's ARGB32 in memory).
fn decode(jpeg: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let image = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).ok()?;
    let mut rgba = image.into_rgba8();
    let (w, h) = rgba.dimensions();
    for px in rgba.as_mut().as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Some((w, h, rgba.into_raw()))
}

fn page_for(kind: Kind, url: &str, window: bool) -> String {
    match (kind, window) {
        (Kind::Embed, false) => iframe_page(url),
        (Kind::Video, false) => video_page(url, false),
        (Kind::Gif, false) => gif_page(url),
        (Kind::Embed, true) => iframe_page(&autoplay(url)),
        (Kind::Video | Kind::Gif, true) => video_page(url, true),
    }
}

fn parse(args: &[String]) -> (Kind, String) {
    let kind = args.first().map(String::as_str).and_then(Kind::parse);
    let url = args.get(1).cloned().unwrap_or_default();
    match kind.filter(|_| valid_player_url(&url)) {
        Some(kind) => (kind, url),
        None => std::process::exit(2),
    }
}

/// `godwit-player <kind> <url> <width> <height> <scale>` on Windows.
pub fn run_helper(args: &[String]) -> ! {
    let (kind, url) = parse(args);
    let num = |i: usize, default: u32, max: u32| {
        args.get(i)
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(default)
            .clamp(90, max)
    };
    let (width, height) = (num(2, 640, 1600), num(3, 360, 1000));

    struct Inline {
        kind: Kind,
        url: String,
        size: (u32, u32),
        proxy: EventLoopProxy<Event>,
        window: Option<Window>,
        web: Option<wry::WebView>,
        out: io::BufWriter<io::Stdout>,
        capturing: bool,
        drawn: bool,
        pressed: bool,
        /// The last sent capture's JPEG, hashed (None: send the next one).
        last_hash: Option<u64>,
        /// Unchanged captures since the last change or input.
        still: u32,
    }

    impl Inline {
        fn send(&mut self, tag: u8, a: u32, b: u32, body: &[u8]) {
            let ok = crate::write_header(&mut self.out, tag, a, b).is_ok()
                && self.out.write_all(body).is_ok()
                && self.out.flush().is_ok();
            if !ok {
                // The app is gone.
                std::process::exit(0);
            }
        }

        fn scale(&self) -> f64 {
            self.window.as_ref().map_or(1.0, Window::scale_factor)
        }
    }

    impl ApplicationHandler<Event> for Inline {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attributes = Window::default_attributes()
                .with_title("Godwit player")
                .with_decorations(false)
                .with_skip_taskbar(true)
                .with_active(false)
                .with_position(winit::dpi::PhysicalPosition::new(-20000, -20000))
                .with_inner_size(winit::dpi::PhysicalSize::new(self.size.0, self.size.1));
            let Ok(window) = event_loop.create_window(attributes) else {
                std::process::exit(4)
            };
            let links = self.proxy.clone();
            let built = wry::WebViewBuilder::new()
                .with_incognito(true)
                .with_additional_browser_args(INLINE_ARGS)
                .with_autoplay(true)
                .with_url("about:blank")
                // Links the page opens in a new window go to the browser.
                .with_new_window_req_handler(move |uri, _| {
                    let _ = links.send_event(Event::Link(uri));
                    wry::NewWindowResponse::Deny
                })
                .build(&window);
            let Ok(web) = built else {
                eprintln!("error: no WebView2 runtime for the player");
                std::process::exit(4)
            };
            if serve_page(&web, page_for(self.kind, &self.url, false)).is_err() {
                std::process::exit(4);
            }
            let _ = watch_fullscreen(&web, self.proxy.clone());
            self.web = Some(web);
            self.window = Some(window);
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + FRAME_EVERY));
        }

        fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
            if !matches!(cause, StartCause::ResumeTimeReached { .. }) {
                return;
            }
            if !self.capturing
                && let Some(web) = &self.web
            {
                self.capturing = capture(web, self.proxy.clone());
            }
            let every = if self.still >= STILL_AFTER {
                STILL_EVERY
            } else {
                FRAME_EVERY
            };
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + every));
        }

        fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Event) {
            match event {
                Event::Captured(jpeg) => {
                    self.capturing = false;
                    let Some(jpeg) = jpeg else {
                        return;
                    };
                    // Unchanged frames are not resent: comparing the JPEG
                    // (tens of KB) skips decoding and hashing the pixels
                    // (megabytes), the most of a still player's cost.
                    use std::hash::{Hash, Hasher};
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    jpeg.hash(&mut hasher);
                    let hash = hasher.finish();
                    if self.last_hash == Some(hash) {
                        self.still = self.still.saturating_add(1);
                        return;
                    }
                    self.still = 0;
                    let Some((w, h, pixels)) = decode(&jpeg) else {
                        return;
                    };
                    // Blank frames before the page draws would hide the
                    // app's thumbnail.
                    self.drawn = self.drawn || !flat(&pixels);
                    if !self.drawn {
                        return;
                    }
                    self.last_hash = Some(hash);
                    self.send(b'F', w, h, &pixels);
                }
                Event::Fullscreen => self.send(b'X', 0, 0, &[]),
                Event::Link(uri) => self.send(b'U', uri.len() as u32, 0, uri.as_bytes()),
                Event::Key(_) => {}
                Event::Command(line) => {
                    // Input or a resize: the picture is about to change.
                    self.still = 0;
                    let mut fields = line.split_whitespace();
                    let op = fields.next().unwrap_or_default();
                    let mut next = || fields.next().and_then(|v| v.parse::<f64>().ok());
                    match op {
                        "q" => event_loop.exit(),
                        "s" => {
                            if let (Some(w), Some(h)) = (next(), next()) {
                                let (w, h) = (w as u32, h as u32);
                                if (2..=1600).contains(&w) && (2..=1000).contains(&h) {
                                    self.size = (w, h);
                                    if let Some(window) = &self.window {
                                        let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(w, h));
                                    }
                                    self.last_hash = None;
                                }
                            }
                        }
                        "d" | "u" | "m" => {
                            if let (Some(x), Some(y)) = (next(), next()) {
                                if op != "m" {
                                    self.pressed = op == "d";
                                }
                                let scale = self.scale();
                                if let Some(web) = &self.web {
                                    pointer(web, x, y, scale, op, self.pressed);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
            match event {
                WindowEvent::CloseRequested => event_loop.exit(),
                WindowEvent::Resized(size) => {
                    if let Some(web) = &self.web {
                        let _ = web.set_bounds(wry::Rect {
                            position: wry::dpi::PhysicalPosition::new(0, 0).into(),
                            size: wry::dpi::PhysicalSize::new(size.width, size.height).into(),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    let Ok(event_loop) = EventLoop::<Event>::with_user_event().build() else {
        std::process::exit(4)
    };
    let proxy = event_loop.create_proxy();
    let commands = proxy.clone();
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            if commands.send_event(Event::Command(line)).is_err() {
                return;
            }
        }
        let _ = commands.send_event(Event::Command("q".into()));
    });
    let mut app = Inline {
        kind,
        url,
        size: (width, height),
        proxy,
        window: None,
        web: None,
        out: io::BufWriter::new(io::stdout()),
        capturing: false,
        drawn: false,
        pressed: false,
        last_hash: None,
        still: 0,
    };
    let _ = event_loop.run_app(&mut app);
    std::process::exit(0)
}

/// `godwit-player --window <kind> <url>`: fullscreen; Escape or the
/// player's own fullscreen button closes it, F11 toggles fullscreen.
pub fn run_window(args: &[String]) -> ! {
    let (kind, url) = parse(args);

    struct Popped {
        kind: Kind,
        url: String,
        proxy: EventLoopProxy<Event>,
        window: Option<Window>,
        web: Option<wry::WebView>,
    }

    impl ApplicationHandler<Event> for Popped {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attributes = Window::default_attributes()
                .with_title("Godwit player")
                .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0))
                .with_fullscreen(Some(Fullscreen::Borderless(None)));
            let Ok(window) = event_loop.create_window(attributes) else {
                std::process::exit(4)
            };
            let built = wry::WebViewBuilder::new()
                .with_incognito(true)
                .with_additional_browser_args(WINDOW_ARGS)
                .with_autoplay(true)
                .with_url("about:blank")
                .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
                .build(&window);
            let Ok(web) = built else {
                eprintln!("error: no WebView2 runtime for the player");
                std::process::exit(4)
            };
            if serve_page(&web, page_for(self.kind, &self.url, true)).is_err() {
                std::process::exit(4);
            }
            // The window is there to watch fullscreen: the page's own
            // fullscreen button closes it (as on Linux).
            let _ = watch_fullscreen(&web, self.proxy.clone());
            // Keys reach the web view, not the window: watch them there.
            let keys = self.proxy.clone();
            let mut token = 0i64;
            // SAFETY: a COM call on the UI thread, on wry's live controller.
            unsafe {
                let _ = web.controller().add_AcceleratorKeyPressed(
                    &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                        let Some(args) = args else { return Ok(()) };
                        let mut kind = Default::default();
                        let mut key = 0u32;
                        args.KeyEventKind(&mut kind)?;
                        args.VirtualKey(&mut key)?;
                        if kind == COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && (key == VK_ESCAPE || key == VK_F11) {
                            args.SetHandled(true)?;
                            let _ = keys.send_event(Event::Key(key));
                        }
                        Ok(())
                    })),
                    &mut token,
                );
            }
            // In front of the chat it was opened from, keys going to the page.
            window.focus_window();
            let _ = web.focus();
            self.web = Some(web);
            self.window = Some(window);
        }

        fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Event) {
            match event {
                Event::Fullscreen | Event::Key(VK_ESCAPE) => event_loop.exit(),
                Event::Key(VK_F11) => {
                    if let Some(window) = &self.window {
                        let full = window.fullscreen().is_some();
                        window.set_fullscreen((!full).then_some(Fullscreen::Borderless(None)));
                    }
                }
                _ => {}
            }
        }

        fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
            match event {
                WindowEvent::CloseRequested => event_loop.exit(),
                WindowEvent::Resized(size) => {
                    if let Some(web) = &self.web {
                        let _ = web.set_bounds(wry::Rect {
                            position: wry::dpi::PhysicalPosition::new(0, 0).into(),
                            size: wry::dpi::PhysicalSize::new(size.width, size.height).into(),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    let Ok(event_loop) = EventLoop::<Event>::with_user_event().build() else {
        std::process::exit(4)
    };
    let mut app = Popped {
        kind,
        url,
        proxy: event_loop.create_proxy(),
        window: None,
        web: None,
    };
    let _ = event_loop.run_app(&mut app);
    std::process::exit(0)
}
