// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Alerts on new messages, alert modes, and the quiet notice.

use super::*;

/// Decoded call sounds kept for replays, bounded in count and in bytes.
pub(super) const SOUND_CACHE_ENTRIES: usize = 8;
pub(super) const SOUND_CACHE_BYTES: usize = 16 * 1024 * 1024;

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
            Event::SoundPlayed {
                room,
                sound,
                url,
                volume,
                ..
            } if self.app.state().call.as_ref().is_some_and(|call| call.room == *room) => {
                self.play_remote_sound(room, sound, url.as_deref(), *volume);
            }
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
                // Every network: only direct and group chats ring, and never
                // for the call we're already in or joining (a server's voice
                // channel starting, or a repeat while we're in it, stays quiet).
                if *kind == godwit_core::application::AlertKind::Call
                    && (call.is_some_and(|c| c.room == *room)
                        || !self.app.state().room(room).is_none_or(|r| {
                            matches!(
                                r.kind,
                                godwit_core::room::RoomKind::Direct { .. } | godwit_core::room::RoomKind::Group
                            )
                        }))
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

    fn play_remote_sound(&mut self, room: &RoomId, sound: &str, url: Option<&str>, volume: f32) {
        self.collect_sound_cache(SOUND_CACHE_ENTRIES, SOUND_CACHE_BYTES);
        if let Some(index) = self.sound_cache.iter().position(|(key, _)| key == sound)
            && let Some(entry) = self.sound_cache.remove(index)
        {
            let samples = entry.1.clone();
            self.sound_cache.push_front(entry);
            self.alerts.call_sound(&self.settings, samples, volume);
            return;
        }
        // The network says where the audio is; none means nothing to play.
        let Some(url) = url.filter(|url| url.starts_with("https://")).map(str::to_owned) else {
            return;
        };
        // Soundboard spam must not pile up threads: drop plays while a few are fetching.
        if self.sound_fetching.load(std::sync::atomic::Ordering::Relaxed) >= 3 {
            return;
        }
        self.sound_fetching.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let fetching = self.sound_fetching.clone();
        let wake = self.inline_wake.clone();
        let events = self.sound_play_done_tx.clone();
        let sound_id = sound.to_owned();
        let room_id = room.clone();
        let cache = self.sound_cache_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("call-sound-fetch".into())
            .spawn(move || {
                let decoded = crate::images::fetch_public_media(&url)
                    .and_then(|bytes| godwit_voice::sounds::decode_sound(&bytes, 48_000).map_err(|e| format!("{e:#}")));
                match decoded {
                    Ok(samples) => {
                        let _ = cache.send((sound_id, samples.clone()));
                        let _ = events.send((room_id, samples, volume));
                        wake();
                    }
                    Err(error) => {
                        // Ogg Opus sounds aren't decodable yet; say so once, not per play.
                        static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                        if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                            godwit_core::verbose!("note: a call sound couldn't be played ({error})");
                        }
                    }
                }
                fetching.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            });
        if let Err(error) = spawned {
            self.sound_fetching.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            godwit_core::verbose!("note: call sound worker unavailable ({error})");
        }
    }

    pub(super) fn finish_call_sound(&mut self, room: &RoomId, samples: Vec<f32>, volume: f32) {
        if self.app.state().call.as_ref().is_some_and(|call| call.room == *room) {
            self.alerts.call_sound(&self.settings, samples, volume);
        }
    }

    pub(super) fn collect_sound_cache(&mut self, max_entries: usize, max_bytes: usize) {
        while let Ok((id, samples)) = self.sound_cache_done.try_recv() {
            insert_sound(
                &mut self.sound_cache,
                &mut self.sound_cache_bytes,
                id,
                samples,
                max_entries,
                max_bytes,
            );
        }
    }

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

/// Adds a decoded sound at the front, evicting from the back until both the
/// count and the byte bound hold; a sound over the byte bound is not kept.
fn insert_sound(
    cache: &mut std::collections::VecDeque<(String, Vec<f32>)>,
    cache_bytes: &mut usize,
    id: String,
    samples: Vec<f32>,
    max_entries: usize,
    max_bytes: usize,
) {
    let size = |samples: &[f32]| samples.len().saturating_mul(std::mem::size_of::<f32>());
    let bytes = size(&samples);
    if bytes > max_bytes {
        return;
    }
    if let Some(index) = cache.iter().position(|(key, _)| *key == id)
        && let Some((_, old)) = cache.remove(index)
    {
        *cache_bytes = cache_bytes.saturating_sub(size(&old));
    }
    cache.push_front((id, samples));
    *cache_bytes += bytes;
    while cache.len() > max_entries || *cache_bytes > max_bytes {
        let Some((_, evicted)) = cache.pop_back() else { break };
        *cache_bytes = cache_bytes.saturating_sub(size(&evicted));
    }
}

#[cfg(test)]
mod sound_cache_tests {
    use super::insert_sound;
    use std::collections::VecDeque;

    #[test]
    fn cache_is_bounded_in_count_and_bytes() {
        let (mut cache, mut bytes) = (VecDeque::new(), 0);
        for n in 0..5 {
            insert_sound(&mut cache, &mut bytes, n.to_string(), vec![0.0; 10], 3, 1000);
        }
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.front().map(|e| e.0.as_str()), Some("4"));
        assert_eq!(bytes, 3 * 40);
        // Bytes: 100 bytes allows two 40-byte entries.
        let (mut cache, mut bytes) = (VecDeque::new(), 0);
        for n in 0..4 {
            insert_sound(&mut cache, &mut bytes, n.to_string(), vec![0.0; 10], 8, 100);
        }
        assert_eq!((cache.len(), bytes), (2, 80));
        // Oversized and repeated sounds.
        insert_sound(&mut cache, &mut bytes, "big".into(), vec![0.0; 100], 8, 100);
        insert_sound(&mut cache, &mut bytes, "3".into(), vec![0.0; 10], 8, 100);
        assert_eq!((cache.len(), bytes), (2, 80));
    }
}
