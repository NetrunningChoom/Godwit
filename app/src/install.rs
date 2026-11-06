// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Adding Godwit to the system's apps, from the one downloaded file.
//!
//! A release build started from anywhere else (Downloads, say) offers once
//! to add itself: it copies itself to the per-user apps folder and makes
//! itself startable like any other app, then runs from there (where it also
//! updates itself, see update.rs). "Not now" is remembered; Settings → About
//! can add or remove it any time. Nothing needs admin rights.
//!
//! - Linux: `~/.local/bin/godwit`, an app-menu entry and its icons in
//!   `~/.local/share` (`$XDG_DATA_HOME`), as packaging/linux/install-user.sh.
//! - Windows: `%LOCALAPPDATA%\Programs\Godwit\godwit.exe`, with Start menu
//!   and desktop shortcuts.
//!
//! Removing takes those away again; settings and sign-ins stay.

use std::io;
use std::path::{Path, PathBuf};

const DESKTOP: &str = include_str!("../../packaging/linux/godwit.desktop");
const ICONS: [(u32, &[u8]); 6] = [
    (16, include_bytes!("../../assets/icon/icon-16.png")),
    (32, include_bytes!("../../assets/icon/icon-32.png")),
    (64, include_bytes!("../../assets/icon/icon-64.png")),
    (128, include_bytes!("../../assets/icon/icon-128.png")),
    (256, include_bytes!("../../assets/icon/icon-256.png")),
    (512, include_bytes!("../../assets/icon/icon-512.png")),
];

/// Where the added copy lives, on platforms that support adding.
pub fn target() -> Option<PathBuf> {
    let dirs = directories::BaseDirs::new()?;
    if cfg!(target_os = "linux") {
        Some(dirs.home_dir().join(".local/bin/godwit"))
    } else if cfg!(windows) {
        Some(dirs.data_local_dir().join("Programs").join("Godwit").join("godwit.exe"))
    } else {
        None
    }
}

/// Whether this copy is the added one (or adding isn't supported here).
pub fn running_installed() -> bool {
    let (Some(target), Ok(exe)) = (target(), std::env::current_exe()) else {
        return true;
    };
    same(&exe, &target)
}

/// Whether an added copy exists (this one or another).
pub fn installed() -> bool {
    target().is_some_and(|t| t.is_file())
}

fn same(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn declined_flag() -> Option<PathBuf> {
    godwit_core::config_dir::get().map(|d| d.join("install-declined"))
}

/// Whether to ask at start: a release build, not running as the added copy,
/// none added yet, and not declined before.
pub fn offer() -> bool {
    !cfg!(debug_assertions)
        && target().is_some()
        && !running_installed()
        && !installed()
        && !declined_flag().is_some_and(|f| f.exists())
}

/// Remembers "Not now" (Settings can still add it).
pub fn decline() {
    if let Some(flag) = declined_flag() {
        if let Some(dir) = flag.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(flag, b"");
    }
}

/// Copies this binary into place and makes it startable; returns the added
/// copy's path (the caller starts it and quits).
pub fn install() -> io::Result<PathBuf> {
    let target = target().ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "not supported here"))?;
    let exe = std::env::current_exe()?;
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(|e| step(e, "making", dir))?;
    }
    if !same(&exe, &target) {
        let part = target.with_extension("part");
        // The running program itself: on Linux /proc/self/exe stays readable
        // even when the file it started from was moved or deleted (a browser
        // running a download from its temporary folder, say), when
        // current_exe() names a path that's gone (" (deleted)").
        let source = if cfg!(target_os = "linux") {
            PathBuf::from("/proc/self/exe")
        } else {
            exe.clone()
        };
        std::fs::copy(&source, &part).map_err(|e| step(e, "copying Godwit to", &part))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&part, &target).map_err(|e| step(e, "moving Godwit to", &target))?;
    }
    if cfg!(target_os = "linux") {
        linux_entries(&target)?;
    } else if cfg!(windows) {
        windows_shortcuts(&target, true)?;
    }
    Ok(target)
}

