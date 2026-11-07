// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! A made-up world for trying Godwit without any account: a Stoat-like
//! account with a server, many DMs, a busy channel, members and pictures.
//! Used by `godwit --demo` and the off-screen preview (`examples/preview.rs`).
//! Nothing here touches the network, a session or the user's saved files.

use godwit_core::application::{Action, Application, Event};
use godwit_core::call::CallMember;
use godwit_core::contacts::Contacts;
use godwit_core::ids::{AccountId, MessageId, RoomId, ServerId, UserId};
use godwit_core::members::{Member, MemberGroup, Presence};
use godwit_core::message::{Attachment, AttachmentKind, Embed, Markup, Message, Reaction};
use godwit_core::network::Network;
use godwit_core::room::{Capabilities, Room, RoomEncryption, RoomKind};
use godwit_core::server::{Category, Server};

use crate::images::Images;

pub fn account() -> AccountId {
    AccountId::new(Network::Stoat, "me")
}

pub fn user(raw: &str) -> UserId {
    UserId::new(account(), raw)
}

pub fn room(raw: &str, name: &str, kind: RoomKind, unread: bool, mentions: u32) -> Room {
    Room {
        id: RoomId::new(account(), raw),
        name: name.into(),
        kind,
        icon_url: None,
        capabilities: Capabilities {
            report: true,
            ..Capabilities::of(Network::Stoat)
        },
        encryption: RoomEncryption::Unencrypted,
        unread,
        mentions,
        last_read: None,
        invited_by: None,
        presence: None,
        status: None,
        member_count: None,
        last_activity: 0,
        parent: None,
    }
}

pub fn msg(id: &str, who: &str, name: &str, at_min: i64, text: &str) -> Message {
    Message {
        id: MessageId(id.into()),
        author: user(who),
        author_name: name.into(),
        author_avatar: Some(format!("preview://avatar/{who}").into()),
        author_colour: None,
        sent_ms: 1_793_000_000_000 + at_min * 60_000,
        edited: false,
        system: false,
        text: text.into(),
        markup: Markup::Rich,
        source: text.into(),
        reply_to: None,
        jumbo: Vec::new(),
        attachments: Vec::new(),
        embeds: Vec::new(),
        invites: Vec::new(),
        reactions: Vec::new(),
        pending: false,
        snippet: None,
        pinned: false,
    }
}

pub fn member(raw: &str, name: &str, presence: Presence, status: Option<&str>, colour: Option<[u8; 3]>) -> Member {
    Member {
        user: user(raw),
        name: name.into(),
        avatar_url: Some(format!("preview://avatar/{raw}").into()),
        colour,
        presence,
        status: status.map(Into::into),
    }
}

/// Shared profile defaults; each scene keeps its own visible text and roles.
pub fn profile(user: UserId, name: &str) -> godwit_core::members::Profile {
    godwit_core::members::Profile {
        me: user.raw.as_ref() == "me",
        can_report: user.raw.as_ref() != "me",
        username: format!("{}#0001", name.to_lowercase().replace(' ', "")).into(),
        avatar_url: Some(format!("preview://avatar/{}", user.raw).into()),
        banner_url: None,
        presence: Presence::Online,
        status: None,
        bio: Some("Made up for the demo.".into()),
        roles: Vec::new(),
        name: name.into(),
        user,
        relationship: Default::default(),
        bot: false,
        has_friends: true,
        moderation: None,
        group_moderation: None,
        can_block: false,
    }
}

pub fn call_member(raw: &str, name: &str, speaking: bool, muted: bool) -> CallMember {
    CallMember {
        user: user(raw),
        name: name.into(),
        avatar_url: None,
        speaking,
        muted,
        deafened: muted,
    }
}

/// Made-up pictures, handed to the loader as if downloaded: a soft
/// two-colour avatar per person, and a landscape "photo".
pub fn pictures(images: &mut Images) {
    use crate::images::{AVATAR, PREVIEW};
    let paint = |w: u32, h: u32, f: &dyn Fn(u32, u32) -> [u8; 3]| -> Vec<u8> {
        (0..h)
            .flat_map(|y| {
                (0..w).flat_map(move |x| {
                    let [r, g, b] = f(x, y);
                    [r, g, b, 255]
                })
            })
            .collect()
    };
    let hues: [(&str, [u8; 3], [u8; 3]); 6] = [
        ("u0", [0xf2, 0x8b, 0x82], [0xc2, 0x4a, 0x6b]),
        ("u1", [0x8a, 0xb4, 0xf8], [0x3b, 0x5b, 0xdb]),
        ("u2", [0x81, 0xc9, 0x95], [0x2e, 0x7d, 0x4f]),
        ("u3", [0xfd, 0xd6, 0x63], [0xd9, 0x8a, 0x1c]),
        ("me", [0xc5, 0x8a, 0xf9], [0x6a, 0x3d, 0xc4]),
        ("u4", [0x78, 0xd9, 0xec], [0x1a, 0x7f, 0x9b]),
    ];
    for (who, a, b) in hues {
        let px = paint(84, 84, &|x, y| {
            let t = (x + y) as f32 / 166.0;
            let mix = |i: usize| (f32::from(a[i]) * (1.0 - t) + f32::from(b[i]) * t) as u8;
            [mix(0), mix(1), mix(2)]
        });
        images.put(&format!("preview://avatar/{who}"), AVATAR, 84, 84, &px);
    }
    // A sunset over water, 400 × 225 (a 16:9 photo's preview size).
    let photo = paint(400, 225, &|x, y| {
        if y < 140 {
            let t = y as f32 / 140.0;
            [
                (250.0 - 60.0 * t) as u8,
                (170.0 - 90.0 * t) as u8,
                (90.0 + 60.0 * t) as u8,
            ]
        } else {
            let wave = ((x as f32 / 9.0).sin() * 10.0) as i32;
            [(40 + wave.max(0)) as u8, 60, (110 + wave) as u8]
        }
    });
    images.put("preview://photo", PREVIEW, 400, 225, &photo);
    // The same photo as a link preview's thumbnail (drawn at 80 px).
    images.put("preview://photo", crate::images::THUMBNAIL, 400, 225, &photo);
    // A server icon: a teal square with a light band (cut round by the loader).
    let icon = paint(84, 84, &|_, y| {
        if (30..54).contains(&y) {
            [0xe0, 0xf7, 0xfa]
        } else {
            [0x00, 0x96, 0x88]
        }
    });
    images.put("preview://server", AVATAR, 84, 84, &icon);
    // Emoji: a disc in a few colours, transparent around it, at each size used.
    use crate::images::{EMOJI, JUMBO};

    let colours = [
        [0xff, 0xc8, 0x3d],
        [0xff, 0x6b, 0x6b],
        [0x6b, 0xcb, 0x77],
        [0x4d, 0x96, 0xff],
    ];
    // The Unicode emoji in the demo's labels, as their pictures.
    for (emoji, c) in [("🎉", colours[0]), ("📢", colours[3])] {
        let url = godwit_stoat::emoji::unicode_url(crate::emoji::pack(), emoji);
        images.put(&url, EMOJI, 40, 40, &disc(40, c));
    }
    for (i, c) in colours.iter().enumerate() {
        images.put(&format!("preview://emoji/{i}"), EMOJI, 40, 40, &disc(40, *c));
        images.put(&format!("preview://emoji/{i}"), JUMBO, 192, 192, &disc(192, *c));
    }
}

/// A soft round dot of `colour`, `size` pixels across (RGBA).
fn disc(size: u32, colour: [u8; 3]) -> Vec<u8> {
    let c = size as f32 / 2.0;
    (0..size)
        .flat_map(|y| {
            (0..size).flat_map(move |x| {
                let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt();
                let a = ((c * 0.9 - d) + 0.5).clamp(0.0, 1.0);
                [colour[0], colour[1], colour[2], (a * 255.0) as u8]
            })
        })
        .collect()
}

/// Stand-ins for every Unicode emoji the picker shows, at their real
/// addresses (about 12 MB: the preview only, never the demo).
pub fn picker_pictures(images: &mut Images) {
    use crate::images::PICKER_EMOJI;
    let colours = [
        [0xff, 0xc8, 0x3d],
        [0xff, 0x6b, 0x6b],
        [0x6b, 0xcb, 0x77],
        [0x4d, 0x96, 0xff],
    ];
    for (i, e) in emojis::iter().enumerate() {
        let url = godwit_stoat::emoji::unicode_url(crate::emoji::pack(), e.as_str());
        images.put(&url, PICKER_EMOJI, 44, 44, &disc(44, colours[i % colours.len()]));
    }
}

/// Made-up filler, the same on every run: nonsense names built from
/// syllables and lorem-style sentences. Nothing in the demo is a real name.
pub struct Bogus(u64);

const SYLLABLES: [&str; 24] = [
    "ka", "lo", "mi", "ra", "ve", "tu", "zen", "pol", "dri", "sa", "no", "fi", "quo", "bex", "ul", "ar", "is", "om",
    "tal", "rin", "co", "pe", "xa", "yu",
];
const FILLER: [&str; 24] = [
    "lorem",
    "ipsum",
    "dolor",
    "sit",
    "amet",
    "consectetur",
    "adipiscing",
    "elit",
    "sed",
    "do",
    "eiusmod",
    "tempor",
    "incididunt",
    "ut",
    "labore",
    "et",
    "dolore",
    "magna",
    "aliqua",
    "enim",
    "minim",
    "veniam",
    "quis",
    "nostrud",
];

