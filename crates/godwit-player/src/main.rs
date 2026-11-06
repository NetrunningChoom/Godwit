// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Godwit's inline player helper (see the app's `inline_player.rs` for the
//! wire format). WebKit lives only here, in its own binary, so the app
//! itself doesn't load WebKit, GTK and GStreamer until a player starts.
//!
//! `godwit-player <kind> <url> <width> <height> <scale>`: a player drawn
//! into the app's message; `godwit-player --window <kind> <url>`: the
//! fullscreen pop-out window; `godwit-player --slack-login` and
//! `--gchat-login`: that network's own sign-in page, its session handed back
//! on stdout (see `login_window`). `--discord-captcha <loopback-url>` shows
//! Discord's challenge and returns `{"token":"…"}` on stdout.
//!
//! Linux draws with WebKitGTK, Windows with WebView2 (`win_player`); elsewhere
//! every entry point exits with status 2, and the page builders go unused.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
// No console window beside the sign-in/player windows; the app's pipes still
// reach stdin/stdout (they're inherited handles, not a console).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod discord_captcha;
mod gchat_login;
mod login_window;
mod slack_login;
#[cfg(windows)]
mod win_player;

#[cfg(target_os = "linux")]
use std::io::{self, BufRead, Write};
#[cfg(target_os = "linux")]
use std::sync::mpsc;

/// What the page's player is (the app's `inline_player::Kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Embed,
    Video,
    Gif,
}

