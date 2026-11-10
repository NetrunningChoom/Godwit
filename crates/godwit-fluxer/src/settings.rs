// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Fluxer's user settings and user guild settings: the shapes that come in
//! (READY, USER_SETTINGS_UPDATE, USER_GUILD_SETTINGS_UPDATE) and the bodies
//! that go out. Documented at <https://docs.fluxer.com/http-api/users/settings/>
//! and <https://docs.fluxer.com/gateway/events/>.

use godwit_core::room::RoomNotifyChange;
use serde_json::{Map, Value, json};

/// Key of the DM settings (`PATCH /users/@me/guilds/@me/settings`, where the
/// guild settings carry a null `guild_id`).
pub const DMS: &str = "@me";

/// The guild settings' key for a guild id (None: the DMs').
pub fn key(guild: Option<&str>) -> String {
    guild.unwrap_or(DMS).to_owned()
}

/// A user guild settings object in the Discord store's shape: Fluxer's
/// `channel_overrides` is a map keyed by channel id ("it's a map, not an
/// array"), the store reads an array of entries carrying `channel_id`.
pub fn normalize_guild_settings(s: &Value) -> Value {
    let mut out = s.clone();
    if let Some(map) = s["channel_overrides"].as_object() {
        let list: Vec<Value> = map
            .iter()
            .filter_map(|(channel, o)| {
                let mut o = o.as_object()?.clone();
                o.insert("channel_id".into(), json!(channel));
                Some(Value::Object(o))
            })
            .collect();
        out["channel_overrides"] = Value::Array(list);
    } else if s["channel_overrides"].is_null() {
        out["channel_overrides"] = json!([]);
    }
    out
}

/// A time as ISO 8601 (Fluxer's output) or Unix milliseconds (also accepted).
fn time_secs(v: &Value) -> Option<i64> {
    match v {
        Value::String(t) => chrono::DateTime::parse_from_rfc3339(t).ok().map(|t| t.timestamp()),
        Value::Number(n) => n.as_i64().map(|ms| ms / 1000),
        _ => None,
    }
}

/// The override fields for a change. A mute carries its end time
/// (`mute_config.end_time`, null: no expiry) and the window chosen
/// (`selected_time_window`, 0 when none); level 3 follows the server.
pub fn channel_patch(change: RoomNotifyChange, now: chrono::DateTime<chrono::Utc>) -> Map<String, Value> {
    let v = match change {
        RoomNotifyChange::Level(level) => json!({ "message_notifications": level.map_or(3, |l| l.min(2)) }),
        RoomNotifyChange::Mute { seconds } => json!({
            "muted": true,
            "mute_config": {
                "end_time": seconds.map(|s| (now + chrono::Duration::seconds(i64::from(s)))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
                "selected_time_window": seconds.map_or(0, i64::from),
            },
        }),
        RoomNotifyChange::Unmute => json!({
            "muted": false,
            "mute_config": { "end_time": null, "selected_time_window": 0 },
        }),
    };
    v.as_object().cloned().unwrap_or_default()
}

/// The fields of a stored override that the PATCH accepts back.
fn input_fields(entry: &Value) -> Map<String, Value> {
    const INPUT: [&str; 6] = [
        "collapsed",
        "message_notifications",
        "muted",
        "mute_config",
        "unread_badges",
        "flags",
    ];
    entry
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, v)| INPUT.contains(&k.as_str()) && !v.is_null())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The PATCH body for a change to one channel. `channel_overrides` replaces
/// every stored override, so the guild's others (and this one's other
/// fields, such as a category's `collapsed`) go back as they were.
pub fn overrides_body(existing: &Map<String, Value>, channel: &str, patch: Map<String, Value>) -> Value {
    let mut map: Map<String, Value> = existing
        .iter()
        .map(|(c, o)| (c.clone(), Value::Object(input_fields(o))))
        .collect();
    let mut entry = map.get(channel).and_then(Value::as_object).cloned().unwrap_or_default();
    entry.extend(patch);
    map.insert(channel.to_owned(), Value::Object(entry));
    json!({ "channel_overrides": map })
}

/// `PATCH /users/@me/settings`: our status (`online`, `dnd`, `idle`,
/// `invisible`). An earlier timed status doesn't linger.
pub fn status_body(status: &str) -> Value {
    json!({ "status": status, "status_resets_at": null, "status_resets_to": null })
}

/// `PATCH /users/@me/settings`: our custom status (`null` clears it).
pub fn custom_status_body(text: Option<&str>) -> Value {
    match text {
        Some(t) => json!({ "custom_status": { "text": t, "expires_at": null, "emoji_id": null, "emoji_name": null } }),
        None => json!({ "custom_status": null }),
    }
}

