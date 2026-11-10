// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Slack's private file URLs need the workspace token and web session cookie
//! (https://api.slack.com/types/file). External/remote file URLs
//! are allowed, but never receive either credential.

use crate::session::Saved;
use godwit_core::media::{Media, Rule, https, kind};
use godwit_core::message::{Attachment, AttachmentKind};
use serde_json::Value;

pub fn configure(media: &Media, saved: &Saved) {
    media.clear();
    for team in &saved.teams {
        media.set(
            &team.id,
            vec![Rule::new(
                &["slack.com", "slack-files.com"],
                vec![
                    ("Authorization", format!("Bearer {}", team.token)),
                    ("Cookie", format!("d={}", saved.d)),
                ],
            )],
        );
    }
}

pub fn attachments(message: &Value, media: &Media, team: &str) -> Vec<Attachment> {
    message["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|file| {
            let name = file["name"].as_str().unwrap_or("file");
            let Some(original) = file["url_private_download"]
                .as_str()
                .or_else(|| file["url_private"].as_str())
            else {
                return Some(Attachment {
                    name: name.into(),
                    url: https(file["permalink"].as_str()?)?.to_string().into(),
                    kind: AttachmentKind::File,
                    size: file["size"].as_u64().unwrap_or(0),
                    dimensions: None,
                    original_url: None,
                    spoiler: false,
                    voice: None,
                });
            };
            let kind = kind(file["mimetype"].as_str().unwrap_or_default());
            let preview = if kind == AttachmentKind::Image {
                ["thumb_1024", "thumb_720", "thumb_480", "url_private"]
                    .iter()
                    .find_map(|key| file[*key].as_str())
                    .unwrap_or(original)
            } else {
                original
            };
            let dim = |key: &str| {
                file[key]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n > 0)
            };
            Some(Attachment {
                name: name.into(),
                url: media.address(team, preview)?.into(),
                original_url: media.address(team, original).map(Into::into),
                spoiler: false,
                voice: None,
                kind,
                size: file["size"].as_u64().unwrap_or(0),
                dimensions: dim("original_w").zip(dim("original_h")),
            })
        })
        .collect()
}

pub fn shared_timestamp(file: &Value, channel: &str) -> Option<String> {
    ["public", "private"].into_iter().find_map(|kind| {
        file["shares"][kind][channel]
            .as_array()?
            .iter()
            .find_map(|share| share["ts"].as_str().filter(|ts| !ts.is_empty()).map(str::to_owned))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn uploaded_files_confirm_against_their_own_channel() {
        let file = json!({"shares":{"private":{"D1":[{"ts":"1.000001"}]},"public":{"C1":[{"ts":"2.000001"}]}}});
        assert_eq!(shared_timestamp(&file, "D1").as_deref(), Some("1.000001"));
        assert_eq!(shared_timestamp(&file, "C1").as_deref(), Some("2.000001"));
        assert_eq!(shared_timestamp(&file, "C2"), None);
    }

    #[test]
    fn private_images_get_previews_originals_and_dimensions() {
        let files = attachments(
            &json!({"files":[{"name":"image.png","mimetype":"image/png","url_private_download":"https://files.slack.com/original","thumb_480":"https://files.slack.com/preview","original_w":640,"original_h":480}]}),
            &Media::new("slack"),
            "T1",
        );
        assert_eq!(files[0].kind, AttachmentKind::Image);
        assert_eq!(files[0].dimensions, Some((640, 480)));
        assert_ne!(files[0].original_url.as_deref(), Some(files[0].url.as_ref()));
        assert!(files[0].url.starts_with("slack-media://"));
    }

    #[test]
    fn private_non_image_files_keep_name_and_size() {
        let files = attachments(
            &json!({"files":[{"name":"notes.txt","mimetype":"text/plain","size":321,"url_private":"https://files.slack.com/private/notes"}]}),
            &Media::new("slack"),
            "T1",
        );
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name.as_ref(), "notes.txt");
        assert_eq!(files[0].size, 321);
        assert_eq!(files[0].kind, AttachmentKind::File);
        assert!(files[0].url.starts_with("slack-media://"));
    }
}
