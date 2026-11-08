// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Alerts on new messages, alert modes, and the quiet notice.

use super::*;

impl Shell {
    /// Settings' row `row` picked alert choice `index`.
    pub fn set_alert_mode(&mut self, ui: &AppWindow, row: i32, index: i32) {
        let Some(id) = self.account_at(row) else { return };
        let choices = settings::alert_choices(id.network);
        let Some(mode) = usize::try_from(index).ok().and_then(|i| choices.get(i)).copied() else {
            return;
        };
        self.settings.set_alert_mode(&id, mode);
        self.save_settings();
        self.refresh(ui);
    }

    /// Notifications and sounds for what's about to be applied (it compares
    /// with the state before the event, so it runs first).
    /// Whether `room` alerted in the last two seconds; notes this one. Keeps
    /// one alert per message when a network raises its own for it too.
    pub(super) fn alerted_lately(&mut self, room: &RoomId) -> bool {
        let now = std::time::Instant::now();
        self.recent_alerts
            .retain(|_, at| now.duration_since(*at) < std::time::Duration::from_secs(2));
        self.recent_alerts.insert(room.clone(), now).is_some()
    }

    /// An alert for any message from someone else in a chat set to "all".
    pub(super) fn alert_every_message(&mut self, room: &RoomId, message: &godwit_core::message::Message) {
        let state = self.app.state();
        let ours = *message.author.raw == *room.account.user || self.live.iter().any(|a| a.is_ours(&message.author));
        let dnd = state
            .accounts
            .iter()
            .find(|a| a.id == room.account)
            .is_some_and(|a| a.presence == Some(godwit_core::members::Presence::Focus));
        // Worth an alert apart from Do Not Disturb: it's listed either way.
        if !crate::alerts::notify_message(
            ours,
            message.system,
            message.pending,
            state.selected.as_ref() == Some(room),
            self.window_focused,
            false,
        ) {
            return;
        }
        let Some(r) = state.room(room) else { return };
        let title = match r.kind {
            godwit_core::room::RoomKind::Direct { .. } => message.author_name.to_string(),
            godwit_core::room::RoomKind::Group => format!("{} ({})", message.author_name, r.name),
            _ => format!("{} · {}", message.author_name, r.name),
        };
        let body = message_body(message);
        if self.alerted_lately(room) {
            return;
        }
        self.note_message(room, message, &body);
        if dnd {
            return;
        }
        let (room, from) = (room.clone(), message.author.clone());
        self.alerts.alert(
            &self.settings,
            &room,
            godwit_core::application::AlertKind::Message,
            Some(&from),
            &title,
            &body,
        );
    }

