// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Messages: mapping, polls, typing, alerts and reactions.

use super::*;

impl DiscordAccount {
    pub(super) fn names(&self, guild: Option<&str>) -> Names<'_> {
        Names {
            store: &self.store,
            guild: guild.map(str::to_owned),
        }
    }

    pub(super) fn message(&self, m: &Value) -> Option<Message> {
        let account = self.account.clone()?;
        let id = m["id"].as_str()?;
        let channel = m["channel_id"].as_str().unwrap_or_default();
        let guild = m["guild_id"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| self.store.channels.get(channel).and_then(|c| c.guild.clone()));
        let author = m["author"]["id"].as_str().unwrap_or_default();
        let kind = m["type"].as_u64().unwrap_or_default();
        // A forwarded message (`message_reference.type` 1) carries the original
        // in `message_snapshots`; it's shown with a "Forwarded" line, as officially.
        let forwarded = (m["message_reference"]["type"].as_u64() == Some(1))
            .then(|| &m["message_snapshots"][0]["message"])
            .filter(|f| f.is_object());
        let body = forwarded.unwrap_or(m);
        if forwarded.is_none() && m["poll"].is_object() {
            self.polls.borrow_mut().insert(id.to_owned(), m.clone());
        }
        let content = body["content"].as_str().unwrap_or_default();
        let names = self.names(guild.as_deref());
        // Default (0) and replies (19) are people's; the rest Discord writes.
        let system = !matches!(kind, 0 | 19 | 20 | 23);
        let (text, markup) = if system {
            (
                system_text(kind, &self.store.display_name(author, guild.as_deref()), content),
                Markup::Plain,
            )
        } else {
            let mut rich = crate::markdown::to_rich(content, &names);
            if let Some(poll) = poll_text(&body["poll"], forwarded.is_none()) {
                if !rich.is_empty() {
                    rich.push('\n');
                }
                rich.push_str(&poll);
            }
            match forwarded {
                Some(_) => (format!("*↪ Forwarded*\n{rich}"), Markup::Rich),
                None => (rich, Markup::Rich),
            }
        };
        let attachments = body["attachments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| {
                let ctype = a["content_type"].as_str().unwrap_or_default();
                let kind = if ctype.starts_with("image/") {
                    AttachmentKind::Image
                } else if ctype.starts_with("video/") {
                    AttachmentKind::Video
                } else if ctype.starts_with("audio/") {
                    AttachmentKind::Audio
                } else {
                    AttachmentKind::File
                };
                let size = |k: &str| a[k].as_u64().and_then(|v| u32::try_from(v).ok());
                Some(Attachment {
                    name: a["filename"].as_str().unwrap_or("file").into(),
                    url: a["url"].as_str()?.into(),
                    kind,
                    size: a["size"].as_u64().unwrap_or_default(),
                    dimensions: size("width").zip(size("height")),
                    original_url: None,
                })
            })
            .collect();
        let reactions = m["reactions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(reaction)
            .collect();
        let sent_ms = m["timestamp"]
            .as_str()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map_or_else(|| crate::api::snowflake_ms(id), |t| t.timestamp_millis());
        Some(Message {
            id: MessageId(id.into()),
            author: UserId::new(account, author),
            author_name: self.store.display_name(author, guild.as_deref()).into(),
            author_avatar: Some(self.avatar(author, guild.as_deref()).into()),
            author_colour: guild.as_deref().and_then(|g| self.store.colour(author, g)),
            sent_ms,
            edited: m["edited_timestamp"].is_string(),
            system,
            text: text.into(),
            markup,
            source: content.into(),
            reply_to: (kind == 19)
                .then(|| m["message_reference"]["message_id"].as_str())
                .flatten()
                .map(|r| MessageId(r.into())),
            jumbo: stickers(m),
            attachments,
            embeds: body["embeds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(embed)
                .collect(),
            invites: invite_codes(content).into_iter().map(Into::into).collect(),
            reactions,
            pending: false,
            snippet: (!system).then(|| crate::markdown::to_plain(content, &names).into()),
            pinned: m["pinned"].as_bool().unwrap_or(false),
        })
    }

    /// Our vote for `answer`: a single-choice poll moves our vote to it (or
    /// takes it back if it was ours); a multiple-choice one toggles it.
    pub(super) fn vote(&mut self, _room: &RoomId, channel: &str, message: &str, answer: u64, out: &mut Vec<Event>) {
        let Some(raw) = self.polls.borrow().get(message).cloned() else {
            return;
        };
        let poll = &raw["poll"];
        let mine: Vec<u64> = poll["results"]["answer_counts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["me_voted"].as_bool() == Some(true))
            .filter_map(|c| c["id"].as_u64())
            .collect();
        let multi = poll["allow_multiselect"].as_bool() == Some(true);
        let had = mine.contains(&answer);
        let now: Vec<u64> = match (multi, had) {
            (true, true) => mine.iter().copied().filter(|a| *a != answer).collect(),
            (true, false) => mine.iter().copied().chain([answer]).collect(),
            (false, true) => Vec::new(),
            (false, false) => vec![answer],
        };
        self.net.send(Cmd::Request(Request::Vote {
            channel: channel.to_owned(),
            message: message.to_owned(),
            answers: now.clone(),
        }));
        // Shown at once; Discord's own events don't repeat our votes.
        let mut changes: Vec<(u64, i64, Option<bool>)> = Vec::new();
        for a in &mine {
            if !now.contains(a) {
                changes.push((*a, -1, Some(false)));
            }
        }
        for a in &now {
            if !mine.contains(a) {
                changes.push((*a, 1, Some(true)));
            }
        }
        self.change_votes(channel, message, &changes, out);
    }

    /// Changes a cached poll's counts by (answer, by, ours now), and shows it again.
    pub(super) fn change_votes(
        &mut self,
        channel: &str,
        message: &str,
        changes: &[(u64, i64, Option<bool>)],
        out: &mut Vec<Event>,
    ) {
        let updated = {
            let mut polls = self.polls.borrow_mut();
            let Some(raw) = polls.get_mut(message) else { return };
            let results = &mut raw["poll"]["results"];
            if !results.is_object() {
                *results = serde_json::json!({ "is_finalized": false, "answer_counts": [] });
            }
            if !results["answer_counts"].is_array() {
                results["answer_counts"] = serde_json::json!([]);
            }
            for (answer, by, ours) in changes {
                let Some(counts) = results["answer_counts"].as_array_mut() else {
                    break;
                };
                let at = counts.iter().position(|c| c["id"].as_u64() == Some(*answer));
                let at = match at {
                    Some(at) => at,
                    None => {
                        counts.push(serde_json::json!({ "id": answer, "count": 0, "me_voted": false }));
                        counts.len() - 1
                    }
                };
                let c = &mut counts[at];
                let count = c["count"].as_i64().unwrap_or(0).saturating_add(*by).max(0);
                c["count"] = count.into();
                if let Some(ours) = ours {
                    c["me_voted"] = (*ours).into();
                }
            }
            raw.clone()
        };
        if let (Some(room), Some(m)) = (self.room_id(channel), self.message(&updated)) {
            out.push(Event::MessageUpdated { room, message: m });
        }
    }

    pub(super) fn typing_event(&mut self, channel: &str, out: &mut Vec<Event>) {
        let Some(room) = self.room_id(channel) else { return };
        let guild = self.store.channels.get(channel).and_then(|c| c.guild.clone());
        let now = Instant::now();
        let names = self
            .typing
            .get_mut(channel)
            .map(|m| {
                m.retain(|_, since| now.duration_since(*since) < TYPING_FOR);
                m.keys()
                    .map(|u| Arc::from(self.store.display_name(u, guild.as_deref())))
                    .collect()
            })
            .unwrap_or_default();
        out.push(Event::Typing { room, names });
    }

    pub(super) fn alert(&self, m: &Value, mentioned: bool, out: &mut Vec<Event>) {
        let channel = m["channel_id"].as_str().unwrap_or_default();
        let Some(c) = self.store.channels.get(channel) else {
            return;
        };
        let muted =
            self.store.channel_muted(channel) || c.guild.as_ref().is_some_and(|g| self.store.muted_guilds.contains(g));
        // The server's notification level: all messages, mentions only, or
        // nothing (DMs always notify unless muted).
        let level = if c.guild.is_some() {
            self.store.channel_notify_level(channel)
        } else {
            0
        };
        let wanted = match level {
            0 => !muted || mentioned,
            1 => mentioned,
            _ => false,
        };
        // The chat on screen is skipped by the app, only while its window
        // is focused (as the official client): not here, by selection.
        if (c.private() && muted && !mentioned) || (!c.private() && !wanted) {
            return;
        }
        let Some(room) = self.room_id(channel) else { return };
        let author = m["author"]["id"].as_str().unwrap_or_default();
        let names = self.names(c.guild.as_deref());
        let body: String = crate::markdown::to_plain(m["content"].as_str().unwrap_or_default(), &names);
        let body = if body.chars().count() > ALERT_CHARS {
            body.chars().take(ALERT_CHARS).collect::<String>() + "…"
        } else {
            body
        };
        let title = match c.kind {
            store::DM => self.store.display_name(author, None),
            _ => format!(
                "{} ({})",
                self.store.display_name(author, c.guild.as_deref()),
                if c.name.is_empty() { "group" } else { &c.name }
            ),
        };
        out.push(Event::Alert {
            from: (!author.is_empty())
                .then(|| self.account.clone().map(|a| UserId::new(a, author)))
                .flatten(),
            room,
            kind: AlertKind::Message,
            title: title.into(),
            body: body.into(),
        });
    }

    // Profiles.

    /// Keeps a shown message's reactions, for live changes to build on.
    pub(super) fn remember_reactions(&mut self, channel: &str, message: &Message) {
        // A fixed budget: past it, the oldest are forgotten all at once
        // (their next change waits for the message to load again).
        const KEPT: usize = 4000;
        if self.reactions.len() >= KEPT {
            self.reactions.clear();
        }
        self.reactions.insert(
            message.id.0.to_string(),
            (channel.to_owned(), message.reactions.clone()),
        );
    }

    /// A reaction added or removed: the message's list, changed, sent again.
    pub(super) fn reaction_event(&mut self, event: &str, d: &Value, out: &mut Vec<Event>) {
        let channel = d["channel_id"].as_str().unwrap_or_default().to_owned();
        let (Some(room), Some(id)) = (self.room_id(&channel), d["message_id"].as_str()) else {
            return;
        };
        let mine = d["user_id"].as_str() == Some(self.me());
        let key = d.get("emoji").map(emoji_key).unwrap_or_default();
        let entry = self
            .reactions
            .entry(id.to_owned())
            .or_insert_with(|| (channel, Vec::new()));
        let list = &mut entry.1;
        match event {
            "MESSAGE_REACTION_ADD" => match list.iter_mut().find(|r| *r.emoji == *key) {
                Some(r) => {
                    r.count += 1;
                    r.me |= mine;
                }
                None => list.push(Reaction {
                    emoji: key.as_str().into(),
                    image_url: d["emoji"]["id"]
                        .as_str()
                        .map(|e| store::emoji_url(e, d["emoji"]["animated"] == true).into()),
                    count: 1,
                    me: mine,
                }),
            },
            "MESSAGE_REACTION_REMOVE" => {
                if let Some(r) = list.iter_mut().find(|r| *r.emoji == *key) {
                    r.count = r.count.saturating_sub(1);
                    if mine {
                        r.me = false;
                    }
                }
                list.retain(|r| r.count > 0);
            }
            "MESSAGE_REACTION_REMOVE_EMOJI" => list.retain(|r| *r.emoji != *key),
            _ => list.clear(),
        }
        out.push(Event::ReactionsChanged {
            room,
            id: MessageId(id.into()),
            reactions: list.clone(),
        });
    }

    pub(super) fn channel_of(&self, room: &RoomId) -> Option<String> {
        (self.account.as_ref() == Some(&room.account)).then(|| room.raw.to_string())
    }
}
