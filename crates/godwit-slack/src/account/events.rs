// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Backend events and the ready state.

use super::*;

impl SlackAccount {
    pub(super) fn news(&mut self, team: &str, e: &Value, out: &mut Vec<Event>) {
        let channel = e["channel"].as_str().unwrap_or_default().to_owned();
        match e["type"].as_str().unwrap_or_default() {
            "message" => match e["subtype"].as_str().unwrap_or_default() {
                "message_changed" => {
                    let m = &e["message"];
                    let (room, in_thread) = self.place(team, &channel, m);
                    if let (Some(room), Some(message)) = (room, self.message(team, &channel, m, in_thread)) {
                        out.push(Event::MessageUpdated { room, message });
                    }
                }
                "message_deleted" => {
                    let (room, _) = self.place(team, &channel, &e["previous_message"]);
                    if let (Some(room), Some(ts)) = (room, e["deleted_ts"].as_str()) {
                        out.push(Event::MessageDeleted {
                            room,
                            id: MessageId(ts.into()),
                        });
                    }
                }
                // A post's reply count changed: its link says so.
                "message_replied" => {
                    let m = &e["message"];
                    if let (Some(room), Some(message)) =
                        (self.room_id(team, &channel), self.message(team, &channel, m, false))
                    {
                        out.push(Event::MessageUpdated { room, message });
                    }
                    let mut synced = Vec::new();
                    self.sync_rooms(&mut synced);
                    out.extend(synced);
                }
                "thread_broadcast_removed" => {}
                _ => self.new_message(team, &channel, e, out),
            },
            "thread_subscribed" | "thread_unsubscribed" | "thread_marked" => {
                let subscription = &e["subscription"];
                if let (Some(channel), Some(root)) =
                    (subscription["channel"].as_str(), subscription["thread_ts"].as_str())
                {
                    self.put_thread(team, channel, root, None, root, false);
                    if let Some(t) = self
                        .workspaces
                        .get_mut(team)
                        .and_then(|w| w.threads.get_mut(&(channel.to_owned(), root.to_owned())))
                    {
                        t.subscribed = e["type"] != "thread_unsubscribed" && subscription["active"] == true;
                        if let Some(read) = subscription["last_read"].as_str()
                            && newer(read, &t.last_read)
                        {
                            t.last_read = read.to_owned();
                        }
                        t.unread = newer(&t.latest, &t.last_read);
                    }
                    self.sync_rooms(out);
                    if e["type"] == "thread_subscribed" {
                        self.refresh_threads(team, false);
                    }
                }
            }
            "pref_change" if e["name"] == "all_notifications_prefs" || e["name"] == "muted_channels" => {
                if let (Some(name), Some(value)) = (e["name"].as_str(), e.get("value")) {
                    let mut prefs = self
                        .workspaces
                        .get(team)
                        .map_or_else(|| serde_json::json!({}), |ws| ws.notification_settings.clone());
                    if !prefs.is_object() {
                        prefs = serde_json::json!({});
                    }
                    prefs[name] = value.clone();
                    self.notification_prefs(team, &prefs);
                } else {
                    self.refresh_notification_prefs(team);
                }
            }
            // Followed people's presence (`presence_sub`): one, or a batch.
            "presence_change" => {
                let Some(account) = self.account.clone() else { return };
                let presence = match e["presence"].as_str() {
                    Some("active") => Presence::Online,
                    Some("away") => Presence::Idle,
                    Some(_) => Presence::Unknown,
                    None => return,
                };
                let users: Vec<&str> = match e["users"].as_array() {
                    Some(list) => list.iter().filter_map(Value::as_str).collect(),
                    None => e["user"].as_str().into_iter().collect(),
                };
                if users
                    .iter()
                    .any(|u| self.workspaces.get(team).is_some_and(|w| w.me == *u))
                {
                    self.own_presence = presence;
                    out.push(Event::OwnPresence {
                        account: account.clone(),
                        presence,
                        status: self.own_status.as_deref().map(Into::into),
                    });
                }
                let presence = users
                    .into_iter()
                    .map(|u| (UserId::new(account.clone(), format!("{team}:{u}").as_str()), presence))
                    .collect();
                out.push(Event::Presences { account, presence });
            }
            "user_typing" => {
                let user = e["user"].as_str().unwrap_or_default().to_owned();
                let people = self.typing.entry(format!("{team}:{channel}")).or_default();
                people.retain(|(p, _)| *p != user);
                people.push((user.clone(), std::time::Instant::now()));
                self.want_names(team, [user]);
                out.extend(self.typing_event(team, &channel));
            }
            // Read elsewhere (another device or the browser).
            "channel_marked" | "group_marked" | "im_marked" | "mpim_marked" => {
                if let Some(c) = self
                    .workspaces
                    .get_mut(team)
                    .and_then(|w| w.conversations.get_mut(&channel))
                {
                    if let Some(ts) = e["ts"].as_str() {
                        c.last_read = ts.to_owned();
                    }
                    c.has_unreads = e["unread_count_display"].as_u64().is_some_and(|n| n > 0);
                    c.mentions = e["mention_count_display"]
                        .as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .unwrap_or(0);
                }
                if let Some(r) = self.room(team, &channel) {
                    out.push(Event::RoomUpdated(r));
                }
            }
            "channel_joined" | "group_joined" | "im_created" | "mpim_joined" | "channel_rename" | "group_rename"
            | "channel_left" | "group_left" | "channel_archive" | "group_archive" => {
                self.refresh_conversations(team);
            }
            _ => {}
        }
    }

