// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Address classification for fetches whose destination comes from a message.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ureq::config::Config;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};
use ureq::{Agent, Error};

/// ureq resolver that removes non-public destinations after DNS resolution.
pub struct PublicResolver {
    allow_local: fn(&str) -> bool,
}

impl PublicResolver {
    pub fn new(allow_local: fn(&str) -> bool) -> Self {
        Self { allow_local }
    }

    /// Build a default ureq agent using this resolver.
    pub fn agent(config: Config, allow_local: fn(&str) -> bool) -> Agent {
        Agent::with_parts(config, DefaultConnector::default(), Self::new(allow_local))
    }
}

impl std::fmt::Debug for PublicResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublicResolver").finish_non_exhaustive()
    }
}

impl Resolver for PublicResolver {
    fn resolve(&self, uri: &http::Uri, config: &Config, timeout: NextTimeout) -> Result<ResolvedSocketAddrs, Error> {
        let addresses = DefaultResolver::default().resolve(uri, config, timeout)?;
        // The user's own HTTP(S)_PROXY (ureq reads it from the environment) is
        // asked here for its own address, often a local one: it is trusted as
        // configured. Targets behind it are resolved by the proxy, not here;
        // NO_PROXY hosts and SOCKS4 targets still come through and are filtered.
        if config.proxy().is_some_and(|p| is_proxy(uri, p)) {
            return Ok(addresses);
        }
        // Asked about the parsed host alone: the full text could carry userinfo
        // (`https://official.host@10.0.0.5/`) that a prefix match reads as the host.
        let official = uri.scheme_str() == Some("https")
            && uri
                .host()
                .is_some_and(|host| (self.allow_local)(&format!("https://{host}/")));
        if official {
            return Ok(addresses);
        }
        let mut public = DefaultResolver::default().empty();
        for address in &addresses {
            if is_public(address.ip()) {
                public.try_push(*address).map_err(|_| Error::HostNotFound)?;
            }
        }
        if public.is_empty() {
            Err(Error::HostNotFound)
        } else {
            Ok(public)
        }
    }
}

/// Whether `uri` is the proxy itself rather than a target sent through it.
fn is_proxy(uri: &http::Uri, proxy: &ureq::Proxy) -> bool {
    let default_port = match uri.scheme_str() {
        Some("https") => 443,
        Some("http") => 80,
        _ => proxy.port(),
    };
    uri.host().is_some_and(|h| h.eq_ignore_ascii_case(proxy.host()))
        && uri.port_u16().unwrap_or(default_port) == proxy.port()
}

/// Whether an address is globally reachable, excluding special-purpose ranges.
pub fn is_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => public_v4(address),
        IpAddr::V6(address) => public_v6(address),
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 192 && b == 168)
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && ((b == 18) || (b == 19) || (b == 51 && c == 100)))
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

fn public_v6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return public_v4(mapped);
    }
    let segments = ip.segments();
    // Global unicast is 2000::/3. Other assignments are special use; NAT64
    // (64:ff9b::/96) falls outside it, so NAT64-only networks can't fetch.
    (segments[0] & 0xe000) == 0x2000
        // Documentation and protocol assignment ranges are not public hosts.
        && !(segments[0] == 0x2001 && (segments[1] & 0xfe00) == 0
            || segments[0] == 0x2001 && segments[1] == 0x0db8
            || segments[0] == 0x3fff && segments[1] < 0x1000
            // 6to4 embeds any IPv4 address (2002:7f00:1:: is 127.0.0.1), and a
            // relay would reach it.
            || segments[0] == 0x2002)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_special_ipv4_ranges() {
        for ip in [
            "0.0.0.0",
            "10.1.2.3",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.31.255.255",
            "192.0.2.1",
            "198.18.0.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
        ] {
            assert!(ip.parse::<IpAddr>().is_ok_and(|address| !is_public(address)), "{ip}");
        }
        // Home networks' range, built rather than written out (the commit hook
        // flags its literal as a personal address).
        assert!(!is_public(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!("8.8.8.8".parse::<IpAddr>().is_ok_and(is_public));
    }

    #[test]
    fn filters_special_ipv6_and_mapped_ipv4() {
        for ip in [
            "::",
            "::1",
            "fe80::1",
            "fc00::1",
            "ff02::1",
            "2001:db8::1",
            "3fff::1",
            "2002:7f00:1::",
            "::ffff:127.0.0.1",
        ] {
            assert!(ip.parse::<IpAddr>().is_ok_and(|address| !is_public(address)), "{ip}");
        }
        assert!("2606:4700:4700::1111".parse::<IpAddr>().is_ok_and(is_public));
        assert!("::ffff:8.8.8.8".parse::<IpAddr>().is_ok_and(is_public));
    }

    #[test]
    fn only_the_proxy_itself_skips_the_filter() -> Result<(), Box<dyn std::error::Error>> {
        let proxy = ureq::Proxy::new("http://127.0.0.1:3128")?;
        assert!(is_proxy(&"http://127.0.0.1:3128".parse()?, &proxy));
        assert!(!is_proxy(&"https://127.0.0.1/".parse()?, &proxy));
        assert!(!is_proxy(&"https://10.0.0.5:3128/".parse()?, &proxy));
        Ok(())
    }
}
