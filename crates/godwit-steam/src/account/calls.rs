// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Calls: voice settings, ringing and who is in a call.

use super::*;

impl SteamAccount {
    /// New settings apply to calls from now on (and to the volumes of one under way).
    pub fn set_voice_settings(&mut self, settings: VoiceSettings) {
        if self.call.is_some() && settings != self.voice_settings {
            self.net
                .send(Cmd::CallCmd(SessionCmd::Settings(Box::new(settings.clone()))));
        }
        self.voice_settings = settings;
    }

    /// Someone in our call, as the core shows them.
    pub(super) fn call_member(&self, account: &AccountId, member: &Member) -> CallMember {
        let avatar = if Some(member.steam_id) == self.me {
            self.my_avatar.clone()
        } else {
            self.friends.get(&member.steam_id).map(|f| {
                f.persona
                    .avatar_hash
                    .as_deref()
                    .map_or_else(|| avatar_url(&[]), avatar_url)
            })
        };
        CallMember {
            user: UserId::new(account.clone(), member.steam_id.to_string()),
            name: self.name_of(member.steam_id).into(),
            avatar_url: avatar.map(Into::into),
            speaking: member.speaking,
            muted: member.muted,
            deafened: member.deafened,
            stage: None,
            video: false,
            streaming: false,
        }
    }

    /// A friend calling us shows as them being in the DM's call; joining it answers.
    pub(super) fn ringing_from(&self, partner: u64) -> Vec<CallMember> {
        let Some(account) = self.account.clone() else {
            return Vec::new();
        };
        let member = Member {
            steam_id: partner,
            speaking: false,
            muted: false,
            deafened: false,
        };
        vec![self.call_member(&account, &member)]
    }

    pub(super) fn on_call(&mut self, call: u64, event: SessionEvent, out: &mut Vec<Event>) {
        let Some((_, room)) = self.call.clone().filter(|(c, _)| *c == call) else {
            return;
        };
        match event {
            SessionEvent::Ringing => {}
            SessionEvent::Connected => out.push(Event::CallConnected { room }),
            SessionEvent::Members(members) => {
                let Some(account) = self.account.clone() else { return };
                let members = members.iter().map(|m| self.call_member(&account, m)).collect();
                out.push(Event::CallMembers { room, members });
            }
            SessionEvent::Ended(reason) => {
                self.call = None;
                out.push(Event::CallEnded {
                    room,
                    reason: reason.map(|r| format!("Steam call: {r}").into()),
                });
            }
        }
    }

    /// Who's in each group room's voice chat, seen from outside it.
    pub(super) fn push_voice_occupants(&mut self, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else { return };
        let mut now = HashSet::new();
        for g in self.groups.values() {
            for r in g.rooms.iter().filter(|r| !r.in_voice.is_empty()) {
                now.insert((g.id, r.chat));
                let members = r
                    .in_voice
                    .iter()
                    .map(|a| {
                        let member = Member {
                            steam_id: backend::steam_id(*a),
                            speaking: false,
                            muted: false,
                            deafened: false,
                        };
                        self.call_member(&account, &member)
                    })
                    .collect();
                out.push(Event::VoiceOccupants {
                    room: RoomId::new(account.clone(), group_room_raw(g.id, r.chat)),
                    members,
                });
            }
        }
        for (g, c) in self.voiced.difference(&now) {
            out.push(Event::VoiceOccupants {
                room: RoomId::new(account.clone(), group_room_raw(*g, *c)),
                members: Vec::new(),
            });
        }
        self.voiced = now;
    }
}
