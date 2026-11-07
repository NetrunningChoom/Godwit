// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Contacts, friends, members, presence and profile-card rows.

use super::*;

/// The statuses an account can set, as its network names them (values as
/// the profile popup's: 0 online, 1 idle, 2 focus, 3 busy, 4 invisible).
pub fn presence_choices(network: godwit_core::network::Network) -> Vec<crate::ui::PresenceChoice> {
    let choice = |label: &str, value: Presence, index: i32| crate::ui::PresenceChoice {
        label: label.into(),
        colour: presence_colour(value),
        value: index,
    };
    network
        .presence_choices()
        .iter()
        .map(|&(label, value)| {
            // The UI's presence index (present.rs reads it back).
            let index = match value {
                Presence::Idle => 1,
                Presence::Focus => 2,
                Presence::Busy => 3,
                Presence::Invisible => 4,
                _ => 0,
            };
            choice(label, value, index)
        })
        .collect()
}

/// `user`'s contact, if it holds more than one signed-in identity (someone
/// merged across networks or accounts).
pub fn merged<'a>(state: &'a AppState, user: &UserId) -> Option<&'a Metacontact> {
    let c = state.contacts.of(user)?;
    let signed_in = c
        .identities
        .iter()
        .filter(|u| state.accounts.iter().any(|a| a.id == u.account))
        .count();
    (signed_in > 1).then_some(c)
}

/// The network Godwit talks to `user`'s person on: their chosen one, else
/// the most private, among identities signed in now.
pub fn talk_on_network(state: &AppState, user: &UserId) -> Option<godwit_core::network::Network> {
    let c = merged(state, user)?;
    let mut only = c.clone();
    only.identities
        .retain(|u| state.accounts.iter().any(|a| a.id == u.account));
    only.best_network()
}

/// A name a network gave someone: as a friend, else their direct chat's.
/// Every name someone goes by across their accounts, for search.
pub fn names_of(state: &AppState, users: &[UserId]) -> Vec<String> {
    users.iter().filter_map(|u| identity_name(state, u)).collect()
}

fn identity_name(state: &AppState, user: &UserId) -> Option<String> {
    state
        .friends
        .iter()
        .find(|p| p.id == *user)
        .map(|p| p.name.to_string())
        .or_else(|| {
            state
                .rooms
                .iter()
                .find(|r| matches!(&r.kind, RoomKind::Direct { with } if with == user))
                .map(|r| r.name.to_string())
        })
}

/// What we call `user`'s person: our own nickname for them; for someone
/// merged across networks, their name on the network we talk to them on;
/// else None (the network's name for them stands).
pub fn person_name(state: &AppState, user: &UserId) -> Option<String> {
    let c = state.contacts.of(user)?;
    if let Some(n) = &c.nickname {
        return Some(n.clone());
    }
    let best = talk_on_network(state, user)?;
    let main = c
        .identities
        .iter()
        .filter(|u| state.accounts.iter().any(|a| a.id == u.account))
        .find(|u| u.network() == best)?;
    identity_name(state, main)
}

/// `user`'s status on their network, as a dot's colour: their direct chat's
/// presence, else as a friend online or not; transparent when unknown.
pub fn identity_presence(state: &AppState, user: &UserId) -> slint::Color {
    let room = state
        .rooms
        .iter()
        .find(|r| matches!(&r.kind, RoomKind::Direct { with } if with == user))
        .and_then(|r| r.presence);
    match room {
        Some(p) => presence_colour(p),
        None => match state.friends.iter().find(|p| p.id == *user) {
            Some(p) => presence_colour(if p.online { Presence::Online } else { Presence::Offline }),
            None => presence_colour(Presence::Unknown),
        },
    }
}

/// `user` as a network with their status there.
/// How present someone is, for ordering Home: online, then away (idle,
/// busy, focus), then offline or unknown.
pub(super) fn status_rank(p: Presence) -> u8 {
    match p {
        Presence::Online => 0,
        Presence::Idle | Presence::Busy | Presence::Focus => 1,
        _ => 2,
    }
}

/// A narrow row's networks: each once, an alt counted ("Discord ×2"), so
/// four accounts still fit; the chat's own tabs name which is which. The
/// dot is the first one with a known status (online ones first).
pub fn compact_nets(state: &AppState, users: &[&UserId]) -> Vec<crate::ui::NetPresence> {
    let mut out: Vec<(Network, usize, slint::Color)> = Vec::new();
    for u in users {
        let colour = identity_presence(state, u);
        match out.iter_mut().find(|(n, _, _)| *n == u.network()) {
            Some((_, count, c)) => {
                *count += 1;
                if *c == slint::Color::default() || colour == presence_colour(Presence::Online) {
                    *c = colour;
                }
            }
            None => out.push((u.network(), 1, colour)),
        }
    }
    out.into_iter()
        .map(|(n, count, colour)| crate::ui::NetPresence {
            label: if count > 1 {
                format!("{} ×{count}", n.name())
            } else {
                n.name().to_owned()
            }
            .into(),
            colour,
        })
        .collect()
}

