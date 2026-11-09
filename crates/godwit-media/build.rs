// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Compiles the H264 shim (native/h264.c) against x264, whose headers
//! vendor/x264-src hands over as `DEP_X264_INCLUDE`.

fn main() {
    println!("cargo:rerun-if-changed=native/av1.c");
    let include = webrtc_sys_build::webrtc_dir().join("include");
    cc::Build::new()
        .file("native/av1.c")
        .include(include.join("third_party/dav1d/libdav1d/include"))
        .warnings(true)
        .compile("godwit_av1");
    println!("cargo:rerun-if-changed=native/h264.c");
    let mut build = cc::Build::new();
    build.file("native/h264.c").warnings(false);
    if let Some(include) = std::env::var_os("DEP_X264_INCLUDE") {
        build.include(include);
    }
    build.compile("godwit_h264");
}
