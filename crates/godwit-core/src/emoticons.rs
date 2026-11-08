// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Optional, network-neutral emoticon conversion for the message composer.

/// Converts a just-completed emoticon before whitespace, preserving code spans
/// and URLs. Replacing only a complete trailing token keeps each edit local.
pub fn convert_completed(text: &str) -> String {
    let Some((idx, ch)) = text.char_indices().next_back() else {
        return text.to_owned();
    };
    if !ch.is_whitespace() {
        return text.to_owned();
    }
    let before = &text[..idx];
    let start = before.rfind(char::is_whitespace).map_or(0, |i| i + 1);
    if start == before.len() {
        return text.to_owned();
    }
    let token = &before[start..];
    if token.contains("http://") || token.contains("https://") || token.contains('`') {
        return text.to_owned();
    }
    let emoji = match token {
        ":)" | ":-)" => "🙂",
        "<3" => "❤️",
        ":D" | ":-D" => "😃",
        ";)" | ";-)" => "😉",
        ":(" | ":-(" => "🙁",
        ":P" | ":-P" => "😛",
        ":o" | ":-o" | ":O" | ":-O" => "😮",
        "</3" => "💔",
        ":'(" => "😢",
        _ => return text.to_owned(),
    };
    format!("{}{emoji}{ch}", &before[..start])
}

#[cfg(test)]
mod tests {
    use super::convert_completed;

    #[test]
    fn converts_supported_tokens_only_after_whitespace() {
        for (input, expected) in [
            ("<3 ", "❤️ "),
            (":)\n", "🙂\n"),
            (":D ", "😃 "),
            (";) ", "😉 "),
            (":( ", "🙁 "),
            (":P ", "😛 "),
            (":o ", "😮 "),
            ("</3 ", "💔 "),
            (":'( ", "😢 "),
        ] {
            assert_eq!(convert_completed(input), expected);
        }
        assert_eq!(convert_completed("<3"), "<3");
        assert_eq!(convert_completed("word <3 "), "word ❤️ ");
    }

    #[test]
    fn leaves_code_and_urls_untouched() {
        assert_eq!(convert_completed("`<3 `"), "`<3 `");
        assert_eq!(
            convert_completed("https://example.test/<3 "),
            "https://example.test/<3 "
        );
    }
}
