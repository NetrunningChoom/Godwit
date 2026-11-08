// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Account-scoped media addresses. Cookies/tokens stay in the fetcher, never
//! in the URL handed to the UI. Redirects are checked afresh at every hop.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use crate::account::MediaFetch;

const MAX_BYTES: u64 = 100 * 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(1);

/// Credentials for one set of service-owned hosts. No Debug implementation:
/// even diagnostic output must not contain these headers.
#[derive(Clone)]
pub struct Rule {
    hosts: Vec<String>,
    headers: Vec<(&'static str, String)>,
    subdomains: bool,
}

impl Rule {
    pub fn new(hosts: &[&str], headers: Vec<(&'static str, String)>) -> Self {
        Self {
            hosts: hosts.iter().map(|h| (*h).to_owned()).collect(),
            headers,
            subdomains: true,
        }
    }

    pub fn exact(host: &str, headers: Vec<(&'static str, String)>) -> Self {
        Self {
            hosts: vec![host.to_owned()],
            headers,
            subdomains: false,
        }
    }

    fn matches(&self, host: &str) -> bool {
        self.hosts
            .iter()
            .any(|allowed| host == allowed || (self.subdomains && host.ends_with(&format!(".{allowed}"))))
    }
}

/// Shared by an account and its worker-thread fetcher. A fresh prefix per
/// instance prevents two accounts (or a later login in the same slot) from
/// sharing private media/cache entries.
#[derive(Clone)]
pub struct Media {
    prefix: String,
    rules: Arc<RwLock<HashMap<String, Vec<Rule>>>>,
}

impl std::fmt::Debug for Media {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Media").finish_non_exhaustive()
    }
}

impl Media {
    pub fn new(network: &str) -> Self {
        Self {
            prefix: format!(
                "{network}-media://{:x}-{}-{}/",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ),
            rules: Arc::default(),
        }
    }

    pub fn set(&self, scope: &str, rules: Vec<Rule>) {
        self.rules
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(scope.to_owned(), rules);
    }

