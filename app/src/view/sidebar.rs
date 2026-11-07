// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Account rail, room lists, directory and quick-switcher rows.

use super::*;

/// What a rail row is.
#[derive(Clone, Debug, PartialEq)]
pub enum RailRow {
    Account(AccountId),
    Server(ServerId),
    /// A kept sign-in still connecting (see `NetworkAccount::connecting`).
    Pending(godwit_core::network::Network),
    /// An account its network signed out: sign in again (its network).
    SignIn(godwit_core::network::Network),
}

/// The accounts in the user's rail order; the rest after, as they signed in.
pub fn ordered_accounts<'a>(state: &'a AppState, order: &[String]) -> Vec<&'a godwit_core::application::Account> {
    let mut accounts: Vec<_> = state.accounts.iter().collect();
    accounts.sort_by_key(|a| {
        order
            .iter()
            .position(|k| *k == crate::settings::account_key(&a.id))
            .unwrap_or(usize::MAX)
    });
    accounts
}

/// The rail: each account's avatar heading its servers (the multi-account
/// layout "A"). An account's own unread badge only shows with several
/// accounts; with one, Home already says it.
pub fn servers(
    state: &AppState,
    images: &mut Images,
    collapsed: &HashSet<AccountId>,
    order: &[String],
    account_order: &[String],
    pending: &[godwit_core::network::Network],
    sign_in: &[crate::settings::SignInNeeded],
) -> (Vec<ServerItem>, Vec<RailRow>) {
    let several = state.accounts.len() > 1;
    let mut rows = Vec::new();
    let mut ids = Vec::new();
    for (n, account) in ordered_accounts(state, account_order).into_iter().enumerate() {
        // Its DMs and groups, and its unmuted servers' channels.
        let mut servers: Vec<&Arc<Server>> = state.servers.iter().filter(|s| s.id.account == account.id).collect();
        // The user's order first; the rest after, as their network keeps them.
        servers.sort_by_key(|s| {
            order
                .iter()
                .position(|k| *k == crate::settings::server_key(&s.id))
                .unwrap_or(usize::MAX)
        });
        let in_server: HashSet<&RoomId> = servers.iter().flat_map(|s| s.rooms()).collect();
        let loud: HashSet<&RoomId> = servers.iter().filter(|s| !s.muted).flat_map(|s| s.rooms()).collect();
        let mine = |r: &&Arc<Room>| r.id.account == account.id && (loud.contains(&r.id) || !in_server.contains(&r.id));
        let (avatar, has_avatar) = picture(images, account.avatar_url.as_deref());
        let mentions: u32 = state.rooms.iter().filter(mine).map(|r| r.mentions).sum();
        let folded = collapsed.contains(&account.id);
        // Its badge when it alone says it: with several accounts, or folded.
        let badge = several || folded;
        rows.push(ServerItem {
            account: true,
            network: account.id.network.short_name().into(),
            collapsed: folded,
            divider_before: n > 0,
            // The tooltip says what a click does (right-click: profile).
            name: format!(
                "{} ({}) · {}",
                account.display_name,
                account.id.network.name(),
                if folded {
                    "click to show its servers"
                } else {
                    "click to fold it"
                }
            )
            .into(),
            initials: initials(&account.display_name).into(),
            icon: avatar,
            has_icon: has_avatar,
            unread: badge && state.rooms.iter().filter(mine).any(|r| r.unread),
            mentions: if badge {
                i32::try_from(mentions).unwrap_or(i32::MAX)
            } else {
                0
            },
            selected: false,
            presence: account.presence.map(presence_colour).unwrap_or_default(),
            pending: false,
            sign_in: false,
        });
        ids.push(RailRow::Account(account.id.clone()));
        if folded {
            continue;
        }
        let mut first = true;
        for s in servers {
            ids.push(RailRow::Server(s.id.clone()));
            let rooms: Vec<&Arc<Room>> = s.rooms().filter_map(|id| state.room(id)).collect();
            let mentions: u32 = rooms.iter().map(|r| r.mentions).sum();
            let (icon, has_icon) = picture(images, s.icon_url.as_deref());
            rows.push(ServerItem {
                account: false,
                network: SharedString::new(),
                divider_before: std::mem::take(&mut first),
                name: s.name.as_ref().into(),
                initials: initials(&s.name).into(),
                icon,
                has_icon,
                unread: !s.muted && rooms.iter().any(|r| r.unread),
                mentions: i32::try_from(mentions).unwrap_or(i32::MAX),
                selected: state.selected_server.as_ref() == Some(&s.id),
                presence: slint::Color::default(),
                collapsed: false,
                pending: false,
                sign_in: false,
            });
        }
    }
    // Accounts that haven't connected yet: there, greyed out, instead of
    // missing until they do.
    for network in pending {
        rows.push(ServerItem {
            account: true,
            network: network.short_name().into(),
            divider_before: !rows.is_empty(),
            name: format!(
                "{}: connecting… (it keeps trying while it can't reach {})",
                network.name(),
                network.name()
            )
            .into(),
            initials: initials(network.name()).into(),
            pending: true,
            ..ServerItem::default()
        });
        ids.push(RailRow::Pending(*network));
    }
    // Accounts their network signed out: asking to sign in again, instead
    // of disappearing.
    for needed in sign_in {
        let Some(network) = godwit_core::network::Network::from_key(&needed.network) else {
            continue;
        };
        let who = if needed.name.is_empty() {
            network.name()
        } else {
            needed.name.as_str()
        };
        rows.push(ServerItem {
            account: true,
            network: network.short_name().into(),
            divider_before: !rows.is_empty(),
            name: format!(
                "{who} ({}): sign-in needed — {}. Click to sign in again.",
                network.name(),
                needed.reason
            )
            .into(),
            initials: initials(who).into(),
            sign_in: true,
            ..ServerItem::default()
        });
        ids.push(RailRow::SignIn(network));
    }
    (rows, ids)
}

