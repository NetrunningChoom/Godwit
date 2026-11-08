# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (C) 2026 Godwit contributors
#
# The release build image (tools/release.sh --container). Ubuntu 22.04 is the oldest with
# WebKitGTK 4.1 (the video player), so Linux binaries built here need
# glibc 2.35: Ubuntu 22.04+, Debian 12+, Fedora 36+ and alike.
# Windows binaries are cross-built here too, with cargo-xwin.
#
# release.sh builds it the first time (podman or docker build -t godwit-build:22.04-llvm21-xz tools/build).
FROM ubuntu:22.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl git gnupg nasm openssl pkg-config xz-utils \
        libasound2-dev libglib2.0-dev libgtk-3-dev libva-dev libwebkit2gtk-4.1-dev libxkbcommon-dev \
    && rm -rf /var/lib/apt/lists/*
# LiveKit's prebuilt libwebrtc ships a libc++ that needs clang 21 or later;
# 22.04 has 14, so LLVM's own packages (apt.llvm.org). Also clang-cl and
# lld-link for the Windows cross-build.
RUN curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key | gpg --dearmor -o /usr/share/keyrings/llvm.gpg \
    && echo "deb [signed-by=/usr/share/keyrings/llvm.gpg] http://apt.llvm.org/jammy/ llvm-toolchain-jammy-21 main" \
        >/etc/apt/sources.list.d/llvm.list \
    && apt-get update && apt-get install -y --no-install-recommends clang-21 lld-21 llvm-21 libclang-21-dev \
    && rm -rf /var/lib/apt/lists/*
ENV PATH=/usr/lib/llvm-21/bin:$PATH CC=clang CXX=clang++
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --component clippy,rustfmt \
    && rustup target add x86_64-pc-windows-msvc \
    && cargo install --locked cargo-xwin \
    && chmod -R a+rwX /opt/cargo /opt/rustup
