// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Checks screen-share sound capture without a call: records for a few
//! seconds and prints how many 10 ms frames came and their peak level.
//! Nothing is saved or sent.
//!
//! `cargo run -p godwit-voice --example stream_audio_probe -- [seconds] [pid]`
//! (no pid: every app but this one).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use godwit_voice::stream_audio::{self, Target};

fn main() {
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);
    let target = match args.next().and_then(|s| s.parse().ok()) {
        Some(pid) => Target::App(pid),
        None => Target::AllButGodwit,
    };
    let frames = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicU32::new(0));
    let (f, p) = (frames.clone(), peak.clone());
    let capture = stream_audio::start(target, move |frame: &[i16]| {
        f.fetch_add(1, Ordering::Relaxed);
        let loudest = frame.iter().map(|s| s.unsigned_abs() as u32).max().unwrap_or(0);
        p.fetch_max(loudest, Ordering::Relaxed);
    });
    match capture {
        Ok(capture) => {
            std::thread::sleep(std::time::Duration::from_secs(seconds));
            drop(capture);
            println!(
                "target={target:?} frames={} (of ~{} for {seconds}s) peak={}",
                frames.load(Ordering::Relaxed),
                seconds * 100,
                peak.load(Ordering::Relaxed)
            );
        }
        Err(error) => println!("target={target:?} error: {error:#}"),
    }
}
