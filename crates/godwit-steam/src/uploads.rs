// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Steam Friends & Chat's web file upload flow.

use sha1::{Digest, Sha1};
use serde::Deserialize;

// Response shape of `beginfileupload` as in node-steamcommunity's
// `sendImageToUser` (components/users.js).
#[derive(Debug, Deserialize)]
struct BeginResponse {
    success: i64,
    #[serde(default)]
    hmac: String,
    #[serde(default)]
    timestamp: serde_json::Value,
    result: Option<BeginResult>,
}

#[derive(Debug, Deserialize)]
struct BeginResult {
    ugcid: serde_json::Value,
    url_host: String,
    #[serde(default)]
    url_path: String,
    #[serde(default)]
    use_https: bool,
    #[serde(default)]
    request_headers: Vec<HeaderPair>,
}

#[derive(Debug, Deserialize)]
struct HeaderPair {
    name: String,
    value: String,
}

/// Steam sends ids and timestamps as numbers or strings.
fn text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

struct Meta<'a> {
    name: &'a str,
    sha: &'a str,
    width: u32,
    height: u32,
    mime: &'a str,
}

/// Image dimensions without decoding the pixel data. Steam's chat web client
/// sends these in `beginfileupload`; supported formats match the composer.
pub(crate) fn image_info(_name: &str, bytes: &[u8]) -> Result<(u32, u32, &'static str), String> {
    let format = image::guess_format(bytes)
        .map_err(|_| "Steam chat attachments must be PNG, JPEG, GIF or WebP images.".to_owned())?;
    let mime = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        _ => return Err("Steam chat attachments must be PNG, JPEG, GIF or WebP images.".into()),
    };
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "Couldn't read the image dimensions.".to_owned())?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| "Couldn't read the image dimensions.".to_owned())?;
    if width == 0 || height == 0 {
        return Err("Couldn't read the image dimensions.".into());
    }
    Ok((width, height, mime))
}

fn begin_form(sessionid: &str, size: usize, meta: &Meta) -> Vec<(&'static str, String)> {
    vec![
        ("sessionid", sessionid.into()),
        ("l", "english".into()),
        ("file_size", size.to_string()),
        ("file_name", meta.name.into()),
        ("file_sha", meta.sha.into()),
        ("file_image_width", meta.width.to_string()),
        ("file_image_height", meta.height.to_string()),
        ("file_type", meta.mime.into()),
    ]
}

fn commit_form(
    begin: &BeginResponse,
    result: &BeginResult,
    meta: &Meta,
    sessionid: &str,
    target: Target,
    spoiler: bool,
) -> Vec<(&'static str, String)> {
    let mut form = vec![
        ("sessionid", sessionid.into()),
        ("l", "english".into()),
        ("file_name", meta.name.into()),
        ("file_sha", meta.sha.into()),
        ("success", "1".into()),
        ("ugcid", text(&result.ugcid)),
        ("file_type", meta.mime.into()),
        ("file_image_width", meta.width.to_string()),
        ("file_image_height", meta.height.to_string()),
        ("timestamp", text(&begin.timestamp)),
        ("hmac", begin.hmac.clone()),
        ("spoiler", if spoiler { "1" } else { "0" }.into()),
    ];
    match target {
        Target::Friend(friend) => form.push(("friend_steamid", friend.to_string())),
        // Group fields are not in the reference library; they follow the web client.
        Target::Group { group, chat } => {
            form.push(("chat_group_id", group.to_string()));
            form.push(("chat_id", chat.to_string()));
        }
    }
    form
}

