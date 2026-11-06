// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Fluxer's sign-in CAPTCHA (docs.fluxer.com, CAPTCHA): an ALTCHA v2 proof of
//! work the client solves without the user. A gated request without a token
//! answers 400 `CAPTCHA_REQUIRED` with `altcha_challenge` (`parameters`,
//! `signature`); the request goes again with `X-Captcha-Token`, the base64
//! of `{"challenge": …, "solution": {"counter", "derivedKey"}}`.
//!
//! The solver follows altcha-lib's reference (`src/v2/pow.ts`,
//! `algorithms/pbkdf2.ts`): the PBKDF2 password is the nonce's bytes then
//! the counter as a big-endian u32, the salt the salt's bytes, `cost`
//! iterations, `keyLength` bytes out; the first counter whose key starts
//! with `keyPrefix` wins, sent as lowercase hex. Unconfirmed against a live
//! challenge: the counter mode (u32, the library's default) and the base64
//! flavour (standard, padded).

use base64::Engine;
use serde_json::{Value, json};

/// The most counters tried before giving up (the library stops at u32's end;
/// a real challenge takes a few hundred at most).
const MOST: u32 = 10_000_000;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Solves `challenge` (the error's `altcha_challenge`) into the token for
/// `X-Captcha-Token`; None if it isn't a challenge this solves.
pub fn solve(challenge: &Value) -> Option<String> {
    let p = &challenge["parameters"];
    if p["algorithm"].as_str()? != "PBKDF2/SHA-256" {
        return None;
    }
    let nonce = unhex(p["nonce"].as_str()?)?;
    let salt = unhex(p["salt"].as_str()?)?;
    let cost = u32::try_from(p["cost"].as_u64()?).ok().filter(|c| *c > 0)?;
    let length = usize::try_from(p["keyLength"].as_u64().unwrap_or(32))
        .ok()
        .filter(|l| (1..=64).contains(l))?;
    let prefix = p["keyPrefix"].as_str().unwrap_or("00").to_lowercase();
    let mut password = nonce.clone();
    password.extend_from_slice(&[0; 4]);
    let mut key = vec![0u8; length];
    for counter in 0..MOST {
        password.truncate(nonce.len());
        password.extend_from_slice(&counter.to_be_bytes());
        pbkdf2::pbkdf2_hmac::<sha2::Sha256>(&password, &salt, cost, &mut key);
        let derived = hex(&key);
        if derived.starts_with(&prefix) {
            let token = json!({
                // Copied unchanged: the signature covers them.
                "challenge": {"parameters": p, "signature": challenge["signature"]},
                "solution": {"counter": counter, "derivedKey": derived},
            });
            return Some(base64::engine::general_purpose::STANDARD.encode(token.to_string()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solves_a_small_challenge_as_the_reference_does() {
        let challenge = json!({
            "parameters": {"algorithm": "PBKDF2/SHA-256", "nonce": "00112233445566778899aabbccddeeff",
                "salt": "ffeeddccbbaa99887766554433221100", "cost": 10, "keyLength": 32, "keyPrefix": "0"},
            "signature": "sig"
        });
        let token = solve(&challenge).expect("solved");
        let decoded: Value =
            serde_json::from_slice(&base64::engine::general_purpose::STANDARD.decode(token).unwrap()).unwrap();
        assert_eq!(decoded["challenge"]["signature"], "sig");
        let counter = u32::try_from(decoded["solution"]["counter"].as_u64().unwrap()).unwrap();
        let derived = decoded["solution"]["derivedKey"].as_str().unwrap();
        assert!(derived.starts_with('0') && derived.len() == 64);
        // Checking it again as the server does gives the same key.
        let mut password = unhex("00112233445566778899aabbccddeeff").unwrap();
        password.extend_from_slice(&counter.to_be_bytes());
        let mut key = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<sha2::Sha256>(
            &password,
            &unhex("ffeeddccbbaa99887766554433221100").unwrap(),
            10,
            &mut key,
        );
        assert_eq!(hex(&key), derived);
    }

    #[test]
    fn refuses_what_it_cannot_solve() {
        assert!(solve(&json!({"parameters": {"algorithm": "ARGON2ID"}})).is_none());
        assert!(
            solve(&json!({"parameters": {"algorithm": "PBKDF2/SHA-256", "nonce": "zz", "salt": "00", "cost": 1}}))
                .is_none()
        );
    }
}
