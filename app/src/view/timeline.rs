// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Timeline keys, message blocks, embeds and incremental row updates.

use super::*;
use std::borrow::Cow;
use std::collections::HashMap;

/// Messages from the same author within this gap share one header.
const GROUP_GAP_MS: i64 = 7 * 60 * 1000;
/// A message's local day, as the divider labels it ("October 26, 2026").
fn day(ms: i64) -> Option<String> {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%B %-d, %Y").to_string())
}

pub(crate) fn time(ms: i64) -> String {
    let Some(at) = Local.timestamp_millis_opt(ms).single() else {
        return String::new();
    };
    let now: DateTime<Local> = Local::now();
    if at.date_naive() == now.date_naive() {
        at.format("Today at %H:%M").to_string()
    } else {
        at.format("%d/%m/%Y %H:%M").to_string()
    }
}

/// What a click on a collapsed run of hidden messages sends as its link
/// (timeline.slint repeats it: Slint can't share the constant).
pub const REVEAL_HIDDEN: &str = "godwit://reveal-hidden";

/// What a timeline row depends on. Rows whose key didn't change aren't
/// rebuilt, which keeps hover, scrolling and sub-models intact.
#[derive(Clone)]
pub struct RowKey {
    pub(crate) message: Arc<Message>,
    /// Players for links in its text that the network sent no embed for
    /// (music.youtube.com, Spotify): see [`link_embeds`].
    links: Vec<Embed>,
    /// The width its text is laid out in (textlayout.rs), in pixels.
    width: u32,
    first: bool,
    /// The day divider's label above it, if the day changed.
    date: Option<String>,
    /// The "NEW" divider goes above it.
    new: bool,
    revealed: bool,
    /// Above zero: this row stands for a run of hidden messages.
    hidden_run: u32,
    ignored_run: bool,
    reply: Option<Arc<Message>>,
    /// Every picture the row shows was ready when it was built.
    complete: bool,
    /// The jump number when this row is the jump's target (0 otherwise).
    jump: i32,
    /// Near the view: its pictures are shown (and fetched). Rows further
    /// off have none, so they're neither downloaded nor kept decoded.
    wanted: bool,
    /// Re-entering the playback margin must resolve animation slots again:
    /// a prefetched/offscreen animation may have left the bounded cache.
    animation_live: bool,
    /// Its invite cards' states when built (None: not asked for yet), and
    /// whether we're in each one's server.
    invites: Vec<(Option<Invite>, bool)>,
    /// Animation slots the row shows (anim.rs).
    slots: Vec<usize>,
}

fn animation_near(i: usize, window: Option<(usize, usize)>) -> bool {
    window.is_some_and(|(top, bottom)| i.saturating_add(2) >= top && i <= bottom.saturating_add(2))
}

/// The animation slots of rows near the view: these play (anim.rs).
pub fn live_slots(rows: &[RowKey], window: Option<(usize, usize)>) -> std::collections::HashSet<usize> {
    rows.iter()
        .enumerate()
        .filter(|(i, _)| animation_near(*i, window))
        .flat_map(|(_, r)| r.slots.iter().copied())
        .collect()
}

impl RowKey {
    /// It starts a group (shows its author).
    pub fn is_first(&self) -> bool {
        self.first
    }

    pub fn id(&self) -> &MessageId {
        &self.message.id
    }

    /// Rebuild this row on the next refresh: its inline player started or
    /// stopped, which its message alone doesn't show.
    pub fn invalidate(&mut self) {
        self.complete = false;
    }

    /// The invite codes the row shows, with the account to ask.
    pub fn invite_codes(&self) -> impl Iterator<Item = (&godwit_core::ids::AccountId, &Arc<str>)> {
        self.message
            .invites
            .iter()
            .map(|code| (&self.message.author.account, code))
    }

    /// The join links of the row's activity invites (a game invite's).
    pub fn activity_links(&self) -> impl Iterator<Item = &str> {
        self.message
            .embeds
            .iter()
            .filter(|e| e.style == EmbedStyle::Activity)
            .filter_map(|e| e.url.as_deref())
    }

    pub fn player_urls(&self) -> impl Iterator<Item = &str> {
        self.players().map(|(url, _, _)| url)
    }

    /// The embeds the row shows: the network's, then players for links it
    /// sent none for. A network card for such a link without a player gives
    /// way to the player (the same link twice would look doubled).
    fn shown_embeds(&self) -> impl Iterator<Item = &Embed> {
        self.message
            .embeds
            .iter()
            .filter(|e| e.player_url.is_some() || !self.links.iter().any(|l| l.url.is_some() && l.url == e.url))
            .chain(&self.links)
    }

    /// The row's players: each playable embed's URL, kind and media size,
    /// then its video files (played as the official `<video controls>`).
    /// GIFs with a picture animate by themselves; ones that are only a video
    /// play it looping (PlayerKind::Gif).
    fn players(&self) -> impl Iterator<Item = (&str, PlayerKind, (u32, u32))> {
        let embeds = self
            .shown_embeds()
            .filter(|e| e.style != EmbedStyle::Gif || e.image_url.is_none())
            .filter_map(|embed| {
                let url = embed.player_url.as_deref().or(embed.video_url.as_deref())?;
                let kind = if embed.player_url.is_some() {
                    PlayerKind::Embed
                } else if embed.style == EmbedStyle::Gif {
                    PlayerKind::Gif
                } else {
                    PlayerKind::Video
                };
                Some((url, kind, embed_media_size(embed)))
            });
        let videos =
            self.message.attachments.iter().filter_map(|a| {
                video_embed(a).map(|embed| (a.url.as_ref(), PlayerKind::Video, embed_media_size(&embed)))
            });
        embeds.chain(videos)
    }

    /// What loading the row's players needs: URL, kind and size on screen.
    pub fn player_specs(&self) -> impl Iterator<Item = PlayerSpec> + '_ {
        self.players().map(|(url, kind, (width, height))| PlayerSpec {
            url: url.to_owned(),
            kind,
            width,
            height,
        })
    }

    fn same(&self, other: &Self, images_changed: bool) -> bool {
        Arc::ptr_eq(&self.message, &other.message)
            && self.first == other.first
            && self.date == other.date
            && self.new == other.new
            && self.jump == other.jump
            && self.revealed == other.revealed
            && self.hidden_run == other.hidden_run
            && self.ignored_run == other.ignored_run
            && self.wanted == other.wanted
            && self.animation_live == other.animation_live
            && self.width == other.width
            && self.invites == other.invites
            && match (&self.reply, &other.reply) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
            && (other.complete || !images_changed)
    }
}

