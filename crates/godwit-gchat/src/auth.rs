// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Request signing as Google's web clients do it: the cookies of the
//! sign-in, and an `Authorization: SAPISIDHASH {ts}_{sha1}` header where the
//! hash is SHA-1 over `"{ts} {SAPISID} {origin}"`. The same scheme the
//! community clients for Google's web APIs use (hangups, mautrix-googlechat's
//! maugclib); the header proves the caller holds `SAPISID` for that origin.

use crate::session::Saved;

/// The web client's origin; the hash is bound to it.
pub const ORIGIN: &str = "https://chat.google.com";

/// The `Authorization` header value for a request at `unix_secs`.
pub fn sapisid_hash(sapisid: &str, origin: &str, unix_secs: u64) -> String {
    let input = format!("{unix_secs} {sapisid} {origin}");
    let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, input.as_bytes());
    let hex: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    format!("SAPISIDHASH {unix_secs}_{hex}")
}

/// The `Cookie` header for the sign-in's cookies, in their saved order.
pub fn cookie_header(saved: &Saved) -> String {
    saved
        .cookies
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Both headers for a request now, or `None` when the sign-in has no
/// `SAPISID` (signed out, or not a full sign-in).
pub fn headers(saved: &Saved) -> Option<[(&'static str, String); 3]> {
    let sapisid = saved.cookie("SAPISID")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    Some([
        ("authorization", sapisid_hash(sapisid, ORIGIN, now)),
        ("cookie", cookie_header(saved)),
        ("origin", ORIGIN.to_string()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_matches_sha1_of_time_secret_and_origin() {
        // SHA-1("1700000000 fake-sapisid https://chat.google.com"), computed
        // independently with `printf … | sha1sum`.
        assert_eq!(
            sapisid_hash("fake-sapisid", ORIGIN, 1_700_000_000),
            "SAPISIDHASH 1700000000_3268c43deb6e832a15f6c80dcd48ca734845bbac"
        );
    }

    #[test]
    fn headers_need_sapisid_and_carry_every_cookie() {
        let saved = Saved::from_line(r#"{"cookies":[["SID","a"],["SAPISID","b"]]}"#).unwrap();
        let headers = headers(&saved).unwrap();
        assert!(headers[0].1.starts_with("SAPISIDHASH "));
        assert_eq!(headers[1].1, "SID=a; SAPISID=b");
        let no_secret = Saved::from_line(r#"{"cookies":[["SID","a"]]}"#);
        assert!(no_secret.is_none());
    }
}
