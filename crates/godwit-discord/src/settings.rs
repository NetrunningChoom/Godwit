// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! The account's status as Discord's user settings keep it, written as the
//! web client writes it: a `PreloadedUserSettings` protobuf in base64, sent to
//! `PATCH /users/@me/settings-proto/1`, including the server-folder list
//! (by hand, as Yapfast's `settings.rs` does). Each session also tells the
//! gateway its presence.

use base64::Engine;
use godwit_core::preferences::SyncedPreferences;
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

/// `PreloadedUserSettings.text_and_images` and `.appearance`, with the fields
/// used (discord-protos, MIT: https://github.com/discord-userdoccers/discord-protos
/// and https://docs.discord.food/resources/user-settings-proto).
const TEXT_AND_IMAGES: u64 = 6;
/// `TextAndImagesSettings.convert_emoticons`, a `BoolValue`.
const CONVERT_EMOTICONS: u64 = 21;
const APPEARANCE: u64 = 13;
/// `AppearanceSettings.theme`: an enum (0 unset, 1 dark, 2 light, 3 darker, 4 midnight).
const THEME: u64 = 1;
const THEME_DARK: u64 = 1;
const THEME_LIGHT: u64 = 2;

/// Settings that write the look-and-feel choices that are `Some`.
pub fn write_preferences(prefs: &SyncedPreferences) -> String {
    let mut settings = Vec::new();
    if let Some(convert) = prefs.convert_emoticons {
        let mut wrapper = Vec::new();
        // A BoolValue leaves out false.
        if convert {
            put_varint(&mut wrapper, 1 << 3);
            put_varint(&mut wrapper, 1);
        }
        let mut inner = Vec::new();
        put_bytes(&mut inner, CONVERT_EMOTICONS, &wrapper);
        put_bytes(&mut settings, TEXT_AND_IMAGES, &inner);
    }
    if let Some(dark) = prefs.dark {
        let mut inner = Vec::new();
        put_varint(&mut inner, THEME << 3);
        put_varint(&mut inner, if dark { THEME_DARK } else { THEME_LIGHT });
        put_bytes(&mut settings, APPEARANCE, &inner);
    }
    base64::engine::general_purpose::STANDARD.encode(settings)
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

/// Encodes the complete folder list at field 14 of PreloadedUserSettings.
pub fn write_folder_list(encoded: &str, folders: &[Folder]) -> String {
    let old = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap_or_default();
    let mut root = Vec::new();
    for (number, value) in fields(&old) {
        if number == GUILD_FOLDERS {
            continue;
        }
        match value {
            Field::Varint(v) => {
                put_varint(&mut root, number << 3);
                put_varint(&mut root, v);
            }
            Field::Fixed64(v) => {
                put_varint(&mut root, (number << 3) | 1);
                root.extend_from_slice(&v.to_le_bytes());
            }
            Field::Bytes(v) => put_bytes(&mut root, number, v),
            Field::Fixed32(v) => {
                put_varint(&mut root, (number << 3) | 5);
                root.extend_from_slice(&v);
            }
        }
    }
    let mut list = Vec::new();
    for folder in folders {
        let mut entry = Vec::new();
        let mut packed = Vec::new();
        for id in &folder.guilds {
            if let Ok(id) = id.parse::<u64>() {
                packed.extend_from_slice(&id.to_le_bytes());
            }
        }
        if !packed.is_empty() {
            put_bytes(&mut entry, FOLDER_GUILDS, &packed);
        }
        if let Some(id) = folder.id.as_deref().and_then(|s| s.parse::<i64>().ok()) {
            let mut wrapper = Vec::new();
            put_varint(&mut wrapper, 8);
            put_varint(&mut wrapper, id as u64);
            put_bytes(&mut entry, FOLDER_ID, &wrapper);
        }
        if let Some(name) = folder.name.as_deref().filter(|s| !s.is_empty()) {
            let mut wrapper = Vec::new();
            put_bytes(&mut wrapper, 1, name.as_bytes());
            put_bytes(&mut entry, FOLDER_NAME, &wrapper);
        }
        if let Some(colour) = folder.colour {
            let mut wrapper = Vec::new();
            put_varint(&mut wrapper, 8);
            put_varint(&mut wrapper, u64::from(colour));
            put_bytes(&mut entry, FOLDER_COLOUR, &wrapper);
        }
        put_bytes(&mut list, FOLDERS, &entry);
    }
    put_bytes(&mut root, GUILD_FOLDERS, &list);
    base64::engine::general_purpose::STANDARD.encode(root)
}

/// `PreloadedUserSettings.guild_folders` and in it `folders` (each a
/// `GuildFolder` whose field 1 lists its servers) and the older `guild_positions`.
const GUILD_FOLDERS: u64 = 14;
const FOLDERS: u64 = 1;
const GUILD_POSITIONS: u64 = 2;
const FOLDER_GUILDS: u64 = 1;
/// `GuildFolder.id` (Int64Value), `.name` (StringValue), `.color` (UInt64Value)
/// (docs.discord.food, User Settings, guild folder structure).
const FOLDER_ID: u64 = 2;
const FOLDER_NAME: u64 = 3;
const FOLDER_COLOUR: u64 = 4;

enum Field<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32([u8; 4]),
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
                let (bytes, rest) = data.split_first_chunk::<4>()?;
                data = rest;
                Field::Fixed32(*bytes)
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

/// A wrapper's (`Int64Value`, `UInt64Value`) number.
fn wrapped_varint(wrapper: &[u8]) -> Option<u64> {
    fields(wrapper).find_map(|(n, v)| match v {
        Field::Varint(i) if n == 1 => Some(i),
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

/// One entry of `guild_folders`: a folder, or (without an `id`) a server on
/// its own, which Discord keeps in the same list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Folder {
    pub id: Option<String>,
    pub name: Option<String>,
    /// 0xRRGGBB.
    pub colour: Option<u32>,
    pub guilds: Vec<String>,
}

/// The legacy JSON form of the list (READY's `user_settings.guild_folders`:
/// `id` null for a lone guild, `color` an integer, `guild_ids` strings).
pub fn folders_from_json(list: &Value) -> Vec<Folder> {
    list.as_array()
        .into_iter()
        .flatten()
        .map(|f| Folder {
            id: f["id"].as_u64().map(|i| i.to_string()),
            name: f["name"].as_str().filter(|n| !n.is_empty()).map(str::to_owned),
            colour: f["color"]
                .as_u64()
                .and_then(|c| u32::try_from(c & 0xff_ffff).ok())
                .filter(|c| *c != 0),
            guilds: f["guild_ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|g| {
                    g.as_str()
                        .map(str::to_owned)
                        .or_else(|| g.as_u64().map(|n| n.to_string()))
                })
                .collect(),
        })
        .collect()
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
    /// The server list's folders, in order (a lone server is one without an id).
    pub folders: Option<Vec<Folder>>,
    /// Whether the presence shares activities (games); None when unsaid.
    pub show_game: Option<bool>,
    /// Theme and emoticon conversion, where the settings carry them.
    pub prefs: SyncedPreferences,
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
    if let Some(text) = message(&data, TEXT_AND_IMAGES) {
        for (number, value) in fields(text) {
            if let (CONVERT_EMOTICONS, Field::Bytes(wrapper)) = (number, value) {
                out.prefs.convert_emoticons =
                    Some(fields(wrapper).any(|(n, v)| n == 1 && matches!(v, Field::Varint(1..))));
            }
        }
    }
    if let Some(appearance) = message(&data, APPEARANCE) {
        // No theme is the client's default, which says nothing; the other
        // dark variants are dark too.
        let theme = fields(appearance).find_map(|(n, v)| match v {
            Field::Varint(t) if n == THEME => Some(t),
            _ => None,
        });
        out.prefs.dark = theme.filter(|t| *t != 0).map(|t| t != THEME_LIGHT);
    }
    if let Some(folders) = message(&data, GUILD_FOLDERS) {
        let mut order = Vec::new();
        let mut positions = Vec::new();
        let mut listed = Vec::new();
        for (number, value) in fields(folders) {
            match (number, value) {
                (FOLDERS, Field::Bytes(entry)) => {
                    let mut folder = Folder::default();
                    for (n, v) in fields(entry) {
                        match (n, v) {
                            (FOLDER_GUILDS, v) => {
                                let ids: Vec<String> = fixed64s(v).into_iter().map(|id| id.to_string()).collect();
                                order.extend(ids.iter().cloned());
                                folder.guilds.extend(ids);
                            }
                            (FOLDER_ID, Field::Bytes(w)) => folder.id = wrapped_varint(w).map(|i| i.to_string()),
                            (FOLDER_NAME, Field::Bytes(w)) => folder.name = wrapped_text(w).filter(|t| !t.is_empty()),
                            (FOLDER_COLOUR, Field::Bytes(w)) => {
                                folder.colour = wrapped_varint(w)
                                    .and_then(|c| u32::try_from(c & 0xff_ffff).ok())
                                    .filter(|c| *c != 0);
                            }
                            _ => {}
                        }
                    }
                    listed.push(folder);
                }
                (GUILD_POSITIONS, value) => positions.extend(fixed64s(value)),
                _ => {}
            }
        }
        if order.is_empty() {
            order = positions.into_iter().map(|id| id.to_string()).collect();
        }
        out.guild_order = Some(order);
        out.folders = Some(listed);
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

/// Test helper: base64 settings holding folders of (id, servers).
#[cfg(test)]
pub(crate) fn test_folders_proto(folders: &[(u64, &[u64])]) -> String {
    let mut list = Vec::new();
    for (id, guilds) in folders {
        let mut entry = Vec::new();
        let packed: Vec<u8> = guilds.iter().flat_map(|g| g.to_le_bytes()).collect();
        put_bytes(&mut entry, FOLDER_GUILDS, &packed);
        let mut wrapper = Vec::new();
        put_varint(&mut wrapper, 1 << 3);
        put_varint(&mut wrapper, *id);
        put_bytes(&mut entry, FOLDER_ID, &wrapper);
        put_bytes(&mut list, FOLDERS, &entry);
    }
    let mut settings = Vec::new();
    put_bytes(&mut settings, GUILD_FOLDERS, &list);
    base64::engine::general_purpose::STANDARD.encode(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_are_written_and_read_back() {
        let prefs = SyncedPreferences {
            dark: Some(false),
            convert_emoticons: Some(true),
        };
        let read = read(&write_preferences(&prefs));
        assert_eq!(read.prefs, prefs);
        assert_eq!(read.status, None);
    }

    #[test]
    fn false_emoticons_is_an_empty_wrapper_and_dark_is_theme_one() {
        let prefs = SyncedPreferences {
            dark: Some(true),
            convert_emoticons: Some(false),
        };
        let encoded = write_preferences(&prefs);
        let bytes = base64::engine::general_purpose::STANDARD.decode(&encoded).unwrap();
        // text_and_images { convert_emoticons {} }, appearance { theme: 1 }.
        assert_eq!(bytes, [0x32, 3, 0xaa, 1, 0, 0x6a, 2, 8, 1]);
        assert_eq!(read(&encoded).prefs, prefs);
    }

    #[test]
    fn a_partial_write_names_only_what_changed() {
        let only_theme = SyncedPreferences {
            dark: Some(true),
            ..Default::default()
        };
        let read = read(&write_preferences(&only_theme));
        assert_eq!(read.prefs.dark, Some(true));
        assert_eq!(read.prefs.convert_emoticons, None);
        assert!(write_preferences(&SyncedPreferences::default()).is_empty());
    }

    #[test]
    fn other_dark_themes_read_as_dark_and_light_as_not() {
        let theme = |value: u64| {
            let mut inner = Vec::new();
            put_varint(&mut inner, THEME << 3);
            put_varint(&mut inner, value);
            let mut settings = Vec::new();
            put_bytes(&mut settings, APPEARANCE, &inner);
            read(&base64::engine::general_purpose::STANDARD.encode(settings))
                .prefs
                .dark
        };
        assert_eq!(theme(4), Some(true));
        assert_eq!(theme(2), Some(false));
    }

    #[test]
    fn an_edited_folder_list_round_trips_and_keeps_other_fields() {
        // A status, plus a field we don't know (99, a varint, and 98, a fixed32).
        let mut old = Vec::new();
        let mut wrapper = Vec::new();
        put_bytes(&mut wrapper, 1, b"dnd");
        let mut status = Vec::new();
        put_bytes(&mut status, STATUS_STATUS, &wrapper);
        put_bytes(&mut old, STATUS, &status);
        put_varint(&mut old, 99 << 3);
        put_varint(&mut old, 7);
        put_varint(&mut old, (98 << 3) | 5);
        old.extend_from_slice(&[1, 2, 3, 4]);
        let old = base64::engine::general_purpose::STANDARD.encode(old);
        let folders = vec![
            Folder {
                id: Some("9007199254740991".into()),
                name: Some("Games".into()),
                colour: Some(0x5470ec),
                guilds: vec!["111".into(), "18446744073709551".into()],
            },
            Folder {
                guilds: vec!["333".into()],
                ..Folder::default()
            },
        ];
        let new = write_folder_list(&old, &folders);
        let read = read(&new);
        assert_eq!(read.folders.as_deref(), Some(folders.as_slice()));
        assert_eq!(
            read.guild_order,
            Some(vec!["111".into(), "18446744073709551".into(), "333".into()])
        );
        assert_eq!(read.status.as_deref(), Some("dnd"), "fields it doesn't own are kept");
        let bytes = decode(&new);
        assert!(fields(&bytes).any(|(n, v)| n == 99 && matches!(v, Field::Varint(7))));
        assert!(fields(&bytes).any(|(n, v)| n == 98 && matches!(v, Field::Fixed32([1, 2, 3, 4]))));
        // Writing again over the result replaces the list, not appends to it.
        let again = write_folder_list(&new, &folders[..1]);
        assert_eq!(read_folders(&again).len(), 1);
    }

    fn read_folders(proto: &str) -> Vec<Folder> {
        read(proto).folders.unwrap_or_default()
    }

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
        // Folder details: id, name and colour wrappers; the lone guild has none.
        let mut named = Vec::new();
        let mut ids = Vec::new();
        ids.extend_from_slice(&7u64.to_le_bytes());
        ids.extend_from_slice(&5u64.to_le_bytes());
        put_bytes(&mut named, 1, &ids);
        put_bytes(&mut named, FOLDER_ID, &[0x08, 42]);
        put_bytes(&mut named, FOLDER_NAME, &[0x0a, 2, b'h', b'i']);
        put_bytes(&mut named, FOLDER_COLOUR, &[0x08, 0xff, 0xff, 0xff, 0x07]);
        let mut lone = Vec::new();
        put_bytes(&mut lone, 1, &9u64.to_le_bytes());
        let mut list = Vec::new();
        put_bytes(&mut list, 1, &named);
        put_bytes(&mut list, 1, &lone);
        let mut settings = Vec::new();
        put_bytes(&mut settings, 14, &list);
        let read = read(&base64::engine::general_purpose::STANDARD.encode(settings));
        assert_eq!(
            read.folders,
            Some(vec![
                Folder {
                    id: Some("42".into()),
                    name: Some("hi".into()),
                    colour: Some(0xff_ffff),
                    guilds: vec!["7".into(), "5".into()]
                },
                Folder {
                    guilds: vec!["9".into()],
                    ..Folder::default()
                },
            ])
        );
    }

    #[test]
    fn a_folder_with_an_id_but_one_guild_is_read_as_listed() {
        let mut one = Vec::new();
        put_bytes(&mut one, 1, &9u64.to_le_bytes());
        put_bytes(&mut one, FOLDER_ID, &[0x08, 7]);
        put_bytes(&mut one, FOLDER_COLOUR, &[0x08, 0]);
        let mut list = Vec::new();
        put_bytes(&mut list, 1, &one);
        let mut settings = Vec::new();
        put_bytes(&mut settings, 14, &list);
        let folders = read(&base64::engine::general_purpose::STANDARD.encode(settings))
            .folders
            .unwrap();
        assert_eq!(folders[0].id.as_deref(), Some("7"));
        assert_eq!(folders[0].colour, None, "0 is no colour");
        // The store keeps it plain: a folder needs two servers.
        let mut store = crate::store::Store::default();
        store.set_folders(&folders);
        assert!(store.folder_of.is_empty());
    }

    #[test]
    fn reads_the_json_folder_list() {
        let folders = folders_from_json(&json!([
            {"id": 3, "name": "", "color": 255, "guild_ids": ["1", "2"]},
            {"id": null, "name": null, "color": null, "guild_ids": ["9"]}
        ]));
        assert_eq!(folders[0].id.as_deref(), Some("3"));
        assert_eq!(folders[0].name, None);
        assert_eq!(folders[0].colour, Some(255));
        assert_eq!(folders[1].id, None);
        assert_eq!(folders[1].guilds, vec!["9".to_owned()]);
    }

    #[test]
    fn presence_has_activities() {
        let p = presence("idle", Some("brb"));
        assert_eq!(p["activities"][0]["state"], "brb");
        assert_eq!(presence("online", None)["activities"].as_array().map(Vec::len), Some(0));
    }
}
