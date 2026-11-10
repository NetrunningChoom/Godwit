// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Steam's friend groups (tags). Steam sends `CMsgClientFriendsGroupsList`
//! (EMsg `k_EMsgClientFriendsGroupsList`) on sign-in and again when a group
//! or membership changes. Message shapes: SteamDatabase/Protobufs,
//! `steammessages_clientserver_friends.proto`
//! (<https://github.com/SteamDatabase/Protobufs>). Read-only.

use std::collections::{BTreeMap, BTreeSet};

pub use steam_vent_proto_steam::steammessages_clientserver_friends::CMsgClientFriendsGroupsList as GroupsList;

/// The groups as Steam last told us: names by group id, and who is in which.
#[derive(Default)]
pub struct Groups {
    names: BTreeMap<i32, String>,
    /// (group id, friend's 64-bit Steam id).
    members: BTreeSet<(i32, u64)>,
}

/// One group, mapped for the neutral model: id, name, members' account ids.
pub type Group = (i32, String, Vec<u32>);

impl Groups {
    /// Folds in a message. A full list replaces everything; an incremental
    /// one adds, or (with the removal flag) removes what it names.
    pub fn apply(&mut self, list: &GroupsList) {
        let removal = list.bremoval();
        if !list.bincremental() {
            self.names.clear();
            self.members.clear();
        }
        for g in &list.friendGroups {
            let id = g.nGroupID();
            if removal {
                self.names.remove(&id);
                self.members.retain(|(group, _)| *group != id);
            } else {
                self.names.insert(id, g.strGroupName().to_owned());
            }
        }
        for m in &list.memberships {
            let key = (m.nGroupID(), m.ulSteamID());
            if removal {
                self.members.remove(&key);
            } else {
                self.members.insert(key);
            }
        }
    }

    /// Every group with its members, by group id. Members of a group the
    /// list never named are dropped.
    pub fn snapshot(&self) -> Vec<Group> {
        self.names
            .iter()
            .map(|(id, name)| {
                let members = self
                    .members
                    .iter()
                    .filter(|(g, _)| g == id)
                    .map(|(_, steam)| crate::backend::account_id(*steam))
                    .collect();
                (*id, name.clone(), members)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(groups: &[(i32, &str)], members: &[(i32, u64)], incremental: bool, removal: bool) -> GroupsList {
        let mut l = GroupsList::default();
        l.set_bincremental(incremental);
        l.set_bremoval(removal);
        for (id, name) in groups {
            let mut g = steam_vent_proto_steam::steammessages_clientserver_friends::cmsg_client_friends_groups_list::FriendGroup::default();
            g.set_nGroupID(*id);
            g.set_strGroupName((*name).to_owned());
            l.friendGroups.push(g);
        }
        for (id, steam) in members {
            let mut m = steam_vent_proto_steam::steammessages_clientserver_friends::cmsg_client_friends_groups_list::FriendGroupsMembership::default();
            m.set_nGroupID(*id);
            m.set_ulSteamID(*steam);
            l.memberships.push(m);
        }
        l
    }

    const A: u64 = 0x0110_0001_0000_0001;
    const B: u64 = 0x0110_0001_0000_0002;

    #[test]
    fn a_full_list_maps_to_groups_with_account_ids() {
        let mut g = Groups::default();
        g.apply(&list(
            &[(1, "Family"), (2, "Raid")],
            &[(1, A), (2, A), (2, B)],
            false,
            false,
        ));
        assert_eq!(
            g.snapshot(),
            vec![(1, "Family".into(), vec![1]), (2, "Raid".into(), vec![1, 2])]
        );
    }

    #[test]
    fn incremental_messages_add_and_remove() {
        let mut g = Groups::default();
        g.apply(&list(&[(1, "Family")], &[(1, A)], false, false));
        g.apply(&list(&[(2, "Raid")], &[(2, B)], true, false));
        g.apply(&list(&[], &[(1, A)], true, true));
        assert_eq!(
            g.snapshot(),
            vec![(1, "Family".into(), vec![]), (2, "Raid".into(), vec![2])]
        );
        g.apply(&list(&[(1, "")], &[], true, true));
        assert_eq!(g.snapshot().len(), 1);
        // A new full list replaces the lot.
        g.apply(&list(&[(3, "New")], &[], false, false));
        assert_eq!(g.snapshot(), vec![(3, "New".into(), vec![])]);
    }
}
