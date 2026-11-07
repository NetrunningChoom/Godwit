// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Friends, personas and profiles.

use super::*;

impl SteamAccount {
    /// After friends' personas changed: the friends list, member lists and
    /// unnamed groups (named by their members), and these chats' rows.
    pub(super) fn personas_changed(&self, changed_friends: bool, changed_rooms: Vec<u64>, out: &mut Vec<Event>) {
        if self.ready {
            if changed_friends {
                self.send_friends(out);
                if let Some((g, c)) = self.members_for
                    && let Some(event) = self.group_members(g, c)
                {
                    out.push(event);
                }
                // Unnamed groups go by their members' names.
                let unnamed: Vec<(u64, u64)> = self
                    .groups
                    .values()
                    .filter(|g| g.name.is_empty())
                    .flat_map(|g| g.rooms.iter().map(move |r| (g.id, r.chat)))
                    .collect();
                for (g, c) in unnamed {
                    if let Some(room) = self.group_room(g, c) {
                        out.push(Event::RoomUpdated(room));
                    }
                }
            }
            for id in changed_rooms {
                if let Some(room) = self.room(id) {
                    out.push(Event::RoomUpdated(room));
                }
            }
        }
    }

    /// Ready once our own persona came (or a few seconds passed): the
    /// account with its chats.
    pub(super) fn make_ready(&mut self, out: &mut Vec<Event>) {
        if self.ready {
            return;
        }
        let Some(account) = self.account.clone() else { return };
        self.ready = true;
        self.status = Status::Ready;
        let mut ids: Vec<u64> = self.chats.keys().copied().collect();
        ids.sort_by_key(|id| std::cmp::Reverse(self.chats.get(id).map_or(0, |c| c.last)));
        let mut rooms: Vec<Room> = ids.into_iter().filter_map(|id| self.room(id)).collect();
        rooms.extend(self.all_group_rooms());
        out.push(Event::AccountReady {
            account: account.clone(),
            display_name: self.my_name.clone().unwrap_or_else(|| "Steam".into()).into(),
            username: self.my_name.clone().unwrap_or_default().into(),
            avatar_url: self.my_avatar.clone().map(Into::into),
            rooms,
            servers: self.group_servers(),
        });
        self.push_voice_occupants(out);
        if let Some(presence) = self.my_state.map(presence) {
            out.push(Event::OwnPresence {
                account,
                presence,
                status: None,
            });
        }
        self.send_friends(out);
    }

    pub(super) fn send_friends(&self, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else { return };
        if !self.ready {
            return;
        }
        let with = |kind: u32| {
            let mut people: Vec<Person> = self
                .friends
                .iter()
                .filter(|(_, f)| f.relationship == kind)
                .map(|(id, f)| Person {
                    id: UserId::new(account.clone(), id.to_string()),
                    name: self.name_of(*id).into(),
                    avatar_url: Some(
                        f.persona
                            .avatar_hash
                            .as_deref()
                            .map_or_else(|| avatar_url(&[]), avatar_url)
                            .into(),
                    ),
                    online: f.persona.state.is_some_and(|s| s != 0),
                })
                .collect();
            people.sort_by_key(|p| p.name.to_lowercase());
            people
        };
        // Pending requests are in the same list, by relationship: accepting is
        // AddFriend, declining or cancelling RemoveFriend, as in Steam's client.
        let (incoming, outgoing, people) = (with(REQUEST_RECIPIENT), with(REQUEST_INITIATOR), with(FRIEND));
        out.push(Event::FriendRequests {
            account: account.clone(),
            incoming,
            outgoing,
        });
        // What friends are playing, for Active now (as Steam's friends list says it).
        let activities = self
            .friends
            .iter()
            .filter(|(_, f)| f.relationship == FRIEND && f.persona.state.is_some_and(|s| s != 0))
            .filter_map(|(id, f)| {
                Some((
                    UserId::new(account.clone(), id.to_string()),
                    playing(&f.persona)?.into(),
                ))
            })
            .collect();
        out.push(Event::Activities {
            account: account.clone(),
            activities,
        });
        out.push(Event::Friends { account, people });
        if let Some(profile) = self.profile_for.and_then(|id| self.profile(id)) {
            out.push(Event::Profile(profile));
        }
    }

    pub(super) fn profile(&self, id: u64) -> Option<Profile> {
        let account = self.account.clone()?;
        let friend = self.friends.get(&id);
        let persona = friend.map(|friend| &friend.persona);
        let own = self.me == Some(id);
        Some(Profile {
            user: UserId::new(account, id.to_string()),
            name: self.name_of(id).into(),
            username: id.to_string().into(),
            avatar_url: if own {
                self.my_avatar.clone().map(Into::into)
            } else {
                Some(avatar_url(persona.and_then(|p| p.avatar_hash.as_deref()).unwrap_or_default()).into())
            },
            banner_url: None,
            presence: if own {
                self.my_state.map(presence)
            } else {
                persona.and_then(|p| p.state).map(presence)
            }
            .unwrap_or(Presence::Unknown),
            status: persona.and_then(playing).map(Into::into),
            bio: None,
            roles: Vec::new(),
            me: own,
            relationship: relationship(friend.map(|f| f.relationship)),
            bot: false,
            has_friends: true,
            moderation: None,
            group_moderation: None,
            can_block: false,
        })
    }

    pub(super) fn name_of(&self, id: u64) -> String {
        if Some(id) == self.me {
            return self.my_name.clone().unwrap_or_else(|| "You".into());
        }
        self.friends
            .get(&id)
            .and_then(|f| f.persona.name.clone())
            .unwrap_or_else(|| format!("Steam user {}", backend::account_id(id)))
    }

    pub(super) fn friend_of(room: &RoomId) -> Option<u64> {
        room.raw.parse().ok()
    }
}
