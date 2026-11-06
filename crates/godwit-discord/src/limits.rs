// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Ported from Yapfast's `limits.rs` (GPL-3.0-or-later).
//!
//! Discord's HTTP rate limits, which its API asks every client to keep. Each
//! route belongs to a bucket, named in `X-RateLimit-Bucket`, that allows
//! `X-RateLimit-Remaining` more requests until `X-RateLimit-Reset-After`
//! seconds pass, and a 429 marked global pauses every route. Routes about
//! different channels, servers or webhooks share a bucket's name but not its
//! count. Godwit waits rather than sending a request Discord would refuse:
//! refusals count against the account, and too many of them get its address
//! blocked for a while.

use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};

use reqwest::{Method, StatusCode, header::HeaderMap};
use serde_json::Value;

/// The longest Godwit waits on a limit before giving up on the request.
pub const MAX_WAIT: Duration = Duration::from_secs(60);

/// A time in seconds that Discord sent, held between `min` and `max`: a
/// negative, endless or huge one would panic, and zero would spin.
pub fn sent_seconds(seconds: f64, min: Duration, max: Duration) -> Duration {
    if seconds.is_nan() {
        return min;
    }
    Duration::from_secs_f64(seconds.clamp(min.as_secs_f64(), max.as_secs_f64()))
}

/// Counts what happened within the last `span`, up to `limit` times.
pub struct Window {
    limit: usize,
    span: Duration,
    times: VecDeque<Instant>,
}

impl Window {
    pub const fn new(limit: usize, span: Duration) -> Self {
        Self {
            limit,
            span,
            times: VecDeque::new(),
        }
    }

    /// How long until it may happen again, if it already happened `limit`
    /// times within the span.
    pub fn full(&mut self, now: Instant) -> Option<Duration> {
        while self
            .times
            .front()
            .is_some_and(|&time| now.duration_since(time) >= self.span)
        {
            self.times.pop_front();
        }
        (self.times.len() >= self.limit).then(|| self.span.saturating_sub(now.duration_since(self.times[0])))
    }

    pub fn push(&mut self, now: Instant) {
        if self.full(now).is_some() {
            self.times.pop_front();
        }
        self.times.push_back(now);
    }
}

/// Discord blocks an address for an hour or more once 10,000 of its
/// requests in 10 minutes were refused as unauthorized, forbidden or too
/// many. Godwit stops after 20 refusals in that window, and a rejected
/// authenticated request stops the account immediately.
pub struct Refusals {
    inner: Mutex<(Window, bool)>,
}

/// How many refusals in [`REFUSAL_SPAN`] stop Godwit's requests until fewer
/// are recent.
const REFUSAL_LIMIT: usize = 20;
const REFUSAL_SPAN: Duration = Duration::from_secs(10 * 60);

impl Default for Refusals {
    fn default() -> Self {
        Self {
            inner: Mutex::new((Window::new(REFUSAL_LIMIT, REFUSAL_SPAN), false)),
        }
    }
}

/// Why Godwit didn't send a request.
#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    /// Discord refused the login.
    Unauthorized,
    /// Too many requests were refused lately; another may go after this.
    Refused(Duration),
}

impl Refusals {
    /// Whether a request may go.
    pub fn check(&self) -> Result<(), Stopped> {
        let mut inner = self.lock();
        if inner.1 {
            return Err(Stopped::Unauthorized);
        }
        match inner.0.full(Instant::now()) {
            Some(wait) => Err(Stopped::Refused(wait)),
            None => Ok(()),
        }
    }

    /// Counts the answer to a request, if Discord refused it.
    pub fn answered(&self, status: StatusCode) {
        if matches!(
            status,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
        ) {
            self.lock().0.push(Instant::now());
        }
    }

    /// Discord refused the login itself; nothing more goes.
    pub fn unauthorized(&self) {
        self.lock().1 = true;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (Window, bool)> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Default)]
pub struct Limits {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// The bucket each route counts against, as Discord last said: its name
    /// and the route's channel, server or webhook.
    buckets_by_route: HashMap<String, String>,
    buckets: HashMap<String, Bucket>,
    /// Every route waits until then.
    global_until: Option<Instant>,
}

