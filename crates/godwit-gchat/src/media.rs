// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Google Chat media URLs, following mautrix-googlechat's
//! `portal._preprocess_annotations` and `maugclib.Client.download_attachment`.
//! Cookies go only to Google domains, including after redirecting back from
//! googleusercontent.com; they never go to an external attachment host.

use godwit_core::media::{Media, Rule, https, kind};
use godwit_core::message::{Attachment, AttachmentKind};

use crate::{proto, session::Saved};

pub fn configure(media: &Media, saved: &Saved) {
    // Only the hosts Chat's attachments live on: the sign-in opens the whole
    // Google account, so not every *.google.com host (sites.google.com and
    // other user-hosted pages a link preview can point at).
    media.set(
        "",
        vec![Rule::new(
            &[
                "chat.google.com",
                "drive.google.com",
                "drive.usercontent.google.com",
                "docs.google.com",
            ],
            vec![("Cookie", crate::auth::cookie_header(saved))],
        )],
    );
    // Uploaded attachments only: their address is Chat's own
    // get_attachment_url, and Google redirects a download through several
    // of its hosts. maugclib's download_attachment authenticates every
    // *.google.com hop ("usually 4 redirects for files and 1 for images");
    // with the short list above, full images, videos and downloads lost the
    // sign-in partway while thumbnails (one hop) loaded. Link previews and
    // card images keep the short list.
    media.set(
        ATTACHMENTS,
        vec![Rule::new(
            &["google.com"],
            vec![("Cookie", crate::auth::cookie_header(saved))],
        )],
    );
}

/// The media scope for Chat's own uploaded attachments (see `configure`).
const ATTACHMENTS: &str = "attachments";

/// A person's or space's picture, as mautrix-googlechat fetches it
/// (`puppet._reupload_gc_photo`): Chat's address with `https` put first
/// when it's protocol-relative, otherwise unchanged. Rewriting its size
/// broke older addresses (`…/photo.jpg`), leaving initials.
pub fn avatar(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    let raw = if raw.starts_with("//") {
        format!("https:{raw}")
    } else {
        raw.to_owned()
    };
    https(&raw).map(String::from)
}

pub fn uploaded(upload: &proto::UploadMetadata, preview: bool) -> Option<String> {
    if upload.attachment_token().is_empty() {
        return None;
    }
    let mut url = https("https://chat.google.com/api/get_attachment_url")?;
    let image = preview && upload.content_type().starts_with("image/");
    url.query_pairs_mut()
        .append_pair("url_type", if image { "FIFE_URL" } else { "DOWNLOAD_URL" })
        .append_pair("attachment_token", upload.attachment_token());
    if image {
        // Bound the timeline preview; the original stays available separately.
        url.query_pairs_mut()
            .append_pair("sz", "w1600-h1600")
            .append_pair("content_type", upload.content_type());
    }
    Some(url.into())
}

