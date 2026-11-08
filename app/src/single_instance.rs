// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! One Godwit per user and config directory: opening it again brings up the
//! window that is already there instead of a second client signed in to the
//! same accounts (two would fight over sessions, caches and settings).
//!
//! Who runs is decided by an OS file lock (`File::try_lock`), which the
//! kernel drops when the process ends, even after a crash, so a stale lock
//! can't keep Godwit from starting. The running one listens on a local,
//! per-user channel; a later start writes `show` to it and exits.
//!
//! - Unix (Linux, macOS): a Unix domain socket next to the lock, in
//!   `$XDG_RUNTIME_DIR` (or the config directory). A socket file left by a
//!   crash is removed by the next one to hold the lock.
//! - Windows: a named pipe, local clients only; it goes with its process.
//! - Elsewhere: no channel; every start runs.
//!
//! Std plus `windows-sys` (already a dependency) cover this in a few lines
//! per OS; a crate such as `interprocess` would add its own dependencies for
//! the same two calls.
//!
//! The lock and channel names carry a hash of the config directory, so a
//! different `XDG_CONFIG_HOME` (another profile on disk) gets its own
//! instance.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Set (to anything) on a start that replaces the running Godwit (after an
/// update or an install): it waits for that one to quit instead of showing
/// it. Read once and cleared by [`start`].
pub const REPLACING: &str = "GODWIT_REPLACING";

/// How long a replacing start waits for the old one to let go.
const REPLACE_WAIT: Duration = Duration::from_secs(20);
/// How long a second start keeps trying to reach the first, which may hold
/// the lock but not be listening yet (it is starting too).
const SIGNAL_WAIT: Duration = Duration::from_secs(3);
const STEP: Duration = Duration::from_millis(100);
const SHOW: &[u8] = b"show\n";

/// Where one instance's lock and channel live.
#[derive(Clone, Debug)]
pub struct Place {
    lock: PathBuf,
    /// The Unix socket's path, or the Windows pipe's name.
    channel: PathBuf,
}

impl Place {
    /// For the config directory `config`, with the lock and socket in `dir`.
    pub fn new(config: &Path, dir: &Path) -> Self {
        let key = format!("godwit-{:016x}", fnv1a(config.as_os_str().as_encoded_bytes()));
        let channel = if cfg!(windows) {
            PathBuf::from(format!(r"\\.\pipe\{key}"))
        } else {
            dir.join(format!("{key}.sock"))
        };
        Self {
            lock: dir.join(format!("{key}.lock")),
            channel,
        }
    }

    /// This user's: the runtime directory (Linux) or else the config one.
    pub fn for_user() -> Option<Self> {
        let config = godwit_core::config_dir::get()?;
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|d| d.is_absolute() && d.is_dir())
            .unwrap_or_else(|| config.clone());
        Some(Self::new(&config, &dir))
    }
}

/// How starting went.
pub enum Start {
    /// This is the one: [`Primary::serve`] it once the window exists.
    Primary(Primary),
    /// Another one runs and was asked to show itself: quit.
    Shown,
}

/// The running instance: holds the lock for as long as it lives.
pub struct Primary {
    lock: Option<File>,
    listener: Option<sys::Listener>,
}

impl Primary {
    /// No lock or channel (single instance couldn't be set up, or isn't
    /// wanted, as in demo mode): runs anyway.
    pub fn unguarded() -> Self {
        Self {
            lock: None,
            listener: None,
        }
    }