impl Bogus {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x2545_f491_4f6c_dd1d)
    }

    /// splitmix64: small, fast, and good enough for filler.
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % (n.max(1) as u64)).unwrap_or(0)
    }

    /// A made-up word of two or three syllables.
    pub fn word(&mut self) -> String {
        let n = 2 + self.below(2);
        (0..n).map(|_| SYLLABLES[self.below(SYLLABLES.len())]).collect()
    }

    /// A made-up name: one word, or two (lengths vary, for layouts).
    pub fn name(&mut self) -> String {
        let words = 1 + self.below(2);
        (0..words)
            .map(|_| capitalise(&self.word()))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// `n` words of filler, starting with a capital.
    pub fn sentence(&mut self, n: usize) -> String {
        let words: Vec<&str> = (0..n.max(1)).map(|_| FILLER[self.below(FILLER.len())]).collect();
        capitalise(&words.join(" "))
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// The made-up name of person `u{i}`.
pub fn person(i: usize) -> String {
    Bogus::new(1000 + i as u64).name()
}

/// The demo account's own made-up name.
pub fn me() -> String {
    Bogus::new(7).name()
}

/// Filler text: `words` words, from `seed`.
pub fn filler(seed: u64, words: usize) -> String {
    Bogus::new(seed).sentence(words)
}

/// An account with a server, many DMs, and a busy channel. Every name and
/// text is made up (see [`Bogus`]).
pub fn world(contacts: Contacts) -> (Application, RoomId, RoomId) {
    let mut app = Application::new(contacts);
    let mut bogus = Bogus::new(42);
    let server = ServerId::new(account(), "S");
    // As the bridge does: only voice channels (and DMs, groups) offer calls.
    let channel = |raw: &str, name: &str, unread: bool, mentions: u32| {
        let mut r = room(
            raw,
            name,
            RoomKind::Channel { server: server.clone() },
            unread,
            mentions,
        );
        r.capabilities.voice = raw == "voice";
        r
    };
    let mut rooms = vec![room("notes", "Saved Notes", RoomKind::Notes, false, 0)];
    for i in 0..14 {
        let mut dm = room(
            &format!("dm{i}"),
            &person(i),
            RoomKind::Direct {
                with: user(&format!("u{i}")),
            },
            i == 1,
            u32::from(i == 3),
        );
        if i < 5 {
            dm.icon_url = Some(format!("preview://avatar/u{i}").into());
        }
        // Talked to at times spread over the last few days, so Home's
        // grouping (by day, then status) shows.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0));
        dm.last_activity = now - i64::try_from(i).unwrap_or(0) * 7 * 3_600_000;
        // Every presence, and a custom status now and then.
        dm.presence = Some(
            [
                Presence::Online,
                Presence::Idle,
                Presence::Busy,
                Presence::Focus,
                Presence::Offline,
            ][i % 5],
        );
        if i % 3 == 0 {
            dm.status = Some(bogus.sentence(3).to_lowercase().into());
        }
        rooms.push(dm);
    }
    let mut group = room("grp", &bogus.sentence(2), RoomKind::Group, true, 0);
    group.member_count = Some(4);
    rooms.push(group);
    let mut channel_names: Vec<String> = (0..4).map(|_| bogus.word()).collect();
    // Discord's channel and category names often carry emoji.
    if let Some(name) = channel_names.get_mut(1) {
        *name = format!("🎉-{name}");
    }
    for (i, (raw, unread, mentions)) in [
        ("general", false, 0),
        ("random", true, 0),
        ("dev", true, 2),
        ("voice", false, 0),
    ]
    .into_iter()
    .enumerate()
    {
        rooms.push(channel(raw, &channel_names[i], unread, mentions));
    }
    let r = |raw: &str| RoomId::new(account(), raw);
    let servers = vec![
        Server {
            id: server.clone(),
            name: bogus.name().into(),
            icon_url: Some("preview://server".into()),
            banner_url: None,
            categories: vec![
                Category {
                    title: "".into(),
                    rooms: vec![r("general"), r("random")],
                },
                Category {
                    title: format!("📢 {}", capitalise(&bogus.word())).into(),
                    rooms: vec![r("dev"), r("voice")],
                },
            ],
            muted: false,
            // Ours to run: every entry in its menu.
            rights: godwit_core::server::ServerRights {
                invite: true,
                create_channels: true,
                settings: true,
                owner: true,
                report: true,
                ..Default::default()
            },
        },
        Server {
            id: ServerId::new(account(), "S2"),
            name: bogus.name().into(),
            icon_url: None,
            banner_url: None,
            categories: Vec::new(),
            muted: false,
            rights: godwit_core::server::ServerRights {
                invite: true,
                report: true,
                ..Default::default()
            },
        },
        Server {
            id: ServerId::new(account(), "S3"),
            name: bogus.name().into(),
            icon_url: None,
            banner_url: None,
            categories: Vec::new(),
            muted: false,
            rights: godwit_core::server::ServerRights {
                invite: true,
                report: true,
                ..Default::default()
            },
        },
    ];
    let me = me();
    app.apply(Event::AccountReady {
        account: account(),
        username: format!("{}#0001", me.to_lowercase().replace(' ', "")).into(),
        display_name: me.as_str().into(),
        avatar_url: Some("preview://avatar/me".into()),
        rooms,
        servers,
    });
    app.apply(Event::OwnPresence {
        account: account(),
        presence: Presence::Online,
        status: Some(filler(41, 3).to_lowercase().into()),
    });
    let general = r("general");
    let dm = r("dm0");
    let (p0, p1, p2, p3) = (person(0), person(1), person(2), person(3));
    let word = |b: &mut Bogus| b.word();

    let mut history = vec![
        msg("00", "u2", &p2, -24 * 60, &bogus.sentence(4)),
        // A mention and a channel link.
        msg(
            "01",
            "u1",
            &p1,
            0,
            &format!(
                "{} [\\@{p0}](godwit://user/u0), {} [\\#{}](godwit://room/dev)",
                bogus.sentence(1),
                bogus.sentence(1).to_lowercase(),
                channel_names[2]
            ),
        ),
        msg("02", "u1", &p1, 1, &format!("{}?", bogus.sentence(5))),
        // Bold text.
        msg(
            "03",
            "me",
            &me,
            3,
            &format!(
                "{} **{}** {}.",
                bogus.sentence(3),
                bogus.sentence(2),
                bogus.sentence(5).to_lowercase()
            ),
        ),
        // Italics.
        msg(
            "04",
            "u0",
            &p0,
            5,
            &format!("{} *{}*?", bogus.sentence(2), word(&mut bogus)),
        ),
        // A code block.
        msg(
            "05",
            "me",
            &me,
            6,
            &format!(
                "{}:\n\n```rust\nlet {} = {}::new({});\n{}.{}(|x| {{ x.{}(&mut {}); }});\n```",
                bogus.sentence(4),
                word(&mut bogus),
                capitalise(&word(&mut bogus)),
                word(&mut bogus),
                word(&mut bogus),
                word(&mut bogus),
                word(&mut bogus),
                word(&mut bogus)
            ),
        ),
        // A quote.
        msg(
            "06",
            "u2",
            &p2,
            9,
            &format!("> {}\n\n{}", bogus.sentence(5), bogus.sentence(6).to_lowercase()),
        ),
        // A long message, for wrapping.
        msg("07", "u0", &p0, 12, &format!("{}.", bogus.sentence(40))),
    ];
    let mut reply = msg("08", "u1", &p1, 14, &bogus.sentence(1).to_lowercase());
    reply.reply_to = Some(MessageId("07".into()));
    reply.reactions = vec![
        Reaction {
            emoji: "👍".into(),
            image_url: Some("preview://emoji/0".into()),
            count: 3,
            me: true,
        },
        Reaction {
            emoji: "🎉".into(),
            image_url: Some("preview://emoji/1".into()),
            count: 1,
            me: false,
        },
    ];
    history.push(reply);
    let mut file = msg("09", "u2", &p2, 15, &bogus.sentence(3).to_lowercase());
    file.attachments = vec![Attachment {
        name: format!("{}.tsv", word(&mut bogus)).into(),
        url: "preview://file".into(),
        kind: AttachmentKind::File,
        size: 2_480,
        dimensions: None,
        original_url: None,
    }];
    history.push(file);
    let mut photo = msg("09b", "u0", &p0, 15, &bogus.sentence(3).to_lowercase());
    photo.attachments = vec![Attachment {
        name: format!("{}.jpg", word(&mut bogus)).into(),
        url: "preview://photo".into(),
        kind: AttachmentKind::Image,
        size: 1_800_000,
        dimensions: Some((1600, 900)),
        original_url: None,
    }];
    photo.embeds.push(Embed {
        title: Some("A landscape from the demo".into()),
        description: Some("A preview card with its thumbnail beside the details.".into()),
        url: Some("https://example.invalid/demo-landscape".into()),
        site_name: Some("Godwit demo".into()),
        image_url: Some("preview://photo".into()),
        media_dimensions: Some((400, 225)),
        image_thumbnail: true,
        colour: Some([0x58, 0x8b, 0xa8]),
        ..Embed::default()
    });
    // A bot's card: rich text, and its media below (no network fetch: the
    // stand-in picture).
    photo.embeds.push(Embed {
        title: Some("Status".into()),
        description: Some(format!("**{}** and *{}*", bogus.sentence(2), bogus.sentence(3)).into()),
        description_markup: Markup::Rich,
        image_url: Some("preview://photo".into()),
        media_dimensions: Some((1600, 900)),
        ..Embed::default()
    });
    history.push(photo);
    // Invite links: one to a server we're in, one to try Join with.
    let mut invite = msg(
        "09d",
        "u2",
        &person(2),
        15,
        "https://stt.gg/DemoMine and https://stoat.chat/invite/DemoJoin",
    );
    invite.invites = vec!["DemoMine".into(), "DemoJoin".into()];
    history.push(invite);
    for code in ["DemoMine", "DemoJoin"] {
        app.apply(Event::InviteLoaded {
            account: account(),
            code: code.into(),
            preview: Some(invite_preview(code)),
        });
    }
    let mut big = msg("09c", "u1", &p1, 15, "🎉🎉");
    big.jumbo = vec![
        ("preview://emoji/1".into(), ":tada:".into()),
        ("preview://emoji/2".into(), ":tada:".into()),
    ];
    history.push(big);
    let mut sys = msg("10", "u3", &p3, 16, &format!("{p3} joined the server"));
    sys.system = true;
    sys.markup = Markup::Plain;
    history.push(sys);
    let mut edited = msg("11", "me", &me, 17, &bogus.sentence(5));
    edited.edited = true;
    edited.author_colour = Some([0x79, 0xc0, 0xff]);
    history.push(edited);
    // Two in a row from someone we blocked: one collapsed row.
    let blocked_name = person(20);
    history.push(msg("12", BLOCKED, &blocked_name, 18, &bogus.sentence(4)));
    history.push(msg("13", BLOCKED, &blocked_name, 18, &bogus.sentence(6)));
    app.apply(Event::Blocked {
        account: account(),
        users: vec![user(BLOCKED)],
    });

    app.dispatch(Action::SelectRoom(general.clone()));
    app.apply(Event::History {
        room: general.clone(),
        before: None,
        messages: history,
        reached_start: true,
    });
    let status = bogus.sentence(2).to_lowercase();
    let busy = bogus.sentence(3).to_lowercase();
    app.apply(Event::Members {
        room: general.clone(),
        groups: vec![
            MemberGroup {
                title: "Moderators — 1".into(),
                members: vec![member(
                    "u0",
                    &p0,
                    Presence::Online,
                    Some(&status),
                    Some([0xff, 0x7b, 0x72]),
                )],
            },
            MemberGroup {
                title: "Online — 3".into(),
                members: vec![
                    member("u1", &p1, Presence::Idle, None, None),
                    member("u2", &p2, Presence::Busy, Some(&busy), None),
                    member("me", &me, Presence::Online, None, Some([0x79, 0xc0, 0xff])),
                ],
            },
            MemberGroup {
                title: "Offline — 1".into(),
                members: vec![member("u3", &p3, Presence::Offline, None, None)],
            },
        ],
    });
    app.apply(Event::Typing {
        room: general.clone(),
        names: vec![p1.as_str().into()],
    });
    // Friends and pending requests, following `relationship`.
    let demo_person = |n: usize| godwit_core::application::Person {
        id: user(&format!("u{n}")),
        name: person(n).into(),
        avatar_url: Some(format!("preview://avatar/u{n}").into()),
        online: !n.is_multiple_of(3),
    };
    let with = |r: godwit_core::members::Relationship| -> Vec<godwit_core::application::Person> {
        (0..14)
            .filter(|n| relationship(&format!("u{n}")) == r)
            .map(demo_person)
            .collect()
    };
    use godwit_core::members::Relationship as R;
    app.apply(Event::Friends {
        account: account(),
        people: with(R::Friend),
    });
    app.apply(Event::FriendRequests {
        account: account(),
        incoming: with(R::Incoming).into_iter().take(2).collect(),
        outgoing: with(R::Outgoing).into_iter().take(1).collect(),
    });
    // Two people already in the voice channel, for joining.
    let lounge = RoomId::new(account(), "voice");
    app.apply(Event::VoiceOccupants {
        room: lounge.clone(),
        members: DemoAccount::occupants(),
    });
    (app, general, dm)
}

/// The demo's Steam account (made up; a valid-looking individual id).
pub fn steam_account() -> AccountId {
    AccountId::new(Network::Steam, "76561190000000001")
}

pub fn steam_user(n: u64) -> UserId {
    UserId::new(steam_account(), (76_561_190_000_000_100 + n).to_string())
}

/// A second account, for trying several at once (`godwit --demo` and the
/// preview's multi-account scenes): a Steam account whose friends include
/// one person merged with a Stoat friend (online only on Steam), one whose
/// name nearly matches another Stoat friend (a merge suggestion), one with a
/// nickname, and a chat with Steam's own markup (an emoticon, a sticker, a
/// link, a spoiler) converted as the real account does. Returns that chat.
pub fn multi(app: &mut Application) -> RoomId {
    use godwit_core::application::Person;
    let steam = steam_account();
    let me_on_steam = UserId::new(steam.clone(), steam.user.as_ref());
    // Steam's names: person 4 by a gamer tag, person 8 near-identically.
    let tag = |n: usize| person(n).to_lowercase().replace(' ', "_");
    let friends = [
        (0u64, format!("{}_gg", tag(4)), true, Some("Playing Goose Game")),
        (1, person(8).replace(' ', "."), true, None),
        (2, person(20), false, None),
        (3, person(21), true, None),
    ];
    let steam_room = |n: u64, name: &str, online: bool, status: Option<&str>, unread: bool| Room {
        id: RoomId::new(steam.clone(), steam_user(n).raw.as_ref()),
        name: name.into(),
        kind: RoomKind::Direct { with: steam_user(n) },
        icon_url: None,
        capabilities: Capabilities::of(Network::Steam),
        encryption: RoomEncryption::Unencrypted,
        unread,
        mentions: 0,
        last_read: None,
        invited_by: None,
        presence: Some(if online { Presence::Online } else { Presence::Offline }),
        status: status.map(Into::into),
        member_count: None,
        last_activity: 0,
        parent: None,
    };
    let rooms = vec![
        steam_room(0, &friends[0].1, true, friends[0].3, false),
        steam_room(2, &friends[2].1, false, None, true),
    ];
    let chat = rooms[0].id.clone();
    app.apply(Event::AccountReady {
        account: steam.clone(),
        display_name: format!("{} (Steam)", me()).into(),
        username: me().to_lowercase().replace(' ', "_").into(),
        avatar_url: None,
        rooms,
        servers: Vec::new(),
    });
    app.apply(Event::OwnPresence {
        account: steam.clone(),
        presence: Presence::Online,
        status: None,
    });
    app.apply(Event::Friends {
        account: steam.clone(),
        people: friends
            .iter()
            .map(|(n, name, online, _)| Person {
                id: steam_user(*n),
                name: name.as_str().into(),
                avatar_url: None,
                online: *online,
            })
            .collect(),
    });
    // Person 4 is one person on both: merged, talked to on Steam, and
    // offline on Stoat (so the dots differ).
    let stoat4 = user("u4");
    let (into, from) = (
        app.state().contacts.of(&stoat4).map(|c| c.id),
        app.state().contacts.of(&steam_user(0)).map(|c| c.id),
    );
    if let (Some(into), Some(from)) = (into, from) {
        app.dispatch(Action::MergeContacts { into, from });
        app.dispatch(Action::SetPreferred {
            contact: into,
            network: Some(Network::Steam),
        });
    }
    if let Some(mut r) = app
        .state()
        .rooms
        .iter()
        .find(|r| r.id == RoomId::new(account(), "dm4"))
        .map(|r| (**r).clone())
    {
        r.presence = Some(Presence::Offline);
        r.status = None;
        app.apply(Event::RoomUpdated(r));
    }
    // A nickname, as the user would give one.
    if let Some(c) = app.state().contacts.of(&steam_user(3)).map(|c| c.id) {
        app.dispatch(Action::SetNickname {
            contact: c,
            nickname: Some("Raid lead".into()),
        });
    }
    // The chat with person 4 on Steam, in Steam's markup.
    let lines: [(u64, i64, &str); 5] = [
        (0, 0, "hey [emoticon]steamhappy[/emoticon] you around tonight?"),
        (1, 1, "yes! after dinner"),
        (0, 2, r#"[sticker type="Goose Chase" limit="0"][/sticker]"#),
        (
            0,
            3,
            "found the [url=https://store.steampowered.com/]store page[/url] btw",
        ),
        (1, 4, "[spoiler]the goose wins[/spoiler] [b]always[/b]"),
    ];
    let messages = lines
        .iter()
        .map(|(from, at, src)| {
            let c = godwit_steam::bbcode::convert(src);
            let (author, name) = if *from == 0 {
                (steam_user(0), friends[0].1.clone())
            } else {
                (me_on_steam.clone(), format!("{} (Steam)", me()))
            };
            Message {
                id: MessageId(format!("{}-0", 1_793_000_000 + at * 60).into()),
                author,
                author_name: name.into(),
                author_avatar: None,
                author_colour: None,
                sent_ms: (1_793_000_000 + at * 60) * 1000,
                edited: false,
                system: false,
                text: c.rich.into(),
                markup: Markup::Rich,
                source: c.plain.into(),
                reply_to: None,
                jumbo: c.jumbo.into_iter().map(|(u, n)| (u.into(), n.into())).collect(),
                attachments: Vec::new(),
                embeds: Vec::new(),
                invites: Vec::new(),
                reactions: Vec::new(),
                pending: false,
                snippet: None,
                pinned: false,
            }
        })
        .collect();
    app.apply(Event::History {
        room: chat.clone(),
        before: None,
        messages,
        reached_start: true,
    });
    chat
}

/// Steam's emoticon and sticker pictures in the demo chat, made up.
pub fn steam_pictures(images: &mut Images) {
    use crate::images::{EMOJI, JUMBO};
    let emoticon = "https://community.fastly.steamstatic.com/economy/emoticon/steamhappy";
    images.put(emoticon, EMOJI, 40, 40, &disc(40, [0x6b, 0xcb, 0x77]));
    let sticker = "https://community.fastly.steamstatic.com/economy/sticker/Goose%20Chase";
    images.put(sticker, JUMBO, 192, 192, &disc(192, [0xff, 0xc8, 0x3d]));
}

/// The demo's Discord account: a READY and canned history for
/// `godwit_discord::account::DiscordAccount::demo`, so the real bridge
/// converts them (mentions, custom emoji, spoilers, replies, reactions).
/// Every name is made up; ids are snowflakes from fixed times.
pub fn discord_world() -> (
    serde_json::Value,
    std::collections::HashMap<String, Vec<serde_json::Value>>,
) {
    use serde_json::json;
    let mut bogus = Bogus::new(311);
    // A snowflake for `minute` minutes into the demo's day.
    let flake = |minute: u64| (((1_793_000_000_000u64 + minute * 60_000) - 1_420_070_400_000) << 22).to_string();
    let me_id = flake(1);
    // Discord picks default avatars from the id: a millisecond apart, each its own.
    let people: Vec<(String, String)> = (0..5u64)
        .map(|i| {
            let id = flake(10 + i).parse::<u64>().unwrap_or_default() + (i << 22);
            (id.to_string(), person(30 + i as usize))
        })
        .collect();
    let user = |i: usize| {
        let (id, name) = &people[i];
        json!({"id": id, "username": name.to_lowercase().replace(' ', "_"), "global_name": name, "avatar": null})
    };
    let guild = flake(100);
    let (category, general, memes, lounge) = (flake(101), flake(102), flake(103), flake(104));
    let (mods, members) = (flake(105), flake(106));
    let emoji = flake(107);
    let (dm, group) = (flake(110), flake(111));
    let view_send = (1u64 << 10 | 1 << 11 | 1 << 6 | 1 << 15 | 1 << 16 | 1 << 0).to_string();
    let ready = json!({
        "user": {"id": me_id, "username": me().to_lowercase().replace(' ', "_"), "global_name": format!("{} (Discord)", me()), "avatar": null},
        "users": (0..5).map(user).collect::<Vec<_>>(),
        "guilds": [{
            "id": guild,
            "properties": {"name": bogus.name(), "owner_id": me_id, "icon": null, "description": filler(312, 6)},
            "roles": [
                {"id": guild, "name": "@everyone", "permissions": view_send, "position": 0},
                {"id": mods, "name": capitalise(&bogus.word()), "permissions": (1u64 << 1 | 1 << 2 | 1 << 13 | 1 << 28 | 1 << 5).to_string(), "position": 2, "color": 0x00e8_a33d, "hoist": true},
                {"id": members, "name": capitalise(&bogus.word()), "permissions": "0", "position": 1, "color": 0x004c_af8f, "hoist": false}
            ],
            "channels": [
                {"id": general, "type": 0, "name": bogus.word(), "position": 0, "last_message_id": flake(209)},
                {"id": category, "type": 4, "name": capitalise(&bogus.word()), "position": 0},
                {"id": memes, "type": 0, "name": bogus.word(), "position": 1, "parent_id": category, "last_message_id": flake(220)},
                {"id": lounge, "type": 2, "name": capitalise(&bogus.word()), "position": 2, "parent_id": category}
            ],
            "emojis": [{"id": emoji, "name": bogus.word(), "animated": false}]
        }],
        "merged_members": [[{"user_id": me_id, "roles": [mods]}, {"user_id": people[0].0, "roles": [members]}, {"user_id": people[1].0, "roles": [mods]}]],
        "private_channels": [
            {"id": dm, "type": 1, "recipient_ids": [people[2].0], "last_message_id": flake(232)},
            {"id": group, "type": 3, "name": "", "recipient_ids": [people[0].0, people[3].0], "last_message_id": flake(240)}
        ],
        "relationships": [{"id": people[2].0, "type": 1}, {"id": people[0].0, "type": 1}, {"id": people[4].0, "type": 3}],
        "read_state": {"entries": [
            {"id": general, "last_message_id": flake(205), "mention_count": 1},
            {"id": memes, "last_message_id": flake(220), "mention_count": 0},
            {"id": dm, "last_message_id": flake(230), "mention_count": 0}
        ]},
        "user_guild_settings": {"entries": []},
        "sessions": [{"status": "online"}]
    });
    let msg = |minute: u64, channel: &str, author: serde_json::Value, content: String| {
        json!({"id": flake(minute), "channel_id": channel, "guild_id": if channel == general || channel == memes { Some(guild.clone()) } else { None },
               "author": author, "content": content, "type": 0,
               "timestamp": chrono::DateTime::from_timestamp_millis(i64::try_from(1_793_000_000_000u64 + minute * 60_000).unwrap_or(0)).map(|t| t.to_rfc3339()).unwrap_or_default(),
               "edited_timestamp": null, "attachments": [], "embeds": [], "reactions": []})
    };
    let me_user = json!({"id": me_id, "username": "me", "global_name": format!("{} (Discord)", me()), "avatar": null});
    let mut general_msgs = vec![
        msg(
            200,
            &general,
            user(0),
            format!("**{}** — welcome to <#{general}>!", filler(313, 2)),
        ),
        msg(
            201,
            &general,
            user(1),
            format!("hey <@{me_id}>, try the ||spoiler|| and <:{}:{emoji}>", "blob"),
        ),
        msg(
            202,
            &general,
            me_user.clone(),
            "```rust\nfn main() { println!(\"hi\"); }\n```".into(),
        ),
        msg(
            203,
            &general,
            user(0),
            format!("{} __{}__ ~~{}~~", filler(314, 3), filler(315, 1), filler(316, 1)),
        ),
        msg(
            205,
            &general,
            user(3),
            format!("> {}\n{}", filler(317, 4), filler(318, 3)),
        ),
        msg(207, &general, user(1), format!("<@&{mods}> {}", filler(319, 4))),
        msg(209, &general, user(0), "https://discord.gg/DemoInvite".into()),
    ];
    general_msgs[1]["reactions"] = json!([{"emoji": {"id": null, "name": "👍"}, "count": 2, "me": true}, {"emoji": {"id": emoji, "name": "blob"}, "count": 1, "me": false}]);
    general_msgs[4]["type"] = json!(19);
    general_msgs[4]["message_reference"] = json!({"message_id": flake(201)});
    general_msgs[3]["attachments"] = json!([{"filename": "sunset.png", "url": "preview://photo", "content_type": "image/png", "size": 48_000, "width": 400, "height": 225}]);
    let mut join = msg(204, &general, user(4), String::new());
    join["type"] = json!(7);
    general_msgs.push(join);
    let memes_msgs = vec![msg(220, &memes, user(1), filler(320, 6))];
    let dm_msgs = vec![
        msg(230, &dm, user(2), filler(321, 5)),
        msg(231, &dm, me_user.clone(), filler(322, 3)),
        msg(232, &dm, user(2), format!("{} 🎉", filler(323, 2))),
    ];
    let group_msgs = vec![msg(240, &group, user(3), filler(324, 4))];
    let newest_first = |mut v: Vec<serde_json::Value>| {
        v.sort_by(|a, b| {
            let (x, y) = (
                b["id"].as_str().unwrap_or_default(),
                a["id"].as_str().unwrap_or_default(),
            );
            (x.len(), x).cmp(&(y.len(), y))
        });
        v
    };
    let history = [
        (general, general_msgs),
        (memes, memes_msgs),
        (dm, dm_msgs),
        (group, group_msgs),
    ]
    .into_iter()
    .map(|(c, v)| (c, newest_first(v)))
    .collect();
    (ready, history)
}

/// Discord's default avatars and the demo's custom emoji, painted, so demo
/// mode never fetches from Discord.
pub fn discord_pictures(images: &mut Images) {
    use crate::images::{AVATAR, EMOJI, JUMBO};
    let colours = [
        [0x58, 0x65, 0xf2],
        [0x75, 0x7e, 0x8a],
        [0x3b, 0xa5, 0x5c],
        [0xfa, 0xa6, 0x1a],
        [0xed, 0x42, 0x45],
        [0xeb, 0x45, 0x9f],
    ];
    for (i, c) in colours.into_iter().enumerate() {
        images.put(
            &format!("https://cdn.discordapp.com/embed/avatars/{i}.png"),
            AVATAR,
            84,
            84,
            &disc(84, c),
        );
    }
    let (ready, _) = discord_world();
    for e in ready["guilds"][0]["emojis"].as_array().into_iter().flatten() {
        if let Some(id) = e["id"].as_str() {
            let url = godwit_discord::store::emoji_url(id, false);
            images.put(&url, EMOJI, 40, 40, &disc(40, [0xff, 0x9f, 0x43]));
            images.put(&url, JUMBO, 192, 192, &disc(192, [0xff, 0x9f, 0x43]));
        }
    }
}

/// The demo's stand-in for a network: it answers commands the way a server
/// would (a sent message is confirmed, an edit comes back, …) without
/// sending anything anywhere.
fn demo_own_profile() -> godwit_core::own_profile::OwnProfile {
    godwit_core::own_profile::OwnProfile {
        name: me().into(),
        display_name: me().into(),
        pronouns: "they/them".into(),
        bio: "A **demo profile**. Nothing here is sent to a network.".into(),
        avatar_url: Some("preview://avatar/me".into()),
        banner_url: Some("preview://photo".into()),
    }
}

pub struct DemoAccount {
    own_profile: godwit_core::own_profile::OwnProfile,
    id: AccountId,
    /// People's names, for profile cards.
    names: std::collections::HashMap<String, String>,
    sent: u64,
    /// Each message's reactions, to toggle ours.
    reactions: std::collections::HashMap<String, Vec<Reaction>>,
    /// The call we're in, and whether we're muted there.
    call: Option<RoomId>,
    muted: bool,
    deafened: bool,
    /// Our presence and status text, as last set.
    presence: Presence,
    status: Option<std::sync::Arc<str>>,
    /// Relationships changed from profile cards (others follow `relationship`).
    relationships: std::collections::HashMap<String, godwit_core::members::Relationship>,
    /// The servers as the rail shows them, and each one's editable overview,
    /// so a saved rename or icon comes back like a server's update would.
    servers: Vec<Server>,
    overviews: std::collections::HashMap<String, godwit_core::server_settings::ServerOverview>,
    /// Each server's invites, bans and roles, changed as the pages ask.
    admin: std::collections::HashMap<String, DemoAdmin>,
    /// Friends and pending requests (theirs, ours), changed as the user
    /// accepts, declines, cancels or adds, and sent back each time.
    friends: Vec<godwit_core::application::Person>,
    incoming: Vec<godwit_core::application::Person>,
    outgoing: Vec<godwit_core::application::Person>,
}

/// A demo server's overview. Only the first server is ours to manage, so the
/// read-only form can be tried on the others.
fn demo_overview(app: &Application, server: &Server, index: usize) -> godwit_core::server_settings::ServerOverview {
    let channels: Vec<(RoomId, std::sync::Arc<str>)> = server
        .rooms()
        .filter_map(|id| app.state().room(id))
        .map(|r| (r.id.clone(), r.name.clone()))
        .collect();
    let first = channels.first().map(|(id, _)| id.raw.clone());
    godwit_core::server_settings::ServerOverview {
        name: server.name.clone(),
        description: filler(60 + index as u64, 8).into(),
        icon_url: server.icon_url.clone(),
        banner_url: (index == 0).then(|| "preview://photo".into()),
        can_manage: index == 0,
        can_manage_roles: index == 0,
        can_ban: index == 0,
        can_manage_emoji: index == 0,
        owner: index == 0,
        channels,
        system_channels: [first.clone(), first, None, None],
    }
}

/// A demo server's admin pages: made-up invites, bans and roles.
struct DemoAdmin {
    invites: Vec<godwit_core::server_settings::ServerInvite>,
    bans: Vec<godwit_core::server_settings::ServerBan>,
    roles: godwit_core::server_settings::ServerRoles,
    emoji: godwit_core::server_settings::ServerEmojis,
}

fn demo_admin(overview: &godwit_core::server_settings::ServerOverview, seed: u64) -> DemoAdmin {
    use godwit_core::server_settings::{ServerBan, ServerInvite, ServerRole, ServerRoles};
    use godwit_stoat::permissions as p;
    let mut bogus = Bogus::new(seed);
    let channel = |i: usize| overview.channels.get(i).map(|(_, n)| n.clone()).unwrap_or_default();
    let invites = (0..3)
        .map(|i| {
            let code: String = bogus.word().chars().chain(format!("{i}{seed}").chars()).collect();
            ServerInvite {
                link: format!("https://stt.gg/{code}").into(),
                code: code.into(),
                creator: person(i + 1).into(),
                creator_avatar: Some(format!("preview://avatar/u{}", i + 1).into()),
                channel: channel(i),
            }
        })
        .collect();
    let bans = (0..2)
        .map(|i| ServerBan {
            user: user(&format!("banned{i}")),
            name: format!("{}#{:04}", bogus.word(), 1000 + i).into(),
            avatar_url: None,
            reason: bogus.sentence(4).into(),
        })
        .collect();
    let mut role = |colour: Option<&str>, hoist: bool, allow: u64| ServerRole {
        id: format!("R{}", bogus.word()).into(),
        name: capitalise(&bogus.word()).into(),
        colour: colour.map(Into::into),
        hoist,
        allow,
        deny: 0,
    };
    let roles = vec![
        role(
            Some("#e8a33d"),
            true,
            p::MANAGE_SERVER | p::MANAGE_ROLE | p::KICK_MEMBERS | p::BAN_MEMBERS,
        ),
        role(Some("#4caf8f"), true, p::MANAGE_MESSAGES | p::TIMEOUT_MEMBERS),
        role(None, false, p::REACT),
    ];
    let emoji = godwit_core::server_settings::ServerEmojis {
        emoji: ["party_parrot", "this_is_fine", "shipit", "pepe_sad", "big_brain"]
            .iter()
            .enumerate()
            .map(|(i, name)| godwit_core::server_settings::ServerEmoji {
                id: format!("E{i}").into(),
                name: (*name).into(),
                url: Some(format!("preview://emoji/{}", i % 4).into()),
                creator: person(i % 3 + 1).into(),
            })
            .collect(),
        limit: Some(20),
        max_file_bytes: Some(500_000),
        rules: Default::default(),
    };
    DemoAdmin {
        emoji,
        invites,
        bans,
        roles: ServerRoles {
            roles,
            default_allow: p::VIEW_CHANNEL | p::READ_MESSAGE_HISTORY | p::SEND_MESSAGE | p::REACT | p::CONNECT,
            permissions: p::server_list(),
        },
    }
}

/// What the demo's invite links lead to: DemoMine to the server we're in,
/// anything else to "S9" (not joined; joining opens S3).
fn invite_preview(code: &str) -> godwit_core::message::InvitePreview {
    let mine = code == "DemoMine";
    godwit_core::message::InvitePreview {
        server: Some(ServerId::new(account(), if mine { "S" } else { "S9" })),
        name: Bogus::new(if mine { 11 } else { 13 }).name().into(),
        icon_url: mine.then(|| "preview://server".into()),
        members: Some(if mine { 1_204 } else { 87 }),
    }
}

/// The demo user we start out having blocked, so the collapsed row of
/// blocked messages can be seen (and tried: unblock shows them).
const BLOCKED: &str = "u20";

/// A made-up person's standing, so every profile card button can be tried:
/// `u{n}` by n mod 4 is a friend, nobody, someone who asked, someone asked.
fn relationship(raw: &str) -> godwit_core::members::Relationship {
    use godwit_core::members::Relationship;
    if raw == BLOCKED {
        return Relationship::Blocked;
    }
    match raw
        .strip_prefix('u')
        .and_then(|n| n.parse::<usize>().ok())
        .map(|n| n % 4)
    {
        Some(1) => Relationship::None,
        Some(2) => Relationship::Incoming,
        Some(3) => Relationship::Outgoing,
        _ => Relationship::Friend,
    }
}

impl DemoAccount {
    pub fn new(app: &Application) -> Self {
        let mut names = std::collections::HashMap::new();
        let mut reactions = std::collections::HashMap::new();
        for t in app.state().timelines.values() {
            for m in t.messages.iter() {
                names.insert(m.author.raw.to_string(), m.author_name.to_string());
                reactions.insert(m.id.0.to_string(), m.reactions.clone());
            }
        }
        let servers: Vec<Server> = app.state().servers.iter().map(|s| (**s).clone()).collect();
        let overviews = servers
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.raw.to_string(), demo_overview(app, s, i)))
            .collect::<std::collections::HashMap<_, _>>();
        let admin = overviews
            .iter()
            .map(|(raw, o)| (raw.clone(), demo_admin(o, raw.len() as u64 + 90)))
            .collect();
        let ours = |p: &&godwit_core::application::Person| p.id.account == account();
        let friends = app.state().friends.iter().filter(ours).cloned().collect();
        let request = |incoming: bool| {
            app.state()
                .friend_requests
                .iter()
                .filter(|(p, i)| *i == incoming && p.id.account == account())
                .map(|(p, _)| p.clone())
                .collect()
        };
        let (incoming, outgoing) = (request(true), request(false));
        Self {
            own_profile: demo_own_profile(),
            id: account(),
            names,
            servers,
            overviews,
            admin,
            friends,
            incoming,
            outgoing,
            sent: 0,
            reactions,
            call: None,
            muted: false,
            deafened: false,
            relationships: [(BLOCKED.to_owned(), godwit_core::members::Relationship::Blocked)]
                .into_iter()
                .collect(),
            presence: Presence::Online,
            status: Some(filler(41, 3).to_lowercase().into()),
        }
    }

    /// The two made-up people in the voice channel before we join.
    fn occupants() -> Vec<CallMember> {
        [("u1", 1, true, false), ("u2", 2, false, true)]
            .into_iter()
            .map(|(raw, n, speaking, muted)| CallMember {
                avatar_url: Some(format!("preview://avatar/{raw}").into()),
                ..call_member(raw, &person(n), speaking, muted)
            })
            .collect()
    }

    /// Who's in a call: two made-up people already there, and us once joined.
    fn call_members(&self, with_us: bool) -> Vec<CallMember> {
        let mut out = Self::occupants();
        if with_us {
            out.push(CallMember {
                avatar_url: Some("preview://avatar/me".into()),
                deafened: self.deafened,
                ..call_member("me", &me(), false, self.muted)
            });
        }
        out
    }

    /// The call's events after a change: who's in it, inside and out.
    fn call_news(&self, room: &RoomId) -> Vec<Event> {
        let members = self.call_members(true);
        vec![
            Event::CallMembers {
                room: room.clone(),
                members: members.clone(),
            },
            Event::VoiceOccupants {
                room: room.clone(),
                members,
            },
        ]
    }
}

impl DemoAccount {
    /// A profile card's friend button: requests go out (or accept theirs),
    /// removing cancels, declines or unfriends; the card shows the result.
    fn befriend(&mut self, user: UserId, add: bool) -> Vec<Event> {
        use godwit_core::account::NetworkAccount;
        use godwit_core::members::Relationship;
        let raw = user.raw.to_string();
        let now = self
            .relationships
            .get(&raw)
            .copied()
            .unwrap_or_else(|| relationship(&raw));
        let next = match (now, add) {
            (Relationship::Incoming, true) => Relationship::Friend,
            (_, true) => Relationship::Outgoing,
            (_, false) => Relationship::None,
        };
        self.relationships.insert(raw.clone(), next);
        // The lists, as a server's events would change them.
        let person = self
            .friends
            .iter()
            .chain(&self.incoming)
            .chain(&self.outgoing)
            .find(|p| p.id == user)
            .cloned()
            .unwrap_or_else(|| godwit_core::application::Person {
                id: user.clone(),
                name: self.names.get(&raw).cloned().unwrap_or_else(|| raw.clone()).into(),
                avatar_url: Some(format!("preview://avatar/{raw}").into()),
                online: true,
            });
        self.friends.retain(|p| p.id != user);
        self.incoming.retain(|p| p.id != user);
        self.outgoing.retain(|p| p.id != user);
        match next {
            Relationship::Friend => self.friends.push(person),
            Relationship::Outgoing => self.outgoing.push(person),
            _ => {}
        }
        let mut out = self.friend_lists();
        out.extend(self.run(godwit_core::application::Command::LoadProfile { user, room: None }));
        out
    }

    /// A profile card's Block / Unblock: the person leaves the friend lists
    /// when blocked and comes back as nobody; the card shows the result.
    fn block(&mut self, user: UserId, block: bool) -> Vec<Event> {
        use godwit_core::account::NetworkAccount;
        use godwit_core::members::Relationship;
        let raw = user.raw.to_string();
        let now = self
            .relationships
            .get(&raw)
            .copied()
            .unwrap_or_else(|| relationship(&raw));
        if block == (now == Relationship::Blocked) {
            return Vec::new();
        }
        self.relationships.insert(
            raw,
            if block {
                Relationship::Blocked
            } else {
                Relationship::None
            },
        );
        if block {
            self.friends.retain(|p| p.id != user);
            self.incoming.retain(|p| p.id != user);
            self.outgoing.retain(|p| p.id != user);
        }
        let mut out = self.friend_lists();
        out.push(Event::Blocked {
            account: self.id.clone(),
            users: self
                .relationships
                .iter()
                .filter(|(_, r)| **r == Relationship::Blocked)
                .map(|(raw, _)| UserId::new(self.id.clone(), raw.as_str()))
                .collect(),
        });
        out.extend(self.run(godwit_core::application::Command::LoadProfile { user, room: None }));
        out
    }

    /// The friends and requests as they stand, as the network sends them.
    fn friend_lists(&self) -> Vec<Event> {
        vec![
            Event::Friends {
                account: self.id.clone(),
                people: self.friends.clone(),
            },
            Event::FriendRequests {
                account: self.id.clone(),
                incoming: self.incoming.clone(),
                outgoing: self.outgoing.clone(),
            },
        ]
    }
}

impl DemoAccount {
    /// Server settings' pages, answered as Stoat would; only the first
    /// server is ours to manage, so refusals can be tried on the others.
    fn server_admin(
        &mut self,
        server: &ServerId,
        op: godwit_core::server_settings::ServerAdmin,
    ) -> Result<godwit_core::server_settings::ServerAdminResult, std::sync::Arc<str>> {
        use godwit_core::server_settings::{ServerAdmin as A, ServerAdminResult as R, ServerRole};
        let manage = self.overviews.get(server.raw.as_ref()).is_some_and(|o| o.can_manage);
        let Some(admin) = self.admin.get_mut(server.raw.as_ref()) else {
            return Err("This server is no longer available.".into());
        };
        let anyone = matches!(op, A::CreateInvite | A::MarkRead | A::SetNotify(_) | A::Leave);
        if !op.is_load() && !anyone && !manage {
            return Err("You need the Manage Server permission for this.".into());
        }
        Ok(match op {
            A::LoadInvites => R::Invites(admin.invites.clone()),
            A::LoadBans => R::Bans(admin.bans.clone()),
            A::LoadRoles => R::Roles(admin.roles.clone()),
            A::LoadEmoji => R::Emoji(admin.emoji.clone()),
            A::CreateEmoji { name, file } => {
                admin.emoji.check_new(name.trim(), file.size)?;
                let n = admin.emoji.emoji.len();
                admin.emoji.emoji.insert(
                    0,
                    godwit_core::server_settings::ServerEmoji {
                        id: format!("E{}", n + 100).into(),
                        name: name.trim().into(),
                        url: Some(format!("preview://emoji/{}", n % 4).into()),
                        creator: "You".into(),
                    },
                );
                R::Emoji(admin.emoji.clone())
            }
            A::RenameEmoji { id, name } => {
                admin.emoji.rules.validate_name(name.trim())?;
                let Some(e) = admin.emoji.emoji.iter_mut().find(|e| e.id == id) else {
                    return Err("That emoji is no longer here.".into());
                };
                e.name = name.trim().into();
                R::Emoji(admin.emoji.clone())
            }
            A::DeleteEmoji(id) => {
                admin.emoji.emoji.retain(|e| e.id != id);
                R::Emoji(admin.emoji.clone())
            }
            A::DeleteInvite(code) => {
                admin.invites.retain(|i| i.code != code);
                R::Invites(admin.invites.clone())
            }
            A::Unban(user) => {
                admin.bans.retain(|b| b.user != user);
                R::Bans(admin.bans.clone())
            }
            A::CreateRole(name) => {
                let id = format!("R{}", admin.roles.roles.len() + 100);
                admin.roles.roles.push(ServerRole {
                    id: id.into(),
                    name: name.trim().into(),
                    colour: None,
                    hoist: false,
                    allow: 0,
                    deny: 0,
                });
                R::Roles(admin.roles.clone())
            }
            A::EditRole(edit) => {
                let Some(role) = admin.roles.roles.iter_mut().find(|r| r.id == edit.id) else {
                    return Err("That role no longer exists.".into());
                };
                if let Some(name) = edit.name {
                    role.name = name.trim().into();
                }
                if let Some(colour) = edit.colour {
                    role.colour = colour;
                }
                if let Some(hoist) = edit.hoist {
                    role.hoist = hoist;
                }
                if let Some((allow, deny)) = edit.permissions {
                    (role.allow, role.deny) = (allow, deny);
                }
                R::Roles(admin.roles.clone())
            }
            A::DeleteRole(id) => {
                admin.roles.roles.retain(|r| r.id != id);
                R::Roles(admin.roles.clone())
            }
            A::SetDefaultPermissions(allow) => {
                admin.roles.default_allow = allow;
                R::Roles(admin.roles.clone())
            }
            A::Kick(_) | A::Ban { .. } => R::Done("Done.".into()),
            A::CreateInvite => R::InviteCreated(format!("https://stt.gg/{}", Bogus::new(77).word()).into()),
            A::MarkRead => R::Done("Marked as read.".into()),
            A::SetNotify(_) => R::Done("Notifications changed.".into()),
            A::CreateChannel { .. } => R::Done("Channel created.".into()),
            // Leaving would leave the demo without its server, as deleting would.
            A::Leave => return Err("The demo server can't be left.".into()),
            // Deleting would leave the demo without its server.
            A::DeleteServer => return Err("The demo server can't be deleted.".into()),
        })
    }

    /// Applies an overview edit the way the server would: refused without
    /// Manage Server, otherwise the overview and the rail entry both change.
    fn save_overview(
        &mut self,
        server: &ServerId,
        edit: godwit_core::server_settings::ServerOverviewEdit,
    ) -> Result<godwit_core::server_settings::ServerOverview, std::sync::Arc<str>> {
        use godwit_core::own_profile::ProfileImage;
        let Some(o) = self.overviews.get_mut(server.raw.as_ref()) else {
            return Err("This server is no longer available.".into());
        };
        if !o.can_manage {
            return Err("You no longer have permission to manage this server.".into());
        }
        if let Some(name) = edit.name {
            o.name = name.trim().into();
        }
        if let Some(description) = edit.description {
            o.description = description;
        }
        for (choice, slot, url) in [
            (edit.icon, &mut o.icon_url, "preview://server"),
            (edit.banner, &mut o.banner_url, "preview://photo"),
        ] {
            match choice {
                ProfileImage::Keep => {}
                ProfileImage::Remove => *slot = None,
                ProfileImage::File { .. } => *slot = Some(url.into()),
            }
        }
        if let Some(channels) = edit.system_channels {
            o.system_channels = channels;
        }
        if let Some(s) = self.servers.iter_mut().find(|s| &s.id == server) {
            s.name = o.name.clone();
            s.icon_url = o.icon_url.clone();
        }
        Ok(o.clone())
    }
}

impl godwit_core::account::NetworkAccount for DemoAccount {
    fn can_edit_profile(&self) -> bool {
        true
    }
    fn supports_invites(&self) -> bool {
        true
    }

    fn parse_invite(&self, input: &str) -> Option<String> {
        godwit_stoat::account::invite_code(input)
    }

    fn network(&self) -> Network {
        Network::Stoat
    }

    fn id(&self) -> Option<&AccountId> {
        Some(&self.id)
    }

    fn poll(&mut self) -> Vec<Event> {
        Vec::new()
    }

    fn run(&mut self, command: godwit_core::application::Command) -> Vec<Event> {
        use godwit_core::application::Command;
        match command {
            Command::ServerAdmin { server, op } => {
                use godwit_core::server_settings::ServerAdminResult as R;
                let change = !op.is_load();
                let result = self.server_admin(&server, op);
                // A change answers with the new list, then its outcome, as Stoat does.
                let listed = matches!(result, Ok(R::Invites(_) | R::Bans(_) | R::Roles(_) | R::Emoji(_)));
                let mut out = vec![Event::ServerAdmin {
                    server: server.clone(),
                    result,
                }];
                if change && listed {
                    out.push(Event::ServerAdmin {
                        server,
                        result: Ok(R::Done("Saved.".into())),
                    });
                }
                out
            }
            Command::LoadServerOverview { server, request } => {
                let result = self
                    .overviews
                    .get(server.raw.as_ref())
                    .cloned()
                    .ok_or_else(|| "This server is no longer available.".into());
                vec![Event::ServerOverview {
                    server,
                    request,
                    result,
                }]
            }
            Command::SaveServerOverview { server, request, edit } => {
                let result = self.save_overview(&server, *edit);
                let mut out = Vec::new();
                if result.is_ok() {
                    out.push(Event::ServersChanged {
                        account: self.id.clone(),
                        servers: self.servers.clone(),
                    });
                }
                out.push(Event::ServerOverview {
                    server,
                    request,
                    result,
                });
                out
            }
            Command::LoadOwnProfile => vec![Event::OwnProfileLoaded {
                account: self.id.clone(),
                result: Ok(self.own_profile.clone()),
            }],
            Command::SaveOwnProfile(edit) => {
                use godwit_core::own_profile::ProfileImage;
                if let Some(name) = edit.display_name {
                    self.own_profile.display_name = name.trim().into();
                    self.own_profile.name = if name.trim().is_empty() {
                        me().into()
                    } else {
                        name.trim().into()
                    };
                }
                if let Some(pronouns) = edit.pronouns {
                    self.own_profile.pronouns = pronouns.trim().into();
                }
                if let Some(bio) = edit.bio {
                    self.own_profile.bio = bio;
                }
                for (choice, slot) in [
                    (edit.avatar, &mut self.own_profile.avatar_url),
                    (edit.banner, &mut self.own_profile.banner_url),
                ] {
                    match choice {
                        ProfileImage::Keep => {}
                        ProfileImage::Remove => *slot = None,
                        ProfileImage::File { .. } => *slot = Some("preview://photo".into()),
                    }
                }
                vec![Event::OwnProfileSaved {
                    account: self.id.clone(),
                    result: Ok(self.own_profile.clone()),
                }]
            }
            Command::SendMessage {
                room,
                text,
                nonce,
                reply_to,
                files,
            } => {
                self.sent += 1;
                let mut m = msg(&format!("demo-{}", self.sent), "me", &me(), 0, &text);
                m.sent_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0));
                m.reply_to = reply_to;
                m.attachments = files
                    .iter()
                    .map(|f| Attachment {
                        name: f.name.clone(),
                        url: "preview://file".into(),
                        kind: AttachmentKind::File,
                        size: f.size,
                        dimensions: None,
                        original_url: None,
                    })
                    .collect();
                vec![Event::MessageConfirmed {
                    room,
                    nonce,
                    message: m,
                }]
            }
            Command::EditMessage { room, message, text } => vec![Event::MessageEdited {
                room,
                id: message,
                text: text.as_str().into(),
                markup: Markup::Rich,
                source: text.as_str().into(),
            }],
            Command::DeleteMessage { room, message } => vec![Event::MessageDeleted { room, id: message }],
            // Every room's messages are there from the start.
            Command::LoadHistory { room, before } => {
                vec![Event::History {
                    room,
                    before,
                    messages: Vec::new(),
                    reached_start: true,
                }]
            }
            Command::SetPresence(presence) => {
                self.presence = presence;
                vec![Event::OwnPresence {
                    account: self.id.clone(),
                    presence,
                    status: self.status.clone(),
                }]
            }
            Command::SetStatusText(text) => {
                self.status = text.map(Into::into);
                vec![Event::OwnPresence {
                    account: self.id.clone(),
                    presence: self.presence,
                    status: self.status.clone(),
                }]
            }
            Command::JoinCall { room } => {
                self.call = Some(room.clone());
                let mut out = vec![Event::CallConnected { room: room.clone() }];
                out.extend(self.call_news(&room));
                out
            }
            Command::LeaveCall => {
                let Some(room) = self.call.take() else {
                    return Vec::new();
                };
                vec![
                    Event::CallEnded {
                        room: room.clone(),
                        reason: None,
                    },
                    Event::VoiceOccupants {
                        room,
                        members: self.call_members(false),
                    },
                ]
            }
            Command::SetDeafened(deafened) => {
                self.deafened = deafened;
                self.call.clone().map(|room| self.call_news(&room)).unwrap_or_default()
            }
            Command::StartScreenShare { .. } => self
                .call
                .clone()
                .map(|room| vec![Event::CallScreenShare { room, active: true }])
                .unwrap_or_default(),
            Command::SetCamera(active) => self
                .call
                .clone()
                .map(|room| vec![Event::CallCamera { room, active }])
                .unwrap_or_default(),
            Command::SetScreenShare(active) => self
                .call
                .clone()
                .map(|room| vec![Event::CallScreenShare { room, active }])
                .unwrap_or_default(),
            Command::SetMuted(muted) => {
                self.muted = muted;
                self.call.clone().map(|room| self.call_news(&room)).unwrap_or_default()
            }
            Command::React {
                room,
                message,
                emoji,
                add,
            } => {
                let list = self.reactions.entry(message.0.to_string()).or_default();
                match (list.iter().position(|r| r.emoji == emoji), add) {
                    (Some(i), true) if !list[i].me => {
                        list[i].count += 1;
                        list[i].me = true;
                    }
                    (Some(i), false) if list[i].me => {
                        list[i].count -= 1;
                        list[i].me = false;
                        if list[i].count == 0 {
                            list.remove(i);
                        }
                    }
                    (None, true) => list.push(Reaction {
                        emoji,
                        image_url: None,
                        count: 1,
                        me: true,
                    }),
                    _ => {}
                }
                vec![Event::ReactionsChanged {
                    room,
                    id: message,
                    reactions: list.clone(),
                }]
            }
            // The made-up people's direct chats are `dm{n}` for `u{n}`.
            Command::OpenDirect { user } => match user.raw.strip_prefix('u').and_then(|n| n.parse::<usize>().ok()) {
                Some(n) if n < 14 => vec![Event::RoomOpened(RoomId::new(self.id.clone(), format!("dm{n}")))],
                _ => Vec::new(),
            },
            Command::CreateGroup { name, .. } => {
                let id = format!("group-{}", self.sent);
                vec![
                    Event::RoomUpdated(room(&id, &name, RoomKind::Group, false, 0)),
                    Event::RoomOpened(RoomId::new(self.id.clone(), id)),
                ]
            }
            // Any room's members: a few made-up people, seeded by the room.
            Command::LoadMembers { room } => {
                let seed = room.raw.bytes().map(u64::from).sum::<u64>();
                let mut b = Bogus::new(seed);
                let online: Vec<Member> = (0..3)
                    .map(|i| {
                        let n = (seed as usize + i) % 14;
                        member(
                            &format!("u{n}"),
                            &person(n),
                            Presence::Online,
                            Some(&b.sentence(2).to_lowercase()),
                            None,
                        )
                    })
                    .collect();
                let n = (seed as usize + 5) % 14;
                vec![Event::Members {
                    room,
                    groups: vec![
                        MemberGroup {
                            title: format!("Online — {}", online.len()).into(),
                            members: online,
                        },
                        MemberGroup {
                            title: "Offline — 1".into(),
                            members: vec![member(&format!("u{n}"), &person(n), Presence::Offline, None, None)],
                        },
                    ],
                }]
            }
            // DemoMine leads to a server we're in; DemoJoin to S3, which
            // "joining" opens.
            Command::LoadInvite { code } => vec![Event::InviteLoaded {
                account: self.id.clone(),
                preview: Some(invite_preview(&code)),
                code,
            }],
            Command::JoinInvite { code } => vec![Event::InviteJoined {
                account: self.id.clone(),
                code,
                server: ServerId::new(self.id.clone(), "S3"),
            }],
            // Names with a tag are "found"; anything else isn't, to try both.
            Command::SendFriendRequest { request, name } => {
                let Some((shown, _)) = name.trim().split_once('#').filter(|(n, _)| !n.is_empty()) else {
                    return vec![Event::FriendRequestSent {
                        account: self.id.clone(),
                        request,
                        result: Err("no one on Stoat goes by that name (it looks like name#1234)".into()),
                    }];
                };
                // Someone new, waiting in Sent requests.
                let raw = format!("req-{}", name.trim());
                if !self.outgoing.iter().chain(&self.friends).any(|p| *p.id.raw == *raw) {
                    self.outgoing.push(godwit_core::application::Person {
                        id: user(&raw),
                        name: shown.into(),
                        avatar_url: None,
                        online: false,
                    });
                }
                let mut out = vec![Event::FriendRequestSent {
                    account: self.id.clone(),
                    request,
                    result: Ok(()),
                }];
                out.extend(self.friend_lists());
                out
            }
            // Nobody reads demo reports: say it went through.
            Command::Report { .. } => vec![Event::Reported {
                account: self.id.clone(),
                result: Ok(()),
            }],
            Command::AddFriend { user } => self.befriend(user, true),
            Command::RemoveFriend { user } => self.befriend(user, false),
            Command::BlockUser { user } => self.block(user, true),
            Command::UnblockUser { user } => self.block(user, false),
            Command::LoadProfile { user, room } => {
                let raw = user.raw.to_string();
                // Seen in a channel of a server we manage: the card offers Kick and Ban.
                let moderation = room
                    .and_then(|room| {
                        self.overviews
                            .iter()
                            .find(|(_, o)| o.can_manage && o.channels.iter().any(|(r, _)| *r == room))
                    })
                    .filter(|_| raw != "me")
                    .map(|(server, _)| godwit_core::members::Moderation {
                        server: ServerId::new(account(), server.as_str()),
                        kick: true,
                        ban: true,
                    });
                let name = self.names.get(&raw).cloned().unwrap_or_else(|| raw.clone());
                let relationship = self
                    .relationships
                    .get(&raw)
                    .copied()
                    .unwrap_or_else(|| relationship(&raw));
                vec![Event::Profile(godwit_core::members::Profile {
                    relationship,
                    can_block: raw != "me",
                    moderation,
                    ..profile(user, &name)
                })]
            }
            _ => Vec::new(),
        }
    }

    fn message_link(&self, _: &RoomId, _: &MessageId) -> Option<String> {
        None
    }

    fn log_out(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use godwit_core::account::NetworkAccount;
    use godwit_core::application::{Action, Command, Effect};

    fn feed(app: &mut Application, demo: &mut DemoAccount, command: Command) {
        for event in demo.run(command) {
            app.apply(event);
        }
    }

    fn run(app: &mut Application, demo: &mut DemoAccount, action: Action) {
        for effect in app.dispatch(action) {
            if let Effect::Backend { command, .. } = effect {
                feed(app, demo, command);
            }
        }
    }

    #[test]
    fn shared_profile_and_call_fixtures_keep_their_defaults() {
        let p = profile(user("u0"), "Test Person");
        assert_eq!(&*p.username, "testperson#0001");
        assert_eq!(p.avatar_url.as_deref(), Some("preview://avatar/u0"));
        assert_eq!(p.presence, Presence::Online);
        assert!(!p.me && !p.bot && p.has_friends);
        let members = DemoAccount::occupants();
        assert_eq!(
            members.iter().map(|m| m.user.raw.as_ref()).collect::<Vec<_>>(),
            ["u1", "u2"]
        );
        assert!(members[0].speaking && !members[0].muted);
        assert!(members[1].muted && members[1].deafened);
        assert!(members.iter().all(|m| m.avatar_url.is_some()));
        assert!(call_member("u1", "Test Person", true, false).avatar_url.is_none());
    }

    #[test]
    fn filler_is_made_up_and_the_same_every_run() {
        assert_eq!(person(3), person(3));
        assert_ne!(person(0), person(1));
        assert!(person(0).chars().all(|c| c.is_ascii_alphabetic() || c == ' '));
        assert_eq!(filler(5, 4).split(' ').count(), 4);
    }

    #[test]
    fn demo_world_has_a_shared_thumbnail_embed() {
        let (app, general, _) = world(Contacts::default());
        let timeline = app.state().timelines.get(&general).expect("demo timeline exists");
        let embed = timeline
            .messages
            .iter()
            .flat_map(|m| &m.embeds)
            .next()
            .expect("demo embed exists");
        assert_eq!(embed.title.as_deref(), Some("A landscape from the demo"));
        assert!(embed.image_thumbnail);
        assert_eq!(embed.image_url.as_deref(), Some("preview://photo"));
    }

    #[test]
    fn demo_server_overview_loads_saves_and_refuses_the_unmanaged_one() {
        let (mut app, _, _) = world(Contacts::default());
        let mut demo = DemoAccount::new(&app);
        let ours = ServerId::new(account(), "S");
        run(&mut app, &mut demo, Action::OpenServerSettings(ours.clone()));
        let s = app.state().server_settings.as_ref().unwrap();
        let loaded = s.overview.clone().unwrap();
        assert!(loaded.can_manage && !s.busy);
        assert_eq!(loaded.channels.len(), 4);
        let edit = godwit_core::server_settings::ServerOverviewEdit {
            name: Some(" Zibble ".into()),
            icon: godwit_core::own_profile::ProfileImage::Remove,
            system_channels: Some([None, None, Some("dev".into()), None]),
            ..Default::default()
        };
        run(&mut app, &mut demo, Action::SaveServerOverview(edit));
        let s = app.state().server_settings.as_ref().unwrap();
        assert!(s.saved && s.error.is_none());
        let saved = s.overview.clone().unwrap();
        assert_eq!(saved.name.as_ref(), "Zibble");
        assert_eq!(saved.icon_url, None);
        assert_eq!(saved.system_channels[2].as_deref(), Some("dev"));
        let rail = app.state().server(&ours).unwrap();
        assert_eq!(
            (rail.name.as_ref(), rail.icon_url.as_deref()),
            ("Zibble", None),
            "the rail follows the save"
        );

        let theirs = ServerId::new(account(), "S2");
        run(&mut app, &mut demo, Action::OpenServerSettings(theirs.clone()));
        let s = app.state().server_settings.as_ref().unwrap();
        assert!(!s.overview.as_ref().unwrap().can_manage);
        // Straight to the account, as a stale UI could: it still refuses.
        let refused = demo.run(Command::SaveServerOverview {
            server: theirs,
            request: 0,
            edit: Default::default(),
        });
        assert!(matches!(
            refused.as_slice(),
            [Event::ServerOverview { result: Err(_), .. }]
        ));
    }

    #[test]
    fn demo_roles_can_be_created_edited_and_deleted() {
        use godwit_core::server_settings::{RoleEdit, ServerAdmin, ServerPage};
        let (mut app, _, _) = world(Contacts::default());
        let mut demo = DemoAccount::new(&app);
        let server = ServerId::new(account(), "S");
        run(&mut app, &mut demo, Action::OpenServerSettings(server.clone()));
        run(&mut app, &mut demo, Action::ServerSettingsPage(ServerPage::Roles));
        fn app_roles(app: &Application) -> Vec<String> {
            let s = app.state().server_settings.as_ref().unwrap();
            s.roles
                .as_ref()
                .unwrap()
                .roles
                .iter()
                .map(|r| r.name.to_string())
                .collect()
        }
        let before = app_roles(&app).len();
        let admin = |op| Action::ServerAdmin {
            server: server.clone(),
            op,
        };
        run(&mut app, &mut demo, admin(ServerAdmin::CreateRole(" Flurb ".into())));
        assert_eq!(app_roles(&app).len(), before + 1);
        assert!(
            !app.state().server_settings.as_ref().unwrap().admin_busy,
            "the change finished"
        );
        let id = app
            .state()
            .server_settings
            .as_ref()
            .unwrap()
            .roles
            .as_ref()
            .unwrap()
            .roles[before]
            .id
            .clone();
        let edit = RoleEdit {
            id: id.clone(),
            name: Some("Glonk".into()),
            colour: Some(Some("#123456".into())),
            permissions: Some((4, 0)),
            ..Default::default()
        };
        run(&mut app, &mut demo, admin(ServerAdmin::EditRole(edit)));
        let s = app.state().server_settings.as_ref().unwrap();
        let role = &s.roles.as_ref().unwrap().roles[before];
        assert_eq!(
            (role.name.as_ref(), role.colour.as_deref(), role.allow),
            ("Glonk", Some("#123456"), 4)
        );
        run(&mut app, &mut demo, admin(ServerAdmin::DeleteRole(id)));
        assert_eq!(app_roles(&app).len(), before);
    }

    #[test]
    fn demo_blocking_updates_the_card_and_the_blocked_list() {
        let (mut app, _, _) = world(Contacts::default());
        let mut demo = DemoAccount::new(&app);
        let (friend, stranger) = (user("u0"), user(BLOCKED));
        assert!(app.state().blocked.contains(&stranger), "starts blocked");
        feed(&mut app, &mut demo, Command::UnblockUser { user: stranger.clone() });
        assert!(app.state().blocked.is_empty());
        app.dispatch(Action::OpenProfile(friend.clone()));
        feed(&mut app, &mut demo, Command::BlockUser { user: friend.clone() });
        assert!(app.state().blocked.contains(&friend));
        assert!(
            !app.state().friends.iter().any(|p| p.id == friend),
            "no longer a friend"
        );
        let shown = app.state().profile.clone();
        assert!(shown.is_some_and(|(u, p)| u == friend
            && p.is_some_and(|p| p.relationship == godwit_core::members::Relationship::Blocked && p.can_block)));
    }

    #[test]
    fn demo_emoji_can_be_added_renamed_and_deleted() {
        use godwit_core::server_settings::{EmojiFile, ServerAdmin, ServerPage};
        let (mut app, _, _) = world(Contacts::default());
        let mut demo = DemoAccount::new(&app);
        let server = ServerId::new(account(), "S");
        run(&mut app, &mut demo, Action::OpenServerSettings(server.clone()));
        run(&mut app, &mut demo, Action::ServerSettingsPage(ServerPage::Emoji));
        let names = |app: &Application| -> Vec<String> {
            let s = app.state().server_settings.as_ref().unwrap();
            s.emoji
                .as_ref()
                .unwrap()
                .emoji
                .iter()
                .map(|e| e.name.to_string())
                .collect()
        };
        let before = names(&app).len();
        assert!(before > 0);
        let admin = |op| Action::ServerAdmin {
            server: server.clone(),
            op,
        };
        let file = EmojiFile {
            path: "/demo/blorp.png".into(),
            size: 1000,
        };
        // A bad name is refused by the core's rules and nothing changes.
        run(
            &mut app,
            &mut demo,
            admin(ServerAdmin::CreateEmoji {
                name: "Bad Name".into(),
                file: file.clone(),
            }),
        );
        assert_eq!(names(&app).len(), before);
        assert!(matches!(
            app.state().server_settings.as_ref().unwrap().notice,
            Some(Err(_))
        ));
        run(
            &mut app,
            &mut demo,
            admin(ServerAdmin::CreateEmoji {
                name: " blorp ".into(),
                file,
            }),
        );
        assert_eq!(names(&app)[0], "blorp");
        assert!(!app.state().server_settings.as_ref().unwrap().admin_busy);
        let id = app
            .state()
            .server_settings
            .as_ref()
            .unwrap()
            .emoji
            .as_ref()
            .unwrap()
            .emoji[0]
            .id
            .clone();
        run(
            &mut app,
            &mut demo,
            admin(ServerAdmin::RenameEmoji {
                id: id.clone(),
                name: "glonk".into(),
            }),
        );
        assert_eq!(names(&app)[0], "glonk");
        run(&mut app, &mut demo, admin(ServerAdmin::DeleteEmoji(id)));
        assert_eq!(names(&app).len(), before);
    }

    #[test]
    fn demo_friend_requests_move_between_lists() {
        use godwit_core::application::Person;
        let (mut app, _, _) = world(Contacts::default());
        let mut demo = DemoAccount::new(&app);
        let theirs: Vec<Person> = app
            .state()
            .friend_requests
            .iter()
            .filter(|(_, i)| *i)
            .map(|(p, _)| p.clone())
            .collect();
        let ours: Vec<Person> = app
            .state()
            .friend_requests
            .iter()
            .filter(|(_, i)| !*i)
            .map(|(p, _)| p.clone())
            .collect();
        let friends = app.state().friends.iter().filter(|p| p.id.account == account()).count();
        feed(
            &mut app,
            &mut demo,
            Command::AddFriend {
                user: theirs[0].id.clone(),
            },
        );
        assert!(
            app.state().friends.iter().any(|p| p.id == theirs[0].id),
            "accepted: a friend now"
        );
        assert_eq!(
            app.state().friends.iter().filter(|p| p.id.account == account()).count(),
            friends + 1
        );
        feed(
            &mut app,
            &mut demo,
            Command::RemoveFriend {
                user: ours[0].id.clone(),
            },
        );
        assert!(
            !app.state().friend_requests.iter().any(|(p, _)| p.id == ours[0].id),
            "cancelled: gone"
        );
        feed(
            &mut app,
            &mut demo,
            Command::SendFriendRequest {
                request: 1,
                name: "zorp#0001".into(),
            },
        );
        assert!(
            app.state()
                .friend_requests
                .iter()
                .any(|(p, i)| !*i && &*p.name == "zorp")
        );
    }

    #[test]
    fn reports_are_gated_and_answered() {
        use godwit_core::report::{ReportReason, ReportTarget};
        let (mut app, general, _) = world(Contacts::default());
        let mut demo = DemoAccount::new(&app);
        app.dispatch(Action::SelectRoom(general.clone()));
        let message = MessageId("m1".into());
        let report = |reason| Action::Report {
            target: ReportTarget::Message {
                message: message.clone(),
                room: general.clone(),
            },
            reason,
            detail: "spam".into(),
        };
        // A user-only reason is refused for a message, with an outcome.
        let effects = app.dispatch(report(ReportReason::Underage));
        assert!(effects.is_empty());
        assert!(matches!(app.state().report_outcome, Some(Err(_))));
        for effect in app.dispatch(report(ReportReason::ScamsFraud)) {
            if let Effect::Backend { command, .. } = effect {
                feed(&mut app, &mut demo, command);
            }
        }
        assert_eq!(app.state().report_outcome, Some(Ok(())));
    }

    #[test]
    fn the_demo_world_answers_like_a_server() {
        let (mut app, general, _) = world(Contacts::default());
        let mut net = DemoAccount::new(&app);
        app.dispatch(Action::SelectRoom(general.clone()));
        app.dispatch(Action::SetDraft("hello demo".into()));
        let mut confirmed = false;
        for effect in app.dispatch(Action::Send { now_ms: 1 }) {
            if let Effect::Backend {
                command: c @ Command::SendMessage { .. },
                ..
            } = effect
            {
                for event in net.run(c) {
                    confirmed |= matches!(&event, Event::MessageConfirmed { .. });
                    app.apply(event);
                }
            }
        }
        assert!(confirmed);
        let last = app.state().timelines[&general].messages.last().cloned();
        assert!(last.is_some_and(|m| &*m.text == "hello demo" && !m.pending));
        let profile = net.run(Command::LoadProfile {
            user: user("u0"),
            room: None,
        });
        assert!(matches!(&profile[..], [Event::Profile(p)] if p.name.as_ref() == person(0)));

        // A call: join, mute, leave, as the real core drives it.
        let lounge = RoomId::new(account(), "voice");
        let mut run = |app: &mut Application, action: Action| {
            for effect in app.dispatch(action) {
                if let Effect::Backend { command, .. } = effect {
                    for event in net.run(command) {
                        app.apply(event);
                    }
                }
            }
        };
        run(&mut app, Action::JoinCall(lounge));
        assert!(
            app.state().call.as_ref().is_some_and(|c| c.members.len() == 3),
            "us and the two already there"
        );
        run(&mut app, Action::ToggleMute);
        assert!(
            app.state()
                .call
                .as_ref()
                .is_some_and(|c| c.members.iter().any(|m| m.user == user("me") && m.muted))
        );
        run(&mut app, Action::LeaveCall);
        assert!(app.state().call.is_none());
        // Toggling a reaction on the reply ("08" has 👍 ×3, ours).
        app.dispatch(Action::SelectRoom(general.clone()));
        run(
            &mut app,
            Action::ToggleReaction {
                message: MessageId("08".into()),
                emoji: "👍".into(),
            },
        );
        let thumbs = app.state().timelines[&general]
            .messages
            .iter()
            .find(|m| &*m.id.0 == "08")
            .map(|m| m.reactions[0].count);
        assert_eq!(thumbs, Some(2), "ours taken back");
    }
}
