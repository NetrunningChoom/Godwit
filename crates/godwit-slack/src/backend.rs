// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The Slack network thread: the sign-in window (the player helper's
//! `--slack-login`), then for every workspace its API and websocket with
//! back-off, and requests through `Api`'s pace. The UI side is `account`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc as tmpsc;

use crate::api::{self, Api, Refused};
use crate::rtm;
use crate::session::Saved;

use godwit_core::backend::Start;
pub use godwit_core::backend::Waker;

/// A request to one workspace, made because the user did something.
#[derive(Debug, Clone)]
pub enum Request {
    /// A conversation's history, or (with `thread`) a thread's: its root and replies.
    History {
        channel: String,
        thread: Option<String>,
        before: Option<String>,
    },
    /// Into the conversation, or (with `thread`) as a reply in that thread.
    Send {
        channel: String,
        thread: Option<String>,
        text: String,
        nonce: String,
        files: Vec<godwit_core::application::Outgoing>,
    },
    Edit {
        channel: String,
        ts: String,
        text: String,
    },
    Delete {
        channel: String,
        ts: String,
    },
    React {
        channel: String,
        ts: String,
        name: String,
        add: bool,
    },
    Mark {
        channel: String,
        ts: String,
    },
    NotificationPrefs,
    Conversation(String),
    OwnState,
    SetOwnPresence(bool),
    SetStatus(Option<String>),
    /// A page of the Threads view: the first, or the one older than this cursor.
    Threads(Option<String>),
    MarkThread {
        channel: String,
        root: String,
        ts: String,
    },
    Users(Vec<String>),
    /// The conversation list again (one we hadn't heard of spoke).
    Conversations,
    /// The workspace's member directory.
    Directory,
    /// Follow these people's presence (over the websocket).
    Presence(Vec<String>),
}

pub enum Cmd {
    /// Open Slack's sign-in in the helper at this path.
    SignIn(PathBuf),
    CancelSignIn,
    UseSession(Saved),
    Request {
        team: String,
        request: Request,
    },
    Logout,
}

#[derive(Debug)]
pub enum Answer {
    History {
        channel: String,
        thread: Option<String>,
        before: Option<String>,
        result: Result<Value, String>,
    },
    Sent {
        channel: String,
        thread: Option<String>,
        nonce: String,
        result: Result<Value, String>,
    },
    /// People's ids, names and pictures.
    Users(Vec<(String, String, Option<String>)>),
    NotificationPrefs(Result<Value, String>),
    Conversation(Result<Value, String>),
    OwnState(Result<Value, String>),
    Threads {
        more: bool,
        result: Result<(Vec<Value>, Option<String>), String>,
    },
    ThreadMarked {
        channel: String,
        root: String,
        ts: String,
        result: Result<(), String>,
    },
    Conversations(Vec<Value>),
    /// The workspace's people: ids, names and pictures.
    Directory(Vec<(String, String, Option<String>)>),
    Emoji(Value),
    Done {
        what: &'static str,
        result: Result<(), String>,
    },
}

#[derive(Debug)]
pub enum Ev {
    /// The sign-in window is open.
    Waiting,
    LoginFailed(String),
    SignedIn(Saved),
    /// A workspace is signed in: who we are, its conversations and their unread counts.
    Ready {
        team: String,
        me: Value,
        conversations: Vec<Value>,
        counts: Value,
        /// The workspace's icon, if it has one.
        icon: Option<String>,
    },
    News {
        team: String,
        event: Value,
    },
    Online {
        team: String,
        online: bool,
    },
    Lost {
        reason: String,
        signed_out: bool,
    },
    Answer {
        team: String,
        answer: Answer,
    },
    LoggedOut,
}

type Out = godwit_core::backend::Events<Ev>;

pub type Handle = godwit_core::backend::Handle<Cmd>;

pub fn spawn(wake: Waker) -> std::io::Result<(Handle, mpsc::Receiver<Ev>)> {
    godwit_core::backend::spawn(
        "slack-net",
        tokio::runtime::Builder::new_current_thread(),
        move |tx| Out::new(tx, wake),
        run,
        |out, e| out.send(Ev::LoginFailed(format!("Couldn't start Slack's network thread: {e}"))),
    )
}

use godwit_core::backend::error_text as text;

fn signed_out(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Refused>().is_some_and(Refused::signed_out)
}

type Child = godwit_core::backend::SignInWindow;

#[derive(Default)]
struct State {
    apis: HashMap<String, Arc<Api>>,
    window: Option<Child>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    requests: tokio::task::JoinSet<()>,
    pending: std::collections::VecDeque<(String, Request)>,
    /// The sign-in starting (and retrying while unreachable).
    starting: Option<tokio::task::JoinHandle<()>>,
}