impl Kind {
    fn parse(kind: &str) -> Option<Self> {
        match kind {
            "embed" => Some(Self::Embed),
            "video" => Some(Self::Video),
            "gif" => Some(Self::Gif),
            _ => None,
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--window") {
        run_window(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("--slack-login") {
        slack_login::run();
    }
    if args.first().map(String::as_str) == Some("--gchat-login") {
        gchat_login::run();
    }
    if args.first().map(String::as_str) == Some("--discord-captcha") {
        discord_captcha::run(&args[1..]);
    }
    run_helper(&args);
}

/// Writes a helper → app message without a body.
#[cfg(any(target_os = "linux", windows))]
fn write_header(out: &mut impl std::io::Write, tag: u8, a: u32, b: u32) -> std::io::Result<()> {
    out.write_all(&[tag])?;
    out.write_all(&a.to_le_bytes())?;
    out.write_all(&b.to_le_bytes())
}

/// Helper entry point, called before the window is created.
#[cfg(target_os = "linux")]
fn run_helper(args: &[String]) -> ! {
    use gtk::prelude::*;
    use webkit2gtk::{
        NavigationPolicyDecisionExt, PolicyDecisionExt, SettingsExt, URIRequestExt, UserContentManagerExt, WebViewExt,
    };

    let kind = args.first().map(String::as_str).and_then(Kind::parse);
    let url = args.get(1).map(String::as_str).unwrap_or_default();
    let Some(kind) = kind.filter(|_| valid_player_url(url)) else {
        std::process::exit(2);
    };
    let width = args
        .get(2)
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(640)
        .clamp(90, 1600);
    let height = args
        .get(3)
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(360)
        .clamp(90, 1000);
    // The screen's scale: the page is zoomed to match, so its text is the
    // size it'd be in a browser, not shrunk to fit.
    let zoom = args
        .get(4)
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(1.0)
        .clamp(0.5, 3.0);
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let display = 200 + std::process::id() % 600;
    let socket = runtime.join(format!("godwit-inline-{}.sock", std::process::id()));
    let mut command = std::process::Command::new("broadwayd");
    command
        .arg(format!(":{display}"))
        .arg("--unixsocket")
        .arg(&socket)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // The display goes with the helper, even when the helper is killed
    // (otherwise every stopped player left a broadwayd running).
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: prctl is async-signal-safe and touches only this child.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
    }
    let Ok(mut broadway) = command.spawn() else {
        std::process::exit(3);
    };
    // GTK talks to broadwayd on an abstract socket (`@…/broadwayN+1.socket`),
    // which never shows up as a file: wait for it in /proc/net/unix, which
    // doesn't connect to it.
    let gdk_socket = format!("@{}", runtime.join(format!("broadway{}.socket", display + 1)).display());
    for _ in 0..200 {
        let listening = std::fs::read_to_string("/proc/net/unix")
            .is_ok_and(|table| table.lines().any(|line| line.ends_with(&gdk_socket)));
        if listening {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // SAFETY: this is a fresh helper process, before GTK or any worker thread.
    unsafe {
        std::env::set_var("GDK_BACKEND", "broadway");
        std::env::set_var("BROADWAY_DISPLAY", format!(":{display}"));
    }
    if gtk::init().is_err() {
        let _ = broadway.kill();
        std::process::exit(4);
    }
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_xft_dpi(96 * 1024);
    }
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_default_size(width, height);
    // Broadway reports a touchscreen, so pages saw a phone: YouTube showed
    // its touch controls (mute only, no volume slider). The script makes
    // every frame see a mouse instead.
    let content = webkit2gtk::UserContentManager::new();
    content.add_script(&webkit2gtk::UserScript::new(
        MOUSE_ONLY,
        webkit2gtk::UserContentInjectedFrames::AllFrames,
        webkit2gtk::UserScriptInjectionTime::Start,
        &[],
        &[],
    ));
    let web = webkit2gtk::WebView::builder()
        .web_context(&player_context())
        .user_content_manager(&content)
        .build();
    let (outgoing, to_app) = mpsc::channel::<(u8, String)>();
    // Links the page opens in a new window ("Watch on YouTube") go to the
    // browser, as an iframe's would.
    let links = outgoing.clone();
    web.connect_decide_policy(move |_, decision, kind| {
        if kind != webkit2gtk::PolicyDecisionType::NewWindowAction {
            return false;
        }
        let uri = decision
            .downcast_ref::<webkit2gtk::NavigationPolicyDecision>()
            .and_then(|d| d.navigation_action())
            .and_then(|a| a.request())
            .and_then(|r| r.uri());
        if let Some(uri) = uri {
            let _ = links.send((b'U', uri.to_string()));
        }
        decision.ignore();
        true
    });
    // Fullscreen can't happen inside a message: ask the app to pop it out.
    web.connect_enter_fullscreen(move |_| {
        let _ = outgoing.send((b'X', String::new()));
        true
    });
    if let Some(settings) = WebViewExt::settings(&web) {
        settings.set_hardware_acceleration_policy(webkit2gtk::HardwareAccelerationPolicy::Never);
        settings.set_media_playback_requires_user_gesture(false);
    }
    web.set_zoom_level(zoom);
    web.set_size_request(width, height);
    window.add(&web);
    window.show_all();
    let page = match kind {
        Kind::Embed => iframe_page(url),
        Kind::Video => video_page(url, false),
        Kind::Gif => gif_page(url),
    };
    web.load_html(&page, Some("https://stoat.chat/"));
    web.grab_focus();

    let (commands, receiver) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            if commands.send(line).is_err() {
                break;
            }
        }
        let _ = commands.send("q".into());
    });
    let main_loop = gtk::glib::MainLoop::new(None, false);
    let exit_loop = main_loop.clone();
    let mut output = io::BufWriter::new(io::stdout().lock());
    let mut last_hash = None;
    let mut pressed = false;
    // Until the page draws something, its frames are one flat colour: they'd
    // hide the thumbnail the app shows meanwhile, so they aren't sent.
    let mut drawn = false;
    let (mut width, mut height) = (width, height);
    gtk::glib::timeout_add_local(std::time::Duration::from_millis(33), move || {
        // Moves are merged into the latest, as a browser delivers one per
        // frame: replaying each step put the pointer in gaps a real move
        // jumps (WebKit's volume slider closes when the pointer leaves the
        // mute button for anything but the slider, a few pixels above it).
        let mut pending_move: Option<(f64, f64)> = None;
        for command in receiver.try_iter() {
            let mut fields = command.split_whitespace();
            let op = fields.next().unwrap_or_default();
            if op != "m"
                && let Some((x, y)) = pending_move.take()
            {
                inject::pointer(&web, x, y, pressed, true);
            }
            match op {
                "q" => {
                    exit_loop.quit();
                    return gtk::glib::ControlFlow::Break;
                }
                "s" => {
                    let dimensions = fields
                        .next()
                        .and_then(|w| w.parse::<i32>().ok())
                        .zip(fields.next().and_then(|h| h.parse::<i32>().ok()));
                    if let Some((w, h)) = dimensions.filter(|(w, h)| (2..=2560).contains(w) && (2..=1440).contains(h)) {
                        width = w;
                        height = h;
                        web.set_size_request(w, h);
                        window.resize(w, h);
                        last_hash = None;
                    }
                }
                "d" | "u" | "m" => {
                    let (Some(x), Some(y)) = (
                        fields.next().and_then(|x| x.parse::<f64>().ok()),
                        fields.next().and_then(|y| y.parse::<f64>().ok()),
                    ) else {
                        continue;
                    };
                    if op == "m" {
                        pending_move = Some((x, y));
                        continue;
                    }
                    pressed = op == "d";
                    inject::pointer(&web, x, y, pressed, false);
                }
                _ => {}
            }
        }
        if let Some((x, y)) = pending_move {
            inject::pointer(&web, x, y, pressed, true);
        }
        for (tag, text) in to_app.try_iter() {
            let ok = write_header(&mut output, tag, text.len() as u32, 0).is_ok()
                && output.write_all(text.as_bytes()).is_ok()
                && output.flush().is_ok();
            if !ok {
                exit_loop.quit();
                return gtk::glib::ControlFlow::Break;
            }
        }
        let Ok(mut surface) = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, width, height) else {
            return gtk::glib::ControlFlow::Continue;
        };
        if let Ok(context) = gtk::cairo::Context::new(&surface) {
            web.draw(&context);
        }
        surface.flush();
        if let Ok(data) = surface.data() {
            use std::hash::{Hash, Hasher};
            drawn = drawn || !flat(&data);
            if !drawn {
                return gtk::glib::ControlFlow::Continue;
            }
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            data.hash(&mut hasher);
            let hash = hasher.finish();
            if last_hash != Some(hash) {
                last_hash = Some(hash);
                let ok = write_header(&mut output, b'F', width as u32, height as u32).is_ok()
                    && output.write_all(data.as_ref()).is_ok()
                    && output.flush().is_ok();
                if !ok {
                    exit_loop.quit();
                    return gtk::glib::ControlFlow::Break;
                }
            }
        }
        gtk::glib::ControlFlow::Continue
    });
    main_loop.run();
    let _ = broadway.kill();
    let _ = broadway.wait();
    let _ = std::fs::remove_file(socket);
    std::process::exit(0)
}