    pub(super) fn observe_alerts(&mut self, event: &Event) {
        use crate::settings::RoomAlerts;
        use godwit_voice::sounds::Sound;
        // A chat set to every message alerts on each one, whatever the
        // network's own rules (its own alert for the same message is skipped).
        if let Event::MessageAdded { room, message } = event
            && self.settings.room_alerts(room) == RoomAlerts::All
        {
            self.alert_every_message(room, message);
            return;
        }
        if let Event::Alert { room, kind, .. } = event
            && *kind == godwit_core::application::AlertKind::Message
            && (self.settings.room_alerts(room) == RoomAlerts::Off
                || (self.settings.room_alerts(room) == RoomAlerts::All && self.alerted_lately(room)))
        {
            return;
        }
        if let Event::MessageAdded { room, .. } = event
            && self.settings.room_alerts(room) == RoomAlerts::Off
        {
            return;
        }
        let call = self.app.state().call.as_ref();
        match event {
            Event::Alert {
                room,
                kind,
                from,
                title,
                body,
            } => {
                // Every network: the chat on screen stays quiet only while
                // the window is focused.
                if *kind == godwit_core::application::AlertKind::Message
                    && self.window_focused
                    && self.app.state().selected.as_ref() == Some(room)
                {
                    return;
                }
                // Listed before Do Not Disturb, the account's mode or muzzle
                // mode decide on a banner and a sound.
                self.note_alert(room, *kind, from.as_ref(), title, body);
                if *kind == godwit_core::application::AlertKind::Message
                    && !room.account.network.raises_alerts()
                    && self
                        .app
                        .state()
                        .accounts
                        .iter()
                        .any(|a| a.id == room.account && a.presence == Some(godwit_core::members::Presence::Focus))
                {
                    return;
                }
                if *kind == godwit_core::application::AlertKind::Call
                    && !self.app.state().call.as_ref().is_some_and(|c| c.room == *room)
                {
                    self.incoming_call = Some((room.clone(), title.to_string(), body.to_string()));
                }
                let kept_quiet = self
                    .alerts
                    .alert(&self.settings, room, *kind, from.as_ref(), title, body);
                if kept_quiet && self.quiet_notice.is_none() && !self.settings.quiet_told(&room.account) {
                    self.quiet_notice = Some(room.account.clone());
                }
            }
            Event::AlertDone { room } => {
                self.alerts.done(room);
                self.drop_incoming(room);
            }
            // Networks that don't raise alerts themselves (Slack, Teams,
            // Fluxer, Google Chat): someone else's new message in a direct or
            // group chat we aren't looking at, as their own clients notify by
            // default. Channels need the network's mention rules, so not yet.
            Event::MessageAdded { room, message } if !room.account.network.raises_alerts() => {
                let state = self.app.state();
                let ours =
                    *message.author.raw == *room.account.user || self.live.iter().any(|a| a.is_ours(&message.author));
                if self.live.iter().any(|a| a.notifications_muted(room)) {
                    return;
                }
                // Teams and Google Chat keep their own Do Not Disturb: their
                // own presence is Focus then, and their clients stay quiet.
                let dnd = state
                    .accounts
                    .iter()
                    .find(|a| a.id == room.account)
                    .is_some_and(|a| a.presence == Some(godwit_core::members::Presence::Focus));
                // Worth an alert apart from Do Not Disturb: listed either way.
                if !crate::alerts::notify_message(
                    ours,
                    message.system,
                    message.pending,
                    state.selected.as_ref() == Some(room),
                    self.window_focused,
                    false,
                ) {
                    return;
                }
                let Some(r) = state.room(room) else { return };
                let title = match r.kind {
                    godwit_core::room::RoomKind::Direct { .. } => message.author_name.to_string(),
                    godwit_core::room::RoomKind::Group => format!("{} ({})", message.author_name, r.name),
                    _ => return,
                };
                let body = message_body(message);
                self.note_message(room, message, &body);
                if dnd {
                    return;
                }
                let (room, from) = (room.clone(), message.author.clone());
                self.alerts.alert(
                    &self.settings,
                    &room,
                    godwit_core::application::AlertKind::Message,
                    Some(&from),
                    &title,
                    &body,
                );
            }
            Event::CallConnected { room } => {
                self.alerts.done(room);
                self.drop_incoming(room);
                self.alerts.sound(&self.settings, Sound::Join);
            }
            Event::CallEnded { room, .. } if call.is_some_and(|c| c.room == *room) => {
                self.alerts.sound(&self.settings, Sound::Leave);
            }
            // Our own stream started or stopped, as Discord's client sounds
            // going live: rising notes on, falling off (only on a change).
            Event::CallScreenShare { room, active } => {
                if call.is_some_and(|c| c.room == *room && c.screen_share != *active) {
                    let sound = if *active { Sound::WatchStart } else { Sound::WatchStop };
                    self.alerts.sound(&self.settings, sound);
                }
                if !*active {
                    godwit_voice::video::focus_preview(None);
                }
            }
            // Someone in our call going live, or stopping.
            Event::CallStreams { room, streams } => {
                let Some(call) = call.filter(|c| c.room == *room && c.status == CallStatus::Connected) else {
                    return;
                };
                let before: Vec<_> = call.streams.iter().map(|s| &s.user).collect();
                let now: Vec<_> = streams.iter().map(|s| &s.user).collect();
                let viewers_changed = streams.iter().any(|stream| {
                    call.streams
                        .iter()
                        .find(|old| old.user == stream.user)
                        .is_some_and(|old| old.viewers != stream.viewers)
                });
                if now.len() != before.len() || now.iter().any(|u| !before.contains(u)) || viewers_changed {
                    self.alerts.sound(&self.settings, Sound::StreamChanged);
                }
            }
            // Someone else joining or leaving the call we're in.
            Event::CallMembers { room, members } => {
                let Some(call) = call.filter(|c| c.room == *room && c.status == CallStatus::Connected) else {
                    return;
                };
                if call.members.is_empty() {
                    return;
                }
                let cue = match members.len().cmp(&call.members.len()) {
                    std::cmp::Ordering::Greater => Some(Sound::Join),
                    std::cmp::Ordering::Less => Some(Sound::Leave),
                    std::cmp::Ordering::Equal => None,
                };
                if let Some(cue) = cue {
                    self.alerts.sound(&self.settings, cue);
                }
            }
            _ => {}
        }
    }

