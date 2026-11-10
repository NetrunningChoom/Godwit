// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Shared model, picture, label, time and size helpers.

use super::*;

/// Replaces `model`'s rows with `rows`, touching only the ones that differ:
/// the common start and end stay, the middle is swapped. Prepending older
/// messages or appending new ones keeps the list where it is.
pub fn sync<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    let old = model.row_count();
    let mut start = 0;
    while start < old && start < rows.len() && model.row_data(start).as_ref() == Some(&rows[start]) {
        start += 1;
    }
    let mut end = 0;
    while end < old - start
        && end < rows.len() - start
        && model.row_data(old - 1 - end).as_ref() == Some(&rows[rows.len() - 1 - end])
    {
        end += 1;
    }
    // The changed stretch is updated in place, and only the difference in
    // length is removed or inserted: removing a long stretch first made the
    // list briefly short, which threw a scrolled list (members) back to the top.
    let (old_mid, new_mid) = (old - end - start, rows.len() - end - start);
    for i in 0..old_mid.min(new_mid) {
        if model.row_data(start + i).as_ref() != Some(&rows[start + i]) {
            model.set_row_data(start + i, rows[start + i].clone());
        }
    }
    for i in (new_mid..old_mid).rev() {
        model.remove(start + i);
    }
    for i in old_mid..new_mid {
        model.insert(start + i, rows[start + i].clone());
    }
}

/// Up to two letters for a picture that hasn't loaded.
pub fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

/// An avatar or icon, if it's loaded (loading starts otherwise).
pub fn avatar(images: &mut Images, url: Option<&str>) -> (Image, bool) {
    picture(images, url)
}

thread_local! {
    pub(super) static NO_OCCUPANTS: ModelRc<CallMemberItem> = ModelRc::from(Rc::new(VecModel::<CallMemberItem>::default()));
    static NO_IMAGE: Image = Image::from_rgba8(slint::SharedPixelBuffer::new(1, 1));
}

/// The stand-in when there's no picture (rows say so with a `has-…` flag).
/// Not `Image::default()`: Slint never finds two empty images equal, so rows
/// holding one were replaced on every refresh, losing hover and typing.
/// Copies of one 1×1 picture compare equal.
pub fn no_image() -> Image {
    NO_IMAGE.with(Clone::clone)
}

type KeptLabels = std::collections::HashMap<String, (Vec<crate::ui::LabelRun>, ModelRc<crate::ui::LabelRun>)>;

/// A short label (a channel's or person's name) as text and emoji pictures,
/// for the UI's `EmojiText`: the text font draws emoji as monochrome
/// outlines, where messages show the colour pictures. Labels without emoji
/// share one empty model, so their rows still compare equal between refreshes.
pub fn label_runs(text: &str, images: &mut Images) -> ModelRc<crate::ui::LabelRun> {
    use unicode_segmentation::UnicodeSegmentation;
    thread_local! {
        static NONE: ModelRc<crate::ui::LabelRun> = ModelRc::from(Rc::new(VecModel::<crate::ui::LabelRun>::default()));
        static KEPT: std::cell::RefCell<KeptLabels> = std::cell::RefCell::default();
    }
    if text.is_ascii() {
        return NONE.with(Clone::clone);
    }
    let mut runs: Vec<crate::ui::LabelRun> = Vec::new();
    let mut plain = String::new();
    let mut any = false;
    for g in text.graphemes(true) {
        // The shared lookup, as message text and reactions use.
        let Some(found) = crate::emoji::canonical(g) else {
            plain.push_str(g);
            continue;
        };
        any = true;
        if !plain.is_empty() {
            runs.push(crate::ui::LabelRun {
                text: std::mem::take(&mut plain).into(),
                ..Default::default()
            });
        }
        let url = crate::emoji::url_of(found);
        match images.get(&url, EMOJI) {
            Some(emoji) => runs.push(crate::ui::LabelRun {
                text: g.into(),
                is_emoji: true,
                emoji,
            }),
            // Until its picture loads: the character itself.
            None => runs.push(crate::ui::LabelRun {
                text: g.into(),
                ..Default::default()
            }),
        }
    }
    if !any {
        return NONE.with(Clone::clone);
    }
    if !plain.is_empty() {
        runs.push(crate::ui::LabelRun {
            text: plain.into(),
            ..Default::default()
        });
    }
    KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        if let Some((old, model)) = kept.get(text)
            && *old == runs
        {
            return model.clone();
        }
        // Bounded: names come and go as servers are opened.
        if kept.len() > 2048 {
            kept.clear();
        }
        let model = ModelRc::from(Rc::new(VecModel::from(runs.clone())));
        kept.insert(text.to_owned(), (runs, model.clone()));
        model
    })
}