struct Bucket {
    remaining: u64,
    reset: Instant,
}

impl Limits {
    /// Waits until a request on `route` may go, and counts it against its
    /// bucket. Errors when that would take longer than [`MAX_WAIT`].
    pub async fn acquire(&self, route: &str) -> Result<(), Duration> {
        loop {
            let wait = self.reserve(route, Instant::now());
            if wait.is_zero() {
                return Ok(());
            }
            if wait > MAX_WAIT {
                return Err(wait);
            }
            tokio::time::sleep(wait).await;
        }
    }

    /// How long a request on `route` must wait; when it needn't, the request
    /// takes one of its bucket's remaining requests.
    fn reserve(&self, route: &str, now: Instant) -> Duration {
        let mut inner = self.lock();
        if let Some(until) = inner.global_until {
            if until > now {
                return until - now;
            }
            inner.global_until = None;
        }
        let Some(key) = inner.buckets_by_route.get(route).cloned() else {
            return Duration::ZERO;
        };
        let Some(bucket) = inner.buckets.get_mut(&key) else {
            return Duration::ZERO;
        };
        if bucket.reset <= now {
            // The bucket refilled; Discord's next answer says by how much.
            inner.buckets.remove(&key);
            return Duration::ZERO;
        }
        if bucket.remaining == 0 {
            return bucket.reset - now;
        }
        bucket.remaining -= 1;
        Duration::ZERO
    }

    /// Learns the limits from an answer to a request on `route` that
    /// Discord didn't refuse with a 429; see [`Self::limited`] for those.
    pub fn update(&self, route: &str, headers: &HeaderMap) {
        self.update_at(route, StatusCode::OK, headers, &Value::Null, Instant::now());
    }

    /// Learns the limits from a 429 on `route`, with its JSON `body`, whose
    /// `retry_after` and `global` say the same as the headers, or say it
    /// when the headers don't.
    pub fn limited(&self, route: &str, headers: &HeaderMap, body: &Value) {
        let status = StatusCode::TOO_MANY_REQUESTS;
        let now = Instant::now();
        self.update_at(route, status, headers, body, now);
        // A local safety margin, not a claim about Discord's limits: any
        // 429 pauses this account for at least a minute, with no replay.
        let mut inner = self.lock();
        let until = now + Duration::from_secs(60);
        inner.global_until = Some(inner.global_until.map_or(until, |old| old.max(until)));
    }

    /// Cool down a refused or failing route instead of repeatedly probing
    /// it (permissions may have changed since the last gateway update).
    pub fn failed(&self, route: &str, status: StatusCode) {
        let wait = if status == StatusCode::FORBIDDEN {
            Duration::from_secs(5 * 60)
        } else if status.is_server_error() {
            Duration::from_secs(60)
        } else {
            return;
        };
        let mut inner = self.lock();
        let key = inner
            .buckets_by_route
            .get(route)
            .cloned()
            .unwrap_or_else(|| route.to_owned());
        let until = Instant::now() + wait;
        let reset = inner.buckets.get(&key).map_or(until, |bucket| bucket.reset.max(until));
        inner.buckets_by_route.insert(route.to_owned(), key.clone());
        inner.buckets.insert(key, Bucket { remaining: 0, reset });
    }

