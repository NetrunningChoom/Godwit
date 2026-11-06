// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Backend events and the ready state.

use super::*;

impl TeamsAccount {
    pub(super) fn news(&mut self, n: &Value, out: &mut Vec<Event>) {
        let resource = &n["resource"];
        match n["resourceType"].as_str().unwrap_or_default() {
            "Presence" => self.answer(Answer::Presence(crate::api::presences(&n["resource"])), out),
            "PresenceError" => out.push(Event::Error(match n["reason"].as_str() {
                Some(reason) => format!("Teams live presence subscription failed: {reason}").into(),
                None => "Teams live presence subscription failed".into(),
            })),
            "NewMessage" => self.new_message(resource, out),
            "MessageUpdate" => {
                let Some((conversation, root)) = conversation_of(resource) else {
                    return;
                };
                // A thread reply lives in its own room (when we know it):
                // its edits and deletes must update that timeline, not just
                // the channel's. A root post shows in both, so both hear
                // about it.
                let id = resource["id"].as_str().map(str::to_owned);
                let is_root = root.as_ref().is_some_and(|r| id.as_deref() == Some(r.as_str()));
                let thread = root
                    .as_ref()
                    .map(|r| format!("{conversation};messageid={r}"))
                    .filter(|id| self.conversations.contains_key(id));
                let mut targets: Vec<String> = Vec::new();
                if let Some(thread) = thread {
                    targets.push(thread);
                }
                if is_root || root.is_none() {
                    targets.push(conversation.clone());
                }
                for target in targets {
                    let Some(room) = self.room_id(&target) else {
                        continue;
                    };
                    if is_set(&resource["properties"]["deletetime"]) {
                        if let Some(id) = resource["id"].as_str() {
                            out.push(Event::MessageDeleted {
                                room,
                                id: MessageId(id.into()),
                            });
                        }
                    } else if let Some(message) = self.message(resource).inspect(|_| self.ask_unnamed()) {
                        out.push(Event::MessageUpdated { room, message });
                    }
                }
            }
            "ConversationUpdate" | "ThreadUpdate" => {
                let known = resource["id"]
                    .as_str()
                    .is_some_and(|id| self.conversations.contains_key(&split_thread(id).0));
                if n["resourceType"] == "ConversationUpdate" || !known {
                    self.put_conversation(resource);
                } else if let (Some(id), Some(topic)) =
                    (resource["id"].as_str(), resource["properties"]["topic"].as_str())
                    && let Some(c) = self.conversations.get_mut(&split_thread(id).0)
                {
                    c.topic = topic.to_owned();
                }
                if let Some(id) = resource["id"].as_str()
                    && let Some(r) = self.room(&split_thread(id).0)
                {
                    out.push(Event::RoomUpdated(r));
                }
            }
            _ => {}
        }
    }

    pub(super) fn ready(&mut self, me: &Value, conversations: &Value, out: &mut Vec<Event>) {
        let Some(skype) = me["skypeName"].as_str().filter(|s| !s.is_empty()) else {
            self.status = Status::SignedOut {
                error: Some("Teams didn't say who you are.".into()),
            };
            return;
        };
        self.me = if skype.starts_with("8:") {
            skype.to_owned()
        } else {
            format!("8:{skype}")
        };
        // `userDetails` is a JSON string: {"name": …, "upn": …}.
        let details: Value = match &me["userDetails"] {
            Value::String(s) => serde_json::from_str(s).unwrap_or_default(),
            v => v.clone(),
        };
        let upn = details["upn"].as_str().unwrap_or_default();
        self.my_name = details["name"]
            .as_str()
            .filter(|n| !n.is_empty() && *n != skype)
            .or(Some(upn).filter(|u| !u.is_empty()))
            .unwrap_or("Me")
            .to_owned();
        self.put_conversations(conversations);
        // The organisation's people, once per sign-in.
        self.net.send(Cmd::Request(Request::Directory));
        // Our DM partners' statuses now (a snapshot; refreshed as chats open).
        let partners: Vec<String> = self
            .conversations
            .values()
            .filter_map(|c| match &c.kind {
                Kind::Direct(with) => Some(with.clone()),
                _ => None,
            })
            .chain([self.me.clone()])
            .collect();
        self.net.send(Cmd::Request(Request::Presence(partners)));
        // Without a name in the profile, ask for it as for anyone's.
        if self.my_name == "Me" {
            let me = self.me.clone();
            self.want_names(std::slice::from_ref(&me));
        }
        let account = AccountId::new(Network::Teams, self.me.as_str());
        self.account = Some(account.clone());
        self.status = Status::Ready;
        out.push(Event::AccountReady {
            account,
            display_name: self.my_name.as_str().into(),
            username: if upn.is_empty() {
                self.my_name.as_str().into()
            } else {
                upn.into()
            },
            avatar_url: self.picture(&self.me).map(Into::into),
            rooms: {
                let rooms = self.all_rooms();
                self.room_sync.reset(rooms.clone());
                rooms
            },
            servers: self.servers(),
        });
        out.extend(self.own_state());
    }

    pub(super) fn handle(&mut self, ev: Ev, out: &mut Vec<Event>) {
        match ev {
            Ev::DeviceCode { user_code, url } => {
                self.status = Status::Code { code: user_code, url };
            }
            Ev::LoginFailed(error) => self.status = Status::SignedOut { error: Some(error) },
            Ev::Media { media, region } => {
                self.media = Some(media);
                self.media_region = region;
            }
            Ev::SignedIn(saved) => {
                if self.persist {
                    session::save(self.slot, &saved);
                }
                if !matches!(self.status, Status::Ready) {
                    self.status = Status::Connecting;
                }
            }
            Ev::Ready { me, conversations } => self.ready(&me, &conversations, out),
            Ev::Conversations(v) => {
                self.put_conversations(&v);
                self.sync_rooms(out);
            }
            Ev::Teams(v) => {
                self.put_user_state(&v);
                self.sync_rooms(out);
            }
            Ev::News(n) => self.news(&n, out),
            Ev::Online(online) => {
                if let Some(account) = self.account.clone() {
                    out.push(Event::Connection { account, online });
                }
            }
            Ev::Lost { reason, signed_out } => {
                if signed_out {
                    if let Some(media) = &self.media {
                        media.clear();
                    }
                    if self.persist {
                        session::clear(self.slot);
                    }
                    self.status = Status::SignedOut {
                        error: Some(format!("Teams no longer accepts this sign-in: {reason} Sign in again.")),
                    };
                    if let Some(account) = self.account.take() {
                        out.push(Event::AccountLost {
                            account,
                            reason: reason.into(),
                        });
                    }
                } else if let Some(account) = self.account.clone() {
                    out.push(Event::Error(format!("Teams: {reason}").into()));
                    out.push(Event::Connection { account, online: false });
                } else if self.status == Status::Connecting {
                    // Not connected yet and not signed out: the network
                    // thread keeps trying (`keep_starting`), so this stays
                    // "Connecting…" instead of asking to sign in again.
                    eprintln!("note: Teams can't connect yet ({reason}); trying again");
                } else {
                    self.status = Status::SignedOut { error: Some(reason) };
                }
            }
            Ev::Answer(answer) => self.answer(answer, out),
            Ev::LoggedOut => {
                if let Some(media) = &self.media {
                    media.clear();
                }
                if self.persist {
                    session::clear(self.slot);
                }
                self.status = Status::SignedOut { error: None };
                if let Some(account) = self.account.take() {
                    out.push(Event::AccountLost {
                        account,
                        reason: "".into(),
                    });
                }
            }
        }
    }
}