/// Rows either side of the view that get pictures too (Yapfast's margin).
const MARGIN: usize = 10;
/// Rows keep their pictures until this far from the view.
const KEEP_MARGIN: usize = 20;
/// Rows given pictures before the list reports where it is.
const OPENING_ROWS: usize = 20;

/// The message the "NEW" divider goes above: the first one after `read` that
/// isn't ours (as Godwit), if `read` is loaded.
fn new_divider_at(list: &[Arc<Message>], read: Option<&MessageId>) -> Option<usize> {
    let at = list.iter().position(|m| Some(&m.id) == read)?;
    list.iter()
        .enumerate()
        .skip(at + 1)
        .find(|(_, m)| m.author.raw != m.author.account.user)
        .map(|(i, _)| i)
}

/// The timeline with blocked/ignored runs folded (see [`collapse_hidden`]).
pub struct Collapsed<'a> {
    /// The messages to show rows for.
    pub list: Cow<'a, [Arc<Message>]>,
    /// Each folded run by its first message (the one left in `list`): every
    /// message of the run, that one included.
    pub runs: HashMap<MessageId, Vec<MessageId>>,
    /// Run starts whose hidden messages were ignored rather than blocked.
    pub ignored: HashSet<MessageId>,
}

/// Folds each run of consecutive messages from blocked or ignored people into
/// its first message, like the upstream `BlockedMessage.tsx` row
/// (`packages/client/components/ui/components/messaging/elements/
/// BlockedMessage.tsx`). The run label preserves the kind.
/// Messages in `revealed` stay shown. The messages
/// the NEW divider and the jump point at are always shown (the run splits
/// around them), or their markers would have nothing to sit on. Nothing is
/// copied when no message is folded.
pub fn collapse_hidden<'a>(
    list: &'a [Arc<Message>],
    blocked: &HashSet<UserId>,
    ignored: &HashSet<UserId>,
    revealed: &HashSet<MessageId>,
    unread_from: Option<&MessageId>,
    jump: Option<&MessageId>,
) -> Collapsed<'a> {
    let kept_always: Vec<&MessageId> = unread_from
        .into_iter()
        .chain(new_divider_at(list, unread_from).map(|i| &list[i].id))
        .chain(jump)
        .collect();
    let hidden = |m: &Message| {
        !m.system
            && (blocked.contains(&m.author) || ignored.contains(&m.author))
            && !revealed.contains(&m.id)
            && !kept_always.contains(&&m.id)
    };
    if (blocked.is_empty() && ignored.is_empty()) || !list.iter().any(|m| hidden(m)) {
        return Collapsed {
            list: Cow::Borrowed(list),
            runs: HashMap::new(),
            ignored: HashSet::new(),
        };
    }
    let mut kept: Vec<Arc<Message>> = Vec::with_capacity(list.len());
    let mut runs = HashMap::new();
    let mut ignored_runs = HashSet::new();
    let mut run: Vec<MessageId> = Vec::new();
    let mut run_ignored = false;
    let mut close = |run: &mut Vec<MessageId>, ignored_run: bool| {
        if let Some(first) = run.first().cloned() {
            if ignored_run {
                ignored_runs.insert(first.clone());
            }
            runs.insert(first, std::mem::take(run));
        }
    };
    for m in list {
        if hidden(m) {
            let is_ignored = ignored.contains(&m.author) && !blocked.contains(&m.author);
            if !run.is_empty() && is_ignored != run_ignored {
                close(&mut run, run_ignored);
            }
            if run.is_empty() {
                // The run's first message stands for it.
                kept.push(m.clone());
                run_ignored = is_ignored;
            }
            run.push(m.id.clone());
        } else {
            close(&mut run, run_ignored);
            kept.push(m.clone());
        }
    }
    close(&mut run, run_ignored);
    Collapsed {
        list: Cow::Owned(kept),
        runs,
        ignored: ignored_runs,
    }
}

/// What marks rows besides their messages.
pub struct Marks<'a> {
    /// The "NEW" divider goes after this message.
    pub unread_from: Option<&'a MessageId>,
    /// The message jumped to, and the jump's number.
    pub jump: Option<&'a (MessageId, i32)>,
    /// Messages whose spoilers were clicked open.
    pub revealed: &'a HashSet<MessageId>,
    /// The runs [`collapse_hidden`] folded, by first message.
    pub hidden_runs: &'a HashMap<MessageId, Vec<MessageId>>,
    /// Starts of ignored-user runs.
    pub ignored_runs: &'a HashSet<MessageId>,
    /// The timeline before folding, where replies find their originals
    /// (None: the list itself).
    pub all: Option<&'a [Arc<Message>]>,
    pub players: Option<&'a InlinePlayers>,
    pub invites: &'a std::collections::HashMap<(godwit_core::ids::AccountId, Arc<str>), Invite>,
    /// Servers we're in (an invite card to one says Joined).
    pub servers: &'a [Arc<godwit_core::server::Server>],
    /// The width of message text (the timeline reports it).
    pub text_width: f32,
}

