// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The Teams network thread: the device-code sign-in, keeping the tokens
//! fresh (the access token and the Skype token last about an hour; they're
//! renewed a few minutes early), the Trouter connection with its back-off,
//! and HTTP requests through `Api`'s pace. The UI side is `account`.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use tokio::sync::mpsc as tmpsc;

use crate::api::{self, Api};
use crate::auth::{self, Reauth};
use crate::session::Saved;
use crate::trouter;

use godwit_core::backend::Start;
pub use godwit_core::backend::Waker;

/// A request made because the user did something.
#[derive(Debug, Clone)]
pub enum Request {
    History {
        conversation: String,
        before: Option<String>,
    },
    Send {
        conversation: String,
        html: String,
        client_id: String,
        my_name: String,
        recipient: Option<String>,
        files: Vec<godwit_core::application::Outgoing>,
    },
    Delete {
        conversation: String,
        message: String,
    },
    Typing {
        conversation: String,
    },
    Ack {
        conversation: String,
        message: String,
        client_id: String,
    },
    /// Names for people seen without one.
    Names(Vec<String>),
    /// The organisation's people directory.
    Directory,
    /// People's presence now (MRIs).
    Presence(Vec<String>),
    SetPresence {
        presence: godwit_core::members::Presence,
        user: String,
    },
    SetStatus(Option<String>),
    /// The conversation list again (a chat we hadn't heard of spoke).
    Conversations,
}

pub enum Cmd {
    /// Start signing in: a device code for the user to enter.
    SignIn,
    CancelSignIn,
    /// Use a kept sign-in.
    UseSession(Saved),
    Request(Request),
    /// Forget the sign-in and stop.
    Logout,
}

#[derive(Debug)]
pub enum Answer {
    History {
        conversation: String,
        before: Option<String>,
        result: Result<Value, String>,
    },
    Sent {
        conversation: String,
        client_id: String,
        result: Result<Value, String>,
    },
    Names(Vec<(String, String)>),
    /// The directory's people: MRIs and names.
    Directory(Vec<(String, String)>),
    /// MRIs, their availability (`Available`, `Away`, …) and custom status
    /// text (`presence.note.message`, as purple-teams reads it).
    Presence(Vec<api::PresenceInfo>),
    OwnPresence(Result<Vec<api::PresenceInfo>, String>),
    OwnStatus {
        status: Option<String>,
        result: Result<(), String>,
    },
    Conversations(Value),
    Done {
        what: &'static str,
        result: Result<(), String>,
    },
}

#[derive(Debug)]
pub enum Ev {
    /// Show this code and address.
    DeviceCode {
        user_code: String,
        url: String,
    },
    LoginFailed(String),
    Media {
        media: godwit_core::media::Media,
        region: String,
    },
    /// Keep this sign-in (new, or its refresh token rotated).
    SignedIn(Saved),
    /// Signed in: who we are and the conversation list.
    Ready {
        me: Value,
        conversations: Value,
    },
    /// The conversation list again, after a reconnect.
    Conversations(Value),
    /// Teams with their channels (the chat service aggregator's answer).
    Teams(Value),
    /// An `EventMessage` from Trouter.
    News(Value),
    /// The push connection is up (`true`) or down, reconnecting.
    Online(bool),
    /// The sign-in ended for good (`signed_out`) or something failed on the way.
    Lost {
        reason: String,
        signed_out: bool,
    },
    Answer(Answer),
    LoggedOut,
}

type Out = godwit_core::backend::Events<Ev>;

pub type Handle = godwit_core::backend::Handle<Cmd>;

pub fn spawn(wake: Waker) -> std::io::Result<(Handle, mpsc::Receiver<Ev>)> {
    godwit_core::backend::spawn(
        "teams-net",
        tokio::runtime::Builder::new_current_thread(),
        move |tx| Out::new(tx, wake),
        run,
        |out, e| out.send(Ev::LoginFailed(format!("Couldn't start Teams' network thread: {e}"))),
    )
}

