// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// The multipart form body is adapted from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! A server's stickers and soundboard sounds: list parsing, local file checks
//! and request bodies. Endpoints and limits are from docs.discord.food:
//! <https://docs.discord.food/resources/sticker> (Create Guild Sticker) and
//! <https://docs.discord.food/resources/soundboard> (Create Guild Soundboard Sound).

use godwit_core::server_settings::{EmojiRules, ExpressionKind, ServerEmoji, ServerEmojis};
use serde_json::{Value, json};
use std::collections::HashMap;

/// A sticker file is at most 500 KiB.
pub const STICKER_MAX_BYTES: u64 = 500 << 10;
/// A sound is at most 512 KiB (and 5.2 s, which only Discord can measure).
pub const SOUND_MAX_BYTES: u64 = 512 << 10;

pub fn max_bytes(kind: ExpressionKind) -> u64 {
    match kind {
        ExpressionKind::Sticker => STICKER_MAX_BYTES,
        ExpressionKind::Sound => SOUND_MAX_BYTES,
    }
}

/// Names are 2 to 30 (stickers) or 32 (sounds) characters, free text.
pub fn rules(kind: ExpressionKind) -> EmojiRules {
    EmojiRules {
        min_len: 2,
        max_len: match kind {
            ExpressionKind::Sticker => 30,
            ExpressionKind::Sound => 32,
        },
        uppercase: true,
        any_chars: true,
    }
}

/// What is known of a listed item besides what the core shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listed {
    pub list: ServerEmojis,
    /// Item id → uploader's user id, for the Create-only rule.
    pub owners: HashMap<String, String>,
}

/// The answer of `GET /guilds/{id}/stickers` (an array) or
/// `GET /guilds/{id}/soundboard-sounds` (`{items: [...]}`), newest first.
/// The uploader is present only for accounts with an expressions permission.
pub fn parse_list(kind: ExpressionKind, value: &Value) -> Listed {
    let items = value.get("items").unwrap_or(value);
    let mut out = Listed::default();
    for item in items.as_array().into_iter().flatten() {
        let id_key = if kind == ExpressionKind::Sound {
            "sound_id"
        } else {
            "id"
        };
        let Some(id) = item[id_key]
            .as_str()
            .map(str::to_owned)
            .or_else(|| item[id_key].as_u64().map(|n| n.to_string()))
        else {
            continue;
        };
        let Some(name) = item["name"].as_str() else { continue };
        let user = &item["user"];
        let creator = user["global_name"]
            .as_str()
            .or_else(|| user["username"].as_str())
            .unwrap_or_default();
        if let Some(uid) = user["id"].as_str().or_else(|| item["user_id"].as_str()) {
            out.owners.insert(id.clone(), uid.to_owned());
        }
        let url = match kind {
            ExpressionKind::Sticker => crate::store::sticker_url(&id, item["format_type"].as_u64().unwrap_or(1), 96),
            // Sounds have no picture.
            ExpressionKind::Sound => None,
        };
        out.list.emoji.push(ServerEmoji {
            id: id.into(),
            name: name.into(),
            url: url.map(Into::into),
            creator: creator.into(),
        });
    }
    // Snowflakes sort by age: newest first.
    out.list
        .emoji
        .sort_by(|a, b| b.id.len().cmp(&a.id.len()).then_with(|| b.id.cmp(&a.id)));
    out.list.max_file_bytes = Some(max_bytes(kind));
    out.list.rules = rules(kind);
    out.list.kind = Some(kind);
    out
}