/// Brings the timeline model up to date with `list`, rebuilding only the
/// rows that changed. Returns the new keys.
pub fn sync_timeline(
    model: &VecModel<MessageItem>,
    old: &[RowKey],
    list: &[Arc<Message>],
    marks: &Marks<'_>,
    window: &mut Option<(usize, usize)>,
    images_changed: bool,
    images: &mut Images,
) -> Vec<RowKey> {
    // Rows added above (an older page) push the view's rows down: move the
    // range with them until the view reports again.
    if let (Some((top, bottom)), Some(first)) = (window.as_mut(), old.first())
        && let Some(shift) = list.iter().position(|m| m.id == first.message.id).filter(|&s| s > 0)
    {
        *top += shift;
        *bottom += shift;
    }
    let window = *window; // Pictures for the rows in view and ten either side; before
    // the list says where it is, the newest rows, where a room opens.
    // Which messages had pictures, by message rather than row: pages
    // loading above shift every row, which mustn't look like a change.
    let had: HashSet<&MessageId> = old.iter().filter(|k| k.wanted).map(|k| &k.message.id).collect();
    let wanted = |i: usize, id: &MessageId| match window {
        // Near the view: pictures. Further: kept until well away (so
        // scrolling back and forth doesn't rebuild rows), then dropped.
        Some((top, bottom)) => {
            (i + MARGIN >= top && i <= bottom + MARGIN)
                || (had.contains(id) && i + KEEP_MARGIN >= top && i <= bottom + KEEP_MARGIN)
        }
        None => i + OPENING_ROWS >= list.len() || had.contains(id),
    };
    let mut keys: Vec<RowKey> = Vec::with_capacity(list.len());
    let mut prev: Option<&Message> = None;
    // The NEW line goes before the first message after the last one read
    // that isn't ours (as Godwit), if that one is loaded.
    let new_at = new_divider_at(list, marks.unread_from);
    let originals = marks.all.unwrap_or(list);
    for (i, m) in list.iter().enumerate() {
        let date = day(m.sent_ms).filter(|d| prev.is_none_or(|p| day(p.sent_ms).as_ref() != Some(d)));
        let new = new_at == Some(i);
        let first = date.is_some()
            || new
            || prev.is_none_or(|p| {
                p.author != m.author
                    || p.system
                    || m.system
                    || m.sent_ms - p.sent_ms > GROUP_GAP_MS
                    || m.reply_to.is_some()
            });
        prev = Some(m);
        let reply = m
            .reply_to
            .as_ref()
            .and_then(|r| originals.iter().find(|x| &x.id == r))
            .cloned();
        let jump = marks.jump.filter(|(id, _)| *id == m.id).map_or(0, |(_, n)| *n);
        keys.push(RowKey {
            message: m.clone(),
            links: link_embeds(m),
            width: marks.text_width.round() as u32,
            first,
            date,
            new,
            revealed: marks.revealed.contains(&m.id),
            hidden_run: marks
                .hidden_runs
                .get(&m.id)
                .map_or(0, |run| u32::try_from(run.len()).unwrap_or(u32::MAX)),
            ignored_run: marks.ignored_runs.contains(&m.id),
            reply,
            complete: false,
            jump,
            wanted: wanted(i, &m.id),
            animation_live: animation_near(i, window),
            invites: m
                .invites
                .iter()
                .map(|code| {
                    let state = marks.invites.get(&(m.author.account.clone(), code.clone())).cloned();
                    let joined = match &state {
                        Some(Invite::Ready(p) | Invite::Joining(p)) => p
                            .server
                            .as_ref()
                            .is_some_and(|s| marks.servers.iter().any(|x| &x.id == s)),
                        _ => false,
                    };
                    (state, joined)
                })
                .collect(),
            slots: Vec::new(),
        });
    }
    let n_old = old.len().min(model.row_count());
    let mut start = 0;
    while start < n_old && start < keys.len() && keys[start].same(&old[start], images_changed) {
        keys[start].complete = old[start].complete;
        keys[start].slots.clone_from(&old[start].slots);
        start += 1;
    }
    let mut end = 0;
    while end < n_old - start
        && end < keys.len() - start
        && keys[keys.len() - 1 - end].same(&old[n_old - 1 - end], images_changed)
    {
        let (k, o) = (keys.len() - 1 - end, n_old - 1 - end);
        keys[k].complete = old[o].complete;
        keys[k].slots.clone_from(&old[o].slots);
        end += 1;
    }
    // Changed rows are updated in place where they line up (a picture
    // arriving, a row coming near the view), so the list keeps its rows and
    // layout; only a different count removes or inserts.
    let (gone, came) = (n_old - end - start, keys.len() - end - start);
    for i in 0..gone.min(came) {
        let (item, complete, slots) = message_row(&keys[start + i], images, marks.players);
        keys[start + i].complete = complete;
        keys[start + i].slots = slots;
        model.set_row_data(start + i, item);
    }
    for i in (start + came..start + gone).rev() {
        model.remove(i);
    }
    for (i, key) in keys
        .iter_mut()
        .enumerate()
        .take(start + came)
        .skip(start + gone.min(came))
    {
        let (item, complete, slots) = message_row(key, images, marks.players);
        key.complete = complete;
        key.slots = slots;
        model.insert(i, item);
    }
    keys
}

