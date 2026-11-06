// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Steam group chats (`ChatRoom.*`): the account's groups and their rooms,
//! history, sending and read state. The order and fields follow vaporchat's
//! `steam/groups.ts` and `steam/messages.ts` (github.com/vaporlens/vaporchat,
//! MIT), which follow Steam's web chat.

use steam_vent::{Connection, ConnectionTrait};
use steam_vent_proto_steam::steammessages_chat_steamclient::{
    CChatRoom_AckChatMessage_Notification, CChatRoom_GetChatRoomGroupSummary_Request,
    CChatRoom_GetChatRoomGroupSummary_Response, CChatRoom_GetMessageHistory_Request,
    CChatRoom_GetMyChatRoomGroups_Request, CChatRoom_JoinChatRoomGroup_Request, CChatRoom_SendChatMessage_Request,
    CChatRoom_SetSessionActiveChatRoomGroups_Request, CChatRoomState,
};

use crate::backend::Line;

/// A page of group history.
const PAGE: u32 = 50;

/// One of the account's group chats.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GroupInfo {
    pub id: u64,
    /// "" for an ad-hoc chat nobody named (Steam shows its members' names).
    pub name: String,
    /// The Steam community group (clan) it belongs to, if any.
    pub clan: Option<u32>,
    pub avatar_url: Option<String>,
    /// Some of its members' account ids (all of them for small groups).
    pub members: Vec<u32>,
    /// Members' ranks where Steam said (`EChatRoomGroupRank`: 50 owner,
    /// 40 officer, 30 moderator, 20 member, 15 guest), by account id.
    pub ranks: std::collections::BTreeMap<u32, i32>,
    pub default_chat: u64,
    pub rooms: Vec<GroupRoom>,
}

/// A room (channel) of a group chat. Every room has text; some also voice.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GroupRoom {
    pub chat: u64,
    /// Steam leaves the default room unnamed and shows it as "General".
    pub name: String,
    pub voice_allowed: bool,
    /// Account ids in its voice chat now.
    pub in_voice: Vec<u32>,
    pub sort_order: u32,
    /// Its newest message's time, and the last we've read (0: unknown).
    pub last_message: u32,
    pub last_read: u32,
}

impl GroupInfo {
    /// Shown as a server (its own rail entry with channels) rather than one
    /// chat: a Steam community group's chat, or one with more than one room.
    /// An ad-hoc chat with one room reads as a group DM, as in Steam's list.
    pub fn is_server(&self) -> bool {
        self.clan.is_some() || self.rooms.len() > 1
    }
}

fn room(state: &CChatRoomState) -> GroupRoom {
    GroupRoom {
        chat: state.chat_id(),
        name: match state.chat_name() {
            "" => "General".to_owned(),
            name => name.to_owned(),
        },
        voice_allowed: state.voice_allowed(),
        in_voice: state.members_in_voice.clone(),
        sort_order: state.sort_order(),
        last_message: state.time_last_message(),
        last_read: 0,
    }
}

/// The account's groups with their rooms, and subscribes this session to
/// them (without that, Steam sends no messages or voice changes for them).
pub async fn fetch(conn: &Connection) -> Result<Vec<GroupInfo>, String> {
    let res = conn
        .service_method(CChatRoom_GetMyChatRoomGroups_Request::default())
        .await
        .map_err(|e| e.to_string())?;
    let mut groups: Vec<GroupInfo> = res
        .chat_room_groups
        .iter()
        .filter_map(|pair| {
            let s = pair.group_summary.as_ref()?;
            let read: Vec<(u64, u32)> = pair
                .user_chat_group_state
                .as_ref()
                .map(|u| {
                    u.user_chat_room_state
                        .iter()
                        .map(|r| (r.chat_id(), r.time_last_ack()))
                        .collect()
                })
                .unwrap_or_default();
            let mut rooms: Vec<GroupRoom> = s.chat_rooms.iter().map(room).collect();
            for r in &mut rooms {
                r.last_read = read.iter().find(|(c, _)| *c == r.chat).map_or(0, |(_, t)| *t);
            }
            Some(summary_info(s, rooms))
        })
        .collect();
    if groups.is_empty() {
        return Ok(groups);
    }
    let ids: Vec<u64> = groups.iter().map(|g| g.id).collect();
    let req = CChatRoom_SetSessionActiveChatRoomGroups_Request {
        chat_group_ids: ids.clone(),
        chat_groups_data_requested: ids,
        ..Default::default()
    };
    // The active state has what summaries leave out: every member of smaller
    // groups, and rooms' voice details.
    if let Ok(active) = conn.service_method(req).await {
        for state in &active.chat_states {
            let Some(header) = state.header_state.as_ref() else {
                continue;
            };
            let Some(group) = groups.iter_mut().find(|g| g.id == header.chat_group_id()) else {
                continue;
            };
            if !state.members.is_empty() {
                group.members = state.members.iter().map(|m| m.accountid()).collect();
                group.ranks = state.members.iter().map(|m| (m.accountid(), m.rank() as i32)).collect();
            }
            for full in &state.chat_rooms {
                let fresh = room(full);
                if let Some(r) = group.rooms.iter_mut().find(|r| r.chat == fresh.chat) {
                    let last_read = r.last_read;
                    *r = GroupRoom { last_read, ..fresh };
                }
            }
        }
    }
    Ok(groups)
}

