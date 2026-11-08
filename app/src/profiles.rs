// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Profiles: named sets of accounts, each with its own view (pinned and hidden
//! chats, rail order, panel widths, local People stars) and People merges.
//! The live view sits in the top-level `Settings` fields so the rest of the
//! app reads it as before; [`Settings::switch_view`] swaps it with the
//! stored one, and [`Settings::sync_view`] writes it back before saving.

use serde::{Deserialize, Serialize};

use crate::settings::Settings;

/// What differs per profile (everything about how the accounts are laid out).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileView {
    pub pinned_rooms: Vec<String>,
    pub hidden_rooms: Vec<String>,
    pub server_order: Vec<String>,
    pub account_order: Vec<String>,
    pub collapsed: Vec<String>,
    pub collapsed_categories: Vec<String>,
    pub person_favourites: Vec<String>,
    pub sidebar_width: f32,
    pub members_width: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub id: u32,
    pub name: String,
    /// Its accounts (`network:user`, see `settings::account_key`).
    pub accounts: Vec<String>,
    pub view: ProfileView,
}

/// An account seen signed in, remembered so a profile can start it later
/// without knowing its user in advance: `network` is `Network::key`, `slot`
/// is where its sign-in is kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownAccount {
    pub key: String,
    pub network: String,
    pub slot: u32,
}

impl Settings {
    fn take_view(&mut self) -> ProfileView {
        ProfileView {
            pinned_rooms: std::mem::take(&mut self.pinned_rooms),
            hidden_rooms: std::mem::take(&mut self.hidden_rooms),
            server_order: std::mem::take(&mut self.server_order),
            account_order: std::mem::take(&mut self.account_order),
            collapsed: std::mem::take(&mut self.collapsed),
            collapsed_categories: std::mem::take(&mut self.collapsed_categories),
            person_favourites: std::mem::take(&mut self.person_favourites),
            sidebar_width: std::mem::take(&mut self.sidebar_width),
            members_width: std::mem::take(&mut self.members_width),
        }
    }

    fn put_view(&mut self, v: ProfileView) {
        self.pinned_rooms = v.pinned_rooms;
        self.hidden_rooms = v.hidden_rooms;
        self.server_order = v.server_order;
        self.account_order = v.account_order;
        self.collapsed = v.collapsed;
        self.collapsed_categories = v.collapsed_categories;
        self.person_favourites = v.person_favourites;
        self.sidebar_width = v.sidebar_width;
        self.members_width = v.members_width;
    }

    fn index_of(&self, id: u32) -> Option<usize> {
        self.profiles.iter().position(|p| p.id == id)
    }

