// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors

//! Planning a "delete many messages" action for a network with a bulk
//! endpoint: which ids go in bulk calls (count and age limits) and which
//! must be deleted one by one.

/// Most messages one "delete from here down" removes.
pub const MAX_PER_ACTION: usize = 100;

/// A plan: bulk batches (each within `max_batch`, at least `min_batch`
/// long) and the ids left to delete individually.
#[derive(Debug, PartialEq, Eq, Default)]
pub struct Plan {
    pub batches: Vec<Vec<String>>,
    pub singles: Vec<String>,
}

/// Split `ids` (with each message's send time in ms) by a bulk endpoint's
/// limits: only messages younger than `max_age_ms` may go in a batch of
/// `min_batch..=max_batch`; the rest (too old, or a leftover short of
/// `min_batch`) are single deletes.
pub fn plan(ids: &[(String, i64)], now_ms: i64, max_age_ms: i64, min_batch: usize, max_batch: usize) -> Plan {
    let mut young = Vec::new();
    let mut singles = Vec::new();
    for (id, at) in ids {
        if now_ms.saturating_sub(*at) < max_age_ms {
            young.push(id.clone());
        } else {
            singles.push(id.clone());
        }
    }
    let mut batches: Vec<Vec<String>> = young.chunks(max_batch.max(1)).map(<[String]>::to_vec).collect();
    if batches.last().is_some_and(|b| b.len() < min_batch)
        && let Some(short) = batches.pop()
    {
        singles.extend(short);
    }
    Plan { batches, singles }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize, at: i64) -> Vec<(String, i64)> {
        (0..n).map(|i| (format!("m{i}"), at)).collect()
    }

    #[test]
    fn chunks_young_messages_and_singles_the_old() {
        let mut all = ids(150, 900);
        all.extend((0..3).map(|i| (format!("old{i}"), 0)));
        let p = plan(&all, 1000, 500, 2, 100);
        assert_eq!(p.batches.iter().map(Vec::len).collect::<Vec<_>>(), [100, 50]);
        assert_eq!(p.singles, ["old0", "old1", "old2"]);
    }

    #[test]
    fn a_leftover_below_the_minimum_goes_single() {
        let p = plan(&ids(101, 900), 1000, 500, 2, 100);
        assert_eq!(p.batches.len(), 1);
        assert_eq!(p.singles, ["m100"]);
    }

    #[test]
    fn nothing_young_means_all_single() {
        let p = plan(&ids(3, 0), 1000, 500, 2, 100);
        assert!(p.batches.is_empty());
        assert_eq!(p.singles.len(), 3);
    }
}