pub fn net_presence(state: &AppState, user: &UserId) -> crate::ui::NetPresence {
    crate::ui::NetPresence {
        label: identity_label(state, user).into(),
        colour: identity_presence(state, user),
    }
}

/// Which of our accounts someone is reached through: the network, and with
/// two or more of ours on it, which one ("Steam (Name)").
pub fn identity_label(state: &AppState, user: &UserId) -> String {
    let network = user.network();
    let mut label = network.name().to_owned();
    // Someone with two accounts on one network (an alt): which one this is.
    let twins = state.contacts.of(user).is_some_and(|c| {
        c.identities
            .iter()
            .filter(|u| u.network() == network && state.accounts.iter().any(|a| a.id == u.account))
            .count()
            > 1
    });
    if twins && let Some(name) = identity_name(state, user) {
        label = format!("{label} · {name}");
    }
    // And with several of our own accounts there, which one reaches them.
    let ours = state.accounts.iter().filter(|a| a.id.network == network).count();
    if ours > 1
        && let Some(a) = state.accounts.iter().find(|a| a.id == user.account)
    {
        label = format!("{label} (via {})", a.display_name);
    }
    label
}

/// The DM account picker names both the platform identity and the person on it.
pub fn dm_account_label(state: &AppState, user: &UserId) -> String {
    let name = identity_name(state, user).unwrap_or_else(|| "Unknown person".to_owned());
    let mut label = format!("{}: {name}", user.network().name());
    let own_accounts = state.accounts.iter().filter(|a| a.id.network == user.network());
    if own_accounts.count() > 1
        && let Some(account) = state.accounts.iter().find(|a| a.id == user.account)
    {
        label.push_str(&format!(" (via {})", account.display_name));
    }
    label
}

/// The friends list: one row per person (identities merged under People
/// are one row), online first, each group by name, saying which networks
/// they're on; and who each row opens: the identity Godwit would talk to
/// them on (the chosen network, else the most private).
pub fn friends(state: &AppState, images: &mut Images) -> (Vec<PersonItem>, Vec<UserId>) {
    // Each person's friends, keyed by contact (or alone without one).
    let mut groups: Vec<(Option<&Metacontact>, Vec<&Person>)> = Vec::new();
    for p in &state.friends {
        let contact = state.contacts.of(&p.id);
        match contact.and_then(|c| groups.iter_mut().find(|(g, _)| g.is_some_and(|g| g.id == c.id))) {
            Some((_, members)) => members.push(p),
            None => groups.push((contact, vec![p])),
        }
    }
    let mut rows: Vec<(bool, String, PersonItem, UserId)> = Vec::with_capacity(groups.len());
    for (contact, members) in groups {
        let best = contact.and_then(|c| {
            let ours: Vec<&UserId> = members.iter().map(|p| &p.id).collect();
            let mut only = c.clone();
            only.identities.retain(|u| ours.contains(&u));
            only.best_network()
        });
        let Some(main) = members
            .iter()
            .find(|p| Some(p.id.network()) == best)
            .or(members.first())
            .copied()
        else {
            continue;
        };
        // Our nickname for them, else the name on the network chosen.
        let name = contact
            .and_then(|c| c.nickname.clone())
            .unwrap_or_else(|| main.name.to_string());
        let online = members.iter().any(|p| p.online);
        let ids: Vec<&UserId> = members.iter().map(|p| &p.id).collect();
        let networks = compact_nets(state, &ids);
        let avatar_url = main
            .avatar_url
            .as_deref()
            .or_else(|| members.iter().find_map(|p| p.avatar_url.as_deref()));
        let (avatar, has_avatar) = picture(images, avatar_url);
        rows.push((
            online,
            name.to_lowercase(),
            PersonItem {
                name: name.as_str().into(),
                name_runs: label_runs(&name, images),
                initials: initials(&name).into(),
                avatar,
                has_avatar,
                online,
                networks: ModelRc::new(VecModel::from(networks)),
            },
            main.id.clone(),
        ));
    }
    rows.sort_by(|a, b| (!a.0, &a.1).cmp(&(!b.0, &b.1)));
    let ids = rows.iter().map(|r| r.3.clone()).collect();
    (rows.into_iter().map(|r| r.2).collect(), ids)
}