#[cfg(windows)]
fn run_helper(args: &[String]) -> ! {
    win_player::run_helper(args)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn run_helper(_: &[String]) -> ! {
    std::process::exit(2)
}

/// The player window (`godwit --player-window <kind> <url>`): the player on
/// the real display, fullscreen. Escape (or the player's fullscreen button)
/// closes it; F11 toggles fullscreen.
#[cfg(target_os = "linux")]
fn run_window(args: &[String]) -> ! {
    use gtk::prelude::*;
    use webkit2gtk::{PolicyDecisionExt, SettingsExt, WebViewExt};

    let kind = args.first().map(String::as_str).and_then(Kind::parse);
    let url = args.get(1).map(String::as_str).unwrap_or_default();
    let Some(kind) = kind.filter(|_| valid_player_url(url)) else {
        std::process::exit(2);
    };
    // The desktop matches it to godwit.desktop, as the main window.
    gtk::glib::set_prgname(Some("godwit"));
    if gtk::init().is_err() {
        std::process::exit(4);
    }
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_title("Godwit player");
    window.set_default_size(1280, 720);
    let web = webkit2gtk::WebView::with_context(&player_context());
    web.connect_decide_policy(|_, decision, kind| {
        if kind == webkit2gtk::PolicyDecisionType::NewWindowAction {
            decision.ignore();
            true
        } else {
            false
        }
    });
    if let Some(settings) = WebViewExt::settings(&web) {
        settings.set_media_playback_requires_user_gesture(false);
    }
    window.add(&web);
    // The window exists to watch fullscreen, so leaving fullscreen closes it
    // and the chat is back: Escape, or the player's own fullscreen button.
    // The page itself never goes fullscreen (that left two fullscreen states
    // fighting, and Escape had to be pressed again and again).
    {
        let window = window.clone();
        web.connect_enter_fullscreen(move |_| {
            window.close();
            true
        });
    }
    web.connect_leave_fullscreen(|_| true);
    window.connect_key_press_event(|w, e| {
        let key = e.keyval();
        if key == gtk::gdk::keys::constants::Escape {
            w.close();
            gtk::glib::Propagation::Stop
        } else if key == gtk::gdk::keys::constants::F11 {
            let full = w
                .window()
                .is_some_and(|g| g.state().contains(gtk::gdk::WindowState::FULLSCREEN));
            if full {
                w.unfullscreen();
            } else {
                w.fullscreen();
            }
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    window.connect_destroy(|_| gtk::main_quit());
    let page = match kind {
        Kind::Embed => iframe_page(&autoplay(url)),
        Kind::Video | Kind::Gif => video_page(url, true),
    };
    web.load_html(&page, Some("https://stoat.chat/"));
    window.show_all();
    window.fullscreen();
    gtk::main();
    std::process::exit(0)
}

#[cfg(windows)]
fn run_window(args: &[String]) -> ! {
    win_player::run_window(args)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn run_window(_: &[String]) -> ! {
    std::process::exit(2)
}

/// Run in every frame of an inline player before its own scripts: no touch
/// support (`ontouchstart`, `maxTouchPoints`, `TouchEvent`) and a mouse to
/// `matchMedia` (hover, fine pointer). Broadway's display has a touchscreen
/// device, which made WebKit tell pages they were on a phone. Media queries
/// in stylesheets can't be changed from here; the players choose their
/// layout in script.
const MOUSE_ONLY: &str = r#"(() => {
  try {
    const touch = ["ontouchstart", "ontouchmove", "ontouchend", "ontouchcancel"];
    for (const target of [window, Window.prototype, document, Document.prototype, Element.prototype, HTMLElement.prototype]) {
      for (const name of touch) {
        try { delete target[name]; } catch (_) {}
      }
    }
    for (const name of ["TouchEvent", "Touch", "TouchList"]) {
      try { delete window[name]; } catch (_) {}
    }
    try { Object.defineProperty(Navigator.prototype, "maxTouchPoints", { get: () => 0, configurable: true }); } catch (_) {}
    const yes = "(min-width: 0px)";
    const no = "(max-width: 0px)";
    const real = window.matchMedia;
    if (typeof real === "function") {
      const mouse = (query) => String(query)
        .replace(/\(\s*(any-)?hover\s*:\s*hover\s*\)/gi, yes)
        .replace(/\(\s*(any-)?hover\s*:\s*none\s*\)/gi, no)
        .replace(/\(\s*(any-)?pointer\s*:\s*fine\s*\)/gi, yes)
        .replace(/\(\s*(any-)?pointer\s*:\s*(coarse|none)\s*\)/gi, no)
        .replace(/\(\s*(any-)?(hover|pointer)\s*\)/gi, yes);
      window.matchMedia = function (query) { return real.call(this, mouse(query)); };
    }
  } catch (_) {}
})();"#;

/// The player URL set to start playing: it was opened to watch.
fn autoplay(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return url.to_owned();
    };
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(k, _)| k != "autoplay")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    parsed
        .query_pairs_mut()
        .clear()
        .extend_pairs(pairs)
        .append_pair("autoplay", "1");
    parsed.into()
}

/// Whether a frame is one colour (sampled: a blank page before it loads).
fn flat(pixels: &[u8]) -> bool {
    let mut samples = pixels.as_chunks::<4>().0.iter().step_by(97);
    let Some(first) = samples.next() else { return true };
    samples.all(|p| p == first)
}

fn valid_player_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|url| url.scheme() == "https" && url.host_str().is_some())
}

