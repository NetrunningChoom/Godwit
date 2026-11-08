// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The friends list's favourites, as the official client keeps them: one
//! list per account on Steam's side (`FriendsList.GetFavorites` /
//! `SetFavorites`, and `FriendsListClient.FavoritesChanged` when another
//! client changes it). The list also holds groups and group chats; Godwit
//! only stars people, so those entries are kept as they are when writing.

use steam_vent::{Connection, ConnectionTrait};
use steam_vent_proto_common::protobuf::Message;
use steam_vent_proto_common::{RpcMessage, RpcMethod};

#[allow(unknown_lints, clippy::all, dead_code, missing_docs, non_camel_case_types, non_snake_case)]
#[allow(non_upper_case_globals, trivial_casts, unused_results, unused_mut, unused_attributes)]
#[rustfmt::skip]
mod proto {
    include!(concat!(env!("OUT_DIR"), "/friendslist.rs"));
}

pub use proto::CFriendsListFavoriteEntry as Entry;
use proto::*;

macro_rules! rpc_message {
    ($($ty:ty),* $(,)?) => {$(
        impl RpcMessage for $ty {
            fn parse(reader: &mut dyn std::io::Read) -> steam_vent_proto_common::protobuf::Result<Self> {
                <Self as Message>::parse_from_reader(reader)
            }
            fn write(&self, writer: &mut dyn std::io::Write) -> steam_vent_proto_common::protobuf::Result<()> {
                self.write_to_writer(writer)
            }
            fn encode_size(&self) -> usize {
                self.compute_size() as usize
            }
        }
    )*};
}

rpc_message!(
    CFriendsList_GetFavorites_Request,
    CFriendsList_GetFavorites_Response,
    CFriendsList_SetFavorites_Request,
    CFriendsList_SetFavorites_Response,
    CFriendsList_FavoritesChanged_Notification,
);

impl RpcMethod for CFriendsList_GetFavorites_Request {
    const METHOD_NAME: &'static str = "FriendsList.GetFavorites#1";
    type Response = CFriendsList_GetFavorites_Response;
}

impl RpcMethod for CFriendsList_SetFavorites_Request {
    const METHOD_NAME: &'static str = "FriendsList.SetFavorites#1";
    type Response = CFriendsList_SetFavorites_Response;
}

/// Sent by Steam when the list changes elsewhere.
pub type Changed = CFriendsList_FavoritesChanged_Notification;

impl RpcMethod for CFriendsList_FavoritesChanged_Notification {
    const METHOD_NAME: &'static str = "FriendsListClient.FavoritesChanged#1";
    type Response = ();
}

/// The whole list.
pub async fn get(conn: &Connection) -> Result<Vec<Entry>, String> {
    let res = conn
        .service_method(CFriendsList_GetFavorites_Request::default())
        .await
        .map_err(|e| e.to_string())?;
    Ok(res.favorites)
}

/// Stars or unstars `friend` (an account id): reads the list fresh, so
/// stars set elsewhere aren't lost, and writes it back if it changed.
/// The list as it is now.
pub async fn set(conn: &Connection, friend: u32, favourite: bool) -> Result<Vec<Entry>, String> {
    let list = get(conn).await?;
    let Some(list) = toggled(list.clone(), friend, favourite) else {
        return Ok(list);
    };
    let req = CFriendsList_SetFavorites_Request {
        favorites: list.clone(),
        ..Default::default()
    };
    conn.service_method(req).await.map_err(|e| e.to_string())?;
    Ok(list)
}

/// The people in the list (account ids), in its order.
pub fn people(list: &[Entry]) -> Vec<u32> {
    list.iter().filter_map(|e| e.accountid).filter(|a| *a != 0).collect()
}

/// `list` with `friend` starred (added last, as the official client adds
/// to the end) or unstarred; None if that's how it already is. Groups and
/// group chats stay as they are.
pub fn toggled(mut list: Vec<Entry>, friend: u32, favourite: bool) -> Option<Vec<Entry>> {
    let has = list.iter().any(|e| e.accountid == Some(friend));
    if has == favourite || friend == 0 {
        return None;
    }
    if favourite {
        list.push(Entry {
            accountid: Some(friend),
            ..Default::default()
        });
    } else {
        list.retain(|e| e.accountid != Some(friend));
    }
    Some(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(a: u32) -> Entry {
        Entry {
            accountid: Some(a),
            ..Default::default()
        }
    }

    fn group(c: u32) -> Entry {
        Entry {
            clanid: Some(c),
            ..Default::default()
        }
    }

    #[test]
    fn starring_adds_at_the_end_and_keeps_groups() {
        let list = toggled(vec![group(9), person(1)], 2, true).unwrap_or_default();
        assert_eq!(list, vec![group(9), person(1), person(2)]);
        assert_eq!(people(&list), vec![1, 2]);
    }

    #[test]
    fn unstarring_removes_only_that_person() {
        let list = toggled(vec![person(1), group(9), person(2)], 1, false).unwrap_or_default();
        assert_eq!(list, vec![group(9), person(2)]);
    }

    #[test]
    fn no_change_writes_nothing() {
        assert!(toggled(vec![person(1)], 1, true).is_none());
        assert!(toggled(vec![person(1)], 2, false).is_none());
        assert!(toggled(Vec::new(), 0, true).is_none());
    }

    #[test]
    fn groups_and_empty_ids_are_not_people() {
        let list = vec![group(9), person(0), person(3)];
        assert_eq!(people(&list), vec![3]);
    }

    #[test]
    fn method_names_match_the_services() {
        assert_eq!(
            CFriendsList_GetFavorites_Request::METHOD_NAME,
            "FriendsList.GetFavorites#1"
        );
        assert_eq!(
            CFriendsList_SetFavorites_Request::METHOD_NAME,
            "FriendsList.SetFavorites#1"
        );
        assert_eq!(Changed::METHOD_NAME, "FriendsListClient.FavoritesChanged#1");
    }
}
