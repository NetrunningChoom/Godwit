// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Google Chat media URLs, following mautrix-googlechat's
//! `portal._preprocess_annotations` and `maugclib.Client.download_attachment`.
//! Cookies go only to Google domains, including after redirecting back from
//! googleusercontent.com; they never go to an external attachment host.

use godwit_core::media::{Media, Rule, https};
use godwit_core::message::{Attachment, AttachmentKind, Embed};

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

/// Video and audio need their own kinds so the app's player path is used
/// (the shared `media::kind` only tells images from files).
fn kind(mime: &str) -> AttachmentKind {
    if mime.starts_with("video/") {
        AttachmentKind::Video
    } else if mime.starts_with("audio/") {
        AttachmentKind::Audio
    } else {
        godwit_core::media::kind(mime)
    }
}

/// A URL annotation that is a file (non-HTML MIME) rather than a web page:
/// it becomes an attachment, never a card.
fn is_file_link(meta: &proto::UrlMetadata) -> bool {
    let mime = meta.mime_type();
    !mime.is_empty() && !mime.starts_with("text/html")
}

/// Link previews (`url_metadata`) as cards, so a link is openable from the
/// message and not only from a text span. Chat doesn't send file sizes in
/// any of these messages (see `docs/gchat.md`), so sizes stay unknown (0).
pub fn embeds(message: &proto::Message, media: &Media) -> Vec<Embed> {
    let mut out: Vec<Embed> = Vec::new();
    for annotation in &message.annotations {
        if !annotation.has_url_metadata() {
            continue;
        }
        let meta = annotation.url_metadata();
        if meta.should_not_render() || is_file_link(meta) {
            continue;
        }
        let Some(link) = [meta.url.url(), meta.redirect_url.url(), meta.gws_url.url()]
            .into_iter()
            .find_map(https)
            .map(String::from)
        else {
            continue;
        };
        if out.iter().any(|e| e.url.as_deref() == Some(link.as_str())) {
            continue;
        }
        let text = |s: &str| (!s.is_empty()).then(|| s.into());
        out.push(Embed {
            title: text(meta.title()),
            description: text(meta.snippet()),
            site_name: text(meta.domain()),
            image_url: media.address("", meta.image_url()).map(Into::into),
            media_dimensions: u32::try_from(meta.int_image_width())
                .ok()
                .filter(|n| *n > 0)
                .zip(u32::try_from(meta.int_image_height()).ok().filter(|n| *n > 0)),
            url: Some(link.into()),
            ..Embed::default()
        });
    }
    out
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
            // A normal web link is an embed card (see `embeds`), not a
            // file; only file MIME types become attachments.
            if !is_file_link(meta) {
                continue;
            }
            let mime = meta.mime_type();
            // The real file, never its preview image (core has no thumbnail field).
            let raw = meta.url.url();
            let kind = kind(mime);
            // Players fetch an external video themselves, so it keeps its
            // plain address (cookies never go to an external host anyway).
            let url = if matches!(kind, AttachmentKind::Video | AttachmentKind::Audio) {
                https(raw).map(String::from)
            } else {
                media.address("", raw)
            };
            url.map(|url| Attachment {
                name: if meta.title().is_empty() {
                    "attachment"
                } else {
                    meta.title()
                }
                .into(),
                url: url.into(),
                original_url: None,
                kind,
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

    fn upload_message(name: &str, mime: &str) -> proto::Message {
        let mut upload = proto::UploadMetadata::new();
        upload.set_attachment_token("tok".into());
        upload.set_content_name(name.into());
        upload.set_content_type(mime.into());
        let mut annotation = proto::Annotation::new();
        annotation.set_upload_metadata(upload);
        let mut message = proto::Message::new();
        message.annotations.push(annotation);
        message
    }

    fn link_message(link: &str, mime: &str) -> proto::Message {
        let mut url = proto::Url::new();
        url.set_url(link.into());
        let mut meta = proto::UrlMetadata::new();
        meta.url = Some(url).into();
        meta.set_title("Title".into());
        meta.set_snippet("About".into());
        meta.set_domain("example.org".into());
        if !mime.is_empty() {
            meta.set_mime_type(mime.into());
        }
        let mut annotation = proto::Annotation::new();
        annotation.set_url_metadata(meta);
        annotation.set_start_index(0);
        annotation.set_length(4);
        let mut message = proto::Message::new();
        message.set_text_body("link and more".into());
        message.annotations.push(annotation);
        message
    }

    #[test]
    fn uploaded_video_is_a_video_with_a_download_address() {
        let files = attachments(&upload_message("clip.mp4", "video/mp4"), &Media::new("gchat"));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].kind, AttachmentKind::Video);
        assert!(files[0].url.starts_with("gchat-media://"));
        assert!(files[0].url.contains("DOWNLOAD_URL"));
        assert_eq!(files[0].name.as_ref(), "clip.mp4");
    }

    #[test]
    fn uploaded_image_and_file_kinds() {
        let m = Media::new("gchat");
        assert_eq!(
            attachments(&upload_message("a.png", "image/png"), &m)[0].kind,
            AttachmentKind::Image
        );
        assert_eq!(
            attachments(&upload_message("a.pdf", "application/pdf"), &m)[0].kind,
            AttachmentKind::File
        );
        assert_eq!(
            attachments(&upload_message("a.mp3", "audio/mpeg"), &m)[0].kind,
            AttachmentKind::Audio
        );
    }

    #[test]
    fn drive_video_is_a_video_attachment() {
        let mut drive = proto::DriveMetadata::new();
        drive.set_id("vid".into());
        drive.set_mimetype("video/mp4".into());
        let mut annotation = proto::Annotation::new();
        annotation.set_drive_metadata(drive);
        let mut message = proto::Message::new();
        message.annotations.push(annotation);
        let files = attachments(&message, &Media::new("gchat"));
        assert_eq!(files[0].kind, AttachmentKind::Video);
    }

    #[test]
    fn a_plain_url_is_an_openable_embed_not_a_file() {
        let message = link_message("https://example.org/page", "");
        let media = Media::new("gchat");
        assert!(attachments(&message, &media).is_empty());
        let cards = embeds(&message, &media);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].url.as_deref(), Some("https://example.org/page"));
        assert_eq!(cards[0].title.as_deref(), Some("Title"));
        assert_eq!(cards[0].site_name.as_deref(), Some("example.org"));
    }

    #[test]
    fn text_with_a_url_keeps_the_link_in_text_and_as_a_card() {
        let message = link_message("https://example.org/page", "text/html");
        assert_eq!(
            crate::annotations::text(&message),
            "[link](<https://example.org/page>) and more"
        );
        assert_eq!(embeds(&message, &Media::new("gchat")).len(), 1);
    }

    #[test]
    fn a_remote_video_link_keeps_its_plain_address() {
        let message = link_message("https://example.org/v.mp4", "video/mp4");
        let files = attachments(&message, &Media::new("gchat"));
        assert_eq!(files[0].kind, AttachmentKind::Video);
        assert_eq!(files[0].url.as_ref(), "https://example.org/v.mp4");
    }

    #[test]
    fn file_links_are_attachments_only() {
        let message = link_message("https://example.org/v.mp4", "video/mp4");
        let media = Media::new("gchat");
        assert_eq!(attachments(&message, &media).len(), 1);
        assert!(embeds(&message, &media).is_empty());
    }

    #[test]
    fn video_with_preview_image_keeps_real_url_and_kind() {
        let mut message = link_message("https://example.org/v.mp4", "video/mp4");
        message.annotations[0]
            .mut_url_metadata()
            .set_image_url("https://example.org/p.jpg".into());
        let files = attachments(&message, &Media::new("gchat"));
        assert_eq!(files[0].kind, AttachmentKind::Video);
        assert_eq!(files[0].url.as_ref(), "https://example.org/v.mp4");
    }

    #[test]
    fn unsafe_urls_make_no_card_or_attachment() {
        let media = Media::new("gchat");
        for link in ["javascript:alert(1)", "data:text/html,hi"] {
            for mime in ["", "video/mp4"] {
                let message = link_message(link, mime);
                assert!(embeds(&message, &media).is_empty());
                assert!(attachments(&message, &media).is_empty());
            }
        }
    }
}
