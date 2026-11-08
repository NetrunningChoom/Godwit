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

    /// What demo mode answers for a command; None for anything Discord
    /// would be asked (which then goes nowhere).
    pub(super) fn demo_answer(&mut self, command: &Command) -> Option<Vec<Event>> {
        let history = self.demo.as_ref()?;
        let mut out = Vec::new();
        match command {
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
                self.sync_profile(&mut out);
            }
            // What the gateway would say after the request, as the real account sees it.
            Command::BlockUser { user } if user.raw.as_ref() != self.me() => {
                let d = json!({"id": user.raw.as_ref(), "type": store::BLOCKED});
                self.dispatch("RELATIONSHIP_ADD", &d, &mut out);
            }
            Command::UnblockUser { user } if user.raw.as_ref() != self.me() => {
                self.dispatch("RELATIONSHIP_REMOVE", &json!({"id": user.raw.as_ref()}), &mut out);
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
            Command::Typing { .. } | Command::MarkRead { .. } | Command::Focus { .. } => return None,
            Command::LoadMembers { .. } | Command::LoadServerOverview { .. } | Command::OpenDirect { .. } => {
                return None;
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