pub fn attachments(message: &proto::Message, media: &Media) -> Vec<Attachment> {
    let mut out = Vec::new();
    for annotation in &message.annotations {
        let attachment = if annotation.has_upload_metadata() {
            let upload = annotation.upload_metadata();
            uploaded(upload, true).and_then(|url| {
                Some(Attachment {
                    name: if upload.content_name().is_empty() {
                        "file"
                    } else {
                        upload.content_name()
                    }
                    .into(),
                    url: media.address(ATTACHMENTS, &url)?.into(),
                    original_url: uploaded(upload, false)
                        .and_then(|u| media.address(ATTACHMENTS, &u))
                        .map(Into::into),
                    kind: kind(upload.content_type()),
                    size: 0,
                    dimensions: None,
                })
            })
        } else if annotation.has_url_metadata() {
            let meta = annotation.url_metadata();
            if meta.should_not_render() {
                continue;
            }
            let image = !meta.image_url().is_empty();
            // A normal web link isn't a downloadable file. Keep it in the
            // text; only file MIME types and preview images become media.
            let mime = meta.mime_type();
            if !image && (mime.is_empty() || mime.starts_with("text/html")) {
                continue;
            }
            let url = if image { meta.image_url() } else { meta.url.url() };
            media.address("", url).map(|url| Attachment {
                name: if meta.title().is_empty() {
                    "attachment"
                } else {
                    meta.title()
                }
                .into(),
                url: url.into(),
                original_url: None,
                kind: if image { AttachmentKind::Image } else { kind(mime) },
                size: 0,
                dimensions: u32::try_from(meta.int_image_width())
                    .ok()
                    .filter(|n| *n > 0)
                    .zip(u32::try_from(meta.int_image_height()).ok().filter(|n| *n > 0)),
            })
        } else if annotation.has_drive_metadata() {
            let drive = annotation.drive_metadata();
            if drive.should_not_render() || drive.id().is_empty() {
                continue;
            }
            let native = drive.mimetype().starts_with("application/vnd.google-apps.") || drive.is_download_restricted();
            let mut url = match https(if native {
                "https://drive.google.com/open"
            } else {
                "https://drive.google.com/uc"
            }) {
                Some(url) => url,
                None => continue,
            };
            url.query_pairs_mut().append_pair("id", drive.id());
            if !native {
                url.query_pairs_mut().append_pair("export", "download");
            }
            let raw = url.to_string();
            let url = if native { Some(raw) } else { media.address("", &raw) };
            url.map(|url| Attachment {
                name: if drive.title().is_empty() {
                    "Drive file"
                } else {
                    drive.title()
                }
                .into(),
                url: url.into(),
                original_url: None,
                kind: if native {
                    AttachmentKind::File
                } else {
                    kind(drive.mimetype())
                },
                size: 0,
                dimensions: None,
            })
        } else {
            None
        };
        if let Some(attachment) = attachment
            && !out.iter().any(|old: &Attachment| old.url == attachment.url)
        {
            out.push(attachment);
        }
    }
    for attachment in &message.attachments {
        if attachment.has_add_on_data() {
            for card in &attachment.add_on_data().cards {
                push_image(&mut out, media, card.header.image_url(), card.header.image_alt_text());
                for section in &card.sections {
                    for widget in &section.widgets {
                        if widget.has_image() {
                            push_image(
                                &mut out,
                                media,
                                widget.image().fife_image_url(),
                                widget.image().alt_text(),
                            );
                        }
                        if widget.has_grid() {
                            for item in &widget.grid().items {
                                push_image(&mut out, media, item.image.image_url(), item.image.alt_text());
                            }
                        }
                    }
                }
            }
        }
        if attachment.has_html() {
            for url in html_images(attachment.html().html.html()) {
                push_image(&mut out, media, &url, "image");
            }
        }
    }
    out
}

fn push_image(out: &mut Vec<Attachment>, media: &Media, raw: &str, name: &str) {
    let Some(url) = media.address("", raw) else { return };
    if out.iter().any(|a| a.url.as_ref() == url) {
        return;
    }
    out.push(Attachment {
        name: if name.is_empty() { "image" } else { name }.into(),
        url: url.into(),
        kind: AttachmentKind::Image,
        original_url: None,
        size: 0,
        dimensions: None,
    });
}