/// A file's type from its first bytes, with the file name Discord's form
/// wants: PNG (APNG shares its signature), GIF or Lottie JSON for a sticker;
/// MP3 or Ogg for a sound.
pub fn sniff(kind: ExpressionKind, bytes: &[u8]) -> Option<&'static str> {
    match kind {
        ExpressionKind::Sticker => match bytes {
            [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
            [b'G', b'I', b'F', ..] => Some("image/gif"),
            [b'{', ..] => Some("application/json"),
            _ => None,
        },
        ExpressionKind::Sound => match bytes {
            [b'O', b'g', b'g', b'S', ..] => Some("audio/ogg"),
            [b'I', b'D', b'3', ..] => Some("audio/mpeg"),
            // An MP3 frame header: 11 sync bits.
            [0xff, b, ..] if b & 0xe0 == 0xe0 => Some("audio/mpeg"),
            _ => None,
        },
    }
}

/// Why `bytes` can't be uploaded as this kind, checked before anything is sent.
pub fn check_file(kind: ExpressionKind, bytes: &[u8]) -> Result<&'static str, String> {
    let noun = kind.noun();
    if bytes.is_empty() || bytes.len() as u64 > max_bytes(kind) {
        return Err(format!("A {noun} file is at most {} KB.", max_bytes(kind) >> 10));
    }
    sniff(kind, bytes).ok_or_else(|| match kind {
        ExpressionKind::Sticker => "Choose a PNG, APNG, GIF or Lottie file.".to_owned(),
        ExpressionKind::Sound => "Choose an MP3 or Ogg file.".to_owned(),
    })
}

/// Create Guild Soundboard Sound's JSON body: `sound` is a data URI.
pub fn sound_body(name: &str, mime: &str, bytes: &[u8]) -> Value {
    use base64::Engine;
    json!({
        "name": name,
        "sound": format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)),
        "volume": 1.0,
    })
}

/// Create Guild Sticker's form fields: the name doubles as the tags (they
/// are required, 1 to 200 characters) and the description stays empty.
pub fn sticker_fields(name: &str) -> [(&'static str, String); 3] {
    let tags: String = name.chars().take(200).collect();
    [
        ("name", name.to_owned()),
        ("tags", tags),
        ("description", String::new()),
    ]
}

/// A `multipart/form-data` body of text `fields` and a file, with its content type.
pub fn form(fields: &[(&str, String)], file: (&str, &str, &[u8])) -> (String, Vec<u8>) {
    let boundary = format!("godwit{:016x}", rand::random::<u64>());
    let mut body = Vec::with_capacity(file.2.len() + 512);
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
        );
    }
    let (filename, mime, data) = file;
    let filename = filename.replace(['"', '\r', '\n', '\\'], "_");
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: {mime}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// The Modify body: only the name changes here.
pub fn rename_body(name: &str) -> Value {
    json!({"name": name})
}

