// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! An instance's addresses, from `GET /.well-known/fluxer` on its origin
//! (docs.fluxer.com, Instance discovery): Fluxer asks clients never to
//! build them from the origin or hard-code its domains.

use serde_json::Value;

/// The official instance. Its discovery answers from `api.fluxer.app`
/// (a redirect from here, followed).
pub const OFFICIAL: &str = "https://fluxer.app";

/// What Godwit uses of an instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    /// The HTTP API for third-party clients (`endpoints.api_public`), without `/v1`.
    pub api: String,
    pub gateway: String,
    /// Avatars, icons, emoji (`endpoints.media`).
    pub media: String,
    /// Clickable message links (`endpoints.webapp`).
    pub webapp: String,
}

impl Instance {
    /// Reads a discovery answer; None if an address is missing or not https/wss.
    pub fn from(v: &Value) -> Option<Self> {
        let e = &v["endpoints"];
        let get = |k: &str, scheme: &str| {
            e[k].as_str()
                .filter(|u| u.starts_with(scheme))
                .map(|u| u.trim_end_matches('/').to_owned())
        };
        Some(Self {
            api: get("api_public", "https://")?,
            gateway: get("gateway", "wss://")?,
            media: get("media", "https://")?,
            webapp: get("webapp", "https://").unwrap_or_else(|| OFFICIAL.to_owned()),
        })
    }

    /// A REST path (`/users/@me`) as a full URL.
    pub fn url(&self, path: &str) -> String {
        format!("{}/v1{path}", self.api)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_the_official_answer() {
        // The live answer's addresses (2026-11-05).
        let v = json!({"api_code_version": 1, "endpoints": {
            "api": "https://web.fluxer.app/api", "api_public": "https://fluxer.com/api",
            "gateway": "wss://gateway.fluxer.com", "media": "https://fluxerusercontent.com",
            "webapp": "https://fluxer.com"
        }, "captcha": {"provider": "altcha"}});
        let i = Instance::from(&v).unwrap();
        assert_eq!(i.url("/users/@me"), "https://fluxer.com/api/v1/users/@me");
        assert_eq!(i.gateway, "wss://gateway.fluxer.com");
        assert!(Instance::from(&json!({"endpoints": {"api_public": "http://x"}})).is_none());
    }
}
