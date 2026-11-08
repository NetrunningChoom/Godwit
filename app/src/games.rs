// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The "Games in common" page's contents, from the core's comparison
//! (`godwit_core::games`): who can be picked, notes on libraries that
//! can't be compared, the Venn diagram's counts and the list of games.

use std::rc::Rc;
use std::sync::Arc;

use godwit_core::application::{AppState, GamesInCommon};
use godwit_core::games::{self, Library, OwnedGame};
use godwit_core::ids::UserId;
use slint::{Model, ModelRc, SharedString, VecModel};

use crate::images::{Fit, Images};
use crate::ui::{GameRowItem, GamesFriendItem};

/// A store capsule's own size (184×69): never scaled up.
pub const CAPSULE: Fit = Fit::Within(184, 69);
/// Rows decoded beyond the ones on screen, each way.
const MARGIN: usize = 6;
/// Rows the page's list shows at once on a tall window (48 px rows).
const ON_SCREEN: usize = 24;

/// A list row before its picture: a heading, or a game.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub header: bool,
    pub title: SharedString,
    pub image_url: Option<Arc<str>>,
}

/// What the dialog shows, apart from pictures.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contents {
    pub notes: Vec<String>,
    /// 2 or 3 for a Venn diagram, else 0.
    pub venn: usize,
    pub legend: String,
    /// Games per Venn region, by owners' bitmask.
    pub regions: Vec<u32>,
    /// The region listed (its bitmask).
    pub region: u32,
    pub rows: Vec<Row>,
}

/// What `user` is called here: our own name for them, else as a friend or
/// in a member list we have, else their id.
fn name_of(state: &AppState, user: &UserId) -> String {
    if user.raw == user.account.user {
        return "You".to_owned();
    }
    crate::view::person_name(state, user)
        .or_else(|| state.friends.iter().find(|p| p.id == *user).map(|p| p.name.to_string()))
        .or_else(|| {
            state
                .members
                .values()
                .flat_map(|groups| groups.iter())
                .flat_map(|g| g.members.iter())
                .find(|m| m.user == *user)
                .map(|m| m.name.to_string())
        })
        .unwrap_or_else(|| user.raw.to_string())
}

/// The people the picker offers: anyone compared who isn't a friend (a
/// group chat's member), then the account's friends by name. The order
/// doesn't follow picking, so a row stays the same person under its toggle.
pub fn candidates(state: &AppState, g: &GamesInCommon) -> Vec<(UserId, String, bool)> {
    let is_friend = |u: &UserId| state.friends.iter().any(|p| p.id == *u);
    let mut list: Vec<(UserId, String, bool)> = g
        .people
        .iter()
        .skip(1)
        .filter(|u| !is_friend(u))
        .map(|u| (u.clone(), name_of(state, u), true))
        .collect();
    let mut friends: Vec<(UserId, String, bool)> = state
        .friends
        .iter()
        .filter(|p| p.id.account == g.account)
        .map(|p| (p.id.clone(), name_of(state, &p.id), g.people.contains(&p.id)))
        .collect();
    friends.sort_by_cached_key(|(_, name, _)| name.to_lowercase());
    list.extend(friends);
    list
}

/// The picker's rows for the search `query` (case-insensitive, anywhere in
/// the name): whoever is picked stays, on top, so they can be taken out
/// whatever is typed; the rest keep their order. No query: everyone, as is.
pub fn filter_picks(picks: Vec<(UserId, String, bool)>, query: &str) -> Vec<(UserId, String, bool)> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return picks;
    }
    let (picked, rest): (Vec<_>, Vec<_>) = picks.into_iter().partition(|(_, _, picked)| *picked);
    picked
        .into_iter()
        .chain(
            rest.into_iter()
                .filter(|(_, name, _)| name.to_lowercase().contains(&query)),
        )
        .collect()
}