pub(super) fn room_row(r: &Room, selected: bool, occupants: Option<&Vec<CallMember>>, images: &mut Images) -> RoomItem {
    let kind = match r.kind {
        RoomKind::Direct { .. } => 0,
        RoomKind::Group => 1,
        RoomKind::Channel { .. } => 2,
        RoomKind::Notes => 3,
    };
    let (icon, has_icon) = picture(images, r.icon_url.as_deref());
    RoomItem {
        thread: r.parent.is_some(),
        header: false,
        name: r.name.as_ref().into(),
        name_runs: label_runs(&r.name, images),
        kind,
        icon,
        has_icon,
        initials: initials(&r.name).into(),
        unread: r.unread,
        mentions: i32::try_from(r.mentions).unwrap_or(i32::MAX),
        selected,
        voice_channel: r.capabilities.voice_user_limit.is_some(),
        voice_since: 0,
        voice_count: r
            .capabilities
            .voice_user_limit
            .map_or_else(String::new, |limit| {
                let count = occupants.map_or(0, Vec::len);
                if limit == 0 {
                    format!("{count}/∞")
                } else {
                    format!("{count}/{limit}")
                }
            })
            .into(),
        presence: r.presence.map(presence_colour).unwrap_or_default(),
        has_presence: r.presence.is_some_and(|p| p != Presence::Unknown),
        subtitle: match (r.presence, &r.status, r.member_count) {
            // A voice channel with a limit: how full it is, as Discord shows it.
            _ if r.capabilities.voice_user_limit.is_some_and(|n| n > 0) => format!(
                "{}/{}",
                occupants.map_or(0, Vec::len),
                r.capabilities.voice_user_limit.unwrap_or(0)
            ),
            (_, Some(status), _) => status.to_string(),
            (Some(p), None, _) => presence_label(p).to_owned(),
            (None, None, Some(n)) => format!("{n} {}", if n == 1 { "member" } else { "members" }),
            _ => String::new(),
        }
        .into(),
        // One shared empty model for rooms without a call, so their rows
        // compare equal between refreshes (see `no_image`).
        occupants: match occupants {
            Some(m) if !m.is_empty() => occupants_model(&r.id.to_string(), call_members(m, images)),
            _ => NO_OCCUPANTS.with(Clone::clone),
        },
        // A thread: how long since it was last active ("3d"), as chats show.
        when: if r.parent.is_some() {
            last_talked(r.last_activity).into()
        } else {
            SharedString::new()
        },
        ..RoomItem::default()
    }
}

