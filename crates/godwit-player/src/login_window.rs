// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A network's own sign-in page in a window of its own, shared by every
//! sign-in that goes through a browser (Slack, Google Chat): the network's
//! page handles passwords, passkeys, single sign-on and two-factor, and
//! Godwit never sees them. The browser profile is ephemeral (nothing on
//! disk); once the network's app has loaded, the session goes to Godwit as
//! one JSON line on stdout, a pipe only the app reads, and is never logged.
//! Exit status 1: the window was closed first; 2: no browser on this OS.

/// Safari's agent: the window is WebKit, as Safari is, so a page's checks
/// of what the browser can do match what it says it is. Networks support
/// recent versions only: Slack refused Safari 17.6 (and Chrome 126) as "not
/// supported" (2026-11-04). Keep this current.
pub const SAFARI: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.1 Safari/605.1.15";

/// Reads the session once the app is `ready`: calls the last argument with
/// the line once it's all there (it may be called on a later try).
#[cfg(target_os = "linux")]
pub type Read = fn(&webkit2gtk::WebView, &webkit2gtk::WebContext, Box<dyn FnOnce(String)>);

/// Windows: reads the session once the app is `ready` and hands the line to
/// [`found`] once it's all there (it may be on a later try).
#[cfg(windows)]
pub type Read = fn(&wry::WebView);

/// What differs between networks.
pub struct Spec {
    /// The network's name, for the title and the log.
    pub network: &'static str,
    /// The sign-in page.
    pub start: &'static str,
    /// The browser the page is told it runs in.
    pub agent: &'static str,
    /// Whether `uri` is the network's app, where the session can be read.
    pub ready: fn(&str) -> bool,
    /// A page to go on to after `uri` loaded, if any (e.g. past an "open
    /// the desktop app" page).
    pub redirect: fn(&str) -> Option<&'static str>,
    #[cfg(any(target_os = "linux", windows))]
    pub read: Read,
}

/// A JSON string literal.
pub fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `(host, path)` of an `https://` address; the host lowercased.
pub fn https_parts(uri: &str) -> Option<(String, &str)> {
    let rest = uri.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let path = rest[end..].split(['?', '#']).next().unwrap_or_default();
    Some((rest[..end].to_ascii_lowercase(), path))
}