    pub fn clear(&self) {
        self.rules
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    pub fn address(&self, scope: &str, url: &str) -> Option<String> {
        https(url)?;
        Some(format!(
            "{}{}/{}",
            self.prefix,
            crate::encoding::percent_encode(scope),
            crate::encoding::percent_encode(url)
        ))
    }

    pub fn registration(&self) -> (String, MediaFetch) {
        let media = self.clone();
        (self.prefix.clone(), Arc::new(move |address| media.fetch(address)))
    }

    fn target(&self, address: &str) -> Option<(String, url::Url)> {
        let (scope, encoded) = address.strip_prefix(&self.prefix)?.split_once('/')?;
        let decode = |s: &str| {
            url::form_urlencoded::parse(format!("v={s}").as_bytes())
                .next()
                .map(|(_, v)| v.into_owned())
        };
        Some((decode(scope)?, https(&decode(encoded)?)?))
    }

    fn headers(&self, scope: &str, url: &url::Url) -> Vec<(&'static str, String)> {
        let rules = self.rules.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        rules
            .get(scope)
            .into_iter()
            .flatten()
            .filter(|r| r.matches(url.host_str().unwrap_or_default()))
            .flat_map(|r| r.headers.clone())
            .collect()
    }

    fn fetch(&self, address: &str) -> Option<Vec<u8>> {
        let (scope, mut url) = self.target(address)?;
        // Missing/cleared credentials mean signed out, even for a public
        // redirect. An old image-loader closure must not keep a login alive.
        if !self
            .rules
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&scope)
        {
            return None;
        }
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            // A configured HTTP(S)_PROXY is kept (ureq's default): direct
            // connections are filtered in `PublicResolver`, see there.
            .max_redirects(0)
            .http_status_as_error(false)
            .build();
        let agent = crate::public_address::PublicResolver::agent(config, |_| false);
        for _ in 0..10 {
            let mut request = agent.get(url.as_str());
            for (name, value) in self.headers(&scope, &url) {
                request = request.header(name, value);
                if let Some(headers) = request.headers_mut()
                    && let Some(header) = headers.get_mut(name)
                {
                    header.set_sensitive(true);
                }
            }
            // Errors can contain signed URLs: deliberately return no detail.
            let mut response = request.call().ok()?;
            if response.status().is_redirection() {
                let location = response.headers().get("location")?.to_str().ok()?;
                url = https(url.join(location).ok()?.as_str())?;
                continue;
            }
            if !response.status().is_success() {
                return None;
            }
            if response
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .is_some_and(|n| n > MAX_BYTES)
            {
                return None;
            }
            let mut bytes = Vec::new();
            response
                .body_mut()
                .as_reader()
                .take(MAX_BYTES + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            return (bytes.len() as u64 <= MAX_BYTES).then_some(bytes);
        }
        None
    }
}

pub fn https(raw: &str) -> Option<url::Url> {
    let normalized;
    let raw = if raw.starts_with("//") {
        normalized = format!("https:{raw}");
        &normalized
    } else {
        raw
    };
    let url = url::Url::parse(raw).ok()?;
    (url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443))
    .then_some(url)
}

/// Images have in-app previews. Other private media stays a file card:
/// WebKit's separate player cannot carry an account's credentials.
pub fn kind(mime: &str) -> crate::message::AttachmentKind {
    if mime.starts_with("image/") {
        crate::message::AttachmentKind::Image
    } else {
        crate::message::AttachmentKind::File
    }
}

/// A deliberate client memory bound for uploads; network limits may be lower.
pub const MAX_UPLOAD: u64 = 25 * 1024 * 1024;

/// Read an explicitly selected file off the runtime/UI threads, with a bound
/// on actual bytes (the file may have changed after it was selected).
pub async fn read_upload(file: &crate::application::Outgoing) -> Result<Vec<u8>, String> {
    let path = file.path.clone();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(path).map_err(|_| "Could not read the attachment.")?;
        let metadata = file.metadata().map_err(|_| "Could not read the attachment.")?;
        if !metadata.is_file() || metadata.len() > MAX_UPLOAD {
            return Err("Attachments must be regular files up to 25 MiB.".to_owned());
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_UPLOAD + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Could not read the attachment.")?;
        if bytes.len() as u64 > MAX_UPLOAD {
            return Err("The attachment grew beyond 25 MiB.".to_owned());
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| "Reading the attachment stopped.".to_owned())?
}

pub fn mime(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_addresses_are_scoped_and_round_trip_signed_urls() {
        let a = Media::new("chat");
        let b = Media::new("chat");
        let url = "https://chat.example/file?q=a%2Bb&x=1#part";
        let address = a.address("team/one", url).unwrap();
        let (scope, target) = a.target(&address).unwrap();
        assert_eq!(scope, "team/one");
        assert_eq!(target.as_str(), url);
        assert!(b.target(&address).is_none());
        for url in [
            "http://chat.example/x",
            "https://user@chat.example/x",
            "https://chat.example:444/x",
            "file:///tmp/file",
        ] {
            assert!(a.address("", url).is_none());
        }
    }

    #[test]
    fn redirects_recompute_credentials_and_never_authorize_other_hosts() {
        let m = Media::new("chat");
        m.set(
            "T1",
            vec![Rule::new(&["chat.example"], vec![("Cookie", "test-cookie".into())])],
        );
        assert_eq!(m.headers("T1", &https("https://chat.example/file").unwrap()).len(), 1);
        for host in ["evilchat.example", "chat.example.evil", "cdn.example"] {
            assert!(
                m.headers("T1", &https(&format!("https://{host}/file")).unwrap())
                    .is_empty()
            );
        }
        assert!(m.headers("T2", &https("https://chat.example/file").unwrap()).is_empty());
        m.clear();
        assert!(m.headers("T1", &https("https://chat.example/file").unwrap()).is_empty());
    }
}
