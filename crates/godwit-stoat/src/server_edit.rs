// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Official server Overview.tsx and OpenAPI DataEditServer.
use crate::backend::{Upload, UploadSource};
use godwit_core::own_profile::ProfileImage;
use godwit_core::server_settings::ServerOverviewEdit;
use serde_json::{Value, json};
use std::collections::HashMap;

pub const SYSTEM_FIELDS: [&str; 4] = ["user_joined", "user_left", "user_kicked", "user_banned"];
pub struct Prepared {
    pub body: Value,
    pub icon: Option<Upload>,
    pub banner: Option<Upload>,
}
pub fn prepare(edit: ServerOverviewEdit, limits: &HashMap<String, u64>) -> Result<Prepared, String> {
    let mut body = json!({});
    let mut remove = Vec::new();
    if let Some(name) = edit.name {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 32 {
            return Err("Server names must contain 1–32 characters.".into());
        }
        body["name"] = name.into();
    }
    if let Some(description) = edit.description {
        let description = description.trim();
        if description.chars().count() > 1024 {
            return Err("Server descriptions can contain at most 1024 characters.".into());
        }
        if description.is_empty() {
            remove.push("Description")
        } else {
            body["description"] = description.into();
        }
    }
    let mut picture = |image, tag: &str, field| -> Result<Option<Upload>, String> {
        match image {
            ProfileImage::Keep => Ok(None),
            ProfileImage::Remove => {
                remove.push(field);
                Ok(None)
            }
            ProfileImage::File { path, size } => {
                if size == 0 || limits.get(tag).is_some_and(|limit| size > *limit) {
                    return Err(format!(
                        "The selected image is empty or exceeds this instance's limit for {tag}."
                    ));
                }
                Ok(Some(Upload {
                    name: path.file_name().and_then(|s| s.to_str()).unwrap_or("image").into(),
                    size,
                    source: UploadSource::Path(path),
                }))
            }
        }
    };
    let icon = picture(edit.icon, "icons", "Icon")?;
    let banner = picture(edit.banner, "banners", "Banner")?;
    if !remove.is_empty() {
        body["remove"] = json!(remove);
    }
    if let Some(channels) = edit.system_channels {
        let mut config = json!({});
        for (field, channel) in SYSTEM_FIELDS.iter().zip(channels) {
            if let Some(channel) = channel {
                config[*field] = channel.as_ref().into();
            }
        }
        body["system_messages"] = config;
    }
    Ok(Prepared { body, icon, banner })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn patch_clears_fields_without_touching_other_configuration() {
        let request = prepare(
            ServerOverviewEdit {
                description: Some(" ".into()),
                icon: ProfileImage::Remove,
                banner: ProfileImage::Remove,
                ..Default::default()
            },
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(request.body, json!({"remove": ["Description", "Icon", "Banner"]}));
        let request = prepare(
            ServerOverviewEdit {
                system_channels: Some([None, Some("left".into()), None, None]),
                ..Default::default()
            },
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(request.body, json!({"system_messages": {"user_left": "left"}}));
        let request = prepare(
            ServerOverviewEdit {
                system_channels: Some(Default::default()),
                ..Default::default()
            },
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(request.body, json!({"system_messages": {}}));
    }
    #[test]
    fn validates_unicode_limits_and_upload_tags() {
        for (name, valid) in [
            (" ".to_owned(), false),
            ("🦆".repeat(32), true),
            ("x".repeat(33), false),
        ] {
            assert_eq!(
                prepare(
                    ServerOverviewEdit {
                        name: Some(name.into()),
                        ..Default::default()
                    },
                    &HashMap::new()
                )
                .is_ok(),
                valid
            );
        }
        assert!(
            prepare(
                ServerOverviewEdit {
                    description: Some("x".repeat(1025).into()),
                    ..Default::default()
                },
                &HashMap::new()
            )
            .is_err()
        );
        let edit = ServerOverviewEdit {
            banner: ProfileImage::File {
                path: "banner.png".into(),
                size: 11,
            },
            ..Default::default()
        };
        assert!(prepare(edit.clone(), &HashMap::from([("banners".into(), 10)])).is_err());
        assert!(
            prepare(edit, &HashMap::from([("banners".into(), 11)]))
                .unwrap()
                .banner
                .is_some()
        );
    }
}
