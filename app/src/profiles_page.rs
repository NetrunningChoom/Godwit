// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! What the rail's profile switcher and Settings → Profiles show: the rows,
//! and which profile or account each one is (the UI only knows row numbers).

use std::collections::HashMap;
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::shell::ProfileInfo;
use crate::ui::{AppWindow, ProfileItem, ProfileRowItem, ProfilesState};

/// What a row of the Profiles page stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKey {
    Profile(u32),
    /// An account (its key, `network:user`) under a profile.
    Account(u32, String),
}

/// An account the page can put in a profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountEntry {
    pub key: String,
    pub label: String,
    pub network: String,
}

/// Every account to offer: `keys` in order (duplicates dropped), named from
/// `live` (key -> network, label) when it is running, else by its user and
/// the network `network_name` finds for its key.
pub fn entries(
    keys: impl IntoIterator<Item = String>,
    live: &HashMap<String, (String, String)>,
    network_name: impl Fn(&str) -> String,
) -> Vec<AccountEntry> {
    let mut out: Vec<AccountEntry> = Vec::new();
    for key in keys {
        if out.iter().any(|e| e.key == key) {
            continue;
        }
        let (network, label) = match live.get(&key) {
            Some((network, label)) => (network.clone(), label.clone()),
            None => {
                let (net, user) = key.split_once(':').unwrap_or(("", key.as_str()));
                (network_name(net), user.to_owned())
            }
        };
        out.push(AccountEntry { key, label, network });
    }
    out
}

/// The page's rows: each profile, then every account with its toggle.
pub fn rows(
    profiles: &[ProfileInfo],
    accounts: &[AccountEntry],
    has: impl Fn(u32, &str) -> bool,
) -> Vec<(RowKey, ProfileRowItem)> {
    let mut out = Vec::new();
    for p in profiles {
        out.push((
            RowKey::Profile(p.id),
            ProfileRowItem {
                header: true,
                name: p.name.as_str().into(),
                current: p.current,
                default: p.default,
                // The last one can't go.
                can_delete: profiles.len() > 1,
                detail: if p.accounts == 1 {
                    "1 account".into()
                } else {
                    format!("{} accounts", p.accounts).into()
                },
                label: "".into(),
                network: "".into(),
                member: false,
            },
        ));
        for a in accounts {
            out.push((
                RowKey::Account(p.id, a.key.clone()),
                ProfileRowItem {
                    header: false,
                    name: "".into(),
                    current: false,
                    default: false,
                    can_delete: false,
                    detail: "".into(),
                    label: a.label.as_str().into(),
                    network: a.network.as_str().into(),
                    member: has(p.id, &a.key),
                },
            ));
        }
    }
    out
}

/// The two models, bound to the UI once (a new model each time would make
/// every row look changed and lose a name being typed).
pub struct ProfilesView {
    rail: Rc<VecModel<ProfileItem>>,
    page: Rc<VecModel<ProfileRowItem>>,
    keys: Vec<RowKey>,
    bound: bool,
}

impl Default for ProfilesView {
    fn default() -> Self {
        Self {
            rail: Rc::new(VecModel::default()),
            page: Rc::new(VecModel::default()),
            keys: Vec::new(),
            bound: false,
        }
    }
}

impl ProfilesView {
    /// The rail's tile and menu.
    pub fn show_rail(&mut self, ui: &AppWindow, profiles: &[ProfileInfo]) {
        if !self.bound {
            self.bound = true;
            ui.set_profiles(ModelRc::from(self.rail.clone()));
            ui.global::<ProfilesState>().set_rows(ModelRc::from(self.page.clone()));
        }
        if let Some(current) = profiles.iter().find(|p| p.current) {
            ui.set_profile_name(current.name.as_str().into());
            ui.set_profile_initial(current.initial.as_str().into());
        }
        crate::view::sync(
            &self.rail,
            profiles
                .iter()
                .map(|p| ProfileItem {
                    name: p.name.as_str().into(),
                    initial: p.initial.as_str().into(),
                    current: p.current,
                    alert: p.alert,
                })
                .collect(),
        );
    }

    /// Settings → Profiles.
    pub fn show_page(&mut self, rows: Vec<(RowKey, ProfileRowItem)>) {
        let (keys, items): (Vec<_>, Vec<_>) = rows.into_iter().unzip();
        self.keys = keys;
        crate::view::sync(&self.page, items);
    }

    pub fn key(&self, row: i32) -> Option<&RowKey> {
        usize::try_from(row).ok().and_then(|i| self.keys.get(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: u32, name: &str, current: bool, accounts: usize) -> ProfileInfo {
        ProfileInfo {
            id,
            name: name.into(),
            initial: name.chars().take(1).collect(),
            current,
            default: id == 0,
            accounts,
            alert: false,
        }
    }

    fn entry(key: &str) -> AccountEntry {
        AccountEntry {
            key: key.into(),
            label: key.into(),
            network: "Net".into(),
        }
    }

    #[test]
    fn rows_list_every_account_under_every_profile() {
        let profiles = [info(0, "Work", true, 1), info(3, "Private", false, 0)];
        let accounts = [entry("a:1"), entry("b:2")];
        let r = rows(&profiles, &accounts, |p, k| p == 0 && k == "a:1");
        let keys: Vec<_> = r.iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(
            keys,
            vec![
                RowKey::Profile(0),
                RowKey::Account(0, "a:1".into()),
                RowKey::Account(0, "b:2".into()),
                RowKey::Profile(3),
                RowKey::Account(3, "a:1".into()),
                RowKey::Account(3, "b:2".into()),
            ]
        );
        assert!(r[1].1.member && !r[2].1.member && !r[4].1.member);
        assert_eq!(r[0].1.detail.as_str(), "1 account");
        assert_eq!(r[3].1.detail.as_str(), "0 accounts");
        assert!(r[0].1.header && r[0].1.current && r[0].1.default && !r[3].1.default);
    }

    #[test]
    fn the_last_profile_cannot_be_deleted() {
        let only = [info(0, "Work", true, 0)];
        assert!(!rows(&only, &[], |_, _| false)[0].1.can_delete);
        let two = [info(0, "Work", true, 0), info(1, "Private", false, 0)];
        assert!(rows(&two, &[], |_, _| false)[0].1.can_delete);
    }

    #[test]
    fn entries_dedupe_and_name_from_live_accounts() {
        let mut live = HashMap::new();
        live.insert("stoat:7".to_owned(), ("Stoat".to_owned(), "Ann (@ann)".to_owned()));
        let e = entries(
            ["stoat:7".to_owned(), "disc:9".to_owned(), "stoat:7".to_owned()],
            &live,
            |n| n.to_uppercase(),
        );
        assert_eq!(e.len(), 2);
        assert_eq!((e[0].network.as_str(), e[0].label.as_str()), ("Stoat", "Ann (@ann)"));
        assert_eq!((e[1].network.as_str(), e[1].label.as_str()), ("DISC", "9"));
    }
}