/// Joins names as a sentence does: "You", "You and Ann", "You, Ann and Bob".
fn and_list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn game_row(game: &OwnedGame) -> Row {
    Row {
        header: false,
        title: game.name.as_ref().into(),
        image_url: game.image_url.clone(),
    }
}

fn header(title: String) -> Row {
    Row {
        header: true,
        title: title.into(),
        image_url: None,
    }
}

/// The dialog for `g`, listing Venn region `region` (0: everyone's).
pub fn contents(state: &AppState, g: &GamesInCommon, region: u32) -> Contents {
    let mut out = Contents::default();
    // Only libraries we have can be compared; the others get a line each.
    let mut names = Vec::new();
    let mut libraries: Vec<&[OwnedGame]> = Vec::new();
    for user in &g.people {
        let name = name_of(state, user);
        let mine = user.raw == user.account.user;
        match g.libraries.get(user) {
            Some(Library::Games(list)) => {
                libraries.push(list);
                names.push(name);
            }
            Some(Library::Private) if mine => out.notes.push("Your games are private.".to_owned()),
            Some(Library::Private) => out.notes.push(format!("{name}'s games are private.")),
            Some(Library::Failed(e)) => out.notes.push(format!("Couldn't load {name}'s games: {e}")),
            None if mine => out.notes.push("Loading your games…".to_owned()),
            None => out.notes.push(format!("Loading {name}'s games…")),
        }
    }
    if g.people.len() < 2 {
        out.notes
            .push("Pick friends to see which games you all own.".to_owned());
    }
    let n = libraries.len();
    if n < 2 {
        return out;
    }
    let shared = games::compare(&libraries);
    let all = games::everyone_mask(n);
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    if let Some(regions) = games::regions(&shared, n) {
        out.venn = n;
        out.region = if region == 0 || region > all { all } else { region };
        out.legend = ["Left", "Right", "Bottom"]
            .iter()
            .zip(&name_refs)
            .map(|(side, name)| format!("{side}: {name}"))
            .collect::<Vec<_>>()
            .join(" · ");
        let count = regions.get(out.region as usize).copied().unwrap_or(0);
        let title = if out.region == all {
            format!("Everyone owns ({count})")
        } else {
            let who: Vec<&str> = (0..n)
                .filter(|i| out.region & (1 << i) != 0)
                .map(|i| name_refs[i])
                .collect();
            format!("Only {} ({count})", and_list(&who))
        };
        out.regions = regions;
        out.rows.push(header(title));
        out.rows.extend(
            shared
                .iter()
                .filter(|s| s.owners == out.region)
                .map(|s| game_row(s.game)),
        );
        return out;
    }
    // Four or more: a ranked list, everyone's first, then each "all but one".
    let everyone: Vec<Row> = games::everyone(&shared, n).map(|s| game_row(s.game)).collect();
    out.rows.push(header(format!("Everyone owns ({})", everyone.len())));
    out.rows.extend(everyone);
    let mut missing: Vec<Vec<Row>> = vec![Vec::new(); n];
    for (s, who) in games::missing_one(&shared, n) {
        if let Some(list) = missing.get_mut(who) {
            list.push(game_row(s.game));
        }
    }
    for (who, list) in missing.into_iter().enumerate() {
        if !list.is_empty() {
            let title = if who == 0 {
                format!("Everyone but you ({})", list.len())
            } else {
                format!("Everyone but {} ({})", name_refs[who], list.len())
            };
            out.rows.push(header(title));
            out.rows.extend(list);
        }
    }
    out
}

/// What contents were made from: who, how many libraries, which region.
type Key = (Vec<UserId>, usize, u32);