/// Pending friend requests, theirs first, each by name; and who each row is.
pub fn requests(state: &AppState, images: &mut Images) -> (Vec<RequestItem>, Vec<UserId>) {
    // Only signed-in accounts' requests (the core drops late ones too).
    let mut pending: Vec<&(Person, bool)> = state
        .friend_requests
        .iter()
        .filter(|(p, _)| state.accounts.iter().any(|a| a.id == p.id.account))
        .collect();
    pending.sort_by_cached_key(|(p, incoming)| (!incoming, p.name.to_lowercase()));
    let ids = pending.iter().map(|(p, _)| p.id.clone()).collect();
    let rows = pending
        .into_iter()
        .map(|(p, incoming)| {
            let (avatar, has_avatar) = picture(images, p.avatar_url.as_deref());
            RequestItem {
                name: p.name.as_ref().into(),
                initials: initials(&p.name).into(),
                avatar,
                has_avatar,
                incoming: *incoming,
                network: identity_label(state, &p.id).into(),
            }
        })
        .collect();
    (rows, ids)
}

/// People: every contact with their accounts, by name; and each row's
/// contact with its accounts in chip order.
/// Each People row's contact and its accounts, in chip order.
pub type PeopleRows = Vec<(ContactId, Vec<UserId>)>;

pub fn people(
    state: &AppState,
    images: &mut Images,
    favourites: &[String],
) -> (Vec<ContactItem>, PeopleRows, Vec<MergeItem>) {
    // Only identities on accounts signed in now: a signed-out account's
    // contacts (and merges) wait, hidden, until it signs in again.
    let signed_in = |u: &UserId| state.accounts.iter().any(|a| a.id == u.account);
    let mut contacts: Vec<Metacontact> = state
        .contacts
        .iter()
        .filter(|c| c.identities.iter().any(signed_in))
        .map(|c| {
            let mut c = c.clone();
            c.identities.retain(signed_in);
            c
        })
        .collect();
    contacts.sort_by_cached_key(|c| c.name.to_lowercase());
    let mut rows = Vec::with_capacity(contacts.len());
    let mut who = Vec::with_capacity(contacts.len());
    for c in &contacts {
        let best = c.best_network();
        // A picture from any of their accounts we know: a friend's, else
        // the direct chat's.
        let avatar_url = c.identities.iter().find_map(|u| {
            state
                .friends
                .iter()
                .find(|p| p.id == *u)
                .and_then(|p| p.avatar_url.as_deref())
                .or_else(|| {
                    state
                        .rooms
                        .iter()
                        .find(|r| matches!(&r.kind, RoomKind::Direct { with } if with == u))
                        .and_then(|r| r.icon_url.as_deref())
                })
        });
        let (avatar, has_avatar) = picture(images, avatar_url);
        let identities: Vec<IdentityItem> = c
            .identities
            .iter()
            .map(|u| IdentityItem {
                network: identity_label(state, u).into(),
                chosen: Some(u.network()) == best && c.identities.len() > 1,
            })
            .collect();
        // Our nickname, else the name on the network we talk to them on.
        let shown = c
            .identities
            .first()
            .and_then(|u| person_name(state, u))
            .or_else(|| c.identities.first().and_then(|u| identity_name(state, u)))
            .unwrap_or_else(|| c.name.clone());
        rows.push(ContactItem {
            name: shown.as_str().into(),
            name_runs: label_runs(&shown, images),
            initials: initials(&shown).into(),
            avatar,
            has_avatar,
            // Three to a line: a person with a dozen accounts still fits.
            identity_rows: ModelRc::from(Rc::new(VecModel::from(
                identities
                    .chunks(3)
                    .enumerate()
                    .map(|(i, chunk)| crate::ui::IdentityRow {
                        items: ModelRc::from(Rc::new(VecModel::from(chunk.to_vec()))),
                        first: i32::try_from(i * 3).unwrap_or(0),
                    })
                    .collect::<Vec<_>>(),
            ))),
            identities: ModelRc::from(Rc::new(VecModel::from(identities))),
            talk_on: best
                .map(|n| talk_on(n, c.preferred == Some(n)))
                .unwrap_or_default()
                .into(),
            no_nudges: c.no_nudges,
            favourite: c.identities.iter().any(|u| favourites.contains(&u.to_string())),
            nickname: c.nickname.clone().unwrap_or_default().into(),
        });
        who.push((c.id, c.identities.clone()));
    }
    // Who might be one person (Godwit never merges on its own), by row.
    let row_of = |id: ContactId| {
        who.iter()
            .position(|(c, _)| *c == id)
            .and_then(|r| i32::try_from(r).ok())
    };
    let where_of = |c: &Metacontact| {
        let mut labels: Vec<String> = Vec::new();
        for u in &c.identities {
            let l = identity_label(state, u);
            if !labels.contains(&l) {
                labels.push(l);
            }
        }
        labels.join(", ")
    };
    let merges = godwit_core::contacts::suggest_merges(&contacts)
        .into_iter()
        .filter_map(|(x, y)| {
            let (cx, cy) = (
                contacts.iter().find(|c| c.id == x)?,
                contacts.iter().find(|c| c.id == y)?,
            );
            Some(MergeItem {
                a: row_of(x)?,
                b: row_of(y)?,
                a_name: cx.name.as_str().into(),
                b_name: cy.name.as_str().into(),
                a_where: where_of(cx).into(),
                b_where: where_of(cy).into(),
            })
        })
        .collect();
    (rows, who, merges)
}

