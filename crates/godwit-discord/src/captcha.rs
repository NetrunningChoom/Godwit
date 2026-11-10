// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Ported from Yapfast's `captcha.rs` (GPL-3.0-or-later).
//!
//! Discord's captchas, which it asks for before a risky request such as
//! trading a QR login's ticket: hCaptcha, with a site key and request data
//! that change each time. A loopback page opens in the player helper's
//! ephemeral window on Linux, or the user's browser when unavailable.
//! The page tells hCaptcha it's on discord.com, as hCaptcha's mobile SDKs
//! name their host, and returns the solution to Godwit. The request is
//! then sent again with it in `X-Captcha-Key`; see
//! https://docs.discord.food/topics/captcha-handling.
//!
//! The listener lives only while the page waits, so nothing stays running
//! once the captcha is solved or signing in stops.

use std::{fmt, time::Duration};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// How long the page waits for someone to solve it.
const WAIT: Duration = Duration::from_secs(10 * 60);
/// The most a request to the page may send: the solution is a few KiB.
const MOST: usize = 64 * 1024;

/// A captcha Discord asked for, from its answer's body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captcha {
    pub sitekey: String,
    /// Data the challenge must carry, when Discord sends it.
    pub rqdata: Option<String>,
    /// What the retried request must carry back, when Discord sends them.
    pub rqtoken: Option<String>,
    pub session_id: Option<String>,
}

impl fmt::Display for Captcha {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Discord asked for a captcha")
    }
}

impl std::error::Error for Captcha {}

impl Captcha {
    /// The hCaptcha a refusal's body asks for. Other services, which
    /// Discord doesn't use now, aren't solved.
    pub fn from_body(body: &Value) -> Option<Self> {
        if !body["captcha_key"].is_array() || body["captcha_service"] != "hcaptcha" {
            return None;
        }
        let text = |key: &str| body[key].as_str().map(str::to_owned);
        Some(Self {
            sitekey: text("captcha_sitekey")?,
            rqdata: text("captcha_rqdata"),
            rqtoken: text("captcha_rqtoken"),
            session_id: text("captcha_session_id"),
        })
    }

    /// The headers a retried request carries with `solution`.
    pub fn headers<'a>(&'a self, solution: &'a str) -> Vec<(&'static str, &'a str)> {
        let mut headers = vec![("X-Captcha-Key", solution)];
        if let Some(rqtoken) = &self.rqtoken {
            headers.push(("X-Captcha-Rqtoken", rqtoken));
        }
        if let Some(session) = &self.session_id {
            headers.push(("X-Captcha-Session-Id", session));
        }
        headers
    }
}

/// How many captchas a request shows before giving up: Discord asks again
/// when it doesn't take a solution.
pub const TRIES: usize = 3;

/// Sends a request until Discord takes it, showing each captcha it asks for
/// first. `send` sends the request, with the last captcha's solution after
/// the first try. `shown` hears the page address only when falling back
/// to the external browser; the helper shows its own window.
pub async fn solving<T, Sent: Future<Output = Result<T>>>(
    mut send: impl FnMut(Option<(Captcha, String)>) -> Sent,
    mut shown: impl FnMut(&str),
) -> Result<T> {
    let mut solved: Option<(Captcha, String)> = None;
    for tries in 0..=TRIES {
        let error = match send(solved.take()).await {
            Ok(answer) => return Ok(answer),
            Err(error) => error,
        };
        let Some(captcha) = error.downcast_ref::<Captcha>().cloned() else {
            return Err(error);
        };
        if tries == TRIES {
            bail!("Discord didn't take the captcha. Try again.");
        }
        let page = Page::serve(&captcha).await?;
        solved = Some((captcha, page.solve(&mut shown).await?));
    }
    bail!("Discord didn't take the captcha. Try again.")
}

/// The page with a captcha, served until it's solved.
pub struct Page {
    /// Where to open it.
    pub url: String,
    path: String,
    html: String,
    listener: TcpListener,
}