/// Whether Create Expressions alone allows changing an item: only our own;
/// Manage Expressions covers any (docs.discord.food, Modify Guild Sticker).
pub fn change_allowed(perms: u64, own: bool) -> Result<(), &'static str> {
    use crate::permissions::{CREATE_GUILD_EXPRESSIONS, MANAGE_GUILD_EXPRESSIONS};
    if perms & MANAGE_GUILD_EXPRESSIONS != 0 || (perms & CREATE_GUILD_EXPRESSIONS != 0 && own) {
        Ok(())
    } else {
        Err("You can only change what you added unless you can manage expressions.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stickers_list_newest_first_with_pictures_and_owners() {
        let l = parse_list(
            ExpressionKind::Sticker,
            &json!([
                {"id":"5","name":"Old","format_type":1,"user":{"id":"u1","username":"ann"}},
                {"id":"15","name":"New","format_type":4,"user":{"id":"u2","username":"bo","global_name":"Bo"}},
                {"id":"16","name":"Lot","format_type":3},
                {"name":"no id"}
            ]),
        );
        let names: Vec<_> = l.list.emoji.iter().map(|e| e.name.as_ref()).collect();
        assert_eq!(names, ["Lot", "New", "Old"]);
        assert_eq!(l.list.emoji[1].creator.as_ref(), "Bo");
        assert!(
            l.list.emoji[1]
                .url
                .as_deref()
                .is_some_and(|u| u.contains("/stickers/15.gif"))
        );
        assert_eq!(l.list.emoji[0].url, None);
        assert_eq!(l.owners.get("5").map(String::as_str), Some("u1"));
        assert_eq!(l.list.kind, Some(ExpressionKind::Sticker));
        assert_eq!(l.list.max_file_bytes, Some(500 << 10));
    }

    #[test]
    fn sounds_list_reads_the_items_wrapper() {
        let l = parse_list(
            ExpressionKind::Sound,
            &json!({"items":[{"sound_id":"7","name":"Horn","volume":1.0,"user":{"id":"u","username":"x"}}]}),
        );
        assert_eq!(l.list.emoji.len(), 1);
        assert_eq!(l.list.emoji[0].id.as_ref(), "7");
        assert_eq!(l.list.emoji[0].url, None);
        assert_eq!(l.list.rules.max_len, 32);
        assert!(l.list.rules.validate_name("Air horn!").is_ok());
        assert!(l.list.rules.validate_name("a").is_err());
    }

    #[test]
    fn files_are_checked_by_content_and_size() {
        let png = [0x89, b'P', b'N', b'G', 1, 2];
        assert_eq!(check_file(ExpressionKind::Sticker, &png), Ok("image/png"));
        assert!(check_file(ExpressionKind::Sticker, b"nope").is_err());
        assert!(check_file(ExpressionKind::Sticker, &[]).is_err());
        let big = vec![0x89u8; (STICKER_MAX_BYTES + 1) as usize];
        assert!(check_file(ExpressionKind::Sticker, &big).is_err());
        assert_eq!(check_file(ExpressionKind::Sound, b"ID3\x04"), Ok("audio/mpeg"));
        assert_eq!(check_file(ExpressionKind::Sound, &[0xff, 0xfb, 0x90]), Ok("audio/mpeg"));
        assert_eq!(check_file(ExpressionKind::Sound, b"OggS\0"), Ok("audio/ogg"));
        assert!(check_file(ExpressionKind::Sound, &png).is_err());
        let big = vec![b'O'; (SOUND_MAX_BYTES + 1) as usize];
        assert!(check_file(ExpressionKind::Sound, &big).is_err());
    }

    #[test]
    fn bodies_have_the_documented_shapes() {
        let b = sound_body("Horn", "audio/mpeg", b"abc");
        assert_eq!(b["name"], "Horn");
        assert_eq!(b["sound"], "data:audio/mpeg;base64,YWJj");
        assert_eq!(b["volume"], 1.0);
        let fields = sticker_fields("Wave");
        let (ct, body) = form(&fields, ("a\"b.png", "image/png", b"DATA"));
        assert!(ct.starts_with("multipart/form-data; boundary="));
        let text = String::from_utf8_lossy(&body);
        let (n, t, d, f) = (
            text.find("name=\"name\"").unwrap_or(usize::MAX),
            text.find("name=\"tags\"").unwrap_or(usize::MAX),
            text.find("name=\"description\"").unwrap_or(usize::MAX),
            text.find("name=\"file\"").unwrap_or(usize::MAX),
        );
        assert!(n < t && t < d && d < f);
        assert!(text.contains("filename=\"a_b.png\"") && text.contains("Content-Type: image/png\r\n\r\nDATA\r\n"));
        assert!(text.trim_end().ends_with("--"));
        assert_eq!(rename_body("X"), json!({"name":"X"}));
    }

    #[test]
    fn create_only_changes_just_our_own() {
        use crate::permissions::{CREATE_GUILD_EXPRESSIONS as C, MANAGE_GUILD_EXPRESSIONS as M};
        assert!(change_allowed(M, false).is_ok());
        assert!(change_allowed(C, true).is_ok());
        assert!(change_allowed(C, false).is_err());
        assert!(change_allowed(0, true).is_err());
    }
}
