// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Teams ASM media, following purple-teams' teams_contacts.c. SharePoint
//! links stay browser links: the Skype token doesn't authorize OneDrive.

use crate::api::Tokens;
use godwit_core::media::{Media, Rule, https, kind};
use godwit_core::message::Attachment;
use serde_json::Value;

pub fn configure(media: &Media, tokens: &Tokens) {
    media.set(
        "",
        vec![
            Rule::new(
                &["asm.skype.com", "asyncgw.teams.microsoft.com"],
                vec![("Cookie", format!("skypetoken_asm={}", tokens.skype))],
            ),
            Rule::exact(
                "teams.microsoft.com",
                vec![
                    (
                        "Cookie",
                        format!(
                            "authtoken=Bearer%3D{}%26Origin%3Dhttps%3A%2F%2Fteams.microsoft.com",
                            tokens.access
                        ),
                    ),
                    ("Referer", "https://teams.microsoft.com/".into()),
                ],
            ),
        ],
    );
}

pub fn is_asm(raw: &str) -> bool {
    https(raw)
        .and_then(|u| u.host_str().map(str::to_owned))
        .is_some_and(|h| {
            h == "api.asm.skype.com" || h.ends_with(".asm.skype.com") || h.ends_with(".asyncgw.teams.microsoft.com")
        })
}

pub fn attachments(content: &str, files: &[Value], media: Option<&Media>) -> Vec<Attachment> {
    let mut out = crate::html::attachments(content);
    for file in files {
        let Some(raw) = file["objectUrl"]
            .as_str()
            .or_else(|| file["fileUrl"].as_str())
            .filter(|u| https(u).is_some())
        else {
            continue;
        };
        let name = file["fileName"]
            .as_str()
            .or_else(|| file["title"].as_str())
            .unwrap_or("file");
        let url = if is_asm(raw) && !raw.contains("/views/") {
            format!("{}/views/original", raw.trim_end_matches('/'))
        } else {
            raw.to_owned()
        };
        if out.iter().any(|a| a.url.as_ref() == url) {
            continue;
        }
        out.push(Attachment {
            name: name.into(),
            url: url.into(),
            original_url: None,
            spoiler: false,
            voice: None,
            kind: kind(
                file["contentType"]
                    .as_str()
                    .or_else(|| file["mimeType"].as_str())
                    .unwrap_or_default(),
            ),
            size: file["size"].as_u64().unwrap_or(0),
            dimensions: None,
        });
    }
    out.into_iter()
        .filter_map(|mut a| {
            // Images and ASM files are fetched inside Godwit; shared documents
            // still open in their own web app, which obtains its own credentials.
            if is_asm(&a.url) || a.kind == godwit_core::message::AttachmentKind::Image {
                a.url = media?.address("", &a.url)?.into();
                a.original_url = a.original_url.and_then(|u| media?.address("", &u)).map(Into::into);
            }
            Some(a)
        })
        .collect()
}

pub fn file_message(object: &str, name: &str, size: u64, text: &str) -> String {
    let escape = crate::html::escape_text;
    format!(
        "<URIObject type=\"File.1\" uri=\"{}\"><Title>{}</Title><Description>{}</Description><OriginalName v=\"{}\"/><FileSize v=\"{}\"/></URIObject>",
        escape(object),
        escape(name),
        escape(text),
        escape(name),
        size
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outgoing_file_metadata_escapes_names_and_round_trips() {
        let html = file_message(
            "https://api.asm.skype.com/v1/objects/x",
            "a\"b&c.png",
            42,
            "caption <text>",
        );
        assert!(html.contains("caption &lt;text&gt;"));
        let files = crate::html::attachments(&html);
        assert_eq!(files[0].name.as_ref(), "a\"b&c.png");
        assert_eq!(files[0].size, 42);
        assert_eq!(files[0].kind, godwit_core::message::AttachmentKind::Image);
    }

    #[test]
    fn only_asm_hosts_are_treated_as_authenticated_file_objects() {
        assert!(is_asm("https://eu-api.asm.skype.com/v1/objects/x"));
        assert!(is_asm("https://emea.asyncgw.teams.microsoft.com/x"));
        assert!(!is_asm("https://api.asm.skype.com.evil/x"));
        assert!(!is_asm("https://tenant.sharepoint.com/file"));
    }
}
