// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! How times and dates are written, chosen in Settings and applied app-wide.
//!
//! Stoat's official client offers the same two choices under Settings >
//! Language (a time format of 24-hour or 12-hour, and a date format of
//! DD/MM/YYYY, MM/DD/YYYY or YYYY-MM-DD):
//! for-web `packages/client/components/app/interface/settings/user/Language.tsx`.
//! The choice lives in process-wide atomics so every formatter (message
//! times, chat lists, event times) reads it without threading settings through.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeFormat {
    /// 13:05
    #[default]
    Hour24,
    /// 1:05 PM
    Hour12,
}

impl TimeFormat {
    pub const ALL: [(Self, &'static str); 2] = [(Self::Hour24, "24-hour (13:05)"), (Self::Hour12, "12-hour (1:05 PM)")];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|(o, _)| *o == self).unwrap_or(0)
    }

    pub fn at(index: i32) -> Option<Self> {
        usize::try_from(index)
            .ok()
            .and_then(|i| Self::ALL.get(i))
            .map(|(o, _)| *o)
    }

    fn pattern(self) -> &'static str {
        match self {
            Self::Hour24 => "%H:%M",
            Self::Hour12 => "%-I:%M %p",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateFormat {
    /// 31/12/2026
    #[default]
    DayFirst,
    /// 12/31/2026
    MonthFirst,
    /// 2026-12-31
    Iso,
}

impl DateFormat {
    pub const ALL: [(Self, &'static str); 3] = [
        (Self::DayFirst, "Traditional (DD/MM/YYYY)"),
        (Self::MonthFirst, "American (MM/DD/YYYY)"),
        (Self::Iso, "ISO standard (YYYY-MM-DD)"),
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|(o, _)| *o == self).unwrap_or(0)
    }

    pub fn at(index: i32) -> Option<Self> {
        usize::try_from(index)
            .ok()
            .and_then(|i| Self::ALL.get(i))
            .map(|(o, _)| *o)
    }

    fn pattern(self) -> &'static str {
        match self {
            Self::DayFirst => "%d/%m/%Y",
            Self::MonthFirst => "%m/%d/%Y",
            Self::Iso => "%Y-%m-%d",
        }
    }
}

static TIME: AtomicU8 = AtomicU8::new(0);
static DATE: AtomicU8 = AtomicU8::new(0);

/// Make `time` and `date` the formats every later formatting call uses.
pub fn apply(time: TimeFormat, date: DateFormat) {
    TIME.store(time.index() as u8, Ordering::Relaxed);
    DATE.store(date.index() as u8, Ordering::Relaxed);
}

fn time_format() -> TimeFormat {
    TimeFormat::at(i32::from(TIME.load(Ordering::Relaxed))).unwrap_or_default()
}

fn date_format() -> DateFormat {
    DateFormat::at(i32::from(DATE.load(Ordering::Relaxed))).unwrap_or_default()
}

/// The time of day in the chosen format.
pub fn time_of_day<Tz: chrono::TimeZone>(at: &chrono::DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    at.format(time_format().pattern()).to_string()
}

/// A numeric date in the chosen format.
pub fn numeric_date<Tz: chrono::TimeZone>(at: &chrono::DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    at.format(date_format().pattern()).to_string()
}

/// A message's time: "Today at <time>", else "<date> <time>".
pub fn message_time<Tz: chrono::TimeZone>(at: &chrono::DateTime<Tz>, today: bool) -> String
where
    Tz::Offset: std::fmt::Display,
{
    if today {
        format!("Today at {}", time_of_day(at))
    } else {
        format!("{} {}", numeric_date(at), time_of_day(at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn formats_follow_the_choice() {
        let at = Utc.with_ymd_and_hms(2026, 12, 31, 13, 5, 0).unwrap();
        let morning = Utc.with_ymd_and_hms(2026, 1, 2, 0, 7, 0).unwrap();
        for (time, date, today, other) in [
            (
                TimeFormat::Hour24,
                DateFormat::DayFirst,
                "Today at 13:05",
                "31/12/2026 13:05",
            ),
            (
                TimeFormat::Hour12,
                DateFormat::MonthFirst,
                "Today at 1:05 PM",
                "12/31/2026 1:05 PM",
            ),
            (
                TimeFormat::Hour24,
                DateFormat::Iso,
                "Today at 13:05",
                "2026-12-31 13:05",
            ),
        ] {
            apply(time, date);
            assert_eq!(message_time(&at, true), today);
            assert_eq!(message_time(&at, false), other);
        }
        apply(TimeFormat::Hour12, DateFormat::Iso);
        assert_eq!(time_of_day(&morning), "12:07 AM");
        apply(TimeFormat::default(), DateFormat::default());
    }

    #[test]
    fn choices_round_trip_through_their_index() {
        for (t, _) in TimeFormat::ALL {
            assert_eq!(TimeFormat::at(t.index() as i32), Some(t));
        }
        for (d, _) in DateFormat::ALL {
            assert_eq!(DateFormat::at(d.index() as i32), Some(d));
        }
        assert_eq!(TimeFormat::at(-1), None);
    }
}