fn escape_attr(url: &str) -> String {
    url.chars()
        .map(|c| match c {
            '&' => "&amp;".to_owned(),
            '"' => "&quot;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            _ => c.to_string(),
        })
        .collect()
}

fn iframe_page(url: &str) -> String {
    format!(
        "<!doctype html><meta name=viewport content='width=device-width,initial-scale=1'><body style='margin:0;background:#000;overflow:hidden'><iframe src=\"{}\" scrolling=no allowfullscreen allowtransparency frameborder=0 allow='autoplay; encrypted-media; fullscreen; picture-in-picture' style='border:0;width:100vw;height:100vh'></iframe>",
        escape_attr(url)
    )
}

/// The players' browser context: nothing kept (ephemeral) and, where the
/// tools for it are installed, WebKit's sandbox (bubblewrap) around its web
/// process. Videos and pages come from strangers, and that process is where
/// their decoding (GStreamer) runs: a decoder bug someone crafts a file for
/// then reaches only the sandbox, not the user's files. WebKit aborts when
/// the sandbox is on without bubblewrap, so it's only switched on with it.
#[cfg(target_os = "linux")]
fn player_context() -> webkit2gtk::WebContext {
    use webkit2gtk::WebContextExt;
    let context = webkit2gtk::WebContext::new_ephemeral();
    if on_path("bwrap") && on_path("xdg-dbus-proxy") {
        context.set_sandbox_enabled(true);
    } else {
        eprintln!("note: bubblewrap or xdg-dbus-proxy isn't installed: the player runs without WebKit's sandbox");
    }
    context
}

/// An executable called `name` is on PATH.
#[cfg(target_os = "linux")]
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

/// The official `<video controls preload="metadata">`: its first frame shows
/// in the message without playing; the player window plays it at once.
fn video_page(url: &str, autoplay: bool) -> String {
    format!(
        "<!doctype html><meta name=viewport content='width=device-width,initial-scale=1'><body style='margin:0;background:#000;overflow:hidden'><video controls playsinline preload=metadata{} style='display:block;width:100vw;height:100vh;object-fit:contain'><source src=\"{}\"></video>",
        if autoplay { " autoplay" } else { "" },
        escape_attr(url)
    )
}

