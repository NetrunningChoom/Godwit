// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! People's game libraries (for "Games in common"): `Player.GetOwnedGames`
//! over the client connection, as Steam's own client asks for a friend's
//! games, one request per person and only when the dialog asks.

use std::sync::Arc;

use godwit_core::games::{Library, OwnedGame};
use steam_vent::{Connection, ConnectionTrait, EResult, NetworkError};
use steam_vent_proto_steam::steammessages_player_steamclient::{
    CPlayer_GetOwnedGames_Request, CPlayer_GetOwnedGames_Response,
};

/// A game's wide store picture (Steam's CDN).
pub fn capsule_url(app: u64) -> String {
    format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app}/capsule_184x69.jpg")
}

/// `steam_id`'s games.
pub async fn owned_games(conn: &Connection, steam_id: u64) -> Library {
    let req = CPlayer_GetOwnedGames_Request {
        steamid: Some(steam_id),
        // Names come with the list (no separate lookup per game), and free
        // games played count as owned, as the client's library shows them.
        include_appinfo: Some(true),
        include_played_free_games: Some(true),
        ..Default::default()
    };
    match conn.service_method(req).await {
        Ok(res) => library(res),
        Err(NetworkError::ApiError(EResult::AccessDenied)) => Library::Private,
        Err(e) => Library::Failed(format!("Steam didn't send the games: {e}").into()),
    }
}

/// The answer as a library. Steam answers a hidden library with an empty
/// list, so an empty one is reported as private, not as owning nothing.
pub fn library(res: CPlayer_GetOwnedGames_Response) -> Library {
    let games: Vec<OwnedGame> = res
        .games
        .into_iter()
        .filter_map(|g| {
            let id = u64::try_from(g.appid?).ok()?;
            let name = g.name.filter(|n| !n.is_empty()).unwrap_or_else(|| format!("App {id}"));
            Some(OwnedGame {
                id,
                name: name.into(),
                image_url: Some(capsule_url(id).into()),
            })
        })
        .collect();
    if games.is_empty() {
        Library::Private
    } else {
        Library::Games(Arc::from(games))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steam_vent_proto_steam::steammessages_player_steamclient::cplayer_get_owned_games_response::Game;

    fn game(appid: i32, name: Option<&str>) -> Game {
        Game {
            appid: Some(appid),
            name: name.map(str::to_owned),
            ..Default::default()
        }
    }

    #[test]
    fn games_map_with_names_and_capsules() {
        let res = CPlayer_GetOwnedGames_Response {
            game_count: Some(3),
            games: vec![
                game(440, Some("Team Fortress 2")),
                game(10, None),
                game(-1, Some("bad")),
            ],
            ..Default::default()
        };
        let Library::Games(games) = library(res) else {
            panic!("expected games");
        };
        assert_eq!(games.len(), 2);
        assert_eq!(&*games[0].name, "Team Fortress 2");
        assert_eq!(
            games[0].image_url.as_deref(),
            Some("https://cdn.cloudflare.steamstatic.com/steam/apps/440/capsule_184x69.jpg")
        );
        // Without a name, the app id stands in.
        assert_eq!(&*games[1].name, "App 10");
    }

    #[test]
    fn an_empty_answer_is_a_private_library() {
        assert_eq!(library(CPlayer_GetOwnedGames_Response::default()), Library::Private);
    }
}
