// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Discord's loopback hCaptcha page in the shared ephemeral browser.

fn is_page(uri: &str) -> bool {
    let Ok(url) = url::Url::parse(uri) else { return false };
    url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment() == Some("godwit-player")
        && url
            .path()
            .strip_prefix("/captcha/")
            .is_some_and(|path| path.len() == 32 && path.bytes().all(|c| c.is_ascii_hexdigit()))
}

#[cfg(any(target_os = "linux", windows, test))]
fn solution_line(token: &str) -> Option<String> {
    (!token.trim().is_empty() && !token.chars().any(char::is_control))
        .then(|| format!("{{\"token\":{}}}", crate::login_window::quote(token)))
}

#[cfg(target_os = "linux")]
fn read(web: &webkit2gtk::WebView, _: &webkit2gtk::WebContext, found: Box<dyn FnOnce(String)>) {
    use javascriptcore::ValueExt;
    use webkit2gtk::WebViewExt;

    web.evaluate_javascript(
        "typeof window.godwitCaptchaSolution === 'string' ? window.godwitCaptchaSolution : ''",
        None,
        None,
        gtk::gio::Cancellable::NONE,
        move |result| {
            if let Ok(value) = result
                && let Some(line) = solution_line(&value.to_str())
            {
                found(line);
            }
        },
    );
}

/// Windows: the solved token, once the page has it (WebView2 hands a
/// script's answer back as JSON).
#[cfg(windows)]
fn read(web: &wry::WebView) {
    let _ = web.evaluate_script_with_callback(
        "typeof window.godwitCaptchaSolution === 'string' ? window.godwitCaptchaSolution : ''",
        |result| {
            let token: String = serde_json::from_str(&result).unwrap_or_default();
            if let Some(line) = solution_line(&token) {
                crate::login_window::found(line);
            }
        },
    );
}

pub fn run(args: &[String]) -> ! {
    let [page] = args else { std::process::exit(2) };
    if !is_page(page) {
        std::process::exit(2);
    }
    crate::login_window::run_at(
        crate::login_window::Spec {
            network: "Discord",
            start: "",
            agent: crate::login_window::SAFARI,
            ready: is_page,
            redirect: |_| None,
            #[cfg(any(target_os = "linux", windows))]
            read,
        },
        page,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_the_loopback_captcha_page() {
        let page = "http://127.0.0.1:1234/captcha/0123456789abcdef0123456789abcdef#godwit-player";
        assert!(is_page(page));
        for invalid in [
            page.replace("127.0.0.1", "example.com"),
            page.replace("http:", "https:"),
            page.replace(":1234", ""),
            page.replace("/captcha/", "/other/"),
            page.replace("#godwit-player", ""),
            page.replace("#godwit-player", "?x=1#godwit-player"),
            page.replace("0123456789abcdef0123456789abcdef", "short"),
            page.replace("127.0.0.1", "user@127.0.0.1"),
        ] {
            assert!(!is_page(&invalid));
        }
    }

    #[test]
    fn token_is_one_escaped_json_line() {
        assert_eq!(solution_line("a\"b\\c"), Some(r#"{"token":"a\"b\\c"}"#.into()));
        assert_eq!(solution_line(""), None);
        assert_eq!(solution_line("  "), None);
        assert_eq!(solution_line("a\nb"), None);
    }
}