fn message_row(key: &RowKey, images: &mut Images, players: Option<&InlinePlayers>) -> (MessageItem, bool, Vec<usize>) {
    let m = &key.message;
    if key.hidden_run > 0 {
        // Nothing of the message is built: it is hidden until the run is clicked open.
        let item = MessageItem {
            hidden_label: if key.ignored_run {
                if key.hidden_run == 1 {
                    "1 ignored message".into()
                } else {
                    format!("{} ignored messages", key.hidden_run).into()
                }
            } else if key.hidden_run == 1 {
                "1 blocked message".into()
            } else {
                format!("{} blocked messages", key.hidden_run).into()
            },
            divider_date: key.date.clone().unwrap_or_default().into(),
            divider_new: key.new,
            jump: key.jump,
            ..MessageItem::default()
        };
        return (item, true, Vec::new());
    }
    let mut complete = true;
    // The animation slots the row shows (anim.rs): they tick while it's near
    // the view, without the row being rebuilt.
    let mut slots: Vec<usize> = Vec::new();
    // A row with a playing player is never complete: each new frame rebuilds it.
    let playing = players.is_some_and(|players| key.player_urls().any(|url| players.active(url)));
    let wanted = key.wanted;
    let mut pic = |url: Option<&str>, fit: Fit| match url {
        // Far from the view: no picture, nothing fetched.
        Some(_) if !wanted => {
            complete = false;
            (no_image(), false, -1)
        }
        Some(u) => match images.get_animated(u, fit) {
            Some((img, slot)) => {
                if let Ok(s) = usize::try_from(slot) {
                    slots.push(s);
                }
                (img, true, slot)
            }
            None => {
                complete = false;
                (no_image(), false, -1)
            }
        },
        None => (no_image(), false, -1),
    };
    let (avatar, has_avatar) = if key.first {
        let (image, has, _) = pic(m.author_avatar.as_deref(), AVATAR);
        (image, has)
    } else {
        (no_image(), false)
    };
    let (header, blocks) = blocks(
        m,
        key.first && !m.system,
        m.jumbo.is_empty(),
        key.revealed,
        key.width as f32,
        &mut pic,
    );
    let jumbo: Vec<EmojiCellItem> = m
        .jumbo
        .iter()
        .map(|(url, name)| {
            let (image, has_image, anim) = pic(Some(url), JUMBO);
            // A Unicode emoji's own character, until its picture loads.
            let glyph = emojis::get_by_shortcode(name.trim_matches(':'));
            EmojiCellItem {
                name: name.as_ref().into(),
                glyph: glyph.map_or("", |e| e.as_str()).into(),
                image,
                has_image,
                anim,
            }
        })
        .collect();
    // Video files play in the same media area as a bare video link.
    let videos: Vec<Option<Embed>> = m.attachments.iter().map(video_embed).collect();
    let shown: Vec<&Embed> = key.shown_embeds().collect();
    let mut embeds: Vec<EmbedItem> = shown
        .iter()
        .copied()
        .chain(videos.iter().flatten())
        .map(|embed| {
            let thumbnail = embed.style == EmbedStyle::Card && embed.image_thumbnail;
            let fit = match embed.style {
                EmbedStyle::Activity => CAPSULE,
                _ if thumbnail => THUMBNAIL,
                _ => PREVIEW,
            };
            let (image, has_image, anim) = pic(embed.image_url.as_deref(), fit);
            let (icon, has_icon, _) = pic(embed.icon_url.as_deref(), EMBED_ICON);
            let play_url = embed.player_url.as_deref().or(embed.video_url.as_deref());
            let playing = players.zip(play_url).is_some_and(|(players, url)| players.active(url));
            let (width, height) = embed_media_size(embed);
            let accent = embed
                .colour
                .map(|c| slint::Color::from_rgb_u8(c[0], c[1], c[2]))
                .unwrap_or_default();
            // A YouTube video's description is its whole video blurb: the
            // card would run far down the timeline, so it is left out.
            let description = embed
                .description
                .as_deref()
                .filter(|_| !embed.player_url.as_deref().is_some_and(is_youtube_player))
                .unwrap_or_default();
            EmbedItem {
                style: match embed.style {
                    EmbedStyle::Card => 0,
                    EmbedStyle::Media => 1,
                    EmbedStyle::Gif => 2,
                    EmbedStyle::Activity => 3,
                },
                title: embed.title.as_deref().unwrap_or_default().into(),
                description: embed_description(description, embed.description_markup),
                has_description: !description.is_empty(),
                url: embed.url.as_deref().unwrap_or_default().into(),
                play_url: play_url.unwrap_or_default().into(),
                gif_picture: if embed.style == EmbedStyle::Gif {
                    embed.image_url.as_deref().unwrap_or_default().into()
                } else {
                    Default::default()
                },
                play_kind: if embed.player_url.is_some() {
                    "embed"
                } else if embed.style == EmbedStyle::Gif {
                    "gif"
                } else {
                    "video"
                }
                .into(),
                player_slot: players
                    .zip(play_url)
                    .and_then(|(players, url)| players.slot(url))
                    .and_then(|slot| i32::try_from(slot).ok())
                    .unwrap_or(-1),
                has_player_frame: players
                    .zip(play_url)
                    .is_some_and(|(players, url)| players.slot(url).is_some()),
                inline_playing: playing,
                site_name: embed.site_name.as_deref().unwrap_or_default().into(),
                more_label: embed
                    .more_link
                    .as_ref()
                    .map(|(label, _)| &**label)
                    .unwrap_or_default()
                    .into(),
                more_url: embed
                    .more_link
                    .as_ref()
                    .map(|(_, url)| &**url)
                    .unwrap_or_default()
                    .into(),
                icon,
                has_icon,
                image,
                has_image,
                anim,
                image_thumbnail: thumbnail,
                // Players and videos always show their area, as the official
                // SpecialEmbed and `<video>`: dark until playing.
                has_media: embed.style != EmbedStyle::Activity
                    && (play_url.is_some() || (embed.image_url.is_some() && !thumbnail)),
                width: width as f32,
                height: height as f32,
                accent,
                has_accent: embed.colour.is_some(),
            }
        })
        .collect();
    let mut video_items = embeds.split_off(shown.len()).into_iter();
    let attachments: Vec<AttachmentItem> = m
        .attachments
        .iter()
        .zip(&videos)
        .enumerate()
        .map(|(index, (a, video))| {
            let kind = match a.kind {
                AttachmentKind::Image => 0,
                AttachmentKind::Video => 1,
                AttachmentKind::Audio => 2,
                AttachmentKind::File => 3,
            };
            let (w, h) = preview_size(a.dimensions);
            let (image, has_image, anim) = if kind == 0 {
                pic(Some(&a.url), PREVIEW)
            } else {
                (no_image(), false, -1)
            };
            let uploading = a.url.is_empty();
            // Audio plays in place (a `key` of "" means a plain file card).
            let playable = kind == 2 && !uploading;
            let video = video.as_ref().and_then(|_| video_items.next());
            AttachmentItem {
                kind,
                has_video: video.is_some(),
                video: video.unwrap_or_else(empty_embed_item),
                name: a.name.as_ref().into(),
                detail: if uploading {
                    "Uploading…".into()
                } else if a.size == 0 {
                    // Some networks (Google Chat) don't report sizes.
                    "".into()
                } else {
                    human_size(a.size).into()
                },
                openable: !uploading,
                image,
                has_image,
                anim,
                width: w as f32,
                height: h as f32,
                spoiler: a.spoiler,
                spoiler_revealed: false,
                key: if playable {
                    format!("{}:{index}", m.id.0).into()
                } else {
                    "".into()
                },
                voice: a.voice.is_some(),
                wave: ModelRc::new(VecModel::from(
                    a.voice.as_ref().map(|v| wave_bars(&v.waveform, 40)).unwrap_or_default(),
                )),
                length: a
                    .voice
                    .as_ref()
                    .map(|v| clock(u64::from(v.duration_ms)))
                    .unwrap_or_default()
                    .into(),
            }
        })
        .collect();
    let invites: Vec<InviteItem> = m
        .invites
        .iter()
        .zip(&key.invites)
        .map(|(code, (state, joined))| {
            let preview = match state {
                Some(Invite::Ready(p) | Invite::Joining(p)) => Some(p),
                _ => None,
            };
            let (icon, has_icon, _) = pic(preview.and_then(|p| p.icon_url.as_deref()), AVATAR);
            let name = preview.map_or("", |p| p.name.as_ref());
            InviteItem {
                code: code.as_ref().into(),
                // 0 loading, 1 can join, 2 joining, 3 joined, 4 not valid.
                state: match state {
                    None | Some(Invite::Loading) => 0,
                    Some(Invite::Ready(_)) if *joined => 3,
                    Some(Invite::Ready(_)) => 1,
                    Some(Invite::Joining(_)) => 2,
                    Some(Invite::Invalid) => 4,
                },
                name: name.into(),
                initials: initials(name).into(),
                members: preview
                    .and_then(|p| p.members)
                    .map_or_else(String::new, |n| format!("{n} members"))
                    .into(),
                icon,
                has_icon,
            }
        })
        .collect();
    let reactions: Vec<ReactionItem> = m
        .reactions
        .iter()
        .map(|r| {
            // A custom emoji's own picture, else the shared one for a
            // Unicode emoji: networks needn't fill it in for those.
            let url = r
                .image_url
                .as_deref()
                .map(str::to_owned)
                .or_else(|| crate::emoji::url(&r.emoji));
            let (image, has_image, anim) = pic(url.as_deref(), EMOJI);
            ReactionItem {
                emoji: r.emoji.as_ref().into(),
                image,
                has_image,
                count: i32::try_from(r.count).unwrap_or(i32::MAX),
                me: r.me,
                anim,
            }
        })
        .collect();
    let (has_reply, reply_author, (reply_text, reply_runs, reply_ready)) = match (&m.reply_to, &key.reply) {
        (Some(_), Some(r)) => (true, r.author_name.to_string(), quote_runs(r, images)),
        (Some(_), None) => (true, String::new(), (String::new(), label_runs("", images), true)),
        (None, _) => (false, String::new(), (String::new(), label_runs("", images), true)),
    };
    complete &= reply_ready;
    let reply_author_runs = label_runs(&reply_author, images);
    let replied = key.reply.as_deref().filter(|_| m.reply_to.is_some());
    let (reply_avatar, reply_has_avatar) = match replied
        .and_then(|r| r.author_avatar.as_deref())
        .and_then(|u| images.get(u, REPLY_AVATAR))
    {
        Some(img) => (img, true),
        None => (no_image(), false),
    };
    // As the official client: "Sent an attachment", or several.
    let reply_note = match replied.map_or(0, |r| r.attachments.len()) {
        0 => "",
        1 => "Sent an attachment",
        _ => "Sent multiple attachments",
    };
    let colour = |c: [u8; 3]| slint::Color::from_rgb_u8(c[0], c[1], c[2]);
    let item = MessageItem {
        author: m.author_name.as_ref().into(),
        author_colour: m.author_colour.map(colour).unwrap_or_default(),
        has_author_colour: m.author_colour.is_some(),
        initials: initials(&m.author_name).into(),
        avatar,
        has_avatar,
        time: time(m.sent_ms).into(),
        header,
        blocks: ModelRc::new(VecModel::from(blocks)),
        jumbo: ModelRc::new(VecModel::from(jumbo)),
        system: m.system,
        hidden_label: String::new().into(),
        pending: m.pending,
        edited: m.edited,
        pinned: m.pinned,
        forwardable: m.forwardable,
        poll_endable: m.poll_endable,
        mine: m.author.raw == m.author.account.user,
        first_in_group: key.first,
        divider_date: key.date.clone().unwrap_or_default().into(),
        divider_new: key.new,
        jump: key.jump,
        has_reply,
        reply_initials: initials(&reply_author).into(),
        reply_author: reply_author.into(),
        reply_author_runs,
        reply_text: reply_text.into(),
        reply_runs,
        reply_avatar,
        reply_has_avatar,
        reply_colour: replied.and_then(|r| r.author_colour).map(colour).unwrap_or_default(),
        reply_has_colour: replied.is_some_and(|r| r.author_colour.is_some()),
        reply_note: reply_note.into(),
        reply_missing: m.reply_to.is_some() && replied.is_none(),
        attachments: ModelRc::new(VecModel::from(attachments)),
        components: ModelRc::new(VecModel::from(m.components.as_ref().map_or_else(
            Vec::new,
            |components| {
                components
                    .rows
                    .iter()
                    .map(|row| ComponentRowItem {
                        items: ModelRc::new(VecModel::from(
                            row.items
                                .iter()
                                .map(|component| match component {
                                    godwit_core::message::MessageComponent::Button(button) => {
                                        let style = match button.style {
                                            godwit_core::message::MessageButtonStyle::Primary => 1,
                                            godwit_core::message::MessageButtonStyle::Secondary => 2,
                                            godwit_core::message::MessageButtonStyle::Success => 3,
                                            godwit_core::message::MessageButtonStyle::Danger => 4,
                                            godwit_core::message::MessageButtonStyle::Link => 5,
                                        };
                                        let label = match (&button.emoji, &button.label) {
                                            (Some(emoji), Some(label)) => format!("{emoji} {label}"),
                                            (Some(emoji), None) => emoji.to_string(),
                                            (None, Some(label)) => label.to_string(),
                                            (None, None) => String::new(),
                                        };
                                        ComponentItem {
                                            kind: "button".into(),
                                            label: label.into(),
                                            url: button.url.as_deref().unwrap_or_default().into(),
                                            custom_id: button.custom_id.as_deref().unwrap_or_default().into(),
                                            style,
                                            disabled: button.disabled,
                                        }
                                    }
                                    godwit_core::message::MessageComponent::Select(select) => ComponentItem {
                                        kind: "select".into(),
                                        label: select
                                            .placeholder
                                            .as_deref()
                                            .or_else(|| select.options.first().map(|option| option.label.as_ref()))
                                            .unwrap_or("Choose an option")
                                            .into(),
                                        url: "".into(),
                                        custom_id: "".into(),
                                        style: 2,
                                        disabled: true,
                                    },
                                })
                                .collect::<Vec<_>>(),
                        )),
                    })
                    .collect()
            },
        ))),
        embeds: ModelRc::new(VecModel::from(embeds)),
        invites: ModelRc::new(VecModel::from(invites)),
        reactions: ModelRc::new(VecModel::from(reactions)),
    };
    (item, complete && !playing, slots)
}

