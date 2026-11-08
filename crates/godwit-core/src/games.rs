// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! "Games in common": people's game libraries, and which games they share,
//! to see what a group can play together. Networks that keep libraries
//! (see [`crate::network::Network::has_game_libraries`]) load them; the
//! comparison is the same for all of them.

use std::sync::Arc;

/// A game someone owns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedGame {
    /// The network's id for the game (the same for every owner).
    pub id: u64,
    pub name: Arc<str>,
    /// A wide picture of it (a store capsule), where the network has one.
    pub image_url: Option<Arc<str>>,
}

/// What loading someone's library gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Library {
    /// Their games (shared: the network keeps the same list for the session).
    Games(Arc<[OwnedGame]>),
    /// They don't show their games to us. Networks report an empty answer
    /// as this: a hidden library looks the same as an empty one, and
    /// saying "owns nothing" would be wrong far more often.
    Private,
    Failed(Arc<str>),
}

/// The most people a comparison can tell apart (owners are a bitmask).
pub const MAX_PEOPLE: usize = 32;

/// A game in a comparison, and who owns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shared<'a> {
    pub game: &'a OwnedGame,
    /// Bit `i`: person `i` owns it.
    pub owners: u32,
}

impl Shared<'_> {
    pub fn owner_count(&self) -> u32 {
        self.owners.count_ones()
    }
}

/// Every bit for `people` people.
pub fn everyone_mask(people: usize) -> u32 {
    if people >= MAX_PEOPLE {
        u32::MAX
    } else {
        (1u32 << people) - 1
    }
}

/// Every game anyone in `libraries` owns, with its owners, by owner count
/// (most first), then name. People past [`MAX_PEOPLE`] are left out.
pub fn compare<'a>(libraries: &[&'a [OwnedGame]]) -> Vec<Shared<'a>> {
    let mut by_id: std::collections::HashMap<u64, Shared<'a>> = std::collections::HashMap::new();
    for (i, games) in libraries.iter().take(MAX_PEOPLE).enumerate() {
        for game in games.iter() {
            by_id.entry(game.id).or_insert(Shared { game, owners: 0 }).owners |= 1 << i;
        }
    }
    let mut shared: Vec<Shared<'a>> = by_id.into_values().collect();
    // Names compare without case, as a library lists them; keys are made
    // once per game, not per comparison.
    shared.sort_by_cached_key(|s| {
        (
            std::cmp::Reverse(s.owner_count()),
            s.game.name.to_lowercase(),
            s.game.id,
        )
    });
    shared
}

/// The games all `people` own.
pub fn everyone<'s, 'a>(shared: &'s [Shared<'a>], people: usize) -> impl Iterator<Item = &'s Shared<'a>> {
    let all = everyone_mask(people);
    shared.iter().filter(move |s| s.owners == all)
}

/// The games all but one of `people` own, with who lacks each (their
/// index). Only for three or more: with two, "all but one" is either one.
pub fn missing_one<'s, 'a>(shared: &'s [Shared<'a>], people: usize) -> impl Iterator<Item = (&'s Shared<'a>, usize)> {
    let all = everyone_mask(people);
    shared.iter().filter(move |_| people >= 3).filter_map(move |s| {
        let lacking = all & !s.owners;
        (lacking.count_ones() == 1).then(|| (s, lacking.trailing_zeros() as usize))
    })
}

/// How many games are in each region of a Venn diagram of `people` (2 or
/// 3): index `mask` counts the games owned by exactly those people. Index 0
/// stays 0. None for other sizes, which a diagram can't show.
pub fn regions(shared: &[Shared<'_>], people: usize) -> Option<Vec<u32>> {
    if !(2..=3).contains(&people) {
        return None;
    }
    let mut counts = vec![0; 1 << people];
    for s in shared {
        if let Some(c) = usize::try_from(s.owners).ok().and_then(|m| counts.get_mut(m)) {
            *c += 1;
        }
    }
    Some(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(id: u64, name: &str) -> OwnedGame {
        OwnedGame {
            id,
            name: name.into(),
            image_url: None,
        }
    }

    fn names(list: &[&Shared<'_>]) -> Vec<String> {
        list.iter().map(|s| s.game.name.to_string()).collect()
    }

    #[test]
    fn sorts_by_owner_count_then_name() {
        let a = [game(1, "zeta"), game(2, "Alpha"), game(3, "mid")];
        let b = [game(1, "zeta"), game(3, "mid"), game(4, "beta")];
        let c = [game(3, "mid")];
        let shared = compare(&[&a, &b, &c]);
        let order: Vec<(&str, u32)> = shared.iter().map(|s| (&*s.game.name, s.owners)).collect();
        assert_eq!(
            order,
            [("mid", 0b111), ("zeta", 0b011), ("Alpha", 0b001), ("beta", 0b010)]
        );
    }

    #[test]
    fn everyone_and_missing_one() {
        let a = [game(1, "Portal"), game(2, "Dota"), game(3, "Goose")];
        let b = [game(1, "Portal"), game(2, "Dota")];
        let c = [game(1, "Portal"), game(3, "Goose")];
        let d = [game(1, "Portal"), game(2, "Dota"), game(3, "Goose")];
        let shared = compare(&[&a, &b, &c, &d]);
        let all: Vec<_> = everyone(&shared, 4).collect();
        assert_eq!(names(&all), ["Portal"]);
        let missing: Vec<(String, usize)> = missing_one(&shared, 4)
            .map(|(s, who)| (s.game.name.to_string(), who))
            .collect();
        // Dota: everyone but c (2); Goose: everyone but b (1).
        assert_eq!(missing, [("Dota".to_owned(), 2), ("Goose".to_owned(), 1)]);
        // Two people: nobody is "the one missing".
        let two = compare(&[&a, &b]);
        assert_eq!(missing_one(&two, 2).count(), 0);
    }

    #[test]
    fn venn_regions_for_two_and_three() {
        let a = [game(1, "x"), game(2, "y"), game(5, "only a")];
        let b = [game(1, "x"), game(3, "z")];
        let c = [game(1, "x"), game(2, "y"), game(3, "z"), game(4, "w")];
        let three = compare(&[&a, &b, &c]);
        // [-, a, b, ab, c, ac, bc, abc]
        assert_eq!(regions(&three, 3), Some(vec![0, 1, 0, 0, 1, 1, 1, 1]));
        let two = compare(&[&a, &b]);
        // [-, a only, b only, both]
        assert_eq!(regions(&two, 2), Some(vec![0, 2, 1, 1]));
        assert_eq!(regions(&compare(&[&a]), 1), None);
        assert_eq!(regions(&compare(&[&a, &b, &c, &a]), 4), None);
    }

    #[test]
    fn empty_libraries_share_nothing() {
        let a = [game(1, "x")];
        let shared = compare(&[&a, &[]]);
        assert_eq!(everyone(&shared, 2).count(), 0);
        assert_eq!(everyone_mask(MAX_PEOPLE), u32::MAX);
    }
}
