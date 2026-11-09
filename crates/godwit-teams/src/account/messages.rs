// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Messages: mapping, mentions, typing and new ones.

use super::*;

impl TeamsAccount {
    /// Whether the message @-mentions us: `properties.mentions` is a JSON
    /// string of `{"mri": …}` objects (as msteams-mcp's client writes them).
    pub(super) fn mentions_me(&self, m: &Value) -> bool {
        html::mention_mris(&m["properties"]).iter().any(|mri| mri == &self.me)
    }

    pub(super) fn message_in(&mut self, conversation: &str, raw: &Value) -> Option<Message> {
        let mut message = self.message(raw)?;
        let (channel, thread) = split_thread(conversation);
        if let Some(root) = thread {
            if message.reply_to.as_ref().is_some_and(|id| *id.0 == root) {
                message.reply_to = None;
            }
        } else if self
            .conversations
            .get(&channel)
            .is_some_and(|c| matches!(c.kind, Kind::Channel(_)))
        {
            let root = raw["rootMessageId"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .or_else(|| conversation_of(raw).and_then(|(_, root)| root))
                .unwrap_or_else(|| message.id.0.to_string());
            if let Some(thread) = self.remember_thread(&channel, &root, Some(&message)) {
                message.text = format!(
                    "{}\n\n[Open thread](godwit://room/{})",
                    message.text,
                    crate::api::seg(&thread)
                )
                .into();
                message.markup = Markup::Rich;
                if root != *message.id.0 {
                    message.reply_to = Some(MessageId(root.into()));
                }
            }
        }
        Some(message)
    }

    /// A message as Godwit shows it; `None` for control messages (typing)
    /// and deleted ones.
    pub(super) fn message(&mut self, m: &Value) -> Option<Message> {
        let account = self.account.clone()?;
        let id = m["id"].as_str().filter(|s| !s.is_empty())?.to_owned();
        let kind = m["messagetype"].as_str().unwrap_or("Text");
        if kind.starts_with("Control/") {
            return None;
        }
        let props = &m["properties"];
        if is_set(&props["deletetime"]) {
            return None;
        }
        let author = m["from"].as_str().and_then(mri).unwrap_or_default();
        if let Some(name) = m["imdisplayname"].as_str().filter(|n| !n.is_empty())
            && !author.is_empty()
        {
            self.names.insert(author.clone(), name.to_owned());
        }
        // Without `imdisplayname` (older messages, some kinds) the name is
        // looked up; until it comes the message says "Someone", and the
        // Authors event renames it.
        if !author.is_empty() && !self.names.contains_key(&author) {
            self.unnamed.insert(author.clone());
        }
        let author_name = self.name(&author);
        let content = m["content"].as_str().unwrap_or_default();
        let (system, converted) = match kind {
            "Text" => (
                false,
                html::Converted {
                    rich: godwit_core::rich::escape(content),
                    plain: content.to_owned(),
                    reply_to: None,
                },
            ),
            k if k.starts_with("RichText") => (false, html::convert(content, &html::mentions(props))),
            k if k.starts_with("ThreadActivity/") => {
                let what = match &k["ThreadActivity/".len()..] {
                    "AddMember" => "added someone to the chat",
                    "DeleteMember" => "removed someone from the chat",
                    "MemberJoined" => "joined",
                    "MemberLeft" => "left",
                    "TopicUpdate" => "renamed the chat",
                    "PictureUpdate" => "changed the chat's picture",
                    _ => "changed the chat",
                };
                let text = format!(
                    "{} {what}",
                    if author.is_empty() {
                        "Someone".into()
                    } else {
                        author_name.clone()
                    }
                );
                (
                    true,
                    html::Converted {
                        rich: godwit_core::rich::escape(&text),
                        plain: text,
                        reply_to: None,
                    },
                )
            }
            k if k.starts_with("Event/Call") => {
                let text = "A call happened in Teams".to_owned();
                (
                    true,
                    html::Converted {
                        rich: godwit_core::rich::escape(&text),
                        plain: text,
                        reply_to: None,
                    },
                )
            }
            _ => {
                let text = "Godwit can't show this kind of Teams message yet".to_owned();
                (
                    true,
                    html::Converted {
                        rich: godwit_core::rich::escape(&text),
                        plain: text,
                        reply_to: None,
                    },
                )
            }
        };
        let attachments = crate::media::attachments(content, &json_list(&props["files"]), self.media.as_ref());
        let reactions = json_list(&props["emotions"])
            .iter()
            .filter_map(|e| {
                let key = e["key"].as_str().filter(|k| !k.is_empty())?;
                let users = e["users"].as_array()?;
                (!users.is_empty()).then(|| Reaction {
                    emoji: reaction_emoji(key).into(),
                    image_url: None,
                    count: u32::try_from(users.len()).unwrap_or(u32::MAX),
                    me: users
                        .iter()
                        .any(|u| u["mri"].as_str().and_then(mri).as_deref() == Some(self.me.as_str())),
                })
            })
            .collect();
        let thread_root = conversation_of(m)
            .and_then(|(_, root)| root)
            .or_else(|| m["rootMessageId"].as_str().map(str::to_owned))
            .filter(|r| *r != id);
        let sent_ms = iso_ms(&m["originalarrivaltime"])
            .or_else(|| id.parse().ok())
            .or_else(|| iso_ms(&m["composetime"]))
            .unwrap_or(0);
        Some(Message {
            id: MessageId(id.into()),
            author: UserId::new(account, author.as_str()),
            author_name: author_name.into(),
            author_avatar: self.picture(&author).map(Into::into),
            author_colour: None,
            sent_ms,
            edited: is_set(&m["skypeeditedid"]) || is_set(&props["edittime"]),
            system,
            text: converted.rich.into(),
            markup: if system { Markup::Plain } else { Markup::Rich },
            source: content.into(),
            reply_to: converted.reply_to.or(thread_root).map(|r| MessageId(r.into())),
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

    pub(super) fn typing_event(&mut self, conversation: &str) -> Option<Event> {
        let now = std::time::Instant::now();
        if let Some(people) = self.typing.get_mut(conversation) {
            people.retain(|(_, at)| now.saturating_duration_since(*at) < godwit_core::application::TYPING_FOR);
        }
        let names = self
            .typing
            .get(conversation)
            .map(|people| people.iter().map(|(p, _)| self.name(p).into()).collect())
            .unwrap_or_default();
        Some(Event::Typing {
            room: self.room_id(conversation)?,
            names,
        })
    }

    /// A message pushed over Trouter (or answered to a send).
    pub(super) fn new_message(&mut self, m: &Value, out: &mut Vec<Event>) {
        let Some((conversation, root)) = conversation_of(m) else {
            return;
        };
        let author = m["from"].as_str().and_then(mri).unwrap_or_default();
        let kind = m["messagetype"].as_str().unwrap_or_default();
        // Typing: shown until they stop or send.
        if kind == "Control/Typing" || kind == "Control/ClearTyping" {
            if author == self.me {
                return;
            }
            let people = self.typing.entry(conversation.clone()).or_default();
            people.retain(|(p, _)| *p != author);
            if kind == "Control/Typing" {
                people.push((author, std::time::Instant::now()));
            }
            out.extend(self.typing_event(&conversation));
            return;
        }
        if let Some(people) = self.typing.get_mut(&conversation)
            && people.iter().any(|(p, _)| *p == author)
        {
            people.retain(|(p, _)| *p != author);
            out.extend(self.typing_event(&conversation));
        }
        let client_id = m["clientmessageid"].as_str().unwrap_or_default().to_owned();
        let root = root.or_else(|| {
            m["rootMessageId"]
                .as_str()
                .filter(|id| Some(*id) != m["id"].as_str())
                .map(str::to_owned)
        });
        let route = root
            .as_deref()
            .and_then(|root| self.remember_thread(&conversation, root, None))
            .unwrap_or_else(|| conversation.clone());
        let Some(room) = self.room_id(&route) else {
            return;
        };
        let Some(message) = self.message_in(&route, m) else {
            return;
        };
        if let Some(root) = root.as_deref() {
            self.remember_thread(&conversation, root, Some(&message));
            if let Some(thread) = self.conversations.get_mut(&route) {
                thread.last = Some((message.id.0.to_string(), client_id.clone(), author.clone()));
            }
            if author == self.me {
                // We wrote there: its replies alert from now on.
                self.threads_mine.insert(route.clone());
            }
        }
        if route != conversation {
            self.sync_rooms(out);
        }
        self.ask_unnamed();
        let id = message.id.0.to_string();
        let unknown = !self.conversations.contains_key(&conversation);
        let conv = self
            .conversations
            .entry(conversation.clone())
            .or_insert_with(|| Conversation {
                id: conversation.clone(),
                ..Conversation::default()
            });
        conv.last_ms = conv.last_ms.max(message.sent_ms);
        conv.last = Some((id.clone(), client_id.clone(), author.clone()));
        conv.hidden = false;
        if author == self.me
            && let Ok(n) = id.parse()
        {
            // Our own message: everything before it is read.
            conv.read = conv.read.max(n);
        }
        if self.confirmed.remove(&id) {
            // Already shown from the send's answer.
        } else if let Some(nonce) = self.sending.remove(&client_id) {
            out.push(Event::MessageConfirmed {
                room,
                nonce: nonce.into(),
                message,
            });
        } else {
            // An unknown DM must exist before the shared notification observer
            // sees its first incoming message.
            if unknown {
                out.extend(self.room(&conversation).map(Event::RoomUpdated));
            }
            // Channel mentions and replies to threads we took part in alert
            // here: the app's fallback only covers direct and group chats.
            let mentioned = self.mentions_me(m) && author != self.me;
            let in_channel = self
                .conversations
                .get(&conversation)
                .is_some_and(|c| matches!(c.kind, Kind::Channel(_)));
            let thread_mine = route != conversation && self.threads_mine.contains(&route);
            if (mentioned || thread_mine) && author != self.me && self.own_presence != Presence::Focus {
                let title = if in_channel {
                    format!(
                        "{} · #{}",
                        message.author_name,
                        self.conversations
                            .get(&conversation)
                            .map_or(conversation.as_str(), |c| c.topic.as_str())
                    )
                } else {
                    message.author_name.to_string()
                };
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
        out.extend(self.room(&route).map(Event::RoomUpdated));
        if let Some(r) = self.room(&conversation) {
            out.push(Event::RoomUpdated(r));
        }
        // A chat we hadn't heard of: its details come with the list.
        if unknown {
            self.want_names(std::iter::once(&author));
            self.net.send(Cmd::Request(Request::Conversations));
        }
    }

    /// The text of a send still waiting for its answer.
    pub(super) fn pending_text(&mut self, _conversation: &str, client_id: &str) -> String {
        self.pending.remove(client_id).unwrap_or_default()
    }
}
