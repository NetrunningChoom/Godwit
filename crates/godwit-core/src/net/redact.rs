// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! What of a request may be written down: an endpoint's shape, never who or
//! what it is about. Path parts that look like ids become `{id}` (with a
//! short hash that changes each run, so one chat can be followed through a
//! log without the log naming it), subdomains that may name a workplace or
//! workspace become `*`, and query values are dropped (small numbers such as
//! page sizes stay).

use std::hash::{BuildHasher, RandomState};
use std::sync::OnceLock;

/// Subdomain labels generic enough to keep (they name a service, not a customer).
const PLAIN_LABELS: &[&str] = &[
    "api",
    "www",
    "cdn",
    "media",
    "gateway",
    "chat",
    "app",
    "web",
    "static",
    "images",
    "img",
    "upload",
    "uploads",
    "files",
    "file",
    "edge",
    "eu",
    "us",
    "login",
    "auth",
    "accounts",
    "account",
    "asm",
    "asyncgw",
    "go",
    "presence",
    "graph",
    "status",
    "voice",
    "ws",
    "wss",
    "rtm",
    "events",
    "push",
    "msgapi",
    "teams",
    "clients",
    "people",
    "content",
    "avatars",
    "emoji",
    "stickers",
    "attachments",
    "autumn",
    "january",
    "proxy",
    "realtime",
    "mail",
];

/// A per-run salt: hashes in one log match each other, not other logs.
fn salt() -> &'static RandomState {
    static SALT: OnceLock<RandomState> = OnceLock::new();
    SALT.get_or_init(RandomState::new)
}

/// A short salted hash of `value`.
pub fn short_hash(value: &[u8]) -> String {
    format!("{:04x}", salt().hash_one(value) & 0xffff)
}

/// Whether a path segment is a word (an endpoint's name) rather than data.
fn is_word(segment: &str) -> bool {
    if segment.is_empty() || segment.len() > 40 {
        return false;
    }
    // API versions: `v9`, `v10`.
    if segment.len() <= 3 && segment.starts_with('v') && segment[1..].chars().all(|c| c.is_ascii_digit()) {
        return segment.len() > 1;
    }
    if !segment
        .chars()
        .all(|c| c.is_ascii_alphabetic() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return false;
    }
    // Long mixed-case runs are encoded ids, not names.
    let mixed = segment.chars().any(|c| c.is_ascii_uppercase()) && segment.chars().any(|c| c.is_ascii_lowercase());
    !(mixed && segment.len() > 16)
}