/// `PATCH …/guilds/{id}/settings`: the server's own level.
pub fn guild_level_body(level: u8) -> Value {
    json!({ "message_notifications": level.min(2) })
}

/// What a user settings object says of our status.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct StatusRead {
    pub status: Option<String>,
    /// Some(None): no custom status (or it ran out).
    pub custom: Option<Option<String>>,
}

/// Our status and custom status. A custom status keeps its `text` after
/// `expires_at`, so one in the past is hidden (the docs say clients should).
pub fn read_status(settings: &Value, now_secs: i64) -> StatusRead {
    let status = settings["status"]
        .as_str()
        .filter(|s| matches!(*s, "online" | "dnd" | "idle" | "invisible"))
        .map(str::to_owned);
    let cs = &settings["custom_status"];
    let custom = if cs.is_null() {
        settings.get("custom_status").map(|_| None)
    } else {
        let live = time_secs(&cs["expires_at"]).is_none_or(|t| t > now_secs);
        Some(cs["text"].as_str().filter(|t| !t.is_empty() && live).map(str::to_owned))
    };
    StatusRead { status, custom }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap_or_default()
    }

    #[test]
    fn override_maps_become_store_entries() {
        let s = json!({"guild_id": "1", "message_notifications": 1, "channel_overrides": {"5": {"muted": true, "message_notifications": 2}}});
        let n = normalize_guild_settings(&s);
        assert_eq!(
            n["channel_overrides"],
            json!([{"channel_id": "5", "muted": true, "message_notifications": 2}])
        );
        let none = normalize_guild_settings(&json!({"guild_id": null, "channel_overrides": null}));
        assert_eq!(none["channel_overrides"], json!([]));
    }

    #[test]
    fn channel_changes_keep_the_other_overrides() {
        let existing = json!({
            "5": {"muted": true, "message_notifications": 2, "mute_config": {"end_time": null, "selected_time_window": 0}},
            "6": {"collapsed": true, "message_notifications": 3, "channel_id": "6", "unread_badges": null}
        });
        let existing = existing.as_object().cloned().unwrap_or_default();
        let body = overrides_body(&existing, "6", channel_patch(RoomNotifyChange::Level(Some(1)), now()));
        assert_eq!(
            body,
            json!({"channel_overrides": {
                "5": {"muted": true, "message_notifications": 2, "mute_config": {"end_time": null, "selected_time_window": 0}},
                "6": {"collapsed": true, "message_notifications": 1}
            }})
        );
        let body = overrides_body(
            &Map::new(),
            "9",
            channel_patch(RoomNotifyChange::Mute { seconds: Some(3600) }, now()),
        );
        assert_eq!(
            body,
            json!({"channel_overrides": {"9": {"muted": true, "mute_config": {"end_time": "2023-11-14T23:13:20.000Z", "selected_time_window": 3600}}}})
        );
        assert_eq!(
            channel_patch(RoomNotifyChange::Level(None), now()).get("message_notifications"),
            Some(&json!(3))
        );
        assert_eq!(
            channel_patch(RoomNotifyChange::Unmute, now()),
            json!({"muted": false, "mute_config": {"end_time": null, "selected_time_window": 0}})
                .as_object()
                .cloned()
                .unwrap_or_default()
        );
    }

    #[test]
    fn settings_bodies() {
        assert_eq!(status_body("dnd")["status"], "dnd");
        assert_eq!(custom_status_body(None), json!({"custom_status": null}));
        assert_eq!(custom_status_body(Some("hi"))["custom_status"]["text"], "hi");
        assert_eq!(guild_level_body(7), json!({"message_notifications": 2}));
    }

    #[test]
    fn reads_status_and_hides_expired_custom_status() {
        let s = json!({"status": "idle", "custom_status": {"text": "away", "expires_at": "2023-11-14T22:13:20Z"}});
        assert_eq!(
            read_status(&s, 1_700_000_000),
            StatusRead {
                status: Some("idle".into()),
                custom: Some(None)
            }
        );
        let live = json!({"custom_status": {"text": "hi", "expires_at": 1_700_003_600_000_i64}});
        assert_eq!(read_status(&live, 1_700_000_000).custom, Some(Some("hi".into())));
        assert_eq!(read_status(&json!({"status": "offline"}), 0), StatusRead::default());
        assert_eq!(read_status(&json!({"custom_status": null}), 0).custom, Some(None));
    }
}
