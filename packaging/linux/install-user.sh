#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (C) 2026 Godwit contributors
# Installs Godwit's desktop entry and icons for this user, so GNOME (and
# other desktops) show its window icon, dock entry and notifications as
# Godwit's: they find all three through `godwit.desktop`, matched to the
# window's app id "godwit". Usage: install-user.sh [path/to/godwit]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
bin=$(realpath "${1:-$root/target/release/godwit}")
[ -x "$bin" ] || { echo "no godwit binary at $bin (pass its path)" >&2; exit 1; }
data=${XDG_DATA_HOME:-$HOME/.local/share}

for size in 16 32 64 128 256 512; do
  install -Dm644 "$root/assets/icon/icon-$size.png" "$data/icons/hicolor/${size}x$size/apps/godwit.png"
done
install -d "$data/applications"
sed "s|^Exec=godwit|Exec=$bin|" "$here/godwit.desktop" > "$data/applications/godwit.desktop"

gtk-update-icon-cache -q -t "$data/icons/hicolor" 2>/dev/null || true
update-desktop-database -q "$data/applications" 2>/dev/null || true
echo "installed: $data/applications/godwit.desktop → $bin"
