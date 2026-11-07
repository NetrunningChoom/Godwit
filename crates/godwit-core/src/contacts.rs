// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Metacontacts: one person across networks.
//!
//! The user merges identities by hand (Godwit may suggest, never decides). A
//! network identity belongs to at most one metacontact. Stored locally only.

use serde::{Deserialize, Serialize};

use crate::ids::UserId;
use crate::network::Network;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContactId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metacontact {
    pub id: ContactId,
    pub name: String,
    /// The person's accounts, as seen from ours. Never empty.
    pub identities: Vec<UserId>,
    /// The network the user chose for this person; overrides the ranking.
    pub preferred: Option<Network>,
    /// The exact identity the user chose, when they're reachable through more
    /// than one of our accounts on that network (two Steam sign-ins). Older
    /// contact files lack it; [`Self::preferred`] still applies then.
    #[serde(default)]
    pub preferred_user: Option<UserId>,
    /// A "switch to a better network" suggestion was shown for this person.
    pub nudged: bool,
    /// The user asked never to suggest a switch for this person.
    pub no_nudges: bool,
    /// People the user said this isn't: never suggested as a link again.
    #[serde(default)]
    pub not_same: Vec<ContactId>,
    /// The user's own name for them, on this device only; shown in place
    /// of the networks' names.
    #[serde(default)]
    pub nickname: Option<String>,
}

impl Metacontact {
    /// The network to talk to this person on: the user's choice if it's
    /// still one of theirs, else the best-ranked one (ties keep merge order).
    pub fn best_network(&self) -> Option<Network> {
        if let Some(p) = self.preferred
            && self.identities.iter().any(|u| u.network() == p)
        {
            return Some(p);
        }
        let mut best: Option<Network> = None;
        for n in self.identities.iter().map(UserId::network) {
            if best.is_none_or(|b| n.security().score() > b.security().score()) {
                best = Some(n);
            }
        }
        best
    }

    /// Who to message: the identity the user chose if `usable` (we're signed in
    /// there) and still theirs, else one on [`Self::best_network`], else any
    /// usable one.
    pub fn best_identity(&self, usable: impl Fn(&UserId) -> bool) -> Option<&UserId> {
        if let Some(chosen) = &self.preferred_user
            && self.preferred == Some(chosen.network())
            && let Some(user) = self.identities.iter().find(|u| *u == chosen && usable(u))
        {
            return Some(user);
        }
        let best = self.best_network();
        let mut ours = self.identities.iter().filter(|u| usable(u));
        let first = ours.clone().next();
        ours.find(|u| Some(u.network()) == best).or(first)
    }

