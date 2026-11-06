// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors

use super::*;

/// What the shell must do after a dispatch or apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Send to the backend of `account`.
    Backend { account: AccountId, command: Command },
    /// Contacts changed: write them to disk.
    SaveContacts,
    /// Watch or stop watching `user`'s screen share in our call in `room`:
    /// [`crate::account::NetworkAccount::watch_stream`] on `account`.
    WatchStream {
        account: AccountId,
        room: RoomId,
        user: UserId,
        watch: bool,
    },
}
