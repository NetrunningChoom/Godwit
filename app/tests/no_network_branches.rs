// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The app never names a network or calls a network crate outside the
//! startup registration; networks declare what differs (godwit-core's
//! `network`, `room::Capabilities`, `hooks`) and the app reads that.
//!
//! Only the demo and the startup registration (`registry.rs`) may name
//! networks; never add a file to the allowlist.

use std::collections::BTreeMap;
use std::path::Path;

/// File (under `app/src`) → most references allowed outside its tests.
const ALLOWED: &[(&str, usize)] = &[
    // Demo data stands in for real networks.
    ("demo.rs", usize::MAX),
    // The startup registration: each network's account type, sign-in steps
    // and hooks.
    ("registry.rs", usize::MAX),
];

const VARIANTS: &[&str] = &["Stoat", "Discord", "Steam", "Fluxer", "Teams", "Slack", "GoogleChat"];
const CRATES: &[&str] = &["stoat", "discord", "steam", "slack", "teams", "fluxer", "gchat"];

fn count(code: &str) -> usize {
    // Only shipped code: tests may name networks freely.
    let code = code.split("#[cfg(test)]").next().unwrap_or(code);
    code.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .map(|l| {
            VARIANTS
                .iter()
                .map(|v| matches(l, &format!("Network::{v}")))
                .sum::<usize>()
                + CRATES
                    .iter()
                    .map(|c| l.matches(&format!("godwit_{c}::")).count())
                    .sum::<usize>()
        })
        .sum()
}

/// `needle` not followed by more of an identifier (`Network::Steam` but not
/// `Network::SteamLike`).
fn matches(line: &str, needle: &str) -> usize {
    line.match_indices(needle)
        .filter(|(i, _)| {
            line[i + needle.len()..]
                .chars()
                .next()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
        })
        .count()
}

fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, usize>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            walk(&path, root, out);
        } else if path.extension().is_some_and(|x| x == "rs") {
            let code = std::fs::read_to_string(&path).unwrap_or_default();
            let n = count(&code);
            if n > 0 {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, n);
            }
        }
    }
}

#[test]
fn the_app_never_names_a_network() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = BTreeMap::new();
    walk(&root, &root, &mut found);
    let allowed: BTreeMap<_, _> = ALLOWED.iter().copied().collect();
    let mut problems = Vec::new();
    for (file, n) in &found {
        match allowed.get(file.as_str()) {
            None => problems.push(format!("{file}: {n} network reference(s); read a declaration instead")),
            Some(&max) if *n > max => problems.push(format!("{file}: {n} > {max} allowed")),
            Some(&max) if *n < max && max != usize::MAX => {
                problems.push(format!("{file}: down to {n}, lower its count from {max}"))
            }
            _ => {}
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_counter_sees_references() {
    assert_eq!(count("if n == Network::Steam {}\n// Network::Steam\n"), 1);
    assert_eq!(count("godwit_teams::emoji::all()"), 1);
    assert_eq!(count("Network::ALL"), 0);
    assert_eq!(count("x\n#[cfg(test)]\nNetwork::Stoat"), 0);
}
