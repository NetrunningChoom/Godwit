// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A capture thread that can be told to stop and confirms it did.
//!
//! Dropping a [`Worker`] sets its stop flag and waits (bounded) for the
//! thread to finish, so "camera off" means the device was released and no
//! frame goes out afterwards. Capture backends (nokhwa, WebRTC's desktop
//! capturer) have no way to interrupt a blocked read, so the wait is bounded
//! instead: a stuck device is reported, not waited on forever.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::time::Duration;

/// How long dropping waits for the thread. A frame takes ~33 ms at 30 fps.
const STOP_WAIT: Duration = Duration::from_secs(2);

/// Shared with the thread: whether it should stop.
#[derive(Clone)]
pub struct StopFlag(Arc<AtomicBool>);

impl StopFlag {
    pub fn stopped(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub struct Worker {
    stop: Arc<AtomicBool>,
    done: mpsc::Receiver<()>,
    name: &'static str,
}

/// Signals the end of the thread however it ends (return or panic).
struct Done(SyncSender<()>);

impl Drop for Done {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}

impl Worker {
    /// Stop capture immediately without waiting inside a network negotiation.
    /// Drop still confirms completion with the usual bounded wait.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// The flag the thread reads, for a helper that should stop with it.
    pub fn stop_flag(&self) -> StopFlag {
        StopFlag(self.stop.clone())
    }

    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    pub fn spawn(name: &'static str, run: impl FnOnce(StopFlag) + Send + 'static) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, done) = mpsc::sync_channel(1);
        let flag = StopFlag(stop.clone());
        std::thread::Builder::new().name(name.into()).spawn(move || {
            let _done = Done(tx);
            run(flag);
        })?;
        Ok(Self { stop, done, name })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.request_stop();
        if let Err(RecvTimeoutError::Timeout) = self.done.recv_timeout(STOP_WAIT) {
            eprintln!("warning: {} did not stop within {STOP_WAIT:?}", self.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn capture_can_stop_while_its_handle_is_retained_for_unpublication() {
        let (tx, rx) = mpsc::channel();
        let worker = Worker::spawn("test-stop-request", move |stop| {
            while !stop.stopped() {
                std::thread::yield_now();
            }
            tx.send(()).unwrap();
        })
        .unwrap();
        assert!(!worker.stop_requested());
        worker.request_stop();
        assert!(worker.stop_requested());
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(worker);
    }

    #[test]
    fn dropping_waits_until_the_thread_has_ended() {
        let after = Arc::new(AtomicUsize::new(0));
        let seen = after.clone();
        let worker = Worker::spawn("test-capture", move |stop| {
            while !stop.stopped() {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(30));
            seen.store(1, Ordering::SeqCst);
        })
        .unwrap();
        drop(worker);
        assert_eq!(
            after.load(Ordering::SeqCst),
            1,
            "drop returned before the thread finished"
        );
    }

    #[test]
    fn a_thread_that_already_ended_does_not_block() {
        let worker = Worker::spawn("test-capture", |_| {}).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let start = std::time::Instant::now();
        drop(worker);
        assert!(start.elapsed() < Duration::from_millis(500));
    }
}
