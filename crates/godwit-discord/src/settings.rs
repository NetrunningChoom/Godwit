// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! The account's status as Discord's user settings keep it, written as the
//! web client writes it: a `PreloadedUserSettings` protobuf in base64, sent to
//! `PATCH /users/@me/settings-proto/1`, of which only `status` is written
//! (by hand, as Yapfast's `settings.rs` does). Each session also tells the
//! gateway its presence.

use base64::Engine;
use serde_json::{Value, json};

/// `PreloadedUserSettings.status`.
const STATUS: u64 = 11;
/// `StatusSettings.status` (a `StringValue`) and `.custom_status`.
const STATUS_STATUS: u64 = 1;
const STATUS_CUSTOM: u64 = 2;
/// `CustomStatus.text`.
const CUSTOM_TEXT: u64 = 1;

const LENGTH_DELIMITED: u64 = 2;

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_bytes(out: &mut Vec<u8>, field: u64, bytes: &[u8]) {
    put_varint(out, field << 3 | LENGTH_DELIMITED);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn status_settings(inner: &[u8]) -> String {
    let mut settings = Vec::new();
    put_bytes(&mut settings, STATUS, inner);
    base64::engine::general_purpose::STANDARD.encode(settings)
}

/// Settings that set the status: `online`, `idle`, `dnd` or `invisible`.
pub fn write_status(status: &str) -> String {
    let mut wrapper = Vec::new();
    put_bytes(&mut wrapper, 1, status.as_bytes());
    let mut inner = Vec::new();
    put_bytes(&mut inner, STATUS_STATUS, &wrapper);
    status_settings(&inner)
}

/// `StatusSettings.show_current_game`, a `BoolValue`.
const STATUS_SHOW_GAME: u64 = 3;

/// Settings that share the game being played (and other activities) in
/// the presence, or stop sharing it.
pub fn write_show_game(shown: bool) -> String {
    let mut wrapper = Vec::new();
    // A BoolValue leaves out false.
    if shown {
        put_varint(&mut wrapper, 1 << 3);
        put_varint(&mut wrapper, 1);
    }
    let mut inner = Vec::new();
    put_bytes(&mut inner, STATUS_SHOW_GAME, &wrapper);
    status_settings(&inner)
}

/// Settings that set the custom status's text, or clear it with `None`.
pub fn write_custom(text: Option<&str>) -> String {
    let mut message = Vec::new();
    if let Some(text) = text {
        put_bytes(&mut message, CUSTOM_TEXT, text.as_bytes());
    }
    let mut inner = Vec::new();
    put_bytes(&mut inner, STATUS_CUSTOM, &message);
    status_settings(&inner)
}

/// `PreloadedUserSettings.guild_folders` and in it `folders` (each a
/// `GuildFolder` whose field 1 lists its servers) and the older `guild_positions`.
const GUILD_FOLDERS: u64 = 14;
const FOLDERS: u64 = 1;
const GUILD_POSITIONS: u64 = 2;
const FOLDER_GUILDS: u64 = 1;

enum Field<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32,
}

fn varint(data: &mut &[u8]) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = data.split_first()?;
        *data = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// A protobuf message's fields, as far as they parse.
fn fields(mut data: &[u8]) -> impl Iterator<Item = (u64, Field<'_>)> {
    std::iter::from_fn(move || {
        let key = varint(&mut data)?;
        let value = match key & 7 {
            0 => Field::Varint(varint(&mut data)?),
            1 => {
                let (bytes, rest) = data.split_first_chunk::<8>()?;
                data = rest;
                Field::Fixed64(u64::from_le_bytes(*bytes))
            }
            2 => {
                let length = usize::try_from(varint(&mut data)?).ok()?;
                let bytes = data.get(..length)?;
                data = data.get(length..)?;
                Field::Bytes(bytes)
            }
            5 => {
                let (_, rest) = data.split_first_chunk::<4>()?;
                data = rest;
                Field::Fixed32
            }
            _ => return None,
        };
        Some((key >> 3, value))
    })
}

/// The last field `number` holding a message.
fn message(data: &[u8], number: u64) -> Option<&[u8]> {
    fields(data)
        .filter_map(|(field, value)| match value {
            Field::Bytes(bytes) if field == number => Some(bytes),
            _ => None,
        })
        .last()
}

/// A wrapper's (`StringValue`) text.
fn wrapped_text(wrapper: &[u8]) -> Option<String> {
    fields(wrapper).find_map(|(n, v)| match v {
        Field::Bytes(text) if n == 1 => Some(String::from_utf8_lossy(text).into_owned()),
        _ => None,
    })
}

/// A repeated `fixed64` field's values: packed, or one by one.
fn fixed64s(value: Field<'_>) -> Vec<u64> {
    match value {
        Field::Fixed64(v) => vec![v],
        Field::Bytes(packed) => packed
            .as_chunks::<8>()
            .0
            .iter()
            .map(|b| u64::from_le_bytes(*b))
            .collect(),
        _ => Vec::new(),
    }
}

