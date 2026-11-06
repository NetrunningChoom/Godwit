// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Typed ids. Every id carries the account it belongs to, so two accounts on
//! the same network (or the same raw id on two networks) never mix.
//! Raw ids are `Arc<str>`: cloning one is a reference-count bump, which
//! matters for lists the UI rebuilds often.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::network::Network;

/// One signed-in account: a network plus that network's id for the user
/// (a Stoat user id, a Discord snowflake, a Steam id).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AccountId {
    pub network: Network,
    pub user: Arc<str>,
}

impl AccountId {
    pub fn new(network: Network, user: impl Into<Arc<str>>) -> Self {
        Self {
            network,
            user: user.into(),
        }
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.network.key(), self.user)
    }
}

macro_rules! scoped_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name {
            pub account: AccountId,
            pub raw: Arc<str>,
        }

        impl $name {
            pub fn new(account: AccountId, raw: impl Into<Arc<str>>) -> Self {
                Self { account, raw: raw.into() }
            }

            pub fn network(&self) -> Network {
                self.account.network
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}/{}", self.account, self.raw)
            }
        }
    };
}

scoped_id!(
    /// A conversation: a DM, a group or a server channel.
    RoomId
);
scoped_id!(
    /// A server, guild or Steam group.
    ServerId
);
scoped_id!(
    /// Another person (or bot) as one account sees them.
    UserId
);

/// A message, unique within its room.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MessageId(pub Arc<str>);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_raw_id_on_two_accounts_differs() {
        let a = AccountId::new(Network::Stoat, "me");
        let b = AccountId::new(Network::Stoat, "alt");
        assert_ne!(RoomId::new(a.clone(), "01ABC"), RoomId::new(b, "01ABC"));
        assert_eq!(RoomId::new(a.clone(), "01ABC"), RoomId::new(a, "01ABC"));
    }

    #[test]
    fn display_names_network_and_account() {
        let a = AccountId::new(Network::Discord, "100");
        assert_eq!(RoomId::new(a, "200").to_string(), "discord:100/200");
    }
}