/// A message's text as blocks: text runs, quotes, headings and code.
/// Gets a picture: (image, loaded, animation slot).
pub(super) type Pic<'a> = dyn FnMut(Option<&str>, Fit) -> (slint::Image, bool, i32) + 'a;

/// Message text width taken by a quote's bar and gap (timeline.slint).
pub const QUOTE_INSET: f32 = 12.0;
/// Message text width taken by a code block's padding (timeline.slint).
pub const CODE_INSET: f32 = 16.0;

/// A message's header (its group's author and time, when `header`) and text
/// blocks (when `text`), laid out at `width` (textlayout.rs): the pieces the
/// timeline draws, and each layout kept for selecting. The header is kept as
/// block 0 (empty without one), the text from block 1.
fn blocks(
    m: &Message,
    header: bool,
    with_text: bool,
    revealed: bool,
    width: f32,
    pic: &mut Pic<'_>,
) -> (BlockItem, Vec<BlockItem>) {
    use crate::textlayout::{Base, Paragraph, lay_out};
    let with_text = with_text && !m.text.is_empty();
    if !header && !with_text {
        crate::textlayout::forget(&m.id);
        return (BlockItem::default(), Vec::new());
    }
    let base = Base {
        size: 14.0,
        bold: false,
        mono: false,
    };
    // Unicode emoji from the shared pack (crate::emoji).
    let emoji_url = |e: &str| crate::emoji::canonical(e).map(crate::emoji::url_of);
    let head = if header {
        let time = time(m.sent_ms);
        lay_out(
            Paragraph::header(&m.author_name, m.author_colour, &time, &emoji_url),
            base,
            width,
        )
    } else {
        lay_out(Paragraph::default(), base, width)
    };
    let mut kept = vec![head];
    let header_item = if header {
        kept.first().map(|laid| block_item(4, laid, pic)).unwrap_or_default()
    } else {
        BlockItem::default()
    };
    if !with_text {
        crate::textlayout::remember(&m.id, kept);
        return (header_item, Vec::new());
    }
    let text = |size: f32| Base {
        size,
        bold: false,
        mono: false,
    };
    let parts: Vec<(i32, Paragraph, Base, f32)> = if m.markup == Markup::Plain {
        vec![(0, Paragraph::plain(&m.text, &emoji_url), text(14.0), width)]
    } else {
        let source = if revealed {
            rich::reveal_spoilers(&m.text)
        } else {
            m.text.to_string()
        };
        rich::blocks(&source)
            .into_iter()
            .map(|b| match b {
                rich::Block::Text(t) => (0, Paragraph::rich(&t, &emoji_url), text(14.0), width),
                rich::Block::Quote(t) => (1, Paragraph::rich(&t, &emoji_url), text(14.0), width - QUOTE_INSET),
                // Godwit's heading sizes: 2.0 down to 0.8 of the message size.
                rich::Block::Heading(level, t) => (
                    2,
                    Paragraph::rich(&t, &emoji_url),
                    Base {
                        size: 14.0 * [2.0, 1.6, 1.4, 1.2, 1.0, 0.8][usize::from(level.clamp(1, 6)) - 1],
                        bold: true,
                        mono: false,
                    },
                    width,
                ),
                rich::Block::Code { text: code, lang } => (
                    3,
                    Paragraph::rich(&code::highlight(&code, &lang), &|_| None),
                    Base {
                        size: 13.0,
                        bold: false,
                        mono: true,
                    },
                    width - CODE_INSET,
                ),
            })
            .collect()
    };
    let mut out = Vec::with_capacity(parts.len());
    for (kind, paragraph, base, width) in parts {
        let laid = lay_out(paragraph, base, width);
        out.push(block_item(kind, &laid, pic));
        kept.push(laid);
    }
    crate::textlayout::remember(&m.id, kept);
    (header_item, out)
}

