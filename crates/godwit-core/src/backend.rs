// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Transport plumbing shared by network threads and their account bridges.
use std::{
    future::Future,
    sync::{Arc, mpsc},
};
use tokio::sync::mpsc as tmpsc;

/// Wakes the UI after an event is queued, so it drains the receiver. The UI
/// framework supplies it (Slint: `invoke_from_event_loop`).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

pub struct Handle<C> {
    tx: tmpsc::UnboundedSender<C>,
}
impl<C> Clone for Handle<C> {
    fn clone(&self) -> Self {
        Self { tx: self.tx.clone() }
    }
}
impl<C> Handle<C> {
    pub fn send(&self, cmd: C) {
        let _ = self.tx.send(cmd);
    }
    /// Commands stay in the returned receiver, without touching the network.
    pub fn detached() -> (Self, tmpsc::UnboundedReceiver<C>) {
        let (tx, rx) = tmpsc::unbounded_channel();
        (Self { tx }, rx)
    }
}

/// Runtime choice belongs to the backend: some transports need worker threads.
pub fn spawn<C, E, O, F>(
    name: &str,
    mut runtime: tokio::runtime::Builder,
    output: impl FnOnce(mpsc::Sender<E>) -> O + Send + 'static,
    run: impl FnOnce(tmpsc::UnboundedReceiver<C>, O) -> F + Send + 'static,
    failed: impl FnOnce(O, std::io::Error) + Send + 'static,
) -> std::io::Result<(Handle<C>, mpsc::Receiver<E>)>
where
    C: Send + 'static,
    E: Send + 'static,
    F: Future<Output = ()>,
{
    let (handle, commands) = Handle::detached();
    let (tx, events) = mpsc::channel();
    std::thread::Builder::new().name(name.into()).spawn(move || {
        let out = output(tx);
        match runtime.enable_all().build() {
            Ok(rt) => rt.block_on(run(commands, out)),
            Err(e) => failed(out, e),
        }
    })?;
    Ok((handle, events))
}

/// Drain in order, including events queued while an earlier event is handled.
pub fn poll<S, E, A>(
    state: &mut S,
    mut next: impl FnMut(&mut S) -> Option<E>,
    mut handle: impl FnMut(&mut S, E, &mut Vec<A>),
    out: &mut Vec<A>,
) {
    while let Some(event) = next(state) {
        handle(state, event, out);
    }
}

/// Queue an event before waking its consumer.
pub struct Events<E> {
    tx: mpsc::Sender<E>,
    wake: Waker,
}
impl<E> Clone for Events<E> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            wake: self.wake.clone(),
        }
    }
}
impl<E> Events<E> {
    pub fn new(tx: mpsc::Sender<E>, wake: Waker) -> Self {
        Self { tx, wake }
    }
    pub fn send(&self, event: E) {
        if self.tx.send(event).is_ok() {
            (self.wake)();
        }
    }
    /// Stoat also wakes after a disconnected receiver, preserving its contract.
    pub fn emit(&self, event: E) {
        let _ = self.tx.send(event);
        (self.wake)();
    }
}

/// Retry-After accepts delay seconds or an HTTP date. Keep even long waits;
/// checked_deadline handles values outside the platform clock's range.
pub fn retry_after(value: Option<&str>, fallback: std::time::Duration) -> std::time::Duration {
    use std::time::{Duration, SystemTime};
    value
        .and_then(|v| {
            v.trim().parse::<u64>().ok().map(Duration::from_secs).or_else(|| {
                httpdate::parse_http_date(v)
                    .ok()
                    .map(|at| at.duration_since(SystemTime::now()).unwrap_or_default())
            })
        })
        .unwrap_or(fallback)
        .max(Duration::from_secs(1))
}
pub fn checked_deadline(delay: std::time::Duration) -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    now.checked_add(delay)
        .unwrap_or_else(|| now + std::time::Duration::from_secs(100 * 365 * 24 * 3600))
}