/// The profile card's "Also on …" line: the person's other networks.
pub fn linked(state: &AppState, user: &UserId) -> String {
    let Some(c) = state.contacts.of(user) else {
        return String::new();
    };
    let mut others: Vec<&str> = Vec::new();
    for u in c.identities.iter().filter(|u| *u != user) {
        let name = u.network().name();
        if !others.contains(&name) {
            others.push(name);
        }
    }
    if others.is_empty() {
        String::new()
    } else {
        format!("Also on {}", others.join(", "))
    }
}

/// Where Godwit talks to someone, and why, in one calm line.
fn talk_on(n: Network, chosen: bool) -> String {
    let why = if chosen {
        "your choice"
    } else {
        "the most private network you share"
    };
    let what = match n.security().messages {
        Encryption::Always => "always end-to-end encrypted",
        Encryption::ByDefault => "end-to-end encrypted by default",
        Encryption::None => "not end-to-end encrypted",
    };
    format!("Talk on {} ({why}): {what}", n.name())
}

/// The member list as rows: each group's heading, then its members.
pub fn members(groups: &[MemberGroup], images: &mut Images) -> (Vec<MemberItem>, Vec<Option<UserId>>) {
    let mut rows = Vec::new();
    let mut who = Vec::new();
    for g in groups {
        rows.push(MemberItem {
            header: true,
            name: g.title.as_ref().into(),
            ..MemberItem::default()
        });
        who.push(None);
        for m in &g.members {
            // A row the network hasn't sent yet (a streamed list's range not
            // loaded): blank, and nobody to open.
            if m.user.raw.is_empty() {
                rows.push(MemberItem {
                    placeholder: true,
                    ..MemberItem::default()
                });
                who.push(None);
                continue;
            }
            let (avatar, has_avatar) = picture(images, m.avatar_url.as_deref());
            let [r, gr, b] = m.colour.unwrap_or_default();
            rows.push(MemberItem {
                header: false,
                placeholder: false,
                name: m.name.as_ref().into(),
                name_runs: label_runs(&m.name, images),
                initials: initials(&m.name).into(),
                avatar,
                has_avatar,
                colour: slint::Color::from_rgb_u8(r, gr, b),
                has_colour: m.colour.is_some(),
                presence: presence_colour(m.presence),
                status: m.status.as_deref().unwrap_or_default().into(),
            });
            who.push(Some(m.user.clone()));
        }
    }
    (rows, who)
}

/// The profile card's row in `Select`: its text selects as message text does.
pub const CARD_ROW: i32 = -2;

/// The card's inner width (its 340 px less 15 px padding a side), and the
/// username's (beside its copy button).
const CARD_TEXT_WIDTH: f32 = 310.0;
const CARD_USERNAME_WIDTH: f32 = 274.0;