/// What the timeline draws of one laid-out block.
pub(super) fn block_item(kind: i32, laid: &crate::textlayout::Laid, pic: &mut Pic<'_>) -> BlockItem {
    use crate::textlayout::Piece;
    let pieces: Vec<PieceItem> = laid
        .pieces()
        .into_iter()
        .map(|piece| match piece {
            Piece::Text {
                x,
                y,
                width,
                text,
                size,
                bold,
                italic,
                brush,
            } => PieceItem {
                x,
                y,
                width,
                text: text.into(),
                size,
                bold,
                italic,
                strike: brush.strike,
                link: brush.link > 0,
                quiet: brush.quiet,
                faint: brush.faint,
                code: brush.code,
                has_colour: brush.colour.is_some(),
                colour: brush
                    .colour
                    .map(|[r, g, b]| slint::Color::from_rgb_u8(r, g, b))
                    .unwrap_or_default(),
                ..PieceItem::default()
            },
            Piece::Emoji { x, y, size, index } => {
                let e = laid.paragraph.emoji.get(index);
                let (image, has_image, anim) = pic(e.and_then(|e| e.url.as_deref()), EMOJI);
                PieceItem {
                    x,
                    y,
                    width: size,
                    emoji: true,
                    // Until its picture loads, the emoji's own character.
                    text: e.map(|e| e.text.to_string()).unwrap_or_default().into(),
                    image,
                    has_image,
                    anim,
                    ..PieceItem::default()
                }
            }
        })
        .collect();
    BlockItem {
        kind,
        size: laid.base.size,
        text_height: laid.height().ceil(),
        pieces: slint::ModelRc::new(slint::VecModel::from(pieces)),
    }
}

/// One line of a message for its reply preview.
/// The replied-to text on one line, as the official client: newlines as
/// spaces, cut at 128 characters.
/// The official card cuts long link descriptions at 350 characters; bots'
/// embeds are rich text, shown whole.
fn embed_description(text: &str, markup: Markup) -> StyledText {
    match markup {
        Markup::Rich => StyledText::from_markdown(text).unwrap_or_else(|_| StyledText::from_plain_text(text)),
        Markup::Plain if text.chars().count() > 350 => {
            StyledText::from_plain_text(&(text.chars().take(350).collect::<String>() + "…"))
        }
        Markup::Plain => StyledText::from_plain_text(text),
    }
}