    fn update_at(&self, route: &str, status: StatusCode, headers: &HeaderMap, body: &Value, now: Instant) {
        let text = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
        let seconds = |value: Option<f64>| {
            value
                .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
                .map(|seconds| sent_seconds(seconds, Duration::ZERO, Duration::from_secs(24 * 3600)))
        };
        let header_seconds = |name: &str| seconds(text(name).and_then(|value| value.parse().ok()));
        let reset = header_seconds("x-ratelimit-reset-after").or_else(|| {
            let epoch: f64 = text("x-ratelimit-reset")?.parse().ok()?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs_f64();
            seconds(Some((epoch - now).max(0.0)))
        });
        let remaining = text("x-ratelimit-remaining").and_then(|value| value.parse::<u64>().ok());
        // Leave 20% of a known bucket available to other sessions. This
        // local margin supplements, rather than replaces, server headers.
        let spare = text("x-ratelimit-limit")
            .and_then(|value| value.parse::<u64>().ok())
            .map_or(0, |limit| if limit > 1 { limit.div_ceil(5) } else { 0 });
        let mut inner = self.lock();
        let key = match text("x-ratelimit-bucket") {
            Some(name) => format!("{name}{}", major(route)),
            // A route Discord named no bucket for counts on its own.
            None => inner
                .buckets_by_route
                .get(route)
                .cloned()
                .unwrap_or_else(|| route.to_owned()),
        };
        if status != StatusCode::TOO_MANY_REQUESTS {
            if let (Some(remaining), Some(reset)) = (remaining, reset) {
                inner.buckets.insert(
                    key.clone(),
                    Bucket {
                        remaining: remaining.saturating_sub(spare),
                        reset: now + reset,
                    },
                );
            }
            if text("x-ratelimit-bucket").is_some() {
                inner.buckets_by_route.insert(route.to_owned(), key);
            }
            return;
        }
        // Respect the longest wait across the body and both headers.
        let wait = seconds(body["retry_after"].as_f64())
            .into_iter()
            .chain(header_seconds("retry-after"))
            .chain(reset)
            .max()
            .unwrap_or(Duration::from_secs(1));
        let global = body["global"] == true
            || text("x-ratelimit-global") == Some("true")
            || text("x-ratelimit-scope") == Some("global");
        if global {
            inner.global_until = Some(now + wait);
            return;
        }
        inner.buckets.insert(
            key.clone(),
            Bucket {
                remaining: 0,
                reset: now + wait,
            },
        );
        inner.buckets_by_route.insert(route.to_owned(), key);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The start of a route that Discord counts limits by on their own: its
/// channel, server, or webhook and token. Empty for other routes.
fn major(route: &str) -> &str {
    let path = route.split_once(' ').map_or(route, |(_, path)| path);
    let segments = match path.split('/').find(|segment| !segment.is_empty()) {
        Some("channels" | "guilds") => 2,
        Some("webhooks") => 3,
        _ => return "",
    };
    // The path starts with a slash, so the one after the major segments is
    // that many further on.
    let end = path
        .match_indices('/')
        .nth(segments)
        .map_or(path.len(), |(index, _)| index);
    &path[..end]
}

/// A request's route as Discord counts its limits: the method and path, with
/// the ids in it replaced, except the channel, server or webhook it's about,
/// whose limits are its own. Reactions' emoji are replaced too.
pub fn route(method: &Method, path: &str) -> String {
    let path = path.split_once("/api/v9").map_or(path, |(_, rest)| rest);
    let mut out = format!("{method} ");
    let mut previous = "";
    for (index, segment) in path.split('/').filter(|segment| !segment.is_empty()).enumerate() {
        let major = index == 1 && matches!(previous, "channels" | "guilds" | "webhooks");
        let replaced = if major {
            segment
        } else if previous == "reactions" {
            ":emoji"
        } else if segment.bytes().all(|byte| byte.is_ascii_digit()) {
            ":id"
        } else {
            segment
        };
        out.push('/');
        out.push_str(replaced);
        previous = segment;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_bucket_headroom_and_uses_the_longest_retry_header() {
        let limits = Limits::default();
        let now = Instant::now();
        let route = "GET /channels/1/messages";
        limits.update_at(
            route,
            StatusCode::OK,
            &headers(&[
                ("x-ratelimit-bucket", "test"),
                ("x-ratelimit-limit", "5"),
                ("x-ratelimit-remaining", "4"),
                ("x-ratelimit-reset-after", "10"),
            ]),
            &Value::Null,
            now,
        );
        for _ in 0..3 {
            assert_eq!(limits.reserve(route, now), Duration::ZERO);
        }
        assert_eq!(
            limits.reserve(route, now),
            Duration::from_secs(10),
            "one slot stays spare"
        );
        limits.update_at(
            route,
            StatusCode::TOO_MANY_REQUESTS,
            &headers(&[("retry-after", "120"), ("x-ratelimit-reset-after", "30")]),
            &json!({"retry_after":1}),
            now,
        );
        assert_eq!(limits.reserve(route, now), Duration::from_secs(120));
    }

    #[test]
    fn rate_limits_and_refusals_apply_long_cooldowns_without_retries() {
        let limits = Limits::default();
        let route = "GET /channels/1/messages";
        limits.limited(route, &HeaderMap::new(), &json!({"retry_after":0}));
        assert!(
            limits.reserve("GET /other", Instant::now()) >= Duration::from_secs(59),
            "a 429 pauses the account"
        );
        let limits = Limits::default();
        limits.failed(route, StatusCode::FORBIDDEN);
        assert!(limits.reserve(route, Instant::now()) >= Duration::from_secs(299));
        assert_eq!(limits.reserve("GET /other", Instant::now()), Duration::ZERO);
        let limits = Limits::default();
        limits.failed(route, StatusCode::SERVICE_UNAVAILABLE);
        assert!(limits.reserve(route, Instant::now()) >= Duration::from_secs(59));
    }
    use reqwest::header::HeaderValue;
    use serde_json::json;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn names_routes_by_their_major_id() {
        assert_eq!(
            route(&Method::GET, "/api/v9/channels/12/messages/34"),
            "GET /channels/12/messages/:id"
        );
        assert_eq!(
            route(&Method::PUT, "/channels/12/messages/34/reactions/%F0%9F%91%8D/@me"),
            "PUT /channels/12/messages/:id/reactions/:emoji/@me"
        );
        assert_eq!(route(&Method::GET, "/users/56/profile"), "GET /users/:id/profile");
    }

    #[test]
    fn waits_for_an_empty_bucket_and_a_global_limit() {
        let limits = Limits::default();
        let now = Instant::now();
        let route = "POST /channels/1/messages";
        let ok = StatusCode::OK;
        let limited = StatusCode::TOO_MANY_REQUESTS;
        assert_eq!(limits.reserve(route, now), Duration::ZERO, "unknown routes go");
        limits.update_at(
            route,
            ok,
            &headers(&[
                ("x-ratelimit-bucket", "abc"),
                ("x-ratelimit-remaining", "1"),
                ("x-ratelimit-reset-after", "2.5"),
            ]),
            &Value::Null,
            now,
        );
        assert_eq!(limits.reserve(route, now), Duration::ZERO, "one left");
        assert_eq!(limits.reserve(route, now), Duration::from_millis(2500));
        assert_eq!(
            limits.reserve(route, now + Duration::from_secs(3)),
            Duration::ZERO,
            "refilled"
        );

        // A 429 for the bucket empties it until Discord says.
        limits.update_at(
            route,
            limited,
            &headers(&[
                ("x-ratelimit-bucket", "abc"),
                ("x-ratelimit-remaining", "3"),
                ("retry-after", "4"),
            ]),
            &Value::Null,
            now,
        );
        assert_eq!(limits.reserve(route, now), Duration::from_secs(4));

        // A global one holds every route.
        limits.update_at(
            "GET /users/@me",
            limited,
            &headers(&[("x-ratelimit-global", "true"), ("retry-after", "7")]),
            &Value::Null,
            now,
        );
        assert_eq!(limits.reserve("GET /gateway", now), Duration::from_secs(7));
        assert_eq!(
            limits.reserve("GET /gateway", now + Duration::from_secs(8)),
            Duration::ZERO
        );
    }

    #[test]
    fn channels_sharing_a_bucket_count_apart() {
        let limits = Limits::default();
        let now = Instant::now();
        let first = "POST /channels/1/messages";
        let second = "POST /channels/2/messages";
        let answer = |route, remaining| {
            limits.update_at(
                route,
                StatusCode::OK,
                &headers(&[
                    ("x-ratelimit-bucket", "abc"),
                    ("x-ratelimit-remaining", remaining),
                    ("x-ratelimit-reset-after", "5"),
                ]),
                &Value::Null,
                now,
            );
        };
        answer(first, "0");
        answer(second, "4");
        assert_eq!(
            limits.reserve(first, now),
            Duration::from_secs(5),
            "the second channel's answer leaves the first one's count"
        );
        for _ in 0..4 {
            assert_eq!(limits.reserve(second, now), Duration::ZERO);
        }
        assert_eq!(limits.reserve(second, now), Duration::from_secs(5));

        // A 429 in one channel doesn't hold the other.
        let limits = Limits::default();
        answer(second, "4");
        limits.update_at(
            first,
            StatusCode::TOO_MANY_REQUESTS,
            &headers(&[("x-ratelimit-bucket", "abc"), ("retry-after", "3")]),
            &Value::Null,
            now,
        );
        assert_eq!(limits.reserve(first, now), Duration::from_secs(3));
        assert_eq!(limits.reserve(second, now), Duration::ZERO);
    }

    #[test]
    fn a_429_without_a_bucket_still_waits() {
        let limits = Limits::default();
        let now = Instant::now();
        let route = "PUT /channels/1/messages/:id/reactions/:emoji/@me";
        let limited = StatusCode::TOO_MANY_REQUESTS;
        limits.update_at(
            route,
            limited,
            &HeaderMap::new(),
            &json!({"retry_after": 0.75, "global": false}),
            now,
        );
        assert_eq!(limits.reserve(route, now), Duration::from_millis(750));
        assert_eq!(
            limits.reserve("PUT /channels/2/messages/:id/reactions/:emoji/@me", now),
            Duration::ZERO,
            "another channel goes"
        );
        assert_eq!(limits.reserve(route, now + Duration::from_secs(1)), Duration::ZERO);

        // Only the header.
        limits.update_at(route, limited, &headers(&[("retry-after", "2")]), &Value::Null, now);
        assert_eq!(limits.reserve(route, now), Duration::from_secs(2));

        // Nothing at all: a second.
        let other = "GET /users/:id/profile";
        limits.update_at(other, limited, &HeaderMap::new(), &Value::Null, now);
        assert_eq!(limits.reserve(other, now), Duration::from_secs(1));

        // Global in the body alone holds every route.
        limits.update_at(
            other,
            limited,
            &HeaderMap::new(),
            &json!({"retry_after": 6.0, "global": true}),
            now,
        );
        assert_eq!(limits.reserve("GET /gateway", now), Duration::from_secs(6));
    }

    #[test]
    fn finds_the_major_part_of_a_route() {
        assert_eq!(major("POST /channels/1/messages"), "/channels/1");
        assert_eq!(major("GET /guilds/2"), "/guilds/2");
        assert_eq!(major("POST /webhooks/3/token/messages"), "/webhooks/3/token");
        assert_eq!(major("GET /users/@me"), "");
    }

    #[test]
    fn a_window_counts_only_what_is_recent() {
        let start = Instant::now();
        let mut window = Window::new(2, Duration::from_secs(10));
        window.push(start);
        assert_eq!(window.full(start), None);
        window.push(start + Duration::from_secs(4));
        assert_eq!(
            window.full(start + Duration::from_secs(5)),
            Some(Duration::from_secs(5)),
            "full until the first is ten seconds old"
        );
        assert_eq!(window.full(start + Duration::from_secs(10)), None);
    }

    #[test]
    fn refusals_stop_requests_before_discord_blocks_the_address() {
        let refusals = Refusals::default();
        for _ in 0..REFUSAL_LIMIT - 1 {
            refusals.answered(StatusCode::FORBIDDEN);
            refusals.answered(StatusCode::OK);
            refusals.answered(StatusCode::NOT_FOUND);
        }
        assert_eq!(refusals.check(), Ok(()));
        refusals.answered(StatusCode::TOO_MANY_REQUESTS);
        assert!(matches!(refusals.check(), Err(Stopped::Refused(wait)) if wait > Duration::from_secs(9 * 60)));

        let refusals = Refusals::default();
        refusals.answered(StatusCode::UNAUTHORIZED);
        assert_eq!(refusals.check(), Ok(()), "one refused request isn't a refused login");
        refusals.unauthorized();
        assert_eq!(refusals.check(), Err(Stopped::Unauthorized));
    }
}