/// A one-line quote of a message (a reply's) as text and emoji pictures, for
/// `EmojiText`: Unicode emoji from the pack, and custom emoji (rich text's
/// `godwit://emoji/` links) as their own pictures, where the quote used to
/// show `:name:` or the network's raw code. At most 128 characters.
/// Also whether every picture was ready (else the row is built again).
pub fn quote_runs(m: &Message, images: &mut Images) -> (String, ModelRc<crate::ui::LabelRun>, bool) {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};
    const MAX: usize = 128;
    // Rich text: its words and custom emoji, without the markup.
    let mut parts: Vec<(String, Option<String>)> = Vec::new();
    let mut plain = String::new();
    if m.markup == Markup::Rich {
        let mut in_emoji = false;
        for event in Parser::new(&m.text) {
            match event {
                Event::Start(Tag::Link { dest_url, .. }) => {
                    if let Some(image) = dest_url.strip_prefix(rich::EMOJI) {
                        parts.push((std::mem::take(&mut plain), None));
                        parts.push((String::new(), Some(rich::percent_decode(image))));
                        in_emoji = true;
                    }
                }
                Event::End(TagEnd::Link) => in_emoji = false,
                Event::Text(t) | Event::Code(t) if !in_emoji => plain.push_str(&t),
                Event::SoftBreak | Event::HardBreak | Event::End(TagEnd::Paragraph | TagEnd::Item)
                    if !plain.ends_with(' ') && !plain.is_empty() =>
                {
                    plain.push(' ');
                }
                _ => {}
            }
        }
    } else {
        plain = m.snippet.as_deref().unwrap_or(&m.source).replace('\n', " ");
    }
    parts.push((plain, None));
    // The text alone, cut to MAX characters (what's shown with no emoji).
    let mut text = String::new();
    let mut runs: Vec<crate::ui::LabelRun> = Vec::new();
    let mut left = MAX;
    let mut any = false;
    let mut ready = true;
    for (words, custom) in parts {
        if left == 0 {
            break;
        }
        if let Some(url) = custom {
            any = true;
            left = left.saturating_sub(1);
            match images.get(&url, EMOJI) {
                Some(emoji) => runs.push(crate::ui::LabelRun {
                    text: "".into(),
                    is_emoji: true,
                    emoji,
                }),
                None => {
                    ready = false;
                    runs.push(crate::ui::LabelRun::default());
                }
            }
            continue;
        }
        let cut: String = words.chars().take(left).collect();
        left -= cut.chars().count();
        text.push_str(&cut);
        let model = label_runs(&cut, images);
        if model.row_count() == 0 {
            if !cut.is_empty() {
                runs.push(crate::ui::LabelRun {
                    text: cut.into(),
                    ..Default::default()
                });
            }
        } else {
            any = true;
            // A Unicode emoji still loading is its character for now.
            ready &= model
                .iter()
                .all(|r| r.is_emoji || crate::emoji::canonical(&r.text).is_none());
            runs.extend(model.iter());
        }
    }
    if left == 0 {
        text.push_str("...");
        runs.push(crate::ui::LabelRun {
            text: "...".into(),
            ..Default::default()
        });
    }
    if !any {
        return (text, label_runs("", images), true);
    }
    (text, ModelRc::from(Rc::new(VecModel::from(runs))), ready)
}

pub(crate) fn picture(images: &mut Images, url: Option<&str>) -> (Image, bool) {
    match url.and_then(|u| images.get(u, AVATAR)) {
        Some(img) => (img, true),
        None => (no_image(), false),
    }
}