/// Keep an error's context chain when presenting transport failures.
pub fn error_text(error: impl std::fmt::Display) -> String {
    format!("{error:#}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[test]
    fn retry_after_honors_seconds_dates_and_overflow_without_early_retry() {
        use std::time::{Duration, SystemTime};
        assert_eq!(
            retry_after(Some("600"), Duration::from_secs(10)),
            Duration::from_secs(600)
        );
        let later = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(600));
        assert!(retry_after(Some(&later), Duration::from_secs(10)) >= Duration::from_secs(598));
        assert!(checked_deadline(Duration::MAX) > tokio::time::Instant::now());
    }

    #[test]
    fn events_are_queued_before_waking() {
        let (tx, rx) = mpsc::channel();
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = wakes.clone();
        let events = Events::new(
            tx,
            Arc::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            }),
        );
        events.send(1);
        assert_eq!(rx.try_recv(), Ok(1));
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        drop(rx);
        events.send(2);
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        events.emit(3);
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn poll_includes_events_queued_by_handlers() {
        let (tx, rx) = mpsc::channel();
        assert!(tx.send(1).is_ok());
        let mut out = Vec::new();
        poll(
            &mut (tx, rx),
            |s| s.1.try_recv().ok(),
            |s, e, out| {
                out.push(e);
                if e == 1 {
                    assert!(s.0.send(2).is_ok());
                }
            },
            &mut out,
        );
        assert_eq!(out, [1, 2]);
    }
}

/// A sign-in window the player helper runs (`helper flag`): kept here so it
/// can be closed early (`kill` it to cancel).
pub type SignInWindow = Arc<std::sync::Mutex<Option<std::process::Child>>>;

/// Why a sign-in window gave no session: the helper's exit code (1: closed
/// first, 2: not on this system), or that it couldn't start.
#[derive(Debug)]
pub enum SignInStopped {
    Start(std::io::Error),
    Exit(Option<i32>),
}

impl SignInStopped {
    /// The sign-in screen's text, naming `network`.
    pub fn describe(&self, network: &str) -> String {
        match self {
            Self::Start(e) => format!("Couldn't open {network}'s sign-in window: {e}"),
            Self::Exit(Some(1) | None) => "The sign-in window was closed before signing in.".into(),
            Self::Exit(Some(2)) => format!("Signing in to {network} isn't available on this system yet."),
            Self::Exit(Some(code)) => format!("{network}'s sign-in window stopped ({code})."),
        }
    }
}

/// Runs `helper flag` and returns its one stdout line, the session
/// (blocking: call it off the runtime). Its notes go to Godwit's log; the
/// line is never logged.
pub fn sign_in_window(helper: &std::path::Path, flag: &str, window: &SignInWindow) -> Result<String, SignInStopped> {
    use std::io::BufRead;
    let lock = |w: &SignInWindow| w.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    let mut child = std::process::Command::new(helper)
        .arg(flag)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(SignInStopped::Start)?;
    let stdout = child.stdout.take();
    *window.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(child);
    let mut line = String::new();
    if let Some(out) = stdout {
        let _ = std::io::BufReader::new(out).read_line(&mut line);
    }
    let status = lock(window).and_then(|mut c| c.wait().ok());
    if line.trim().is_empty() {
        return Err(SignInStopped::Exit(status.and_then(|s| s.code())));
    }
    Ok(line)
}

/// Closes a sign-in window still open.
pub fn close_sign_in_window(window: &SignInWindow) {
    if let Some(mut child) = window.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// How a sign-in's start went: connected, signed out for good (or nothing
/// more to do), or unreachable for now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Done,
    Unreachable,
}

/// Starts a kept sign-in, trying again while the network is unreachable
/// (offline, its servers down): after 10 s, doubling to 5 min, with a little
/// jitter so accounts don't retry in step. A sign-in that ended for good
/// isn't retried, and nothing here runs once it's up.
pub async fn keep_starting<F, Fut>(mut attempt: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Start>,
{
    let mut wait = std::time::Duration::from_secs(10);
    while attempt().await == Start::Unreachable {
        let jitter = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_millis() % 3000);
        tokio::time::sleep(wait + std::time::Duration::from_millis(u64::from(jitter))).await;
        wait = (wait * 2).min(std::time::Duration::from_secs(300));
    }
}