/// The value of cookie `name` among `found`, or empty.
#[cfg(target_os = "linux")]
pub fn cookie(found: &mut [soup::Cookie], name: &str) -> String {
    found
        .iter_mut()
        .find_map(|c| {
            (c.name().as_deref() == Some(name))
                .then(|| c.value().map(|v| v.to_string()))
                .flatten()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "linux")]
pub fn run(spec: Spec) -> ! {
    let start = spec.start;
    run_at(spec, start)
}

/// Opens a runtime page (the loopback Discord captcha) in the same window.
#[cfg(target_os = "linux")]
pub fn run_at(spec: Spec, start: &str) -> ! {
    use std::cell::Cell;
    use std::io::Write;
    use std::rc::Rc;

    use gtk::prelude::*;
    use javascriptcore::ValueExt;
    use webkit2gtk::{NavigationPolicyDecisionExt, PolicyDecisionExt, SettingsExt, URIRequestExt, WebViewExt};

    gtk::glib::set_prgname(Some("godwit"));
    if gtk::init().is_err() {
        std::process::exit(4);
    }
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_title(&format!("Sign in to {} — Godwit", spec.network));
    window.set_default_size(900, 760);
    let context = webkit2gtk::WebContext::new_ephemeral();
    let web = webkit2gtk::WebView::with_context(&context);
    if let Some(settings) = WebViewExt::settings(&web) {
        settings.set_user_agent(Some(spec.agent));
    }
    // Pop-ups (single sign-on) open here instead: one window, one session.
    web.connect_decide_policy(|view, decision, kind| {
        if kind != webkit2gtk::PolicyDecisionType::NewWindowAction {
            return false;
        }
        let uri = decision
            .clone()
            .downcast::<webkit2gtk::NavigationPolicyDecision>()
            .ok()
            .and_then(|d| d.navigation_action())
            .and_then(|a| a.request())
            .and_then(|r| r.uri());
        if let Some(uri) = uri.filter(|u| u.starts_with("https://")) {
            view.load_uri(&uri);
        }
        decision.ignore();
        true
    });
    let done = Rc::new(Cell::new(false));
    // On the app, read the session every 2 s until it's there (apps write
    // their state a moment after loading).
    {
        let (web, context, done) = (web.clone(), context.clone(), done.clone());
        let (ready, read) = (spec.ready, spec.read);
        gtk::glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
            if done.get() {
                return gtk::glib::ControlFlow::Break;
            }
            if web.uri().is_some_and(|u| ready(&u)) {
                let done = done.clone();
                read(
                    &web,
                    &context,
                    Box::new(move |line| {
                        if done.replace(true) {
                            return;
                        }
                        let mut out = std::io::stdout().lock();
                        let ok = writeln!(out, "{line}").and_then(|()| out.flush()).is_ok();
                        std::process::exit(if ok { 0 } else { 3 });
                    }),
                );
            }
            gtk::glib::ControlFlow::Continue
        });
    }
    // Where the page is and what it sees, for the app's log: the host and
    // first path part only (later parts and queries can hold sign-in
    // tokens), the title, and the browser name the page was given.
    let (network, redirect) = (spec.network, spec.redirect);
    web.connect_load_changed(move |view, event| {
        if event != webkit2gtk::LoadEvent::Finished {
            return;
        }
        let uri = view.uri().map(|u| u.to_string()).unwrap_or_default();
        if let Some(next) = redirect(&uri) {
            view.load_uri(next);
        }
        let place = uri
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
            .splitn(5, '/')
            .take(4)
            .collect::<Vec<_>>()
            .join("/");
        let title = view.title().map(|t| t.to_string()).unwrap_or_default();
        view.evaluate_javascript(
            "navigator.userAgent + ' | vendor=' + navigator.vendor",
            None,
            None,
            gtk::gio::Cancellable::NONE,
            move |r| {
                let seen = r.map(|v| v.to_str().to_string()).unwrap_or_default();
                eprintln!("note: {network} sign-in page: {place} — \"{title}\" — sees: {seen}");
            },
        );
    });
    window.add(&web);
    window.connect_destroy(|_| std::process::exit(1));
    web.load_uri(start);
    window.show_all();
    gtk::main();
    std::process::exit(1)
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn run(_spec: Spec) -> ! {
    std::process::exit(2)
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn run_at(_spec: Spec, _start: &str) -> ! {
    std::process::exit(2)
}

#[cfg(windows)]
pub fn run(spec: Spec) -> ! {
    let start = spec.start;
    run_at(spec, start)
}

/// Windows: the session line to the app, then exit (0; 3 if the pipe failed).
#[cfg(windows)]
pub fn found(line: String) -> ! {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let ok = writeln!(out, "{line}").and_then(|()| out.flush()).is_ok();
    std::process::exit(if ok { 0 } else { 3 })
}

/// Windows: the same window on WebView2 (wry) in a winit window. InPrivate,
/// so nothing stays on disk; WebView2 is Edge, so the page gets Edge's own
/// browser name rather than [`SAFARI`]. Exit status 4: no window or no
/// WebView2 runtime.
#[cfg(windows)]
pub fn run_at(spec: Spec, start: &str) -> ! {
    use std::time::{Duration, Instant};

    use winit::application::ApplicationHandler;
    use winit::event::{StartCause, WindowEvent};
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
    use winit::window::{Window, WindowId};

    /// A page to open in the one view (a pop-up, or past an app prompt).
    struct Load(String);

    struct App {
        spec: Spec,
        start: String,
        proxy: EventLoopProxy<Load>,
        window: Option<Window>,
        web: Option<wry::WebView>,
    }

    /// Apps write their state a moment after loading: read every 2 s.
    fn next_try(event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_secs(2)));
    }

    impl ApplicationHandler<Load> for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let attributes = Window::default_attributes()
                .with_title(format!("Sign in to {} — Godwit", self.spec.network))
                .with_inner_size(winit::dpi::LogicalSize::new(900.0, 760.0));
            let Ok(window) = event_loop.create_window(attributes) else {
                std::process::exit(4)
            };
            let popup = self.proxy.clone();
            let loaded = self.proxy.clone();
            let (network, redirect) = (self.spec.network, self.spec.redirect);
            let built = wry::WebViewBuilder::new()
                .with_incognito(true)
                .with_url(&self.start)
                // Pop-ups (single sign-on) open here instead: one window, one session.
                .with_new_window_req_handler(move |uri, _| {
                    if uri.starts_with("https://") {
                        let _ = popup.send_event(Load(uri));
                    }
                    wry::NewWindowResponse::Deny
                })
                // Where the page is, for the app's log: the host and first
                // path part only (later parts and queries can hold tokens).
                .with_on_page_load_handler(move |event, uri| {
                    if !matches!(event, wry::PageLoadEvent::Finished) {
                        return;
                    }
                    if let Some(next) = redirect(&uri) {
                        let _ = loaded.send_event(Load(next.to_owned()));
                    }
                    let place = uri
                        .split(['?', '#'])
                        .next()
                        .unwrap_or_default()
                        .splitn(5, '/')
                        .take(4)
                        .collect::<Vec<_>>()
                        .join("/");
                    eprintln!("note: {network} sign-in page: {place}");
                })
                .build(&window);
            let Ok(web) = built else {
                eprintln!("error: no WebView2 runtime for the {network} sign-in");
                std::process::exit(4)
            };
            self.web = Some(web);
            self.window = Some(window);
            next_try(event_loop);
        }

        fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
            if !matches!(cause, StartCause::ResumeTimeReached { .. }) {
                return;
            }
            if let Some(web) = &self.web
                && web.url().is_ok_and(|uri| (self.spec.ready)(&uri))
            {
                (self.spec.read)(web);
            }
            next_try(event_loop);
        }

        fn user_event(&mut self, _: &ActiveEventLoop, Load(uri): Load) {
            if let Some(web) = &self.web {
                let _ = web.load_url(&uri);
            }
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
            match event {
                WindowEvent::CloseRequested => std::process::exit(1),
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

    let Ok(event_loop) = EventLoop::<Load>::with_user_event().build() else {
        std::process::exit(4)
    };
    let mut app = App {
        spec,
        start: start.to_owned(),
        proxy: event_loop.create_proxy(),
        window: None,
        web: None,
    };
    let _ = event_loop.run_app(&mut app);
    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_parts_split_host_and_path() {
        assert_eq!(
            https_parts("https://App.Slack.com/client?x=1"),
            Some(("app.slack.com".into(), "/client"))
        );
        assert_eq!(
            https_parts("https://chat.google.com"),
            Some(("chat.google.com".into(), ""))
        );
        assert_eq!(https_parts("http://chat.google.com/"), None);
    }
}
