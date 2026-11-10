// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Account security: change the password and set up or remove the
//! authenticator (docs.discord.food/resources/user: "Modify Current User",
//! "Enable User MFA", "Disable User MFA"). Both rotate the account's token;
//! the answer carries the new one. Nothing here prints a password, token,
//! secret or code.

use rand::RngCore;
use serde_json::{Value, json};

/// The JSON error code Discord answers `mfa/totp/enable` with when only the
/// password was sent and it was right: setup goes on with a secret and code.
pub(crate) const TOTP_SETUP_CONTINUES: u64 = 60005;

/// Discord's limits for a new password (docs.discord.food, "Modify Current User").
pub(crate) const PASSWORD_LEN: std::ops::RangeInclusive<usize> = 8..=72;

/// A new TOTP secret: 20 random bytes as 32 base32 characters, the length
/// the enable request takes.
pub(crate) fn totp_secret() -> String {
    let mut bytes = [0u8; 20];
    rand::rng().fill_bytes(&mut bytes);
    base32(&bytes)
}

/// RFC 4648 base32 without padding (what authenticator apps read).
pub(crate) fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let index = usize::try_from((buffer >> bits) & 31).unwrap_or(0);
            out.push(char::from(ALPHABET.get(index).copied().unwrap_or(b'A')));
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        let index = usize::try_from((buffer << (5 - bits)) & 31).unwrap_or(0);
        out.push(char::from(ALPHABET.get(index).copied().unwrap_or(b'A')));
    }
    out
}

/// The address an authenticator app scans for `secret`.
pub(crate) fn otpauth_uri(secret: &str) -> String {
    format!("otpauth://totp/Discord?secret={secret}&issuer=Discord")
}

pub(crate) fn password_change_body(current: &str, new: &str) -> Value {
    json!({"password": current, "new_password": new})
}

/// Only the password: Discord checks it and answers [`TOTP_SETUP_CONTINUES`].
pub(crate) fn totp_check_body(password: &str) -> Value {
    json!({"password": password})
}

pub(crate) fn totp_enable_body(password: &str, secret: &str, code: &str) -> Value {
    json!({"password": password, "secret": secret, "code": code})
}

/// The new token an answer carries.
pub(crate) fn rotated_token(answer: &Value) -> Option<String> {
    answer["token"].as_str().filter(|t| !t.is_empty()).map(str::to_owned)
}

/// The backup codes still unused (`consumed` false).
pub(crate) fn backup_codes(answer: &Value) -> Vec<String> {
    answer["backup_codes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["consumed"] != true)
        .filter_map(|c| c["code"].as_str().map(str::to_owned))
        .collect()
}

/// An address with most of its name hidden: `a•••@example.com`.
pub(crate) fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((name, domain)) if !name.is_empty() => {
            let first: String = name.chars().take(1).collect();
            format!("{first}•••@{domain}")
        }
        _ => String::new(),
    }
}

/// What to tell the person about a failure. Discord's own words are left
/// out so nothing typed can come back in them.
pub(crate) fn failure(error: &anyhow::Error) -> String {
    match error.downcast_ref::<crate::api::DiscordError>() {
        Some(e) if e.code == Some(crate::mfa::INVALID_MFA) => {
            "That code didn't match. Check it and try again.".to_owned()
        }
        Some(_) => "Discord refused that. Check the details and try again.".to_owned(),
        // Our own sentences (a wrong password, a confirmation not given).
        None => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_matches_the_rfc_vectors() {
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32(b"fooba"), "MZXW6YTB");
        assert_eq!(base32(b"f"), "MY");
        assert_eq!(base32(b""), "");
    }

    #[test]
    fn a_secret_is_32_base32_characters_and_differs_each_time() {
        let (a, b) = (totp_secret(), totp_secret());
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_uppercase() || (b'2'..=b'7').contains(&c)));
        assert_ne!(a, b);
    }

    #[test]
    fn request_bodies_follow_the_docs() {
        assert_eq!(
            password_change_body("old", "new"),
            json!({"password": "old", "new_password": "new"})
        );
        assert_eq!(totp_check_body("pw"), json!({"password": "pw"}));
        assert_eq!(
            totp_enable_body("pw", "SECRET", "123456"),
            json!({"password": "pw", "secret": "SECRET", "code": "123456"})
        );
    }

    #[test]
    fn answers_give_the_new_token_and_unused_codes() {
        let answer = json!({
            "token": "new-token",
            "backup_codes": [
                {"user_id": "1", "code": "aaaa1111", "consumed": false},
                {"user_id": "1", "code": "bbbb2222", "consumed": true},
                {"user_id": "1", "code": "cccc3333", "consumed": false}
            ]
        });
        assert_eq!(rotated_token(&answer).as_deref(), Some("new-token"));
        assert_eq!(backup_codes(&answer), ["aaaa1111", "cccc3333"]);
        assert_eq!(rotated_token(&json!({"token": ""})), None);
        assert!(backup_codes(&json!({})).is_empty());
    }

    #[test]
    fn emails_are_masked() {
        assert_eq!(mask_email("someone@example.com"), "s•••@example.com");
        assert_eq!(mask_email("nonsense"), "");
    }

    #[test]
    fn failures_never_echo_discords_words() {
        let refused = anyhow::Error::new(crate::api::DiscordError {
            status: reqwest::StatusCode::BAD_REQUEST,
            code: Some(50035),
            message: "contains hunter2".into(),
        });
        assert!(!failure(&refused).contains("hunter2"));
        let wrong = anyhow::Error::new(crate::api::DiscordError {
            status: reqwest::StatusCode::BAD_REQUEST,
            code: Some(crate::mfa::INVALID_MFA),
            message: String::new(),
        });
        assert!(failure(&wrong).contains("code"));
    }
}