    /// A message alert for the Notifications page.
    fn note_message(&mut self, room: &RoomId, message: &godwit_core::message::Message, body: &str) {
        self.notifications.push(crate::notifications::Notification {
            room: room.clone(),
            kind: godwit_core::application::AlertKind::Message,
            from: Some(message.author.clone()),
            author: message.author_name.clone(),
            avatar: message.author_avatar.clone(),
            preview: body.into(),
            message: Some(message.id.clone()),
            at_ms: now_ms(),
        });
    }

    /// A network's own alert for the Notifications page. It names no
    /// message, so the sender's newest in the chat (if it came just now)
    /// gives the name, picture and the message to jump to; else the alert's
    /// title stands in.
    fn note_alert(
        &mut self,
        room: &RoomId,
        kind: godwit_core::application::AlertKind,
        from: Option<&godwit_core::ids::UserId>,
        title: &str,
        body: &str,
    ) {
        let now = now_ms();
        let state = self.app.state();
        let message = from.and_then(|from| {
            state
                .timelines
                .get(room)?
                .messages
                .iter()
                .rev()
                .find(|m| m.author == *from && !m.pending)
                .filter(|m| (now - m.sent_ms).abs() < 60_000)
                .cloned()
        });
        let friend = from.and_then(|from| state.friends.iter().find(|p| p.id == *from));
        let author = message
            .as_ref()
            .map(|m| m.author_name.clone())
            .or_else(|| friend.map(|p| p.name.clone()))
            .unwrap_or_else(|| title.into());
        let avatar = message
            .as_ref()
            .and_then(|m| m.author_avatar.clone())
            .or_else(|| friend.and_then(|p| p.avatar_url.clone()));
        let preview = match kind {
            godwit_core::application::AlertKind::Call if body.trim().is_empty() => "Incoming call".into(),
            _ => body.into(),
        };
        self.notifications.push(crate::notifications::Notification {
            room: room.clone(),
            kind,
            from: from.cloned(),
            author,
            avatar,
            preview,
            message: message.map(|m| m.id.clone()),
            at_ms: now,
        });
    }

    pub(super) fn show_quiet_notice(&self, ui: &AppWindow) {
        let name = self.quiet_notice.as_ref().map(|id| {
            self.app
                .state()
                .accounts
                .iter()
                .find(|a| &a.id == id)
                .map_or_else(|| id.network.name().to_owned(), |a| a.display_name.to_string())
        });
        ui.set_quiet_notice_open(name.is_some());
        ui.set_quiet_notice_account(name.unwrap_or_default().into());
    }

    /// The "Steam is running" notice answered: alert in Godwit too (the
    /// account's mode becomes Notify), or stay quiet. Shown once per account.
    pub fn quiet_notice(&mut self, ui: &AppWindow, notify: bool) {
        let Some(id) = self.quiet_notice.take() else { return };
        if notify {
            self.settings.set_alert_mode(&id, crate::settings::AlertMode::Normal);
        }
        self.settings.mark_quiet_told(&id);
        self.save_settings();
        self.show_quiet_notice(ui);
    }
}

/// A message's text for an alert: up to 200 characters, or that it's a file.
fn message_body(message: &godwit_core::message::Message) -> String {
    let text = message.snippet.as_deref().unwrap_or(&message.source);
    let mut body: String = text.chars().take(200).collect();
    if text.chars().count() > 200 {
        body.push('…');
    }
    if body.trim().is_empty() && !message.attachments.is_empty() {
        body = "Sent a file".into();
    }
    body
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