    pub fn profile(&self, id: u32) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    /// Makes sure at least one profile exists, as when a settings file from
    /// before profiles loads: "Default" (id 0) holds every account known
    /// from the file and its view is the legacy top-level fields (which
    /// simply stay where they are as the live view). Also repairs
    /// `default_profile` and `current_profile` pointing at nothing.
    pub fn migrate_profiles(&mut self) {
        if self.profiles.is_empty() {
            let mut keys: Vec<String> = Vec::new();
            let found = self
                .known_accounts
                .iter()
                .map(|k| k.key.clone())
                .chain(self.alert_modes.keys().cloned())
                .chain(self.offline.iter().cloned())
                .chain(self.sign_in_needed.iter().map(|s| s.account.clone()))
                .chain(self.account_order.iter().cloned());
            for key in found {
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
            // The live view stays in the top-level fields; sync_view stores
            // a copy in the profile before the next save.
            self.profiles.push(Profile {
                id: 0,
                name: "Default".to_owned(),
                accounts: keys,
                view: ProfileView::default(),
            });
            self.default_profile = 0;
            self.current_profile = 0;
            self.sync_view();
        } else {
            if self.index_of(self.default_profile).is_none() {
                self.default_profile = self.profiles[0].id;
            }
            // A start always opens the default profile (`current_profile`
            // isn't saved).
            self.current_profile = self.default_profile;
            // Loaded profiles hold their views; the live view is the current one's.
            let current = self.current_profile;
            if let Some(i) = self.index_of(current) {
                // A copy: the stored one stays valid until the next sync.
                let v = self.profiles[i].view.clone();
                self.put_view(v);
            }
        }

        // Ids already used (even by deleted profiles) are never handed out again.
        let used = self.profiles.iter().map(|p| p.id + 1).max().unwrap_or(0);
        self.next_profile_id = self.next_profile_id.max(used);
    }

    /// Copies the live view into the current profile (call before saving).
    pub fn sync_view(&mut self) {
        let Some(i) = self.index_of(self.current_profile) else {
            return;
        };
        let view = self.take_view();
        self.put_view(view.clone());
        self.profiles[i].view = view;
    }

    /// Makes `to` the current profile: the live view goes into the old one
    /// and the new one's becomes the live view. False if `to` doesn't exist.
    pub fn switch_view(&mut self, to: u32) -> bool {
        let Some(new) = self.index_of(to) else {
            return false;
        };
        if to == self.current_profile {
            return true;
        }
        let old = self.index_of(self.current_profile);
        let live = self.take_view();
        if let Some(old) = old {
            self.profiles[old].view = live;
        }
        let v = std::mem::take(&mut self.profiles[new].view);
        self.put_view(v);
        self.current_profile = to;
        true
    }

    /// A new, empty profile; returns its id.
    pub fn create_profile(&mut self, name: &str) -> u32 {
        let id = self
            .next_profile_id
            .max(self.profiles.iter().map(|p| p.id + 1).max().unwrap_or(0));
        self.next_profile_id = id + 1;
        self.profiles.push(Profile {
            id,
            name: name.trim().to_owned(),
            accounts: Vec::new(),
            view: ProfileView::default(),
        });
        id
    }

    pub fn rename_profile(&mut self, id: u32, name: &str) -> bool {
        let name = name.trim();
        match self.profiles.iter_mut().find(|p| p.id == id) {
            Some(p) if !name.is_empty() => {
                p.name = name.to_owned();
                true
            }
            _ => false,
        }
    }

    /// Deletes a profile (never the last one). Deleting the default makes
    /// the first remaining the default; deleting the current one switches to
    /// the default. Its accounts stay signed in, just in no profile.
    pub fn delete_profile(&mut self, id: u32) -> bool {
        if self.profiles.len() < 2 {
            return false;
        }
        let Some(i) = self.index_of(id) else {
            return false;
        };
        if id == self.current_profile {
            // Keep the live view out of the profile being removed.
            let to = if id != self.default_profile {
                self.default_profile
            } else {
                self.profiles.iter().find(|p| p.id != id).map_or(id, |p| p.id)
            };
            self.switch_view(to);
        }
        self.profiles.remove(i);
        if self.default_profile == id {
            self.default_profile = self.profiles[0].id;
        }
        true
    }

    /// Adds an account to a profile (an account may be in several).
    pub fn assign_account(&mut self, profile: u32, key: &str) {
        if let Some(p) = self.profiles.iter_mut().find(|p| p.id == profile)
            && !p.accounts.iter().any(|k| k == key)
        {
            p.accounts.push(key.to_owned());
        }
    }

    pub fn unassign_account(&mut self, profile: u32, key: &str) {
        if let Some(p) = self.profiles.iter_mut().find(|p| p.id == profile) {
            p.accounts.retain(|k| k != key);
        }
    }

    /// An account running (and ready) in the current profile belongs to it:
    /// a switch only keeps the current profile's accounts running, so one
    /// seen ready is one just added, an old sign-in seen for the first time,
    /// or the same user signed in again under a new slot (which another
    /// profile may already list). Returns the profile if it joined now.
    pub fn profile_of_new_account(&mut self, key: &str) -> Option<u32> {
        if self.in_current_profile(key) {
            return None;
        }
        let id = self.current_profile;
        self.assign_account(id, key);
        self.profile(id).map(|_| id)
    }

    /// An account was signed out by the user: no profile lists it and its
    /// slot is forgotten (the sign-in itself is gone), so a switch never
    /// tries to start it again.
    pub fn forget_account(&mut self, key: &str) {
        self.known_accounts.retain(|k| k.key != key);
        for p in &mut self.profiles {
            p.accounts.retain(|k| k != key);
        }
    }

    /// Remembers where an account's sign-in is kept (updated each time it
    /// is seen ready).
    pub fn record_known(&mut self, key: &str, network: &str, slot: u32) {
        let known = KnownAccount {
            key: key.to_owned(),
            network: network.to_owned(),
            slot,
        };
        // A slot holds one sign-in: another key remembered for it is stale
        // (signed out, the slot then used by someone else) and would make a
        // profile start this account.
        self.known_accounts
            .retain(|k| k.key == key || k.network != network || k.slot != slot);
        match self.known_accounts.iter_mut().find(|k| k.key == key) {
            Some(k) => *k = known,
            None => self.known_accounts.push(known),
        }
    }

    /// Whether the current profile has this account.
    pub fn in_current_profile(&self, key: &str) -> bool {
        self.profile(self.current_profile)
            .is_some_and(|p| p.accounts.iter().any(|k| k == key))
    }
}

/// An account the app runs, as far as switching cares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Running {
    /// Its key once signed in.
    pub key: Option<String>,
    pub network: String,
    pub slot: u32,
}