/// What the settings say; None where they don't (a partial update keeps
/// the rest as it was).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Read {
    /// `online`, `idle`, `dnd` or `invisible`.
    pub status: Option<String>,
    /// The custom status's text; `Some(None)` when it's cleared.
    pub custom: Option<Option<String>>,
    /// The server list's order, folders flattened.
    pub guild_order: Option<Vec<String>>,
    /// Whether the presence shares activities (games); None when unsaid.
    pub show_game: Option<bool>,
}

/// Reads base64 settings, as READY's `user_settings_proto` or a
/// USER_SETTINGS_PROTO_UPDATE carries them.
pub fn read(encoded: &str) -> Read {
    let Ok(data) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return Read::default();
    };
    let mut out = Read::default();
    if let Some(status) = message(&data, STATUS) {
        for (number, value) in fields(status) {
            match (number, value) {
                (STATUS_STATUS, Field::Bytes(wrapper)) => out.status = wrapped_text(wrapper),
                // A BoolValue: empty is false.
                (STATUS_SHOW_GAME, Field::Bytes(wrapper)) => {
                    out.show_game = Some(fields(wrapper).any(|(n, v)| n == 1 && matches!(v, Field::Varint(1..))));
                }
                (STATUS_CUSTOM, Field::Bytes(custom)) => {
                    let text = fields(custom).find_map(|(n, v)| match v {
                        Field::Bytes(t) if n == CUSTOM_TEXT && !t.is_empty() => {
                            Some(String::from_utf8_lossy(t).into_owned())
                        }
                        _ => None,
                    });
                    out.custom = Some(text);
                }
                _ => {}
            }
        }
    }
    if let Some(folders) = message(&data, GUILD_FOLDERS) {
        let mut order = Vec::new();
        let mut positions = Vec::new();
        for (number, value) in fields(folders) {
            match (number, value) {
                (FOLDERS, Field::Bytes(entry)) => {
                    for (n, v) in fields(entry) {
                        if n == FOLDER_GUILDS {
                            order.extend(fixed64s(v).into_iter().map(|id| id.to_string()));
                        }
                    }
                }
                (GUILD_POSITIONS, value) => positions.extend(fixed64s(value)),
                _ => {}
            }
        }
        if order.is_empty() {
            order = positions.into_iter().map(|id| id.to_string()).collect();
        }
        out.guild_order = Some(order);
    }
    out
}

/// The presence a session sends the gateway (op 3): the status, and the
/// custom status as an activity.
pub fn presence(status: &str, custom: Option<&str>) -> Value {
    let activities: Vec<Value> = custom
        .filter(|t| !t.is_empty())
        .map(|t| json!({"type": 4, "name": "Custom Status", "state": t}))
        .into_iter()
        .collect();
    json!({"status": status, "since": 0, "activities": activities, "afk": false})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(s: &str) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD.decode(s).unwrap()
    }

    #[test]
    fn writes_the_status_field_as_protobuf() {
        // status (field 11) > status (1) > StringValue.value (1) = "dnd".
        assert_eq!(
            decode(&write_status("dnd")),
            vec![0x5a, 7, 0x0a, 5, 0x0a, 3, b'd', b'n', b'd']
        );
        assert_eq!(decode(&write_custom(None)), vec![0x5a, 2, 0x12, 0]);
        assert_eq!(decode(&write_show_game(false)), vec![0x5a, 2, 0x1a, 0]);
        assert_eq!(decode(&write_show_game(true)), vec![0x5a, 4, 0x1a, 2, 0x08, 1]);
        assert_eq!(
            decode(&write_custom(Some("hi"))),
            vec![0x5a, 6, 0x12, 4, 0x0a, 2, b'h', b'i']
        );
        let read_back = read(&write_status("idle"));
        assert_eq!(read_back.status.as_deref(), Some("idle"));
        assert_eq!(read(&write_custom(Some("brb"))).custom, Some(Some("brb".into())));
        assert_eq!(read(&write_custom(None)).custom, Some(None));
        // guild_folders: one folder of two servers (packed fixed64), then one loose.
        let mut folder = Vec::new();
        let mut packed = Vec::new();
        packed.extend_from_slice(&7u64.to_le_bytes());
        packed.extend_from_slice(&5u64.to_le_bytes());
        put_bytes(&mut folder, 1, &packed);
        let mut loose = Vec::new();
        put_bytes(&mut loose, 1, &9u64.to_le_bytes());
        let mut folders = Vec::new();
        put_bytes(&mut folders, 1, &folder);
        put_bytes(&mut folders, 1, &loose);
        let mut settings = Vec::new();
        put_bytes(&mut settings, 14, &folders);
        let encoded = base64::engine::general_purpose::STANDARD.encode(settings);
        assert_eq!(
            read(&encoded).guild_order,
            Some(vec!["7".into(), "5".into(), "9".into()])
        );
        assert_eq!(read("not base64!"), Read::default());
        let p = presence("idle", Some("brb"));
        assert_eq!(p["activities"][0]["state"], "brb");
        assert_eq!(presence("online", None)["activities"].as_array().map(Vec::len), Some(0));
    }
}
