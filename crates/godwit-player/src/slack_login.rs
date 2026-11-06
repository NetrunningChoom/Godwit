// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Slack's sign-in, in its own window (`godwit-player --slack-login`): Slack
//! has no device-code sign-in, and its own page handles passwords, magic
//! links, single sign-on and two-factor. Once its web app has loaded, the
//! session is what it keeps: each workspace's client token (`xoxc-…`, in the
//! page's `localConfig_v2`) and the `d` cookie that goes with them.
//!
//! The browser profile is ephemeral (nothing on disk), and the session goes
//! to Godwit as one JSON line on stdout, a pipe only the app reads:
//! `{"d": "…", "teams": [{"id", "name", "url", "token", "user"}]}`. Exit
//! status 1: the window was closed first. The session is never logged.

/// Slack's sign-in page.
const START: &str = "https://slack.com/signin";
/// Reads the workspaces the web app signed in to (never throws).
#[cfg(any(target_os = "linux", windows))]
const READ_TEAMS: &str = r#"(() => { try {
  const c = JSON.parse(localStorage.getItem('localConfig_v2') || '{}');
  return JSON.stringify(Object.values(c.teams || {}).filter(t => t && t.token)
    .map(t => ({ id: t.id, name: t.name, url: t.url, token: t.token, user: t.user_id })));
} catch (e) { return '[]'; } })()"#;

/// Whether `uri` is Slack's web app, where the session can be read.
pub fn is_client(uri: &str) -> bool {
    uri.strip_prefix("https://app.slack.com/client")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || rest.starts_with('?'))
}

/// The session line, from the page's workspaces (JSON) and the `d` cookie;
/// `None` until both are there.
pub fn session_line(teams_json: &str, d: &str) -> Option<String> {
    let teams: serde_like::Teams = serde_like::parse(teams_json)?;
    if teams == 0 || d.is_empty() {
        return None;
    }
    Some(format!(
        "{{\"d\":{},\"teams\":{teams_json}}}",
        crate::login_window::quote(d)
    ))
}

/// Just enough JSON for this, without a JSON crate in the player: the
/// page's answer is checked to be a non-empty array whose entries all hold
/// an `xoxc-` token, then passed on as it is.
mod serde_like {
    pub type Teams = usize;

    pub fn parse(s: &str) -> Option<Teams> {
        let s = s.trim();
        if !(s.starts_with('[') && s.ends_with(']')) || s.contains('\n') {
            return None;
        }
        let n = s.matches("\"token\":\"xoxc-").count();
        let entries = s.matches("\"id\":").count();
        (n > 0 && n == entries).then_some(n)
    }
}

/// After signing in, Slack's "open the desktop app / use Slack in your
/// browser" page: straight on to the web app, where the session is.
fn redirect(uri: &str) -> Option<&'static str> {
    let (host, path) = crate::login_window::https_parts(uri)?;
    (host.ends_with(".slack.com") && (path.starts_with("/ssb") || path.starts_with("/messages")))
        .then_some("https://app.slack.com/client")
}

/// The workspaces from the page, then the `d` cookie.
#[cfg(target_os = "linux")]
fn read(web: &webkit2gtk::WebView, context: &webkit2gtk::WebContext, found: Box<dyn FnOnce(String)>) {
    use javascriptcore::ValueExt;
    use webkit2gtk::{CookieManagerExt, WebContextExt, WebViewExt};

    let context = context.clone();
    web.evaluate_javascript(READ_TEAMS, None, None, gtk::gio::Cancellable::NONE, move |result| {
        let Ok(value) = result else { return };
        let teams = value.to_str().to_string();
        let Some(cookies) = context.cookie_manager() else {
            return;
        };
        cookies.cookies("https://app.slack.com/", gtk::gio::Cancellable::NONE, move |result| {
            let Ok(mut cookies) = result else { return };
            if let Some(line) = session_line(&teams, &crate::login_window::cookie(&mut cookies, "d")) {
                found(line);
            }
        });
    });
}

/// Windows: the `d` cookie, then the workspaces (WebView2 hands a script's
/// answer back as JSON).
#[cfg(windows)]
fn read(web: &wry::WebView) {
    let d = web
        .cookies_for_url("https://app.slack.com/")
        .unwrap_or_default()
        .iter()
        .find(|c| c.name() == "d")
        .map(|c| c.value().to_owned())
        .unwrap_or_default();
    let _ = web.evaluate_script_with_callback(READ_TEAMS, move |result| {
        let teams: String = serde_json::from_str(&result).unwrap_or_default();
        if let Some(line) = session_line(&teams, &d) {
            crate::login_window::found(line);
        }
    });
}

pub fn run() -> ! {
    crate::login_window::run(crate::login_window::Spec {
        network: "Slack",
        start: START,
        agent: crate::login_window::SAFARI,
        ready: is_client,
        redirect,
        #[cfg(any(target_os = "linux", windows))]
        read,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_web_app_is_read() {
        assert!(is_client("https://app.slack.com/client/T1/C2"));
        assert!(is_client("https://app.slack.com/client"));
        assert!(!is_client("https://app.slack.com/clientx"));
        assert!(!is_client("https://evil.example/https://app.slack.com/client"));
        assert!(!is_client("http://app.slack.com/client"));
    }

    #[test]
    fn the_session_needs_tokens_and_the_cookie() {
        let teams = r#"[{"id":"T1","name":"Acme","url":"https://acme.slack.com/","token":"xoxc-1","user":"U1"}]"#;
        let line = session_line(teams, "d-co\"okie").unwrap();
        assert!(line.starts_with(r#"{"d":"d-co\"okie","teams":[{"#));
        assert!(session_line("[]", "d").is_none());
        assert!(session_line(teams, "").is_none());
        assert!(session_line(r#"[{"id":"T1","token":"xoxb-1"}]"#, "d").is_none());
    }
}
