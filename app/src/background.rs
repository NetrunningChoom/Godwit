// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Soft-locked profiles (Settings → Profiles, "Keep other profiles
//! connected"): a profile not in use keeps its accounts running on a
//! [`Shelf`] of its own, with its own core [`Application`], so nothing of
//! it reaches the window's state (the UI only reads the shell's
//! application). Only its alerts come out, as desktop notifications naming
//! the profile; switching back swaps the shelf in whole, so it is instant.
//! Which accounts run on which shelf is planned in
//! [`crate::profiles::background_plan`].

use godwit_core::application::{AlertKind, AppState, Application, Effect, Event};
use godwit_core::contacts::Contacts;
use godwit_core::ids::{AccountId, RoomId, UserId};
use godwit_core::members::Presence;
use godwit_core::message::Message;
use godwit_core::room::RoomKind;

use crate::profiles::Running;
use crate::registry::Account;
use crate::settings::{RoomAlerts, Settings};

/// A background profile: its running accounts and what they showed.
pub struct Shelf {
    pub app: Application,
    pub live: Vec<Box<dyn Account>>,
    /// Something alerted since it was last in use (a dot in the profile menu).
    pub unseen: bool,
}

impl Default for Shelf {
    fn default() -> Self {
        Self {
            // Contacts are the foreground's: a shelf names nobody on screen,
            // and a switch loads the profile's contacts anyway.
            app: Application::new(Contacts::default()),
            live: Vec::new(),
            unseen: false,
        }
    }
}

impl Shelf {
    /// Its accounts, as switching plans them.
    pub fn running(&self) -> Vec<Running> {
        self.live
            .iter()
            .map(|a| Running {
                key: a.id().map(crate::settings::account_key),
                network: a.network().key().to_owned(),
                slot: a.slot(),
            })
            .collect()
    }

    /// Everything its accounts sent since the last call.
    pub fn poll(&mut self) -> Vec<Event> {
        self.live.iter_mut().flat_map(|a| a.poll()).collect()
    }

    /// Takes account `i` off the shelf; what it showed goes with it (a
    /// sign-in form is cancelled).
    pub fn remove(&mut self, i: usize) {
        if i >= self.live.len() {
            return;
        }
        let mut account = self.live.remove(i);
        match account.id().cloned() {
            Some(id) => self.app.unload_account(&id),
            None => account.cancel_sign_in(),
        }
    }

    /// Applies an event to the shelf's own state and carries out what it
    /// asks of the network (reads, history the network fills in by
    /// itself). An account set offline goes down as soon as it's known,
    /// as in the foreground.
    pub fn apply(&mut self, event: Event, offline: &dyn Fn(&AccountId) -> bool) {
        let down = match &event {
            Event::AccountReady { account, .. } if offline(account) => Some(account.clone()),
            _ => None,
        };
        let mut queue: std::collections::VecDeque<Effect> = self.app.apply(event).into();
        if let Some(account) = down
            && let Some(a) = self.live.iter_mut().find(|a| a.route_id() == Some(&account))
        {
            for e in a.set_offline(true) {
                queue.extend(self.app.apply(e));
            }
        }
        while let Some(effect) = queue.pop_front() {
            match effect {
                Effect::Backend { account, command } => {
                    if offline(&account) {
                        continue;
                    }
                    let Some(a) = self.live.iter_mut().find(|a| a.route_id() == Some(&account)) else {
                        continue;
                    };
                    for e in a.run(command) {
                        queue.extend(self.app.apply(e));
                    }
                }
                // Nothing on screen watches a stream, and the contacts file
                // is written by the profile in use only.
                Effect::WatchStream { .. } | Effect::SaveContacts | Effect::CopyWebhookUrl(_) => {}
            }
        }
    }

