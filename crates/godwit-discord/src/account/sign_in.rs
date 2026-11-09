// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Signing in, demo mode, privacy and voice settings.

use super::*;

impl DiscordAccount {
    /// A made-up account for demo mode: signed in from `ready` (a READY
    /// payload), with `history` (messages by channel, newest first, as
    /// Discord pages them). It runs this bridge's real conversions, and
    /// answers what would go to Discord itself, as Discord's events would.
    pub fn demo(ready: Value, history: HashMap<String, Vec<Value>>) -> (Self, Vec<Event>) {
        let (net, _) = Handle::detached();
        let (_, events) = mpsc::channel();
        let mut account = Self::with(u32::MAX, net, events, Status::Connecting);
        account.demo = Some(history);
        let mut out = Vec::new();
        account.ready(&ready, &mut out);
        (account, out)
    }

    /// A made-up profile answer: the first servers we have and the first
    /// friends as mutuals, and a note (as the profile request and the note
    /// request would bring).
    fn demo_profile(&mut self, user: &str) {
        if user == self.me() || self.profiles.contains_key(user) {
            return;
        }
        let guilds: Vec<Value> = self
            .store
            .guild_order
            .iter()
            .take(2)
            .map(|g| json!({"id": g, "nick": null}))
            .collect();
        let mut friends: Vec<&String> = self
            .store
            .relationships
            .iter()
            .filter(|(u, k)| **k == store::FRIEND && u.as_str() != user)
            .map(|(u, _)| u)
            .collect();
        friends.sort();
        let friends: Vec<Value> = friends
            .into_iter()
            .take(2)
            .filter_map(|u| {
                self.store
                    .users
                    .get(u.as_str())
                    .map(|x| json!({"id": u, "name": x.username}))
            })
            .collect();
        self.profiles.insert(
            user.to_owned(),
            json!({"user": {"id": user}, "mutual_guilds": guilds, "mutual_friends": friends}),
        );
        self.notes
            .entry(user.to_owned())
            .or_insert_with(|| "Met in the demo.".to_owned());
    }

