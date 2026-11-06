<p align="center">
  <img src="assets/icon/icon-256.png" width="96" alt="">
</p>

<h1 align="center">Godwit</h1>

<p align="center">
  <a href="https://github.com/NetrunningChoom/Godwit/releases/latest"><img src="https://img.shields.io/github/v/release/NetrunningChoom/Godwit?label=early&color=f0b232" alt="The latest release"></a>
  <a href="#download"><img src="https://img.shields.io/badge/Linux%20%7C%20Windows%2010%2F11-0078D4" alt="Windows and Linux"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/NetrunningChoom/Godwit?color=blue" alt="License: AGPL-3.0"></a>
  <img src="https://img.shields.io/badge/AI--slop-8B4513" alt="Vibe coded with AI assistance">
</p>

<p align="center">
  <b>One native app for all your chat networks.</b><br>
  About 26 MiB idle with three networks signed in. Sign in to each once, and
  your friends, servers and calls sit side by side, so hopping between them
  takes a click instead of another app.
</p>

<p align="center">
  <a href="#download"><b>Download</b></a>
  &nbsp;·&nbsp;
  <a href="#what-it-can-do"><b>What it can do</b></a>
</p>

> **godwit** */ˈɡɒdwɪt/*, noun. A long-billed wading bird. The bar-tailed
> godwit makes the longest nonstop migration of any bird: from Alaska to New
> Zealand and Australia, over 11,000 km of open ocean in about nine days,
> without landing to eat or rest. It travels in flocks.

> Early and in active development: test builds work day to day, with rough
> edges. Vibe coded with AI assistance. Use at own risk.

> **Disclaimer.** Godwit is an independent, unofficial project. It is not
> affiliated with, endorsed by or sponsored by Stoat, Discord, Steam, Fluxer,
> Microsoft Teams, Slack, Google Chat, or any other network or company it
> connects to; their names belong to their owners and are used only to say
> what Godwit works with. Some networks' terms don't allow third-party
> clients, and using one may get an account limited or closed. **Use Godwit
> entirely at your own risk**: it comes with no warranty of any kind (see
> [LICENSE](LICENSE)).

## What it's for

One light native client for all your chat apps, instead of a browser tab or
a heavy app per network.

- **One place for everything:** every account's chats, servers, friends and
  calls, with one look and one set of shortcuts.
- **Easy to hop between:** a friend you know on several networks is one
  contact, with each network a tab away.
- **Efficient:** drawn natively, with a small memory footprint and nothing
  running while idle.
- **Plain about privacy:** each chat shows whether it's end-to-end encrypted.

## Download

**[Latest release](https://github.com/NetrunningChoom/Godwit/releases/latest)**:
one file per platform.

| Platform | Download | Then |
|---|---|---|
| Linux (x86-64) | `godwit-linux-x86_64` | make it executable (`chmod +x godwit-linux-x86_64`, or tick "Allow executing" in its file properties) and run it |
| Windows 10/11 (x86-64) | `godwit-windows-x86_64.exe` | run it (Windows may warn about an unknown publisher: "More info" → "Run anyway") |

On first start Godwit offers to **add itself to your apps**: it moves to your
user's apps folder and appears in the app menu (on Windows: the Start menu and
the desktop). No admin rights needed. Said "Not now"? Settings › About has the
same button, and the one to remove it again.

Godwit **updates itself**: now and then it looks for a newer release and asks
before installing it ("Update and restart"), checked against the project's
signing key. Set `GODWIT_NO_UPDATE=1` to turn that off. Each release lists
`SHA256SUMS`.

### Linux compatibility

- **x86-64** (64-bit Intel/AMD). No ARM builds yet.
- **glibc 2.35 or newer**: Ubuntu 22.04+, Debian 12+, Fedora 36+, Linux Mint 21+,
  Pop!_OS 22.04+, openSUSE Tumbleweed, Arch and others as recent. Not musl-based
  systems (Alpine).
- **Wayland or X11**, any desktop.
- **Sound**: PulseAudio or PipeWire.
- **Video players** (inline videos, embeds): WebKitGTK 4.1 (`libwebkit2gtk-4.1`),
  installed on most desktops; everything else works without it.
- **Sign-ins** are kept in your keyring (GNOME Keyring, KWallet or any Secret
  Service); without one, in owner-only files.

## What it can do

Rough and honest: what works, and what each network is still missing.

| Network | Works today | Still missing |
|---|---|---|
| Stoat | Text, voice and video calls, screen share with its sound | — |
| Discord | Text, voice calls, cameras and Go Live (watching and streaming) | — |
| Steam | Friends, 1:1 and group chats, voice calls | — |
| Fluxer | Text, first version | Voice, calls and the rest |
| Microsoft Teams | Text chat, work in progress | Calls, files and most of the rest |
| Slack | Text chat, work in progress | Calls, files and most of the rest |
| Google Chat | Text chat, work in progress | Calls, files and most of the rest |

Not there yet: no ARM Linux builds; macOS untested; Android planned. And the
whole thing is early: expect rough edges everywhere, not just in the
work-in-progress networks.

## Platforms

Linux (PulseAudio or PipeWire), used day to day; Windows 10/11, newly
tested; macOS untested; Android planned. `godwit --demo` starts with made-up
data, no account needed.

## Building

```sh
cargo run --release
```

Rust stable (edition 2024), plus per OS:

| OS | Install |
|---|---|
| Fedora | `sudo dnf install -y clang nasm glib2-devel alsa-lib-devel libva-devel webkit2gtk4.1-devel` |
| Debian/Ubuntu | `sudo apt install -y clang nasm libglib2.0-dev libasound2-dev libva-dev libxkbcommon-dev libgtk-3-dev libwebkit2gtk-4.1-dev` |
| macOS | `xcode-select --install`, `brew install nasm` |
| Windows | Visual Studio Build Tools (MSVC), [nasm](https://www.nasm.us) on `PATH` |

The first build downloads LiveKit's prebuilt libwebrtc once (a few hundred
MB). Windows builds can also be made from Linux with `cargo xwin`.

## Contributing

[AGENTS.md](AGENTS.md) has the rules for contributors and AI agents.

## Thanks

- [Pidgin](https://pidgin.im) — the classic multi-network chat client. Godwit
  is a multi-client too, for a subset of today's networks.
- [Yapfast](https://github.com/yapfast/yapfast) — a native Discord client that
  proved a browser engine has no business in a chat app; a reference for the
  architecture here.
- [BetterDiscord](https://betterdiscord.app) — showed how much a client can do
  past the official one.
- [Stoat](https://stoat.chat) — its documented client
  ([for-web](https://github.com/stoatchat/for-web)) and published API spec are
  the reference for how Godwit talks to Stoat.

## License

AGPL-3.0-only ([LICENSE](LICENSE)). Bundles the [Inter](https://github.com/rsms/inter)
font (SIL Open Font License 1.1, `assets/fonts/Inter-LICENSE.txt`) and Google's
[Material Symbols](https://github.com/google/material-design-icons) (Apache
License 2.0, `assets/fonts/MaterialSymbols-LICENSE.txt`). The app icon and the
sign-in picture come from a public-domain photo of a bar-tailed godwit by the
U.S. Fish and Wildlife Service (`assets/art/godwit/README.md`).
