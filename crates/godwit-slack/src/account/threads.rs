// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Threads: fetching, marking read and showing them.

use super::*;

impl SlackAccount {
    pub(super) fn refresh_threads(&mut self, team: &str, more: bool) {
        let Some(ws) = self.workspaces.get_mut(team) else {
            return;
        };
        if ws.threads_pending {
            return;
        }
        let cursor = if more {
            // Nothing older, or no first page yet: nothing to fetch.
            let Some(cursor) = ws.threads_cursor.clone() else {
                return;
            };
            Some(cursor)
        } else {
            None
        };
        ws.threads_pending = true;
        self.request(team, Request::Threads(cursor));
    }
    pub(super) fn mark_thread(&mut self, team: &str, channel: &str, root: &str, ts: String) {
        let Some(t) = self
            .workspaces
            .get_mut(team)
            .and_then(|w| w.threads.get_mut(&(channel.to_owned(), root.to_owned())))
        else {
            return;
        };
        t.mark_wanted = Some(ts.clone());
        if t.mark_pending.is_some() {
            return;
        }
        t.mark_pending = Some(ts.clone());
        self.request(
            team,
            Request::MarkThread {
                channel: channel.to_owned(),
                root: root.to_owned(),
                ts,
            },
        );
    }

    /// A thread's room, under its channel.
    pub(super) fn thread_room(&self, team: &str, channel: &str, ts: &str) -> Option<Room> {
        let ws = self.workspaces.get(team)?;
        let t = ws.threads.get(&(channel.to_owned(), ts.to_owned()))?;
        let account = self.account.clone()?;
        let mut capabilities = Capabilities::of(Network::Slack);
        capabilities.threads = false;
        Some(Room {
            notify: Default::default(),
            id: RoomId::new(account.clone(), format!("{team}:{channel}:{ts}").as_str()),
            name: if t.subscribed {
                let label = self
                    .room(team, channel)
                    .map_or_else(|| channel.to_owned(), |r| r.name.to_string());
                let prefix = if ws.conversations.get(channel).is_some_and(|c| c.kind == Kind::Channel) {
                    "#"
                } else {
                    ""
                };
                format!("{prefix}{label} · {}", t.name).into()
            } else {
                t.name.as_str().into()
            },
            // Every thread lives in its workspace (its Threads page and
            // section), DM threads too: as a Direct room it would open in
            // Home as a chat with the partner. Its name carries the partner.
            kind: RoomKind::Channel {
                server: ServerId::new(account, team),
            },
            icon_url: None,
            capabilities,
            encryption: RoomEncryption::Unencrypted,
            unread: t.unread,
            mentions: 0,
            last_read: (!t.last_read.is_empty()).then(|| MessageId(t.last_read.as_str().into())),
            invited_by: None,
            presence: None,
            status: (!t.preview.is_empty()).then(|| t.preview.as_str().into()),
            member_count: None,
            last_activity: ts_ms(&t.latest),
            parent: Some(format!("{team}:{channel}").into()),
            joined_thread: false,
        })
    }

    /// A channel's threads to list: the most recently active first.
    pub(super) fn shown_threads(&self, team: &str, channel: &str) -> Vec<String> {
        let Some(ws) = self.workspaces.get(team) else {
            return Vec::new();
        };
        let mut threads: Vec<(&String, &Thread)> = ws
            .threads
            .iter()
            .filter(|((c, _), t)| c == channel && !t.subscribed)
            .map(|((_, ts), t)| (ts, t))
            .collect();
        threads.sort_by(|a, b| {
            if newer(&a.1.latest, &b.1.latest) {
                std::cmp::Ordering::Less
            } else if newer(&b.1.latest, &a.1.latest) {
                std::cmp::Ordering::Greater
            } else {
                b.0.cmp(a.0)
            }
        });
        threads
            .into_iter()
            .take(THREADS_SHOWN)
            .map(|(ts, _)| ts.clone())
            .collect()
    }

    /// Notes a thread (from its root post, or a reply seen live).
    pub(super) fn put_thread(
        &mut self,
        team: &str,
        channel: &str,
        ts: &str,
        name: Option<String>,
        latest: &str,
        unread: bool,
    ) {
        let Some(ws) = self.workspaces.get_mut(team) else {
            return;
        };
        let t = ws.threads.entry((channel.to_owned(), ts.to_owned())).or_default();
        if let Some(n) = name {
            t.name = n;
        } else if t.name.is_empty() {
            t.name = "Thread".into();
        }
        if latest.is_empty() || newer(latest, &t.latest) || t.latest.is_empty() {
            t.latest = if latest.is_empty() {
                ts.to_owned()
            } else {
                latest.to_owned()
            };
        }
        t.unread |= unread;
    }
}
