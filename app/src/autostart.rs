// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Start Godwit when the user signs in to their computer. One thin adapter
//! per OS: an XDG autostart entry on Linux (freedesktop Desktop Entry and
//! Autostart specs), a LaunchAgent on macOS, a `Run` registry value on
//! Windows. The file contents are pure functions; only [`apply`] touches
//! the system. Removing only ever deletes the one file Godwit wrote.

use std::path::{Path, PathBuf};

/// The launch flag that shows the window minimized (see `main`).
pub const MINIMIZED_FLAG: &str = "--minimized";

/// Marks a file as Godwit's own, so a removal never deletes someone else's.
const MARKER: &str = "X-Godwit-Autostart=true";
/// The LaunchAgent's label (reverse-DNS, as launchd expects).
#[cfg(any(target_os = "macos", test))]
const LABEL: &str = "org.godwit.Godwit";

/// Makes the system match the settings: an entry that starts Godwit at
/// sign-in when `enabled`, none otherwise. Errors are logged, never fatal.
pub fn apply(enabled: bool, minimized: bool) {
    if let Err(e) = apply_inner(enabled, minimized) {
        eprintln!("error: updating start at sign-in: {e}");
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn apply_inner(enabled: bool, minimized: bool) -> std::io::Result<()> {
    let Some(path) = entry_path() else {
        return Ok(());
    };
    if enabled {
        let exe = std::env::current_exe()?;
        write_entry(&path, &desktop_entry(&exe, minimized))
    } else {
        remove_entry(&path)
    }
}

#[cfg(target_os = "macos")]
fn apply_inner(enabled: bool, minimized: bool) -> std::io::Result<()> {
    let Some(path) = entry_path() else {
        return Ok(());
    };
    if enabled {
        let exe = std::env::current_exe()?;
        write_entry(&path, &launch_agent(&exe, minimized))
    } else {
        remove_entry(&path)
    }
}

#[cfg(windows)]
fn apply_inner(enabled: bool, minimized: bool) -> std::io::Result<()> {
    if enabled {
        let exe = std::env::current_exe()?;
        crate::win32::set_run_value("Godwit", &run_command(&exe, minimized)).map_err(win_error)
    } else {
        crate::win32::delete_run_value("Godwit").map_err(win_error)
    }
}

/// A Win32 status as an I/O error (statuses are small positive codes).
#[cfg(windows)]
fn win_error(status: u32) -> std::io::Error {
    std::io::Error::from_raw_os_error(i32::try_from(status).unwrap_or(i32::MAX))
}

/// Platforms with no startup mechanism here: nothing to do.
#[cfg(not(any(unix, windows)))]
fn apply_inner(_enabled: bool, _minimized: bool) -> std::io::Result<()> {
    Ok(())
}

/// Where this OS keeps Godwit's startup file.
#[cfg(all(unix, not(target_os = "macos")))]
fn entry_path() -> Option<PathBuf> {
    // XDG_CONFIG_HOME, else ~/.config (`directories` follows the spec).
    let dirs = directories::BaseDirs::new()?;
    Some(dirs.config_dir().join("autostart/godwit.desktop"))
}

#[cfg(target_os = "macos")]
fn entry_path() -> Option<PathBuf> {
    let dirs = directories::BaseDirs::new()?;
    Some(dirs.home_dir().join(format!("Library/LaunchAgents/{LABEL}.plist")))
}

/// Writes the startup file, creating its folder.
#[cfg(unix)]
fn write_entry(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, contents)
}

/// Deletes the startup file when it is a regular file (not a link) that
/// Godwit wrote; anything else is left alone. A missing file is fine.
#[cfg(any(unix, test))]
fn remove_entry(path: &Path) -> std::io::Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !meta.is_file() {
        return Err(std::io::Error::other("not a regular file; left alone"));
    }
    // A few KB at most; refuse to read anything big.
    if meta.len() > 64 * 1024 || !std::fs::read_to_string(path)?.contains(MARKER) {
        return Err(std::io::Error::other("not written by Godwit; left alone"));
    }
    std::fs::remove_file(path)
}