/// The profile card's text, laid out as message text is (the one code path:
/// emoji pictures from the pack, custom emoji, links): name, username,
/// status and bio, as blocks 0–3 (empty ones lay out empty).
pub fn card_text(p: &Profile, images: &mut Images) -> (Vec<BlockItem>, Vec<crate::textlayout::Laid>) {
    use crate::textlayout::{Base, Paragraph, lay_out};
    let emoji_url = |e: &str| crate::emoji::canonical(e).map(crate::emoji::url_of);
    let base = |size: f32, bold: bool| Base {
        size,
        bold,
        mono: false,
    };
    let status = p.status.as_deref().unwrap_or_default();
    let laid = vec![
        lay_out(Paragraph::plain(&p.name, &emoji_url), base(18.0, true), CARD_TEXT_WIDTH),
        lay_out(
            Paragraph::plain(&p.username, &emoji_url),
            base(12.0, false),
            CARD_USERNAME_WIDTH,
        ),
        lay_out(Paragraph::plain(status, &emoji_url), base(14.0, false), CARD_TEXT_WIDTH),
        lay_out(
            p.bio
                .as_deref()
                .map_or_else(Paragraph::default, |b| Paragraph::rich(b, &emoji_url)),
            base(14.0, false),
            CARD_TEXT_WIDTH,
        ),
    ];
    // Still pictures: the card isn't a timeline row, so nothing would tick them.
    let mut pic = |url: Option<&str>, fit: Fit| match url.and_then(|u| images.get(u, fit)) {
        Some(image) => (image, true, -1),
        None => (no_image(), false, -1),
    };
    let blocks = laid
        .iter()
        .enumerate()
        .map(|(i, l)| block_item(if i == 0 { 2 } else { 0 }, l, &mut pic))
        .collect();
    (blocks, laid)
}

/// Fills the profile card.
pub fn profile(ui: &AppWindow, p: &Profile, images: &mut Images) {
    let (avatar, has_avatar) = picture(images, p.avatar_url.as_deref());
    let banner = p.banner_url.as_deref().and_then(|u| images.get(u, BANNER));
    ui.set_card_name(p.name.as_ref().into());
    ui.set_card_username(p.username.as_ref().into());
    ui.set_card_avatar(avatar);
    ui.set_card_has_avatar(has_avatar);
    ui.set_card_initials(initials(&p.name).into());
    ui.set_card_has_banner(banner.is_some());
    ui.set_card_banner(banner.unwrap_or_default());
    ui.set_card_presence(presence_colour(p.presence));
    ui.set_card_status(p.status.as_deref().unwrap_or_default().into());
    let roles: Vec<RoleItem> = p
        .roles
        .iter()
        .map(|(name, colour)| {
            let [r, g, b] = colour.unwrap_or_default();
            RoleItem {
                name: name.as_ref().into(),
                colour: slint::Color::from_rgb_u8(r, g, b),
                has_colour: colour.is_some(),
            }
        })
        .collect();
    ui.set_card_roles(ModelRc::new(VecModel::from(roles)));
    ui.set_card_has_bio(p.bio.is_some());
    ui.set_card_me(p.me);
    ui.set_card_can_kick(p.moderation.as_ref().is_some_and(|m| m.kick));
    ui.set_card_can_ban(p.moderation.as_ref().is_some_and(|m| m.ban));
    ui.set_card_can_remove_group_member(p.group_moderation.as_ref().is_some_and(|m| m.remove_member));
    ui.set_card_can_block(p.can_block);
    ui.set_card_blocked(p.relationship == Relationship::Blocked);
    ui.set_card_can_transfer_group_ownership(p.group_moderation.as_ref().is_some_and(|m| m.transfer_ownership));
    // The official ProfileActions: what we can do depends on where we stand.
    ui.set_card_relationship(match (p.has_friends, p.relationship) {
        (false, _) => 1,
        (true, Relationship::None) if p.bot => 6,
        (true, Relationship::None) => 0,
        (true, Relationship::Friend) => 1,
        (true, Relationship::Incoming) => 2,
        (true, Relationship::Outgoing) => 3,
        (true, Relationship::Blocked | Relationship::BlockedOther) => 4,
    });
}

/// The official presence colours.
/// How the official client names a presence.
pub(super) fn presence_label(p: Presence) -> &'static str {
    match p {
        Presence::Online => "Online",
        Presence::Idle => "Idle",
        Presence::Busy => "Do not disturb",
        Presence::Focus => "Focus",
        Presence::Offline => "Offline",
        Presence::Invisible => "Invisible",
        Presence::Unknown => "",
    }
}

pub fn presence_colour(p: Presence) -> slint::Color {
    let (r, g, b) = match p {
        Presence::Online => (0x3a, 0xbf, 0x7e),
        Presence::Idle => (0xf3, 0x9f, 0x00),
        Presence::Busy => (0xf8, 0x48, 0x48),
        Presence::Focus => (0x47, 0x99, 0xf0),
        Presence::Offline | Presence::Invisible => (0xa5, 0xa5, 0xa5),
        // Fully transparent: the UI draws no dot for it.
        Presence::Unknown => return slint::Color::from_argb_u8(0, 0, 0, 0),
    };
    slint::Color::from_rgb_u8(r, g, b)
}