/// An error that says which step and which path failed.
fn step(e: io::Error, doing: &str, path: &Path) -> io::Error {
    io::Error::new(e.kind(), format!("{doing} {}: {e}", path.display()))
}

/// Removes the menu entries and the added copy. Settings and sign-ins stay.
pub fn uninstall() -> io::Result<()> {
    let Some(target) = target() else {
        return Ok(());
    };
    if cfg!(target_os = "linux") {
        let data = data_home();
        let _ = std::fs::remove_file(data.join("applications/godwit.desktop"));
        for (size, _) in ICONS {
            let _ = std::fs::remove_file(icon_path(&data, size));
        }
        refresh_linux(&data);
        // A running binary can be unlinked on Linux.
        match std::fs::remove_file(&target) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    } else if cfg!(windows) {
        windows_shortcuts(&target, false)?;
        // A running .exe can't be deleted: a short-lived cmd deletes just
        // godwit.exe once this one has quit, then the folder only if that
        // left it empty (rmdir without /s), so nothing else in it is touched.
        if let Some(dir) = target.parent() {
            let script = format!(
                "ping -n 4 127.0.0.1 >nul & del /f /q \"{}\" & rmdir \"{}\"",
                target.display(),
                dir.display()
            );
            hidden("cmd", &["/c", &script]).spawn()?;
        }
    }
    if let Some(flag) = declined_flag() {
        let _ = std::fs::remove_file(flag);
    }
    Ok(())
}

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| directories::BaseDirs::new().map(|d| d.home_dir().join(".local/share")))
        .unwrap_or_default()
}

fn icon_path(data: &Path, size: u32) -> PathBuf {
    data.join(format!("icons/hicolor/{size}x{size}/apps/godwit.png"))
}

fn linux_entries(target: &Path) -> io::Result<()> {
    let data = data_home();
    for (size, png) in ICONS {
        let path = icon_path(&data, size);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, png).map_err(|e| step(e, "writing", &path))?;
    }
    let apps = data.join("applications");
    std::fs::create_dir_all(&apps).map_err(|e| step(e, "making", &apps))?;
    let entry = DESKTOP
        .lines()
        .map(|l| {
            if l.starts_with("Exec=") {
                format!("Exec=\"{}\"", target.display())
            } else {
                l.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let desktop = apps.join("godwit.desktop");
    std::fs::write(&desktop, entry + "\n").map_err(|e| step(e, "writing", &desktop))?;
    refresh_linux(&data);
    Ok(())
}

/// Desktops pick up new entries and icons faster with these (if present).
fn refresh_linux(data: &Path) {
    let quiet = |cmd: &str, args: &[&std::ffi::OsStr]| {
        let _ = std::process::Command::new(cmd)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    };
    quiet(
        "gtk-update-icon-cache",
        &["-q".as_ref(), "-t".as_ref(), data.join("icons/hicolor").as_os_str()],
    );
    quiet(
        "update-desktop-database",
        &["-q".as_ref(), data.join("applications").as_os_str()],
    );
}

/// A child process without a console window (Windows).
fn hidden(program: &str, args: &[&str]) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Start menu and desktop shortcuts, made (or removed) through Windows' own
/// shell object via PowerShell, which every Windows 10/11 has.
fn windows_shortcuts(target: &Path, make: bool) -> io::Result<()> {
    let quote = |p: &Path| p.display().to_string().replace('\'', "''");
    let target = quote(target);
    let script = if make {
        format!(
            "$w = New-Object -ComObject WScript.Shell; \
             foreach ($d in @([Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('Desktop'))) {{ \
               $s = $w.CreateShortcut((Join-Path $d 'Godwit.lnk')); $s.TargetPath = '{target}'; \
               $s.WorkingDirectory = (Split-Path '{target}'); $s.Save() }}"
        )
    } else {
        "foreach ($d in @([Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('Desktop'))) { \
           Remove-Item -ErrorAction SilentlyContinue (Join-Path $d 'Godwit.lnk') }"
            .to_owned()
    };
    let status = hidden("powershell", &["-NoProfile", "-NonInteractive", "-Command", &script]).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("PowerShell couldn't make the shortcuts"))
    }
}
