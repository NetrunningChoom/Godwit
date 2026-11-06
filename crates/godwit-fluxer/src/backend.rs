// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The Fluxer network thread: signing in (discovery, password, CAPTCHA, a
//! code), the gateway session with its reconnects, and HTTP requests, one
//! at a time through `Api`'s pace. The UI side is `account`.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc as tmpsc;

use crate::api::{Api, Login};
use crate::gateway::{Dispatch, Gateway};
use crate::session::Saved;

pub use godwit_core::backend::Waker;

/// A request to Fluxer's HTTP API, made because the user did something.
#[derive(Debug, Clone)]
pub enum Request {
    History {
        channel: String,
        before: Option<String>,
    },
    Send {
        channel: String,
        content: String,
        nonce: String,
        reply: Option<String>,
        files: Vec<godwit_core::application::Outgoing>,
    },
    Edit {
        channel: String,
        message: String,
        content: String,
    },
    Delete {
        channel: String,
        message: String,
    },
    React {
        channel: String,
        message: String,
        emoji: String,
        on: bool,
    },
    Typing {
        channel: String,
    },
    Ack {
        channel: String,
        message: String,
    },
}

pub enum Cmd {
    /// Sign in with an email (or username) and password on the official instance.
    Login {
        login: String,
        password: String,
    },
    /// The authenticator (or a backup) code after `Ev::NeedsCode`.
    Code(String),
    /// Use a kept sign-in.
    UseSession(Saved),
    Request(Request),
    /// A gateway command (op, data).
    Gateway(u64, Value),
    /// End the session at Fluxer, then stop.
    Logout,
}

#[derive(Debug)]
pub enum Answer {
    History {
        channel: String,
        before: Option<String>,
        result: Result<Value, String>,
    },
    Sent {
        channel: String,
        nonce: String,
        result: Result<Value, String>,
    },
    Done {
        what: &'static str,
        result: Result<(), String>,
    },
}

#[derive(Debug)]
pub enum Ev {
    /// Signing in failed; why.
    LoginFailed(String),
    /// A second-factor code is needed.
    NeedsCode,
    /// Signed in: keep this sign-in.
    SignedIn(Saved),
    /// The instance's media address, for pictures.
    Media(String),
    Dispatch(Dispatch),
    /// The connection dropped (it reconnects) or ended for good.
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
        "fluxer-net",
        tokio::runtime::Builder::new_current_thread(),
        move |tx| Out::new(tx, wake),
        run,
        |out, e| out.send(Ev::LoginFailed(format!("Couldn't start Fluxer's network thread: {e}"))),
    )
}

use godwit_core::backend::error_text as text;

#[derive(Default)]
struct State {
    api: Option<Arc<Api>>,
    /// Signing in: the ticket waiting for a code, and who.
    ticket: Option<(Arc<Api>, String)>,
    gateway: Option<tmpsc::UnboundedSender<(u64, Value)>>,
    session: Option<tokio::task::JoinHandle<()>>,
    /// Finding the kept sign-in's instance (retrying while unreachable).
    starting: Option<tokio::task::JoinHandle<()>>,
}

async fn run(mut cmds: tmpsc::UnboundedReceiver<Cmd>, out: Out) {
    let mut st = State::default();
    // A kept sign-in's instance, once found (see `UseSession`).
    let (found_tx, mut found_rx) = tmpsc::unbounded_channel::<(crate::discovery::Instance, Saved)>();
    loop {
        let cmd = tokio::select! {
            cmd = cmds.recv() => match cmd {
                Some(cmd) => cmd,
                None => break,
            },
            Some((instance, saved)) = found_rx.recv() => {
                connect(&mut st, &out, instance, saved);
                continue;
            }
        };
        match cmd {
            Cmd::Login { login, password } => {
                let result = async {
                    let instance = Api::discover(crate::discovery::OFFICIAL).await?;
                    let api = Arc::new(Api::new(instance, String::new())?);
                    let answer = api.login(&login, &password).await?;
                    anyhow::Ok((api, answer))
                }
                .await;
                match result {
                    Ok((_, Login::Token { token, user })) => sign_in(&mut st, &out, token, user).await,
                    Ok((api, Login::Mfa { ticket })) => {
                        st.ticket = Some((api, ticket));
                        out.send(Ev::NeedsCode);
                    }
                    Err(e) => out.send(Ev::LoginFailed(text(e))),
                }
            }
            Cmd::Code(code) => {
                let Some((api, ticket)) = st.ticket.clone() else {
                    continue;
                };
                match api.totp(&ticket, &code).await {
                    Ok(Login::Token { token, user }) => {
                        st.ticket = None;
                        sign_in(&mut st, &out, token, user).await;
                    }
                    Ok(Login::Mfa { .. }) => out.send(Ev::NeedsCode),
                    Err(e) => out.send(Ev::LoginFailed(text(e))),
                }
            }
            // The instance first; unreachable, it's tried again (`keep_starting`).
            Cmd::UseSession(saved) => {
                if let Some(t) = st.starting.take() {
                    t.abort();
                }
                let (out, found) = (out.clone(), found_tx.clone());
                st.starting = Some(tokio::spawn(async move {
                    godwit_core::backend::keep_starting(|| {
                        let (saved, out, found) = (saved.clone(), out.clone(), found.clone());
                        async move {
                            match Api::discover(&saved.origin).await {
                                Ok(instance) => {
                                    let _ = found.send((instance, saved));
                                    godwit_core::backend::Start::Done
                                }
                                Err(e) => {
                                    out.send(Ev::Lost {
                                        reason: text(e),
                                        signed_out: false,
                                    });
                                    godwit_core::backend::Start::Unreachable
                                }
                            }
                        }
                    })
                    .await;
                }));
            }
            Cmd::Request(request) => {
                if let Some(api) = st.api.clone() {
                    let out = out.clone();
                    // One at a time is `Api`'s pace; answers come back in any order.
                    tokio::spawn(async move { out.send(Ev::Answer(answer(&api, request).await)) });
                }
            }
            Cmd::Gateway(op, data) => {
                if let Some(gateway) = &st.gateway {
                    let _ = gateway.send((op, data));
                }
            }
            Cmd::Logout => {
                if let Some(t) = st.starting.take() {
                    t.abort();
                }
                if let Some(session) = st.session.take() {
                    session.abort();
                }
                st.gateway = None;
                if let Some(api) = st.api.take() {
                    let _ = api.logout().await;
                }
                out.send(Ev::LoggedOut);
            }
        }
    }
}