use godwit_core::backend::error_text as text;

fn reauth(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Reauth>().is_some()
}

/// A random UUID (version 4) for the push endpoint.
fn uuid() -> String {
    let mut b: [u8; 16] = rand::random();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

#[derive(Default)]
struct State {
    api: Option<Arc<Api>>,
    /// The device-code sign-in under way.
    signing: Option<tokio::task::JoinHandle<()>>,
    /// The session's tasks: token renewal and the push connection.
    tasks: Vec<tokio::task::JoinHandle<()>>,
    requests: tokio::task::JoinSet<()>,
    pending: std::collections::VecDeque<Request>,
}

impl State {
    fn request(&mut self, request: Request, out: &Out) {
        if let Some(api) = self.api.clone() {
            let out = out.clone();
            self.requests
                .spawn(async move { out.send(Ev::Answer(answer(&api, request).await)) });
        } else {
            self.pending.push_back(request);
        }
    }

    fn stop(&mut self) {
        self.requests.abort_all();
        self.pending.clear();
        if let Some(t) = self.signing.take() {
            t.abort();
        }
        for t in self.tasks.drain(..) {
            t.abort();
        }
        self.api = None;
    }
}

/// The session a sign-in produced, stamped with the generation it started
/// under: a ready result from an older generation (the user signed out or
/// started another sign-in meanwhile) must not install itself.
struct Started {
    generation: u64,
    api: Arc<Api>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

async fn run(mut cmds: tmpsc::UnboundedReceiver<Cmd>, out: Out) {
    let mut st = State::default();
    let mut generation = 0_u64;
    // Sign-in failures come from inside the spawned task; this tells the loop
    // so queued requests don't wait forever for a session that never came.
    let (fail_tx, mut fail_rx) = tmpsc::unbounded_channel::<()>();
    let (ready_tx, mut ready_rx) = tmpsc::unbounded_channel::<Started>();
    loop {
        tokio::select! {
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    Cmd::SignIn => {
                        if let Some(t) = st.signing.take() {
                            t.abort();
                        }
                        st.stop();
                        generation = generation.wrapping_add(1);
                        let (out, ready, fail) = (out.clone(), ready_tx.clone(), fail_tx.clone());
                        st.signing = Some(tokio::spawn(async move {
                            match device_sign_in(&out).await {
                                Ok(saved) => {
                                    out.send(Ev::SignedIn(saved.clone()));
                                    start(saved, out, ready, generation).await;
                                }
                                Err(e) => {
                                    out.send(Ev::LoginFailed(text(&e)));
                                    let _ = fail.send(());
                                }
                            }
                        }));
                    }
                    Cmd::CancelSignIn => {
                        // Only the sign-in goes: a running session stays.
                        if let Some(t) = st.signing.take() {
                            t.abort();
                        }
                        generation = generation.wrapping_add(1);
                    }
                    Cmd::UseSession(saved) => {
                        st.stop();
                        generation = generation.wrapping_add(1);
                        let (out, ready) = (out.clone(), ready_tx.clone());
                        st.signing = Some(tokio::spawn(async move {
                            godwit_core::backend::keep_starting(|| {
                                start(saved.clone(), out.clone(), ready.clone(), generation)
                            })
                            .await;
                        }));
                    }
                    Cmd::Request(request) => st.request(request, &out),
                    Cmd::Logout => {
                        if let Some(t) = st.signing.take() {
                            t.abort();
                        }
                        st.stop();
                        generation = generation.wrapping_add(1);
                        // Microsoft has no public sign-out for a refresh token:
                        // forgetting it here is the sign-out (the app's sessions
                        // page at mysignins.microsoft.com revokes it for good).
                        out.send(Ev::LoggedOut);
                    }
                }
            }
            Some(ready) = ready_rx.recv() => {
                if ready.generation != generation {
                    continue;
                }
                let pending = std::mem::take(&mut st.pending);
                st.stop();
                st.api = Some(ready.api);
                st.tasks = ready.tasks;
                for request in pending { st.request(request, &out); }
            }
            Some(_) = fail_rx.recv() => {
                // The sign-in failed: nothing will drain the queue.
                st.pending.clear();
            }
            Some(_) = st.requests.join_next(), if !st.requests.is_empty() => {}
        }
    }
    st.stop();
}

