// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Signs in to Google Chat through the player helper's window and keeps the
//! sign-in in the OS keystore (slot 0), for developing the network before
//! the app's login page knows it: `cargo run -p godwit-gchat --example
//! sign_in -- path/to/godwit-player`. Prints only the kept cookies' names.

use std::io::BufRead;
use std::process::{Command, Stdio};

fn main() {
    let player = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/debug/godwit-player".into());
    let mut child = match Command::new(&player)
        .arg("--gchat-login")
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return eprintln!("error: starting {player}: {e}"),
    };
    let mut line = String::new();
    if let Some(out) = child.stdout.take() {
        let _ = std::io::BufReader::new(out).read_line(&mut line);
    }
    let status = child.wait().map(|s| s.code());
    let Some(saved) = godwit_gchat::session::Saved::from_line(&line) else {
        return eprintln!("not signed in (helper exit: {status:?})");
    };
    godwit_gchat::session::save(0, &saved);
    let names: Vec<&str> = saved.cookies.iter().map(|(n, _)| n.as_str()).collect();
    println!("kept in slot 0: {}", names.join(", "));
}
