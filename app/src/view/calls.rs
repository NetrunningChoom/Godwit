// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Call participants, connection labels and Active now rows.

use super::*;

pub fn call_members(members: &[CallMember], images: &mut Images) -> Vec<CallMemberItem> {
    members
        .iter()
        .map(|m| {
            let (avatar, has_avatar) = picture(images, m.avatar_url.as_deref());
            CallMemberItem {
                name: m.name.as_ref().into(),
                name_runs: label_runs(&m.name, images),
                initials: initials(&m.name).into(),
                avatar,
                has_avatar,
                speaking: m.speaking,
                muted: m.muted,
                deafened: m.deafened,
                volume: 1.0,
                ..CallMemberItem::default()
            }
        })
        .collect()
}

/// The call bar's first line.
pub fn call_status(status: CallStatus) -> &'static str {
    match status {
        CallStatus::Connecting => "Connecting…",
        CallStatus::Connected => "Voice connected",
    }
}
/// What an Active now row opens: the call's room, or a chat with them.
#[derive(Clone, Debug, PartialEq)]
pub enum ActiveTarget {
    Call(RoomId),
    Person(UserId),
}

/// Active now (Discord's column of that name, across every network): calls
/// with friends in them, then friends doing something ("Playing …"). Each
/// person shows once, merged contacts by our nickname for them.
pub fn active(state: &AppState, images: &mut Images) -> (Vec<crate::ui::ActiveItem>, Vec<ActiveTarget>) {
    let friends: HashSet<&UserId> = state.friends.iter().map(|p| &p.id).collect();
    let me = |u: &UserId| u.raw == u.account.user;
    let name_of = |u: &UserId, fallback: &str| {
        state
            .contacts
            .of(u)
            .and_then(|c| c.nickname.clone())
            .unwrap_or_else(|| fallback.to_owned())
    };
    let mut rows = Vec::new();
    let mut targets = Vec::new();

    // Calls: the busiest first, then by name.
    let mut calls: Vec<(&Arc<Room>, Vec<&godwit_core::call::CallMember>, usize)> = state
        .voice
        .iter()
        .filter_map(|(room, members)| {
            let room = state.room(room)?;
            let theirs: Vec<_> = members.iter().filter(|m| friends.contains(&m.user)).collect();
            (!theirs.is_empty()).then_some((room, theirs, members.len()))
        })
        .collect();
    calls.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.name.cmp(&b.0.name)));
    for (room, theirs, all) in calls {
        let names: Vec<String> = theirs.iter().map(|m| name_of(&m.user, &m.name)).collect();
        let here = state.call.as_ref().is_some_and(|c| c.room == room.id);
        // Everyone else in it, not counting us.
        let others = all.saturating_sub(theirs.len()).saturating_sub(usize::from(here));
        let who = match (names.split_last(), others) {
            (Some((last, [])), 0) => last.clone(),
            (Some((last, rest)), 0) => format!("{} and {last}", rest.join(", ")),
            (_, n) => format!("{} and {n} more", names.join(", ")),
        };
        let place = match &room.kind {
            RoomKind::Channel { server } => state.server(server).map(|s| s.name.to_string()),
            _ => None,
        };
        let network = room.id.account.network.name();
        let (avatar, has_avatar) = picture(images, theirs.first().and_then(|m| m.avatar_url.as_deref()));
        let title = match &room.kind {
            RoomKind::Channel { .. } => format!("🔊 {}", room.name),
            _ => room.name.to_string(),
        };
        rows.push(crate::ui::ActiveItem {
            kind: 0,
            title_runs: label_runs(&title, images),
            title: title.into(),
            detail: place
                .map_or_else(|| network.to_owned(), |p| format!("{p} · {network}"))
                .into(),
            who: who.into(),
            initials: initials(names.first().map_or("", String::as_str)).into(),
            avatar,
            has_avatar,
            can_join: !here && room.capabilities.voice,
            here,
        });
        targets.push(ActiveTarget::Call(room.id.clone()));
    }

    // Friends doing something, by name; a merged person once.
    let mut doing: Vec<(String, &UserId, &Arc<str>)> = Vec::new();
    let mut seen = HashSet::new();
    for (user, what) in &state.activities {
        if me(user) || !friends.contains(user) {
            continue;
        }
        if let Some(c) = state.contacts.of(user)
            && !seen.insert(c.id)
        {
            continue;
        }
        let fallback = state
            .friends
            .iter()
            .find(|p| &p.id == user)
            .map_or("", |p| p.name.as_ref());
        doing.push((name_of(user, fallback), user, what));
    }
    doing.sort_by_key(|(name, ..)| name.to_lowercase());
    for (name, user, what) in doing {
        let avatar_url = state
            .friends
            .iter()
            .find(|p| &p.id == user)
            .and_then(|p| p.avatar_url.as_deref());
        let (avatar, has_avatar) = picture(images, avatar_url);
        rows.push(crate::ui::ActiveItem {
            kind: 1,
            title_runs: label_runs(&name, images),
            title: name.as_str().into(),
            detail: user.network().name().into(),
            who: what.as_ref().into(),
            initials: initials(&name).into(),
            avatar,
            has_avatar,
            can_join: false,
            here: false,
        });
        targets.push(ActiveTarget::Person(user.clone()));
    }
    (rows, targets)
}