/// What a profile change does to the running accounts.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Indexes into the running list to stop.
    pub stop: Vec<usize>,
    /// `(network key, slot)` to start.
    pub start: Vec<(String, u32)>,
}

/// An account's key: its own once signed in, else the one remembered for
/// its network and slot (an account still connecting, or a sign-in form,
/// has none).
fn key_of<'a>(r: &'a Running, known: &'a [KnownAccount]) -> Option<&'a str> {
    r.key.as_deref().or_else(|| {
        known
            .iter()
            .find(|k| k.network == r.network && k.slot == r.slot)
            .map(|k| k.key.as_str())
    })
}

/// What to stop and start so exactly `profile`'s accounts run. Forms and
/// accounts of unknown identity stop too (a sign-in is cancelled by a
/// switch); a profile's account never seen signed in can't be started
/// (its slot is unknown).
pub fn plan(running: &[Running], profile: &Profile, known: &[KnownAccount]) -> Plan {
    let wanted = |key: &str| profile.accounts.iter().any(|k| k == key);
    let mut out = Plan::default();
    for (i, r) in running.iter().enumerate() {
        if !key_of(r, known).is_some_and(wanted) {
            out.stop.push(i);
        }
    }
    for key in &profile.accounts {
        let Some(k) = known.iter().find(|k| k.key == *key) else {
            continue;
        };
        let up = running.iter().enumerate().any(|(i, r)| {
            !out.stop.contains(&i) && (r.key.as_deref() == Some(key) || (r.network == k.network && r.slot == k.slot))
        });
        if !up && !out.start.contains(&(k.network.clone(), k.slot)) {
            out.start.push((k.network.clone(), k.slot));
        }
    }
    out
}

