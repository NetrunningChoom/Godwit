// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Google Chat's sign-in, in its own window (`godwit-player --gchat-login`):
//! Google's own page (passwords, passkeys, two-step, Workspace single
//! sign-on). Once Chat's web app has loaded, the session is the Google
//! cookies it sends, handed over as `{"cookies": [[name, value], …]}`; the
//! app keeps only the ones Chat needs (`godwit_gchat::session::KEPT`).

use crate::login_window::quote;

/// Google's sign-in, going on to Chat.
const START: &str = "https://accounts.google.com/ServiceLogin?continue=https%3A%2F%2Fchat.google.com%2F";

/// Chat's web app: chat.google.com, or Chat inside Gmail's address.
pub fn is_client(uri: &str) -> bool {
    crate::login_window::https_parts(uri).is_some_and(|(host, path)| {
        host == "chat.google.com" || (host == "mail.google.com" && path.starts_with("/chat"))
    })
}

/// The session line from `(name, value)` cookies; `None` without the two
/// every request needs.
pub fn session_line(cookies: &[(String, String)]) -> Option<String> {
    let has = |name: &str| cookies.iter().any(|(n, v)| n == name && !v.is_empty());
    if !(has("SID") && has("SAPISID")) {
        return None;
    }
    let pairs: Vec<String> = cookies
        .iter()
        .map(|(n, v)| format!("[{},{}]", quote(n), quote(v)))
        .collect();
    Some(format!("{{\"cookies\":[{}]}}", pairs.join(",")))
}

#[cfg(target_os = "linux")]
fn read(_web: &webkit2gtk::WebView, context: &webkit2gtk::WebContext, found: Box<dyn FnOnce(String)>) {
    use webkit2gtk::{CookieManagerExt, WebContextExt};

    let Some(cookies) = context.cookie_manager() else {
        return;
    };
    cookies.cookies("https://chat.google.com/", gtk::gio::Cancellable::NONE, move |result| {
        let Ok(mut cookies) = result else { return };
        let pairs: Vec<(String, String)> = cookies
            .iter_mut()
            .filter_map(|c| Some((c.name()?.to_string(), c.value()?.to_string())))
            .collect();
        if let Some(line) = session_line(&pairs) {
            found(line);
        }
    });
}

/// Windows: every cookie a request to Chat would carry (HttpOnly ones too).
#[cfg(windows)]
fn read(web: &wry::WebView) {
    let Ok(cookies) = web.cookies_for_url("https://chat.google.com/") else {
        return;
    };
    let pairs: Vec<(String, String)> = cookies
        .iter()
        .map(|c| (c.name().to_owned(), c.value().to_owned()))
        .collect();
    if let Some(line) = session_line(&pairs) {
        crate::login_window::found(line);
    }
}

pub fn run() -> ! {
    crate::login_window::run(crate::login_window::Spec {
        network: "Google Chat",
        start: START,
        agent: crate::login_window::SAFARI,
        ready: is_client,
        redirect: |_| None,
        #[cfg(any(target_os = "linux", windows))]
        read,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_chats_web_app_is_read() {
        assert!(is_client("https://chat.google.com/u/0/app/home"));
        assert!(is_client("https://mail.google.com/chat/u/0/"));
        assert!(!is_client("https://mail.google.com/mail/u/0/"));
        assert!(!is_client("https://accounts.google.com/ServiceLogin"));
        assert!(!is_client("https://evil.example/https://chat.google.com/"));
        assert!(!is_client("http://chat.google.com/"));
    }

    #[test]
    fn the_session_needs_sid_and_sapisid() {
        let c = |n: &str, v: &str| (n.to_string(), v.to_string());
        let line = session_line(&[c("SID", "a"), c("SAPISID", "b\"c")]).unwrap();
        assert_eq!(line, r#"{"cookies":[["SID","a"],["SAPISID","b\"c"]]}"#);
        assert!(session_line(&[c("SID", "a")]).is_none());
        assert!(session_line(&[c("SID", "a"), c("SAPISID", "")]).is_none());
    }
}
