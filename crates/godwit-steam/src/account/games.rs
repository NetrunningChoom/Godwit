// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Game invites as activity cards, as Steam's chat shows them: the game's
//! capsule, its name (looked up like "Playing …"'s) and Join, which hands
//! Steam its own link to start the game or join the lobby.

use super::*;
use crate::bbcode::{GameInvite, GameJoin};
use godwit_core::message::{Embed, EmbedStyle};

/// Messages whose cards wait for a game's name, at most this many (the
/// oldest give up first; a name that never comes mustn't grow the list).
const NAME_WAITS: usize = 64;

/// A message shown before its games' names were known.
pub(super) struct NameWait {
    room: RoomId,
    id: MessageId,
    inviter: u64,
    games: Vec<GameInvite>,
}

/// A game's 184 × 69 capsule, the picture Steam's invite card shows.
fn capsule_url(app: u32) -> String {
    format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app}/capsule_184x69.jpg")
}

/// Steam's link that accepts the invite, as its client's Accept does:
/// `steam://run/<app>//<args>/` starts the game with its connect string
/// (Steam asks before passing arguments on), `steam://joinlobby/<app>/<lobby>/<inviter>`
/// joins the lobby. Valve's documented steam:// commands.
fn join_url(invite: &GameInvite, inviter: u64) -> String {
    let app = invite.app;
    match &invite.join {
        GameJoin::Connect(args) if args.trim().is_empty() => format!("steam://run/{app}"),
        GameJoin::Connect(args) => {
            format!(
                "steam://run/{app}//{}/",
                godwit_core::encoding::percent_encode(args.trim())
            )
        }
        GameJoin::Lobby(lobby) => format!("steam://joinlobby/{app}/{lobby}/{inviter}"),
    }
}

impl SteamAccount {
    /// The cards for a message's game invites from `inviter`. Unknown game
    /// names are looked up, and the cards are sent again once they arrive.
    pub(super) fn game_embeds(
        &mut self,
        room: Option<RoomId>,
        id: &MessageId,
        inviter: u64,
        games: Vec<GameInvite>,
    ) -> Vec<Embed> {
        let embeds = games.iter().map(|g| self.game_embed(g, inviter)).collect();
        let ask: Vec<u32> = games
            .iter()
            .map(|g| g.app)
            .filter(|a| !self.app_names.contains_key(a))
            .collect();
        if ask.is_empty() {
            return embeds;
        }
        let new: Vec<u32> = ask.into_iter().filter(|a| self.asked_apps.insert(*a)).collect();
        if !new.is_empty() {
            self.net.send(Cmd::AppNames(new));
        }
        if let Some(room) = room
            && !self.name_waits.iter().any(|w| w.id == *id && w.room == room)
        {
            if self.name_waits.len() >= NAME_WAITS {
                self.name_waits.remove(0);
            }
            self.name_waits.push(NameWait {
                room,
                id: id.clone(),
                inviter,
                games,
            });
        }
        embeds
    }

    fn game_embed(&self, invite: &GameInvite, inviter: u64) -> Embed {
        Embed {
            site_name: Some("Game invite".into()),
            title: Some(
                self.app_names
                    .get(&invite.app)
                    .map_or("A Steam game", String::as_str)
                    .into(),
            ),
            image_url: Some(capsule_url(invite.app).into()),
            // Our own invites have nothing to join from here.
            url: (Some(inviter) != self.me).then(|| join_url(invite, inviter).into()),
            style: EmbedStyle::Activity,
            ..Embed::default()
        }
    }

    /// Game names arrived: cards that waited for them, with their names.
    pub(super) fn games_named(&mut self, out: &mut Vec<Event>) {
        let waits = std::mem::take(&mut self.name_waits);
        for w in waits {
            if w.games.iter().all(|g| self.app_names.contains_key(&g.app)) {
                let embeds = w.games.iter().map(|g| self.game_embed(g, w.inviter)).collect();
                out.push(Event::EmbedsUpdated {
                    room: w.room,
                    id: w.id,
                    embeds,
                });
            } else {
                self.name_waits.push(w);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_links_are_steams_own() {
        let game = |join| GameInvite { app: 730, join };
        assert_eq!(join_url(&game(GameJoin::Connect(String::new())), 7), "steam://run/730");
        assert_eq!(
            join_url(&game(GameJoin::Connect(" +connect 1.2.3.4".into())), 7),
            "steam://run/730//%2Bconnect%201.2.3.4/"
        );
        assert_eq!(
            join_url(&game(GameJoin::Lobby(99)), 7656),
            "steam://joinlobby/730/99/7656"
        );
    }
}