/// A room's call occupants as one model per room, reused while they're the
/// same: a new model each refresh made the row differ every time, so the
/// sidebar rebuilt it and its members flickered.
fn occupants_model(room: &str, rows: Vec<CallMemberItem>) -> ModelRc<CallMemberItem> {
    type Kept = std::collections::HashMap<String, (Vec<CallMemberItem>, ModelRc<CallMemberItem>)>;
    thread_local! {
        static KEPT: std::cell::RefCell<Kept> = std::cell::RefCell::default();
    }
    KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        if let Some((old, model)) = kept.get(room)
            && *old == rows
        {
            return model.clone();
        }
        let model = ModelRc::from(Rc::new(VecModel::from(rows.clone())));
        kept.insert(room.to_owned(), (rows, model.clone()));
        model
    })
}

fn header(title: &str) -> RoomItem {
    // As written, not uppercased: the official sidebar shows titles as they are.
    RoomItem {
        header: true,
        name: title.into(),
        ..RoomItem::default()
    }
}

/// The sidebar's title, its rows, the room each row opens, and each
/// heading's category key.
pub type SidebarRows = (String, Vec<RoomItem>, Vec<Option<RoomId>>, Vec<Option<String>>);

/// A category as kept in settings when folded: its server's key and title.
pub fn category_key(server: &ServerId, title: &str) -> String {
    format!("{}#{title}", crate::settings::server_key(server))
}

/// A category's heading, its emoji as pictures (Discord's often have them).
fn category_header(title: &str, images: &mut Images) -> RoomItem {
    RoomItem {
        name_runs: label_runs(title, images),
        ..header(title)
    }
}