/// The device-code flow: show the code, poll at Microsoft's pace until the
/// user finishes or the code expires.
async fn device_sign_in(out: &Out) -> Result<Saved> {
    let http = api::client()?;
    let code = auth::device_code(&http).await?;
    out.send(Ev::DeviceCode {
        user_code: code.user_code.clone(),
        url: code.url.clone(),
    });
    let deadline = tokio::time::Instant::now() + code.expires_in;
    let mut interval = code.interval;
    loop {
        tokio::time::sleep(interval).await;
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(Reauth(
                "The code expired before the sign-in was finished. Try again.".into()
            ));
        }
        match auth::poll(&http, &code).await? {
            auth::Poll::Pending => {}
            // RFC 8628: slow down by five seconds.
            auth::Poll::SlowDown => interval += Duration::from_secs(5),
            auth::Poll::Granted(g) => {
                let refresh = g
                    .refresh_token
                    .ok_or_else(|| anyhow::anyhow!("Microsoft gave no refresh token."))?;
                let mut saved = Saved::new(refresh);
                saved.endpoint = uuid();
                return Ok(saved);
            }
        }
    }
}

/// Fresh tokens for a kept sign-in: an access token for the chat service
/// (rotating the refresh token if Microsoft does), then the Skype token.
async fn tokens(http: &reqwest::Client, saved: &mut Saved, out: &Out) -> Result<(auth::Granted, auth::Skype)> {
    let granted = auth::refresh(http, &saved.tenant, &saved.refresh_token, auth::SPACES_SCOPE).await?;
    if let Some(rotated) = granted.refresh_token.clone().filter(|r| *r != saved.refresh_token) {
        saved.refresh_token = rotated;
        out.send(Ev::SignedIn(saved.clone()));
    }
    let skype = auth::authz(http, &granted.access_token).await?;
    Ok((granted, skype))
}

/// The presence service's token (for people's statuses); without it the
/// dots just stay empty.
async fn load_presence(api: &Api, http: &reqwest::Client, saved: &Saved) {
    match auth::refresh(http, &saved.tenant, &saved.refresh_token, auth::PRESENCE_SCOPE).await {
        Ok(g) => api.set_tokens(|t| t.presence = Some(g.access_token)),
        Err(e) => eprintln!("note: Teams statuses unavailable ({})", text(&e)),
    }
}

/// The aggregator's token, then the teams; failures here don't end the session.
async fn load_teams(api: &Api, http: &reqwest::Client, saved: &Saved, out: &Out) {
    match auth::refresh(http, &saved.tenant, &saved.refresh_token, auth::CSA_SCOPE).await {
        Ok(g) => api.set_tokens(|t| t.csa = Some(g.access_token)),
        Err(e) => {
            return out.send(Ev::Answer(Answer::Done {
                what: "loading teams",
                result: Err(text(&e)),
            }));
        }
    }
    match api.teams().await {
        Ok(v) => out.send(Ev::Teams(v)),
        Err(e) => out.send(Ev::Answer(Answer::Done {
            what: "loading teams",
            result: Err(text(&e)),
        })),
    }
}

