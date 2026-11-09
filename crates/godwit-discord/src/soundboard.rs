// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Discord soundboard response and gateway payload mapping.
//! See docs.discord.food, Soundboard resource.

use godwit_core::call::Sound;
use serde_json::{Value, json};

pub(crate) fn parse(value: &Value) -> Option<Sound> {
    let id = value["sound_id"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| value["sound_id"].as_u64().map(|n| n.to_string()))?;
    let name = value["name"].as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    let emoji = value["emoji_name"].as_str().filter(|s| !s.is_empty()).map(Into::into);
    let source = value["guild_id"]
        .as_str()
        .filter(|s| !s.is_empty() && *s != "0")
        .map(Into::into);
    Some(Sound {
        id: id.into(),
        name: name.into(),
        emoji,
        source,
    })
}

pub(crate) fn parse_all(value: &Value) -> Vec<Sound> {
    value.as_array().into_iter().flatten().filter_map(parse).collect()
}

pub(crate) fn play_body(sound: &Sound) -> Value {
    let mut body = json!({"sound_id": sound.id, "emoji_name": sound.emoji});
    if let Some(source) = &sound.source {
        body["source_guild_id"] = json!(source);
    }
    body
}

/// Where Discord's CDN serves a sound; ids are numeric snowflakes, anything else is refused.
pub(crate) fn audio_url(sound: &str) -> Option<String> {
    (!sound.is_empty() && sound.bytes().all(|b| b.is_ascii_digit()))
        .then(|| format!("https://cdn.discordapp.com/soundboard-sounds/{sound}"))
}

pub(crate) fn played(value: &Value) -> Option<(String, String, String, f32)> {
    let channel = value["channel_id"].as_str()?.to_owned();
    let user = value["user_id"].as_str()?.to_owned();
    let sound = value["sound_id"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| value["sound_id"].as_u64().map(|n| n.to_string()))?;
    let volume = value["sound_volume"].as_f64().unwrap_or(1.0).clamp(0.0, 1.0) as f32;
    Some((channel, user, sound, volume))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_sound_and_effect_shapes() {
        let sounds = parse_all(&json!([{"sound_id":"12","name":"Airhorn","emoji_name":"📯"},{"name":"missing id"}]));
        assert_eq!(
            sounds,
            vec![Sound {
                id: "12".into(),
                name: "Airhorn".into(),
                emoji: Some("📯".into()),
                source: None
            }]
        );
        assert_eq!(play_body(&sounds[0]), json!({"sound_id":"12","emoji_name":"📯"}));
        assert_eq!(
            audio_url("12").as_deref(),
            Some("https://cdn.discordapp.com/soundboard-sounds/12")
        );
        assert_eq!(audio_url("../x"), None);
        assert_eq!(
            played(&json!({"channel_id":"c","user_id":"u","sound_id":12,"sound_volume":1.5})),
            Some(("c".into(), "u".into(), "12".into(), 1.0))
        );
    }
}
