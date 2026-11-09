// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The member's own identity in one server (nickname and avatar), as
//! for-web's ServerIdentity modal edits it: `PATCH /servers/{server}/members/{member}`
//! (`DataMemberEdit`). Unchanged fields are never sent.

use std::collections::HashMap;

use godwit_core::own_profile::{ProfileEdit, ProfileImage};
use serde_json::{Value, json};

use crate::backend::{Upload, UploadSource};

pub struct Prepared {
    pub body: Value,
    pub avatar: Option<Upload>,
}

/// `can_nickname` / `can_avatar` are our ChangeNickname / ChangeAvatar server
/// permissions; the API would refuse the field otherwise.
pub fn prepare(
    edit: ProfileEdit,
    can_nickname: bool,
    can_avatar: bool,
    limits: &HashMap<String, u64>,
) -> Result<Prepared, String> {
    let mut body = json!({});
    let mut remove = Vec::new();
    if let Some(nick) = edit.display_name {
        if !can_nickname {
            return Err("You can't change your nickname in this server.".into());
        }
        let nick = nick.trim();
        if nick.chars().count() > 32 {
            return Err("Nicknames can be at most 32 characters.".into());
        }
        if nick.is_empty() {
            remove.push("Nickname");
        } else {
            body["nickname"] = nick.into();
        }
    }
    let mut avatar = None;
    match edit.avatar {
        ProfileImage::Keep => {}
        _ if !can_avatar => return Err("You can't change your avatar in this server.".into()),
        ProfileImage::Remove => remove.push("Avatar"),
        ProfileImage::File { path, size } => {
            if limits.get("avatars").is_some_and(|limit| size > *limit) {
                return Err("The selected image exceeds this server's limit for avatars.".into());
            }
            avatar = Some(Upload {
                name: path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("image")
                    .to_owned(),
                size,
                source: UploadSource::Path(path),
            });
        }
    }
    if !remove.is_empty() {
        body["remove"] = json!(remove);
    }
    Ok(Prepared { body, avatar })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn sets_and_removes_only_what_changed() {
        let set = ProfileEdit {
            display_name: Some("  Nick  ".into()),
            ..Default::default()
        };
        assert_eq!(
            prepare(set, true, true, &HashMap::new()).unwrap().body,
            json!({"nickname": "Nick"})
        );
        let clear = ProfileEdit {
            display_name: Some(" ".into()),
            avatar: ProfileImage::Remove,
            ..Default::default()
        };
        let p = prepare(clear, true, true, &HashMap::new()).unwrap();
        assert_eq!(p.body, json!({"remove": ["Nickname", "Avatar"]}));
        assert!(p.avatar.is_none());
        assert_eq!(
            prepare(ProfileEdit::default(), true, true, &HashMap::new())
                .unwrap()
                .body,
            json!({})
        );
    }

    #[test]
    fn new_avatar_is_uploaded_not_inlined() {
        let edit = ProfileEdit {
            avatar: ProfileImage::File {
                path: PathBuf::from("/x/me.png"),
                size: 10,
            },
            ..Default::default()
        };
        let p = prepare(edit.clone(), true, true, &HashMap::new()).unwrap();
        assert_eq!(p.body, json!({}));
        assert_eq!(p.avatar.map(|u| u.name), Some("me.png".to_owned()));
        let limits = HashMap::from([("avatars".to_owned(), 5)]);
        assert!(prepare(edit, true, true, &limits).is_err());
    }

    #[test]
    fn permissions_gate_each_field() {
        let nick = ProfileEdit {
            display_name: Some("a".into()),
            ..Default::default()
        };
        assert!(prepare(nick.clone(), false, true, &HashMap::new()).is_err());
        assert!(prepare(nick, true, false, &HashMap::new()).is_ok());
        let avatar = ProfileEdit {
            avatar: ProfileImage::Remove,
            ..Default::default()
        };
        assert!(prepare(avatar.clone(), true, false, &HashMap::new()).is_err());
        assert!(prepare(avatar, false, true, &HashMap::new()).is_ok());
        let long = ProfileEdit {
            display_name: Some("x".repeat(33).into()),
            ..Default::default()
        };
        assert!(prepare(long, true, true, &HashMap::new()).is_err());
    }
}