/// Signs a kept sign-in in, loads the conversations, and hands the session's
/// tasks (renewal, push) to the thread.
async fn start(mut saved: Saved, out: Out, ready: tmpsc::UnboundedSender<Started>, generation: u64) -> Start {
    if saved.endpoint.is_empty() {
        saved.endpoint = uuid();
        out.send(Ev::SignedIn(saved.clone()));
    }
    let result = async {
        let http = api::client()?;
        let (granted, skype) = tokens(&http, &mut saved, &out).await?;
        let api = Arc::new(Api::new(
            http.clone(),
            &skype,
            granted.access_token.clone(),
            saved.endpoint.clone(),
        ));
        let me = api.me().await?;
        let conversations = api.conversations().await?;
        anyhow::Ok((http, api, me, conversations, granted.expires_in.min(skype.expires_in)))
    }
    .await;
    let (http, api, me, conversations, lifetime) = match result {
        Ok(r) => r,
        Err(e) => {
            let signed_out = reauth(&e);
            out.send(Ev::Lost {
                reason: text(&e),
                signed_out,
            });
            return if signed_out { Start::Done } else { Start::Unreachable };
        }
    };
    if let Some(user) = me["skypeName"].as_str().filter(|u| !u.is_empty()) {
        let user = if user.starts_with("8:") {
            user.to_owned()
        } else {
            format!("8:{user}")
        };
        if user != saved.user {
            saved.user = user;
            out.send(Ev::SignedIn(saved.clone()));
        }
    }
    out.send(Ev::Media {
        media: api.media.clone(),
        region: auth::geo(api.region()).to_owned(),
    });
    // Before Ready: the account asks for statuses as soon as it's ready.
    load_presence(&api, &http, &saved).await;
    if let Err(e) = api.register_presence().await {
        out.send(Ev::Answer(Answer::Done {
            what: "registering presence",
            result: Err(text(&e)),
        }));
    }
    out.send(Ev::Ready { me, conversations });
    load_teams(&api, &http, &saved, &out).await;

    let renewal = {
        let (api, out, http, mut saved) = (api.clone(), out.clone(), http.clone(), saved.clone());
        tokio::spawn(async move {
            let mut lifetime = lifetime;
            loop {
                // Five minutes early, but never more often than once a minute.
                let wait = lifetime
                    .saturating_sub(Duration::from_secs(300))
                    .max(Duration::from_secs(60));
                tokio::time::sleep(wait).await;
                let mut tries = 0u32;
                loop {
                    match tokens(&http, &mut saved, &out).await {
                        Ok((g, s)) => {
                            api.set_tokens(|t| {
                                t.access = g.access_token.clone();
                                t.skype = s.token.clone();
                            });
                            load_presence(&api, &http, &saved).await;
                            lifetime = g.expires_in.min(s.expires_in);
                            break;
                        }
                        Err(e) if reauth(&e) => {
                            return out.send(Ev::Lost {
                                reason: text(&e),
                                signed_out: true,
                            });
                        }
                        // Offline: try again, 30 s doubling to 10 min.
                        Err(_) => {
                            let wait = Duration::from_secs(30 * 2u64.pow(tries.min(4)));
                            tries += 1;
                            tokio::time::sleep(wait).await;
                        }
                    }
                }
            }
        })
    };

    let push = {
        let (api, out, endpoint) = (api.clone(), out.clone(), saved.endpoint.clone());
        tokio::spawn(async move {
            let (news_tx, mut news_rx) = tmpsc::unbounded_channel::<Value>();
            // Ends by itself when this task does (its sender goes).
            let _forward = {
                let out = out.clone();
                tokio::spawn(async move {
                    while let Some(n) = news_rx.recv().await {
                        out.send(Ev::News(n));
                    }
                })
            };
            let mut wait = Duration::from_secs(5);
            loop {
                let started = tokio::time::Instant::now();
                let connected = || {
                    out.send(Ev::Online(true));
                };
                let ended = trouter::run(&api, &endpoint, &news_tx, connected).await;
                // Its own words, never a token: why "reconnecting" shows.
                eprintln!("note: Teams' push connection ended: {}", ended.reason);
                out.send(Ev::Online(false));
                if ended.unauthorized {
                    // The renewal task brings a new token; wait for it.
                    wait = wait.max(Duration::from_secs(60));
                }
                // A connection that held a while starts the back-off over.
                if started.elapsed() > Duration::from_secs(120) {
                    wait = Duration::from_secs(5);
                }
                let jitter = Duration::from_millis(rand::random_range(0..3000));
                wait = if godwit_core::net::watch::reconnect_pause("teams", "push", wait + jitter).await {
                    Duration::from_secs(5)
                } else {
                    (wait * 2).min(Duration::from_secs(300))
                };
                // News missed while down: the list says what's new (one request).
                if let Ok(v) = api.conversations().await {
                    out.send(Ev::Conversations(v));
                }
            }
        })
    };
    let activity = {
        let api = api.clone();
        tokio::spawn(async move {
            // The registered transport's activity lease needs renewal; this is
            // one write every two minutes, not a presence polling loop.
            let mut interval = tokio::time::interval(Duration::from_secs(120));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut outage_reported = false;
            loop {
                interval.tick().await;
                if let Err(error) = api.report_activity_with_recovery().await {
                    if !outage_reported {
                        let status = crate::api::refused_status(&error)
                            .map(|status| status.to_string())
                            .unwrap_or_else(|| "unavailable".into());
                        let action = if status == "404" {
                            "endpoint re-registration or retry failed"
                        } else {
                            "will retry at the next scheduled renewal"
                        };
                        eprintln!("note: Teams activity report failed (HTTP {status}); {action}.");
                        outage_reported = true;
                    }
                } else {
                    outage_reported = false;
                }
            }
        })
    };
    let _ = ready.send(Started {
        generation,
        api,
        tasks: vec![renewal, push, activity],
    });
    Start::Done
}