async fn sign_in(st: &mut State, out: &Out, token: String, user: String) {
    let saved = Saved {
        user,
        token,
        origin: crate::discovery::OFFICIAL.to_owned(),
    };
    match Api::discover(&saved.origin).await {
        Ok(instance) => {
            out.send(Ev::SignedIn(saved.clone()));
            connect(st, out, instance, saved);
        }
        Err(e) => out.send(Ev::LoginFailed(text(e))),
    }
}

/// Starts the gateway session: connects, passes dispatches on, and after a
/// loss waits (10 s, doubling to 5 min) and connects again, until the
/// sign-in is refused.
fn connect(st: &mut State, out: &Out, instance: crate::discovery::Instance, saved: Saved) {
    let api = match Api::new(instance.clone(), saved.token.clone()) {
        Ok(api) => Arc::new(api),
        Err(e) => return out.send(Ev::LoginFailed(text(e))),
    };
    out.send(Ev::Media(instance.media.clone()));
    st.api = Some(api);
    let (tx, mut commands) = tmpsc::unbounded_channel();
    st.gateway = Some(tx);
    if let Some(old) = st.session.take() {
        old.abort();
    }
    let out = out.clone();
    st.session = Some(tokio::spawn(async move {
        let (dtx, mut drx) = tmpsc::unbounded_channel::<Dispatch>();
        let forward = {
            let out = out.clone();
            tokio::spawn(async move {
                while let Some(d) = drx.recv().await {
                    out.send(Ev::Dispatch(d));
                }
            })
        };
        let mut gateway = Gateway::new(&instance.gateway, &saved.token);
        let mut wait = Duration::from_secs(10);
        loop {
            let started = tokio::time::Instant::now();
            let ended = gateway.run(&mut commands, &dtx).await;
            out.send(Ev::Lost {
                reason: ended.reason.clone(),
                signed_out: ended.signed_out,
            });
            if ended.signed_out {
                break;
            }
            // A connection that held a while starts the back-off over.
            if started.elapsed() > Duration::from_secs(120) {
                wait = Duration::from_secs(10);
            }
            let jitter = Duration::from_millis(rand::random_range(0..3000));
            tokio::time::sleep(wait + jitter).await;
            wait = (wait * 2).min(Duration::from_secs(300));
        }
        forward.abort();
    }));
}

async fn answer(api: &Api, request: Request) -> Answer {
    match request {
        Request::History { channel, before } => {
            let result = api.history(&channel, before.as_deref()).await.map_err(text);
            Answer::History {
                channel,
                before,
                result,
            }
        }
        Request::Send {
            channel,
            content,
            nonce,
            reply,
            files,
        } => {
            let result = api
                .send_message(&channel, &content, &nonce, reply.as_deref(), &files)
                .await
                .map_err(text);
            Answer::Sent { channel, nonce, result }
        }
        Request::Edit {
            channel,
            message,
            content,
        } => Answer::Done {
            what: "editing a message",
            result: api
                .edit_message(&channel, &message, &content)
                .await
                .map(drop)
                .map_err(text),
        },
        Request::Delete { channel, message } => Answer::Done {
            what: "deleting a message",
            result: api.delete_message(&channel, &message).await.map_err(text),
        },
        Request::React {
            channel,
            message,
            emoji,
            on,
        } => Answer::Done {
            what: "reacting",
            result: api.react(&channel, &message, &emoji, on).await.map_err(text),
        },
        Request::Typing { channel } => Answer::Done {
            what: "typing",
            result: api.typing(&channel).await.map_err(text),
        },
        Request::Ack { channel, message } => Answer::Done {
            what: "marking read",
            result: api.ack(&channel, &message).await.map_err(text),
        },
    }
}