/// Only quoted src attributes on img elements; malformed/unknown HTML is
/// ignored. This deliberately doesn't load scripts, iframes or CSS URLs.
fn html_images(html: &str) -> Vec<String> {
    let mut images = Vec::new();
    for element in html.split('<').skip(1) {
        let Some((tag, _)) = element.split_once('>') else {
            continue;
        };
        let end = tag.find(char::is_whitespace).unwrap_or(tag.len());
        if !tag[..end].eq_ignore_ascii_case("img") {
            continue;
        }
        let mut rest = tag[end..].trim_start();
        while !rest.is_empty() {
            let end = rest.find(|c: char| c == '=' || c.is_whitespace()).unwrap_or(rest.len());
            if end == 0 {
                break;
            }
            let key = &rest[..end];
            rest = rest[end..].trim_start();
            let Some(after) = rest.strip_prefix('=') else { continue };
            let after = after.trim_start();
            let Some(quote @ ('\'' | '"')) = after.chars().next() else {
                break;
            };
            let tail = &after[1..];
            let Some(end) = tail.find(quote) else { break };
            if key.eq_ignore_ascii_case("src") {
                let url = tail[..end]
                    .replace("&amp;", "&")
                    .replace("&quot;", "\"")
                    .replace("&#39;", "'");
                if https(&url).is_some() {
                    images.push(url);
                }
            }
            rest = tail[end + 1..].trim_start();
        }
    }
    images
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_card_images_are_data_not_executable_markup() {
        let mut html = proto::Html::new();
        html.set_html(r#"<img src="https://example.org/a?x=1&amp;y=2"><img src="javascript:alert(1)"><script src="https://example.org/evil"></script>"#.into());
        let mut card = proto::HtmlAttachment::new();
        card.html = Some(html).into();
        let mut attachment = proto::Attachment::new();
        attachment.set_html(card);
        let mut message = proto::Message::new();
        message.attachments.push(attachment);
        let files = attachments(&message, &Media::new("gchat"));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].kind, AttachmentKind::Image);
        assert!(files[0].url.contains("x%3D1%26y%3D2"));
    }

    #[test]
    fn drive_documents_open_in_drive_and_regular_files_download() {
        let mut document = proto::DriveMetadata::new();
        document.set_id("document id".into());
        document.set_title("Document".into());
        document.set_mimetype("application/vnd.google-apps.document".into());
        let mut pdf = document.clone();
        pdf.set_id("pdf id".into());
        pdf.set_mimetype("application/pdf".into());
        let mut message = proto::Message::new();
        for metadata in [document, pdf] {
            let mut annotation = proto::Annotation::new();
            annotation.set_drive_metadata(metadata);
            message.annotations.push(annotation);
        }
        let files = attachments(&message, &Media::new("gchat"));
        assert_eq!(files.len(), 2);
        assert!(files[0].url.starts_with("https://drive.google.com/open?"));
        assert!(files[1].url.starts_with("gchat-media://"));
        assert!(files[1].url.contains("export%3Ddownload"));
    }

    #[test]
    fn avatars_get_https_and_are_otherwise_unchanged() {
        assert_eq!(
            avatar("//lh3.googleusercontent.com/a/photo=s32-c?x=1").as_deref(),
            Some("https://lh3.googleusercontent.com/a/photo=s32-c?x=1")
        );
        assert!(avatar("javascript:alert(1)").is_none());
        assert!(avatar("").is_none());
        assert_eq!(
            avatar("https://example.org/a").as_deref(),
            Some("https://example.org/a")
        );
    }

    #[test]
    fn uploads_preserve_tokens_without_query_injection() {
        let mut upload = proto::UploadMetadata::new();
        upload.set_attachment_token("a+b&url_type=bad".into());
        upload.set_content_name("picture.png".into());
        upload.set_content_type("image/png".into());
        let url = https(&uploaded(&upload, true).unwrap()).unwrap();
        assert_eq!(
            url.query_pairs().find(|(k, _)| k == "attachment_token").unwrap().1,
            "a+b&url_type=bad"
        );
        assert_eq!(url.query_pairs().find(|(k, _)| k == "url_type").unwrap().1, "FIFE_URL");
        let mut annotation = proto::Annotation::new();
        annotation.set_upload_metadata(upload);
        let mut message = proto::Message::new();
        message.annotations.push(annotation);
        let files = attachments(&message, &Media::new("gchat"));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].kind, AttachmentKind::Image);
        assert!(files[0].original_url.is_some());
        assert!(files[0].url.starts_with("gchat-media://"));
    }
}
