// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Call participants, connection labels and Active now rows.

use godwit_core::call::{CallStage, StageRequest, StageRole};

use super::*;

/// What a stage offers for someone: the button's label and the request it
/// makes. A person on the stage can step down or be moved to the audience;
/// the audience asks to speak (or lowers a raised hand); moderators invite.
pub fn stage_action(role: StageRole, me: bool, stage: &CallStage) -> Option<(&'static str, StageRequest)> {
    match (role, me) {
        (StageRole::Speaker, true) => Some(("Move to audience", StageRequest::MoveToAudience)),
        (StageRole::HandRaised, true) => Some(("Lower hand", StageRequest::LowerHand)),
        (StageRole::Audience, true) if stage.moderate => Some(("Become a speaker", StageRequest::InviteToSpeak)),
        (StageRole::Audience, true) if stage.may_request => Some(("Request to speak", StageRequest::RequestToSpeak)),
        (StageRole::Audience, true) => None,
        (StageRole::Speaker, false) if stage.moderate => Some(("Move to audience", StageRequest::MoveToAudience)),
        (StageRole::Audience | StageRole::HandRaised, false) if stage.moderate => {
            Some(("Invite to speak", StageRequest::InviteToSpeak))
        }
        (_, false) => None,
    }
}

#[cfg(test)]
mod stage_tests {
    use super::*;

    fn stage(moderate: bool, may_request: bool) -> CallStage {
        CallStage {
            topic: "Topic".into(),
            moderate,
            may_request,
        }
    }

    #[test]
    fn the_audience_asks_and_moderators_invite() {
        let guest = stage(false, true);
        assert_eq!(
            stage_action(StageRole::Audience, true, &guest),
            Some(("Request to speak", StageRequest::RequestToSpeak))
        );
        assert_eq!(
            stage_action(StageRole::HandRaised, true, &guest),
            Some(("Lower hand", StageRequest::LowerHand))
        );
        assert_eq!(stage_action(StageRole::HandRaised, false, &guest), None);
        let moderator = stage(true, true);
        assert_eq!(
            stage_action(StageRole::HandRaised, false, &moderator),
            Some(("Invite to speak", StageRequest::InviteToSpeak))
        );
        assert_eq!(
            stage_action(StageRole::Speaker, false, &moderator),
            Some(("Move to audience", StageRequest::MoveToAudience))
        );
        assert_eq!(
            stage_action(StageRole::Speaker, true, &guest),
            Some(("Move to audience", StageRequest::MoveToAudience))
        );
    }
}

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
                stage_label: match m.stage {
                    Some(StageRole::Speaker) => "Speaker",
                    Some(StageRole::Audience) => "Audience",
                    Some(StageRole::HandRaised) => "Hand raised",
                    None => "",
                }
                .into(),
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

/// How many columns the call's video grid gets: every tile is the same
/// 16:9 size, so the count that makes them biggest in `width` x `height`
/// (with `gap` between tiles) wins; ties go to fewer columns.
pub fn video_columns(count: i32, width: f32, height: f32, gap: f32) -> i32 {
    let count = count.max(1);
    let mut best = (1, 0.0f32);
    for cols in 1..=count {
        let rows = (count + cols - 1) / cols;
        let cell_w = (width - gap * (cols - 1) as f32) / cols as f32;
        let cell_h = (height - gap * (rows - 1) as f32) / rows as f32;
        let tile_w = cell_w.min(cell_h * 16.0 / 9.0);
        if tile_w > best.1 + 0.5 {
            best = (cols, tile_w);
        }
    }
    best.0
}

#[cfg(test)]
mod grid_tests {
    use super::video_columns;

    #[test]
    fn a_wide_strip_puts_the_tiles_side_by_side() {
        assert_eq!(video_columns(2, 1200.0, 140.0, 4.0), 2);
        assert_eq!(video_columns(4, 1600.0, 140.0, 4.0), 4);
    }

    #[test]
    fn a_tall_stage_stacks_them() {
        assert_eq!(video_columns(2, 400.0, 900.0, 4.0), 1);
        // Four in a roughly 16:9 stage: two by two.
        assert_eq!(video_columns(4, 1280.0, 720.0, 4.0), 2);
        assert_eq!(video_columns(9, 1280.0, 720.0, 4.0), 3);
    }

    #[test]
    fn nothing_or_no_room_still_gives_one_column() {
        assert_eq!(video_columns(0, 800.0, 400.0, 4.0), 1);
        assert_eq!(video_columns(3, 0.0, 0.0, 4.0), 1);
    }
}
