// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The Google Chat network thread: the sign-in window (the player helper's
//! `--gchat-login`), then the web API (`Api`). Requests are made only
//! because the user did something; there's no push yet (the WebChannel
//! stream comes next), so nothing here polls. The UI side is `account`.

use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use tokio::sync::Mutex;
use tokio::sync::mpsc as tmpsc;

use crate::api::Api;
use crate::proto;
use crate::session::Saved;

use godwit_core::backend::Start;
pub use godwit_core::backend::Waker;

pub enum Change {
    Edit(String),
    Delete,
    React { emoji: String, add: bool },
}

pub enum Cmd {
    /// Open Google's sign-in in the helper at this path.
    SignIn(PathBuf),
    CancelSignIn,
    UseSession(Saved),
    /// A conversation's newest messages (`dm:…` / `space:…`).
    History {
        group: String,
        before: Option<i64>,
        page: i32,
    },
    Send {
        group: String,
        text: String,
        nonce: String,
        files: Vec<godwit_core::application::Outgoing>,
        /// The message this quotes, if a reply.
        reply_to: Option<crate::api::Target>,
    },
    Typing {
        group: String,
    },
    /// Edit, delete or react to a message; the conversation is fetched again
    /// after (no push yet), so it shows the result.
    Change {
        target: crate::api::Target,
        change: Change,
        room: String,
    },
    /// People's names and pictures, by user id.
    Users(Vec<String>),
    /// People's presence now (a snapshot: there's no push yet).
    Presence(Vec<String>),
    SetPresence(godwit_core::members::Presence),
    SetStatus(Option<String>),
    MarkRead {
        group: String,
        timestamp: i64,
    },
    World,
    Logout,
}

#[derive(Debug)]
pub enum Ev {
    Stream(Box<proto::Event>),
    StreamConnected,
    StreamDisconnected,
    World(Vec<proto::WorldItemLite>),
    Read {
        group: String,
        timestamp: i64,
        result: Result<(), String>,
    },
    Waiting,
    LoginFailed(String),
    SignedIn(Saved),
    Ready {
        me: String,
        world: Vec<proto::WorldItemLite>,
    },
    History {
        group: String,
        result: Result<(Vec<proto::Message>, Option<i64>, bool), String>,
    },
    Sent {
        group: String,
        nonce: String,
        result: Result<Box<proto::Message>, String>,
    },
    Users(Vec<proto::User>),
    Presence(Vec<proto::UserPresence>),
    OwnState(Result<Box<proto::UserStatus>, String>),
    /// A change the user asked for didn't go through.
    Failed(String),
    Lost {
        reason: String,
        signed_out: bool,
    },
    LoggedOut,
}

type Out = godwit_core::backend::Events<Ev>;

pub type Handle = godwit_core::backend::Handle<Cmd>;

pub fn spawn(wake: Waker) -> std::io::Result<(Handle, mpsc::Receiver<Ev>)> {
    godwit_core::backend::spawn(
        "gchat-net",
        tokio::runtime::Builder::new_current_thread(),
        move |tx| Out::new(tx, wake),
        run,
        |out, e| {
            out.send(Ev::LoginFailed(format!(
                "Couldn't start Google Chat's network thread: {e}"
            )))
        },
    )
}

use godwit_core::backend::error_text as text;

/// A fresh id for a message we send, as maugclib makes them
/// (`hangups%{random u64}`). Godwit's nonces restart every session
/// (`pending-1`, …), and Chat refuses an id it has seen with 409 Conflict.
fn local_id() -> String {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 8];
    // The system's randomness; if it fails, the time is still unique enough.
    let n = if ring::rand::SystemRandom::new().fill(&mut bytes).is_ok() {
        u64::from_le_bytes(bytes)
    } else {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64)
    };
    format!("hangups%{n}")
}

/// Messages per conversation fetched at once.
pub(crate) const PAGE: i32 = 30;

struct Started {
    generation: u64,
    api: Arc<Mutex<Api>>,
    saved: Saved,
    me: String,
    own_status: proto::UserStatus,
    world: Vec<proto::WorldItemLite>,
}

