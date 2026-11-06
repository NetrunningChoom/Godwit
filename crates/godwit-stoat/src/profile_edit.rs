// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Settings → Profile's DataEditUser patch, following Godwit's ProfileForm
//! and the official UserProfileEditor. Unchanged fields are never sent.

use std::collections::HashMap;

use godwit_core::own_profile::{ProfileEdit, ProfileImage};
use serde_json::{Value, json};

use crate::backend::{Upload, UploadSource};

pub struct Prepared {
    pub body: Value,
    pub avatar: Option<Upload>,
    pub banner: Option<Upload>,
}

pub fn prepare(edit: ProfileEdit, limits: &HashMap<String, u64>) -> Result<Prepared, String> {
    // Official UserProfileEditor limits (also used in Godwit's editor).
    if edit.display_name.as_ref().is_some_and(|s| s.chars().count() > 32) {
        return Err("Display names can contain at most 32 characters.".into());
    }
    if edit.pronouns.as_ref().is_some_and(|s| s.chars().count() > 24) {
        return Err("Pronouns can contain at most 24 characters.".into());
    }
    let mut body = json!({});
    let mut remove = Vec::new();
    for (field, removal, text) in [
        ("display_name", "DisplayName", edit.display_name),
        ("pronouns", "Pronouns", edit.pronouns),
    ] {
        if let Some(text) = text {
            if text.trim().is_empty() {
                remove.push(removal);
            } else {
                body[field] = text.trim().into();
            }
        }
    }
    if let Some(bio) = edit.bio {
        if bio.trim().is_empty() {
            remove.push("ProfileContent");
        } else {
            body["profile"]["content"] = bio.as_ref().into();
        }
    }
    let mut picture = |choice: ProfileImage, tag: &str, removal| -> Result<Option<Upload>, String> {
        match choice {
            ProfileImage::Keep => Ok(None),
            ProfileImage::Remove => {
                remove.push(removal);
                Ok(None)
            }
            ProfileImage::File { path, size } => {
                if limits.get(tag).is_some_and(|limit| size > *limit) {
                    return Err(format!("The selected image exceeds this server's limit for {tag}."));
                }
                Ok(Some(Upload {
                    name: path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("image")
                        .to_owned(),
                    size,
                    source: UploadSource::Path(path),
                }))
            }
        }
    };
    let avatar = picture(edit.avatar, "avatars", "Avatar")?;
    let banner = picture(edit.banner, "backgrounds", "ProfileBackground")?;
    if !remove.is_empty() {
        body["remove"] = json!(remove);
    }
    Ok(Prepared { body, avatar, banner })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_patch_preserves_unmodified_fields_and_bio_markup() {
        let edit = ProfileEdit {
            display_name: Some("  Name  ".into()),
            bio: Some("**Hello**\n  indented".into()),
            ..Default::default()
        };
        let prepared = prepare(edit, &HashMap::new()).unwrap();
        assert_eq!(
            prepared.body,
            json!({ "display_name": "Name", "profile": { "content": "**Hello**\n  indented" } })
        );
        assert!(prepared.avatar.is_none() && prepared.banner.is_none());
    }

    #[test]
    fn clearing_fields_uses_the_protocol_removal_names() {
        let prepared = prepare(
            ProfileEdit {
                display_name: Some("".into()),
                pronouns: Some(" ".into()),
                bio: Some("\n".into()),
                avatar: ProfileImage::Remove,
                banner: ProfileImage::Remove,
            },
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(
            prepared.body,
            json!({"remove": ["DisplayName", "Pronouns", "ProfileContent", "Avatar", "ProfileBackground"]})
        );
    }

    #[test]
    fn validation_counts_characters_and_respects_upload_limits() {
        let name = "🦆".repeat(32);
        assert!(
            prepare(
                ProfileEdit {
                    display_name: Some(name.into()),
                    ..Default::default()
                },
                &HashMap::new()
            )
            .is_ok()
        );
        assert!(
            prepare(
                ProfileEdit {
                    display_name: Some("x".repeat(33).into()),
                    ..Default::default()
                },
                &HashMap::new()
            )
            .is_err()
        );
        assert!(
            prepare(
                ProfileEdit {
                    pronouns: Some("x".repeat(25).into()),
                    ..Default::default()
                },
                &HashMap::new()
            )
            .is_err()
        );
        let edit = ProfileEdit {
            avatar: ProfileImage::File {
                path: "chosen.png".into(),
                size: 11,
            },
            ..Default::default()
        };
        assert!(prepare(edit.clone(), &HashMap::from([("avatars".into(), 10)])).is_err());
        let prepared = prepare(edit, &HashMap::from([("avatars".into(), 11)])).unwrap();
        assert!(matches!(
            prepared.avatar,
            Some(Upload {
                size: 11,
                source: UploadSource::Path(_),
                ..
            })
        ));
    }
}
