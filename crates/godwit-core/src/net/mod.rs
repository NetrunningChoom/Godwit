// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The one way every network crate sends HTTP: [`fetch`] (the whole answer
//! read) or [`stream`] (the answer handed back unread). Each request says
//! what [`Class`] it is, and the layer
//!
//! - logs it, when the network debug log is on ([`log`], [`redact`]);
//! - holds back a request that keeps getting the same answer ([`guard`]),
//!   for the classes where repeating is a bug, never for live traffic;
//! - keeps a generous ceiling on each network's requests per minute.
//!
//! Connections that stay open (websockets, long polls) pace their
//! reconnects with [`Backoff`] and follow the device's network through
//! [`watch`].

pub mod backoff;
pub mod guard;
pub mod log;
pub mod redact;
pub mod watch;

pub use backoff::Backoff;

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use reqwest::header::{self, HeaderMap};
use reqwest::{Method, RequestBuilder, Response, StatusCode};

/// What a request is, which decides how the repeat guard treats it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Reading: history, search, profiles, lists. The same answer again and
    /// again means a loop, so repeats are spaced out (2 s doubling to 60 s).
    Read,
    /// Expected to repeat (polls, presence, typing, long polls): never
    /// spaced out for repeating, only kept this far apart at the least
    /// (per endpoint: its query and body don't count).
    Poll(Duration),
    /// Sending, editing, reacting: never held back; an identical write
    /// right after another is only noted in the log.
    Write,
    /// Signing in, refreshing tokens: failures in a row are spaced out.
    Auth,
    /// Guard off, logged only: media, uploads, anything real-time or paced
    /// by its caller.
    Live,
}

impl Class {
    /// The usual class for a method: GET and HEAD read, the rest write.
    pub fn of(method: &Method) -> Self {
        if *method == Method::GET || *method == Method::HEAD {
            Self::Read
        } else {
            Self::Write
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Poll(_) => "poll",
            Self::Write => "write",
            Self::Auth => "auth",
            Self::Live => "live",
        }
    }
}

/// A whole answer: status, headers and body.
#[derive(Clone, Debug)]
pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: bytes::Bytes,
}

impl Reply {
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_slice(&self.body)
    }
    /// The body as text (invalid UTF-8 replaced).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn header(&self, name: impl header::AsHeaderName) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

fn guard() -> MutexGuard<'static, guard::Guard> {
    static GUARD: LazyLock<Mutex<guard::Guard>> = LazyLock::new(Mutex::default);
    GUARD.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What's known about a request before it goes out.
struct Meta {
    network: &'static str,
    class: Class,
    key: u64,
    method: Method,
    endpoint: String,
    ids: Vec<String>,
    request_hash: String,
    request_bytes: Option<usize>,
    request_shape: Option<String>,
}

fn is_proto(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.contains("protobuf"))
}

impl Meta {
    fn of(network: &'static str, class: Class, request: &reqwest::Request, ignore: &[&str]) -> Self {
        let body = request.body().and_then(reqwest::Body::as_bytes);
        let mut h = DefaultHasher::new();
        network.hash(&mut h);
        request.method().as_str().hash(&mut h);
        let url = request.url();
        if let Class::Poll(_) = class {
            // A poll's floor holds per endpoint, whatever its counters say.
            url.host_str().hash(&mut h);
            url.path().hash(&mut h);
        } else if ignore.is_empty() {
            url.as_str().hash(&mut h);
        } else {
            url.host_str().hash(&mut h);
            url.path().hash(&mut h);
            for (k, v) in url.query_pairs().filter(|(k, _)| !ignore.contains(&k.as_ref())) {
                k.hash(&mut h);
                v.hash(&mut h);
            }
        }
        if !matches!(class, Class::Poll(_)) {
            body.hash(&mut h);
        }
        // Two accounts asking the same thing aren't one loop.
        for name in [header::AUTHORIZATION, header::COOKIE] {
            if let Some(v) = request.headers().get(&name) {
                v.as_bytes().hash(&mut h);
            }
        }
        let key = h.finish();
        let (endpoint, ids) = redact::endpoint(request.url());
        Self {
            network,
            class,
            key,
            method: request.method().clone(),
            endpoint,
            ids,
            request_hash: redact::short_hash(&key.to_le_bytes()),
            request_bytes: body.map(<[u8]>::len),
            request_shape: body
                .filter(|_| log::enabled() && is_proto(request.headers()))
                .and_then(redact::proto_shape),
        }
    }

    /// Asks the guard for this request's slot and waits for it.
    async fn wait(&self) -> Duration {
        let wait = guard().before(self.network, self.key, self.class, Instant::now());
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        wait
    }

