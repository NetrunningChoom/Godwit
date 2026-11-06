// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Windows-only start-up: the console, and the identity Windows shows Godwit's
//! notifications under. Nothing here runs elsewhere.

use std::path::Path;

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegSetValueExW,
};
use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

/// The AppUserModelID Godwit's toasts and taskbar button go under. Windows
/// names the notification after the registry entry [`register_app_id`] writes
/// for it; without one, notify-rust falls back to PowerShell's id and the
/// toast says "Windows PowerShell".
pub const APP_ID: &str = "Godwit.Godwit";

/// Godwit is a GUI-subsystem program (no console window opens beside it from
/// Explorer). Started from a terminal, this attaches to that terminal's
/// console so `--verbose` and errors still show there.
pub fn attach_console() {
    // SAFETY: neither call takes a pointer; called first thing in `main`,
    // before any other thread starts.
    unsafe {
        let err = GetStdHandle(STD_ERROR_HANDLE);
        if err.is_null() || err == INVALID_HANDLE_VALUE {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

/// Sets this process's AppUserModelID and registers it for the current user
/// (`HKCU\Software\Classes\AppUserModelId\Godwit.Godwit`) with Godwit's name
/// and icon, so an unpackaged Godwit can show toasts under its own name.
/// Failures only cost the toast's name and icon, so they're just reported.
pub fn register_app_id() {
    let id = wide(APP_ID);
    // SAFETY: `id` is NUL-terminated and outlives the call.
    let hr = unsafe { SetCurrentProcessExplicitAppUserModelID(id.as_ptr()) };
    if hr < 0 {
        eprintln!("error: setting the app id: HRESULT {hr:#x}");
    }
    let icon = godwit_core::config_dir::get().and_then(|dir| write_icon(&dir));
    let key = format!(r"Software\Classes\AppUserModelId\{APP_ID}");
    let mut values = vec![("DisplayName", "Godwit".to_owned())];
    if let Some(icon) = icon {
        values.push(("IconUri", icon));
    }
    if let Err(code) = set_user_values(&key, &values) {
        eprintln!("error: registering for notifications: Win32 error {code}");
    }
}

/// The 256 px icon as a PNG in Godwit's config folder (the toast's icon
/// needs a file), written when missing or changed.
fn write_icon(dir: &Path) -> Option<String> {
    const PNG: &[u8] = include_bytes!("../../assets/icon/icon-256.png");
    let path = dir.join("icon-256.png");
    if std::fs::read(&path).ok().as_deref() != Some(PNG) {
        let written = std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&path, PNG));
        if let Err(e) = written {
            eprintln!("error: writing the notification icon: {e}");
            return None;
        }
    }
    path.to_str().map(str::to_owned)
}

/// Creates `HKCU\<key>` if needed and sets its string values.
fn set_user_values(key: &str, values: &[(&str, String)]) -> Result<(), u32> {
    let key = wide(key);
    let mut handle: HKEY = std::ptr::null_mut();
    // SAFETY: `key` is NUL-terminated; `handle` receives the opened key and
    // is closed below; the optional pointers are null as documented.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut handle,
            std::ptr::null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(status);
    }
    let mut result = Ok(());
    for (name, value) in values {
        let name = wide(name);
        let data = wide(value);
        let bytes = u32::try_from(data.len() * 2).unwrap_or(u32::MAX);
        // SAFETY: `handle` is open; `name` and `data` are NUL-terminated
        // UTF-16, and `bytes` is `data`'s size including the NUL, as REG_SZ wants.
        let status = unsafe { RegSetValueExW(handle, name.as_ptr(), 0, REG_SZ, data.as_ptr().cast(), bytes) };
        if status != ERROR_SUCCESS {
            result = Err(status);
        }
    }
    // SAFETY: `handle` was opened above and isn't used after this.
    unsafe { RegCloseKey(handle) };
    result
}

/// `s` as NUL-terminated UTF-16.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