/// When a chat was last active, short, as chat lists show it: the time
/// today, the weekday this week, the date this year, else the year. "" when
/// the network gave no time.
pub(crate) fn last_talked(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    let Some(at) = Local.timestamp_millis_opt(ms).single() else {
        return String::new();
    };
    let now = Local::now();
    let days = (now.date_naive() - at.date_naive()).num_days();
    match days {
        ..=0 => at.format("%H:%M").to_string(),
        1..=6 => at.format("%a").to_string(),
        _ if at.format("%Y").to_string() == now.format("%Y").to_string() => at.format("%b %-d").to_string(),
        _ => at.format("%Y").to_string(),
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1000.0 && unit < UNITS.len() - 1 {
        size /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// "Ann is typing…" for the names given.
pub fn typing(names: &[Arc<str>]) -> String {
    match names {
        [] => String::new(),
        [a] => format!("{a} is typing…"),
        [a, b] => format!("{a} and {b} are typing…"),
        _ => "Several people are typing…".to_owned(),
    }
}
#[cfg(test)]
mod tests {
    use super::super::sidebar::room_row;
    use super::super::timeline::preview_size;
    use super::*;

    #[test]
    fn google_chat_is_a_selectable_wip_with_google_privacy_wording() {
        assert_eq!(
            ACCOUNT_NETWORKS.iter().filter(|n| **n == Network::GoogleChat).count(),
            1
        );
        assert_eq!(ACCOUNT_NETWORKS.len(), Network::ALL.len());
        let item = network(Network::GoogleChat, true);
        assert_eq!(item.key, "gchat");
        assert_eq!(item.name, "Google Chat");
        assert!(item.available && item.wip && !item.encrypted);
        assert!(
            item.lines
                .iter()
                .any(|line| line == "Messages aren't end-to-end encrypted")
        );

        let mut room = crate::demo::room("chat", "Chat", RoomKind::Group, false, 0);
        room.id = RoomId::new(AccountId::new(Network::GoogleChat, "test"), "chat");
        room.encryption = RoomEncryption::Unencrypted;
        let (encrypted, line) = security(&room);
        assert!(!encrypted);
        assert_eq!(line, "Not end-to-end encrypted");
    }

    #[test]
    fn voice_rows_keep_all_participants_and_show_capacity_and_unlimited() {
        let mut room = crate::demo::room("voice", "Lounge", RoomKind::Group, false, 0);
        let mut images = Images::offline();
        let members: Vec<CallMember> = (0..15)
            .map(|i| CallMember {
                user: crate::demo::user(&i.to_string()),
                name: format!("Guest {i}").into(),
                avatar_url: None,
                speaking: false,
                muted: false,
                deafened: false,
                stage: None,
            })
            .collect();
        room.capabilities.voice_user_limit = Some(20);
        let row = room_row(&room, false, Some(&members), &mut images);
        assert_eq!(row.voice_count, "15/20");
        assert_eq!(row.occupants.row_count(), 15);
        room.capabilities.voice_user_limit = Some(0);
        assert_eq!(room_row(&room, false, Some(&members), &mut images).voice_count, "15/∞");
        room.capabilities.voice_user_limit = None;
        assert_eq!(room_row(&room, false, Some(&members), &mut images).voice_count, "");
    }

    #[test]
    fn syncing_updates_in_place_and_keeps_the_length_steady() {
        let model = VecModel::from(vec![1, 2, 3, 4, 5]);
        sync(&model, vec![1, 9, 3, 8, 5]);
        assert_eq!(model.iter().collect::<Vec<_>>(), [1, 9, 3, 8, 5]);
        sync(&model, vec![0, 1, 9, 3, 8, 5]);
        assert_eq!(model.iter().collect::<Vec<_>>(), [0, 1, 9, 3, 8, 5]);
        sync(&model, vec![1, 3]);
        assert_eq!(model.iter().collect::<Vec<_>>(), [1, 3]);
        sync(&model, Vec::new());
        assert_eq!(model.row_count(), 0);
    }

    #[test]
    fn initials_take_up_to_two_words() {
        assert_eq!(initials("Godwit Fans"), "GF");
        assert_eq!(initials("ann"), "A");
        assert_eq!(initials("  #general chat here"), "GC");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn switcher_puts_name_prefixes_then_unread_first() {
        use godwit_core::application::{Application, Event};
        use godwit_core::contacts::Contacts;
        use godwit_core::ids::AccountId;
        use godwit_core::network::Network;
        use godwit_core::room::{Capabilities, RoomEncryption};
        let account = AccountId::new(Network::Stoat, "me");
        let room = |raw: &str, name: &str, unread: bool| Room {
            notify: Default::default(),
            id: RoomId::new(account.clone(), raw),
            name: name.into(),
            kind: RoomKind::Group,
            icon_url: None,
            capabilities: Capabilities::of(Network::Stoat),
            encryption: RoomEncryption::Unencrypted,
            unread,
            mentions: 0,
            last_read: None,
            invited_by: None,
            presence: None,
            status: None,
            member_count: None,
            last_activity: 0,
            parent: None,
            joined_thread: false,
        };
        let mut app = Application::new(Contacts::default());
        app.apply(Event::AccountReady {
            account: account.clone(),
            display_name: "Me".into(),
            username: "me".into(),
            avatar_url: None,
            rooms: vec![
                room("a", "the dev chat", false),
                room("b", "devops", false),
                room("c", "my dev notes", true),
                room("d", "music", false),
            ],
            servers: Vec::new(),
        });
        let (rows, _) = switch_results(app.state(), "DEV", &mut Images::offline());
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["devops", "my dev notes", "the dev chat"]);
    }

    #[test]
    fn sizes_and_typing_lines() {
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1_500_000), "1.5 MB");
        assert_eq!(preview_size(Some((800, 300))), (400, 150));
        assert_eq!(preview_size(None), (300, 200));
        assert_eq!(typing(&["Ann".into(), "Bo".into()]), "Ann and Bo are typing…");
    }

    #[test]
    fn sync_keeps_unchanged_rows_and_handles_prepend_append_and_edit() {
        let model = VecModel::from(vec![2, 3, 4]);
        sync(&model, vec![1, 2, 3, 4, 5]);
        assert_eq!(model.iter().collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
        sync(&model, vec![1, 2, 9, 4, 5]);
        assert_eq!(model.iter().collect::<Vec<_>>(), [1, 2, 9, 4, 5]);
        sync(&model, vec![]);
        assert_eq!(model.row_count(), 0);
        sync(&model, vec![7, 7]);
        assert_eq!(model.iter().collect::<Vec<_>>(), [7, 7]);
    }
}