    fn finish(&self, outcome: Outcome<'_>, held: Duration, started: Instant) {
        let (fingerprint, ok) = match &outcome {
            Outcome::Answer { status, body, len, .. } => {
                let mut h = DefaultHasher::new();
                status.as_u16().hash(&mut h);
                match body {
                    Some(b) => b.hash(&mut h),
                    None => len.hash(&mut h),
                }
                (Some(h.finish()), status.is_success())
            }
            Outcome::Failed(_) => (None, false),
        };
        let mut note = guard().after(self.key, self.class, fingerprint, ok, Instant::now());
        note.held = held;
        if note.stepped_up && !log::enabled() {
            // Without the log, a loop still says where it is (no ids).
            eprintln!(
                "warning: {} {} {} got the same answer {} times; spacing it out",
                self.network, self.method, self.endpoint, note.repeats
            );
        }
        if !log::enabled() {
            return;
        }
        let mut line = format!(
            "{} {} {} {}",
            self.network,
            self.class.label(),
            self.method,
            self.endpoint
        );
        if !self.ids.is_empty() {
            line.push_str(&format!(" ids={}", self.ids.join(",")));
        }
        line.push_str(&format!(" req={}", self.request_hash));
        if let Some(n) = self.request_bytes {
            line.push_str(&format!(" out={n}B"));
        }
        if let Some(shape) = &self.request_shape {
            line.push_str(&format!(" out-shape=[{shape}]"));
        }
        match outcome {
            Outcome::Answer {
                status,
                body,
                len,
                proto,
            } => {
                line.push_str(&format!(" -> {}", status.as_u16()));
                if let Some(n) = body.map(|b| b.len()).or(len) {
                    line.push_str(&format!(" in={n}B"));
                }
                if let Some(fp) = fingerprint {
                    line.push_str(&format!(" ans={}", redact::short_hash(&fp.to_le_bytes())));
                }
                if proto && let Some(shape) = body.and_then(redact::proto_shape) {
                    line.push_str(&format!(" in-shape=[{shape}]"));
                }
            }
            Outcome::Failed(e) => line.push_str(&format!(" -> failed ({e})")),
        }
        line.push_str(&format!(" {}ms", started.elapsed().as_millis()));
        if !note.held.is_zero() {
            line.push_str(&format!(" held={:.1}s", note.held.as_secs_f32()));
        }
        if note.repeats >= guard::TRIGGER {
            line.push_str(&format!(" LOOP×{}", note.repeats));
        }
        if note.echo {
            line.push_str(" ECHO");
        }
        log::line(&line);
    }
}

enum Outcome<'a> {
    Answer {
        status: StatusCode,
        body: Option<&'a [u8]>,
        len: Option<usize>,
        proto: bool,
    },
    /// Why no answer came (a kind, never the error's text: it may hold the URL).
    Failed(&'static str),
}

fn failure(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else if e.is_body() || e.is_decode() {
        "body"
    } else if e.is_redirect() {
        "redirect"
    } else if e.is_builder() {
        "request"
    } else {
        "other"
    }
}

/// Sends `request` and reads the whole answer, through the guard and log.
pub async fn fetch(network: &'static str, class: Class, request: RequestBuilder) -> reqwest::Result<Reply> {
    fetch_with(network, class, request, &[]).await
}

/// [`fetch`] where the query parameters named in `ignore` (a request
/// counter, a cache buster) don't make a request a different one.
pub async fn fetch_with(
    network: &'static str,
    class: Class,
    request: RequestBuilder,
    ignore: &[&str],
) -> reqwest::Result<Reply> {
    let (client, request) = request.build_split();
    let request = request?;
    let meta = Meta::of(network, class, &request, ignore);
    fetch_meta(meta, &client, request).await
}

/// [`fetch`] for a request already built (to adjust its headers first).
pub async fn fetch_built(
    network: &'static str,
    class: Class,
    client: &reqwest::Client,
    request: reqwest::Request,
) -> reqwest::Result<Reply> {
    let meta = Meta::of(network, class, &request, &[]);
    fetch_meta(meta, client, request).await
}

async fn fetch_meta(meta: Meta, client: &reqwest::Client, request: reqwest::Request) -> reqwest::Result<Reply> {
    let held = meta.wait().await;
    let started = Instant::now();
    let response = match client.execute(request).await {
        Ok(r) => r,
        Err(e) => {
            meta.finish(Outcome::Failed(failure(&e)), held, started);
            return Err(e);
        }
    };
    let status = response.status();
    let headers = response.headers().clone();
    match response.bytes().await {
        Ok(body) => {
            meta.finish(
                Outcome::Answer {
                    status,
                    body: Some(&body),
                    len: None,
                    proto: is_proto(&headers),
                },
                held,
                started,
            );
            Ok(Reply { status, headers, body })
        }
        Err(e) => {
            meta.finish(Outcome::Failed(failure(&e)), held, started);
            Err(e)
        }
    }
}

/// Sends `request` and hands back the answer unread (downloads, streams,
/// long polls, callers that read it themselves), through the guard and log.
/// The answer is told apart by its status and length only.
pub async fn stream(network: &'static str, class: Class, request: RequestBuilder) -> reqwest::Result<Response> {
    let (client, request) = request.build_split();
    stream_built(network, class, &client, request?).await
}

/// [`stream`] with the class its method suggests ([`Class::of`]): the
/// drop-in for `RequestBuilder::send`.
pub async fn send(network: &'static str, request: RequestBuilder) -> reqwest::Result<Response> {
    let (client, request) = request.build_split();
    let request = request?;
    let class = Class::of(request.method());
    stream_built(network, class, &client, request).await
}

/// [`stream`] for a request already built (to adjust its headers first).
pub async fn stream_built(
    network: &'static str,
    class: Class,
    client: &reqwest::Client,
    request: reqwest::Request,
) -> reqwest::Result<Response> {
    let meta = Meta::of(network, class, &request, &[]);
    let held = meta.wait().await;
    let started = Instant::now();
    match client.execute(request).await {
        Ok(response) => {
            let len = response.content_length().and_then(|n| usize::try_from(n).ok());
            meta.finish(
                Outcome::Answer {
                    status: response.status(),
                    body: None,
                    len,
                    proto: false,
                },
                held,
                started,
            );
            Ok(response)
        }
        Err(e) => {
            meta.finish(Outcome::Failed(failure(&e)), held, started);
            Err(e)
        }
    }
}
