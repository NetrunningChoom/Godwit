// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
// libwebrtc's screen capture on Linux talks to the desktop portal over D-Bus
// through GIO, but webrtc-sys doesn't link GLib itself. (GLib is already a
// Linux build prerequisite, see docs/development.md.) Linked from here, the
// crate that brings in libwebrtc, so every binary using it gets the libraries.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        for lib in ["gio-2.0", "gobject-2.0", "glib-2.0"] {
            println!("cargo:rustc-link-lib=dylib={lib}");
        }
    }
}