    /// The alert `event` is worth, before it's applied (see [`notice`]).
    pub fn notice(&self, event: &Event, settings: &Settings) -> Option<Notice> {
        notice(
            event,
            self.app.state(),
            settings,
            &|user| self.live.iter().any(|a| a.is_ours(user)),
            &|room| self.live.iter().any(|a| a.notifications_muted(room)),
        )
    }
}

/// An alert from a background profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub room: RoomId,
    pub kind: AlertKind,
    pub from: Option<UserId>,
    pub title: String,
    pub body: String,
}

/// What a background account's event alerts, by the foreground's rules
/// (`shell::alerting`): the network's own alerts, a chat's "every message"
/// or "none", and, for networks that raise no alerts themselves, someone
/// else's message in a direct or group chat. Nothing is on screen there,
/// so no chat counts as being looked at.
pub fn notice(
    event: &Event,
    state: &AppState,
    settings: &Settings,
    ours: &dyn Fn(&UserId) -> bool,
    muted: &dyn Fn(&RoomId) -> bool,
) -> Option<Notice> {
    match event {
        Event::Alert {
            room,
            kind,
            from,
            title,
            body,
        } => {
            // "Every message" alerts from the message itself (one alert per
            // message); "none" means none.
            if *kind == AlertKind::Message && settings.room_alerts(room) != RoomAlerts::Default {
                return None;
            }
            Some(Notice {
                room: room.clone(),
                kind: *kind,
                from: from.clone(),
                title: title.to_string(),
                body: body.to_string(),
            })
        }
        Event::MessageAdded { room, message } => {
            let all = match settings.room_alerts(room) {
                RoomAlerts::Off => return None,
                RoomAlerts::All => true,
                RoomAlerts::Default => false,
            };
            if !all && (room.account.network.raises_alerts() || muted(room)) {
                return None;
            }
            let mine = *message.author.raw == *room.account.user || ours(&message.author);
            let dnd = state
                .accounts
                .iter()
                .find(|a| a.id == room.account)
                .is_some_and(|a| a.presence == Some(Presence::Focus));
            if !crate::alerts::notify_message(mine, message.system, message.pending, false, false, dnd) {
                return None;
            }
            let r = state.room(room)?;
            let title = match r.kind {
                RoomKind::Direct { .. } => message.author_name.to_string(),
                RoomKind::Group => format!("{} ({})", message.author_name, r.name),
                _ if all => format!("{} · {}", message.author_name, r.name),
                // Channels need the network's mention rules, as in the foreground.
                _ => return None,
            };
            Some(Notice {
                room: room.clone(),
                kind: AlertKind::Message,
                from: Some(message.author.clone()),
                title,
                body: message_body(message),
            })
        }
        _ => None,
    }
}

/// A notification's title and text: always the profile's name, so it's
/// clear which profile it's from; who and what only with previews on.
pub fn notification_text(profile: &str, notice: &Notice, previews: bool) -> (String, String) {
    let kind = match notice.kind {
        AlertKind::Message => "New message",
        AlertKind::Call => "Incoming call",
    };
    if !previews {
        return (profile.to_owned(), kind.to_owned());
    }
    let body = if notice.body.trim().is_empty() {
        kind.to_owned()
    } else {
        notice.body.clone()
    };
    (format!("{profile} · {}", notice.title), body)
}