/// The host with customer-specific subdomains masked.
pub fn host(host: &str) -> String {
    let labels: Vec<&str> = host.split('.').collect();
    if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
        return format!("ip{{{}}}", short_hash(host.as_bytes()));
    }
    let keep_from = labels.len().saturating_sub(2);
    labels
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if i >= keep_from || PLAIN_LABELS.contains(&l.to_ascii_lowercase().as_str()) {
                (*l).to_owned()
            } else {
                "*".to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// An endpoint's shape and the short hashes of the ids taken out of it:
/// `("api.example.com/channels/{id}/messages?limit=50&before=…", ["3f2a"])`.
pub fn endpoint(url: &url::Url) -> (String, Vec<String>) {
    let mut ids = Vec::new();
    let mut out = host(url.host_str().unwrap_or("?"));
    for segment in url.path().split('/').skip(1) {
        out.push('/');
        if is_word(segment) {
            out.push_str(segment);
        } else if !segment.is_empty() {
            out.push_str("{id}");
            ids.push(short_hash(segment.as_bytes()));
        }
    }
    let mut first = true;
    for (key, value) in url.query_pairs() {
        out.push(if first { '?' } else { '&' });
        first = false;
        if is_word(&key) {
            out.push_str(&key);
        } else {
            out.push_str("{key}");
        }
        out.push('=');
        let small_number = value.len() <= 4 && value.chars().all(|c| c.is_ascii_digit());
        if small_number || matches!(&*value, "true" | "false" | "json" | "proto" | "protojson") {
            out.push_str(&value);
        } else if !value.is_empty() {
            out.push('…');
        }
    }
    (out, ids)
}

/// A protobuf message's shape without its values: field numbers, nested
/// messages in braces (`1{1,2},4,7×3`). None when `bytes` isn't protobuf.
/// The wire format doesn't say which fields are messages, so a string that
/// happens to parse as one shows as nested fields too.
pub fn proto_shape(bytes: &[u8]) -> Option<String> {
    shape(bytes, 0)
}

fn varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *bytes.get(*at)?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn shape(bytes: &[u8], depth: usize) -> Option<String> {
    let mut at = 0;
    let mut fields: Vec<(u64, String, u32)> = Vec::new();
    while at < bytes.len() {
        let tag = varint(bytes, &mut at)?;
        let number = tag >> 3;
        if number == 0 {
            return None;
        }
        let inner = match tag & 7 {
            0 => {
                varint(bytes, &mut at)?;
                String::new()
            }
            1 => {
                at = at.checked_add(8).filter(|end| *end <= bytes.len())?;
                String::new()
            }
            2 => {
                let len = usize::try_from(varint(bytes, &mut at)?).ok()?;
                let end = at.checked_add(len).filter(|end| *end <= bytes.len())?;
                let body = &bytes[at..end];
                at = end;
                // A nested message if it parses as one; strings and bytes otherwise.
                let nested = if depth < 4 && !body.is_empty() {
                    shape(body, depth + 1)
                } else {
                    None
                };
                nested.map(|n| format!("{{{n}}}")).unwrap_or_default()
            }
            5 => {
                at = at.checked_add(4).filter(|end| *end <= bytes.len())?;
                String::new()
            }
            _ => return None,
        };
        match fields.last_mut() {
            Some((n, s, count)) if *n == number && *s == inner => *count += 1,
            _ => fields.push((number, inner, 1)),
        }
    }
    Some(
        fields
            .iter()
            .map(|(n, s, count)| {
                if *count > 1 {
                    format!("{n}{s}×{count}")
                } else {
                    format!("{n}{s}")
                }
            })
            .collect::<Vec<_>>()
            .join(","),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_query_values_are_taken_out() {
        let url = url::Url::parse("https://discord.com/api/v9/channels/123456789/messages?limit=50&before=987654321")
            .unwrap();
        let (shape, ids) = endpoint(&url);
        assert_eq!(shape, "discord.com/api/v9/channels/{id}/messages?limit=50&before=…");
        assert_eq!(ids.len(), 1);
        assert!(!shape.contains("123456789"));
    }

    #[test]
    fn the_same_id_hashes_the_same_within_a_run() {
        let a = url::Url::parse("https://x.example/rooms/abc123").unwrap();
        let b = url::Url::parse("https://x.example/rooms/abc123/messages").unwrap();
        assert_eq!(endpoint(&a).1, endpoint(&b).1);
    }

    #[test]
    fn customer_subdomains_are_masked() {
        assert_eq!(host("contoso.sharepoint.com"), "*.sharepoint.com");
        assert_eq!(host("api.asm.skype.com"), "api.asm.skype.com");
        assert_eq!(host("my-team.slack.com"), "*.slack.com");
        assert!(host("203.0.113.10").starts_with("ip{"));
    }

    #[test]
    fn emails_and_encoded_ids_are_not_words() {
        assert!(!is_word("someone@example.org"));
        assert!(!is_word("AAAAbCdEfGhIjKlMnOp"));
        assert!(is_word("list_topics"));
        assert!(is_word("messages"));
    }

    #[test]
    fn proto_shapes_show_fields_not_values() {
        // field 1: varint 150; field 2: message { field 1: "x" }; field 3 twice.
        let bytes = [0x08, 0x96, 0x01, 0x12, 0x03, 0x0a, 0x01, b'x', 0x18, 1, 0x18, 2];
        assert_eq!(proto_shape(&bytes).as_deref(), Some("1,2{1},3×2"));
        assert_eq!(proto_shape(b"{\"json\":true}"), None);
    }
}
