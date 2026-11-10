// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Messages: mapping, polls, typing, alerts and reactions.

use super::*;

/// Discord and its web client mark attachment spoilers with this prefix
/// (docs.discord.food, Attachment Object; stoatchat/for-web de5379b).
fn is_spoiler_filename(filename: &str) -> bool {
    filename.starts_with("SPOILER_")
}

/// The message flag for a voice message (docs.discord.food, Message Object,
/// Message Flags: IS_VOICE_MESSAGE, 1 << 13).
const IS_VOICE_MESSAGE: u64 = 1 << 13;

/// A voice message's length and loudness curve: its attachment's
/// `duration_secs` and base64 `waveform` (docs.discord.food, Attachment
/// Object). None unless the message carries the voice-message flag.
fn voice_clip(flags: u64, attachment: &serde_json::Value) -> Option<VoiceClip> {
    if flags & IS_VOICE_MESSAGE == 0 {
        return None;
    }
    let secs = attachment["duration_secs"]
        .as_f64()
        .filter(|s| s.is_finite() && *s >= 0.0)?;
    let waveform = attachment["waveform"]
        .as_str()
        .and_then(|w| base64::Engine::decode(&base64::engine::general_purpose::STANDARD, w).ok())
        .unwrap_or_default();
    Some(VoiceClip {
        duration_ms: (secs * 1000.0).min(f64::from(u32::MAX)) as u32,
        waveform,
    })
}

#[cfg(test)]
mod voice_tests {
    use super::voice_clip;
    use serde_json::json;

    #[test]
    fn maps_flagged_attachment_with_waveform() {
        let a = json!({"duration_secs": 2.5, "waveform": "AAH/"});
        let clip = voice_clip(1 << 13, &a).unwrap();
        assert_eq!(clip.duration_ms, 2500);
        assert_eq!(clip.waveform, vec![0, 1, 255]);
    }

    #[test]
    fn needs_flag_and_duration_and_tolerates_bad_waveform() {
        let a = json!({"duration_secs": 1.0, "waveform": "!!"});
        assert!(voice_clip(0, &a).is_none());
        assert!(voice_clip(1 << 13, &json!({})).is_none());
        assert!(voice_clip(1 << 13, &a).unwrap().waveform.is_empty());
    }
}

#[cfg(test)]
mod spoiler_tests {
    use super::is_spoiler_filename;

