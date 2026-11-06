# steam-vent 0.5.0, vendored for Godwit

Upstream: https://codeberg.org/steam-vent/steam-vent (MIT, see LICENSE).
Used through `[patch.crates-io]` in the workspace `Cargo.toml`.

Changes from the published 0.5.0:

- `Cargo.toml`: reqwest without default features (`json`, `rustls-no-provider`) and rustls
  without default features, so no aws-lc is built; Godwit installs rustls's ring provider at
  start.
- `src/transport/websocket.rs`: doesn't install the aws-lc provider, and doesn't log TLS keys
  to `SSLKEYLOGFILE`.
- `src/connection/filter.rs`: per-kind channels hold 1024 messages (was 16) and 512 messages wait
  for a listener (was 32): the persona states Steam sends for every friend on signing in
  overflowed them, so names never arrived.
- `src/connection/mod.rs`: `Connection::notify_service` sends a service notification without a
  job id (as Steam's clients do), for voice status; `service_method` always waits for a reply.
- `src/session.rs`: the logon's `client_os_type` is this system's real EOSType (Linux −203,
  Windows 16, macOS −102), the same as Godwit's sign-in; upstream sent 203, which isn't one.
- `src/connection/filter.rs`: when the connection's reader ends (the socket closed, or Steam
  logged the session off), every listener's channel is dropped and later subscriptions get an
  ended stream. Upstream kept the senders alive, so listeners waited forever on a dead
  connection and Godwit never noticed it had to reconnect.
- Examples, tools, the Nix flake and images left out.
- `src/session.rs`, `src/connection/`: `login`/`access` take the machine name the caller's
  sign-in gave the device (Godwit's random `DESKTOP-…` name), instead of a fixed "Godwit".
