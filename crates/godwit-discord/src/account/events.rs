// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Gateway events: the backend's messages, READY and dispatch.

use super::*;

impl DiscordAccount {
    pub(super) fn parse_server_event(&self, guild: &str, value: &Value) -> Option<godwit_core::server::ServerEvent> {
        let id = value["id"].as_str()?;
        let start = value["scheduled_start_time"]
            .as_str()?
            .parse::<chrono::DateTime<chrono::Utc>>()
            .ok()?;
        let ends_at = value["scheduled_end_time"]
            .as_str()
            .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok())
            .map(|t| t.timestamp());
        let entity = value["entity_type"].as_i64().unwrap_or_default();
        let event_status = value["status"].as_i64().unwrap_or_default();
        // The official Events page lists only scheduled and active events (Discord Developer Documentation, Guild Scheduled Event resource).
        if !matches!(event_status, 1 | 2) {
            return None;
        }
        let channel = value["channel_id"]
            .as_str()
            .and_then(|c| self.store.channels.get(c))
            .map(|c| c.name.clone());
        let location = if entity == 3 {
            value["entity_metadata"]["location"].as_str().map(str::to_owned)
        } else {
            channel.or_else(|| match entity {
                1 => Some("Stage channel".to_owned()),
                2 => Some("Voice channel".to_owned()),
                _ => None,
            })
        };
        let interested = self
            .scheduled_event_membership
            .get(&(guild.to_owned(), id.to_owned()))
            .copied()
            .unwrap_or_else(|| {
                self.scheduled_events
                    .get(guild)
                    .and_then(|events| events.iter().find(|e| e.id.as_ref() == id))
                    .is_some_and(|e| e.interested)
            });
        let description = value["description"].as_str().filter(|s| !s.is_empty()).map(Into::into);
        let status = if event_status == 2 { "Active" } else { "Scheduled" };
        let cover_url = value["image"]
            .as_str()
            .map(|image| format!("https://cdn.discordapp.com/guild-events/{id}/{image}.png?size=256").into());
        Some(godwit_core::server::ServerEvent {
            id: id.into(),
            server: ServerId::new(self.account.clone()?, guild),
            name: value["name"].as_str()?.into(),
            description,
            starts_at: start.timestamp(),
            ends_at,
            location: location.map(Into::into),
            status: status.into(),
            interested_count: u32::try_from(value["user_count"].as_u64().unwrap_or_default()).unwrap_or(u32::MAX),
            interested,
            cover_url,
        })
    }

    pub(super) fn scheduled_events_changed(&mut self, guild: &str, out: &mut Vec<Event>) {
        self.sync_servers(out);
        let (Some(account), Some(events)) = (self.account.clone(), self.scheduled_events.get(guild)) else {
            return;
        };
        out.push(Event::ServerEvents {
            server: ServerId::new(account, guild),
            result: Ok(events.clone()),
            load_response: false,
        });
    }

    pub(super) fn handle(&mut self, ev: Ev, out: &mut Vec<Event>) {
        match ev {
            Ev::LinkStep(Step::Code(url)) => {
                self.status = Status::Qr {
                    url: Some(url),
                    scanned: None,
                    captcha: None,
                }
            }
            Ev::LinkStep(Step::Captcha(page)) => {
                if let Status::Qr { url, scanned, .. } = &self.status {
                    self.status = Status::Qr {
                        url: url.clone(),
                        scanned: scanned.clone(),
                        captcha: Some(page),
                    };
                }
            }
            Ev::LinkStep(Step::Scanned { name }) => {
                if let Status::Qr { url, .. } = &self.status {
                    self.status = Status::Qr {
                        url: url.clone(),
                        scanned: Some(name),
                        captcha: None,
                    };
                }
            }
            Ev::PasswordResult(Ok(answer)) => {
                if let Some(token) = answer["token"].as_str() {
                    self.finish_login(token.to_owned());
                } else if answer["mfa"] == true {
                    let ticket = answer["ticket"].as_str().unwrap_or_default().to_owned();
                    let (totp, sms, backup, webauthn) = (
                        answer["totp"] == true,
                        answer["sms"] == true,
                        answer["backup"] == true,
                        answer["webauthn"] == true,
                    );
                    if webauthn && !totp && !sms && !backup {
                        self.status = Status::SignedOut { error: Some("This account only allows a security key, which Godwit can't use yet. Sign in with the QR code.".into()) };
                    } else {
                        let hint = if totp {
                            "Enter the 6-digit code from your authenticator app, or an 8-digit backup code.".to_owned()
                        } else {
                            "Enter the code Discord texted to your phone.".to_owned()
                        };
                        self.status = Status::NeedsCode {
                            ticket: LoginTicket(ticket),
                            totp,
                            sms,
                            backup,
                            webauthn,
                            hint,
                            error: None,
                        };
                        if sms
                            && !totp
                            && !backup
                            && let Status::NeedsCode { ticket, .. } = &self.status
                        {
                            self.net.send(Cmd::SendLoginSms {
                                ticket: Secret(ticket.0.clone()),
                            });
                        }
                    }
                } else {
                    self.status = Status::SignedOut {
                        error: Some("Discord couldn't complete sign-in. Try again.".into()),
                    };
                }
            }
            Ev::LoginCaptcha(page) => {
                self.status = Status::Qr {
                    url: None,
                    scanned: None,
                    captcha: Some(page),
                };
            }
            Ev::PasswordResult(Err(error)) => self.status = Status::SignedOut { error: Some(error) },
            Ev::LoginCodeResult(Ok(answer)) => {
                if let Some(token) = answer["token"].as_str() {
                    self.finish_login(token.to_owned());
                }
            }
            Ev::LoginCodeResult(Err(error)) => {
                if let Status::NeedsCode { error: slot, .. } = &mut self.status {
                    *slot = Some(error);
                }
            }
            Ev::LoginSmsResult(Ok(answer)) => {
                if let Status::NeedsCode { hint, .. } = &mut self.status
                    && let Some(phone) = answer["phone"].as_str()
                {
                    *hint = format!("Enter the code Discord texted to {phone}.");
                }
            }
            Ev::LoginSmsResult(Err(error)) => {
                if let Status::NeedsCode { error: slot, .. } = &mut self.status {
                    *slot = Some(error);
                }
            }
            Ev::Linked(token) => self.finish_login(token),
            Ev::LinkFailed(error) => {
                const RENEWALS: u32 = 5;
                let expired = error == crate::remote_auth::Stopped::Expired.to_string();
                let unscanned = matches!(self.status, Status::Qr { scanned: None, .. });
                if unscanned && expired && self.renewals < RENEWALS {
                    self.renewals += 1;
                    self.status = Status::Qr {
                        url: None,
                        scanned: None,
                        captcha: None,
                    };
                    self.net.send(Cmd::Link);
                } else if matches!(self.status, Status::Qr { .. }) {
                    self.status = Status::SignedOut { error: Some(error) };
                }
            }
            Ev::Token(token) => {
                let user = self.me().to_owned();
                session::save(self.slot, &session::Saved { user, token });
            }
            Ev::Ready(d) => self.ready(&d, out),
            Ev::Dispatch(event, data) => self.dispatch(&event, &data, out),
            Ev::Connection(online) => {
                if let Some(account) = self.account.clone() {
                    out.push(Event::Connection { account, online });
                }
            }
            Ev::Lost { reason, signed_out } => {
                if signed_out {
                    session::clear(self.slot);
                    self.status = Status::SignedOut {
                        error: (!reason.is_empty()).then(|| reason.clone()),
                    };
                    if let Some(account) = self.account.take() {
                        out.push(Event::AccountLost {
                            account,
                            reason: reason.into(),
                        });
                    }
                } else {
                    out.push(Event::Error(format!("Discord: {reason}").into()));
                }
            }
            Ev::Answer(answer) => self.answer(answer, out),
            Ev::Call { call, event } => self.on_call(call, event, out),
            Ev::StepUpRequired { id, methods } => {
                use godwit_core::step_up::StepUpMethod as M;
                let Some(account) = self.account.clone() else { return };
                let methods = methods
                    .into_iter()
                    .filter_map(|m| match m {
                        crate::mfa::Method::Totp => Some(M::Authenticator),
                        crate::mfa::Method::Backup => Some(M::Recovery),
                        crate::mfa::Method::Password => Some(M::Password),
                        crate::mfa::Method::Webauthn | crate::mfa::Method::Sms => None,
                    })
                    .collect();
                out.push(Event::StepUpRequired { account, id, methods });
            }
            Ev::StepUpFinished { id, result } => {
                let Some(account) = self.account.clone() else { return };
                out.push(Event::StepUpFinished {
                    account,
                    id,
                    result: result.map_err(Into::into),
                });
            }
        }
    }

    fn finish_login(&mut self, token: String) {
        session::save(
            self.slot,
            &session::Saved {
                user: String::new(),
                token: token.clone(),
            },
        );
        self.net.send(Cmd::UseSession { token });
        self.status = Status::Connecting;
    }

    pub(super) fn ready(&mut self, d: &Value, out: &mut Vec<Event>) {
        self.session_id = d["session_id"].as_str().map(str::to_owned);
        self.store.ready(d);
        let Some(me) = self.store.me.clone() else { return };
        let account = AccountId::new(Network::Discord, me.id.as_str());
        // Demo mode keeps nothing.
        if self.demo.is_none()
            && let Some(saved) = session::load(self.slot)
            && saved.user != me.id
        {
            session::save(
                self.slot,
                &session::Saved {
                    user: me.id.clone(),
                    token: saved.token,
                },
            );
        }
        let first = self.account.is_none();
        self.account = Some(account.clone());
        self.status = Status::Ready;
        self.subscribed.clear();
        if first {
            self.rooms.clear();
            self.servers.clear();
            let rooms = self.all_rooms();
            self.rooms.reset(rooms.iter().cloned());
            self.servers = self.build_servers();
            out.push(Event::AccountReady {
                account: account.clone(),
                display_name: me.name().into(),
                username: me.username.as_str().into(),
                avatar_url: Some(store::avatar_url(&me.id, me.avatar.as_deref()).into()),
                rooms,
                servers: self.servers.clone(),
            });
        }
        self.sync_all(out);
        self.voice_ready(d, out);
        let presence = match self.store.status.as_str() {
            "idle" => Presence::Idle,
            "dnd" => Presence::Busy,
            "invisible" => Presence::Invisible,
            _ => Presence::Online,
        };
        if !self.store.prefs.is_empty() {
            out.push(Event::Preferences {
                account: account.clone(),
                prefs: self.store.prefs,
            });
        }
        out.push(Event::OwnPresence {
            account,
            presence,
            status: self.store.custom_status.clone().map(Into::into),
        });
        // Tell the gateway what the settings say, as each session does.
        self.net.send(Cmd::Gateway {
            op: crate::gateway::PRESENCE_UPDATE,
            data: crate::settings::presence(&self.store.status, self.store.custom_status.as_deref()),
        });
    }

    pub(super) fn dispatch(&mut self, event: &str, d: &Value, out: &mut Vec<Event>) {
        // A newly available guild must have rooms/names before its voice
        // occupants can be published against those rooms.
        if event != "GUILD_CREATE" {
            self.voice_dispatch(event, d, out);
        }
        let me = self.me().to_owned();
        match event {
            "GUILD_MEMBER_LIST_UPDATE" => self.member_list_update(d, out),
            "GUILD_MEMBERS_CHUNK" => self.members_chunk(d, out),
            "USER_SETTINGS_PROTO_UPDATE" => {
                // Type 1 is the preloaded settings (status, server list).
                if d["settings"]["type"].as_u64().unwrap_or(1) == 1
                    && let Some(proto) = d["settings"]["proto"].as_str()
                {
                    let prefs = self.store.apply_settings(proto);
                    if !prefs.is_empty()
                        && let Some(account) = self.account.clone()
                    {
                        out.push(Event::Preferences { account, prefs });
                    }
                    self.sync_servers(out);
                }
            }
            "READY_SUPPLEMENTAL" => {
                self.store.supplemental(d);
                self.sync_all(out);
                self.voice_supplemental(d, out);
            }
            "MESSAGE_CREATE" => {
                let mentioned = self.store.message_created(d);
                let channel = d["channel_id"].as_str().unwrap_or_default().to_owned();
                if let Some(t) = self.typing.get_mut(&channel)
                    && let Some(author) = d["author"]["id"].as_str()
                    && t.remove(author).is_some()
                {
                    self.typing_event(&channel, out);
                }
                if let (Some(room), Some(message)) = (self.room_id(&channel), self.message(d)) {
                    self.remember_reactions(&channel, &message);
                    let ours = d["nonce"].as_str().and_then(|n| self.pending.remove(n));
                    match ours {
                        Some(nonce) => out.push(Event::MessageConfirmed { room, nonce, message }),
                        None => out.push(Event::MessageAdded { room, message }),
                    }
                }
                if d["author"]["id"].as_str() != Some(me.as_str()) {
                    self.alert(d, mentioned, out);
                }
                self.sync_rooms(out);
            }
            "MESSAGE_UPDATE" => {
                let channel = d["channel_id"].as_str().unwrap_or_default();
                // Updates without content are embeds arriving (link previews
                // made after sending, a GIF from the picker among them): only
                // those change.
                if d.get("content").is_some()
                    && d.get("author").is_some()
                    && let (Some(room), Some(message)) = (self.room_id(channel), self.message(d))
                {
                    out.push(Event::MessageUpdated { room, message });
                } else if let (Some(room), Some(id)) = (self.room_id(channel), d["id"].as_str()) {
                    // MESSAGE_UPDATE may be partial
                    // (https://docs.discord.food/topics/gateway-events#message-update):
                    // without `author` the message can't be rebuilt, so the
                    // fields that came are applied on their own and the
                    // shown message keeps the rest.
                    let id = godwit_core::ids::MessageId(id.into());
                    if let Some(text) = d["content"].as_str() {
                        let guild = d["guild_id"]
                            .as_str()
                            .map(str::to_owned)
                            .or_else(|| self.store.channels.get(channel).and_then(|c| c.guild.clone()));
                        let names = self.names(guild.as_deref());
                        out.push(Event::MessageEdited {
                            room: room.clone(),
                            id: id.clone(),
                            text: crate::markdown::to_rich(text, &names).into(),
                            markup: Markup::Rich,
                            source: text.into(),
                        });
                    }
                    if let Some(embeds) = d["embeds"].as_array() {
                        out.push(Event::EmbedsUpdated {
                            room,
                            id,
                            embeds: embeds.iter().filter_map(super::embed).collect(),
                        });
                    }
                }
            }
            // Someone voted or took a vote back: the counts change.
            "MESSAGE_POLL_VOTE_ADD" | "MESSAGE_POLL_VOTE_REMOVE" => {
                if d["user_id"].as_str() != Some(me.as_str())
                    && let (Some(channel), Some(message), Some(answer)) = (
                        d["channel_id"].as_str(),
                        d["message_id"].as_str(),
                        d["answer_id"].as_u64(),
                    )
                {
                    let delta = if event == "MESSAGE_POLL_VOTE_ADD" { 1 } else { -1 };
                    self.change_votes(channel, message, &[(answer, delta, None)], out);
                }
            }
            "MESSAGE_DELETE" => {
                if let (Some(room), Some(id)) = (
                    self.room_id(d["channel_id"].as_str().unwrap_or_default()),
                    d["id"].as_str(),
                ) {
                    self.store
                        .forget_message(d["channel_id"].as_str().unwrap_or_default(), id);
                    out.push(Event::MessageDeleted {
                        room,
                        id: MessageId(id.into()),
                    });
                }
            }
            "MESSAGE_DELETE_BULK" => {
                if let Some(room) = self.room_id(d["channel_id"].as_str().unwrap_or_default()) {
                    for id in d["ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        self.store
                            .forget_message(d["channel_id"].as_str().unwrap_or_default(), id);
                        out.push(Event::MessageDeleted {
                            room: room.clone(),
                            id: MessageId(id.into()),
                        });
                    }
                }
            }
            "MESSAGE_ACK" => {
                self.store.message_ack(d);
                self.sync_rooms(out);
            }
            "TYPING_START" => {
                let (Some(channel), Some(user)) = (d["channel_id"].as_str(), d["user_id"].as_str()) else {
                    return;
                };
                if user == me {
                    return;
                }
                if let (Some(guild), Some(member)) = (d["guild_id"].as_str(), d.get("member")) {
                    self.store.put_member(guild, member);
                }
                self.typing
                    .entry(channel.to_owned())
                    .or_default()
                    .insert(user.to_owned(), Instant::now());
                self.typing_event(channel, out);
            }
            // Threads come and go as channels do (Discord sends them separately).
            "THREAD_CREATE" | "THREAD_UPDATE" => {
                self.store.put_channel(d);
                self.sync_rooms(out);
                self.sync_servers(out);
            }
            "THREAD_DELETE" => {
                if let Some(id) = d["id"].as_str() {
                    self.store.remove_channel(id);
                }
                self.sync_rooms(out);
                self.sync_servers(out);
            }
            // The active threads of channels we just looked at.
            "THREAD_LIST_SYNC" => {
                for t in d["threads"].as_array().into_iter().flatten() {
                    self.store.put_channel(t);
                }
                self.sync_rooms(out);
                self.sync_servers(out);
            }
            // Discord reports membership here (docs.discord.food, Gateway
            // Events: THREAD_MEMBER_UPDATE and THREAD_MEMBERS_UPDATE).
            "THREAD_MEMBER_UPDATE" => {
                let channel = d["id"].as_str().or(d["channel_id"].as_str());
                let me = self.me().to_owned();
                let is_me = d["user_id"].as_str() == Some(me.as_str());
                if is_me
                    && let Some(channel) = channel
                    && let Some(thread) = self.store.channels.get_mut(channel)
                {
                    thread.joined = !d["member"].is_null();
                    // The payload is the thread member object itself.
                    if let Some(flags) = d["flags"].as_u64().or(d["member"]["flags"].as_u64()) {
                        thread.thread_flags = Some(flags);
                    }
                    if d["muted"] == true {
                        let until = chrono::DateTime::parse_from_rfc3339(
                            d["mute_config"]["end_time"].as_str().unwrap_or_default(),
                        )
                        .ok()
                        .map(|t| t.timestamp());
                        self.store.muted_channels.insert(channel.to_owned(), until);
                    } else if d["muted"] == false {
                        self.store.muted_channels.remove(channel);
                    }
                    self.sync_rooms(out);
                }
            }
            "THREAD_MEMBERS_UPDATE" => {
                let Some(channel) = d["id"].as_str().or(d["channel_id"].as_str()) else {
                    return;
                };
                let me = self.me().to_owned();
                let added = d["added_members"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|m| m["user_id"].as_str() == Some(me.as_str()));
                let removed = d["removed_member_ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|u| u.as_str() == Some(me.as_str()));
                if (added || removed)
                    && let Some(thread) = self.store.channels.get_mut(channel)
                {
                    thread.joined = added;
                    self.sync_rooms(out);
                }
            }
            "CHANNEL_CREATE" | "CHANNEL_UPDATE" => {
                let id = d["id"].as_str().unwrap_or_default();
                let before = self.store.channels.get(id).map(|c| c.overwrites.clone());
                self.store.put_channel(d);
                // Someone changed its permissions: an open settings page shows them.
                if event == "CHANNEL_UPDATE"
                    && before.is_some_and(|b| self.store.channels.get(id).is_some_and(|c| c.overwrites != b))
                {
                    self.overrides_changed(id, out);
                }
                self.sync_rooms(out);
                self.sync_servers(out);
            }
            "CHANNEL_DELETE" => {
                if let Some(id) = d["id"].as_str() {
                    self.store.remove_channel(id);
                }
                self.sync_rooms(out);
                self.sync_servers(out);
            }
            "CHANNEL_RECIPIENT_ADD" | "CHANNEL_RECIPIENT_REMOVE" => {
                let (Some(c), Some(u)) = (d["channel_id"].as_str(), d["user"]["id"].as_str()) else {
                    return;
                };
                self.store.put_user(&d["user"]);
                if let Some(ch) = self.store.channels.get_mut(c) {
                    ch.recipients.retain(|r| r != u);
                    if event == "CHANNEL_RECIPIENT_ADD" {
                        ch.recipients.push(u.to_owned());
                    }
                }
                self.sync_rooms(out);
            }
            "GUILD_CREATE" => {
                self.store.put_guild(d);
                if let Some(id) = d["id"].as_str() {
                    for m in d["members"].as_array().into_iter().flatten() {
                        self.store.put_member(id, m);
                    }
                    if let Some(entries) = d["guild_scheduled_events"].as_array() {
                        let events = entries.iter().filter_map(|v| self.parse_server_event(id, v)).collect();
                        self.scheduled_events.insert(id.to_owned(), events);
                    }
                }
                self.sync_rooms(out);
                self.sync_servers(out);
                self.sync_emoji(out);
                self.sync_stickers(out);
                self.voice_dispatch(event, d, out);
                if let Some(id) = d["id"].as_str() {
                    self.scheduled_events_changed(id, out);
                }
            }
            // Discord Developer Documentation, Gateway Events: keep the live
            // scheduled-event cache current while the Events page is open.
            "GUILD_SCHEDULED_EVENT_CREATE" | "GUILD_SCHEDULED_EVENT_UPDATE" => {
                let Some(guild) = d["guild_id"].as_str() else { return };
                let Some(id) = d["id"].as_str() else { return };
                let Some(entry) = self.parse_server_event(guild, d) else {
                    if let Some(events) = self.scheduled_events.get_mut(guild) {
                        events.retain(|e| e.id.as_ref() != id);
                    }
                    self.scheduled_event_membership
                        .remove(&(guild.to_owned(), id.to_owned()));
                    self.scheduled_events_changed(guild, out);
                    return;
                };
                let events = self.scheduled_events.entry(guild.to_owned()).or_default();
                if let Some(old) = events.iter_mut().find(|e| e.id == entry.id) {
                    *old = entry;
                } else {
                    events.push(entry);
                }
                self.scheduled_events_changed(guild, out);
            }
            "GUILD_SCHEDULED_EVENT_DELETE" => {
                let (Some(guild), Some(id)) = (d["guild_id"].as_str(), d["id"].as_str()) else {
                    return;
                };
                if let Some(events) = self.scheduled_events.get_mut(guild) {
                    events.retain(|e| e.id.as_ref() != id);
                }
                self.scheduled_event_membership
                    .remove(&(guild.to_owned(), id.to_owned()));
                self.scheduled_events_changed(guild, out);
            }
            "GUILD_SCHEDULED_EVENT_USER_ADD" | "GUILD_SCHEDULED_EVENT_USER_REMOVE" => {
                let (Some(guild), Some(id)) = (d["guild_id"].as_str(), d["guild_scheduled_event_id"].as_str()) else {
                    return;
                };
                let is_me = d["user_id"].as_str() == Some(self.me());
                let add = event == "GUILD_SCHEDULED_EVENT_USER_ADD";
                if is_me {
                    self.scheduled_event_membership
                        .insert((guild.to_owned(), id.to_owned()), add);
                }
                if let Some(entry) = self
                    .scheduled_events
                    .get_mut(guild)
                    .and_then(|events| events.iter_mut().find(|e| e.id.as_ref() == id))
                {
                    entry.interested_count = if add {
                        entry.interested_count.saturating_add(1)
                    } else {
                        entry.interested_count.saturating_sub(1)
                    };
                    if is_me {
                        entry.interested = add;
                    }
                }
                self.scheduled_events_changed(guild, out);
            }
            "GUILD_EMOJIS_UPDATE" => {
                if let Some(guild) = d["guild_id"].as_str().and_then(|g| self.store.guilds.get_mut(g)) {
                    store::put_emojis(guild, &d["emojis"]);
                }
                self.sync_emoji(out);
                self.sync_emoji_page(out);
            }
            "GUILD_STICKERS_UPDATE" => {
                if let Some(guild) = d["guild_id"].as_str().and_then(|g| self.store.guilds.get_mut(g)) {
                    store::put_stickers(guild, &d["stickers"]);
                }
                self.sync_stickers(out);
            }
            "USER_GUILD_SETTINGS_UPDATE" => {
                if let Some(entries) = d["entries"].as_array() {
                    for entry in entries {
                        self.store.apply_guild_settings(entry);
                    }
                } else {
                    self.store.apply_guild_settings(d);
                }
                self.sync_rooms(out);
                self.sync_servers(out);
            }
            "GUILD_UPDATE" => {
                self.store.update_guild(d);
                self.sync_servers(out);
                self.sync_roles(out);
            }
            "GUILD_DELETE" => {
                // `unavailable` is an outage, not leaving.
                if d["unavailable"] != true
                    && let Some(id) = d["id"].as_str()
                {
                    self.scheduled_events.remove(id);
                    self.scheduled_event_membership.retain(|(guild, _), _| guild != id);
                    self.store.remove_guild(id);
                    self.sync_rooms(out);
                    self.sync_servers(out);
                    self.sync_emoji(out);
                    self.sync_stickers(out);
                }
            }
            "GUILD_ROLE_CREATE" | "GUILD_ROLE_UPDATE" => {
                if let Some(g) = d["guild_id"].as_str() {
                    self.store.put_role(g, &d["role"]);
                }
                self.sync_all(out);
            }
            "GUILD_ROLE_DELETE" => {
                if let (Some(g), Some(r)) = (d["guild_id"].as_str(), d["role_id"].as_str())
                    && let Some(guild) = self.store.guilds.get_mut(g)
                {
                    guild.roles.remove(r);
                }
                self.sync_all(out);
            }
            "GUILD_MEMBER_UPDATE" | "GUILD_MEMBER_ADD" => {
                if let Some(g) = d["guild_id"].as_str() {
                    self.store.put_member(g, d);
                }
                if d["user"]["id"].as_str() == Some(me.as_str()) {
                    self.sync_all(out);
                } else {
                    self.sync_profile(out);
                }
            }
            "GUILD_MEMBER_REMOVE" => {
                if let (Some(g), Some(u)) = (d["guild_id"].as_str(), d["user"]["id"].as_str()) {
                    self.store.members.remove(&(g.to_owned(), u.to_owned()));
                }
            }
            "RELATIONSHIP_ADD" | "RELATIONSHIP_UPDATE" => {
                if let Some(u) = d.get("user") {
                    self.store.put_user(u);
                }
                if let (Some(id), Some(kind)) = (d["id"].as_str(), d["type"].as_u64()) {
                    self.store.relationships.insert(id.to_owned(), kind);
                }
                // RELATIONSHIP_UPDATE can be partial; only a present key
                // changes the nickname.
                if let (Some(id), Some(nickname)) = (d["id"].as_str(), d.get("nickname")) {
                    self.store.set_friend_nickname(id, nickname);
                }
                if let (Some(id), Some(ignored)) = (d["id"].as_str(), d["user_ignored"].as_bool()) {
                    if ignored {
                        self.store.ignored.insert(id.to_owned());
                    } else {
                        self.store.ignored.remove(id);
                    }
                }
                self.sync_friends(out);
                self.sync_profile(out);
            }
            // Our note on someone changed (here or on another device).
            "USER_NOTE_UPDATE" => {
                if let Some(id) = d["id"].as_str() {
                    self.set_note(id, d["note"].as_str().unwrap_or_default());
                    self.sync_profile(out);
                }
            }
            "RELATIONSHIP_REMOVE" => {
                if let Some(id) = d["id"].as_str() {
                    self.store.relationships.remove(id);
                    self.store.ignored.remove(id);
                    self.store.friend_nicknames.remove(id);
                }
                self.sync_friends(out);
                self.sync_profile(out);
            }
            // A voice session began or ended (`voice_start_time`: Unix
            // seconds, null when ended), and the answer to op 43.
            "VOICE_CHANNEL_START_TIME_UPDATE" => {
                if let Some(event) = d["id"]
                    .as_str()
                    .and_then(|c| self.voice_since(c, &d["voice_start_time"]))
                {
                    out.push(event);
                }
            }
            "CHANNEL_INFO" => {
                for c in d["channels"].as_array().into_iter().flatten() {
                    if c.get("voice_start_time").is_some()
                        && let Some(event) = c["id"]
                            .as_str()
                            .and_then(|id| self.voice_since(id, &c["voice_start_time"]))
                    {
                        out.push(event);
                    }
                }
            }
            "PRESENCE_UPDATE" => {
                self.store.put_presence(d);
                if let Some(u) = d.get("user").filter(|u| u.get("username").is_some()) {
                    self.store.put_user(u);
                }
                self.sync_friends(out);
                self.sync_rooms(out);
            }
            "USER_UPDATE" => {
                self.store.put_user(d);
                if d["id"].as_str() == Some(me.as_str()) {
                    self.store.me = store::User::from(d);
                    if let Some(on) = d["mfa_enabled"].as_bool() {
                        self.store.mfa_enabled = on;
                    }
                }
            }
            "MESSAGE_REACTION_ADD"
            | "MESSAGE_REACTION_REMOVE"
            | "MESSAGE_REACTION_REMOVE_ALL"
            | "MESSAGE_REACTION_REMOVE_EMOJI" => {
                // The core keeps messages; it gets the whole list again only
                // with a message update, so ask for nothing and nudge it.
                self.reaction_event(event, d, out);
            }
            _ => {}
        }
    }
}