/// The sidebar: its title, its rows, and the room each row opens (None
/// for category headings).
/// `query`: Home's search ("" for none): only chats whose name, or any name
/// the person goes by on one of their networks, contains it.
/// `collapsed`: folded categories (settings' keys). Also returns each row's
/// category key (headings only), for folding it.
/// `network`: Home shows only chats on that network (its name; "" for all).
pub fn rooms(state: &AppState, images: &mut Images, query: &str, network: &str, collapsed: &[String]) -> SidebarRows {
    // Non-room pages keep the last room in core state for context (drafts,
    // profiles, returning to Home), but it isn't the currently open row.
    let selected = (!state.showing_friends && !state.showing_people && !state.showing_active && !state.showing_threads)
        .then_some(state.selected.as_ref())
        .flatten();
    let mut rows = Vec::new();
    let mut ids = Vec::new();
    match state.selected_server.as_ref().and_then(|s| state.server(s)) {
        Some(server) => {
            let mut keys = Vec::new();
            for cat in &server.categories {
                let key = category_key(&server.id, &cat.title);
                let folded = !cat.title.is_empty() && collapsed.contains(&key);
                if !cat.title.is_empty() {
                    let mut heading = category_header(&cat.title, images);
                    heading.category_collapsed = folded;
                    rows.push(heading);
                    ids.push(None);
                    keys.push(Some(key));
                }
                for id in &cat.rooms {
                    if let Some(r) = state.room(id) {
                        // Folded, as the official sidebar: the open channel,
                        // unread ones and voice channels with people stay.
                        let occupants = state.voice.get(id);
                        let keep = !folded
                            || selected == Some(id)
                            || r.unread
                            || r.mentions > 0
                            || occupants.is_some_and(|m| !m.is_empty());
                        if keep {
                            let mut row = room_row(r, selected == Some(id), occupants, images);
                            // Its menu offers pinning to Home, or unpinning.
                            row.pinned = state.pinned_rooms.contains(id);
                            // How long its call has been going (Discord's
                            // session start), while anyone's in it.
                            if occupants.is_some_and(|m| !m.is_empty()) {
                                row.voice_since = state
                                    .voice_since
                                    .get(id)
                                    .and_then(|ms| i32::try_from(ms / 1000).ok())
                                    .unwrap_or(0);
                            }
                            rows.push(row);
                            ids.push(Some(id.clone()));
                            keys.push(None);
                        }
                    }
                }
            }
            (server.name.to_string(), rows, ids, keys)
        }
        None => {
            // Home merges every account's chats: with several, each row
            // says whose it is (the multi-account layout). A person merged
            // across networks is one row, opening the chat on the network
            // Godwit talks to them on (or the one open now).
            let several = state.accounts.len() > 1;
            let mut done: Vec<ContactId> = Vec::new();
            // Each chat (a merged person's together): its row, the room it
            // opens, every room in it, and when it was last active.
            let mut entries: Vec<(RoomItem, RoomId, Vec<RoomId>, i64, u8)> = Vec::new();
            for r in state.direct_rooms() {
                let with = match &r.kind {
                    RoomKind::Direct { with } => Some(with),
                    _ => None,
                };
                let group: Vec<&Arc<Room>> = match with.and_then(|w| merged(state, w)) {
                    Some(c) if done.contains(&c.id) => continue,
                    Some(c) => {
                        done.push(c.id);
                        state
                            .direct_rooms()
                            .filter(|o| matches!(&o.kind, RoomKind::Direct { with } if c.identities.contains(with)))
                            .collect()
                    }
                    None => vec![r],
                };
                let best = with.and_then(|w| talk_on_network(state, w));
                let main = group
                    .iter()
                    .find(|g| selected == Some(&g.id))
                    .or_else(|| group.iter().find(|g| Some(g.network()) == best))
                    .copied()
                    .unwrap_or(r);
                let mut row = room_row(main, selected == Some(&main.id), state.voice.get(&main.id), images);
                if group.len() > 1 {
                    row.unread = group.iter().any(|g| g.unread);
                    row.mentions = i32::try_from(group.iter().map(|g| g.mentions).sum::<u32>()).unwrap_or(i32::MAX);
                }
                if let RoomKind::Direct { with } = &main.kind
                    && let Some(name) = person_name(state, with)
                {
                    row.name = name.into();
                    row.initials = initials(&row.name).into();
                }
                let mut labels: Vec<String> = Vec::new();
                for g in &group {
                    let label = match &g.kind {
                        RoomKind::Direct { with } => identity_label(state, with),
                        _ => identity_label(state, &UserId::new(g.id.account.clone(), "")),
                    };
                    if !labels.contains(&label) {
                        labels.push(label);
                    }
                }
                if several && let Some(a) = state.accounts.iter().find(|a| a.id == main.id.account) {
                    let (badge, has_badge) = picture(images, a.avatar_url.as_deref());
                    row.account_badge = badge;
                    row.has_account_badge = has_badge;
                    row.account_initials = initials(&a.display_name).into();
                    row.account_badged = true;
                }
                // A person on several networks: each, with their status there.
                if group.len() > 1 {
                    let users: Vec<&UserId> = group
                        .iter()
                        .filter_map(|g| match &g.kind {
                            RoomKind::Direct { with } => Some(with),
                            _ => None,
                        })
                        .collect();
                    row.networks = ModelRc::new(VecModel::from(compact_nets(state, &users)));
                } else if several {
                    // In words too (the same picture can be on every network).
                    let with = labels.join(" · ");
                    row.subtitle = if row.subtitle.is_empty() {
                        with.into()
                    } else {
                        format!("{with} · {}", row.subtitle).into()
                    };
                }
                let members: Vec<RoomId> = group.iter().map(|g| g.id.clone()).collect();
                let activity = group.iter().map(|g| g.last_activity).max().unwrap_or(0);
                // Their best status on any network (groups and notes count as "here").
                let status = group
                    .iter()
                    .map(|g| g.presence.map_or(1, status_rank))
                    .min()
                    .unwrap_or(1);
                row.when = last_talked(activity).into();
                entries.push((row, main.id.clone(), members, activity, status));
            }
            // Server channels pinned to Home: their own rows, the server's
            // name beneath (only pinned ones; Home stays DMs otherwise).
            for id in &state.pinned_rooms {
                let Some(r) = state.room(id) else { continue };
                let RoomKind::Channel { server } = &r.kind else {
                    continue;
                };
                let mut row = room_row(r, selected == Some(id), state.voice.get(id), images);
                if let Some(s) = state.server(server) {
                    row.subtitle = s.name.to_string().into();
                }
                row.when = last_talked(r.last_activity).into();
                entries.push((row, id.clone(), vec![id.clone()], r.last_activity, 1));
            }
            // Pinned first, in pin order; then newest first; hidden ones in
            // their own section at the bottom, open only when asked.
            let pin = |members: &[RoomId]| {
                members
                    .iter()
                    .filter_map(|m| state.pinned_rooms.iter().position(|p| p == m))
                    .min()
            };
            let hidden = |members: &[RoomId]| members.iter().any(|m| state.hidden_rooms.contains(m));
            if !network.is_empty() {
                entries.retain(|(_, _, members, _, _)| {
                    members
                        .iter()
                        .filter_map(|m| state.room(m))
                        .any(|r| r.network().name() == network)
                });
            }
            let query = query.trim().to_lowercase();
            if !query.is_empty() {
                entries.retain(|(row, _, members, _, _)| {
                    let people: Vec<UserId> = members
                        .iter()
                        .filter_map(|m| match state.room(m).map(|r| &r.kind) {
                            Some(RoomKind::Direct { with }) => Some(with.clone()),
                            _ => None,
                        })
                        .collect();
                    row.name.to_lowercase().contains(&query)
                        || names_of(state, &people)
                            .iter()
                            .any(|n| n.to_lowercase().contains(&query))
                        || members
                            .iter()
                            .filter_map(|m| state.room(m))
                            .any(|r| r.name.to_lowercase().contains(&query))
                });
            }
            for (row, _, members, _, _) in &mut entries {
                row.pinned = pin(members).is_some();
                row.hidden = !row.pinned && hidden(members);
                if members.iter().any(|m| state.marked_unread.contains(m)) {
                    row.unread = true;
                }
            }
            // Then by when you last talked (today, yesterday, this week,
            // this month, before), and within that online first, then
            // away, then offline; newest first among equals.
            let today = Local::now().date_naive();
            let bucket = |ms: i64| {
                let day = Local.timestamp_millis_opt(ms).single().map(|t| t.date_naive());
                match day.map(|d| (today - d).num_days()) {
                    Some(..=0) => 0u8,
                    Some(1) => 1,
                    Some(2..=6) => 2,
                    Some(7..=30) => 3,
                    _ => 4,
                }
            };
            entries.sort_by(|a, b| match (pin(&a.2), pin(&b.2)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => {
                    (bucket(a.3), a.4, std::cmp::Reverse(a.3)).cmp(&(bucket(b.3), b.4, std::cmp::Reverse(b.3)))
                }
            });
            let (shown, tucked): (Vec<_>, Vec<_>) = entries.into_iter().partition(|e| !e.0.hidden);
            let pinned = shown.iter().filter(|e| e.0.pinned).count();
            for (i, (row, main, _, _, _)) in shown.into_iter().enumerate() {
                if i == 0 && pinned > 0 {
                    rows.push(header("Pinned"));
                    ids.push(None);
                }
                if i == pinned && pinned > 0 {
                    rows.push(header("Recent"));
                    ids.push(None);
                }
                rows.push(row);
                ids.push(Some(main));
            }
            if !tucked.is_empty() {
                let mut h = header(&format!(
                    "{} Hidden — {}",
                    if state.show_hidden { "▾" } else { "▸" },
                    tucked.len()
                ));
                h.toggles_hidden = true;
                rows.push(h);
                ids.push(None);
                if state.show_hidden {
                    for (row, main, _, _, _) in tucked {
                        rows.push(row);
                        ids.push(Some(main));
                    }
                }
            }
            {
                let keys = vec![None; rows.len()];
                ("Direct messages".into(), rows, ids, keys)
            }
        }
    }
}