/// Players for page links in a message's text that none of its embeds
/// plays: networks send no embed (or one without a player) for some, like
/// music.youtube.com and Spotify. At most three per message.
fn link_embeds(m: &Message) -> Vec<Embed> {
    const MAX: usize = 3;
    if !m.source.contains("https://") {
        return Vec::new();
    }
    let mut out: Vec<Embed> = Vec::new();
    for word in m.source.split(|c: char| c.is_whitespace() || "<>()[]\"'|`".contains(c)) {
        if out.len() >= MAX {
            break;
        }
        let Some(link) = word.find("https://").and_then(|at| word.get(at..)) else {
            continue;
        };
        // "… https://youtu.be/x." ends a sentence, not the link.
        let link = link.trim_end_matches(['.', ',', '!', '?', ';', ':']);
        let Some(player) = godwit_core::message::link_player(link) else {
            continue;
        };
        let covered = m.embeds.iter().any(|e| {
            e.player_url.as_deref() == Some(player.as_str())
                || (e.player_url.is_some() && e.url.as_deref() == Some(link))
        }) || out.iter().any(|e| e.player_url.as_deref() == Some(player.as_str()));
        if covered {
            continue;
        }
        let site = if player.starts_with("https://open.spotify.com/") {
            "Spotify"
        } else {
            "YouTube"
        };
        out.push(Embed {
            url: Some(link.into()),
            site_name: Some(site.into()),
            player_url: Some(player.into()),
            style: EmbedStyle::Card,
            ..Embed::default()
        });
    }
    out
}

/// A video file as a bare video link's embed, so it plays in the same media
/// area (the official client's `<video controls>` attachment). Uploads in
/// progress (no address yet) and non-HTTPS files stay file cards.
fn video_embed(a: &Attachment) -> Option<Embed> {
    (a.kind == AttachmentKind::Video && a.url.starts_with("https://")).then(|| Embed {
        url: Some(a.url.clone()),
        video_url: Some(a.url.clone()),
        media_dimensions: a.dimensions,
        style: EmbedStyle::Media,
        ..Embed::default()
    })
}

/// An attachment row's video slot when it has none: the shared stand-in
/// image, so rows still compare equal (see `no_image`).
fn empty_embed_item() -> EmbedItem {
    EmbedItem {
        image: no_image(),
        icon: no_image(),
        player_slot: -1,
        anim: -1,
        ..EmbedItem::default()
    }
}

/// YouTube's player, as every network's adapter builds it.
fn is_youtube_player(url: &str) -> bool {
    url.starts_with("https://www.youtube-nocookie.com/embed/")
}

/// An embed's media area on screen. Players are at least the official
/// 400 × 225 (a thumbnail-only special embed still gets a full player), and a
/// video with no size is 16:9, as the official `<video>` falls back to.
fn embed_media_size(embed: &Embed) -> (u32, u32) {
    if embed.player_url.is_some() && embed.style == EmbedStyle::Card {
        return match embed.media_dimensions.filter(|_| !embed.image_thumbnail) {
            Some(d) => {
                let (w, h) = preview_size(Some(d));
                (w.max(400), h.max(225))
            }
            None => (400, 225),
        };
    }
    match embed.media_dimensions {
        Some(d) => preview_size(Some(d)),
        None if embed.video_url.is_some() => preview_size(Some((1280, 720))),
        None => preview_size(None),
    }
}

/// An image preview's size on screen: its own shape within 400 × 300.
pub(super) fn preview_size(dimensions: Option<(u32, u32)>) -> (u32, u32) {
    let Some((w, h)) = dimensions.filter(|(w, h)| *w > 0 && *h > 0) else {
        return (300, 200);
    };
    let scale = (400.0 / f64::from(w)).min(300.0 / f64::from(h)).min(1.0);
    (
        ((f64::from(w) * scale).round() as u32).max(1),
        ((f64::from(h) * scale).round() as u32).max(1),
    )
}

#[cfg(test)]
mod timeline_tests {
    use super::*;
    use godwit_core::ids::AccountId;
    use godwit_core::network::Network;

    fn messages(range: std::ops::Range<usize>) -> Vec<Arc<Message>> {
        range
            .map(|i| Arc::new(crate::demo::msg(&format!("m{i:03}"), "u1", "x", i as i64, "text")))
            .collect()
    }

    /// Messages from `who` per id letter, one minute apart.
    fn thread(spec: &[(&str, &str)]) -> Vec<Arc<Message>> {
        spec.iter()
            .enumerate()
            .map(|(i, (id, who))| Arc::new(crate::demo::msg(id, who, who, i as i64, "x")))
            .collect()
    }

    fn mid(s: &str) -> MessageId {
        MessageId(s.into())
    }

    fn shown(c: &Collapsed<'_>) -> Vec<String> {
        c.list.iter().map(|m| m.id.0.to_string()).collect()
    }

    fn bad() -> HashSet<UserId> {
        [crate::demo::user("bad")].into_iter().collect()
    }

    #[test]
    fn runs_of_blocked_authors_collapse_into_their_first_message() {
        let list = thread(&[
            ("a", "u1"),
            ("b", "bad"),
            ("c", "bad"),
            ("d", "bad"),
            ("e", "u1"),
            ("f", "bad"),
            ("g", "u1"),
        ]);
        let no_users: HashSet<UserId> = HashSet::new();
        let no_messages: HashSet<MessageId> = HashSet::new();
        let c = collapse_hidden(&list, &bad(), &no_users, &no_messages, None, None);
        assert_eq!(shown(&c), ["a", "b", "e", "f", "g"]);
        assert_eq!(c.runs[&mid("b")], [mid("b"), mid("c"), mid("d")]);
        assert_eq!(c.runs[&mid("f")], [mid("f")]);
        // Revealed messages stay shown, one by one.
        let open: HashSet<MessageId> = [mid("b"), mid("c"), mid("d")].into_iter().collect();
        let c = collapse_hidden(&list, &bad(), &no_users, &open, None, None);
        assert_eq!(shown(&c), ["a", "b", "c", "d", "e", "f", "g"]);
        assert_eq!(
            c.runs.keys().collect::<Vec<_>>(),
            [&mid("f")],
            "the other run stays folded"
        );
        // Nobody blocked: nothing is copied.
        let c = collapse_hidden(&list, &no_users, &no_users, &no_messages, None, None);
        assert!(matches!(c.list, Cow::Borrowed(_)) && c.runs.is_empty());
    }