impl Page {
    /// Serves `captcha` on the loopback address, at a path no other page
    /// can guess.
    pub async fn serve(captcha: &Captcha) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("could not show the captcha")?;
        let port = listener.local_addr()?.port();
        let path = format!("/captcha/{:032x}", rand::random::<u128>());
        Ok(Self {
            url: format!("http://127.0.0.1:{port}{path}"),
            html: page(captcha),
            path,
            listener,
        })
    }

    /// Uses the helper window (WebKitGTK on Linux, WebView2 on Windows) when
    /// installed; otherwise shows the browser page. Dropping this future
    /// (sign-in cancelled) also closes the helper.
    pub async fn solve(&self, mut shown: impl FnMut(&str)) -> Result<String> {
        #[cfg(any(target_os = "linux", windows))]
        {
            let helper = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|p| p.join("godwit-player")));
            if let Some(helper) = helper {
                let child = tokio::process::Command::new(helper)
                    .arg("--discord-captcha")
                    .arg(format!("{}#godwit-player", self.url))
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::inherit())
                    .kill_on_drop(true)
                    .spawn();
                match child {
                    Ok(mut child) => {
                        let output = async {
                            let mut bytes = Vec::new();
                            let stdout = child.stdout.take().context("captcha window has no output")?;
                            stdout.take((MOST + 1) as u64).read_to_end(&mut bytes).await?;
                            if bytes.len() > MOST {
                                bail!("Invalid answer from the captcha window");
                            }
                            let status = child.wait().await?;
                            if !status.success() {
                                bail!("The captcha window closed before the captcha was solved. Try again.");
                            }
                            solution_line(&bytes).context("Invalid answer from the captcha window")
                        };
                        return tokio::time::timeout(WAIT, async {
                            tokio::select! {
                                result = output => result,
                                result = self.solution() => result,
                            }
                        })
                        .await
                        .context("Nobody solved the captcha in time. Try again.")?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => bail!("Could not open the captcha window. Try again."),
                }
            }
        }
        shown(&self.url);
        self.solution().await
    }

    /// Waits for the solution: the page is sent to whoever asks for it,
    /// and the solution comes back as a POST to the same path.
    pub async fn solution(&self) -> Result<String> {
        tokio::time::timeout(WAIT, async {
            loop {
                let (stream, _) = self.listener.accept().await?;
                match self.answer(stream).await {
                    Ok(Some(solution)) => return Ok(solution),
                    Ok(None) => {}
                    Err(error) => godwit_core::verbose!("discord: a captcha page request failed: {error}"),
                }
            }
        })
        .await
        .context("Nobody solved the captcha in time. Try again.")?
    }

    /// Answers one request, giving the solution if it brought one.
    async fn answer(&self, mut stream: TcpStream) -> Result<Option<String>> {
        let request = read(&mut stream).await?;
        let (head, body) = request.split_once("\r\n\r\n").context("an incomplete request")?;
        let mut words = head.split(' ');
        let (method, path) = (words.next(), words.next());
        if path != Some(self.path.as_str()) {
            respond(&mut stream, "404 Not Found", "text/plain", "").await?;
            return Ok(None);
        }
        match method {
            Some("GET") => {
                respond(&mut stream, "200 OK", "text/html; charset=utf-8", &self.html).await?;
                Ok(None)
            }
            Some("POST") if !body.trim().is_empty() => {
                respond(&mut stream, "204 No Content", "text/plain", "").await?;
                Ok(Some(body.trim().to_owned()))
            }
            _ => {
                respond(&mut stream, "400 Bad Request", "text/plain", "").await?;
                Ok(None)
            }
        }
    }
}

/// A whole request: its head, and as much body as it says it has.
async fn read(stream: &mut TcpStream) -> Result<String> {
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            bail!("the request ended early");
        }
        request.extend_from_slice(&chunk[..read]);
        if request.len() > MOST {
            bail!("the request is too large");
        }
        let text = String::from_utf8_lossy(&request);
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if body.len() >= length {
                return Ok(text.into_owned());
            }
        }
    }
}

async fn respond(stream: &mut TcpStream, status: &str, kind: &str, body: &str) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

/// The helper's single JSON line. Never include its contents in an error.
#[cfg(any(target_os = "linux", windows, test))]
fn solution_line(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    if text.trim().lines().count() != 1 {
        return None;
    }
    let value: Value = serde_json::from_str(text).ok()?;
    let token = value.get("token")?.as_str()?;
    (!token.trim().is_empty() && !token.chars().any(char::is_control)).then(|| token.to_owned())
}

