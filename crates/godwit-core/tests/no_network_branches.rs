// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Core never branches on a network: what differs is declared in
//! `network.rs` (methods on `Network`) and `room.rs` (`Capabilities::of`),
//! and everything else reads those declarations.

use std::path::Path;

const DECLARATIONS: &[&str] = &["network.rs", "room.rs"];
const VARIANTS: &[&str] = &["Stoat", "Discord", "Steam", "Fluxer", "Teams", "Slack", "GoogleChat"];

fn walk(dir: &Path, root: &Path, problems: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if path.is_dir() {
            walk(&path, root, problems);
        } else if path.extension().is_some_and(|x| x == "rs")
            && !DECLARATIONS.contains(&rel.as_str())
            && !rel.ends_with("tests.rs")
        {
            let code = std::fs::read_to_string(&path).unwrap_or_default();
            let code = code.split("#[cfg(test)]").next().unwrap_or_default();
            for (i, line) in code.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for v in VARIANTS {
                    let needle = format!("Network::{v}");
                    let named = line.match_indices(&needle).any(|(at, _)| {
                        line[at + needle.len()..]
                            .chars()
                            .next()
                            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
                    });
                    if named {
                        problems.push(format!("{rel}:{}: {}", i + 1, line.trim()));
                    }
                }
            }
        }
    }
}

#[test]
fn core_never_branches_on_a_network() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut problems = Vec::new();
    walk(&root, &root, &mut problems);
    assert!(
        problems.is_empty(),
        "declare it on `Network` or in `Capabilities` instead:\n{}",
        problems.join("\n")
    );
}