/// The open server's rooms we could join, by name; and which room each is.
pub fn directory(state: &AppState) -> (Vec<DirectoryItem>, Vec<RoomId>) {
    let Some(server) = &state.selected_server else {
        return (Vec::new(), Vec::new());
    };
    let mut list: Vec<&DirectoryRoom> = state
        .directory
        .get(server)
        .into_iter()
        .flatten()
        .filter(|d| state.room(&d.id).is_none())
        .collect();
    list.sort_by_cached_key(|d| d.name.to_lowercase());
    let rows = list
        .iter()
        .map(|d| DirectoryItem {
            name: d.name.as_ref().into(),
            detail: match &d.topic {
                Some(t) if !t.trim().is_empty() => t.lines().next().unwrap_or_default().into(),
                _ => format!("{} {}", d.members, if d.members == 1 { "member" } else { "members" }).into(),
            },
        })
        .collect();
    (rows, list.iter().map(|d| d.id.clone()).collect())
}

/// Quick-switcher results for `query`: rooms whose name contains it, those
/// starting with it first, then unread ones; at most eight (they fit any window).
pub fn switch_results(state: &AppState, query: &str, images: &mut Images) -> (Vec<SwitchItem>, Vec<RoomId>) {
    let needle = query.trim().to_lowercase();
    let mut found: Vec<&Arc<Room>> = state
        .rooms
        .iter()
        .filter(|r| r.name.to_lowercase().contains(&needle))
        .collect();
    found.sort_by_key(|r| (!r.name.to_lowercase().starts_with(&needle), !r.unread));
    found.truncate(8);
    let rows = found
        .iter()
        .map(|r| {
            let detail = match &r.kind {
                RoomKind::Channel { server } => state.server(server).map(|s| s.name.to_string()).unwrap_or_default(),
                RoomKind::Direct { .. } => "Direct message".to_owned(),
                RoomKind::Group => "Group".to_owned(),
                RoomKind::Notes => "Notes".to_owned(),
            };
            let item = room_row(r, false, None, images);
            SwitchItem {
                name: r.name.as_ref().into(),
                detail: detail.into(),
                kind: item.kind,
                initials: item.initials,
                icon: item.icon,
                has_icon: item.has_icon,
            }
        })
        .collect();
    (rows, found.iter().map(|r| r.id.clone()).collect())
}

