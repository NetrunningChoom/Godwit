// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Secrets bigger than one keychain entry holds, split over several entries.
//!
//! Windows' Credential Manager keeps at most 2560 bytes per credential
//! (`CRED_MAX_CREDENTIAL_BLOB_SIZE`), and keyring's `set_password` stores the
//! text as UTF-16, so a session's JSON had to stay under 1280 characters. A
//! Teams refresh token or Google Chat's cookie set is longer: saving failed
//! and the account was signed out at every start. Values go in as UTF-8
//! bytes, and one that is still too long is split into `name~1`, `name~2`, …
//! with `name` itself holding only [`MARK`] and the number of parts. JSON never
//! starts with a NUL, so a whole value can't be taken for a list of parts.

use std::io;

/// What one Windows credential holds.
#[cfg(target_os = "windows")]
pub(crate) const LIMIT: usize = 2560;

const MARK: &[u8] = b"\0godwit-parts:";

/// One entry per name, as the keychain keeps them.
pub(crate) trait Raw {
    fn get(&self, name: &str) -> io::Result<Option<Vec<u8>>>;
    fn set(&self, name: &str, value: &[u8]) -> io::Result<()>;
    /// Succeeds when there was nothing to delete.
    fn delete(&self, name: &str) -> io::Result<()>;
}

fn part(name: &str, index: usize) -> String {
    format!("{name}~{index}")
}

/// The number of parts, if `head` is a list of parts rather than the value.
fn parts(head: &[u8]) -> Option<usize> {
    std::str::from_utf8(head.strip_prefix(MARK)?).ok()?.parse().ok()
}

pub(crate) fn get(raw: &dyn Raw, name: &str) -> io::Result<Option<Vec<u8>>> {
    let Some(head) = raw.get(name)? else {
        return Ok(None);
    };
    let Some(count) = parts(&head) else {
        return Ok(Some(head));
    };
    let mut value = Vec::new();
    for index in 1..=count {
        // Names only: the error never carries any of the secret.
        let Some(chunk) = raw.get(&part(name, index))? else {
            return Err(io::Error::other(format!("{name}: part {index} of {count} is missing")));
        };
        value.extend_from_slice(&chunk);
    }
    Ok(Some(value))
}

pub(crate) fn set(raw: &dyn Raw, name: &str, value: &[u8], limit: usize) -> io::Result<()> {
    let before = raw.get(name).ok().flatten().as_deref().and_then(parts).unwrap_or(0);
    let count = if value.len() <= limit && !value.starts_with(MARK) {
        raw.set(name, value)?;
        0
    } else {
        let chunks: Vec<&[u8]> = value.chunks(limit.max(1)).collect();
        for (index, chunk) in chunks.iter().enumerate() {
            raw.set(&part(name, index + 1), chunk)?;
        }
        let mut head = MARK.to_vec();
        head.extend_from_slice(chunks.len().to_string().as_bytes());
        raw.set(name, &head)?;
        chunks.len()
    };
    // A shorter value leaves no stale parts behind.
    for index in count + 1..=before {
        raw.delete(&part(name, index))?;
    }
    Ok(())
}

pub(crate) fn delete(raw: &dyn Raw, name: &str) -> io::Result<()> {
    let count = raw.get(name).ok().flatten().as_deref().and_then(parts).unwrap_or(0);
    for index in 1..=count {
        raw.delete(&part(name, index))?;
    }
    raw.delete(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// Entries in memory, refusing any over `limit` bytes as the keychain does.
    struct Map(RefCell<BTreeMap<String, Vec<u8>>>, usize);

    impl Default for Map {
        fn default() -> Self {
            Self(RefCell::default(), 16)
        }
    }

    impl Raw for Map {
        fn get(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
            Ok(self.0.borrow().get(name).cloned())
        }
        fn set(&self, name: &str, value: &[u8]) -> io::Result<()> {
            assert!(value.len() <= self.1, "an entry over the limit: {name}");
            self.0.borrow_mut().insert(name.to_owned(), value.to_vec());
            Ok(())
        }
        fn delete(&self, name: &str) -> io::Result<()> {
            self.0.borrow_mut().remove(name);
            Ok(())
        }
    }

    impl Map {
        fn names(&self) -> Vec<String> {
            self.0.borrow().keys().cloned().collect()
        }
    }

    #[test]
    fn a_short_value_is_one_entry() {
        let raw = Map::default();
        set(&raw, "s", b"short", 16).unwrap();
        assert_eq!(raw.names(), ["s"]);
        assert_eq!(get(&raw, "s").unwrap().as_deref(), Some(&b"short"[..]));
    }

    #[test]
    fn a_long_value_is_split_and_joined() {
        let raw = Map::default();
        let value = b"0123456789abcdefghijklmnopqrstuvwxyz0123";
        set(&raw, "s", value, 16).unwrap();
        assert_eq!(raw.names(), ["s", "s~1", "s~2", "s~3"]);
        assert_eq!(get(&raw, "s").unwrap().as_deref(), Some(&value[..]));
    }

    #[test]
    fn shrinking_removes_the_old_parts() {
        let raw = Map::default();
        set(&raw, "s", b"0123456789abcdefghijklmnopqrstuvwxyz0123", 16).unwrap();
        set(&raw, "s", b"0123456789abcdefghij", 16).unwrap();
        assert_eq!(raw.names(), ["s", "s~1", "s~2"]);
        set(&raw, "s", b"tiny", 16).unwrap();
        assert_eq!(raw.names(), ["s"]);
        assert_eq!(get(&raw, "s").unwrap().as_deref(), Some(&b"tiny"[..]));
    }

    #[test]
    fn delete_removes_every_part() {
        let raw = Map::default();
        set(&raw, "s", b"0123456789abcdefghijklmnopqrstuvwxyz0123", 16).unwrap();
        delete(&raw, "s").unwrap();
        assert!(raw.names().is_empty());
        assert_eq!(get(&raw, "s").unwrap(), None);
    }

    #[test]
    fn a_value_that_looks_like_the_mark_survives() {
        // Short, but starting with the mark: it still goes in as parts, or
        // reading it back would look for parts that aren't there.
        let raw = Map(RefCell::default(), 64);
        let value = [MARK, b"3"].concat();
        set(&raw, "s", &value, 64).unwrap();
        assert_eq!(raw.names(), ["s", "s~1"]);
        assert_eq!(get(&raw, "s").unwrap(), Some(value));
    }

    #[test]
    fn a_missing_part_is_an_error_without_the_secret() {
        let raw = Map::default();
        set(&raw, "s", b"secretsecretsecretsecretsecretsecretsecr", 16).unwrap();
        raw.delete("s~2").unwrap();
        let error = get(&raw, "s").unwrap_err().to_string();
        assert_eq!(error, "s: part 2 of 3 is missing");
    }
}
