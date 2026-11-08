// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! One-to-one chats: rooms and messages.

use super::*;

impl SteamAccount {
    /// The direct chat with `friend`.
    pub(super) fn room(&self, friend: u64) -> Option<Room> {
        let account = self.account.clone()?;
        let f = self.friends.get(&friend);
        let persona = f.map(|f| &f.persona);
        let chat = self.chats.get(&friend);
        Some(Room {
            notify: Default::default(),
            id: RoomId::new(account.clone(), friend.to_string()),
            name: self.name_of(friend).into(),
            kind: RoomKind::Direct {
                with: UserId::new(account, friend.to_string()),
            },
            icon_url: Some(
                persona
                    .and_then(|p| p.avatar_hash.as_deref())
                    .map_or_else(|| avatar_url(&[]), avatar_url)
                    .into(),
            ),
            capabilities: Capabilities::of(Network::Steam),
            // Steam doesn't encrypt chats end to end.
            encryption: RoomEncryption::Unencrypted,
            unread: chat.is_some_and(|c| c.unread),
            mentions: chat.map_or(0, |c| c.mentions),
            last_read: None,
            invited_by: None,
            presence: Some(persona.and_then(|p| p.state).map_or(Presence::Offline, presence)),
            status: persona.and_then(playing).map(Into::into),
            member_count: None,
            last_activity: chat.map_or(0, |c| i64::from(c.last) * 1000),
            parent: None,
        })
    }

    pub(super) fn message(&mut self, friend: u64, line: &Line) -> Option<Message> {
        let account = self.account.clone()?;
        let me = self.me?;
        let from = if line.from == backend::account_id(me) {
            me
        } else {
            friend
        };
        let avatar = if from == me {
            self.my_avatar.clone()
        } else {
            self.friends
                .get(&friend)
                .and_then(|f| f.persona.avatar_hash.as_deref())
                .map(avatar_url)
        };
        // Steam's markup (emoticons, stickers, links) as Godwit's.
        let converted = crate::bbcode::convert(&line.text);
        let id = message_id(line.time, line.ordinal);
        let embeds = self.game_embeds(self.room_id(friend), &id, from, converted.games);
        Some(Message {
            id,
            author: UserId::new(account, from.to_string()),
            author_name: self.name_of(from).into(),
            author_avatar: Some(avatar.unwrap_or_else(|| avatar_url(&[])).into()),
            author_colour: None,
            sent_ms: i64::from(line.time) * 1000,
            edited: false,
            system: false,
            text: converted.rich.into(),
            markup: Markup::Rich,
            source: converted.plain.into(),
            reply_to: None,
            jumbo: converted
                .jumbo
                .into_iter()
                .map(|(url, name)| (url.into(), name.into()))
                .collect(),
            attachments: images(converted.images),
            embeds,
            invites: converted.invites.into_iter().map(Into::into).collect(),
            reactions: Vec::new(),
            pending: false,
            snippet: None,
            pinned: false,
            forwardable: false,
        })
    }
}