async fn run(mut cmds: tmpsc::UnboundedReceiver<Cmd>, out: Out) {
    let mut api: Option<Arc<Mutex<Api>>> = None;
    let mut stream: Option<tokio::task::JoinHandle<()>> = None;
    let (_, mut news_rx) = tmpsc::unbounded_channel();
    let mut requests = tokio::task::JoinSet::new();
    let mut generation = 0_u64;
    let active = tokio::sync::watch::Sender::new(true);
    let mut window: Option<godwit_core::backend::SignInWindow> = None;
    // The kept sign-in starting (and retrying while unreachable).
    let mut starting: Option<tokio::task::JoinHandle<()>> = None;
    let (ready_tx, mut ready_rx) = tmpsc::unbounded_channel::<Started>();
    loop {
        tokio::select! {
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { break };
                if matches!(&cmd, Cmd::SignIn(_) | Cmd::CancelSignIn | Cmd::UseSession(_) | Cmd::Logout) {
                    generation = generation.wrapping_add(1);
                    requests.abort_all();
                    if let Some(t) = stream.take() { t.abort(); }
                    if let Some(t) = starting.take() { t.abort(); }
                    api = None;
                    let (_, receiver) = tmpsc::unbounded_channel();
                    news_rx = receiver;
                }
                match cmd {
                    Cmd::SignIn(helper) => {
                        if let Some(w) = window.take() {
                            godwit_core::backend::close_sign_in_window(&w);
                        }
                        let slot = godwit_core::backend::SignInWindow::default();
                        window = Some(slot.clone());
                        out.send(Ev::Waiting);
                        let (out, ready) = (out.clone(), ready_tx.clone());
                        starting = Some(tokio::spawn(async move {
                            let line = tokio::task::spawn_blocking(move || {
                                godwit_core::backend::sign_in_window(&helper, "--gchat-login", &slot)
                            })
                            .await;
                            match line {
                                Ok(Ok(line)) => match Saved::from_line(&line) {
                                    Some(saved) => {
                                        out.send(Ev::SignedIn(saved.clone()));
                                        start(saved, out, ready, generation).await;
                                    }
                                    None => out.send(Ev::LoginFailed("Google's sign-in gave no Chat sign-in.".into())),
                                },
                                Ok(Err(e)) => out.send(Ev::LoginFailed(e.describe("Google Chat"))),
                                Err(_) => out.send(Ev::LoginFailed("Google Chat's sign-in stopped.".into())),
                            }
                        }));
                    }
                    Cmd::CancelSignIn => {
                        if let Some(w) = window.take() {
                            godwit_core::backend::close_sign_in_window(&w);
                        }
                    }
                    Cmd::UseSession(saved) => {
                        if let Some(t) = stream.take() { t.abort(); }
                        api = None;
                        if let Some(t) = starting.take() {
                            t.abort();
                        }
                        let (out, ready) = (out.clone(), ready_tx.clone());
                        starting = Some(tokio::spawn(async move {
                            godwit_core::backend::keep_starting(|| start(saved.clone(), out.clone(), ready.clone(), generation)).await;
                        }));
                    }
                    Cmd::History { group, before, page } => {
                        if let Some(api) = api.clone() {
                            let out = out.clone();
                            requests.spawn(async move {
                                let result = api.lock().await.topic_history(&group, page, before).await.map_err(|e| text(&e));
                                out.send(Ev::History { group, result });
                            });
                        }
                    }
                    Cmd::Send { group, text: body, nonce, files, reply_to } => {
                        if let Some(api) = api.clone() {
                            let out = out.clone();
                            requests.spawn(async move {
                                let result = api.lock().await.send(&group, &body, &local_id(), &files, reply_to.as_ref()).await;
                                let result = result
                                    .map(Box::new).map_err(|e| text(&e));
                                out.send(Ev::Sent { group, nonce, result });
                            });
                        }
                    }
                    Cmd::Typing { group } => {
                        if let Some(api) = api.clone() {
                            requests.spawn(async move {
                                let _ = api.lock().await.set_typing(&group).await;
                            });
                        }
                    }
                    Cmd::Change { target, change, room } => {
                        if let Some(api) = api.clone() {
                            let out = out.clone();
                            requests.spawn(async move {
                                let mut api = api.lock().await;
                                let (what, done) = match &change {
                                    Change::Edit(body) => ("editing", api.edit(&target, body).await),
                                    Change::Delete => ("deleting", api.delete(&target).await),
                                    Change::React { emoji, add } => ("reacting", api.react(&target, emoji, *add).await),
                                };
                                if let Err(e) = done {
                                    out.send(Ev::Failed(format!("{what}: {}", text(&e))));
                                }
                                // The conversation again, now showing the change.
                                let group = room;
                                let result = api.topic_history(&group, PAGE, None).await.map_err(|e| text(&e));
                                out.send(Ev::History { group, result });
                            });
                        }
                    }
                    Cmd::Presence(ids) => {
                        if let Some(api) = api.clone() {
                            let out = out.clone();
                            requests.spawn(async move {
                                if let Ok(presence) = api.lock().await.presence(&ids).await {
                                    out.send(Ev::Presence(presence));
                                }
                            });
                        }
                    }
                    Cmd::Users(ids) => {
                        if let Some(api) = api.clone() {
                            let out = out.clone();
                            requests.spawn(async move {
                                if let Ok(users) = api.lock().await.users(&ids).await {
                                    out.send(Ev::Users(users));
                                }
                            });
                        }
                    }
                    Cmd::SetPresence(presence) => {
                        if let Some(api) = api.clone() {
                            let (out, active) = (out.clone(), active.clone());
                            requests.spawn(async move {
                                let result = api.lock().await.set_own_presence(presence).await;
                                if result.is_ok() { active.send_if_modified(|value| { let next = presence != godwit_core::members::Presence::Idle; if *value == next { false } else { *value = next; true } }); }
                                out.send(Ev::OwnState(result.map(Box::new).map_err(|e| text(&e))));
                            });
                        }
                    }
                    Cmd::SetStatus(status) => {
                        if let Some(api) = api.clone() {
                            let out = out.clone();
                            requests.spawn(async move { out.send(Ev::OwnState(api.lock().await.set_custom_status(status.as_deref()).await.map(Box::new).map_err(|e| text(&e)))); });
                        }
                    }
                    Cmd::MarkRead { group, timestamp } => {
                        if let Some(api) = api.clone() {
                            let out = out.clone();
                            requests.spawn(async move {
                                let result = api.lock().await.mark_read(&group,timestamp).await.map_err(|e| text(&e));
                                out.send(Ev::Read { group, timestamp, result });
                            });
                        }
                    }
                    Cmd::World => {
                        if let Some(api) = api.clone() {
                            let out = out.clone();
                            requests.spawn(async move {
                                match api.lock().await.world(100).await {
                                    Ok(world) => out.send(Ev::World(world)),
                                    Err(e) => { out.send(Ev::World(Vec::new())); out.send(Ev::Failed(text(&e))); },
                                }
                            });
                        }
                    }
                    Cmd::Logout => {
                        if let Some(t) = stream.take() { t.abort(); }
                        if let Some(w) = window.take() {
                            godwit_core::backend::close_sign_in_window(&w);
                        }
                        // Signing out of the web session would sign the
                        // browser out of Google too; forgetting it is the
                        // sign-out.
                        api = None;
                        if let Some(t) = starting.take() {
                            t.abort();
                        }
                        out.send(Ev::LoggedOut);
                    }
                }
            }
            Some(ready) = ready_rx.recv() => {
                if ready.generation != generation { continue; }
                api = Some(ready.api);
                active.send_replace(!ready.own_status.has_presence_shared() || ready.own_status.presence_shared());
                out.send(Ev::Ready { me: ready.me, world: ready.world });
                out.send(Ev::OwnState(Ok(Box::new(ready.own_status))));
                if let Some(t) = stream.take() { t.abort(); }
                let (sender, receiver) = tmpsc::unbounded_channel();
                news_rx = receiver;
                stream = Some(tokio::spawn(crate::channel::run(ready.saved, sender, active.subscribe())));
            }
            Some(_) = requests.join_next(), if !requests.is_empty() => {}
            Some(news) = news_rx.recv(), if api.is_some() => match news {
                crate::channel::News::Connected => out.send(Ev::StreamConnected),
                crate::channel::News::Disconnected { signed_out: true } => out.send(Ev::Lost { signed_out: true, reason: "Google Chat signed this stream out".into() }),
                crate::channel::News::Disconnected { signed_out: false } => out.send(Ev::StreamDisconnected),
                crate::channel::News::Event(event) => out.send(Ev::Stream(event)),
            },
        }
    }
    if let Some(t) = stream {
        t.abort();
    }
    if let Some(t) = starting {
        t.abort();
    }
    if let Some(w) = window.take() {
        godwit_core::backend::close_sign_in_window(&w);
    }
}