/// Steam's endpoints take multipart bodies (as node-steamcommunity sends them).
fn multipart(fields: Vec<(&'static str, String)>) -> reqwest::multipart::Form {
    fields
        .into_iter()
        .fold(reqwest::multipart::Form::new(), |form, (k, v)| form.text(k, v))
}

#[derive(Clone, Copy)]
pub(crate) enum Target {
    Friend(u64),
    Group { group: u64, chat: u64 },
}

pub(crate) async fn upload_image(
    http: &reqwest::Client,
    access_token: &str,
    sessionid: &str,
    steamid: u64,
    target: Target,
    name: &str,
    bytes: &[u8],
) -> Result<(), String> {
    // Request names follow Steam's Friends & Chat upload calls, as documented
    // by DoctorMcKay's public node-steamcommunity chat implementation:
    // https://github.com/DoctorMcKay/node-steamcommunity/blob/master/components/users.js
    let (width, height, mime) = image_info(name, bytes)?;
    let digest = format!("{:x}", Sha1::digest(bytes));
    let meta = Meta { name, sha: &digest, width, height, mime };
    let cookie = format!("sessionid={sessionid}; steamLoginSecure={steamid}||{access_token}");
    let begin: BeginResponse = http
        .post("https://steamcommunity.com/chat/beginfileupload/?l=english")
        .header(reqwest::header::COOKIE, &cookie)
        .header(reqwest::header::REFERER, "https://steamcommunity.com/chat/")
        .multipart(multipart(begin_form(sessionid, bytes.len(), &meta)))
        .send()
        .await
        .map_err(|_| "Couldn't start the Steam image upload.".to_owned())?
        .error_for_status()
        .map_err(|_| "Steam rejected the image upload request.".to_owned())?
        .json()
        .await
        .map_err(|_| "Steam returned an invalid image upload response.".to_owned())?;
    let result = match (&begin.result, begin.success) {
        (Some(result), 1) => result,
        _ => return Err("Steam rejected the image upload request.".into()),
    };
    let scheme = if result.use_https { "https" } else { "http" };
    let upload_url = format!("{scheme}://{}{}", result.url_host, result.url_path);
    let mut put = http.put(upload_url).body(bytes.to_vec());
    for header in &result.request_headers {
        put = put.header(header.name.to_lowercase(), &header.value);
    }
    put.send()
        .await
        .map_err(|_| "Couldn't transfer the image to Steam.".to_owned())?
        .error_for_status()
        .map_err(|_| "Steam couldn't store the image.".to_owned())?;
    let done: serde_json::Value = http
        .post("https://steamcommunity.com/chat/commitfileupload/")
        .header(reqwest::header::COOKIE, cookie)
        .header(reqwest::header::REFERER, "https://steamcommunity.com/chat/")
        .multipart(multipart(commit_form(&begin, result, &meta, sessionid, target, false)))
        .send()
        .await
        .map_err(|_| "Couldn't finish the Steam image upload.".to_owned())?
        .error_for_status()
        .map_err(|_| "Steam couldn't finish the image upload.".to_owned())?
        .json()
        .await
        .map_err(|_| "Steam returned an invalid image upload response.".to_owned())?;
    // Both the outer and the inner result must be 1 (node-steamcommunity checks both).
    if done["success"].as_i64() != Some(1) || done["result"]["success"].as_i64() != Some(1) {
        return Err("Steam couldn't finish the image upload.".into());
    }
    Ok(())
}

pub(crate) async fn read_image(file: &godwit_core::application::Outgoing) -> Result<Vec<u8>, String> {
    let bytes = godwit_core::media::read_upload(file).await?;
    image_info(&file.name, &bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_targets_friend_and_group_fields() {
        let begin: BeginResponse = serde_json::from_str(
            r#"{"success":1,"hmac":"h","timestamp":1,"result":{"ugcid":"7","url_host":"h","url_path":"/p","use_https":true,"request_headers":[{"name":"X-Test","value":"v"}]}}"#,
        )
        .unwrap();
        let result = begin.result.as_ref().unwrap();
        assert_eq!(result.request_headers[0].name, "X-Test");
        let meta = Meta { name: "a.png", sha: "s", width: 1, height: 2, mime: "image/png" };
        let friend = commit_form(&begin, result, &meta, "sid", Target::Friend(42), false);
        assert!(friend.contains(&("friend_steamid", "42".into())));
        assert!(friend.contains(&("timestamp", "1".into())));
        assert!(friend.contains(&("file_sha", "s".into())));
        assert!(!friend.iter().any(|(key, _)| *key == "chat_id"));
        let group = commit_form(&begin, result, &meta, "sid", Target::Group { group: 7, chat: 8 }, true);
        assert!(group.contains(&("chat_group_id", "7".into())));
        assert!(group.contains(&("chat_id", "8".into())));
        assert!(group.contains(&("spoiler", "1".into())));
        assert!(!group.iter().any(|(key, _)| *key == "friend_steamid"));
    }

    #[test]
    fn rejects_non_images_without_revealing_secrets() {
        let error = image_info("notes.txt", b"password=private").unwrap_err();
        assert_eq!(error, "Steam chat attachments must be PNG, JPEG, GIF or WebP images.");
        assert!(!error.contains("private"));
    }

    #[test]
    fn sniffs_png_dimensions_from_its_header() {
        use image::ImageEncoder;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[0, 0, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .expect("test PNG encodes");
        assert_eq!(image_info("pixel.png", &png), Ok((1, 1, "image/png")));
    }
}