fn summary_info(s: &CChatRoom_GetChatRoomGroupSummary_Response, mut rooms: Vec<GroupRoom>) -> GroupInfo {
    rooms.sort_by_key(|r| r.sort_order);
    GroupInfo {
        id: s.chat_group_id(),
        name: s.chat_group_name().to_owned(),
        clan: Some(s.clanid()).filter(|c| *c != 0),
        avatar_url: avatar(s.chat_group_avatar_sha(), s.avatar_ugc_url()),
        members: s.top_members.clone(),
        ranks: std::collections::BTreeMap::new(),
        default_chat: s.default_chat_id(),
        rooms,
    }
}

/// A group we may not be in (an invite's card): its summary, and how many
/// are in it.
pub async fn summary(conn: &Connection, group: u64) -> Result<(GroupInfo, u32), String> {
    let req = CChatRoom_GetChatRoomGroupSummary_Request {
        chat_group_id: Some(group),
        ..Default::default()
    };
    let s = conn.service_method(req).await.map_err(|e| e.to_string())?;
    let rooms = s.chat_rooms.iter().map(room).collect();
    Ok((summary_info(&s, rooms), s.active_member_count()))
}

/// Joins a group we were invited to (`chat`: the room the invite named, 0: its default).
pub async fn join(conn: &Connection, group: u64, chat: u64) -> Result<(), String> {
    let req = CChatRoom_JoinChatRoomGroup_Request {
        chat_group_id: Some(group),
        chat_id: Some(chat).filter(|c| *c != 0),
        ..Default::default()
    };
    conn.service_method(req).await.map_err(|e| e.to_string())?;
    Ok(())
}

/// A group's picture: its uploaded one, or by hash like an avatar. Groups
/// without one have none (unlike people, there's no default).
fn avatar(sha: &[u8], ugc: &str) -> Option<String> {
    if ugc.starts_with("https://") {
        return Some(ugc.to_owned());
    }
    if sha.iter().all(|b| *b == 0) {
        return None;
    }
    let hex: String = sha.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!("https://avatars.steamstatic.com/{hex}_medium.jpg"))
}

/// A page of a room's history before `(time, ordinal)` (None: newest),
/// newest first as Steam sends it; and whether there's more.
pub async fn history(
    conn: &Connection,
    group: u64,
    chat: u64,
    before: Option<(u32, u32)>,
) -> Result<(Vec<Line>, bool), String> {
    let req = CChatRoom_GetMessageHistory_Request {
        chat_group_id: Some(group),
        chat_id: Some(chat),
        max_count: Some(PAGE),
        last_time: before.map(|b| b.0),
        last_ordinal: before.map(|b| b.1),
        ..Default::default()
    };
    let res = conn.service_method(req).await.map_err(|e| e.to_string())?;
    let lines = res
        .messages
        .iter()
        // Server messages (joins, renames) and deleted ones aren't chat.
        .filter(|m| m.server_message.is_none() && !m.deleted())
        .map(|m| Line {
            from: m.sender(),
            time: m.server_timestamp(),
            ordinal: m.ordinal(),
            text: m.message().to_owned(),
        })
        .filter(|l| before != Some((l.time, l.ordinal)))
        .collect();
    Ok((lines, res.more_available()))
}

/// Sends to a room; returns the line as Steam stored it.
pub async fn send(conn: &Connection, group: u64, chat: u64, text: &str) -> Result<Line, String> {
    // As Steam's clients send it (vaporchat's `sendMessage`): BBCode with our
    // own brackets escaped, so `:name:` and `/sticker <name>` convert.
    let message = text.replace('[', "\\[");
    let req = CChatRoom_SendChatMessage_Request {
        chat_group_id: Some(group),
        chat_id: Some(chat),
        message: Some(message.clone()),
        echo_to_sender: Some(true),
        ..Default::default()
    };
    let res = conn.service_method(req).await.map_err(|e| e.to_string())?;
    Ok(Line {
        from: crate::backend::account_id(conn.steam_id().steam64()),
        time: res.server_timestamp.unwrap_or_else(crate::backend::now),
        ordinal: res.ordinal(),
        text: res
            .modified_message
            .clone()
            .filter(|t| !t.is_empty())
            .unwrap_or(message),
    })
}

/// Marks a room read up to `time` (a notification, as Steam's clients send it).
pub async fn ack(conn: &Connection, group: u64, chat: u64, time: u32) {
    let req = CChatRoom_AckChatMessage_Notification {
        chat_group_id: Some(group),
        chat_id: Some(chat),
        timestamp: Some(time),
        ..Default::default()
    };
    if let Err(e) = conn.notify_service(req).await {
        crate::backend::diag(&format!("group: marking read failed: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(clan: Option<u32>, rooms: usize) -> GroupInfo {
        GroupInfo {
            clan,
            rooms: vec![GroupRoom::default(); rooms],
            ..Default::default()
        }
    }

    #[test]
    fn community_groups_and_several_rooms_are_servers() {
        assert!(info(Some(7), 1).is_server(), "a Steam group's chat");
        assert!(info(None, 2).is_server(), "an ad-hoc chat with channels added");
        assert!(!info(None, 1).is_server(), "a plain ad-hoc chat is a group DM");
    }

    #[test]
    fn group_pictures() {
        assert_eq!(avatar(&[0; 20], ""), None);
        assert_eq!(
            avatar(&[0xab, 0x01], ""),
            Some("https://avatars.steamstatic.com/ab01_medium.jpg".to_owned())
        );
        assert_eq!(
            avatar(&[1], "https://example.com/a.png"),
            Some("https://example.com/a.png".to_owned())
        );
    }
}
