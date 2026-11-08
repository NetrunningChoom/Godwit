// SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
// Copyright (C) 2026 Godwit contributors
// Portions ported from Yapfast (https://github.com/yapfast/yapfast),
// Copyright (C) the Yapfast authors, GPL-3.0-or-later.
//! Managing channels and categories: settings, delete, create and move.

use super::*;
use godwit_core::channel_settings::{ChannelOverview, ChannelOverviewEdit};
use godwit_core::own_profile::ProfileImage;

/// Modify Channel's limits (docs.discord.food/resources/channel): a name of
/// 1 to 100 characters, a topic of up to 1024 (4096 for forums and media
/// channels), slowmode up to 6 hours.
const NAME_MAX: usize = 100;
const TOPIC_MAX: u32 = 1024;
const FORUM_TOPIC_MAX: u32 = 4096;
const SLOWMODE_MAX: u32 = 21_600;

impl DiscordAccount {
    /// A server channel or category of this account we may manage: Manage
    /// Channels on that channel itself (its overwrites count), as the
    /// official client's channel menu. Threads have their own permission.
    fn managed_channel(&self, room: &RoomId) -> Result<&store::Channel, &'static str> {
        if self.account.as_ref() != Some(&room.account) {
            return Err("This channel belongs to another account.");
        }
        let Some(c) = self.store.channels.get(room.raw.as_ref()) else {
            return Err("This channel is no longer available.");
        };
        if c.guild.is_none() || c.thread() || !self.store.can(&c.id, p::MANAGE_CHANNELS) {
            return Err("You need the Manage Channels permission for this.");
        }
        Ok(c)
    }

    /// A channel as the settings page shows it.
    fn channel_overview(&self, room: &RoomId) -> Result<ChannelOverview, &'static str> {
        let c = self.managed_channel(room)?;
        let category = c.kind == store::GUILD_CATEGORY;
        Ok(ChannelOverview {
            name: c.name.as_str().into(),
            description: c.topic.as_str().into(),
            // Discord channels have no icon.
            icon_url: None,
            icons: false,
            mature: c.nsfw,
            can_manage: true,
            is_category: category,
            slowmode: (!category).then_some(c.slowmode),
            description_max: topic_max(c),
        })
    }

    /// The Modify Channel body for an edit: only what changed.
    fn channel_body(c: &store::Channel, edit: &ChannelOverviewEdit) -> Result<Value, String> {
        let mut body = serde_json::Map::new();
        if let Some(name) = &edit.name {
            let name = name.trim();
            if name.is_empty() || name.chars().count() > NAME_MAX {
                return Err(format!("A channel name is 1 to {NAME_MAX} characters."));
            }
            body.insert("name".into(), json!(name));
        }
        if let Some(topic) = &edit.description {
            if topic.chars().count() > topic_max(c) as usize {
                return Err(format!("A topic is at most {} characters.", topic_max(c)));
            }
            // A cleared topic is null (Modify Channel: topic ?string).
            body.insert(
                "topic".into(),
                if topic.is_empty() {
                    Value::Null
                } else {
                    json!(topic.as_ref())
                },
            );
        }
        if let Some(nsfw) = edit.mature {
            body.insert("nsfw".into(), json!(nsfw));
        }
        if let Some(seconds) = edit.slowmode {
            if seconds > SLOWMODE_MAX {
                return Err("Slowmode is at most 6 hours.".into());
            }
            body.insert("rate_limit_per_user".into(), json!(seconds));
        }
        if edit.icon != ProfileImage::Keep {
            return Err("Discord channels have no icon.".into());
        }
        Ok(Value::Object(body))
    }

    pub(super) fn channel_command(&mut self, command: Command) -> Vec<Event> {
        let mut out = Vec::new();
        let failed = |room: RoomId, request: u64, e: &str| {
            vec![Event::ChannelOverview {
                room,
                request,
                result: Err(e.into()),
            }]
        };
        match command {
            Command::LoadChannelOverview { room, request } => {
                let result = self.channel_overview(&room).map(Some).map_err(Into::into);
                out.push(Event::ChannelOverview { room, request, result });
            }
            Command::SaveChannelOverview { room, request, edit } => {
                let body = match self.managed_channel(&room) {
                    Ok(c) => Self::channel_body(c, &edit),
                    Err(e) => Err(e.to_owned()),
                };
                match body {
                    Ok(body) if self.demo.is_some() => {
                        self.demo_modify(room.raw.as_ref(), &body);
                        self.channel_saved(room.raw.as_ref(), request, &mut out);
                    }
                    Ok(body) => self.net.send(Cmd::Request(Request::ModifyChannel {
                        channel: room.raw.to_string(),
                        request,
                        body,
                    })),
                    Err(e) => return failed(room, request, &e),
                }
            }
            Command::DeleteChannel { room, request } => {
                if let Err(e) = self.managed_channel(&room) {
                    return failed(room, request, e);
                }
                if self.demo.is_some() {
                    self.channel_deleted(room.raw.as_ref(), request, &mut out);
                } else {
                    self.net.send(Cmd::Request(Request::DeleteChannel {
                        channel: room.raw.to_string(),
                        request,
                    }));
                }
            }
            _ => {}
        }
        out
    }

    /// Demo mode: what Discord would answer a Modify Channel, applied to the store.
    fn demo_modify(&mut self, channel: &str, body: &Value) {
        if let Some(c) = self.store.channels.get_mut(channel) {
            if let Some(n) = body["name"].as_str() {
                c.name = n.to_owned();
            }
            if let Some(t) = body.get("topic") {
                c.topic = t.as_str().unwrap_or_default().to_owned();
            }
            if let Some(n) = body["nsfw"].as_bool() {
                c.nsfw = n;
            }
            if let Some(s) = body["rate_limit_per_user"].as_u64() {
                c.slowmode = u32::try_from(s).unwrap_or(0);
            }
        }
    }

    /// The channel as saved: the sidebar and the page show it.
    fn channel_saved(&mut self, channel: &str, request: u64, out: &mut Vec<Event>) {
        let Some(room) = self.room_id(channel) else { return };
        self.sync_rooms(out);
        self.sync_servers(out);
        let result = self.channel_overview(&room).map(Some).map_err(Into::into);
        out.push(Event::ChannelOverview { room, request, result });
    }

    /// Gone: a category's channels move out of it (Discord's CHANNEL_UPDATEs
    /// say so too).
    fn channel_deleted(&mut self, channel: &str, request: u64, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else { return };
        for c in self.store.channels.values_mut() {
            if c.parent.as_deref() == Some(channel) && !c.thread() {
                c.parent = None;
            }
        }
        self.store.remove_channel(channel);
        self.sync_rooms(out);
        self.sync_servers(out);
        out.push(Event::ChannelOverview {
            room: RoomId::new(account, channel),
            request,
            result: Ok(None),
        });
    }

    /// Discord's answer to a channel edit or delete.
    pub(super) fn channel_answer(&mut self, answer: Answer, out: &mut Vec<Event>) {
        let Some(account) = self.account.clone() else { return };
        match answer {
            Answer::ChannelSaved {
                channel,
                request,
                result,
            } => match result {
                // Applied now; the CHANNEL_UPDATE that follows repeats it.
                Ok(updated) if updated["id"].as_str() == Some(channel.as_str()) => {
                    self.store.put_channel(&updated);
                    self.channel_saved(&channel, request, out);
                }
                Ok(_) => out.push(Event::ChannelOverview {
                    room: RoomId::new(account, channel.as_str()),
                    request,
                    result: Err("Discord's answer wasn't what was expected.".into()),
                }),
                Err(e) => out.push(Event::ChannelOverview {
                    room: RoomId::new(account, channel.as_str()),
                    request,
                    result: Err(e.into()),
                }),
            },
            Answer::ChannelDeleted {
                channel,
                request,
                result,
            } => match result {
                Ok(()) => self.channel_deleted(&channel, request, out),
                // Already gone counts as deleted.
                Err(e) if e.is_gone() => self.channel_deleted(&channel, request, out),
                Err(e) => out.push(Event::ChannelOverview {
                    room: RoomId::new(account, channel.as_str()),
                    request,
                    result: Err(e.into()),
                }),
            },
            _ => {}
        }
    }

    /// Where Move up / Move down puts a channel or category: Modify Guild
    /// Channel Positions entries `{id, position, parent_id?}`, only for what
    /// changes. A swap with the neighbour exchanges the two positions; a
    /// channel at the edge of its category crosses into the next one (the
    /// first category's top goes out to the loose channels) and the group it
    /// joins is numbered anew. Text and voice channels keep to their own
    /// group, as the official sidebar lists them (text first).
    pub(super) fn move_plan(&self, guild: &str, id: &str, category: bool, up: bool) -> Vec<Value> {
        let Some(g) = self.store.guilds.get(guild) else {
            return Vec::new();
        };
        // Only what the sidebar shows (rooms.rs `visible`), so a move always
        // passes a visible neighbour.
        let mut all: Vec<&store::Channel> = g
            .channels
            .iter()
            .filter_map(|c| self.store.channels.get(c))
            .filter(|c| self.store.can(&c.id, p::VIEW_CHANNEL))
            .collect();
        // A category shows with a visible channel in it, or when we run it.
        let kept: Vec<String> = all
            .iter()
            .filter(|c| c.kind == store::GUILD_CATEGORY)
            .filter(|cat| {
                self.store.can(&cat.id, p::MANAGE_CHANNELS)
                    || all
                        .iter()
                        .any(|c| c.parent.as_deref() == Some(cat.id.as_str()) && c.kind != store::GUILD_CATEGORY)
            })
            .map(|c| c.id.clone())
            .collect();
        all.retain(|c| c.kind != store::GUILD_CATEGORY || kept.contains(&c.id));
        let key = |c: &&store::Channel| (c.position, c.id.clone());
        if category {
            let mut cats: Vec<&store::Channel> = all
                .iter()
                .copied()
                .filter(|c| c.kind == store::GUILD_CATEGORY)
                .collect();
            cats.sort_by_key(key);
            let Some(i) = cats.iter().position(|c| c.id == id) else {
                return Vec::new();
            };
            let j = if up {
                i.checked_sub(1)
            } else {
                Some(i + 1).filter(|j| *j < cats.len())
            };
            let Some(j) = j else { return Vec::new() };
            return swap_entries(&cats, i, j);
        }
        // Containers in sidebar order: the loose channels, then each category.
        let mut cats: Vec<&store::Channel> = all
            .iter()
            .copied()
            .filter(|c| c.kind == store::GUILD_CATEGORY)
            .collect();
        cats.sort_by_key(key);
        let parents: Vec<Option<&str>> = std::iter::once(None)
            .chain(cats.iter().map(|c| Some(c.id.as_str())))
            .collect();
        let movable =
            |c: &&store::Channel| (c.has_messages() || c.forum()) && !c.thread() && c.kind != store::GUILD_CATEGORY;
        // groups[container][0 text, 1 voice]
        let mut groups: Vec<[Vec<&store::Channel>; 2]> = parents
            .iter()
            .map(|p| {
                let mut two: [Vec<&store::Channel>; 2] = [Vec::new(), Vec::new()];
                for c in all
                    .iter()
                    .copied()
                    .filter(movable)
                    .filter(|c| c.parent.as_deref() == *p)
                {
                    two[usize::from(c.voice())].push(c);
                }
                for list in &mut two {
                    list.sort_by_key(key);
                }
                two
            })
            .collect();
        let Some((ci, gi, k)) = groups.iter().enumerate().find_map(|(ci, two)| {
            two.iter()
                .enumerate()
                .find_map(|(gi, list)| list.iter().position(|c| c.id == id).map(|k| (ci, gi, k)))
        }) else {
            return Vec::new();
        };
        let len = groups[ci][gi].len();
        if (up && k > 0) || (!up && k + 1 < len) {
            let j = if up { k - 1 } else { k + 1 };
            return swap_entries(&groups[ci][gi], k, j);
        }
        let dest = if up {
            ci.checked_sub(1)
        } else {
            Some(ci + 1).filter(|d| *d < groups.len())
        };
        let Some(dest) = dest else { return Vec::new() };
        let moved = groups[ci][gi].remove(k);
        let at = if up { groups[dest][gi].len() } else { 0 };
        groups[dest][gi].insert(at, moved);
        groups[dest][gi]
            .iter()
            .enumerate()
            .filter_map(|(n, c)| {
                let position = i64::try_from(n).unwrap_or(0);
                if c.id == id {
                    Some(json!({"id": c.id, "position": position, "parent_id": parents[dest]}))
                } else {
                    (c.position != position).then(|| json!({"id": c.id, "position": position}))
                }
            })
            .collect()
    }

    /// Demo mode: the CHANNEL_UPDATEs of a move, applied to the store.
    pub(super) fn demo_move(&mut self, moves: &Value, out: &mut Vec<Event>) {
        for m in moves.as_array().into_iter().flatten() {
            let Some(c) = m["id"].as_str().and_then(|id| self.store.channels.get_mut(id)) else {
                continue;
            };
            c.position = m["position"].as_i64().unwrap_or(c.position);
            if let Some(parent) = m.get("parent_id") {
                c.parent = parent.as_str().map(str::to_owned);
            }
        }
        self.sync_rooms(out);
        self.sync_servers(out);
    }

    /// Demo mode: the CHANNEL_CREATE a new channel brings.
    pub(super) fn demo_create(
        &mut self,
        guild: &str,
        name: &str,
        kind: u64,
        parent: Option<&str>,
        out: &mut Vec<Event>,
    ) {
        let position = self
            .store
            .channels
            .values()
            .filter(|c| c.guild.as_deref() == Some(guild) && c.parent.as_deref() == parent)
            .map(|c| c.position + 1)
            .max()
            .unwrap_or(0);
        // The next free id (made-up ones are all numbers).
        let id = self
            .store
            .channels
            .keys()
            .filter_map(|k| k.parse::<u64>().ok())
            .max()
            .map_or_else(crate::api::nonce, |n| n.saturating_add(1).to_string());
        let d = json!({
            "id": id, "type": kind, "guild_id": guild, "name": name,
            "position": position, "parent_id": parent,
        });
        self.dispatch("CHANNEL_CREATE", &d, out);
    }
}

