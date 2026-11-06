#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (C) 2026 Godwit contributors
# Builds the release binaries, one file per platform, into dist/:
#   godwit-linux-x86_64, godwit-windows-x86_64.exe (with cargo xwin),
#   a .sig beside each (Ed25519, hex) and SHA256SUMS.
# The player helper is built first and embedded (app/build.rs).
#
# tools/release.sh --container runs the same inside the build image
# (tools/build/Dockerfile, Ubuntu 22.04), so the Linux binary needs only
# glibc 2.35; the image is built the first time. Podman or Docker.
# Downloads and build output stay in .build-cache/ (git-ignored), or in
# GODWIT_BUILD_CACHE.
#
# Environment:
#   GODWIT_SIGNING_KEY   path to the Ed25519 private key (PEM); without it
#                        nothing is signed and the binaries don't self-update.
#   GODWIT_TARGETS       "linux windows" (default) or one of them.
#   GODWIT_BUILD_CACHE   where the container's downloads and builds go.
#   GODWIT_PODMAN_ROOT   podman's image storage for this build (podman --root).
#   GODWIT_JOBS          parallel jobs and CPUs for the container (default 6).
# private/release.env, if there, sets these for this machine.
# Paths on this machine (home, checkout) are mapped out of the binaries.
set -euo pipefail
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cd "$root"
[ -f private/release.env ] && . private/release.env

if [ "${1:-}" = --container ]; then
    image=godwit-build:22.04-llvm21
    engine=$(command -v podman || command -v docker) || { echo "needs podman or docker" >&2; exit 1; }
    cache=${GODWIT_BUILD_CACHE:-$root/.build-cache}
    jobs=${GODWIT_JOBS:-6}
    run=("$engine")
    case $engine in *podman) [ -n "${GODWIT_PODMAN_ROOT:-}" ] && run+=(--root "$GODWIT_PODMAN_ROOT") ;; esac
    # Lowest CPU and disk priority: the machine stays usable meanwhile.
    run=(nice -n 19 ionice -c3 "${run[@]}")
    "${run[@]}" image inspect "$image" >/dev/null 2>&1 || "${run[@]}" build --jobs 1 -t "$image" tools/build
    mkdir -p "$cache/registry" "$cache/git" "$cache/home" "$cache/target"
    args=(run --rm --cpus "$jobs" -v "$root:/src" -w /src -e GODWIT_TARGETS="${GODWIT_TARGETS:-linux windows}"
        -e CARGO_BUILD_JOBS="$jobs"
        -v "$cache/registry:/opt/cargo/registry" -v "$cache/git:/opt/cargo/git"
        -v "$cache/home:/cache/home" -v "$cache/target:/cache/target"
        -e HOME=/cache/home -e CARGO_TARGET_DIR=/cache/target)
    case $engine in
        # keep-id: files come out as yours; label=disable: SELinux would
        # refuse the mounted checkout.
        *podman) args+=(--userns=keep-id --security-opt label=disable) ;;
        *) args+=(--user "$(id -u):$(id -g)") ;;
    esac
    if [ -n "${GODWIT_SIGNING_KEY:-}" ]; then
        args+=(-v "$(realpath "$GODWIT_SIGNING_KEY"):/run/signing.pem:ro" -e GODWIT_SIGNING_KEY=/run/signing.pem)
    fi
    exec "${run[@]}" "${args[@]}" "$image" tools/release.sh
fi
dist=$root/dist
mkdir -p "$dist"
targets=${GODWIT_TARGETS:-linux windows}

export GODWIT_COMMIT=$(git rev-parse --short=10 HEAD)
# Mapped out of the binaries (panic messages hold source paths): home first,
# the checkout last, as the last matching prefix wins.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=~ --remap-path-prefix=$root=godwit"
# Where cargo builds (a cargo config may move it).
target_dir=$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
if [ -n "${GODWIT_SIGNING_KEY:-}" ]; then
    # The public half, raw 32 bytes in hex, is what builds verify with.
    export GODWIT_UPDATE_KEY=$(openssl pkey -in "$GODWIT_SIGNING_KEY" -pubout -outform DER | tail -c 32 | od -An -v -tx1 | tr -d ' \n')
fi

build() { # target, cargo command, player file, app file, output name
    local target=$1 cargo=$2 player=$3 app=$4 out=$5
    $cargo build --release --locked --target "$target" -p godwit-player
    GODWIT_EMBED_PLAYER="$target_dir/$target/release/$player" \
        $cargo build --release --locked --target "$target" -p godwit
    cp "$target_dir/$target/release/$app" "$dist/$out"
}

for t in $targets; do
    case $t in
        linux) build x86_64-unknown-linux-gnu cargo godwit-player godwit godwit-linux-x86_64 ;;
        # mozjpeg's C calls _BitScanForward64 without <intrin.h>, an error
        # in newer clang: a clang-cl first on PATH includes it in every
        # Windows compile (cargo-xwin sets the target CFLAGS itself, and
        # clang-cl puts _CL_ after the inputs).
        windows) (wrap=$(mktemp -d); real=$(command -v clang-cl)
            # Only for C files: assembly (blake3's .S) can't take a header.
            printf '#!/bin/sh\nfor a; do case $a in *.c) exec "%s" -FIintrin.h "$@" ;; esac; done\nexec "%s" "$@"\n' \
                "$real" "$real" >"$wrap/clang-cl"
            chmod +x "$wrap/clang-cl"
            export PATH="$wrap:$PATH"
            build x86_64-pc-windows-msvc "cargo xwin" godwit-player.exe godwit.exe godwit-windows-x86_64.exe) ;;
        *) echo "unknown target $t" >&2; exit 1 ;;
    esac
done

cd "$dist"
for f in godwit-*; do
    case $f in *.sig) continue ;; esac
    if [ -n "${GODWIT_SIGNING_KEY:-}" ]; then
        openssl pkeyutl -sign -rawin -inkey "$GODWIT_SIGNING_KEY" -in "$f" | od -An -v -tx1 | tr -d ' \n' >"$f.sig"
    fi
done
sha256sum godwit-* >SHA256SUMS
ls -l