/// A GIF provider's video in its message, as the official GIF `<video>`.
fn gif_page(url: &str) -> String {
    format!(
        "<!doctype html><body style='margin:0;background:#000;overflow:hidden'><video autoplay loop muted playsinline preload=metadata style='display:block;width:100vw;height:100vh;object-fit:contain'><source src=\"{}\"></video>",
        escape_attr(url)
    )
}

#[cfg(target_os = "linux")]
mod inject {
    use gtk::gdk;
    use gtk::glib::translate::{ToGlibPtr, ToGlibPtrMut};
    use gtk::prelude::*;

    pub fn pointer(view: &webkit2gtk::WebView, x: f64, y: f64, down: bool, motion: bool) {
        let Some(window) = view.window() else { return };
        let kind = if motion {
            gdk::EventType::MotionNotify
        } else if down {
            gdk::EventType::ButtonPress
        } else {
            gdk::EventType::ButtonRelease
        };
        let mut event = gdk::Event::new(kind);
        let raw = ToGlibPtrMut::<*mut gdk::ffi::GdkEvent>::to_glib_none_mut(&mut event).0;
        let device = window.display().default_seat().and_then(|seat| seat.pointer());
        // SAFETY: the event is allocated for this dispatch and receives an
        // owned reference to the WebKit view's current GDK window.
        unsafe {
            if motion {
                let e = raw as *mut gdk::ffi::GdkEventMotion;
                (*e).window = window.to_glib_full();
                (*e).time = gtk::current_event_time();
                (*e).x = x;
                (*e).y = y;
                (*e).x_root = x;
                (*e).y_root = y;
                (*e).state = if down { gdk::ffi::GDK_BUTTON1_MASK } else { 0 };
            } else {
                let e = raw as *mut gdk::ffi::GdkEventButton;
                (*e).window = window.to_glib_full();
                (*e).time = gtk::current_event_time();
                (*e).x = x;
                (*e).y = y;
                (*e).x_root = x;
                (*e).y_root = y;
                (*e).button = 1;
                (*e).state = if down { 0 } else { gdk::ffi::GDK_BUTTON1_MASK };
            }
        }
        event.set_device(device.as_ref());
        gtk::main_do_event(&mut event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_pages_escape_urls_and_reject_non_https_sources() {
        assert!(valid_player_url("https://media.example/video.mp4"));
        assert!(!valid_player_url("http://media.example/video.mp4"));
        assert!(!valid_player_url("javascript:alert(1)"));
        assert!(video_page("https://media.example/v?a=1&b=2", false).contains("a=1&amp;b=2"));
        assert!(iframe_page("https://player.example/\"x").contains("&quot;x"));
    }

    #[test]
    fn blank_frames_are_flat_and_drawn_ones_are_not() {
        assert!(flat(&[0; 4 * 1000]));
        let mut drawn = vec![0; 4 * 1000];
        drawn[4 * 97] = 255;
        assert!(!flat(&drawn));
    }

    #[test]
    fn inline_videos_preload_without_playing() {
        let inline = video_page("https://media.example/v.mp4", false);
        assert!(inline.contains("preload=metadata") && !inline.contains("autoplay"));
        assert!(video_page("https://media.example/v.mp4", true).contains("autoplay"));
    }

    #[test]
    fn gif_videos_loop_muted_without_controls() {
        let page = gif_page("https://media.example/x.mp4");
        assert!(page.contains("autoplay loop muted") && !page.contains("controls"));
        assert_eq!(Kind::parse("gif"), Some(Kind::Gif));
    }

    #[test]
    fn players_hide_touch_from_pages() {
        assert!(MOUSE_ONLY.contains("maxTouchPoints") && MOUSE_ONLY.contains("ontouchstart"));
        assert!(MOUSE_ONLY.contains("matchMedia"));
    }

    #[test]
    fn popped_out_players_autoplay_once() {
        assert_eq!(
            autoplay("https://player.twitch.tv/?video=1&parent=stoat.chat&autoplay=false"),
            "https://player.twitch.tv/?video=1&parent=stoat.chat&autoplay=1"
        );
        assert_eq!(
            autoplay("https://www.youtube-nocookie.com/embed/x?modestbranding=1"),
            "https://www.youtube-nocookie.com/embed/x?modestbranding=1&autoplay=1"
        );
    }
}