    /// What demo mode answers for a command; None for anything Discord
    /// would be asked (which then goes nowhere).
    pub(super) fn demo_answer(&mut self, command: &Command) -> Option<Vec<Event>> {
        let history = self.demo.as_ref()?;
        let mut out = Vec::new();
        match command {
            Command::LoadWebhooks { .. } | Command::ChangeWebhook { .. } => return Some(Vec::new()),
            Command::LoadHistory { room, before } => {
                let channel = room.raw.to_string();
                let page: Vec<Value> = history
                    .get(&channel)
                    .into_iter()
                    .flatten()
                    .filter(|m| {
                        before
                            .as_ref()
                            .is_none_or(|b| m["id"].as_str().is_some_and(|id| store::newer(&b.0, id)))
                    })
                    .take(crate::api::PAGE)
                    .cloned()
                    .collect();
                self.answer(
                    Answer::History {
                        channel,
                        before: before.as_ref().map(|b| b.0.to_string()),
                        around: None,
                        result: Ok(page),
                    },
                    &mut out,
                );
            }
            // The open posts come from the store, as when signed in.
            Command::LoadForum { room, offset: None, .. } => {
                if let Some(channel) = self.channel_of(room) {
                    self.forum_open(room, &channel, &mut out);
                }
            }
            // A forum's older posts answer from the canned archive (thread
            // objects under "archived:<forum>", with the first message's text
            // in `demo_first`), paged as Discord's thread search does.
            Command::LoadForum {
                room,
                sort,
                tag,
                offset: Some(offset),
            } => {
                let channel = room.raw.to_string();
                let mut list: Vec<Value> = history
                    .get(&format!("archived:{channel}"))
                    .into_iter()
                    .flatten()
                    .filter(|t| {
                        tag.as_ref().is_none_or(|tag| {
                            t["applied_tags"]
                                .as_array()
                                .is_some_and(|a| a.iter().any(|x| x == &**tag))
                        })
                    })
                    .cloned()
                    .collect();
                let key = |t: &Value| {
                    match sort {
                        godwit_core::forum::ForumSort::CreationDate => t["id"].as_str().map(str::to_owned),
                        godwit_core::forum::ForumSort::LatestActivity => {
                            t["last_message_id"].as_str().map(str::to_owned)
                        }
                    }
                    .unwrap_or_default()
                };
                list.sort_by_key(|t| std::cmp::Reverse((key(t).len(), key(t))));
                let total = list.len();
                let page: Vec<Value> = list.into_iter().skip(*offset).take(crate::forum::THREAD_PAGE).collect();
                let first: Vec<Value> = page
                    .iter()
                    .map(|t| json!({"channel_id": t["id"], "content": t["demo_first"]}))
                    .collect();
                let has_more = offset + page.len() < total;
                self.answer(
                    Answer::ForumThreads {
                        channel,
                        sort: *sort,
                        tag: tag.as_ref().map(|t| t.to_string()),
                        offset: *offset,
                        result: Ok(json!({"threads": page, "first_messages": first, "has_more": has_more, "total_results": total})),
                    },
                    &mut out,
                );
            }
            // A search answers from the demo history, newest first, as
            // Discord's pages do (25 groups, `total_results`).
            Command::SearchMessages { room, query, offset } => {
                let Some((guild, channel, params)) = self.search_request(room, query, *offset) else {
                    return Some(out);
                };
                let needle = params.content.to_lowercase();
                let mut found: Vec<Value> = history
                    .iter()
                    .filter(|(ch, _)| match (&params.channel_id, &guild) {
                        (Some(only), _) => *ch == only,
                        (None, Some(g)) => {
                            self.store.channels.get(ch.as_str()).and_then(|c| c.guild.as_ref()) == Some(g)
                        }
                        (None, None) => **ch == channel,
                    })
                    .flat_map(|(ch, list)| {
                        list.iter().map(move |m| {
                            let mut m = m.clone();
                            m["channel_id"] = json!(ch);
                            m
                        })
                    })
                    .filter(|m| {
                        m["content"]
                            .as_str()
                            .is_some_and(|c| c.to_lowercase().contains(&needle))
                            && params
                                .author_id
                                .as_deref()
                                .is_none_or(|a| m["author"]["id"].as_str() == Some(a))
                            && (!params.pinned || m["pinned"] == true)
                    })
                    .collect();
                found.sort_by_key(|m| std::cmp::Reverse(m["id"].as_str().and_then(|i| i.parse::<u64>().ok())));
                let total = found.len();
                let groups: Vec<Value> = found
                    .into_iter()
                    .skip(*offset)
                    .take(crate::search::PAGE)
                    .map(|m| json!([m]))
                    .collect();
                let result = json!({"total_results": total, "messages": groups});
                self.answer(
                    Answer::Search {
                        channel,
                        query: Box::new(query.clone()),
                        offset: *offset,
                        result: Ok(result),
                    },
                    &mut out,
                );
            }
            Command::SendMessage {
                room,
                text,
                nonce,
                reply_to,
                ..
            } => {
                let id = crate::api::nonce();
                self.pending.insert(id.clone(), nonce.clone());
                let me = self.store.me.clone().unwrap_or_default();
                let mut m = json!({
                    "id": id, "nonce": id, "channel_id": room.raw.as_ref(), "content": text, "type": if reply_to.is_some() { 19 } else { 0 },
                    "author": {"id": me.id, "username": me.username, "global_name": me.global_name, "avatar": me.avatar},
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                });
                if let Some(r) = reply_to {
                    m["message_reference"] = json!({"message_id": r.0.as_ref()});
                }
                if let Some(g) = self.store.channels.get(room.raw.as_ref()).and_then(|c| c.guild.clone()) {
                    m["guild_id"] = json!(g);
                }
                self.dispatch("MESSAGE_CREATE", &m, &mut out);
            }
            // The sticker arrives as the gateway would send it: no content,
            // `sticker_items` (docs.discord.food, Message: Sticker Item).
            Command::SendSticker {
                room,
                sticker,
                reply_to,
            } => {
                let me = self.store.me.clone().unwrap_or_default();
                let found = self
                    .store
                    .channels
                    .get(room.raw.as_ref())
                    .and_then(|c| c.guild.as_ref())
                    .and_then(|g| self.store.guilds.get(g))
                    .and_then(|g| g.stickers.iter().find(|s| *s.id == **sticker))
                    .cloned();
                let Some(found) = found else {
                    return Some(out);
                };
                let mut m = json!({
                    "id": crate::api::nonce(), "channel_id": room.raw.as_ref(), "content": "", "type": if reply_to.is_some() { 19 } else { 0 },
                    "author": {"id": me.id, "username": me.username, "global_name": me.global_name, "avatar": me.avatar},
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                    "sticker_items": [{"id": found.id, "name": found.name, "format_type": found.format}],
                });
                if let Some(r) = reply_to {
                    m["message_reference"] = json!({"message_id": r.0.as_ref()});
                }
                if let Some(g) = self.store.channels.get(room.raw.as_ref()).and_then(|c| c.guild.clone()) {
                    m["guild_id"] = json!(g);
                }
                self.dispatch("MESSAGE_CREATE", &m, &mut out);
            }
            // The forward arrives as the gateway would send it: a message with
            // no content of its own, carrying a snapshot of the original.
            Command::ForwardMessage {
                room,
                message,
                to,
                comment,
            } => {
                let original = history
                    .get(room.raw.as_ref())
                    .into_iter()
                    .flatten()
                    .find(|m| m["id"].as_str() == Some(message.0.as_ref()))
                    .cloned();
                let Some(original) = original else {
                    return Some(out);
                };
                let me = self.store.me.clone().unwrap_or_default();
                let author =
                    json!({"id": me.id, "username": me.username, "global_name": me.global_name, "avatar": me.avatar});
                let to_guild = self.store.channels.get(to.raw.as_ref()).and_then(|c| c.guild.clone());
                let mut copy = json!({"id": crate::api::nonce(), "channel_id": to.raw.as_ref(), "content": "", "type": 0, "author": author, "timestamp": chrono::Utc::now().to_rfc3339(),
                "flags": 1 << 14,
                "message_reference": {"type": 1, "message_id": message.0.as_ref(), "channel_id": room.raw.as_ref()},
                "message_snapshots": [{"message": {
                    "type": 0,
                    "content": original["content"],
                    "timestamp": original["timestamp"],
                    "attachments": original["attachments"],
                    "embeds": original["embeds"],
                    "flags": 0,
                }}]});
                if let Some(g) = &to_guild {
                    copy["guild_id"] = json!(g);
                }
                self.dispatch("MESSAGE_CREATE", &copy, &mut out);
                if !comment.is_empty() {
                    let mut note = json!({"id": crate::api::nonce(), "channel_id": to.raw.as_ref(), "content": comment, "type": 0, "author": author, "timestamp": chrono::Utc::now().to_rfc3339()});
                    if let Some(g) = &to_guild {
                        note["guild_id"] = json!(g);
                    }
                    self.dispatch("MESSAGE_CREATE", &note, &mut out);
                }
            }
            // A poll arrives as the gateway would send it: type 0 with a poll
            // object (Discord numbers the answers from 1).
            Command::CreatePoll {
                room,
                question,
                answers,
                hours,
                multi,
            } => {
                let me = self.store.me.clone().unwrap_or_default();
                let now = chrono::Utc::now();
                let mut m = json!({
                    "id": crate::api::nonce(), "channel_id": room.raw.as_ref(), "content": "", "type": 0,
                    "author": {"id": me.id, "username": me.username, "global_name": me.global_name, "avatar": me.avatar},
                    "timestamp": now.to_rfc3339(),
                    "poll": {
                        "question": {"text": question},
                        "answers": answers.iter().enumerate()
                            .map(|(i, a)| json!({"answer_id": i + 1, "poll_media": {"text": a}}))
                            .collect::<Vec<_>>(),
                        "expiry": (now + chrono::Duration::hours(i64::from(*hours))).to_rfc3339(),
                        "allow_multiselect": multi,
                        "layout_type": 1,
                        "results": {"is_finalized": false, "answer_counts": []},
                    },
                });
                if let Some(g) = self.store.channels.get(room.raw.as_ref()).and_then(|c| c.guild.clone()) {
                    m["guild_id"] = json!(g);
                }
                self.dispatch("MESSAGE_CREATE", &m, &mut out);
            }
            // Ending it: the message comes back updated, finalized.
            Command::EndPoll { message, .. } => {
                let mut m = self.polls.borrow().get(message.0.as_ref()).cloned()?;
                m["poll"]["expiry"] = json!(chrono::Utc::now().to_rfc3339());
                m["poll"]["results"]["is_finalized"] = json!(true);
                self.dispatch("MESSAGE_UPDATE", &m, &mut out);
            }
            Command::React {
                room,
                message,
                emoji,
                add,
            } => {
                let (name, id) = match emoji.rsplit_once(':') {
                    Some((n, i)) => (n.to_owned(), Some(i.to_owned())),
                    None => (emoji.to_string(), None),
                };
                let event = if *add {
                    "MESSAGE_REACTION_ADD"
                } else {
                    "MESSAGE_REACTION_REMOVE"
                };
                let d = json!({"channel_id": room.raw.as_ref(), "message_id": message.0.as_ref(), "user_id": self.me(), "emoji": {"name": name, "id": id}});
                self.dispatch(event, &d, &mut out);
            }
            Command::DeleteMessage { room, message } => {
                self.dispatch(
                    "MESSAGE_DELETE",
                    &json!({"channel_id": room.raw.as_ref(), "id": message.0.as_ref()}),
                    &mut out,
                );
            }
            Command::EditMessage { room, message, text } => {
                let names = self.names(
                    self.store
                        .channels
                        .get(room.raw.as_ref())
                        .and_then(|c| c.guild.as_deref()),
                );
                let rich = crate::markdown::to_rich(text, &names);
                out.push(Event::MessageEdited {
                    room: room.clone(),
                    id: message.clone(),
                    text: rich.into(),
                    markup: Markup::Rich,
                    source: text.as_str().into(),
                });
            }
            Command::LoadProfile { user, room } => {
                let guild = room
                    .as_ref()
                    .and_then(|r| self.store.channels.get(r.raw.as_ref()))
                    .and_then(|c| c.guild.clone());
                self.profile_for = Some((user.raw.to_string(), guild));
                self.demo_profile(user.raw.as_ref());
                self.sync_profile(&mut out);
            }
            // What the gateway would say after the PUT.
            Command::SetNote { user, note } if user.raw.as_ref() != self.me() => {
                let d = json!({"id": user.raw.as_ref(), "note": note});
                self.dispatch("USER_NOTE_UPDATE", &d, &mut out);
            }
            // What the gateway would say after the request, as the real account sees it.
            Command::BlockUser { user } if user.raw.as_ref() != self.me() => {
                let d = json!({"id": user.raw.as_ref(), "type": store::BLOCKED});
                self.dispatch("RELATIONSHIP_ADD", &d, &mut out);
            }
            Command::UnblockUser { user } if user.raw.as_ref() != self.me() => {
                self.dispatch("RELATIONSHIP_REMOVE", &json!({"id": user.raw.as_ref()}), &mut out);
            }
            Command::IgnoreUser { user } if user.raw.as_ref() != self.me() => {
                self.dispatch(
                    "RELATIONSHIP_UPDATE",
                    &json!({"id": user.raw.as_ref(), "user_ignored": true}),
                    &mut out,
                );
            }
            Command::UnignoreUser { user } if user.raw.as_ref() != self.me() => {
                self.dispatch(
                    "RELATIONSHIP_UPDATE",
                    &json!({"id": user.raw.as_ref(), "user_ignored": false}),
                    &mut out,
                );
            }
            // Member changes answer with the GUILD_MEMBER_UPDATE Discord would send.
            Command::ServerAdmin { server, op } if op.member_target().is_some() => {
                use godwit_core::server_settings::ServerAdmin as A;
                let guild = server.raw.to_string();
                let user = match self.member_change(&guild, op) {
                    Ok(Some(user)) => user,
                    Ok(None) => return None,
                    Err(e) => {
                        out.push(Event::ServerAdmin {
                            server: server.clone(),
                            result: Err(e.into()),
                        });
                        return Some(out);
                    }
                };
                let current = self.store.members.get(&(guild.clone(), user.clone()))?;
                let (mut roles, mut nick, mut until) = (current.roles.clone(), current.nick.clone(), None);
                match op {
                    A::SetMemberRole { role, on, .. } => {
                        roles.retain(|r| r.as_str() != role.as_ref());
                        if *on {
                            roles.push(role.to_string());
                        }
                    }
                    A::Timeout { duration, .. } => {
                        until = duration.map(|d| (chrono::Utc::now() + d).to_rfc3339());
                    }
                    A::SetServerNickname { nick: n, .. } => {
                        nick = n.as_ref().map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
                    }
                    _ => {}
                }
                // A timeout survives other changes, as on the real member.
                if !matches!(op, A::Timeout { .. }) {
                    until = current
                        .timeout_until
                        .and_then(|t| chrono::DateTime::from_timestamp(t, 0).map(|t| t.to_rfc3339()));
                }
                let username = self
                    .store
                    .users
                    .get(&user)
                    .map(|u| u.username.clone())
                    .unwrap_or_default();
                let d = json!({
                    "guild_id": guild, "user": {"id": user, "username": username},
                    "roles": roles, "nick": nick, "communication_disabled_until": until,
                });
                self.dispatch("GUILD_MEMBER_UPDATE", &d, &mut out);
                out.push(Event::ServerAdmin {
                    server: server.clone(),
                    result: Ok(ServerAdminResult::Done("Done.".into())),
                });
            }
            // Channel settings, creating and moving: answered as Discord's gateway would.
            Command::LoadChannelOverview { .. }
            | Command::SaveChannelOverview { .. }
            | Command::SetChannelOverride { .. }
            | Command::DeleteChannel { .. } => return Some(self.channel_command(command.clone())),
            Command::ServerAdmin { server, op }
                if matches!(
                    op,
                    godwit_core::server_settings::ServerAdmin::CreateChannel { .. }
                        | godwit_core::server_settings::ServerAdmin::MoveChannel { .. }
                        | godwit_core::server_settings::ServerAdmin::LoadEmoji
                        | godwit_core::server_settings::ServerAdmin::CreateEmoji { .. }
                        | godwit_core::server_settings::ServerAdmin::RenameEmoji { .. }
                        | godwit_core::server_settings::ServerAdmin::DeleteEmoji(_)
                ) =>
            {
                return Some(self.server_admin(server.clone(), op.clone()));
            }
            Command::LoadInvite { code } => {
                // Every demo invite leads to the demo's server.
                let account = self.account.clone()?;
                let guild = self.store.guild_order.first().and_then(|g| self.store.guilds.get(g));
                out.push(Event::InviteLoaded {
                    account: account.clone(),
                    code: code.clone(),
                    preview: guild.map(|g| InvitePreview {
                        server: Some(ServerId::new(account, g.id.as_str())),
                        name: g.name.as_str().into(),
                        icon_url: None,
                        members: Some(u64::try_from(self.store.members.len()).unwrap_or_default()),
                    }),
                });
            }
            // Nothing to connect to: the made-up account places no calls.
            Command::JoinCall { room } => out.push(Event::CallEnded {
                room: room.clone(),
                reason: Some("Calls don't connect in demo mode.".into()),
            }),
            // The change is applied as the gateway's settings update would.
            Command::SetRoomNotify { room, change } => out.extend(self.set_room_notify(room, *change)),
            Command::Typing { .. } | Command::MarkRead { .. } | Command::MarkUnread { .. } | Command::Focus { .. } => {
                return None;
            }
            // The chunk Discord's gateway would answer with: a few more members.
            Command::SearchMembers { room, query } => {
                let (query, room) = (query.clone(), room.clone());
                self.demo_member_search(&room, &query, &mut out);
            }
            Command::LoadMembers { .. }
            | Command::LoadServerOverview { .. }
            | Command::OpenDirect { .. }
            | Command::AddGroupMember { .. }
            | Command::RemoveGroupMember { .. } => {
                return None;
            }
            // As the gateway's CHANNEL_DELETE after leaving would.
            Command::LeaveGroup { room } => {
                self.store.channels.remove(&*room.raw);
                out.push(Event::RoomRemoved(room.clone()));
            }
            Command::ServerAdmin { op, .. } if op.is_load() => return None,
            // Nothing else is sent anywhere in the demo.
            _ => {}
        }
        Some(out)
    }

    /// Starts signing in by QR code.
    pub fn link(&mut self) {
        self.renewals = 0;
        self.status = Status::Qr {
            url: None,
            scanned: None,
            captcha: None,
        };
        self.net.send(Cmd::Link);
    }

    /// The first-run privacy choice was accepted: turn off Discord's data use
    /// for personalisation and usage statistics, and stop sharing activities.
    /// Two requests, once, because the user asked.
    pub fn tighten_privacy(&mut self) {
        if self.account.is_some() && self.demo.is_none() {
            self.net.send(Cmd::Request(Request::TightenPrivacy));
        }
    }

    /// New settings apply to calls from now on (and to the volumes of one under way).
    pub fn set_voice_settings(&mut self, settings: VoiceSettings) {
        if self.call.is_some() && settings != self.voice_settings {
            self.net
                .send(Cmd::CallCmd(CallCmd::Settings(Box::new(settings.clone()))));
        }
        self.voice_settings = settings;
    }

    /// Gives up signing in.
    pub fn cancel_link(&mut self) {
        if matches!(self.status, Status::Qr { .. }) {
            self.status = Status::SignedOut { error: None };
        }
    }
}
