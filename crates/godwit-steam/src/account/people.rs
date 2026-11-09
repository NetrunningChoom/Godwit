// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Friends, personas and profiles.

use super::*;
use std::sync::Arc;

#[derive(Clone, Default)]
pub(super) struct ProfileDetails {
    pub(super) summary: Option<Arc<str>>,
    pub(super) level: Option<u32>,
    pub(super) member_since: Option<Arc<str>>,
}

/// Steam sends a Unix timestamp; the profile card needs a concise, stable
/// label, so show the creation year (as Steam's client does in its profile).
fn creation_year(timestamp: u32) -> Arc<str> {
    let days = i64::from(timestamp) / 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    year.to_string().into()
}

impl SteamAccount {
    /// After friends' personas changed: the friends list, member lists and
    /// unnamed groups (named by their members), and these chats' rows.
    pub(super) fn personas_changed(&mut self, changed_friends: bool, changed_rooms: Vec<u64>, out: &mut Vec<Event>) {
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

    pub(super) fn send_friends(&mut self, out: &mut Vec<Event>) {
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
        let mut blocked: Vec<UserId> = self
            .friends
            .iter()
            .filter(|(_, f)| f.relationship == BLOCKED)
            .map(|(id, _)| UserId::new(account.clone(), id.to_string()))
            .collect();
        blocked.sort_by(|a, b| a.raw.cmp(&b.raw));
        if blocked != self.blocked {
            self.blocked = blocked.clone();
            out.push(Event::Blocked {
                account: account.clone(),
                users: blocked,
            });
        }
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
            user: UserId::new(account.clone(), id.to_string()),
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
            bio: self.profile_details.get(&id).and_then(|d| d.summary.clone()),
            profile_level: self.profile_details.get(&id).and_then(|d| d.level),
            member_since: self.profile_details.get(&id).and_then(|d| d.member_since.clone()),
            roles: Vec::new(),
            me: own,
            relationship: relationship(friend.map(|f| f.relationship)),
            bot: false,
            has_friends: true,
            moderation: None,
            group_moderation: self.groups.values().find_map(|g| {
                let me = crate::backend::account_id(self.me?);
                let rank = g.ranks.get(&me).copied().unwrap_or_default();
                (id != self.me? && rank == 50).then(|| godwit_core::members::GroupModeration {
                    room: RoomId::new(account.clone(), group_room_raw(g.id, g.default_chat)),
                    add_member: true,
                    remove_member: false,
                    transfer_ownership: false,
                })
            }),
            can_block: !own,
            can_ignore: false,
            ignored: false,
            // Steam reports are handled through the Steam Community website.
            can_report: false,
            mutual_servers: Vec::new(),
            mutual_friends: Vec::new(),
            note: None,
            can_note: false,
            note_limit: 0,
        })
    }

    pub(super) fn profile_summary(
        &mut self,
        response: steam_vent_proto_steam::steammessages_clientserver_friends::CMsgClientFriendProfileInfoResponse,
    ) -> Option<Profile> {
        let id = response.steamid_friend?;
        let details = self.profile_details.entry(id).or_default();
        details.summary = response.summary.filter(|s| !s.trim().is_empty()).map(Into::into);
        details.member_since = response.time_created.filter(|t| *t > 0).map(creation_year);
        (self.profile_for == Some(id)).then(|| self.profile(id)).flatten()
    }

    pub(super) fn profile_levels(
        &mut self,
        response: steam_vent_proto_steam::steammessages_clientserver_2::CMsgClientFSGetFriendsSteamLevelsResponse,
    ) -> Option<Profile> {
        let mut current_updated = false;
        for friend in response.friends {
            let (Some(account_id), Some(level)) = (friend.accountid, friend.level) else {
                continue;
            };
            let steam_id = backend::steam_id(account_id);
            self.profile_details.entry(steam_id).or_default().level = Some(level);
            current_updated |= self.profile_for == Some(steam_id);
        }
        current_updated
            .then(|| self.profile_for.and_then(|id| self.profile(id)))
            .flatten()
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
