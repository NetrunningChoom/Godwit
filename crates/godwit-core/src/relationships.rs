// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The friends, friend requests and blocked lists an account last sent, so a
//! network crate rebuilds them on every relationship or presence change and
//! still sends each list only when it changed.

use crate::application::{Event, Person};
use crate::ids::{AccountId, UserId};

#[derive(Debug, Default)]
pub struct RelationshipLists {
    friends: Vec<Person>,
    incoming: Vec<Person>,
    outgoing: Vec<Person>,
    blocked: Vec<UserId>,
}

impl RelationshipLists {
    /// Sorts the lists (people by name, blocked by id) and pushes the events
    /// for those that differ from the last call.
    pub fn sync(
        &mut self,
        account: &AccountId,
        mut friends: Vec<Person>,
        mut incoming: Vec<Person>,
        mut outgoing: Vec<Person>,
        mut blocked: Vec<UserId>,
        out: &mut Vec<Event>,
    ) {
        for list in [&mut friends, &mut incoming, &mut outgoing] {
            list.sort_by_cached_key(|p| p.name.to_lowercase());
        }
        // By id, not by name: a rename mustn't send the list again.
        blocked.sort_by(|a, b| a.raw.cmp(&b.raw));
        if friends != self.friends {
            self.friends = friends.clone();
            out.push(Event::Friends {
                account: account.clone(),
                people: friends,
            });
        }
        if incoming != self.incoming || outgoing != self.outgoing {
            self.incoming = incoming.clone();
            self.outgoing = outgoing.clone();
            out.push(Event::FriendRequests {
                account: account.clone(),
                incoming,
                outgoing,
            });
        }
        if blocked != self.blocked {
            self.blocked = blocked.clone();
            out.push(Event::Blocked {
                account: account.clone(),
                users: blocked,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::Network;

    fn person(account: &AccountId, id: &str, name: &str) -> Person {
        Person {
            id: UserId::new(account.clone(), id),
            name: name.into(),
            avatar_url: None,
            online: false,
        }
    }

    #[test]
    fn sends_only_the_lists_that_changed_sorted() {
        let account = AccountId::new(Network::Discord, "me");
        let mut lists = RelationshipLists::default();
        let mut out = Vec::new();
        let friends = vec![person(&account, "2", "bob"), person(&account, "1", "Alice")];
        lists.sync(&account, friends.clone(), Vec::new(), Vec::new(), Vec::new(), &mut out);
        assert!(matches!(&out[..], [Event::Friends { people, .. }] if &*people[0].name == "Alice"));
        out.clear();
        lists.sync(&account, friends, Vec::new(), Vec::new(), Vec::new(), &mut out);
        assert!(out.is_empty());
        let blocked = vec![UserId::new(account.clone(), "9")];
        lists.sync(&account, Vec::new(), Vec::new(), Vec::new(), blocked, &mut out);
        assert!(matches!(&out[..], [Event::Friends { .. }, Event::Blocked { .. }]));
    }
}