    /// Listens on its own thread (blocked in accept, so idle costs nothing);
    /// `show` runs there for every later start.
    pub fn serve(self, show: impl Fn() + Send + 'static) {
        let lock = self.lock;
        let Some(listener) = self.listener else {
            // Not reachable, but still the one: keep the lock until exit, or
            // the next start would run beside this one.
            std::mem::forget(lock);
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("single-instance".into())
            .spawn(move || {
                let _lock = lock;
                listener.serve(&|mut conn: Box<dyn Read>| {
                    let mut buf = [0u8; 16];
                    let n = conn.read(&mut buf).unwrap_or(0);
                    if buf[..n].starts_with(b"show") {
                        show();
                    }
                });
            });
        if let Err(e) = spawned {
            eprintln!("error: single instance: {e}");
        }
    }
}

/// Becomes the running Godwit, or shows the one that already is. Called
/// first thing in `main`, before any other thread (it may clear
/// [`REPLACING`] from the environment).
pub fn start() -> Start {
    let replacing = std::env::var_os(REPLACING).is_some();
    if replacing {
        // SAFETY: called before any other thread starts (see above).
        unsafe { std::env::remove_var(REPLACING) };
    }
    let Some(place) = Place::for_user() else {
        return Start::Primary(Primary::unguarded());
    };
    match start_at(&place, if replacing { REPLACE_WAIT } else { Duration::ZERO }) {
        Ok(start) => start,
        Err(e) => {
            // Better two windows than none.
            eprintln!("error: single instance: {e}");
            Start::Primary(Primary::unguarded())
        }
    }
}

/// [`start`] at `place`: waits up to `wait` for a running one to quit.
pub fn start_at(place: &Place, wait: Duration) -> io::Result<Start> {
    if let Some(dir) = place.lock.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&place.lock)?;
    let deadline = Instant::now() + wait;
    loop {
        match lock.try_lock() {
            Ok(()) => {
                let listener = match sys::Listener::bind(&place.channel) {
                    Ok(l) => Some(l),
                    Err(e) => {
                        eprintln!("error: single instance: listening: {e}");
                        None
                    }
                };
                return Ok(Start::Primary(Primary {
                    lock: Some(lock),
                    listener,
                }));
            }
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => std::thread::sleep(STEP),
            Err(std::fs::TryLockError::WouldBlock) => break,
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
    }
    // Another one holds the lock, so it runs; it may still be starting.
    let deadline = Instant::now() + SIGNAL_WAIT;
    loop {
        match sys::signal(&place.channel, SHOW) {
            Ok(()) => return Ok(Start::Shown),
            Err(_) if Instant::now() < deadline => std::thread::sleep(STEP),
            Err(e) => {
                return Err(io::Error::other(format!(
                    "Godwit is already running, but didn't answer: {e}"
                )));
            }
        }
    }
}

/// FNV-1a: a hash that stays the same across builds (std's may not), so an
/// updated Godwit finds the channel of the one it replaces.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(unix)]
mod sys {
    use std::io::{self, Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::time::Duration;

    pub struct Listener(UnixListener);

    impl Listener {
        /// Only called while holding the lock, so a socket file already
        /// there is a crashed one's.
        pub fn bind(path: &Path) -> io::Result<Self> {
            match std::fs::remove_file(path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            let listener = UnixListener::bind(path)?;
            // The config-directory fallback isn't private like the runtime
            // directory: only this user may connect.
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self(listener))
        }

        pub fn serve(self, handle: &dyn Fn(Box<dyn Read>)) {
            for conn in self.0.incoming() {
                let Ok(conn) = conn else {
                    continue;
                };
                // A client that never writes mustn't block the next one.
                let _ = conn.set_read_timeout(Some(Duration::from_secs(1)));
                handle(Box::new(conn));
            }
        }
    }

    pub fn signal(path: &Path, msg: &[u8]) -> io::Result<()> {
        UnixStream::connect(path)?.write_all(msg)
    }
}

#[cfg(windows)]
mod sys {
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::Path;

    use windows_sys::Win32::Foundation::{ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};

    pub struct Listener(File);

    impl Listener {
        pub fn bind(name: &Path) -> io::Result<Self> {
            let wide: Vec<u16> = name.as_os_str().encode_wide().chain([0]).collect();
            // SAFETY: `wide` is NUL-terminated and outlives the call; no
            // security attributes (the default DACL: only this user writes).
            let handle = unsafe {
                CreateNamedPipeW(
                    wide.as_ptr(),
                    PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    0,
                    64,
                    0,
                    std::ptr::null(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: a valid handle just created, owned from here on.
            let owned = unsafe { OwnedHandle::from_raw_handle(handle) };
            Ok(Self(File::from(owned)))
        }

        pub fn serve(self, handle: &dyn Fn(Box<dyn Read>)) {
            let pipe = self.0.as_raw_handle();
            loop {
                // SAFETY: `pipe` stays open (owned by `self`) for the loop.
                let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) } != 0
                    || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
                if connected && let Ok(conn) = self.0.try_clone() {
                    handle(Box::new(conn));
                }
                // SAFETY: as above.
                if unsafe { DisconnectNamedPipe(pipe) } == 0 {
                    eprintln!("error: single instance: {}", io::Error::last_os_error());
                    return;
                }
            }
        }
    }