/// Checks the sign-in (XSRF token, who we are) and loads the conversation
/// list, once.
async fn start(saved: Saved, out: Out, ready: tmpsc::UnboundedSender<Started>, generation: u64) -> Start {
    let loaded = async {
        let mut api = Api::new(&saved)?;
        api.refresh().await?;
        let me = api.self_status().await?;
        let world = api.world(100).await?;
        anyhow::Ok((
            api,
            me.user_status.user_id.id().to_owned(),
            me.user_status.into_option().unwrap_or_default(),
            world,
        ))
    }
    .await;
    match loaded {
        Ok((api, me, own_status, world)) => {
            let _ = ready.send(Started {
                generation,
                api: Arc::new(Mutex::new(api)),
                saved,
                me,
                own_status,
                world,
            });
            Start::Done
        }
        Err(e) => {
            let signed_out = crate::api::signed_out(&e);
            out.send(Ev::Lost {
                signed_out,
                reason: text(&e),
            });
            if signed_out { Start::Done } else { Start::Unreachable }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_send_gets_a_fresh_local_id() {
        let (a, b) = (super::local_id(), super::local_id());
        assert!(a.starts_with("hangups%") && b.starts_with("hangups%"));
        assert_ne!(a, b, "a reused id is refused with 409 Conflict");
    }
}
