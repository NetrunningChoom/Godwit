// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Valve's text KeyValues (VDF), as far as reading a game's name from its
//! PICS app info: `"appinfo" { "common" { "name" "…" } }`.

/// The game's name in an app info buffer (text KeyValues, maybe ending in a
/// NUL), or None.
pub fn app_name(buffer: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(buffer).ok()?.trim_end_matches('\0');
    let mut path: Vec<String> = Vec::new();
    let mut key: Option<String> = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                let mut token = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('n') => token.push('\n'),
                            Some('t') => token.push('\t'),
                            Some(other) => token.push(other),
                            None => break,
                        },
                        c => token.push(c),
                    }
                }
                match key.take() {
                    None => key = Some(token),
                    Some(k) => {
                        let depth = path.len();
                        if k == "name" && depth == 2 && path.get(1).is_some_and(|p| p == "common") {
                            let name = token.trim().to_owned();
                            return (!name.is_empty()).then_some(name);
                        }
                    }
                }
            }
            '{' => path.push(key.take().unwrap_or_default()),
            '}' => {
                path.pop();
                key = None;
            }
            // Comments run to the end of the line.
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_common_name() {
        let info = b"\"appinfo\"\n{\n\t\"appid\"\t\t\"440\"\n\t\"common\"\n\t{\n\t\t\"name\"\t\t\"Team \\\"Fortress\\\" 2\"\n\t\t\"type\"\t\t\"Game\"\n\t}\n\t\"extended\"\n\t{\n\t\t\"name\"\t\t\"no\"\n\t}\n}\n\0";
        assert_eq!(app_name(info).as_deref(), Some("Team \"Fortress\" 2"));
    }

    #[test]
    fn ignores_names_elsewhere() {
        let info = b"\"appinfo\" { \"config\" { \"name\" \"x\" } \"common\" { \"type\" \"Game\" } }";
        assert_eq!(app_name(info), None);
        assert_eq!(app_name(b"\xff\xfe"), None);
    }
}