    #[test]
    fn ignored_runs_collapse_and_runs_of_different_kinds_get_distinct_labels() {
        let list = thread(&[("a", "ignored"), ("b", "ignored"), ("c", "bad"), ("d", "bad")]);
        let ignored: HashSet<UserId> = [crate::demo::user("ignored")].into_iter().collect();
        let c = collapse_hidden(&list, &bad(), &ignored, &HashSet::new(), None, None);
        assert_eq!(shown(&c), ["a", "c"]);
        assert_eq!(c.runs[&mid("a")], [mid("a"), mid("b")]);
        assert_eq!(c.runs[&mid("c")], [mid("c"), mid("d")]);
        assert!(c.ignored.contains(&mid("a")));
        assert!(!c.ignored.contains(&mid("c")));
        let expanded: HashSet<MessageId> = [mid("a"), mid("b")].into_iter().collect();
        let open = collapse_hidden(&list, &bad(), &ignored, &expanded, None, None);
        assert_eq!(shown(&open), ["a", "b", "c"]);
        assert_eq!(open.runs.keys().collect::<Vec<_>>(), [&mid("c")]);
        assert!(open.ignored.is_empty());
        let revealed = HashSet::new();
        let invites = HashMap::new();
        let marks = Marks {
            unread_from: None,
            jump: None,
            revealed: &revealed,
            hidden_runs: &c.runs,
            ignored_runs: &c.ignored,
            all: Some(&list),
            players: None,
            invites: &invites,
            servers: &[],
            text_width: 600.0,
        };
        let model = VecModel::<MessageItem>::default();
        let keys = sync_timeline(&model, &[], &c.list, &marks, &mut None, false, &mut Images::offline());
        assert_eq!(
            message_row(&keys[0], &mut Images::offline(), None).0.hidden_label,
            "2 ignored messages"
        );
        assert_eq!(
            message_row(&keys[1], &mut Images::offline(), None).0.hidden_label,
            "2 blocked messages"
        );
    }

    #[test]
    fn runs_at_the_ends_fold_and_a_system_message_breaks_one() {
        let mut list = thread(&[
            ("a", "bad"),
            ("b", "bad"),
            ("sys", "bad"),
            ("c", "bad"),
            ("d", "u1"),
            ("e", "bad"),
        ]);
        let mut sys = crate::demo::msg("sys", "bad", "bad", 2, "joined");
        sys.system = true;
        list[2] = Arc::new(sys);
        let c = collapse_hidden(&list, &bad(), &HashSet::new(), &HashSet::new(), None, None);
        assert_eq!(shown(&c), ["a", "sys", "c", "d", "e"]);
        assert_eq!(c.runs[&mid("a")].len(), 2, "a run at the start");
        assert_eq!(c.runs[&mid("c")].len(), 1, "the system message split it");
        assert_eq!(c.runs[&mid("e")].len(), 1, "a run at the end");
    }

    #[test]
    fn the_new_divider_and_the_jump_target_split_a_run() {
        let list = thread(&[
            ("a", "u1"),
            ("b", "bad"),
            ("c", "bad"),
            ("d", "bad"),
            ("e", "bad"),
            ("f", "bad"),
        ]);
        // Read up to "a": the NEW divider goes on "b", the jump target is "e".
        let (read, jump) = (mid("a"), mid("e"));
        let c = collapse_hidden(
            &list,
            &bad(),
            &HashSet::new(),
            &HashSet::new(),
            Some(&read),
            Some(&jump),
        );
        assert_eq!(shown(&c), ["a", "b", "c", "e", "f"]);
        assert_eq!(c.runs[&mid("c")], [mid("c"), mid("d")]);
        assert!(!c.runs.contains_key(&mid("b")) && !c.runs.contains_key(&mid("e")));
        // A hidden "read" message itself is kept too, so the divider is found.
        let read = mid("c");
        let c = collapse_hidden(&list, &bad(), &HashSet::new(), &HashSet::new(), Some(&read), None);
        assert!(shown(&c).contains(&"c".to_owned()) && shown(&c).contains(&"d".to_owned()));
    }

    #[test]
    fn replies_find_their_original_in_the_unfolded_list() {
        let mut list = thread(&[("a", "bad"), ("b", "bad"), ("c", "u1")]);
        let mut reply = crate::demo::msg("c", "u1", "u1", 2, "re");
        reply.reply_to = Some(mid("b"));
        list[2] = Arc::new(reply);
        let c = collapse_hidden(&list, &bad(), &HashSet::new(), &HashSet::new(), None, None);
        assert_eq!(shown(&c), ["a", "c"], "b is folded away");
        let (revealed, invites) = (HashSet::new(), HashMap::new());
        let marks = Marks {
            unread_from: None,
            jump: None,
            revealed: &revealed,
            hidden_runs: &c.runs,
            ignored_runs: &c.ignored,
            all: Some(&list),
            players: None,
            invites: &invites,
            servers: &[],
            text_width: 600.0,
        };
        let model = VecModel::<MessageItem>::default();
        let keys = sync_timeline(&model, &[], &c.list, &marks, &mut None, false, &mut Images::offline());
        assert!(keys[1].reply.as_ref().is_some_and(|r| r.id == mid("b")));
    }

    #[test]
    fn a_page_loaded_above_leaves_the_rows_near_the_view_alone() {
        let _ = AccountId::new(Network::Stoat, "me");
        let mut images = Images::offline();
        let model = VecModel::<MessageItem>::default();
        let revealed = HashSet::new();
        let invites = std::collections::HashMap::new();
        let marks = Marks {
            unread_from: None,
            jump: None,
            revealed: &revealed,
            hidden_runs: &Default::default(),
            ignored_runs: &Default::default(),
            all: None,
            players: None,
            invites: &invites,
            servers: &[],
            text_width: 600.0,
        };
        let newest = messages(100..130);
        // Looking at the newest rows (20..29 of 30).
        let mut window = Some((20, 29));
        let keys = sync_timeline(&model, &[], &newest, &marks, &mut window, false, &mut images);
        let had: Vec<bool> = keys.iter().map(|k| k.wanted).collect();
        assert!(had[29] && had[10] && !had[9], "the view ±10 has pictures");
        // A page of 50 older messages arrives above; the view hasn't reported yet,
        // so its range still has the old row numbers.
        let mut all = messages(50..100);
        all.extend(newest.iter().cloned());
        let keys = sync_timeline(&model, &keys, &all, &marks, &mut window, false, &mut images);
        assert_eq!(model.row_count(), 80);
        assert_eq!(window, Some((70, 79)), "the range moved down with its rows");
        for (i, k) in keys.iter().enumerate().skip(50) {
            assert_eq!(k.wanted, had[i - 50], "row {i} kept what it had, by message");
        }
    }
}