#[cfg(test)]
mod multi_tests {
    use super::*;
    use godwit_core::application::Action;
    use godwit_core::contacts::Contacts;

    #[test]
    fn a_folded_category_keeps_only_its_open_and_unread_channels() {
        let (mut app, general, _) = crate::demo::world(Contacts::default());
        app.dispatch(Action::SelectRoom(general.clone()));
        assert!(app.state().selected_server.is_some(), "its server opened");
        let mut images = Images::offline();
        let (_, open, _, keys) = rooms(app.state(), &mut images, "", "", &[]);
        let heading = keys.iter().position(Option::is_some).expect("a titled category");
        let key = keys[heading].clone().unwrap_or_default();
        let (_, folded, ids, _) = rooms(app.state(), &mut images, "", "", &[key]);
        assert!(folded[heading].category_collapsed);
        assert!(folded.len() <= open.len());
        // What stays under the heading is unread (the demo's "dev" has mentions).
        for id in ids.iter().skip(heading + 1).flatten() {
            let room = app.state().room(id).expect("a room");
            let busy = app.state().voice.get(id).is_some_and(|m| !m.is_empty());
            assert!(*id == general || room.unread || room.mentions > 0 || busy, "{id:?}");
        }
    }

    /// The demo's second account, as `godwit --demo` and the preview show it.
    #[test]
    fn a_person_on_two_networks_is_one_row_with_both_statuses() {
        let (mut app, _, _) = crate::demo::world(Contacts::default());
        let chat = crate::demo::multi(&mut app);
        app.dispatch(Action::SelectServer(None));
        let mut images = Images::offline();
        let (_, rows, ids, _) = rooms(app.state(), &mut images, "", "", &[]);
        // Person 4: one Home row, opening the Steam chat (their chosen network).
        let at = ids
            .iter()
            .position(|id| id.as_ref() == Some(&chat))
            .expect("the Steam chat");
        assert!(
            !ids.iter()
                .any(|id| id.as_ref() == Some(&RoomId::new(crate::demo::account(), "dm4"))),
            "merged away"
        );
        let dots: Vec<(String, slint::Color)> = rows[at]
            .networks
            .iter()
            .map(|n| (n.label.to_string(), n.colour))
            .collect();
        assert_eq!(dots.len(), 2, "{dots:?}");
        assert!(
            dots.iter()
                .any(|(l, c)| l == "Steam" && *c == presence_colour(Presence::Online))
        );
        assert!(
            dots.iter()
                .any(|(l, c)| l == "Stoat" && *c == presence_colour(Presence::Offline))
        );

        // Friends: the merged person once, the nickname shown.
        let (friends, _) = friends(app.state(), &mut images);
        assert_eq!(friends.iter().filter(|f| f.networks.row_count() == 2).count(), 1);
        assert!(friends.iter().any(|f| f.name == "Raid lead"));

        // People: the near-identical names on two networks are suggested.
        let (_, _, merges) = people(app.state(), &mut images, &[]);
        assert_eq!(merges.len(), 1, "one suggestion");

        // The Steam chat's markup: an emoticon picture and a sticker.
        let t = app.state().timeline(&chat).expect("its history");
        assert!(t.messages.iter().any(|m| m.text.contains(godwit_core::rich::EMOJI)));
        assert!(t.messages.iter().any(|m| m.jumbo.len() == 1));
    }
}