/// Which saved sign-ins to start with: those of the profile, and any whose
/// account isn't known yet (the first start after an upgrade, or a sign-in
/// that never became ready), which join a profile once ready.
/// `saved` is `(network key, slots)` per network.
pub fn startup_slots(saved: &[(String, Vec<u32>)], profile: &Profile, known: &[KnownAccount]) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    for (network, slots) in saved {
        for &slot in slots {
            let wanted = known
                .iter()
                .find(|k| k.network == *network && k.slot == slot)
                .is_none_or(|k| profile.accounts.contains(&k.key));
            if wanted {
                out.push((network.clone(), slot));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kn(key: &str, network: &str, slot: u32) -> KnownAccount {
        KnownAccount {
            key: key.into(),
            network: network.into(),
            slot,
        }
    }

    fn run(key: Option<&str>, network: &str, slot: u32) -> Running {
        Running {
            key: key.map(Into::into),
            network: network.into(),
            slot,
        }
    }

    fn prof(keys: &[&str]) -> Profile {
        Profile {
            accounts: keys.iter().map(|k| (*k).to_owned()).collect(),
            ..Profile::default()
        }
    }

    #[test]
    fn a_switch_stops_the_others_and_starts_the_missing() {
        let known = [
            kn("stoat:a", "stoat", 0),
            kn("steam:b", "steam", 1),
            kn("discord:c", "discord", 0),
        ];
        let running = [
            run(Some("stoat:a"), "stoat", 0),
            run(Some("steam:b"), "steam", 1),
            run(None, "slack", 0),
        ];
        let p = plan(&running, &prof(&["stoat:a", "discord:c", "slack:z"]), &known);
        assert_eq!(p.stop, vec![1, 2], "steam leaves and the form is cancelled");
        assert_eq!(
            p.start,
            vec![("discord".to_owned(), 0)],
            "slack:z was never seen, so no slot"
        );
    }

    #[test]
    fn an_account_still_connecting_is_matched_by_its_slot() {
        let known = [kn("stoat:a", "stoat", 0)];
        let p = plan(&[run(None, "stoat", 0)], &prof(&["stoat:a"]), &known);
        assert_eq!(p, Plan::default(), "neither stopped nor started twice");
        let p = plan(&[run(None, "stoat", 0)], &prof(&[]), &known);
        assert_eq!(p.stop, vec![0]);
    }

    #[test]
    fn startup_starts_the_profile_and_the_unknown() {
        let known = [kn("stoat:a", "stoat", 0), kn("steam:b", "steam", 0)];
        let saved = [("stoat".to_owned(), vec![0, 1]), ("steam".to_owned(), vec![0])];
        let got = startup_slots(&saved, &prof(&["stoat:a"]), &known);
        assert_eq!(got, vec![("stoat".to_owned(), 0), ("stoat".to_owned(), 1)]);
    }

    #[test]
    fn deleted_profile_ids_are_not_reused() {
        let mut s = Settings::default();
        s.migrate_profiles();
        let a = s.create_profile("A");
        assert!(s.delete_profile(a));
        let b = s.create_profile("B");
        assert_ne!(a, b);
        let json = serde_json::to_string(&s).unwrap();
        let mut back: Settings = serde_json::from_str(&json).unwrap();
        back.migrate_profiles();
        assert!(back.create_profile("C") > b);
        // An old file without the counter still never goes below max+1.
        let mut old: Settings = serde_json::from_str(r#"{"profiles":[{"id":4,"name":"X"}]}"#).unwrap();
        old.migrate_profiles();
        assert_eq!(old.create_profile("Y"), 5);
    }

    fn legacy() -> Settings {
        let mut s = Settings {
            pinned_rooms: vec!["a:1|r".into()],
            sidebar_width: 300.0,
            offline: vec!["steam:1".into()],
            ..Settings::default()
        };
        s.record_known("stoat:me", "stoat", 0);
        s
    }

    #[test]
    fn old_settings_become_one_default_profile() {
        let mut s = legacy();
        s.migrate_profiles();
        assert_eq!(s.profiles.len(), 1);
        let p = &s.profiles[0];
        assert_eq!((p.id, p.name.as_str()), (0, "Default"));
        assert_eq!(p.accounts, vec!["stoat:me", "steam:1"]);
        assert_eq!(p.view.pinned_rooms, vec!["a:1|r"]);
        assert_eq!(p.view.sidebar_width, 300.0);
        assert_eq!(s.pinned_rooms, vec!["a:1|r"], "the live view is kept");
        let before = s.clone();
        s.migrate_profiles();
        assert_eq!(s, before, "migrating again changes nothing");
    }

    #[test]
    fn a_file_without_profiles_loads() {
        let mut s: Settings = serde_json::from_str(r#"{"pinned_rooms":["x"]}"#).unwrap();
        s.migrate_profiles();
        assert_eq!(s.profiles[0].view.pinned_rooms, vec!["x"]);
    }

    #[test]
    fn switching_swaps_the_views_and_syncing_stores_the_live_one() {
        let mut s = legacy();
        s.migrate_profiles();
        let work = s.create_profile("Work");
        assert!(s.switch_view(work));
        assert!(s.pinned_rooms.is_empty() && s.sidebar_width == 0.0);
        s.pinned_rooms.push("w:1|r".into());
        assert!(s.switch_view(0));
        assert_eq!(s.pinned_rooms, vec!["a:1|r"]);
        assert_eq!(s.profile(work).unwrap().view.pinned_rooms, vec!["w:1|r"]);
        s.hidden_rooms.push("h".into());
        s.sync_view();
        assert_eq!(s.profile(0).unwrap().view.hidden_rooms, vec!["h"]);
        assert!(!s.switch_view(99));
        assert_eq!(s.current_profile, 0);
    }

    #[test]
    fn a_saved_and_reloaded_file_shows_the_current_profile() {
        let mut s = legacy();
        s.migrate_profiles();
        let work = s.create_profile("Work");
        s.switch_view(work);
        s.collapsed.push("c".into());
        s.sync_view();
        let json = serde_json::to_string(&s).unwrap();
        let mut back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.current_profile, 0, "the current profile is runtime-only");
        back.migrate_profiles();
        assert_eq!(back.pinned_rooms, vec!["a:1|r"]);
        back.switch_view(work);
        assert_eq!(back.collapsed, vec!["c"]);
    }

    #[test]
    fn profile_management() {
        let mut s = legacy();
        s.migrate_profiles();
        assert!(!s.delete_profile(0), "the last profile stays");
        let a = s.create_profile(" Work ");
        assert_eq!(s.profile(a).unwrap().name, "Work");
        assert!(s.rename_profile(a, "Job"));
        assert!(!s.rename_profile(a, "  "));
        s.assign_account(a, "steam:1");
        s.assign_account(a, "steam:1");
        assert_eq!(s.profile(a).unwrap().accounts, vec!["steam:1"]);
        s.unassign_account(a, "steam:1");
        assert!(s.profile(a).unwrap().accounts.is_empty());

        s.switch_view(a);
        s.default_profile = a;
        assert!(s.delete_profile(a), "the current and default one can go");
        assert_eq!((s.current_profile, s.default_profile), (0, 0));
        assert_eq!(s.pinned_rooms, vec!["a:1|r"]);
        assert!(!s.delete_profile(0));
    }

    #[test]
    fn a_new_account_joins_the_current_profile_once() {
        let mut s = legacy();
        s.migrate_profiles();
        let a = s.create_profile("Work");
        s.switch_view(a);
        assert_eq!(s.profile_of_new_account("discord:9"), Some(a));
        assert!(s.in_current_profile("discord:9"));
        assert_eq!(s.profile_of_new_account("discord:9"), None);
        assert_eq!(
            s.profile_of_new_account("stoat:me"),
            Some(a),
            "running here, so it belongs here even if Default has it"
        );
        s.forget_account("discord:9");
        assert!(!s.in_current_profile("discord:9"));
    }

    #[test]
    fn a_slot_remembers_one_account() {
        let mut s = Settings::default();
        s.record_known("steam:1", "steam", 0);
        s.record_known("steam:2", "steam", 0);
        assert_eq!(s.known_accounts.len(), 1);
        assert_eq!(s.known_accounts[0].key, "steam:2");
        s.record_known("stoat:x", "stoat", 0);
        assert_eq!(s.known_accounts.len(), 2, "another network's slot 0 is its own");
        s.forget_account("steam:2");
        assert_eq!(s.known_accounts.len(), 1);
    }

    #[test]
    fn known_accounts_are_updated_in_place() {
        let mut s = Settings::default();
        s.record_known("steam:1", "steam", 0);
        s.record_known("steam:1", "steam", 2);
        assert_eq!(s.known_accounts.len(), 1);
        assert_eq!(s.known_accounts[0].slot, 2);
    }
}
