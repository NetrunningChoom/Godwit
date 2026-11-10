// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Friend groups: the named sets a network lets people sort their friends
//! into (Steam's friend tags), shown as sections of the people list. A
//! friend can be in several groups.

use crate::ids::UserId;

/// One group on one account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendGroup {
    /// The network's own id for the group.
    pub id: String,
    pub name: String,
    pub members: Vec<UserId>,
}

/// One line of the arranged people list: which row, and the heading that
/// starts a section when this is its first row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub row: usize,
    pub heading: Option<String>,
}

/// Groups sorted for display: by name, then id.
pub fn ordered(mut groups: Vec<&FriendGroup>) -> Vec<&FriendGroup> {
    groups.sort_by_cached_key(|g| (g.name.to_lowercase(), g.id.clone()));
    groups
}

/// Arranges the people rows (already filtered and sorted) into sections:
/// "Favourites" (every starred row), then each group with its members
/// (a row can appear under several), then "Everyone else" (unstarred rows
/// in no group). `in_groups[row]` lists the indexes into `group_names` the
/// row belongs to. Rows keep their order within a section. Returns `None`
/// when no row is in a group, so the plain layout applies.
pub fn arrange(starred: &[bool], in_groups: &[Vec<usize>], group_names: &[String]) -> Option<Vec<Placed>> {
    if !in_groups.iter().any(|g| !g.is_empty()) {
        return None;
    }
    let mut out = Vec::new();
    let section = |rows: Vec<usize>, heading: &str, out: &mut Vec<Placed>| {
        for (i, row) in rows.into_iter().enumerate() {
            out.push(Placed {
                row,
                heading: (i == 0).then(|| heading.to_owned()),
            });
        }
    };
    section(
        (0..starred.len()).filter(|r| starred[*r]).collect(),
        "Favourites",
        &mut out,
    );
    for (g, name) in group_names.iter().enumerate() {
        section(
            (0..in_groups.len()).filter(|r| in_groups[*r].contains(&g)).collect(),
            name,
            &mut out,
        );
    }
    section(
        (0..starred.len())
            .filter(|r| !starred[*r] && in_groups.get(*r).is_none_or(|g| g.is_empty()))
            .collect(),
        "Everyone else",
        &mut out,
    );
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        vec!["Family".into(), "Raid".into()]
    }

    #[test]
    fn no_memberships_keeps_the_plain_layout() {
        assert_eq!(arrange(&[false, true], &[vec![], vec![]], &names()), None);
    }

    #[test]
    fn rows_land_in_every_group_they_belong_to_and_the_rest_stay_apart() {
        // Rows: 0 in Family+Raid, 1 starred and ungrouped, 2 in Raid, 3 in none.
        let placed = arrange(
            &[false, true, false, false],
            &[vec![0, 1], vec![], vec![1], vec![]],
            &names(),
        )
        .unwrap();
        let flat: Vec<(usize, Option<&str>)> = placed.iter().map(|p| (p.row, p.heading.as_deref())).collect();
        assert_eq!(
            flat,
            vec![
                (1, Some("Favourites")),
                (0, Some("Family")),
                (0, Some("Raid")),
                (2, None),
                (3, Some("Everyone else")),
            ]
        );
    }

    #[test]
    fn empty_groups_get_no_heading() {
        let placed = arrange(&[false, false], &[vec![1], vec![]], &names()).unwrap();
        assert_eq!(placed[0].heading.as_deref(), Some("Raid"));
        assert_eq!(placed.len(), 2);
    }

    #[test]
    fn groups_sort_by_name() {
        let a = FriendGroup {
            id: "2".into(),
            name: "beta".into(),
            members: vec![],
        };
        let b = FriendGroup {
            id: "1".into(),
            name: "Alpha".into(),
            members: vec![],
        };
        let o = ordered(vec![&a, &b]);
        assert_eq!(o[0].id, "1");
    }
}
