// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Notification preferences and mutes.

use super::*;

impl SlackAccount {
    pub fn notifications_muted(&self, room: &RoomId) -> bool {
        self.account.as_ref() == Some(&room.account)
            && parts(&room.raw).is_some_and(|(team, channel, _)| {
                self.workspaces.get(team).is_some_and(|ws| ws.muted.contains(channel))
            })
    }
    pub(super) fn refresh_notification_prefs(&mut self, team: &str) {
        if let Some(ws) = self.workspaces.get_mut(team)
            && !ws.prefs_pending
        {
            ws.prefs_pending = true;
            self.request(team, Request::NotificationPrefs);
        }
    }

    pub(super) fn notification_prefs(&mut self, team: &str, prefs: &Value) {
        // Protocol: slk GetMutedChannels, including the modern JSON-encoded
        // all_notifications_prefs and Slack's older comma-separated list.
        let mut muted: HashSet<String> = prefs["muted_channels"]
            .as_str()
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        let modern: Value = prefs["all_notifications_prefs"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        let mut levels = HashMap::new();
        if let Some(channels) = modern["channels"].as_object() {
            muted.extend(
                channels
                    .iter()
                    .filter(|(_, p)| p["muted"] == true)
                    .map(|(id, _)| id.clone()),
            );
            // The web client's per-channel "desktop" level; "default" defers
            // to the workspace's.
            levels.extend(channels.iter().filter_map(|(id, p)| {
                let level = p["desktop"].as_str()?;
                (level != "default" && !level.is_empty()).then(|| (id.clone(), level.to_owned()))
            }));
        }
        let global = &modern["global"];
        let desktop_level = global["global_desktop"].as_str().unwrap_or_default().to_owned();
        let keywords: Vec<String> = global["global_keywords"]
            .as_str()
            .unwrap_or_default()
            .split(',')
            .map(|k| k.trim().to_lowercase())
            .filter(|k| !k.is_empty())
            .take(100)
            .collect();
        if let Some(ws) = self.workspaces.get_mut(team) {
            ws.muted = muted;
            ws.channel_levels = levels;
            if !desktop_level.is_empty() || global.is_object() {
                ws.desktop_level = desktop_level;
                ws.keywords = keywords;
            }
            ws.notification_settings = prefs.clone();
        }
    }
}