    pub fn signal(name: &Path, msg: &[u8]) -> io::Result<()> {
        let mut pipe = File::options().write(true).open(name)?;
        // Windows only lets the foreground process hand the foreground on;
        // this start is it (the user just opened it), the running one isn't.
        // SAFETY: takes no pointers.
        unsafe { AllowSetForegroundWindow(ASFW_ANY) };
        pipe.write_all(msg)
    }
}

#[cfg(not(any(unix, windows)))]
mod sys {
    use std::io::{self, Read};
    use std::path::Path;

    pub struct Listener;

    impl Listener {
        pub fn bind(_: &Path) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no local channel on this platform",
            ))
        }

        pub fn serve(self, _: &dyn Fn(Box<dyn Read>)) {}
    }

    pub fn signal(_: &Path, _: &[u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no local channel on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A fresh directory, removed again when the test ends.
    struct Temp(PathBuf);

    impl std::ops::Deref for Temp {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp(name: &str) -> Temp {
        let dir = std::env::temp_dir().join(format!("godwit-single-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Temp(dir)
    }

    fn primary(start: Start) -> Primary {
        match start {
            Start::Primary(p) => p,
            Start::Shown => panic!("expected to be the primary"),
        }
    }

    #[test]
    fn second_start_shows_the_first() {
        let dir = temp("second");
        let place = Place::new(&dir.join("config"), &dir);
        let first = primary(start_at(&place, Duration::ZERO).unwrap());
        let (tx, rx) = mpsc::channel();
        first.serve(move || tx.send(()).unwrap());
        assert!(matches!(start_at(&place, Duration::ZERO).unwrap(), Start::Shown));
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(start_at(&place, Duration::ZERO).unwrap(), Start::Shown));
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn other_config_dirs_run_apart() {
        let dir = temp("apart");
        let a = Place::new(&dir.join("a"), &dir);
        let b = Place::new(&dir.join("b"), &dir);
        let _a = primary(start_at(&a, Duration::ZERO).unwrap());
        let _b = primary(start_at(&b, Duration::ZERO).unwrap());
    }

    #[test]
    fn stale_socket_is_replaced() {
        let dir = temp("stale");
        let place = Place::new(&dir.join("config"), &dir);
        // A crashed instance: its socket file is still there, nobody listens.
        {
            let gone = primary(start_at(&place, Duration::ZERO).unwrap());
            drop(gone);
        }
        #[cfg(unix)]
        assert!(place.channel.exists());
        let again = primary(start_at(&place, Duration::ZERO).unwrap());
        let (tx, rx) = mpsc::channel();
        again.serve(move || tx.send(()).unwrap());
        assert!(matches!(start_at(&place, Duration::ZERO).unwrap(), Start::Shown));
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn replacing_waits_for_the_old_one() {
        let dir = temp("replace");
        let place = Place::new(&dir.join("config"), &dir);
        let old = primary(start_at(&place, Duration::ZERO).unwrap());
        let quitter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(old);
        });
        let new = start_at(&place, Duration::from_secs(5)).unwrap();
        assert!(matches!(new, Start::Primary(Primary { listener: Some(_), .. })));
        quitter.join().unwrap();
    }

    #[test]
    fn locked_but_silent_is_an_error() {
        let dir = temp("silent");
        let place = Place::new(&dir.join("config"), &dir);
        let holder = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&place.lock)
            .unwrap();
        holder.try_lock().unwrap();
        assert!(start_at(&place, Duration::ZERO).is_err());
    }

    #[test]
    fn hash_is_stable() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