/// An XDG autostart entry, based on `packaging/linux/godwit.desktop`.
#[cfg(any(unix, test))]
fn desktop_entry(exe: &Path, minimized: bool) -> String {
    let mut exec = exec_quote(&exe.to_string_lossy());
    if minimized {
        exec.push(' ');
        exec.push_str(MINIMIZED_FLAG);
    }
    format!(
        "[Desktop Entry]\nType=Application\nName=Godwit\nComment=One client for your chat networks\n\
         Exec={exec}\nIcon=godwit\nTerminal=false\nX-GNOME-Autostart-enabled=true\n{MARKER}\n"
    )
}

/// One argument of a Desktop Entry `Exec` line: double-quoted, with the
/// characters the spec reserves backslash-escaped, and `%` doubled.
#[cfg(any(unix, test))]
fn exec_quote(arg: &str) -> String {
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A macOS LaunchAgent that runs Godwit at login.
#[cfg(any(target_os = "macos", test))]
fn launch_agent(exe: &Path, minimized: bool) -> String {
    let mut args = format!("    <string>{}</string>\n", xml_escape(&exe.to_string_lossy()));
    if minimized {
        args.push_str(&format!("    <string>{MINIMIZED_FLAG}</string>\n"));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{LABEL}</string>\n\
         \x20 <key>ProgramArguments</key>\n  <array>\n{args}  </array>\n\
         \x20 <key>RunAtLoad</key>\n  <true/>\n  <!-- {MARKER} -->\n</dict>\n</plist>\n"
    )
}

#[cfg(any(target_os = "macos", test))]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The command line the Windows `Run` value holds.
#[cfg(any(windows, test))]
fn run_command(exe: &Path, minimized: bool) -> String {
    let mut cmd = format!("\"{}\"", exe.to_string_lossy());
    if minimized {
        cmd.push(' ');
        cmd.push_str(MINIMIZED_FLAG);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_names_the_executable() {
        let e = desktop_entry(Path::new("/opt/my apps/godwit"), true);
        assert!(e.contains("Exec=\"/opt/my apps/godwit\" --minimized\n"));
        assert!(!desktop_entry(Path::new("/usr/bin/godwit"), false).contains("--minimized"));
        assert!(e.contains(MARKER));
        assert_eq!(exec_quote("a$b%c"), "\"a\\$b%%c\"");
    }

    #[test]
    fn other_formats() {
        let p = launch_agent(Path::new("/Applications/G&G/godwit"), true);
        assert!(p.contains("G&amp;G") && p.contains("--minimized") && p.contains(LABEL));
        assert_eq!(
            run_command(Path::new(r"C:\G\godwit.exe"), true),
            "\"C:\\G\\godwit.exe\" --minimized"
        );
    }

    #[cfg(unix)]
    #[test]
    fn writes_and_removes_only_its_own_file() {
        let dir = std::env::temp_dir().join(format!("godwit-autostart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("autostart/godwit.desktop");
        write_entry(&path, &desktop_entry(Path::new("/usr/bin/godwit"), false)).unwrap();
        remove_entry(&path).unwrap();
        assert!(!path.exists());
        remove_entry(&path).unwrap();

        // Someone else's file with the same name stays.
        std::fs::write(&path, "[Desktop Entry]\nName=Other\n").unwrap();
        assert!(remove_entry(&path).is_err() && path.exists());

        // A link is never followed or deleted, even to a file with the marker.
        std::fs::remove_file(&path).unwrap();
        let target = dir.join("target");
        std::fs::write(&target, desktop_entry(Path::new("/usr/bin/godwit"), false)).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(remove_entry(&path).is_err());
        assert!(path.symlink_metadata().is_ok() && target.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
