// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Percent encoding shared by rich links and network request builders.
use std::fmt::Write as _;

fn encode(text: &str, extra_safe: &[u8], form: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) || extra_safe.contains(&b) {
            out.push(char::from(b));
        } else if form && b == b' ' {
            out.push('+');
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

pub fn percent_encode(text: &str) -> String {
    encode(text, b"", false)
}

/// Additional literal bytes are a caller's protocol policy (e.g. `:` in an emoji).
pub fn percent_encode_with(text: &str, extra_safe: &[u8]) -> String {
    encode(text, extra_safe, false)
}

/// JavaScript's `encodeURIComponent`, including its extra punctuation.
pub fn encode_uri_component(text: &str) -> String {
    encode(text, b"!*'()", false)
}

/// The existing request format: spaces as `+`, literal plus encoded, and
/// the unreserved set (including `~`) left unchanged.
pub fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k, b"", true), encode(v, b"", true)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn path_components_preserve_utf8_and_protocol_punctuation() {
        for (text, path, component) in [
            ("", "", ""),
            ("a b+c/é", "a%20b%2Bc%2F%C3%A9", "a%20b%2Bc%2F%C3%A9"),
            ("!'()*~:-_.", "%21%27%28%29%2A~%3A-_.", "!'()*~%3A-_."),
            ("?#%&=", "%3F%23%25%26%3D", "%3F%23%25%26%3D"),
        ] {
            assert_eq!(percent_encode(text), path);
            assert_eq!(encode_uri_component(text), component);
        }
        assert_eq!(percent_encode_with("name:👍 +", b":"), "name:%F0%9F%91%8D%20%2B");
    }
    #[test]
    fn forms_keep_pair_order_spaces_plus_and_empty_values() {
        assert_eq!(form(&[]), "");
        assert_eq!(
            form(&[("a b", "+/é"), ("empty", ""), ("a b", "~")]),
            "a+b=%2B%2F%C3%A9&empty=&a+b=~"
        );
    }
    #[test]
    fn every_ascii_byte_keeps_the_previous_safe_sets() {
        for b in 0u8..=127 {
            let text = char::from(b).to_string();
            let escaped = format!("%{b:02X}");
            let safe = b.is_ascii_alphanumeric() || b"-_.~".contains(&b);
            for (actual, extra) in [
                (percent_encode(&text), &b""[..]),
                (encode_uri_component(&text), &b"!*'()"[..]),
                (percent_encode_with(&text, b":"), &b":"[..]),
            ] {
                let expected = if safe || extra.contains(&b) {
                    text.as_str()
                } else {
                    escaped.as_str()
                };
                assert_eq!(actual, expected);
            }
            let expected = if safe {
                text.as_str()
            } else if b == b' ' {
                "+"
            } else {
                escaped.as_str()
            };
            assert_eq!(form(&[("k", &text)]), format!("k={expected}"));
        }
    }
}