#[cfg(test)]
mod rail_tests {
    use super::*;
    use godwit_core::application::Event;
    use godwit_core::contacts::Contacts;
    use godwit_core::network::Network;

    #[test]
    fn accounts_signed_out_by_their_network_ask_to_sign_in_again() {
        let (app, _, _) = crate::demo::world(Contacts::default());
        let mut images = Images::offline();
        let needed = crate::settings::SignInNeeded {
            account: "slack:T:U".into(),
            network: "slack".into(),
            name: "Xamika".into(),
            reason: "Your session ended".into(),
        };
        let (rows, ids) = servers(app.state(), &mut images, &HashSet::new(), &[], &[], &[], &[needed]);
        let last = rows.last().unwrap();
        assert!(last.account && last.sign_in && !last.pending);
        assert!(last.name.contains("sign-in needed") && last.name.contains("Your session ended"));
        assert_eq!(ids.last(), Some(&RailRow::SignIn(Network::Slack)));
    }

    #[test]
    fn accounts_still_connecting_show_greyed_at_the_end() {
        let (app, _, _) = crate::demo::world(Contacts::default());
        let mut images = Images::offline();
        let (rows, ids) = servers(
            app.state(),
            &mut images,
            &HashSet::new(),
            &[],
            &[],
            &[Network::Slack],
            &[],
        );
        let last = rows.last().unwrap();
        assert!(last.account && last.pending && &*last.network == Network::Slack.short_name());
        assert_eq!(ids.last(), Some(&RailRow::Pending(Network::Slack)));
        assert!(rows[..rows.len() - 1].iter().all(|r| !r.pending));
    }

