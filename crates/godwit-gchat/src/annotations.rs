// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Cheap inline annotation conversion. Chat indexes UTF-16 code units (as
//! mautrix-googlechat's formatter does), not Rust UTF-8 bytes. Invalid or
//! overlapping ranges degrade to ordinary text.

use crate::proto;
use godwit_core::{media::https, rich};

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
}
