// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Messages: mapping, typing and new ones.

use super::*;
use std::sync::Arc;

impl SlackAccount {
    /// A Slack reaction name as Godwit shows it: the emoji itself, or a
    /// workspace emoji's canonical `:name:` and picture.
    pub(super) fn reaction_emoji(&self, team: &str, name: &str) -> (Arc<str>, Option<Arc<str>>) {
        let custom = self
            .workspaces
            .get(team)
            .and_then(|w| w.emoji.as_object())
            .and_then(|map| mrkdwn::custom_emoji(name, map));
        (
            custom.map_or_else(
                || mrkdwn::shortcode(name).unwrap_or_else(|| format!(":{name}:")).into(),
                |(canonical, _)| canonical.into(),
            ),
            custom
                .map(|(_, url)| url)
                .filter(|u| u.starts_with("https://"))
                .map(Into::into),
        )
    }

    /// A message as Godwit shows it, in `channel`'s timeline or (`in_thread`)
    /// in one of its threads.
    pub(super) fn message(&mut self, team: &str, channel: &str, m: &Value, in_thread: bool) -> Option<Message> {
        let ts = m["ts"].as_str()?.to_owned();
        let subtype = m["subtype"].as_str().unwrap_or_default();
        // Apps and integrations post with a `bot_id` (and often a bot user):
        // they're named by the message itself, else by `bots.info`. Named
        // by their bot user alone, they showed as "Someone".
        let bot = m["bot_id"].as_str().unwrap_or_default().to_owned();
        let from_app = !bot.is_empty() || m["bot_profile"].is_object() || subtype == "bot_message";
        let user = match m["user"].as_str().unwrap_or_default() {
            "" => bot.clone(),
            u => u.to_owned(),
        };
        let own_name = m["username"]
            .as_str()
            .or_else(|| m["bot_profile"]["name"].as_str())
            .filter(|n| from_app && !n.is_empty());
        if !from_app {
            self.follow(team, [user.clone()]);
        }
        if own_name.is_none() && !user.is_empty() && !self.names.contains_key(&format!("{team}:{user}")) {
            self.want_names(team, [user.clone()]);
        }
        let author_name = own_name.map_or_else(|| self.name(team, &user), str::to_owned);
        let own_icon = [
            m["bot_profile"]["icons"]["image_72"].as_str(),
            m["bot_profile"]["icons"]["image_48"].as_str(),
            m["icons"]["image_72"].as_str(),
            m["icons"]["image_48"].as_str(),
        ]
        .into_iter()
        .flatten()
        .find_map(crate::api::picture)
        .filter(|_| from_app);
        let system = matches!(
            subtype,
            "channel_join"
                | "channel_leave"
                | "channel_topic"
                | "channel_purpose"
                | "channel_name"
                | "group_join"
                | "group_leave"
        );
        let names = Names { account: self, team };
        let source = mrkdwn::message_source(m);
        let mut converted = mrkdwn::convert(&source, &names);
        // A channel post with replies: noted as a thread, and linked to it.
        let is_channel = self
            .workspaces
            .get(team)
            .and_then(|w| w.conversations.get(channel))
            .is_some();
        let replies = m["reply_count"].as_u64().unwrap_or(0);
        if is_channel && !in_thread && replies > 0 && m["thread_ts"].as_str() == Some(ts.as_str()) {
            let latest = m["latest_reply"].as_str().unwrap_or_default().to_owned();
            self.put_thread(team, channel, &ts, Some(thread_name(&converted.plain)), &latest, false);
            let label = if replies == 1 {
                "💬 1 reply".to_owned()
            } else {
                format!("💬 {replies} replies")
            };
            converted.rich.push_str(&format!(
                "\n\n[{}](godwit://room/{team}:{channel}:{ts})",
                godwit_core::rich::escape(&label)
            ));
        }
        let attachments = crate::media::attachments(m, &self.media, team);
        let me = self.workspaces.get(team).map(|w| w.me.clone()).unwrap_or_default();
        let reactions = m["reactions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                let (emoji, image_url) = self.reaction_emoji(team, r["name"].as_str()?);
                Some(Reaction {
                    emoji,
                    image_url,
                    count: r["count"].as_u64().and_then(|c| u32::try_from(c).ok()).unwrap_or(1),
                    me: r["users"]
                        .as_array()
                        .is_some_and(|u| u.iter().any(|u| u.as_str() == Some(me.as_str()))),
                })
            })
            .collect();
        // In a thread, replies are just its messages; elsewhere (broadcasts,
        // DMs' threads) they point at their post.
        let thread = m["thread_ts"]
            .as_str()
            .filter(|t| *t != ts && !in_thread)
            .map(|t| MessageId(t.into()));
        Some(Message {
            id: MessageId(ts.as_str().into()),
            author: self.user_id(team, &user)?,
            author_name: author_name.into(),
            author_avatar: own_icon
                .map(Into::into)
                .or_else(|| self.pictures.get(&format!("{team}:{user}")).map(|p| p.as_str().into())),
            author_colour: None,
            sent_ms: ts_ms(&ts),
            edited: m["edited"].is_object(),
            system,
            // System lines ("… has joined the channel") are plain text, so
            // their plain form: the rich one's link markup showed as is.
            text: if system {
                converted.plain.as_str().into()
            } else {
                converted.rich.into()
            },
            markup: if system { Markup::Plain } else { Markup::Rich },
            source: source.as_str().into(),
            reply_to: thread,
            jumbo: Vec::new(),
            attachments,
            components: None,
            embeds: Vec::new(),
            invites: Vec::new(),
            reactions,
            pending: false,
            snippet: (!system).then(|| converted.plain.into()),
            pinned: false,
            forwardable: false,
            poll_endable: false,
        })
    }

    pub(super) fn typing_event(&mut self, team: &str, channel: &str) -> Option<Event> {
        let key = format!("{team}:{channel}");
        let now = std::time::Instant::now();
        if let Some(people) = self.typing.get_mut(&key) {
            people.retain(|(_, at)| now.saturating_duration_since(*at) < godwit_core::application::TYPING_FOR);
        }
        let names = self
            .typing
            .get(&key)
            .map(|people| people.iter().map(|(p, _)| self.name(team, p).into()).collect())
            .unwrap_or_default();
        Some(Event::Typing {
            room: self.room_id(team, channel)?,
            names,
        })
    }

    /// A new message pushed live: into its conversation, or its thread.
    pub(super) fn new_message(&mut self, team: &str, channel: &str, e: &Value, out: &mut Vec<Event>) {
        let Some(ts) = e["ts"].as_str().filter(|ts| !ts.is_empty()) else {
            return;
        };
        if let Some(ws) = self.workspaces.get_mut(team) {
            let key = (channel.to_owned(), ts.to_owned());
            if !ws.seen.insert(key.clone()) {
                return;
            }
            ws.seen_order.push_back(key);
            while ws.seen_order.len() > 4096 {
                if let Some(key) = ws.seen_order.pop_front() {
                    ws.seen.remove(&key);
                }
            }
        }
        let user = e["user"].as_str().unwrap_or_default().to_owned();
        let key = format!("{team}:{channel}");
        if let Some(people) = self.typing.get_mut(&key)
            && people.iter().any(|(p, _)| *p == user)
        {
            people.retain(|(p, _)| *p != user);
            out.extend(self.typing_event(team, channel));
        }
        let known = self
            .workspaces
            .get(team)
            .is_some_and(|w| w.conversations.contains_key(channel));
        if !known {
            self.refresh_conversations(team);
        }
        let me = self.workspaces.get(team).map(|w| w.me.clone()).unwrap_or_default();
        let (room, in_thread) = self.place(team, channel, e);
        let Some(room) = room else { return };
        let ts = e["ts"].as_str().unwrap_or_default().to_owned();
        if in_thread {
            let root = e["thread_ts"].as_str().unwrap_or_default().to_owned();
            self.put_thread(team, channel, &root, None, &ts, user != me);
        }
        let Some(message) = self.message(team, channel, e, in_thread) else {
            return;
        };
        if !in_thread
            && let Some(c) = self
                .workspaces
                .get_mut(team)
                .and_then(|w| w.conversations.get_mut(channel))
        {
            if newer(&ts, &c.latest) {
                c.latest = ts.clone();
            }
            if user == me {
                c.last_read = ts.clone();
                c.has_unreads = false;
            } else {
                c.has_unreads = true;
            }
        }
        if !self.confirmed.remove(&ts) {
            let ws = self.workspaces.get(team);
            let text = e["text"].as_str().unwrap_or_default();
            let personal = !me.is_empty()
                && text
                    .split("<@")
                    .skip(1)
                    .filter_map(|part| part.split_once('>'))
                    .any(|(id, _)| id.split('|').next() == Some(me.as_str()));
            // Slack encodes user-group mentions as <!subteam^ID>; the
            // usergroups.list membership is documented at
            // https://api.slack.com/methods/usergroups.list.
            let group = ws.is_some_and(|w| {
                text.split("<!subteam^")
                    .skip(1)
                    .filter_map(|part| part.split_once('>'))
                    .any(|(id, _)| w.usergroups.contains(id))
            });
            // Slack's broadcast tokens notify channel members; user-group
            // mentions are described at https://api.slack.com/docs/attachments.
            let broadcast = ["<!here", "<!channel", "<!everyone"].iter().any(|b| text.contains(b));
            let keyword = ws.is_some_and(|w| {
                !w.keywords.is_empty() && {
                    let lower = text.to_lowercase();
                    w.keywords.iter().any(|k| lower.contains(k.as_str()))
                }
            });
            // The channel's level, else the workspace's; Slack's default is
            // mentions (and keywords) only.
            let level = ws
                .and_then(|w| w.channel_levels.get(channel).or(Some(&w.desktop_level)))
                .map_or("", String::as_str);
            let wanted = match level {
                "everything" | "all" => true,
                "nothing" | "none" => false,
                _ => personal || group || broadcast || keyword,
            };
            let followed = e["thread_ts"].as_str().is_some_and(|root| {
                ws.and_then(|w| w.threads.get(&(channel.to_owned(), root.to_owned())))
                    .is_some_and(|t| t.subscribed)
            });
            let channel_message = ws
                .and_then(|w| w.conversations.get(channel))
                .is_some_and(|c| c.kind == Kind::Channel);
            let direct_thread = in_thread
                && ws
                    .and_then(|w| w.conversations.get(channel))
                    .is_some_and(|c| c.kind != Kind::Channel);
            if (channel_message || direct_thread)
                && user != me
                && !message.system
                && (wanted || followed || direct_thread)
                && !ws.is_some_and(|w| w.muted.contains(channel))
            {
                let title = format!(
                    "{} · #{}",
                    message.author_name,
                    ws.and_then(|w| w.conversations.get(channel))
                        .map_or(channel, |c| c.name.as_str())
                );
                let body: String = message
                    .snippet
                    .as_deref()
                    .unwrap_or(&message.source)
                    .chars()
                    .take(200)
                    .collect();
                out.push(Event::Alert {
                    room: room.clone(),
                    kind: godwit_core::application::AlertKind::Message,
                    from: Some(message.author.clone()),
                    title: title.into(),
                    body: body.into(),
                });
            }
            out.push(Event::MessageAdded { room, message });
        }
        if in_thread {
            // Its thread moved up (or appeared): the list again.
            self.sync_rooms(out);
        } else if let Some(r) = self.room(team, channel) {
            out.push(Event::RoomUpdated(r));
        }
    }
}
