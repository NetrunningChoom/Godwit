// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A channel's Overview as a `DataEditChannel` (OpenAPI), following the
//! official settings' channel/Overview.tsx (stoatchat/for-web,
//! packages/client/components/app/interface/settings/channel).
use crate::backend::{Upload, UploadSource};
use godwit_core::channel_settings::ChannelOverviewEdit;
use godwit_core::own_profile::ProfileImage;
use serde_json::{Value, json};
use std::collections::HashMap;

pub struct Prepared {
    pub body: Value,
    pub icon: Option<Upload>,
}

pub fn prepare(edit: ChannelOverviewEdit, limits: &HashMap<String, u64>) -> Result<Prepared, String> {
    let mut body = json!({});
    let mut remove = Vec::new();
    if let Some(name) = edit.name {
        let name = name.trim();
        // Overview.tsx: minlength 1, maxlength 32.
        if name.is_empty() || name.chars().count() > 32 {
            return Err("Channel names must contain 1–32 characters.".into());
        }
        body["name"] = name.into();
    }
    if let Some(description) = edit.description {
        let description = description.trim();
        // Overview.tsx: maxlength 1024; an empty one removes it.
        if description.chars().count() > 1024 {
            return Err("Channel descriptions can contain at most 1024 characters.".into());
        }
        if description.is_empty() {
            remove.push("Description");
        } else {
            body["description"] = description.into();
        }
    }
    let icon = match edit.icon {
        ProfileImage::Keep => None,
        ProfileImage::Remove => {
            remove.push("Icon");
            None
        }
        ProfileImage::File { path, size } => {
            if size == 0 || limits.get("icons").is_some_and(|limit| size > *limit) {
                return Err("The selected image is empty or exceeds this instance's limit for icons.".into());
            }
            Some(Upload {
                name: path.file_name().and_then(|s| s.to_str()).unwrap_or("image").into(),
                size,
                source: UploadSource::Path(path),
            })
        }
    };
    if let Some(mature) = edit.mature {
        body["nsfw"] = mature.into();
    }
    if !remove.is_empty() {
        body["remove"] = json!(remove);
    }
    Ok(Prepared { body, icon })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clears_fields_and_toggles_mature() {
        let p = prepare(
            ChannelOverviewEdit {
                description: Some(" ".into()),
                icon: ProfileImage::Remove,
                mature: Some(true),
                ..Default::default()
            },
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(p.body, json!({"nsfw": true, "remove": ["Description", "Icon"]}));
        assert!(p.icon.is_none());
    }

    #[test]
    fn validates_name_and_description_in_characters() {
        let name = |n: &str| ChannelOverviewEdit {
            name: Some(n.into()),
            ..Default::default()
        };
        assert!(prepare(name(" "), &HashMap::new()).is_err());
        assert!(prepare(name(&"x".repeat(33)), &HashMap::new()).is_err());
        assert!(prepare(name(&"🦆".repeat(32)), &HashMap::new()).is_ok());
        let body = prepare(name(" general "), &HashMap::new()).unwrap().body;
        assert_eq!(body, json!({"name": "general"}));
        let long = ChannelOverviewEdit {
            description: Some("y".repeat(1025).into()),
            ..Default::default()
        };
        assert!(prepare(long, &HashMap::new()).is_err());
    }

    #[test]
    fn uploads_respect_the_icons_limit() {
        let file = |size| ChannelOverviewEdit {
            icon: ProfileImage::File {
                path: "/tmp/i.png".into(),
                size,
            },
            ..Default::default()
        };
        let limits = HashMap::from([("icons".to_owned(), 100u64)]);
        assert!(prepare(file(0), &limits).is_err());
        assert!(prepare(file(101), &limits).is_err());
        assert!(prepare(file(100), &limits).unwrap().icon.is_some());
    }
}