    /// A better network to suggest while chatting on `current`, if the
    /// suggestion hasn't been shown or turned off.
    pub fn nudge_from(&self, current: Network) -> Option<Network> {
        if self.nudged || self.no_nudges || self.preferred == Some(current) {
            return None;
        }
        self.best_network()
            .filter(|b| b.security().score() > current.security().score())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contacts {
    next: u64,
    list: Vec<Metacontact>,
}

impl Contacts {
    pub fn iter(&self) -> impl Iterator<Item = &Metacontact> {
        self.list.iter()
    }

    pub fn get(&self, id: ContactId) -> Option<&Metacontact> {
        self.list.iter().find(|c| c.id == id)
    }

    pub fn get_mut(&mut self, id: ContactId) -> Option<&mut Metacontact> {
        self.list.iter_mut().find(|c| c.id == id)
    }

    /// The metacontact `user` belongs to.
    pub fn of(&self, user: &UserId) -> Option<&Metacontact> {
        self.list.iter().find(|c| c.identities.contains(user))
    }

    /// Makes a metacontact for `user`, or returns the one it's already in.
    pub fn ensure(&mut self, user: UserId, name: &str) -> ContactId {
        if let Some(c) = self.of(&user) {
            return c.id;
        }
        self.next += 1;
        let id = ContactId(self.next);
        self.list.push(Metacontact {
            id,
            name: name.to_owned(),
            identities: vec![user],
            preferred: None,
            preferred_user: None,
            nudged: false,
            no_nudges: false,
            not_same: Vec::new(),
            nickname: None,
        });
        id
    }

    /// Moves everything from `from` into `into` (the user said they're the
    /// same person). Returns false if either is missing or they're the same.
    pub fn merge(&mut self, into: ContactId, from: ContactId) -> bool {
        if into == from || self.get(into).is_none() {
            return false;
        }
        let Some(pos) = self.list.iter().position(|c| c.id == from) else {
            return false;
        };
        let gone = self.list.remove(pos);
        if let Some(target) = self.get_mut(into) {
            target.identities.extend(gone.identities);
            if target.preferred.is_none() {
                target.preferred = gone.preferred;
                target.preferred_user = gone.preferred_user;
            }
            target.no_nudges |= gone.no_nudges;
            target.not_same.extend(gone.not_same);
        }
        true
    }

    /// Someone who might be `id` too (see [`suggest_merges`]). Only ever a
    /// suggestion; linking is the user's call.
    pub fn suggestion(&self, id: ContactId) -> Option<ContactId> {
        suggest_merges(&self.list).into_iter().find_map(|(a, b)| {
            if a == id {
                Some(b)
            } else if b == id {
                Some(a)
            } else {
                None
            }
        })
    }

    /// "They aren't the same person": stops suggesting `a` and `b` together.
    pub fn not_same(&mut self, a: ContactId, b: ContactId) {
        for (x, y) in [(a, b), (b, a)] {
            if let Some(c) = self.get_mut(x)
                && !c.not_same.contains(&y)
            {
                c.not_same.push(y);
            }
        }
    }

    /// Takes `user` out of its metacontact into one of its own (a wrong
    /// merge). Returns the new contact, or None if `user` was alone already.
    pub fn split(&mut self, user: &UserId, name: &str) -> Option<ContactId> {
        let c = self.list.iter_mut().find(|c| c.identities.contains(user))?;
        if c.identities.len() < 2 {
            return None;
        }
        c.identities.retain(|u| u != user);
        if c.preferred_user.as_ref() == Some(user) {
            c.preferred_user = None;
        }
        if c.preferred
            .is_some_and(|p| !c.identities.iter().any(|u| u.network() == p))
        {
            c.preferred = None;
        }
        Some(self.ensure(user.clone(), name))
    }
}

/// How alike two names must be to suggest they're one person (Jaro-Winkler
/// on the names lowercased, without spaces or punctuation).
pub const SUGGEST_AT: f64 = 0.95;
/// Shorter names say too little to suggest anything.
const MIN_NAME: usize = 3;

/// Pairs of contacts that might be one person, most alike first: names at
/// least [`SUGGEST_AT`] alike, on different networks (two people on one
/// network are almost always two people), and not said to be different.
/// Only ever suggestions: merging is the user's call.
pub fn suggest_merges(list: &[Metacontact]) -> Vec<(ContactId, ContactId)> {
    let names: Vec<Vec<char>> = list.iter().map(|c| normal(&c.name)).collect();
    let mut pairs: Vec<(f64, ContactId, ContactId)> = Vec::new();
    for (i, a) in list.iter().enumerate() {
        for (j, b) in list.iter().enumerate().skip(i + 1) {
            let (na, nb) = (&names[i], &names[j]);
            if na.len() < MIN_NAME || nb.len() < MIN_NAME || a.not_same.contains(&b.id) || b.not_same.contains(&a.id) {
                continue;
            }
            let shared = a
                .identities
                .iter()
                .any(|u| b.identities.iter().any(|v| v.network() == u.network()));
            if shared {
                continue;
            }
            let score = jaro_winkler(na, nb);
            if score >= SUGGEST_AT {
                pairs.push((score, a.id, b.id));
            }
        }
    }
    pairs.sort_by(|x, y| y.0.total_cmp(&x.0));
    pairs.into_iter().map(|(_, a, b)| (a, b)).collect()
}

/// A name as compared: lowercase letters and digits only.
fn normal(name: &str) -> Vec<char> {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Jaro-Winkler similarity (1.0: the same), as name matching uses it: it
/// forgives small typos and favours a shared beginning.
fn jaro_winkler(a: &[char], b: &[char]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let window = (a.len().max(b.len()) / 2).saturating_sub(1);
    let mut a_hit = vec![false; a.len()];
    let mut b_hit = vec![false; b.len()];
    let mut matches = 0usize;
    for (i, ca) in a.iter().enumerate() {
        let lo = i.saturating_sub(window);
        let hi = (i + window + 1).min(b.len());
        for j in lo..hi {
            if !b_hit[j] && b[j] == *ca {
                a_hit[i] = true;
                b_hit[j] = true;
                matches += 1;
                break;
            }
        }
    }
    if matches == 0 {
        return 0.0;
    }
    let a_seq = a.iter().zip(&a_hit).filter(|(_, h)| **h).map(|(c, _)| c);
    let b_seq = b.iter().zip(&b_hit).filter(|(_, h)| **h).map(|(c, _)| c);
    let transpositions = a_seq.zip(b_seq).filter(|(x, y)| x != y).count() / 2;
    let m = matches as f64;
    let jaro = (m / a.len() as f64 + m / b.len() as f64 + (m - transpositions as f64) / m) / 3.0;
    let prefix = a.iter().zip(b).take(4).take_while(|(x, y)| x == y).count();
    jaro + prefix as f64 * 0.1 * (1.0 - jaro)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::AccountId;

    fn user(n: Network, raw: &str) -> UserId {
        UserId::new(AccountId::new(n, "me"), raw)
    }

    fn alex(c: &mut Contacts) -> ContactId {
        let a = c.ensure(user(Network::Discord, "alex#1"), "Alex");
        let b = c.ensure(user(Network::Stoat, "+31600000000"), "Alex S");
        assert!(c.merge(a, b));
        a
    }

    #[test]
    fn merge_combines_and_picks_the_safer_network() {
        let mut c = Contacts::default();
        let a = alex(&mut c);
        assert_eq!(c.iter().count(), 1);
        let m = c.get(a).unwrap();
        assert_eq!(m.identities.len(), 2);
        assert_eq!(m.best_network(), Some(Network::Stoat));
        assert_eq!(m.nudge_from(Network::Discord), Some(Network::Stoat));
        assert_eq!(m.nudge_from(Network::Stoat), None, "already on the best one");
    }

    #[test]
    fn the_chosen_identity_wins_among_accounts_on_one_network() {
        let mut c = Contacts::default();
        let on = |me: &str| UserId::new(AccountId::new(Network::Steam, me), "friend");
        let a = c.ensure(on("first"), "Friend");
        let b = c.ensure(on("second"), "Friend");
        assert!(c.merge(a, b));
        let all = |_: &UserId| true;
        assert_eq!(c.get(a).unwrap().best_identity(all), Some(&on("first")), "merge order");

        let m = c.get_mut(a).unwrap();
        m.preferred = Some(Network::Steam);
        m.preferred_user = Some(on("second"));
        assert_eq!(c.get(a).unwrap().best_identity(all), Some(&on("second")));
        // Signed out of the chosen one: fall back to one we can use.
        let not_second = |u: &UserId| &*u.account.user != "second";
        assert_eq!(c.get(a).unwrap().best_identity(not_second), Some(&on("first")));

        // Unlinking the chosen identity forgets the choice.
        c.split(&on("second"), "Friend");
        assert_eq!(c.get(a).unwrap().preferred_user, None);
    }

    #[test]
    fn the_users_choice_wins_and_stops_nudges_there() {
        let mut c = Contacts::default();
        let a = alex(&mut c);
        c.get_mut(a).unwrap().preferred = Some(Network::Discord);
        let m = c.get(a).unwrap();
        assert_eq!(m.best_network(), Some(Network::Discord));
        assert_eq!(m.nudge_from(Network::Discord), None);
    }

    #[test]
    fn nudges_show_once_and_respect_never() {
        let mut c = Contacts::default();
        let a = alex(&mut c);
        c.get_mut(a).unwrap().nudged = true;
        assert_eq!(c.get(a).unwrap().nudge_from(Network::Discord), None);
        let m = c.get_mut(a).unwrap();
        m.nudged = false;
        m.no_nudges = true;
        assert_eq!(c.get(a).unwrap().nudge_from(Network::Discord), None);
    }

    #[test]
    fn ensure_is_idempotent_and_merge_rejects_nonsense() {
        let mut c = Contacts::default();
        let u = user(Network::Stoat, "01A");
        let a = c.ensure(u.clone(), "A");
        assert_eq!(c.ensure(u, "A again"), a);
        assert!(!c.merge(a, a));
        assert!(!c.merge(a, ContactId(99)));
    }

    #[test]
    fn suggests_nearly_identical_names_across_networks_until_told_otherwise() {
        let mut c = Contacts::default();
        let a = c.ensure(user(Network::Stoat, "1"), "kalo_rimi");
        let b = c.ensure(user(Network::Steam, "2"), "Kalo Rimi");
        let typo = c.ensure(user(Network::Discord, "5"), "KaloRimi1");
        let same_network = c.ensure(user(Network::Stoat, "3"), "Kalo Rimi");
        let other = c.ensure(user(Network::Steam, "4"), "Kalo Brown");
        let bo = c.ensure(user(Network::Steam, "6"), "Bo");
        c.ensure(user(Network::Stoat, "7"), "bo");
        let pairs = suggest_merges(&c.list);
        assert!(pairs.contains(&(a, b)), "punctuation and case don't count: {pairs:?}");
        assert!(
            pairs.iter().any(|p| *p == (a, typo) || *p == (b, typo)),
            "a one-character difference still is"
        );
        assert!(
            !pairs.iter().any(|(x, y)| (*x, *y) == (a, same_network)),
            "same network: two people"
        );
        assert!(
            !pairs.iter().any(|(x, y)| *x == other || *y == other),
            "a shared first name isn't enough"
        );
        assert!(
            !pairs.iter().any(|(x, y)| *x == bo || *y == bo),
            "too short to mean anything"
        );
        assert!(c.suggestion(b).is_some());
        c.not_same(a, b);
        assert!(!suggest_merges(&c.list).contains(&(a, b)));
        assert!(jaro_winkler(&normal("martha"), &normal("marhta")) > 0.95);
        assert!(jaro_winkler(&normal("alice"), &normal("bob")) < 0.5);
    }

    #[test]
    fn split_undoes_a_merge_and_drops_a_stale_preference() {
        let mut c = Contacts::default();
        let a = alex(&mut c);
        c.get_mut(a).unwrap().preferred = Some(Network::Stoat);
        let signal = user(Network::Stoat, "+31600000000");
        let b = c.split(&signal, "Alex S").unwrap();
        assert_ne!(a, b);
        assert_eq!(c.get(a).unwrap().preferred, None);
        assert_eq!(c.of(&signal).unwrap().id, b);
        assert_eq!(c.split(&signal, "again"), None, "alone already");
    }
}

/// Reading and writing `contacts.json` in the config directory.
pub mod store {
    use std::io;
    use std::path::{Path, PathBuf};

    use super::Contacts;

    pub fn path() -> Option<PathBuf> {
        Some(crate::config_dir::get()?.join("contacts.json"))
    }

    /// The saved contacts; empty if there's no file yet.
    pub fn load(path: &Path) -> io::Result<Contacts> {
        match std::fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Contacts::default()),
            Err(e) => Err(e),
        }
    }

    /// Writes to a temporary file and renames it over the old one, so a crash
    /// mid-write never leaves a truncated file.
    pub fn save(path: &Path, contacts: &Contacts) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(contacts).map_err(io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::ids::{AccountId, UserId};
        use crate::network::Network;

        #[test]
        fn round_trips_and_missing_file_is_empty() {
            let dir = std::env::temp_dir().join(format!("godwit-contacts-test-{}", std::process::id()));
            let path = dir.join("contacts.json");
            assert_eq!(load(&path).unwrap(), Contacts::default());

            let mut c = Contacts::default();
            let a = c.ensure(UserId::new(AccountId::new(Network::Stoat, "me"), "01A"), "Ann");
            let b = c.ensure(UserId::new(AccountId::new(Network::Discord, "@me:x"), "@ann:x"), "Ann");
            assert!(c.merge(a, b));
            save(&path, &c).unwrap();
            assert_eq!(load(&path).unwrap(), c);
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}