/// The page: hCaptcha's widget for `captcha`, which posts its solution
/// back to the address it came from.
fn page(captcha: &Captcha) -> String {
    // As a JavaScript string, which can't end the script element early.
    let script = |value: &str| serde_json::to_string(value).unwrap_or_default().replace("</", "<\\/");
    let rqdata = captcha.rqdata.as_deref().map_or("null".to_owned(), script);
    format!(
        r#"<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width">
<title>Sign in to Discord with Godwit</title>
<style>
  body {{ margin: 0; min-height: 100vh; display: flex; flex-direction: column;
    align-items: center; justify-content: center; gap: 16px;
    background: #1e1f22; color: #dbdee1; font: 15px system-ui, sans-serif; }}
  h1 {{ font-size: 20px; margin: 0; }}
  p {{ margin: 0; color: #b5bac1; }}
</style>
<h1>Sign in to Discord with Godwit</h1>
<p id="note">Discord wants to check you're a person before it signs you in.</p>
<div id="captcha"></div>
<script>
  const sitekey = {sitekey};
  const rqdata = {rqdata};
  const note = document.getElementById("note");
  function ready() {{
    const id = hcaptcha.render("captcha", {{ sitekey, theme: "dark", callback: solved }});
    if (rqdata) hcaptcha.setData(id, {{ rqdata }});
  }}
  async function solved(solution) {{
    if (location.hash === '#godwit-player') {{
      window.godwitCaptchaSolution = solution;
      return;
    }}
    note.textContent = "Sending it to Godwit...";
    try {{
      const answer = await fetch(location.pathname, {{ method: "POST", body: solution }});
      note.textContent = answer.ok
        ? "Done. You can close this page and go back to Godwit."
        : "Godwit didn't take it. Start signing in again in Godwit.";
    }} catch {{
      note.textContent = "Godwit stopped waiting. Start signing in again in Godwit.";
    }}
  }}
</script>
<script src="https://js.hcaptcha.com/1/api.js?render=explicit&onload=ready&recaptchacompat=off&host=discord.com" async defer></script>
</html>
"#,
        sitekey = script(&captcha.sitekey),
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn captcha() -> Captcha {
        Captcha::from_body(&json!({
            "captcha_key": ["captcha-required"],
            "captcha_service": "hcaptcha",
            "captcha_sitekey": "a9b5fb07-92ff-493f-86fe-352a2803b3df",
            "captcha_rqdata": "data</script>",
            "captcha_rqtoken": "token",
            "captcha_session_id": "session",
        }))
        .unwrap()
    }

    #[test]
    fn parses_helper_solution_without_accepting_empty_or_extra_lines() {
        assert_eq!(solution_line(b"{\"token\":\"solved\"}\n"), Some("solved".into()));
        for invalid in [
            b"{}".as_slice(),
            b"{\"token\":\"\"}",
            b"{\"token\":7}",
            b"{\"token\":\"x\"}\n{}",
            b"{\"token\":\"x\\n\"}",
            b"{\"token\":\"   \"}",
            b"not json",
            b"\xff",
        ] {
            assert_eq!(solution_line(invalid), None);
        }
    }

    #[test]
    fn reads_discords_captcha() {
        let captcha = captcha();
        assert_eq!(captcha.rqdata.as_deref(), Some("data</script>"));
        assert_eq!(
            captcha.headers("solved"),
            [
                ("X-Captcha-Key", "solved"),
                ("X-Captcha-Rqtoken", "token"),
                ("X-Captcha-Session-Id", "session"),
            ]
        );
        assert_eq!(
            Captcha::from_body(&json!({"captcha_key": ["x"], "captcha_service": "recaptcha"})),
            None
        );
        assert_eq!(Captcha::from_body(&json!({"message": "no"})), None);
    }

    #[tokio::test]
    async fn serves_the_page_and_takes_the_solution() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let page = Page::serve(&captcha()).await.unwrap();
        let url = page.url.clone();
        let solution = tokio::spawn(async move { page.solution().await });
        let client = reqwest::Client::new();
        let html = client.get(&url).send().await.unwrap().text().await.unwrap();
        assert!(html.contains("host=discord.com"), "{html}");
        assert!(html.contains("window.godwitCaptchaSolution = solution"));
        assert!(html.contains("location.hash === '#godwit-player'"));
        assert!(html.contains(r#""data<\/script>""#), "{html}");
        let elsewhere = url.rsplit_once('/').unwrap().0.to_owned() + "/guess";
        let status = client.get(elsewhere).send().await.unwrap().status();
        assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
        client.post(&url).body("solved").send().await.unwrap();
        assert_eq!(solution.await.unwrap().unwrap(), "solved");
    }

    #[tokio::test]
    async fn a_challenged_request_is_sent_again_once_with_the_solution() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let sent = std::cell::RefCell::new(Vec::new());
        let result = solving(
            |solved| {
                sent.borrow_mut().push(solved.clone());
                async move {
                    match solved {
                        None => Err(anyhow::Error::new(captcha())),
                        Some(_) => Ok("joined"),
                    }
                }
            },
            |url| {
                let url = url.to_owned();
                tokio::spawn(async move {
                    let _ = reqwest::Client::new().post(url).body("solved").send().await;
                });
            },
        )
        .await;
        assert_eq!(result.unwrap(), "joined");
        let sent = sent.into_inner();
        assert_eq!(sent.len(), 2);
        assert!(sent[0].is_none());
        assert_eq!(sent[1].as_ref().map(|(_, s)| s.as_str()), Some("solved"));
    }

    #[tokio::test]
    async fn other_failures_are_not_retried() {
        let mut tries = 0;
        let result: Result<()> = solving(
            |_| {
                tries += 1;
                async { Err(anyhow::anyhow!("nope")) }
            },
            |_| {},
        )
        .await;
        assert!(result.is_err());
        assert_eq!(tries, 1);
    }
}
