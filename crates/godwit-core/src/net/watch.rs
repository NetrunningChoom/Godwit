// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Whether this device is on a network, and when that changes (Wi-Fi to
//! cable, a new address, a VPN coming up). Connections use it to wait for the
//! network instead of burning retries while it's gone, and to check
//! themselves at once after a change instead of waiting for a timeout.
//!
//! It reads the device's own interface addresses every couple of seconds
//! (a local system call, nothing goes on the network), through `if-addrs`,
//! which covers Linux, Android, macOS and Windows.

use std::net::IpAddr;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::sync::watch;

/// How often the interfaces are read.
const EVERY: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    /// An address other than loopback or link-local is up.
    pub online: bool,
    /// Goes up each time the set of addresses changes.
    pub generation: u64,
}

fn routable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local() && !v4.is_unspecified(),
        IpAddr::V6(v6) => {
            // fe80::/10 is link-local.
            !v6.is_loopback() && !v6.is_unspecified() && (v6.segments()[0] & 0xffc0) != 0xfe80
        }
    }
}

/// The routable addresses, sorted; None when they can't be read.
fn addresses() -> Option<Vec<(String, IpAddr)>> {
    let mut list: Vec<(String, IpAddr)> = if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .map(|i| {
            let ip = i.ip();
            (i.name, ip)
        })
        .filter(|(_, ip)| routable(ip))
        .collect();
    list.sort();
    Some(list)
}

fn sender() -> &'static watch::Sender<State> {
    static TX: OnceLock<watch::Sender<State>> = OnceLock::new();
    TX.get_or_init(|| {
        let first = addresses();
        let (tx, _) = watch::channel(State {
            // Can't tell: assume online, so nothing waits forever.
            online: first.as_ref().is_none_or(|a| !a.is_empty()),
            generation: 0,
        });
        let watcher = tx.clone();
        let spawned = std::thread::Builder::new()
            .name("godwit-netwatch".into())
            .spawn(move || run(&watcher, first));
        if let Err(e) = spawned {
            eprintln!("error: network watcher: {e}");
        }
        tx
    })
}

fn run(tx: &watch::Sender<State>, mut last: Option<Vec<(String, IpAddr)>>) {
    loop {
        std::thread::sleep(EVERY);
        let now = addresses();
        if now.is_none() || now == last {
            continue;
        }
        let online = now.as_ref().is_none_or(|a| !a.is_empty());
        last = now;
        tx.send_modify(|s| {
            let was = s.online;
            s.online = online;
            s.generation += 1;
            super::log::connection(
                "device",
                "network",
                match (was, online) {
                    (true, false) => "offline",
                    (false, true) => "online",
                    _ => "changed",
                },
            );
        });
    }
}

/// The current state.
pub fn state() -> State {
    *sender().borrow()
}

/// Whether the device is on a network now.
pub fn online() -> bool {
    state().online
}

/// A receiver for network changes (`changed().await` wakes on each one).
pub fn subscribe() -> watch::Receiver<State> {
    sender().subscribe()
}

/// Waits until the device is on a network (at once when it is). Returns
/// whether it had to wait.
pub async fn until_online() -> bool {
    let mut rx = subscribe();
    if rx.borrow_and_update().online {
        return false;
    }
    while rx.changed().await.is_ok() {
        if rx.borrow_and_update().online {
            return true;
        }
    }
    false
}

/// Sleeps for `wait`, cut short when the network changes (a reconnect then
/// goes ahead at once). Returns whether it was cut short.
pub async fn sleep_or_change(wait: Duration) -> bool {
    let mut rx = subscribe();
    rx.borrow_and_update();
    tokio::select! {
        () = tokio::time::sleep(wait) => false,
        changed = rx.changed() => changed.is_ok(),
    }
}

/// The pause before a reconnect of `network`'s `what` (`gateway`,
/// `voice`, `push`…): without a network, until it's back (and then none, so
/// the reconnect goes ahead at once); otherwise `wait`, cut short by a
/// network change. Returns whether the network was gone, so a caller can
/// leave that try out of its count.
pub async fn reconnect_pause(network: &str, what: &str, wait: Duration) -> bool {
    if !online() {
        super::log::connection(network, what, "waiting for the network");
    }
    if until_online().await {
        super::log::connection(network, what, "network back, reconnecting");
        return true;
    }
    super::log::connection(network, what, &format!("reconnecting in {:.1}s", wait.as_secs_f32()));
    if sleep_or_change(wait).await {
        super::log::connection(network, what, "network changed, reconnecting now");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_and_link_local_dont_count() {
        assert!(!routable(&"127.0.0.1".parse().unwrap()));
        assert!(!routable(&"169.254.3.4".parse().unwrap()));
        assert!(!routable(&"::1".parse().unwrap()));
        assert!(!routable(&"fe80::1".parse().unwrap()));
        assert!(routable(&"198.51.100.20".parse().unwrap()));
        assert!(routable(&"2001:db8::1".parse().unwrap()));
    }
}