    pub(super) fn ready(
        &mut self,
        team: &str,
        me: &Value,
        conversations: &[Value],
        counts: &Value,
        out: &mut Vec<Event>,
    ) {
        let user = me["user_id"].as_str().unwrap_or_default().to_owned();
        let ws = self.workspaces.entry(team.to_owned()).or_default();
        ws.name = me["team"].as_str().unwrap_or("Slack").to_owned();
        ws.me = user.clone();
        if !self.teams.iter().any(|t| t == team) {
            self.teams.push(team.to_owned());
        }
        for c in conversations {
            self.put_conversation(team, c);
        }
        self.put_counts(team, counts);
        // The workspace's people for People, once per sign-in (paged and paced).
        self.request(team, Request::Directory);
        let dm_people: Vec<String> = self
            .workspaces
            .get(team)
            .map(|w| {
                w.conversations
                    .values()
                    .filter_map(|c| match &c.kind {
                        Kind::Direct(u) => Some(u.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.follow(team, dm_people.iter().cloned().chain([user.clone()]));
        self.want_names(team, dm_people.into_iter().chain([user.clone()]));
        if let Some(account) = self.account.clone() {
            // Another workspace of this sign-in.
            self.sync_rooms(out);
            out.push(Event::Connection { account, online: true });
            return;
        }
        let account = AccountId::new(Network::Slack, format!("{team}:{user}").as_str());
        self.account = Some(account.clone());
        self.my_name = me["user"].as_str().unwrap_or("Slack").to_owned();
        self.status = Status::Ready;
        out.push(Event::AccountReady {
            account,
            display_name: self.my_name.as_str().into(),
            username: format!("{}@{}", self.my_name, me["team"].as_str().unwrap_or("slack")).into(),
            avatar_url: None,
            rooms: {
                let rooms = self.all_rooms();
                self.room_sync.reset(rooms.clone());
                rooms
            },
            servers: self.servers(),
        });
    }

    pub(super) fn handle(&mut self, ev: Ev, out: &mut Vec<Event>) {
        match ev {
            Ev::Waiting => self.status = Status::Waiting,
            Ev::LoginFailed(error) => self.status = Status::SignedOut { error: Some(error) },
            Ev::SignedIn(saved) => {
                crate::media::configure(&self.media, &saved);
                if self.persist {
                    session::save(self.slot, &saved);
                }
                self.status = Status::Connecting;
            }
            Ev::Ready {
                team,
                me,
                conversations,
                counts,
                icon,
            } => {
                self.workspaces.entry(team.clone()).or_default().icon = icon;
                self.ready(&team, &me, &conversations, &counts, out);
            }
            Ev::News { team, event } => self.news(&team, &event, out),
            Ev::Online { team, online } => {
                // One workspace's line: the account is online while any is.
                if online {
                    self.refresh_threads(&team, false);
                    self.refresh_notification_prefs(&team);
                    self.request(&team, Request::OwnState);
                }
                if let Some(account) = self.account.clone() {
                    out.push(Event::Connection { account, online });
                }
            }
            Ev::Lost { reason, signed_out } => {
                if signed_out {
                    self.media.clear();
                    if self.persist {
                        session::clear(self.slot);
                    }
                    self.status = Status::SignedOut {
                        error: Some(format!("{reason}. Sign in again.")),
                    };
                    if let Some(account) = self.account.take() {
                        out.push(Event::AccountLost {
                            account,
                            reason: reason.into(),
                        });
                    }
                } else if self.account.is_some() {
                    out.push(Event::Error(format!("Slack: {reason}").into()));
                } else if self.status == Status::Connecting {
                    // Not connected yet and not signed out: the network
                    // thread keeps trying (`keep_starting`), so this stays
                    // "Connecting…" instead of asking to sign in again.
                    eprintln!("note: Slack can't connect yet ({reason}); trying again");
                } else {
                    self.status = Status::SignedOut { error: Some(reason) };
                }
            }
            Ev::Answer { team, answer } => self.answer(&team, answer, out),
            Ev::LoggedOut => {
                self.media.clear();
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
