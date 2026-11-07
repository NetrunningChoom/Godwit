// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Holds the desktop idle inhibitor while call video is played or captured.

use std::sync::mpsc::{self, Sender};

pub struct MediaAwake {
    active: bool,
    changes: Option<Sender<bool>>,
}

impl MediaAwake {
    pub fn new() -> Self {
        #[cfg(target_os = "linux")]
        {
            let (tx, rx) = mpsc::channel();
            let _ = std::thread::Builder::new()
                .name("media-idle-inhibitor".into())
                .spawn(move || {
                    let mut held: Option<Held> = None;
                    while let Ok(active) = rx.recv() {
                        if active && held.is_none() {
                            held = inhibit();
                        } else if !active {
                            release(&mut held);
                        }
                    }
                    release(&mut held);
                });
            Self {
                active: false,
                changes: Some(tx),
            }
        }
        #[cfg(not(target_os = "linux"))]
        Self {
            active: false,
            changes: None,
        }
    }

    pub fn set_active(&mut self, active: bool) {
        if self.active == active {
            return;
        }
        self.active = active;
        if let Some(changes) = &self.changes {
            let _ = changes.send(active);
        }
    }
}

#[cfg(target_os = "linux")]
struct Held {
    screen: Option<(zbus::blocking::Connection, u32)>,
    // Keeping logind's returned descriptor open blocks idle and suspend actions.
    _idle_sleep: Option<zbus::zvariant::OwnedFd>,
}

#[cfg(target_os = "linux")]
fn inhibit() -> Option<Held> {
    let screen = zbus::blocking::Connection::session().ok().and_then(|bus| {
        let reply = bus
            .call_method(
                Some("org.freedesktop.ScreenSaver"),
                "/org/freedesktop/ScreenSaver",
                Some("org.freedesktop.ScreenSaver"),
                "Inhibit",
                &("Godwit", "Watching or sharing live video"),
            )
            .ok()?;
        let cookie = reply.body().deserialize::<u32>().ok()?;
        Some((bus, cookie))
    });
    let idle_sleep = zbus::blocking::Connection::system().ok().and_then(|bus| {
        bus.call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "Inhibit",
            &("idle:sleep", "Godwit", "Watching or sharing live video", "block"),
        )
        .ok()?
        .body()
        .deserialize::<zbus::zvariant::OwnedFd>()
        .ok()
    });
    (screen.is_some() || idle_sleep.is_some()).then_some(Held {
        screen,
        _idle_sleep: idle_sleep,
    })
}

#[cfg(target_os = "linux")]
fn release(held: &mut Option<Held>) {
    if let Some(held) = held.take()
        && let Some((bus, cookie)) = held.screen
    {
        let _ = bus.call_method(
            Some("org.freedesktop.ScreenSaver"),
            "/org/freedesktop/ScreenSaver",
            Some("org.freedesktop.ScreenSaver"),
            "UnInhibit",
            &(cookie,),
        );
    }
}
