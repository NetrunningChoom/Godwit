// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Times as the official client words them.

use chrono::{Local, TimeZone};

/// A `<t:…>` timestamp in the official formats (dayjs: `hh` is the 12-hour
/// clock without AM/PM there, kept as is). `R` is relative ("in 3 hours").
pub fn timestamp(secs: i64, style: Option<char>) -> String {
    let Some(t) = Local.timestamp_opt(secs, 0).single() else {
        return format!("<t:{secs}>");
    };
    match style {
        Some('t') => t.format("%I:%M").to_string(),
        Some('T') => t.format("%I:%M:%S").to_string(),
        Some('D') => t.format("%d %B %Y").to_string(),
        Some('F') => t.format("%A, %d %B %Y %I:%M").to_string(),
        Some('R') => {
            let diff = chrono::Utc::now().timestamp() - secs;
            if diff >= 0 {
                relative(diff * 1000)
            } else {
                let r = relative(-diff * 1000);
                if r == "just now" {
                    r
                } else {
                    format!("in {}", r.trim_end_matches(" ago"))
                }
            }
        }
        _ => t.format("%d %B %Y %I:%M").to_string(),
    }
}

/// "just now", "5 minutes ago", "3 days ago"… for an age in milliseconds.
pub fn relative(age_ms: i64) -> String {
    let secs = age_ms.max(0) / 1000;
    let (n, unit) = match secs {
        s if s < 60 => return "just now".into(),
        s if s < 3600 => (s / 60, "minute"),
        s if s < 86_400 => (s / 3600, "hour"),
        s if s < 30 * 86_400 => (s / 86_400, "day"),
        s if s < 365 * 86_400 => (s / (30 * 86_400), "month"),
        s => (s / (365 * 86_400), "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_wording() {
        assert_eq!(relative(10_000), "just now");
        assert_eq!(relative(60_000), "1 minute ago");
        assert_eq!(relative(3 * 3_600_000), "3 hours ago");
    }
}
