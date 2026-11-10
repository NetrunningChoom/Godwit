// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Network-neutral edits to the signed-in user's server rail folders.

use crate::ids::ServerId;
use crate::server::Server;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FolderEdit {
    Create {
        servers: [ServerId; 2],
    },
    AddServer {
        folder: ServerId,
        server: ServerId,
        at: Option<usize>,
    },
    RemoveServer {
        server: ServerId,
    },
    Rename {
        folder: ServerId,
        name: Option<String>,
    },
    Recolour {
        folder: ServerId,
        colour: Option<u32>,
    },
    /// Dissolve the folder; its servers stay, as plain ones, in place.
    Ungroup {
        folder: ServerId,
    },
}

impl FolderEdit {
    /// Every server id the edit names.
    pub fn server_ids(&self) -> Vec<&ServerId> {
        match self {
            Self::Create { servers } => servers.iter().collect(),
            Self::AddServer { folder, server, .. } => vec![folder, server],
            Self::RemoveServer { server } => vec![server],
            Self::Rename { folder, .. } | Self::Recolour { folder, .. } | Self::Ungroup { folder } => vec![folder],
        }
    }

    /// The edit would change something, given the servers we know. Anything
    /// stale or already so (a folder that is gone, a rename to the same name)
    /// sends no request, so a double click or a race never writes the
    /// network's list for nothing. A `folder` id is the folder's own id,
    /// never a server's.
    pub fn changes(&self, servers: &[Arc<Server>]) -> bool {
        let folder_of = |id: &ServerId| servers.iter().find(|s| &s.id == id).and_then(|s| s.folder.as_ref());
        let folder_named = |f: &ServerId| {
            servers
                .iter()
                .filter(|s| s.id.account == f.account)
                .find_map(|s| s.folder.as_ref().filter(|x| *x.id == *f.raw))
        };
        match self {
            Self::Create { servers: pair } => {
                pair[0] != pair[1]
                    && pair
                        .iter()
                        .all(|id| servers.iter().any(|s| &s.id == id) && folder_of(id).is_none())
            }
            Self::AddServer { folder, server, .. } => {
                servers.iter().any(|s| &s.id == server)
                    && folder_named(folder).is_some_and(|f| folder_of(server).is_none_or(|cur| cur.id != f.id))
            }
            Self::RemoveServer { server } => folder_of(server).is_some(),
            Self::Rename { folder, name } => folder_named(folder).is_some_and(|f| f.name.as_deref() != name.as_deref()),
            Self::Recolour { folder, colour } => folder_named(folder).is_some_and(|f| f.colour != *colour),
            Self::Ungroup { folder } => folder_named(folder).is_some(),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ids::AccountId;
    use crate::network::Network;
    use crate::server::ServerFolder;

    /// A server of `account`, in folder `folder` (id, name, colour) if given.
    pub(crate) fn server(
        account: &AccountId,
        raw: &str,
        folder: Option<(&str, Option<&str>, Option<u32>)>,
    ) -> Arc<Server> {
        Arc::new(Server {
            id: ServerId::new(account.clone(), raw),
            name: raw.into(),
            icon_url: None,
            banner_url: None,
            categories: Vec::new(),
            muted: false,
            rights: Default::default(),
            folder: folder.map(|(id, name, colour)| {
                Arc::new(ServerFolder {
                    id: id.into(),
                    name: name.map(Into::into),
                    colour,
                })
            }),
            scheduled_events: false,
            can_view_moderation_log: false,
            scheduled_event_count: 0,
            edit_own_profile: false,
        })
    }

    #[test]
    fn edits_that_change_nothing_are_not_sent() {
        let a = AccountId::new(Network::Discord, "me");
        let id = |raw: &str| ServerId::new(a.clone(), raw);
        let servers = vec![
            server(&a, "1", Some(("f", Some("Games"), Some(0xff0000)))),
            server(&a, "2", Some(("f", Some("Games"), Some(0xff0000)))),
            server(&a, "3", None),
            server(&a, "4", None),
        ];
        let folder = id("f");
        // A folder id is not a server id: "f" is no server, yet the folder exists.
        assert!(
            FolderEdit::AddServer {
                folder: folder.clone(),
                server: id("3"),
                at: None
            }
            .changes(&servers)
        );
        assert!(
            !FolderEdit::AddServer {
                folder: folder.clone(),
                server: id("1"),
                at: None
            }
            .changes(&servers),
            "already in it"
        );
        assert!(
            !FolderEdit::AddServer {
                folder: id("gone"),
                server: id("3"),
                at: None
            }
            .changes(&servers),
            "no such folder"
        );
        assert!(
            FolderEdit::Create {
                servers: [id("3"), id("4")]
            }
            .changes(&servers)
        );
        assert!(
            !FolderEdit::Create {
                servers: [id("3"), id("3")]
            }
            .changes(&servers),
            "two different servers"
        );
        assert!(
            !FolderEdit::Create {
                servers: [id("1"), id("3")]
            }
            .changes(&servers),
            "one is in a folder already"
        );
        assert!(FolderEdit::RemoveServer { server: id("1") }.changes(&servers));
        assert!(!FolderEdit::RemoveServer { server: id("3") }.changes(&servers));
        assert!(
            !FolderEdit::Rename {
                folder: folder.clone(),
                name: Some("Games".into())
            }
            .changes(&servers)
        );
        assert!(
            FolderEdit::Rename {
                folder: folder.clone(),
                name: None
            }
            .changes(&servers)
        );
        assert!(
            !FolderEdit::Recolour {
                folder: folder.clone(),
                colour: Some(0xff0000)
            }
            .changes(&servers)
        );
        assert!(
            FolderEdit::Recolour {
                folder: folder.clone(),
                colour: None
            }
            .changes(&servers)
        );
        assert!(FolderEdit::Ungroup { folder }.changes(&servers));
        assert!(!FolderEdit::Ungroup { folder: id("gone") }.changes(&servers));
    }
}
