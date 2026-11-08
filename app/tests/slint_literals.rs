// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Keeps raw values out of `crates/godwit-ui/ui/*.slint`: colours and sizes come from
//! `Theme` (theme.slint), so a change of scale happens in one place.
//!
//! Flagged, outside comments and strings: hex colours outside theme.slint;
//! literal `font-size` values outside theme.slint; literal px values for
//! `spacing`, `padding*` and `border-radius` outside theme.slint and
//! widgets.slint. `0px` and `1px` are fine, and so is a line carrying
//! `// literal: <reason>`. See docs/slint.md.
//!
//! `ALLOWED` holds what was left after the token migration. It only goes
//! down: lower a file's number when you remove one, never raise it.

use std::collections::BTreeMap;
use std::path::Path;

/// File (under `crates/godwit-ui/ui`) → most raw values allowed.
const ALLOWED: &[(&str, usize)] = &[];

const MARKER: &str = "// literal:";

/// The line with comments and string contents blanked, carrying block
/// comment state across lines.
fn code_only(line: &str, in_block: &mut bool) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if *in_block {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                *in_block = false;
            }
            continue;
        }
        if in_string {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => {
                    in_string = false;
                    out.push('"');
                }
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('/', Some('/')) => break,
            ('/', Some('*')) => {
                chars.next();
                *in_block = true;
            }
            ('"', _) => {
                in_string = true;
                out.push('"');
            }
            _ => out.push(c),
        }
    }
    out
}

fn has_hex(code: &str) -> bool {
    let bytes = code.as_bytes();
    bytes.iter().enumerate().any(|(i, &b)| {
        b == b'#' && {
            let n = bytes[i + 1..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
            let after = bytes.get(i + 1 + n);
            matches!(n, 3 | 4 | 6 | 8) && !after.is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        }
    })
}

/// Whether `value` holds a number with a unit other than 0px or 1px.
fn has_length(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let starts = bytes[i].is_ascii_digit()
            && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'-' || bytes[i - 1] == b'_'));
        if !starts {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
            i += 1;
        }
        let number = &value[start..i];
        let unit_end = bytes[i..].iter().take_while(|c| c.is_ascii_alphabetic()).count();
        let unit = &value[i..i + unit_end];
        if matches!(unit, "px" | "phx" | "pt" | "rem") && !(unit == "px" && matches!(number, "0" | "1")) {
            return true;
        }
        i += unit_end;
    }
    false
}

/// Raw values on one (comment-free) line of `file`.
fn violations(file: &str, code: &str) -> usize {
    if file == "theme.slint" {
        return 0;
    }
    let mut n = usize::from(has_hex(code));
    for part in code.split(';') {
        let Some((name, value)) = part.split_once(':') else {
            continue;
        };
        let name = name.trim().rsplit(['{', ' ']).next().unwrap_or("");
        let sizes = name == "spacing" || name.starts_with("padding") || name == "border-radius";
        if (name == "font-size" || (sizes && file != "widgets.slint")) && has_length(value) {
            n += 1;
        }
    }
    n
}

fn count(file: &str, source: &str) -> usize {
    let mut in_block = false;
    source
        .lines()
        .map(|line| {
            let code = code_only(line, &mut in_block);
            if line.contains(MARKER) {
                0
            } else {
                violations(file, &code)
            }
        })
        .sum()
}

#[test]
fn slint_files_use_theme_tokens() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/godwit-ui/ui");
    let allowed: BTreeMap<&str, usize> = ALLOWED.iter().copied().collect();
    let mut over = Vec::new();
    let mut lower = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("crates/godwit-ui/ui") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|e| e != "slint") {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_owned();
        let found = count(&name, &std::fs::read_to_string(&path).expect("read"));
        let limit = allowed.get(name.as_str()).copied().unwrap_or(0);
        if found > limit {
            over.push(format!("{name}: {found} raw values (allowed {limit})"));
        } else if found < limit {
            lower.push(format!("{name}: {found} (allowlist says {limit})"));
        }
    }
    if !lower.is_empty() {
        eprintln!("Lower these in ALLOWED: {}", lower.join(", "));
    }
    assert!(
        over.is_empty(),
        "Use Theme tokens, or mark a deliberate one-off with `{MARKER} <reason>`:\n{}",
        over.join("\n")
    );
}

#[test]
fn scanner_rules() {
    let c = |file, s| count(file, s);
    assert_eq!(c("a.slint", "background: #00000080;"), 1);
    assert_eq!(c("theme.slint", "background: #00000080;"), 0);
    assert_eq!(c("a.slint", "text: \"name#1234 #fff\";"), 0);
    assert_eq!(c("a.slint", "// was #fff"), 0);
    assert_eq!(c("a.slint", "font-size: 15px;"), 1);
    assert_eq!(c("a.slint", "font-size: Theme.text-body;"), 0);
    assert_eq!(c("a.slint", "spacing: 6px; padding-left: 0px; padding: 1px;"), 1);
    assert_eq!(c("a.slint", "border-radius: self.height / 2;"), 0);
    assert_eq!(c("widgets.slint", "border-radius: 20px;"), 0);
    assert_eq!(c("widgets.slint", "font-size: 20px;"), 1);
    assert_eq!(c("a.slint", "width: 20px; height: 13px;"), 0);
    assert_eq!(c("a.slint", "spacing: 6px; // literal: matches the official client"), 0);
    assert_eq!(c("a.slint", "Text { text: \"x\"; font-size: 30px; }"), 1);
    assert_eq!(c("a.slint", "/* spacing: 6px;\n font-size: 9px; */ x: 1px;"), 0);
}
