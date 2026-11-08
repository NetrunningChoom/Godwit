// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors

fn main() {
    identical_code_folding();
    commit();
    embedded_player();
    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") {
        windows_icon();
    }
}

/// The commit this is built from, for About: `GODWIT_COMMIT` when set (the
/// release build), else `git rev-parse` here; empty outside a checkout.
fn commit() {
    println!("cargo:rerun-if-env-changed=GODWIT_COMMIT");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_owned())
    };
    let sha = std::env::var("GODWIT_COMMIT")
        .ok()
        .or_else(|| git(&["rev-parse", "--short=10", "HEAD"]))
        .unwrap_or_default();
    // Rebuilt when HEAD moves (a new commit or checkout).
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        println!("cargo:rerun-if-changed={dir}/refs/heads");
    }
    println!("cargo:rustc-env=GODWIT_COMMIT={sha}");
}

/// The player helper built into this binary, for one-file releases:
/// `GODWIT_EMBED_PLAYER` names the built `godwit-player`, and
/// `inline_player::player_executable` unpacks it on first use. Without it
/// the helper is looked for beside the binary, as in development.
fn embedded_player() {
    println!("cargo:rerun-if-env-changed=GODWIT_EMBED_PLAYER");
    let bytes = match std::env::var_os("GODWIT_EMBED_PLAYER") {
        Some(path) => {
            println!("cargo:rerun-if-changed={}", std::path::Path::new(&path).display());
            std::fs::read(&path).unwrap_or_else(|e| panic!("GODWIT_EMBED_PLAYER: {e}"))
        }
        None => Vec::new(),
    };
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default()).join("player.bin");
    std::fs::write(out, bytes).unwrap_or_else(|e| panic!("writing the embedded player: {e}"));
}

/// Lets the linker fold identical machine code into one copy, across Rust and
/// libwebrtc's C++ (docs/measurements.md, Binary size). Only mold and lld
/// have `--icf`; GNU ld would reject it, so it's added only when the
/// rustflags pick one of them (`-C link-arg=-fuse-ld=mold` in a cargo config).
/// Windows' linker already folds identical code in release builds.
fn identical_code_folding() {
    let linux = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "linux");
    let flags = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
    let folding_linker = flags
        .split('\x1f')
        .any(|flag| flag.contains("fuse-ld=mold") || flag.contains("fuse-ld=lld"));
    if linux && folding_linker {
        println!("cargo:rustc-link-arg-bins=-Wl,--icf=all");
    }
}

/// Gives godwit.exe its icon (Explorer, the taskbar, shortcuts): compiles
/// `packaging/windows/godwit.rc` with LLVM's `llvm-rc`, else the Windows SDK's
/// `rc.exe` (as Yapfast's build.rs does), and links the result. Without either
/// the build goes on with a warning: the window still gets its icon from Slint.
fn windows_icon() {
    use std::path::PathBuf;
    use std::process::Command;
    println!("cargo:rerun-if-changed=../packaging/windows");
    let Some(out) = std::env::var_os("OUT_DIR") else { return };
    let res = PathBuf::from(out).join("godwit.res");
    let dir = "../packaging/windows";
    let llvm = Command::new("llvm-rc")
        .current_dir(dir)
        .args(["-no-preprocess", "-FO"])
        .arg(&res)
        .arg("godwit.rc")
        .status();
    let ok = match llvm {
        Ok(status) => status.success(),
        Err(_) => windows_sdk_rc().is_some_and(|rc| {
            Command::new(rc)
                .current_dir(dir)
                .arg("/fo")
                .arg(&res)
                .arg("godwit.rc")
                .status()
                .is_ok_and(|s| s.success())
        }),
    };
    if ok {
        // The MSVC linker takes compiled resources as an input.
        println!("cargo:rustc-link-arg-bin=godwit={}", res.display());
    } else {
        println!("cargo:warning=no resource compiler (llvm-rc or the Windows SDK's rc.exe): godwit.exe gets no icon");
    }
}

/// The newest Windows SDK's x64 `rc.exe`. The SDK isn't one of vswhere's
/// products, and `WindowsSdkDir` is only set in a developer prompt, so look
/// where its installer puts it.
fn windows_sdk_rc() -> Option<std::path::PathBuf> {
    let mut roots = Vec::new();
    if let Some(sdk) = std::env::var_os("WindowsSdkDir") {
        roots.push(std::path::PathBuf::from(sdk).join("bin"));
    }
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(folder) = std::env::var_os(var) {
            roots.push(std::path::PathBuf::from(folder).join("Windows Kits/10/bin"));
        }
    }
    let mut versions: Vec<_> = roots
        .iter()
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    versions.sort();
    versions
        .into_iter()
        .rev()
        .map(|v| v.join("x64/rc.exe"))
        .find(|p| p.is_file())
}