impl State {
    fn request(&mut self, team: String, request: Request, out: &Out) {
        if let Some(api) = self.apis.get(&team).cloned() {
            let out = out.clone();
            self.requests.spawn(async move {
                let answer = answer(&api, request).await;
                out.send(Ev::Answer { team, answer });
            });
        } else {
            self.pending.push_back((team, request));
        }
    }

    fn stop(&mut self) {
        self.requests.abort_all();
        self.pending.clear();
        if let Some(t) = self.starting.take() {
            t.abort();
        }
        for t in self.tasks.drain(..) {
            t.abort();
        }
        self.apis.clear();
    }

    fn close_window(&mut self) {
        if let Some(window) = self.window.take() {
            godwit_core::backend::close_sign_in_window(&window);
        }
    }
}

/// Runs the sign-in window and reads its one line (blocking: off the runtime).
fn sign_in_window(helper: PathBuf, slot: Child) -> Result<Saved, String> {
    let line =
        godwit_core::backend::sign_in_window(&helper, "--slack-login", &slot).map_err(|e| e.describe("Slack"))?;
    Saved::from_line(&line).ok_or_else(|| "Slack's sign-in window gave no sign-in.".into())
}

/// The workspaces a sign-in loaded, stamped with the generation it started
/// under: a ready result from an older generation (the user signed out or
/// started another sign-in meanwhile) must not install itself.
struct Started {
    generation: u64,
    apis: HashMap<String, Arc<Api>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

async fn run(mut cmds: tmpsc::UnboundedReceiver<Cmd>, out: Out) {
    let mut st = State::default();
    let mut generation = 0_u64;
    // Startup failures come from inside the spawned task; this tells the loop
    // so queued requests don't wait forever for workspaces that never came.
    let (fail_tx, mut fail_rx) = tmpsc::unbounded_channel::<()>();
    let (ready_tx, mut ready_rx) = tmpsc::unbounded_channel::<Started>();
    loop {
        tokio::select! {
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    Cmd::SignIn(helper) => {
                        st.close_window();
                        let slot: Child = Arc::default();
                        st.window = Some(slot.clone());
                        out.send(Ev::Waiting);
                        let (out, ready, fail) = (out.clone(), ready_tx.clone(), fail_tx.clone());
                        st.starting = Some(tokio::spawn(async move {
                            match tokio::task::spawn_blocking(move || sign_in_window(helper, slot)).await {
                                Ok(Ok(saved)) => {
                                    out.send(Ev::SignedIn(saved.clone()));
                                    start(saved, out, ready, generation, fail).await;
                                }
                                Ok(Err(e)) => {
                                    out.send(Ev::LoginFailed(e));
                                    let _ = fail.send(());
                                }
                                Err(_) => {
                                    out.send(Ev::LoginFailed("Slack's sign-in stopped.".into()));
                                    let _ = fail.send(());
                                }
                            }
                        }));
                    }
                    Cmd::CancelSignIn => {
                        // Only the sign-in window goes: a running session stays.
                        st.close_window();
                        generation = generation.wrapping_add(1);
                    }
                    Cmd::UseSession(saved) => {
                        st.stop();
                        generation = generation.wrapping_add(1);
                        let (out, ready) = (out.clone(), ready_tx.clone());
                        let fail = fail_tx.clone();
                        st.starting = Some(tokio::spawn(async move {
                            godwit_core::backend::keep_starting(|| {
                                start(saved.clone(), out.clone(), ready.clone(), generation, fail.clone())
                            })
                            .await;
                        }));
                    }
                    Cmd::Request { team, request } => st.request(team, request, &out),
                    Cmd::Logout => {
                        st.close_window();
                        st.stop();
                        generation = generation.wrapping_add(1);
                        // Signing out of the web session would end it in the
                        // browser too; forgetting it here is the sign-out.
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
                st.apis = ready.apis;
                st.tasks = ready.tasks;
                for (team, request) in pending { st.request(team, request, &out); }
            }
            Some(_) = fail_rx.recv() => {
                // Startup failed: nothing will drain the queue.
                st.pending.clear();
            }
            Some(_) = st.requests.join_next(), if !st.requests.is_empty() => {}
        }
    }
    st.close_window();
    st.stop();
}

type Ready = tmpsc::UnboundedSender<Started>;

/// Every workspace of the sign-in: checked (`auth.test`), its conversations
/// and counts loaded, then its websocket kept up.
async fn start(saved: Saved, out: Out, ready: Ready, generation: u64, fail: tmpsc::UnboundedSender<()>) -> Start {
    let http = match api::client() {
        Ok(h) => h,
        Err(e) => {
            out.send(Ev::Lost {
                reason: text(&e),
                signed_out: false,
            });
            let _ = fail.send(());
            return Start::Done;
        }
    };
    // Whether a workspace failed only because Slack couldn't be reached.
    let mut unreachable = false;
    let mut apis = HashMap::new();
    let mut tasks = Vec::new();
    let mut any = false;
    for team in &saved.teams {
        let api = match Api::new(http.clone(), team.clone(), saved.d.clone()) {
            Ok(a) => Arc::new(a),
            Err(_) => continue,
        };
        let loaded = async {
            let me = api.call("auth.test", &[]).await?;
            let conversations = api.conversations().await?;
            // Unread state is the web client's own call: without it, nothing reads as unread.
            let counts = api.counts().await.unwrap_or(Value::Null);
            let icon = api.call("team.info", &[]).await.ok().and_then(|t| {
                let i = &t["team"]["icon"];
                [i["image_88"].as_str(), i["image_68"].as_str()]
                    .into_iter()
                    .flatten()
                    .find_map(api::picture)
            });
            let emoji = api.call("emoji.list", &[]).await.unwrap_or(Value::Null);
            anyhow::Ok((me, conversations, counts, icon, emoji))
        }
        .await;
        match loaded {
            Ok((me, conversations, counts, icon, emoji)) => {
                any = true;
                out.send(Ev::Ready {
                    team: team.id.clone(),
                    me,
                    conversations,
                    counts,
                    icon,
                });
                // After Ready: the workspace and account only exist once it is handled.
                out.send(Ev::Answer {
                    team: team.id.clone(),
                    answer: Answer::Emoji(emoji),
                });
            }
            Err(e) => {
                unreachable |= !signed_out(&e);
                out.send(Ev::Lost {
                    reason: format!("{}: {}", team.name, text(&e)),
                    signed_out: signed_out(&e) && saved.teams.len() == 1,
                });
                continue;
            }
        }
        apis.insert(team.id.clone(), api.clone());
        let out = out.clone();
        let id = team.id.clone();
        tasks.push(tokio::spawn(async move {
            let (tx, mut rx) = tmpsc::unbounded_channel::<Value>();
            let _forward = {
                let (out, id) = (out.clone(), id.clone());
                tokio::spawn(async move {
                    while let Some(event) = rx.recv().await {
                        out.send(Ev::News {
                            team: id.clone(),
                            event,
                        });
                    }
                })
            };
            let mut wait = Duration::from_secs(5);
            let mut presence = api.presence_list();
            loop {
                let started = tokio::time::Instant::now();
                let ended = rtm::run(&api, &tx, &mut presence, || {
                    out.send(Ev::Online {
                        team: id.clone(),
                        online: true,
                    })
                })
                .await;
                eprintln!(
                    "note: Slack's live updates ended ({}): {}",
                    api.team().name,
                    ended.reason
                );
                out.send(Ev::Online {
                    team: id.clone(),
                    online: false,
                });
                if ended.signed_out {
                    return out.send(Ev::Lost {
                        reason: ended.reason,
                        signed_out: true,
                    });
                }
                if started.elapsed() > Duration::from_secs(120) {
                    wait = Duration::from_secs(5);
                }
                let jitter = Duration::from_millis(rand::random_range(0..3000));
                wait = if godwit_core::net::watch::reconnect_pause("slack", "rtm", wait + jitter).await {
                    Duration::from_secs(5)
                } else {
                    (wait * 2).min(Duration::from_secs(300))
                };
                // What was missed while down: the list says what's new.
                if let Ok(c) = api.conversations().await {
                    out.send(Ev::Answer {
                        team: id.clone(),
                        answer: Answer::Conversations(c),
                    });
                }
            }
        }));
    }
    if any {
        let _ = ready.send(Started {
            generation,
            apis,
            tasks,
        });
        return Start::Done;
    }
    // No workspace loaded: nothing will drain the queued requests.
    let _ = fail.send(());
    if unreachable { Start::Unreachable } else { Start::Done }
}

async fn answer(api: &Api, request: Request) -> Answer {
    let done = |what, r: anyhow::Result<()>| Answer::Done {
        what,
        result: r.map_err(|e| text(&e)),
    };
    match request {
        Request::History {
            channel,
            thread,
            before,
        } => {
            let result = match &thread {
                // A thread comes whole: conversations.replies pages oldest
                // first by response_metadata.next_cursor, so walk the pages
                // (bounded) instead of showing 200 replies and calling it
                // the beginning.
                Some(ts) if before.is_none() => {
                    async {
                        let mut messages: Vec<Value> = Vec::new();
                        let mut cursor: Option<String> = None;
                        let mut has_more = false;
                        for _ in 0..5 {
                            let page = match cursor.as_deref() {
                                None => api.replies(&channel, ts, None).await?,
                                Some(c) => api.replies(&channel, ts, Some(c)).await?,
                            };
                            let count = page["messages"].as_array().map_or(0, Vec::len);
                            messages.extend(page["messages"].as_array().cloned().unwrap_or_default());
                            has_more = page["has_more"].as_bool().unwrap_or(false);
                            cursor = page["response_metadata"]["next_cursor"]
                                .as_str()
                                .filter(|c| !c.is_empty())
                                .map(str::to_owned);
                            if cursor.is_none() || count == 0 {
                                break;
                            }
                        }
                        Ok(serde_json::json!({"messages": messages, "has_more": has_more}))
                    }
                    .await
                }
                Some(_) => Ok(serde_json::json!({"messages": []})),
                None => api.history(&channel, before.as_deref()).await,
            }
            .map_err(|e| text(&e));
            Answer::History {
                channel,
                thread,
                before,
                result,
            }
        }
        Request::Send {
            channel,
            thread,
            text: body,
            nonce,
            files,
        } => {
            let result = api
                .post_files(&channel, thread.as_deref(), &body, &files)
                .await
                .map_err(|e| text(&e));
            Answer::Sent {
                channel,
                thread,
                nonce,
                result,
            }
        }
        Request::Edit {
            channel,
            ts,
            text: body,
        } => done("editing a message", api.update(&channel, &ts, &body).await.map(drop)),
        Request::Delete { channel, ts } => done("deleting a message", api.delete(&channel, &ts).await),
        Request::React { channel, ts, name, add } => done("reacting", api.react(&channel, &ts, &name, add).await),
        Request::Mark { channel, ts } => done("marking read", api.mark(&channel, &ts).await),
        Request::NotificationPrefs => {
            Answer::NotificationPrefs(api.call("users.prefs.get", &[]).await.map_err(|e| text(&e)))
        }
        Request::Conversation(channel) => Answer::Conversation(
            api.call("conversations.info", &[("channel", channel.as_str())])
                .await
                .map_err(|e| text(&e)),
        ),
        Request::OwnState => Answer::OwnState(api.own_state().await.map_err(|e| text(&e))),
        Request::SetOwnPresence(available) => {
            let result = async {
                api.call(
                    "users.setPresence",
                    &[("presence", if available { "auto" } else { "away" })],
                )
                .await?;
                api.keep_active(available);
                api.own_state().await
            }
            .await;
            Answer::OwnState(result.map_err(|e| text(&e)))
        }
        Request::SetStatus(status) => {
            let profile = serde_json::json!({"status_text": status.unwrap_or_default(), "status_emoji": "", "status_expiration": 0}).to_string();
            let result = async {
                api.call("users.profile.set", &[("profile", profile.as_str())]).await?;
                api.own_state().await
            }
            .await;
            Answer::OwnState(result.map_err(|e| text(&e)))
        }
        Request::Threads(cursor) => Answer::Threads {
            more: cursor.is_some(),
            result: api.threads(cursor.as_deref()).await.map_err(|e| text(&e)),
        },
        Request::MarkThread { channel, root, ts } => {
            let result = api.mark_thread(&channel, &root, &ts).await.map_err(|e| text(&e));
            Answer::ThreadMarked {
                channel,
                root,
                ts,
                result,
            }
        }
        Request::Users(ids) => {
            let mut names = Vec::new();
            for id in ids {
                // Bot ids (`B…`): an app's name and icon (`bots.info`).
                if id.starts_with('B') {
                    if let Ok(v) = api.bot(&id).await
                        && let Some(name) = v["bot"]["name"].as_str().filter(|n| !n.is_empty())
                    {
                        let icon = [
                            v["bot"]["icons"]["image_72"].as_str(),
                            v["bot"]["icons"]["image_48"].as_str(),
                        ]
                        .into_iter()
                        .flatten()
                        .find_map(api::picture);
                        names.push((id, name.to_owned(), icon));
                    }
                } else if let Ok(v) = api.user(&id).await
                    && let Some(name) = api::user_name(&v["user"])
                {
                    names.push((id, name, api::user_picture(&v["user"])));
                }
            }
            Answer::Users(names)
        }
        Request::Conversations => match api.conversations().await {
            Ok(c) => Answer::Conversations(c),
            Err(e) => done("loading channels", Err(e)),
        },
        Request::Presence(ids) => {
            api.follow_presence(ids);
            Answer::Done {
                what: "following presence",
                result: Ok(()),
            }
        }
        Request::Directory => match api.directory().await {
            Ok(d) => Answer::Directory(d),
            Err(e) => done("loading the member directory", Err(e)),
        },
    }
}
