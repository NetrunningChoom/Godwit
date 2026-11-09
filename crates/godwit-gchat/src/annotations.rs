// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Cheap inline annotation conversion. Chat indexes UTF-16 code units (as
//! mautrix-googlechat's formatter does), not Rust UTF-8 bytes. Invalid or
//! overlapping ranges degrade to ordinary text.

use crate::proto;
use godwit_core::{media::https, rich};

/// Google Chat accepts annotation metadata on both create methods, with
/// `start_index` and `length` measured in UTF-16 code units. The send shape
/// follows mautrix-googlechat's `maugclib.Client.send_message`:
/// https://github.com/mautrix/googlechat/blob/master/maugclib/client.py
pub fn outgoing(
    text: &str,
    users: &std::collections::HashMap<String, (String, Option<String>)>,
) -> (String, Vec<proto::Annotation>) {
    let mut body = String::new();
    let mut annotations = Vec::new();
    let mut cursor = 0;
    let mut units = 0i32;
    while cursor < text.len() {
        let rest = &text[cursor..];
        let Some(after_at) = rest.strip_prefix("<@gchat:") else {
            let Some(c) = rest.chars().next() else { break };
            body.push(c);
            units = units.saturating_add(i32::try_from(c.len_utf16()).unwrap_or(1));
            cursor += c.len_utf8();
            continue;
        };
        let Some(end) = after_at.find('>') else {
            body.push('<');
            cursor += 1;
            units = units.saturating_add(1);
            continue;
        };
        let id = &after_at[..end];
        let Some((name, _)) = users.get(id) else {
            body.push('<');
            cursor += 1;
            units = units.saturating_add(1);
            continue;
        };
        let visible = format!("@{name}");
        let start = units;
        body.push_str(&visible);
        units = units.saturating_add(i32::try_from(visible.encode_utf16().count()).unwrap_or(i32::MAX));
        let mut annotation = proto::Annotation::new();
        annotation.set_type(proto::AnnotationType::USER_MENTION);
        annotation.set_start_index(start);
        annotation.set_length(units.saturating_sub(start));
        let mut mention = proto::UserMentionMetadata::new();
        let mut user_id = proto::UserId::new();
        user_id.set_id(id.to_owned());
        mention.id = Some(user_id).into();
        mention.set_type(proto::user_mention_metadata::Type::MENTION);
        mention.set_display_name(name.clone());
        annotation.set_user_mention_metadata(mention);
        annotations.push(annotation);
        cursor += "<@gchat:".len() + end + 1;
    }
    (body, annotations)
}

#[cfg(test)]
pub fn mentions_user(message: &proto::Message, user_id: &str) -> bool {
    !user_id.is_empty()
        && message.annotations.iter().any(|annotation| {
            annotation.has_user_mention_metadata()
                && annotation.user_mention_metadata().id.id() == user_id
                && annotation.user_mention_metadata().type_() == proto::user_mention_metadata::Type::MENTION
        })
}

fn byte_offset(text: &str, at: i32) -> Option<usize> {
    let at = usize::try_from(at).ok()?;
    let mut units = 0;
    for (byte, c) in text.char_indices() {
        if units == at {
            return Some(byte);
        }
        units += c.len_utf16();
        if units > at {
            return None;
        }
    }
    (units == at).then_some(text.len())
}

pub fn text(message: &proto::Message) -> String {
    let source = message.text_body();
    let mut links = Vec::new();
    for annotation in &message.annotations {
        let url = if annotation.has_user_mention_metadata() {
            let user = annotation.user_mention_metadata();
            if user.id.id().is_empty() {
                continue;
            }
            format!("godwit://user/{}", godwit_core::encoding::percent_encode(user.id.id()))
        } else if annotation.has_url_metadata() {
            let Some(url) = https(annotation.url_metadata().url.url()) else {
                continue;
            };
            url.to_string()
        } else if annotation.has_drive_metadata() {
            let drive = annotation.drive_metadata();
            if drive.id().is_empty() {
                continue;
            }
            format!(
                "https://drive.google.com/open?id={}",
                godwit_core::encoding::percent_encode(drive.id())
            )
        } else {
            continue;
        };
        let Some(end) = annotation.start_index().checked_add(annotation.length()) else {
            continue;
        };
        let (Some(start), Some(end)) = (byte_offset(source, annotation.start_index()), byte_offset(source, end)) else {
            continue;
        };
        if start < end {
            links.push((start, end, url));
        }
    }
    links.sort_by_key(|(start, end, _)| (*start, *end));
    let mut out = String::new();
    let mut at = 0;
    for (start, end, url) in links {
        if start < at {
            continue;
        }
        out.push_str(&rich::escape(&source[at..start]));
        out.push_str(&format!(
            "[{}](<{}>)",
            rich::escape(&source[start..end]),
            url.replace(['<', '>'], "")
        ));
        at = end;
    }
    out.push_str(&rich::escape(&source[at..]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_links_and_invalid_ranges_degrade_to_plain_text() {
        let mut m = proto::Message::new();
        m.set_text_body("click".into());
        let mut url = proto::Url::new();
        url.set_url("javascript:alert(1)".into());
        let mut meta = proto::UrlMetadata::new();
        meta.url = Some(url).into();
        let mut a = proto::Annotation::new();
        a.set_url_metadata(meta);
        a.set_start_index(0);
        a.set_length(5);
        m.annotations.push(a.clone());
        assert_eq!(text(&m), "click");
        let mut url = proto::Url::new();
        url.set_url("https://example.org/".into());
        let mut meta = proto::UrlMetadata::new();
        meta.url = Some(url).into();
        a.set_url_metadata(meta);
        a.set_start_index(-1);
        m.annotations = vec![a];
        assert_eq!(text(&m), "click");
    }

    #[test]
    fn mentions_after_astral_characters_use_utf16_offsets() {
        let mut m = proto::Message::new();
        m.set_text_body("😀 @Ann!".into());
        let mut a = proto::Annotation::new();
        a.set_start_index(3);
        a.set_length(4);
        let mut mention = proto::UserMentionMetadata::new();
        let mut id = proto::UserId::new();
        id.set_id("123".into());
        mention.id = Some(id).into();
        a.set_user_mention_metadata(mention);
        m.annotations.push(a.clone());
        // Rich text escapes `@` and `!` outside links (rich::escape).
        assert_eq!(text(&m), r"😀 [\@Ann](<godwit://user/123>)\!");
        a.set_start_index(1);
        m.annotations = vec![a];
        assert_eq!(text(&m), r"😀 \@Ann\!");
    }

    #[test]
    fn token_becomes_display_text_and_utf16_annotation() {
        let users = std::collections::HashMap::from([("u1".into(), ("Ann😀".into(), None))]);
        let (body, annotations) = outgoing("😀 hi <@gchat:u1>!", &users);
        assert_eq!(body, "😀 hi @Ann😀!");
        assert_eq!(annotations.len(), 1);
        assert_eq!(annotations[0].start_index(), 6);
        assert_eq!(annotations[0].length(), 6);
        assert_eq!(annotations[0].user_mention_metadata().id.id(), "u1");
        assert_eq!(outgoing(&body, &users).0, body);
    }

    #[test]
    fn detects_a_received_mention_of_self() {
        let mut message = proto::Message::new();
        let (_, mut annotations) = outgoing(
            "<@gchat:me>",
            &std::collections::HashMap::from([("me".into(), ("Me".into(), None))]),
        );
        message.annotations.append(&mut annotations);
        assert!(mentions_user(&message, "me"));
        assert!(!mentions_user(&message, "other"));
    }
}