async fn answer(api: &Api, request: Request) -> Answer {
    let done = |what, r: Result<()>| Answer::Done {
        what,
        result: r.map_err(|e| text(&e)),
    };
    match request {
        Request::History { conversation, before } => {
            let result = api
                .messages(&conversation, before.as_deref())
                .await
                .map_err(|e| text(&e));
            Answer::History {
                conversation,
                before,
                result,
            }
        }
        Request::Send {
            conversation,
            html,
            client_id,
            my_name,
            recipient,
            files,
        } => {
            let result = if files.is_empty() {
                api.send_message(&conversation, &html, &client_id, &my_name).await
            } else if let Some(recipient) = recipient {
                api.send_file(&conversation, &html, &client_id, &my_name, &recipient, &files)
                    .await
            } else {
                Err(anyhow::anyhow!(
                    "Sending Teams files currently works only in one-to-one chats."
                ))
            }
            .map_err(|e| text(&e));
            Answer::Sent {
                conversation,
                client_id,
                result,
            }
        }
        Request::Delete { conversation, message } => {
            done("deleting a message", api.delete_message(&conversation, &message).await)
        }
        Request::Typing { conversation } => done("typing", api.typing(&conversation).await),
        Request::Ack {
            conversation,
            message,
            client_id,
        } => done("marking read", api.mark_read(&conversation, &message, &client_id).await),
        Request::Directory => match api.directory().await {
            Ok(people) => Answer::Directory(people),
            Err(e) => done("loading the people directory", Err(e)),
        },
        // Quietly empty when it can't: dots just stay as they were.
        Request::Presence(mris) => {
            api.follow_presence(&mris);
            Answer::Presence(api.presence(&mris, api.endpoint()).await.unwrap_or_default())
        }
        Request::SetPresence { presence, user } => {
            let result = async {
                api.set_presence(presence).await?;
                api.presence(&[user], api.endpoint()).await
            }
            .await;
            Answer::OwnPresence(result.map_err(|e| text(&e)))
        }
        Request::SetStatus(status) => {
            let result = api.set_status(status.as_deref()).await.map_err(|e| text(&e));
            Answer::OwnStatus { status, result }
        }
        Request::Names(mris) => Answer::Names(api.names(&mris).await.unwrap_or_default()),
        Request::Conversations => match api.conversations().await {
            Ok(v) => Answer::Conversations(v),
            Err(e) => done("loading chats", Err(e)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_version_4_uuids() {
        let u = uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert!(matches!(&u[19..20], "8" | "9" | "a" | "b"));
        assert_ne!(u, uuid());
    }
}
