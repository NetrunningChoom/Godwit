# x264 in Godwit

Copied unchanged from Yapfast's `vendor/x264-src` (commit 5d3d4d0,
2026-11-03), which holds x264 at commit 373697b (core 164) under
GPL-2.0-or-later (`x264/COPYING`; see `README.yapfast.md`). Godwit uses it for
Discord Go Live's H264 (`crates/godwit-media/src/h264.rs`), because libwebrtc's
OpenH264 drops frames under motion.

Building it needs nasm on x86-64 (`$NASM`, or `nasm` on the PATH): Fedora
`dnf install nasm`, Debian/Ubuntu `apt install nasm`, Windows the nasm
installer or `winget install nasm`. Other processors build x264's plain C.
`build.rs` writes the configuration for Linux and Windows only; macOS needs
its own entry there before Godwit builds on it.