    #[test]
    fn each_account_heads_its_own_servers_and_badges_show_with_several() {
        let (mut app, _, _) = crate::demo::world(Contacts::default());
        let mut images = Images::offline();
        let none = HashSet::new();
        let (rows, ids) = servers(app.state(), &mut images, &none, &[], &[], &[], &[]);
        assert!(rows[0].account && ids[0] == RailRow::Account(crate::demo::account()));
        assert!(
            rows[1].divider_before && !rows[1].account,
            "a divider under the account"
        );
        assert!(rows.iter().all(|r| r.account || r.mentions >= 0));
        assert_eq!(rows[0].mentions, 0, "one account: Home says it");

        // A second account with one unread DM.
        let second = AccountId::new(Network::Stoat, "second");
        let mut dm = crate::demo::room(
            "other-dm",
            "Someone",
            godwit_core::room::RoomKind::Direct {
                with: UserId::new(second.clone(), "u"),
            },
            true,
            2,
        );
        dm.id = RoomId::new(second.clone(), "other-dm");
        app.apply(Event::AccountReady {
            account: second.clone(),
            display_name: "Second".into(),
            username: "second".into(),
            avatar_url: None,
            rooms: vec![dm],
            servers: Vec::new(),
        });
        let (rows, ids) = servers(app.state(), &mut images, &none, &[], &[], &[], &[]);
        let at = ids.iter().position(|r| r == &RailRow::Account(second.clone())).unwrap();
        assert!(rows[at].divider_before, "sections are divided");
        assert!(rows[at].unread && rows[at].mentions == 2, "its own badge");

        app.dispatch(godwit_core::application::Action::SelectServer(None));
        // Collapsed: its servers hide, its own badge shows.
        let folded: HashSet<AccountId> = [crate::demo::account()].into();
        let (rows, ids) = servers(app.state(), &mut images, &folded, &[], &[], &[], &[]);
        assert!(rows[0].collapsed);
        assert!(
            !ids.iter()
                .any(|r| matches!(r, RailRow::Server(s) if s.account == crate::demo::account()))
        );

        let steam: Vec<(String, i32)> = presence_choices(Network::Steam)
            .iter()
            .map(|c| (c.label.to_string(), c.value))
            .collect();
        assert_eq!(
            steam,
            [("Online".into(), 0), ("Away".into(), 1), ("Invisible".into(), 4)]
        );
        assert_eq!(presence_choices(Network::Stoat).len(), 5);

        let (_, home, _, _) = rooms(app.state(), &mut images, "", "", &[]);
        assert!(!home.is_empty());
        assert!(
            home.iter().filter(|r| !r.header).all(|r| r.account_badged),
            "Home says whose chat"
        );
        // Searching keeps only the chats it names, whatever the case.
        let name = home
            .iter()
            .find(|r| !r.header)
            .map(|r| r.name.to_string())
            .unwrap_or_default();
        let (_, found, ids, _) = rooms(app.state(), &mut images, &name.to_uppercase(), "", &[]);
        assert!(
            found
                .iter()
                .filter(|r| !r.header)
                .all(|r| r.name.to_lowercase().contains(&name.to_lowercase()))
        );
        assert!(ids.iter().flatten().count() >= 1);
        let (_, none, _, _) = rooms(app.state(), &mut images, "zzqqxx-nobody", "", &[]);
        assert!(none.iter().all(|r| r.header), "nothing matches");
        // The network filter keeps only that network's chats.
        let network = app.state().direct_rooms().next().map(|r| r.network()).unwrap();
        let (_, one, ids, _) = rooms(app.state(), &mut images, "", network.name(), &[]);
        assert!(one.iter().any(|r| !r.header), "that network's chats stay");
        assert!(
            ids.iter()
                .flatten()
                .all(|id| app.state().room(id).is_some_and(|r| r.network() == network))
        );
    }
}