/// A message's text for an alert: up to 200 characters, or that it's a
/// file (as `shell::alerting`'s).
fn message_body(message: &Message) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo;
    use godwit_core::ids::AccountId;
    use godwit_core::network::Network;

    fn state_with(room: godwit_core::room::Room) -> Application {
        let mut app = Application::new(Contacts::default());
        app.apply(Event::AccountReady {
            account: room.id.account.clone(),
            display_name: "Me".into(),
            username: "me".into(),
            avatar_url: None,
            rooms: vec![room],
            servers: Vec::new(),
        });
        app
    }

    fn alert(room: &RoomId, kind: AlertKind) -> Event {
        Event::Alert {
            room: room.clone(),
            kind,
            from: None,
            title: "Ann".into(),
            body: "hi".into(),
        }
    }

    #[test]
    fn network_alerts_pass_unless_the_chat_says_otherwise() {
        let app = Application::new(Contacts::default());
        let room = RoomId::new(demo::account(), "r");
        let mut settings = Settings::default();
        let n = notice(
            &alert(&room, AlertKind::Message),
            app.state(),
            &settings,
            &|_| false,
            &|_| false,
        );
        assert_eq!(n.map(|n| n.title), Some("Ann".to_owned()));
        settings.cycle_room_alerts(&room); // every message
        assert!(
            notice(
                &alert(&room, AlertKind::Message),
                app.state(),
                &settings,
                &|_| false,
                &|_| false
            )
            .is_none()
        );
        assert!(
            notice(
                &alert(&room, AlertKind::Call),
                app.state(),
                &settings,
                &|_| false,
                &|_| false
            )
            .is_some(),
            "calls ring whatever the chat's message setting"
        );
    }

    #[test]
    fn messages_alert_where_the_network_raises_none() {
        // A network that raises no alerts of its own.
        let quiet = [
            Network::Slack,
            Network::Teams,
            Network::GoogleChat,
            Network::Fluxer,
            Network::Discord,
            Network::Steam,
            Network::Stoat,
        ]
        .into_iter()
        .find(|n| !n.raises_alerts());
        let Some(network) = quiet else { return };
        let account = AccountId::new(network, "me");
        let mut dm = demo::room("dm", "Ann", RoomKind::Group, false, 0);
        dm.id = RoomId::new(account.clone(), "dm");
        let app = state_with(dm.clone());
        let settings = Settings::default();
        let mut m = demo::msg("1", "ann", "Ann", 0, "hello");
        m.author = UserId::new(account.clone(), "ann");
        let ev = Event::MessageAdded {
            room: dm.id.clone(),
            message: m.clone(),
        };
        let n = notice(&ev, app.state(), &settings, &|_| false, &|_| false).map(|n| (n.title, n.body));
        assert_eq!(n, Some(("Ann (Ann)".to_owned(), "hello".to_owned())));
        assert!(
            notice(&ev, app.state(), &settings, &|_| false, &|_| true).is_none(),
            "muted"
        );
        assert!(
            notice(&ev, app.state(), &settings, &|_| true, &|_| false).is_none(),
            "our own"
        );
    }

    #[test]
    fn every_message_and_none_follow_the_chat() {
        let mut channel = demo::room("c", "general", RoomKind::Group, false, 0);
        channel.id = RoomId::new(demo::account(), "c");
        let app = state_with(channel.clone());
        let mut settings = Settings::default();
        let ev = Event::MessageAdded {
            room: channel.id.clone(),
            message: demo::msg("1", "ann", "Ann", 0, "hello"),
        };
        settings.cycle_room_alerts(&channel.id); // every message
        assert!(notice(&ev, app.state(), &settings, &|_| false, &|_| false).is_some());
        settings.cycle_room_alerts(&channel.id); // none
        assert!(notice(&ev, app.state(), &settings, &|_| false, &|_| false).is_none());
    }

    #[test]
    fn notifications_name_the_profile_and_hide_content_unless_asked() {
        let n = Notice {
            room: RoomId::new(demo::account(), "r"),
            kind: AlertKind::Message,
            from: None,
            title: "Ann".into(),
            body: "the secret plan".into(),
        };
        assert_eq!(
            notification_text("Work", &n, false),
            ("Work".to_owned(), "New message".to_owned())
        );
        assert_eq!(
            notification_text("Work", &n, true),
            ("Work · Ann".to_owned(), "the secret plan".to_owned())
        );
        let call = Notice {
            kind: AlertKind::Call,
            body: String::new(),
            ..n
        };
        assert_eq!(notification_text("Work", &call, true).1, "Incoming call");
    }
}
