// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Gateway events: the backend's messages, READY and dispatch.

use super::*;

impl DiscordAccount {
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
                    self.store.apply_settings(proto);
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
                } else if let (Some(room), Some(id), Some(embeds)) =
                    (self.room_id(channel), d["id"].as_str(), d["embeds"].as_array())
                {
                    out.push(Event::EmbedsUpdated {
                        room,
                        id: godwit_core::ids::MessageId(id.into()),
                        embeds: embeds.iter().filter_map(super::embed).collect(),
                    });
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
                }
                self.sync_rooms(out);
                self.sync_servers(out);
                self.sync_emoji(out);
                self.sync_stickers(out);
                self.voice_dispatch(event, d, out);
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
                self.store.apply_guild_settings(d);
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