/// The dialog's models and what's kept between refreshes.
pub struct GamesDialog {
    /// The Venn region picked (0: everyone's).
    pub region: u32,
    /// The list's first row on screen.
    pub first_row: usize,
    /// Who each picker row is.
    pub friends: Vec<UserId>,
    /// The picker's search.
    pub query: String,
    friend_model: Rc<VecModel<GamesFriendItem>>,
    rows: Rc<VecModel<GameRowItem>>,
    /// The last contents and what they were made from (people, libraries
    /// loaded, region), so a refresh doesn't compare libraries again.
    kept: Option<(Key, Contents)>,
}

impl Default for GamesDialog {
    fn default() -> Self {
        Self {
            region: 0,
            first_row: 0,
            friends: Vec::new(),
            query: String::new(),
            friend_model: Rc::new(VecModel::default()),
            rows: Rc::new(VecModel::default()),
            kept: None,
        }
    }
}

impl GamesDialog {
    pub fn friend_model(&self) -> ModelRc<GamesFriendItem> {
        ModelRc::from(self.friend_model.clone())
    }

    pub fn rows(&self) -> ModelRc<GameRowItem> {
        ModelRc::from(self.rows.clone())
    }

    /// Closed: everything it held goes (a list can be thousands of games).
    pub fn clear(&mut self) {
        self.region = 0;
        self.first_row = 0;
        self.friends = Vec::new();
        self.query = String::new();
        self.kept = None;
        if self.rows.row_count() > 0 {
            self.rows.set_vec(Vec::new());
        }
        if self.friend_model.row_count() > 0 {
            self.friend_model.set_vec(Vec::new());
        }
    }