/// The topic's limit: forums and media channels take more.
fn topic_max(c: &store::Channel) -> u32 {
    if c.forum() { FORUM_TOPIC_MAX } else { TOPIC_MAX }
}

/// The two entries that trade places: their positions swap, or (when they
/// tie) the whole list is numbered, sending only what changes.
fn swap_entries(list: &[&store::Channel], i: usize, j: usize) -> Vec<Value> {
    let (a, b) = (list[i], list[j]);
    if a.position != b.position {
        return vec![
            json!({"id": a.id, "position": b.position}),
            json!({"id": b.id, "position": a.position}),
        ];
    }
    let mut order: Vec<&store::Channel> = list.to_vec();
    order.swap(i, j);
    order
        .iter()
        .enumerate()
        .filter_map(|(n, c)| {
            let position = i64::try_from(n).unwrap_or(0);
            (c.position != position).then(|| json!({"id": c.id, "position": position}))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use godwit_core::server_settings::ServerAdmin;

    /// Server "10" where we (100) are the owner, or only a member, with
    /// categories "1" (position 0) and "2" (1): "1" holds text "11", "12" and
    /// voice "13", "2" holds text "21"; "30" is loose; "40" a forum.
    fn managed(owner: bool) -> (DiscordAccount, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
        let (net, mut cmds) = Handle::detached();
        let (_tx, events) = mpsc::channel();
        let mut a = DiscordAccount::with(u32::MAX, net, events, Status::Connecting);
        let ready = json!({
            "user": {"id": "100", "username": "me_bloop"},
            "guilds": [{
                "id": "10",
                "properties": {"name": "Qux Hall", "owner_id": if owner { "100" } else { "200" }},
                "roles": [{"id": "10", "name": "@everyone", "permissions": (p::VIEW_CHANNEL | p::SEND_MESSAGES).to_string(), "position": 0}],
                "channels": [
                    {"id": "1", "type": 4, "name": "Talk", "position": 0},
                    {"id": "2", "type": 4, "name": "Games", "position": 1},
                    {"id": "11", "type": 0, "name": "general", "position": 0, "parent_id": "1", "topic": "hello", "nsfw": false, "rate_limit_per_user": 5},
                    {"id": "12", "type": 0, "name": "news", "position": 1, "parent_id": "1"},
                    {"id": "13", "type": 2, "name": "lounge", "position": 2, "parent_id": "1"},
                    {"id": "21", "type": 0, "name": "lfg", "position": 0, "parent_id": "2"},
                    {"id": "30", "type": 0, "name": "rules", "position": 0},
                    {"id": "40", "type": 15, "name": "ideas", "position": 5}
                ]
            }],
            "private_channels": [], "relationships": [], "merged_members": [[]],
        });
        a.handle(Ev::Ready(Box::new(ready)), &mut Vec::new());
        while cmds.try_recv().is_ok() {}
        (a, cmds)
    }

    fn room(a: &DiscordAccount, id: &str) -> RoomId {
        RoomId::new(a.account.clone().unwrap(), id)
    }

    fn sent(cmds: &mut tokio::sync::mpsc::UnboundedReceiver<Cmd>) -> Vec<Request> {
        let mut out = Vec::new();
        while let Ok(c) = cmds.try_recv() {
            if let Cmd::Request(r) = c {
                out.push(r);
            }
        }
        out
    }

    fn overview(out: &[Event]) -> Result<Option<ChannelOverview>, Arc<str>> {
        match out {
            [Event::ChannelOverview { result, .. }, ..] => result.clone(),
            other => panic!("expected an overview, got {other:?}"),
        }
    }

    #[test]
    fn overview_shows_discords_fields_and_hides_the_icon() {
        let (mut a, _) = managed(true);
        let id = room(&a, "11");
        let o = overview(&a.channel_command(Command::LoadChannelOverview { room: id, request: 1 }))
            .unwrap()
            .unwrap();
        assert_eq!((o.name.as_ref(), o.description.as_ref()), ("general", "hello"));
        assert!(!o.icons && o.slowmode == Some(5) && !o.is_category && o.description_max == 1024);
        let forum = room(&a, "40");
        let o = overview(&a.channel_command(Command::LoadChannelOverview {
            room: forum,
            request: 2,
        }))
        .unwrap()
        .unwrap();
        assert_eq!(o.description_max, 4096);
        let cat = room(&a, "1");
        let o = overview(&a.channel_command(Command::LoadChannelOverview { room: cat, request: 3 }))
            .unwrap()
            .unwrap();
        assert!(o.is_category && o.slowmode.is_none());
    }

    #[test]
    fn saving_sends_only_the_changed_fields_as_modify_channel_takes_them() {
        let (mut a, mut cmds) = managed(true);
        let edit = ChannelOverviewEdit {
            name: Some(" talk ".into()),
            description: Some("new topic".into()),
            mature: Some(true),
            slowmode: Some(30),
            ..Default::default()
        };
        let out = a.channel_command(Command::SaveChannelOverview {
            room: room(&a, "11"),
            request: 7,
            edit: Box::new(edit),
        });
        assert!(out.is_empty());
        let requests = sent(&mut cmds);
        let [Request::ModifyChannel { channel, request, body }] = requests.as_slice() else {
            panic!("expected one Modify Channel");
        };
        assert_eq!((channel.as_str(), *request), ("11", 7));
        assert_eq!(
            *body,
            json!({"name": "talk", "topic": "new topic", "nsfw": true, "rate_limit_per_user": 30})
        );
        // Discord's answer updates the channel and the page.
        let updated = json!({"id": "11", "type": 0, "guild_id": "10", "name": "talk", "topic": "new topic", "nsfw": true, "rate_limit_per_user": 30, "parent_id": "1", "position": 0});
        let mut out = Vec::new();
        a.channel_answer(
            Answer::ChannelSaved {
                channel: "11".into(),
                request: 7,
                result: Ok(updated),
            },
            &mut out,
        );
        let o = overview(&out[out.len() - 1..]).unwrap().unwrap();
        assert!(o.mature && o.slowmode == Some(30) && o.name.as_ref() == "talk");
        assert_eq!(a.store.channels["11"].name, "talk");
    }

    #[test]
    fn bad_edits_and_missing_permissions_never_reach_discord() {
        let (mut a, mut cmds) = managed(true);
        let save = |a: &mut DiscordAccount, id: &str, edit: ChannelOverviewEdit| {
            let room = room(a, id);
            // Nothing back: the request went out; an event is the refusal.
            match a
                .channel_command(Command::SaveChannelOverview {
                    room,
                    request: 1,
                    edit: Box::new(edit),
                })
                .as_slice()
            {
                [] => Ok(()),
                other => overview(other).map(drop),
            }
        };
        let topic = |n: usize| ChannelOverviewEdit {
            description: Some("x".repeat(n).into()),
            ..Default::default()
        };
        assert!(save(&mut a, "11", topic(1025)).is_err());
        assert!(save(&mut a, "40", topic(4096)).is_ok());
        assert!(
            save(
                &mut a,
                "11",
                ChannelOverviewEdit {
                    name: Some("".into()),
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert!(
            save(
                &mut a,
                "11",
                ChannelOverviewEdit {
                    slowmode: Some(21_601),
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert_eq!(sent(&mut cmds).len(), 1, "only the forum's topic went out");
        // A plain member: no settings, no delete.
        let (mut b, mut cmds) = managed(false);
        assert!(!b.room("11").unwrap().capabilities.manage_channel);
        assert!(save(&mut b, "11", topic(3)).is_err());
        let id = room(&b, "11");
        let out = b.channel_command(Command::DeleteChannel { room: id, request: 2 });
        assert!(overview(&out).is_err());
        assert!(sent(&mut cmds).is_empty());
        // Managing needs the permission on that channel: an owner's channels are all open.
        assert!(a.room("11").unwrap().capabilities.manage_channel);
    }

    #[test]
    fn deleting_a_category_frees_its_channels_and_the_gateway_keeps_up() {
        let (mut a, mut cmds) = managed(true);
        let out = a.channel_command(Command::DeleteChannel {
            room: room(&a, "1"),
            request: 4,
        });
        assert!(out.is_empty());
        assert!(matches!(
            sent(&mut cmds).as_slice(),
            [Request::DeleteChannel { channel, request: 4 }] if channel == "1"
        ));
        let mut out = Vec::new();
        a.channel_answer(
            Answer::ChannelDeleted {
                channel: "1".into(),
                request: 4,
                result: Ok(()),
            },
            &mut out,
        );
        assert!(matches!(
            out.last(),
            Some(Event::ChannelOverview { result: Ok(None), .. })
        ));
        assert!(!a.store.channels.contains_key("1"));
        assert_eq!(a.store.channels["11"].parent, None);
        let servers = a.build_servers();
        assert!(servers[0].categories.iter().all(|c| c.title.as_ref() != "Talk"));
        // CHANNEL_CREATE, _UPDATE and _DELETE reach the sidebar without a request.
        let mut out = Vec::new();
        let d = json!({"id": "50", "type": 4, "guild_id": "10", "name": "Fresh", "position": 9});
        a.dispatch("CHANNEL_CREATE", &d, &mut out);
        let titles = |a: &DiscordAccount| -> Vec<String> {
            a.build_servers()[0]
                .categories
                .iter()
                .map(|c| c.title.to_string())
                .collect()
        };
        assert!(titles(&a).contains(&"Fresh".to_owned()));
        let d = json!({"id": "50", "type": 4, "guild_id": "10", "name": "Renamed", "position": 9});
        a.dispatch("CHANNEL_UPDATE", &d, &mut out);
        assert!(titles(&a).contains(&"Renamed".to_owned()));
        a.dispatch("CHANNEL_DELETE", &json!({"id": "50", "guild_id": "10"}), &mut out);
        assert!(!titles(&a).contains(&"Renamed".to_owned()));
        assert!(sent(&mut cmds).is_empty());
    }

    fn entries(a: &DiscordAccount, id: &str, category: bool, up: bool) -> Vec<Value> {
        a.move_plan("10", id, category, up)
    }

    #[test]
    fn moves_send_only_the_positions_that_change() {
        let (a, _) = managed(true);
        // A swap inside a category trades the two positions.
        assert_eq!(
            entries(&a, "12", false, true),
            [json!({"id": "12", "position": 0}), json!({"id": "11", "position": 1})]
        );
        // Voice and text keep apart: "13", the only voice channel there,
        // never trades with the text ones; it leaves for the loose voice group.
        assert_eq!(
            entries(&a, "13", false, true),
            [json!({"id": "13", "position": 0, "parent_id": null})]
        );
        // The first channel of the first category goes out to the loose ones
        // ("30" and the forum "40"), whose group is numbered anew.
        assert_eq!(
            entries(&a, "11", false, true),
            [
                json!({"id": "40", "position": 1}),
                json!({"id": "11", "position": 2, "parent_id": null})
            ]
        );
        // The first of the next category joins the end of the one above.
        assert_eq!(
            entries(&a, "21", false, true),
            [json!({"id": "21", "position": 2, "parent_id": "1"})]
        );
        // The last of a category goes to the top of the next, which is renumbered.
        assert_eq!(
            entries(&a, "12", false, false),
            [
                json!({"id": "12", "position": 0, "parent_id": "2"}),
                json!({"id": "21", "position": 1})
            ]
        );
        // Categories trade places; the edges stay put.
        assert_eq!(
            entries(&a, "1", true, false),
            [json!({"id": "1", "position": 1}), json!({"id": "2", "position": 0})]
        );
        assert!(entries(&a, "1", true, true).is_empty());
        assert!(entries(&a, "2", true, false).is_empty());
        assert!(entries(&a, "30", false, true).is_empty());
        assert!(entries(&a, "ghost", false, true).is_empty());
    }

    #[test]
    fn hidden_neighbours_are_skipped_so_a_move_shows() {
        // A plain member; "15" sits between "11" and "12" but is hidden from us.
        let (mut b, _) = managed(false);
        let mut out = Vec::new();
        let hidden = json!({
            "id": "15", "type": 0, "guild_id": "10", "name": "staff", "position": 1, "parent_id": "1",
            "permission_overwrites": [{"id": "10", "type": 0, "allow": "0", "deny": p::VIEW_CHANNEL.to_string()}]
        });
        b.dispatch("CHANNEL_CREATE", &hidden, &mut out);
        b.store.channels.get_mut("12").unwrap().position = 2;
        let plan = b.move_plan("10", "12", false, true);
        assert!(plan.iter().all(|e| e["id"] != "15"), "{plan:?}");
        assert_eq!(plan.len(), 2, "12 trades with the visible 11");
    }

    #[test]
    fn creating_and_moving_need_manage_channels_for_the_server() {
        let (mut a, mut cmds) = managed(true);
        let server = ServerId::new(a.account.clone().unwrap(), "10");
        let run = |a: &mut DiscordAccount, op| a.server_admin(server.clone(), op);
        let create = |kind, category: Option<&str>| ServerAdmin::CreateChannel {
            name: " Fun ".into(),
            kind,
            category: category.map(Into::into),
        };
        assert!(run(&mut a, create(NewChannelKind::Category, None)).is_empty());
        assert!(run(&mut a, create(NewChannelKind::Voice, Some("2"))).is_empty());
        assert!(
            run(
                &mut a,
                ServerAdmin::MoveChannel {
                    id: "1".into(),
                    category: true,
                    up: false
                }
            )
            .is_empty()
        );
        let r = sent(&mut cmds);
        assert!(
            matches!(&r[0], Request::Admin { op: Admin::CreateChannel { name, kind: 4, parent: None }, .. } if name == "Fun")
        );
        assert!(
            matches!(&r[1], Request::Admin { op: Admin::CreateChannel { kind: 2, parent: Some(p), .. }, .. } if p == "2")
        );
        assert!(
            matches!(&r[2], Request::Admin { op: Admin::MoveChannels(m), .. } if m.as_array().is_some_and(|m| m.len() == 2))
        );
        // Not into something that isn't a category of this server.
        let out = run(&mut a, create(NewChannelKind::Text, Some("11")));
        assert!(matches!(out.as_slice(), [Event::ServerAdmin { result: Err(_), .. }]));
        // A plain member is refused before anything is sent.
        let (mut b, mut cmds) = managed(false);
        let server = ServerId::new(b.account.clone().unwrap(), "10");
        for op in [
            create(NewChannelKind::Text, None),
            ServerAdmin::MoveChannel {
                id: "11".into(),
                category: false,
                up: true,
            },
        ] {
            let out = b.server_admin(server.clone(), op);
            assert!(matches!(out.as_slice(), [Event::ServerAdmin { result: Err(_), .. }]));
        }
        assert!(sent(&mut cmds).is_empty());
        let rights = b.build_servers()[0].rights;
        assert!(!rights.create_channels && !rights.manage_categories && !rights.reorder_channels);
        let rights = a.build_servers()[0].rights;
        assert!(rights.create_channels && rights.manage_categories && rights.reorder_channels);
    }

    #[test]
    fn demo_creates_edits_moves_and_deletes_like_the_gateway_would() {
        let ready = json!({
            "user": {"id": "100", "username": "me_bloop"},
            "guilds": [{
                "id": "10",
                "properties": {"name": "Qux Hall", "owner_id": "100"},
                "roles": [{"id": "10", "name": "@everyone", "permissions": "0", "position": 0}],
                "channels": [
                    {"id": "1", "type": 4, "name": "Talk", "position": 0},
                    {"id": "2", "type": 4, "name": "Games", "position": 1},
                    {"id": "11", "type": 0, "name": "general", "position": 0, "parent_id": "1"},
                    {"id": "21", "type": 0, "name": "lfg", "position": 0, "parent_id": "2"}
                ]
            }],
            "private_channels": [], "relationships": [], "merged_members": [[]],
        });
        let (mut a, _) = DiscordAccount::demo(ready, HashMap::new());
        let server = ServerId::new(a.account.clone().unwrap(), "10");
        let layout = |a: &DiscordAccount| -> Vec<(String, usize)> {
            a.build_servers()[0]
                .categories
                .iter()
                .map(|c| (c.title.to_string(), c.rooms.len()))
                .collect()
        };
        let admin = |a: &mut DiscordAccount, op| {
            let out = a.run(Command::ServerAdmin {
                server: server.clone(),
                op,
            });
            assert!(
                matches!(out.last(), Some(Event::ServerAdmin { result: Ok(_), .. })),
                "{out:?}"
            );
        };
        admin(
            &mut a,
            ServerAdmin::CreateChannel {
                name: "Fun".into(),
                kind: NewChannelKind::Category,
                category: None,
            },
        );
        admin(
            &mut a,
            ServerAdmin::CreateChannel {
                name: "chess".into(),
                kind: NewChannelKind::Text,
                category: Some("2".into()),
            },
        );
        let names = layout;
        assert_eq!(
            names(&a),
            [("Talk".to_owned(), 1), ("Games".into(), 2), ("Fun".into(), 0)]
        );
        // The last of "Talk" crosses into "Games"; the categories swap.
        admin(
            &mut a,
            ServerAdmin::MoveChannel {
                id: "11".into(),
                category: false,
                up: false,
            },
        );
        admin(
            &mut a,
            ServerAdmin::MoveChannel {
                id: "1".into(),
                category: true,
                up: false,
            },
        );
        assert_eq!(
            names(&a),
            [("Games".to_owned(), 3), ("Talk".into(), 0), ("Fun".into(), 0)]
        );
        // A save and a delete answer on the page.
        let id = room(&a, "21");
        let out = a.run(Command::SaveChannelOverview {
            room: id.clone(),
            request: 1,
            edit: Box::new(ChannelOverviewEdit {
                name: Some("looking".into()),
                slowmode: Some(10),
                ..Default::default()
            }),
        });
        let o = overview(&out[out.len() - 1..]).unwrap().unwrap();
        assert!(o.name.as_ref() == "looking" && o.slowmode == Some(10));
        let out = a.run(Command::DeleteChannel {
            room: room(&a, "2"),
            request: 2,
        });
        assert!(matches!(
            out.last(),
            Some(Event::ChannelOverview { result: Ok(None), .. })
        ));
        // Its channels are loose now (the headingless group first).
        assert_eq!(names(&a)[0], ("".to_owned(), 3));
    }
}