    #[test]
    fn detects_spoiler_filename_prefix() {
        assert!(is_spoiler_filename("SPOILER_image.png"));
        assert!(!is_spoiler_filename("image_SPOILER.png"));
        assert!(!is_spoiler_filename("spoiler_image.png"));
    }
}

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
        let components = parse_message_components(&m["components"]);
        if components.is_some() {
            self.component_contexts.borrow_mut().insert(
                id.to_owned(),
                json!({
                    "application_id": m["application_id"],
                    "guild_id": m["guild_id"],
                    "flags": m["flags"].as_u64().unwrap_or_default(),
                }),
            );
        } else {
            self.component_contexts.borrow_mut().remove(id);
        }
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
                let name = a["filename"].as_str().unwrap_or("file");
                let flags = body["flags"].as_u64().unwrap_or_default();
                Some(Attachment {
                    name: name.into(),
                    url: a["url"].as_str()?.into(),
                    kind: if kind == AttachmentKind::File && flags & IS_VOICE_MESSAGE != 0 {
                        AttachmentKind::Audio
                    } else {
                        kind
                    },
                    size: a["size"].as_u64().unwrap_or_default(),
                    dimensions: size("width").zip(size("height")),
                    original_url: None,
                    spoiler: is_spoiler_filename(name),
                    voice: voice_clip(flags, a),
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
            components: components.map(Box::new),
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
            forwardable: forwardable(m),
            // Judged when the message is read: a poll that runs out while shown
            // keeps the row until Discord's finalizing MESSAGE_UPDATE (no timer);
            // a click in between gets Discord's refusal as a notice.
            poll_endable: forwarded.is_none()
                && self.store.me.as_ref().is_some_and(|me| me.id == author)
                && poll_open(&m["poll"], chrono::Utc::now()),
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
        // Message `mention_everyone`, `mention_roles`, and `mentions` carry
        // the alert categories (docs.discord.food, [Message](https://docs.discord.food/resources/message#message-object));
        // the allowed-mentions docs group @everyone and @here together.
        let mention_alert = c.guild.as_ref().is_some_and(|g| {
            let everyone = m["mention_everyone"] == true;
            let me = self.store.me.as_ref().map(|u| u.id.as_str());
            let direct = m["mentions"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|u| u["id"].as_str() == me);
            let my_roles = me.and_then(|user| self.store.members.get(&(g.clone(), user.to_owned())));
            let role_mention = m["mention_roles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .any(|role| my_roles.is_some_and(|member| member.roles.iter().any(|id| id == role)));
            direct
                || (everyone && !self.store.suppress_everyone.contains(g))
                || (role_mention && !self.store.suppress_roles.contains(g))
                || (!everyone && !m["mention_roles"].as_array().is_some_and(|roles| !roles.is_empty()) && mentioned)
        }) || (c.guild.is_none() && mentioned);
        let wanted = match level {
            0 => !muted || mention_alert,
            1 => mention_alert,
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

    /// Discord's own limit on a message: 2,000 characters (Godwit never
    /// offers Nitro features, so not Nitro's 4,000).
    pub(super) fn message_limit(&self) -> usize {
        2000
    }

    pub(super) fn channel_of(&self, room: &RoomId) -> Option<String> {
        (self.account.as_ref() == Some(&room.account)).then(|| room.raw.to_string())
    }
}

fn parse_message_components(value: &Value) -> Option<godwit_core::message::MessageComponents> {
    use godwit_core::message::{
        MessageButton, MessageButtonStyle as Style, MessageComponent, MessageComponentRow, MessageComponents,
        MessageSelect, MessageSelectOption,
    };
    // Discord's Message Components spec assigns these row, button and select types;
    // skipping unknown types keeps newer controls from hiding known ones.
    // https://discord.com/developers/docs/interactions/message-components
    let mut rows = Vec::new();
    for row in value.as_array()? {
        if row["type"].as_u64() != Some(1) {
            continue;
        }
        let mut items = Vec::new();
        for item in row["components"].as_array().into_iter().flatten() {
            let text = |key: &str| item[key].as_str().map(Into::into);
            let emoji = || item["emoji"]["name"].as_str().map(Into::into);
            match item["type"].as_u64() {
                Some(2) => {
                    let style = match item["style"].as_u64().unwrap_or(2) {
                        1 => Style::Primary,
                        3 => Style::Success,
                        4 => Style::Danger,
                        5 => Style::Link,
                        _ => Style::Secondary,
                    };
                    items.push(MessageComponent::Button(MessageButton {
                        label: text("label"),
                        emoji: emoji(),
                        style,
                        url: text("url"),
                        disabled: item["disabled"].as_bool().unwrap_or(false),
                        custom_id: text("custom_id"),
                    }));
                }
                Some(kind @ (3 | 5 | 6 | 7 | 8)) => {
                    let options = if kind == 3 {
                        item["options"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|option| {
                                Some(MessageSelectOption {
                                    label: option["label"].as_str()?.into(),
                                    description: option["description"].as_str().map(Into::into),
                                    emoji: option["emoji"]["name"].as_str().map(Into::into),
                                })
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    items.push(MessageComponent::Select(MessageSelect {
                        placeholder: text("placeholder"),
                        options,
                        disabled: item["disabled"].as_bool().unwrap_or(false),
                    }));
                }
                _ => {}
            }
        }
        if !items.is_empty() {
            rows.push(MessageComponentRow { items });
        }
    }
    (!rows.is_empty()).then_some(MessageComponents { rows })
}

#[cfg(test)]
mod component_tests {
    use super::*;

    #[test]
    fn parses_supported_component_types_and_skips_unknowns() {
        let parsed = parse_message_components(&json!([
            {"type": 1, "components": [
                {"type": 2, "style": 1, "label": "Go", "custom_id": "go", "emoji": {"name": "✅"}},
                {"type": 2, "style": 5, "label": "Site", "url": "https://example.org"},
                {"type": 99, "label": "future"}
            ]},
            {"type": 1, "components": [
                {"type": 3, "placeholder": "Pick", "options": [{"label": "One", "description": "First", "emoji": {"name": "1️⃣"}}]},
                {"type": 5, "placeholder": "Upload"},
                {"type": 6, "placeholder": "User"},
                {"type": 7, "placeholder": "Role"},
                {"type": 8, "placeholder": "Mentionable"},
                {"type": 100}
            ]},
            {"type": 42, "components": [{"type": 2, "label": "ignored row"}]}
        ])).expect("known components");
        assert_eq!(parsed.rows.len(), 2);
        assert_eq!(parsed.rows[0].items.len(), 2);
        assert_eq!(parsed.rows[1].items.len(), 5);
        let godwit_core::message::MessageComponent::Button(button) = &parsed.rows[0].items[0] else {
            panic!("button")
        };
        assert_eq!(button.custom_id.as_deref(), Some("go"));
        assert_eq!(button.emoji.as_deref(), Some("✅"));
        let godwit_core::message::MessageComponent::Select(select) = &parsed.rows[1].items[0] else {
            panic!("select")
        };
        assert_eq!(select.options[0].description.as_deref(), Some("First"));
    }
}

/// Whether a poll can still be ended: not finalized, and not past its expiry
/// (a poll Discord hasn't finalized yet when the clock passes it can't be
/// ended either).
fn poll_open(poll: &Value, now: chrono::DateTime<chrono::Utc>) -> bool {
    poll.is_object()
        && poll["results"]["is_finalized"].as_bool() != Some(true)
        && poll["expiry"]
            .as_str()
            .and_then(|e| chrono::DateTime::parse_from_rfc3339(e).ok())
            .is_none_or(|e| e > now)
}

/// Whether a message may be forwarded: docs.discord.food (Message Reference
/// Types) takes only default and reply messages, with no polls, calls or
/// components.
fn forwardable(m: &Value) -> bool {
    matches!(m["type"].as_u64().unwrap_or_default(), 0 | 19)
        && !m["poll"].is_object()
        && !m["call"].is_object()
        && m["components"].as_array().is_none_or(Vec::is_empty)
}

#[cfg(test)]
mod forward_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn open_polls_can_be_ended() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
            .unwrap()
            .to_utc();
        let poll = |expiry: &str, done: bool| json!({"expiry": expiry, "results": {"is_finalized": done}});
        assert!(poll_open(&poll("2026-01-03T00:00:00Z", false), now));
        assert!(!poll_open(&poll("2026-01-01T00:00:00Z", false), now));
        assert!(!poll_open(&poll("2026-01-03T00:00:00Z", true), now));
        assert!(!poll_open(&Value::Null, now));
    }

    #[test]
    fn only_plain_messages_can_be_forwarded() {
        assert!(forwardable(&json!({"type": 0, "components": []})));
        assert!(forwardable(&json!({"type": 19})));
        for no in [
            json!({"type": 7}),
            json!({"type": 20}),
            json!({"type": 0, "poll": {"question": {}}}),
            json!({"type": 3, "call": {}}),
            json!({"type": 0, "components": [{"type": 1}]}),
        ] {
            assert!(!forwardable(&no), "{no}");
        }
    }
}