    /// Brings the models up to date with `g`; returns the rest to show.
    pub fn show(&mut self, state: &AppState, g: &GamesInCommon, images: &mut Images) -> &Contents {
        let picks = filter_picks(candidates(state, g), &self.query);
        let friends: Vec<GamesFriendItem> = picks
            .iter()
            .map(|(_, name, picked)| GamesFriendItem {
                name: name.as_str().into(),
                picked: *picked,
            })
            .collect();
        crate::view::sync(&self.friend_model, friends);
        self.friends = picks.into_iter().map(|(u, _, _)| u).collect();
        let key = (g.people.clone(), g.libraries.len(), self.region);
        let kept = match self.kept.take() {
            Some((k, c)) if k == key => (k, c),
            _ => (key, contents(state, g, self.region)),
        };
        let first = self.first_row.saturating_sub(MARGIN);
        let last = self.first_row + ON_SCREEN + MARGIN;
        let rows: Vec<GameRowItem> = kept
            .1
            .rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                // Only pictures near the view are decoded.
                let image = r
                    .image_url
                    .as_deref()
                    .filter(|_| (first..=last).contains(&i))
                    .and_then(|u| images.get(u, CAPSULE));
                GameRowItem {
                    header: r.header,
                    title: r.title.clone(),
                    has_image: image.is_some(),
                    image: image.unwrap_or_else(crate::view::no_image),
                }
            })
            .collect();
        crate::view::sync(&self.rows, rows);
        &self.kept.insert(kept).1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use godwit_core::application::Person;
    use godwit_core::ids::AccountId;
    use godwit_core::network::Network;
    use std::collections::HashMap;

    fn game(id: u64, name: &str) -> OwnedGame {
        OwnedGame {
            id,
            name: name.into(),
            image_url: None,
        }
    }

    fn setup(people: usize, libraries: &[Library]) -> (AppState, GamesInCommon) {
        let account = AccountId::new(Network::Steam, "me");
        let users: Vec<UserId> = std::iter::once(UserId::new(account.clone(), "me"))
            .chain((1..people).map(|i| UserId::new(account.clone(), format!("f{i}"))))
            .collect();
        let mut state = AppState::default();
        for (i, u) in users.iter().enumerate().skip(1) {
            state.friends.push(Person {
                id: u.clone(),
                name: ["", "Ann", "Bob", "Cat", "Dan"][i].into(),
                avatar_url: None,
                online: true,
            });
        }
        let g = GamesInCommon {
            account,
            people: users.clone(),
            libraries: users
                .into_iter()
                .zip(libraries.iter().cloned())
                .collect::<HashMap<_, _>>(),
        };
        (state, g)
    }

    fn titles(c: &Contents) -> Vec<&str> {
        c.rows.iter().map(|r| r.title.as_str()).collect()
    }

    #[test]
    fn two_people_get_a_venn_with_everyone_listed_first() {
        let lib = |g: &[OwnedGame]| Library::Games(g.to_vec().into());
        let (state, g) = setup(
            2,
            &[lib(&[game(1, "Portal"), game(2, "Dota")]), lib(&[game(1, "Portal")])],
        );
        let c = contents(&state, &g, 0);
        assert_eq!(c.venn, 2);
        assert_eq!(c.regions, [0, 1, 0, 1]);
        assert_eq!(titles(&c), ["Everyone owns (1)", "Portal"]);
        assert_eq!(c.legend, "Left: You · Right: Ann");
        let only_me = contents(&state, &g, 1);
        assert_eq!(titles(&only_me), ["Only You (1)", "Dota"]);
    }

    #[test]
    fn private_and_loading_libraries_are_said_plainly() {
        let (state, g) = setup(3, &[Library::Games(vec![game(1, "x")].into()), Library::Private]);
        let c = contents(&state, &g, 0);
        assert_eq!(c.notes, ["Ann's games are private.", "Loading Bob's games…"]);
        assert_eq!(c.venn, 0);
        assert!(c.rows.is_empty());
    }

    #[test]
    fn four_or_more_rank_everyone_then_all_but_one() {
        let lib = |g: &[OwnedGame]| Library::Games(g.to_vec().into());
        let (state, g) = setup(
            4,
            &[
                lib(&[game(1, "Portal"), game(2, "Dota")]),
                lib(&[game(1, "Portal"), game(2, "Dota")]),
                lib(&[game(1, "Portal")]),
                lib(&[game(1, "Portal"), game(2, "Dota")]),
            ],
        );
        let c = contents(&state, &g, 0);
        assert_eq!(c.venn, 0);
        assert_eq!(
            titles(&c),
            ["Everyone owns (1)", "Portal", "Everyone but Bob (1)", "Dota"]
        );
    }

    #[test]
    fn the_picker_lists_friends_by_name_whether_picked_or_not() {
        let (mut state, mut g) = setup(3, &[]);
        g.people.truncate(2);
        state.friends.push(Person {
            id: UserId::new(g.account.clone(), "f9"),
            name: "aaron".into(),
            avatar_url: None,
            online: false,
        });
        let names: Vec<(String, bool)> = candidates(&state, &g).into_iter().map(|(_, n, p)| (n, p)).collect();
        assert_eq!(
            names,
            [
                ("aaron".to_owned(), false),
                ("Ann".to_owned(), true),
                ("Bob".to_owned(), false)
            ]
        );
    }

    #[test]
    fn the_search_keeps_the_picked_on_top_and_filters_the_rest() {
        let (state, g) = setup(3, &[]);
        let mut picks = candidates(&state, &g);
        picks.push((UserId::new(g.account.clone(), "f9"), "Annabel".to_owned(), false));
        let names =
            |q: &str| -> Vec<String> { filter_picks(picks.clone(), q).into_iter().map(|(_, n, _)| n).collect() };
        assert_eq!(names(""), ["Ann", "Bob", "Annabel"]);
        // Bob is picked: he stays, first, though his name doesn't match.
        let mut bob_picked = picks.clone();
        bob_picked[1].2 = true;
        bob_picked[0].2 = false;
        let found: Vec<String> = filter_picks(bob_picked, " ANN")
            .into_iter()
            .map(|(_, n, _)| n)
            .collect();
        assert_eq!(found, ["Bob", "Ann", "Annabel"]);
        assert_eq!(names("zz").len(), picks.iter().filter(|p| p.2).count());
    }
}
