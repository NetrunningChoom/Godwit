//! A channel's own notification settings: Discord's channel context menu
//! ("Mute Channel", "Notification Settings"), kept as a channel override in
//! our guild settings (docs.discord.food, User Settings / Guild Settings).

use super::*;
use godwit_core::room::RoomNotifyChange;
use serde_json::{Value, json};

/// Thread member flags for the notification levels (docs.discord.food,
/// [Thread Member Flags](https://docs.discord.food/resources/channel#thread-member-flags)).
const THREAD_LEVEL_FLAGS: [u64; 3] = [1 << 1, 1 << 2, 1 << 3];

/// The thread member settings for a change, keeping the flags that aren't
/// levels (such as HAS_INTERACTED) as they are.
pub(super) fn thread_notify_body(
    change: RoomNotifyChange,
    flags: u64,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<Value> {
    match change {
        RoomNotifyChange::Level(Some(level)) => {
            let bit = THREAD_LEVEL_FLAGS.get(usize::from(level))?;
            let others = flags & !THREAD_LEVEL_FLAGS.iter().fold(0, |a, b| a | b);
            Some(json!({"flags": others | bit}))
        }
        RoomNotifyChange::Level(None) => None,
        RoomNotifyChange::Mute { seconds } => Some(json!({
            "muted": true,
            "mute_config": {
                "selected_time_window": seconds.map_or(-1, i64::from),
                "end_time": seconds.map(|s| (now + chrono::Duration::seconds(i64::from(s))).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            }
        })),
        RoomNotifyChange::Unmute => Some(json!({"muted": false, "mute_config": null})),
    }
}

/// The override's fields for a change, as the web client sends them: a mute
/// carries its window in seconds (-1: until turned off) and its end time.
pub(super) fn override_entry(change: RoomNotifyChange, now: chrono::DateTime<chrono::Utc>) -> Value {
    match change {
        // 3: follow the server's setting.
        // Core keeps the level in 0..=2 (`dispatch_set_room_notify`).
        RoomNotifyChange::Level(level) => json!({ "message_notifications": level.unwrap_or(3) }),
        RoomNotifyChange::Mute { seconds } => json!({
            "muted": true,
            "mute_config": {
                "selected_time_window": seconds.map_or(-1, i64::from),
                "end_time": seconds.map(|s| (now + chrono::Duration::seconds(i64::from(s))).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            },
        }),
        RoomNotifyChange::Unmute => json!({ "muted": false, "mute_config": null }),
    }
}

impl DiscordAccount {
    /// The channel's whole override as the store has it, in the shape the
    /// gateway sends (`channel_overrides` entries).
    fn current_override(&self, channel: &str) -> Value {
        let muted = self.store.channel_muted(channel);
        let end_time = self
            .store
            .muted_channels
            .get(channel)
            .copied()
            .flatten()
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
            .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        json!({
            "channel_id": channel,
            "message_notifications": self.store.channel_notify.get(channel).copied().unwrap_or(3),
            "muted": muted,
            "mute_config": if muted { json!({ "end_time": end_time }) } else { Value::Null },
        })
    }

    /// Applies the change at once (the gateway's USER_GUILD_SETTINGS_UPDATE
    /// then confirms it) and sends it; a failure puts the old override back
    /// (`Answer::ChannelNotify`).
    pub(super) fn set_room_notify(&mut self, room: &RoomId, change: RoomNotifyChange) -> Vec<Event> {
        let channel = room.raw.to_string();
        let Some(c) = self.store.channels.get(&channel) else {
            return Vec::new();
        };
        let guild = c.guild.clone();
        if c.thread() {
            let now = chrono::Utc::now();
            let Some(body) = thread_notify_body(change, c.thread_flags.unwrap_or(0), now) else {
                return Vec::new();
            };
            // Shown at once; THREAD_MEMBER_UPDATE confirms it.
            if let Some(flags) = body["flags"].as_u64()
                && let Some(t) = self.store.channels.get_mut(&channel)
            {
                t.thread_flags = Some(flags);
            }
            match change {
                RoomNotifyChange::Mute { seconds } => {
                    let until = seconds.map(|s| now.timestamp() + i64::from(s));
                    self.store.muted_channels.insert(channel.clone(), until);
                }
                RoomNotifyChange::Unmute => {
                    self.store.muted_channels.remove(&channel);
                }
                RoomNotifyChange::Level(_) => {}
            }
            if self.demo.is_none() {
                self.net.send(Cmd::Request(Request::ThreadNotify {
                    channel: channel.clone(),
                    body,
                }));
            }
            let mut out = Vec::new();
            if let Some(room) = self.room(&channel) {
                out.push(Event::RoomUpdated(room));
            }
            return out;
        }
        let previous = self.current_override(&channel);
        // The change laid over the whole override, and the whole override is
        // what we send: docs.discord.food (User Settings, Modify User Guild
        // Settings) types `channel_overrides` as a map of full channel override
        // objects and doesn't say whether a partial entry merges, so a level
        // change can't drop a mute this way. A mute kept across a level change
        // loses only its `selected_time_window`, which we don't track.
        let mut body = previous.clone();
        if let (Some(body), Some(entry)) = (
            body.as_object_mut(),
            override_entry(change, chrono::Utc::now()).as_object(),
        ) {
            body.extend(entry.clone());
        }
        self.store.apply_guild_settings(&json!({
            "guild_id": guild,
            "channel_overrides": [body],
        }));
        if self.demo.is_none() {
            self.net.send(Cmd::Request(Request::ChannelNotify {
                guild,
                channel: channel.clone(),
                body,
                previous,
            }));
        }
        let mut out = Vec::new();
        if let Some(room) = self.room(&channel) {
            out.push(Event::RoomUpdated(room));
        }
        out
    }

    pub(super) fn apply_override(&mut self, guild: Option<&str>, entry: &Value) {
        self.store.apply_guild_settings(&json!({
            "guild_id": guild,
            "channel_overrides": [entry],
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_match_the_web_clients() {
        let now = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        assert_eq!(
            override_entry(RoomNotifyChange::Level(None), now),
            json!({"message_notifications": 3})
        );
        assert_eq!(
            override_entry(RoomNotifyChange::Level(Some(1)), now),
            json!({"message_notifications": 1})
        );
        assert_eq!(
            override_entry(RoomNotifyChange::Mute { seconds: Some(3600) }, now),
            json!({"muted": true, "mute_config": {"selected_time_window": 3600, "end_time": "2023-11-14T23:13:20.000Z"}})
        );
        assert_eq!(
            override_entry(RoomNotifyChange::Mute { seconds: None }, now),
            json!({"muted": true, "mute_config": {"selected_time_window": -1, "end_time": null}})
        );
        assert_eq!(
            override_entry(RoomNotifyChange::Unmute, now),
            json!({"muted": false, "mute_config": null})
        );
        assert_eq!(
            thread_notify_body(RoomNotifyChange::Level(Some(1)), 1 | 2, now),
            Some(json!({"flags":5}))
        );
        assert_eq!(
            thread_notify_body(RoomNotifyChange::Unmute, 0, now),
            Some(json!({"muted":false,"mute_config":null}))
        );
    }
}
