// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The GIF picker's window side: fills the `Gifs` global from the core's
//! picker state, loading previews only for rows near the view (they're
//! looping WebM clips, decoded by godwit-media).

use std::cell::RefCell;
use std::collections::HashSet;

use godwit_core::application::AppState;
use godwit_core::gifs::{GifPicker, TRENDING};
use slint::{Image, ModelRc, VecModel};

use crate::images::{Fit, Images};
use crate::ui::GifTileItem;

/// A tile's picture: within the tile (168 × 112), corners cut by the tile.
pub const TILE: Fit = Fit::RoundedWithin(168, 112, 8);
/// Rows either side of the view that get their previews.
const MARGIN: usize = 3;
/// Two tiles to a row.
const COLUMNS: usize = 2;

/// What a tile does when picked.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    Search(String),
    Send(std::sync::Arc<str>),
    /// The starred GIFs (Favourites tile).
    Favourites,
}

// The starred GIFs' links, for the stars' look (`Gifs.is-favourite`, a pure
// callback that mustn't wait for the shell).
thread_local! {
    static STARRED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Keeps the stars in step with the saved favourites.
pub fn set_starred(favourites: &[crate::settings::FavouriteGif]) {
    STARRED.with(|s| *s.borrow_mut() = favourites.iter().map(|f| f.url.clone()).collect());
}

pub fn is_starred(url: &str) -> bool {
    STARRED.with(|s| s.borrow().contains(url))
}

/// Stars `url` (newest first) or, when starred, unstars it; true when starred now.
pub fn toggle_favourite(favourites: &mut Vec<crate::settings::FavouriteGif>, url: &str, picture: &str) -> bool {
    if favourites.iter().any(|f| f.url == url) {
        favourites.retain(|f| f.url != url);
        return false;
    }
    favourites.insert(
        0,
        crate::settings::FavouriteGif {
            url: url.to_owned(),
            picture: picture.to_owned(),
        },
    );
    favourites.truncate(crate::settings::MAX_GIF_FAVOURITES);
    true
}

#[derive(Default)]
pub struct GifTiles {
    pub model: std::rc::Rc<VecModel<GifTileItem>>,
    /// The top row on screen.
    pub first_row: usize,
    picks: Vec<Pick>,
    /// Animation slots of tiles near the view: these play.
    pub live: HashSet<usize>,
    /// Showing the starred GIFs instead of the network's.
    pub favourites: bool,
}

impl GifTiles {
    pub fn model(&self) -> ModelRc<GifTileItem> {
        ModelRc::from(self.model.clone())
    }

    pub fn pick(&self, index: usize) -> Option<&Pick> {
        self.picks.get(index)
    }

    /// Brings the tiles up to date with `picker`; returns the status line.
    pub fn show(
        &mut self,
        picker: Option<&GifPicker>,
        favourites: &[crate::settings::FavouriteGif],
        images: &mut Images,
    ) -> String {
        let default = GifPicker::default();
        let picker = picker.unwrap_or(&default);
        // (url of the picture, title, pick)
        let tiles: Vec<(Option<&str>, &str, Pick)> = if self.favourites {
            favourites
                .iter()
                .map(|f| (Some(f.picture.as_str()), "", Pick::Send(f.url.as_str().into())))
                .collect()
        } else if picker.query.is_empty() {
            // As the official picker: Trending first, then the categories,
            // with your Favourites (once there are any) between.
            std::iter::once((
                picker.categories.first().map(|c| &*c.image),
                "Trending GIFs",
                Pick::Search(TRENDING.to_owned()),
            ))
            .chain(
                favourites
                    .first()
                    .map(|f| (Some(f.picture.as_str()), "★ Favourites", Pick::Favourites)),
            )
            .chain(
                picker
                    .categories
                    .iter()
                    .map(|c| (Some(&*c.image), &*c.title, Pick::Search(c.title.to_string()))),
            )
            .collect()
        } else {
            picker
                .results
                .iter()
                .map(|g| (Some(&*g.preview), "", Pick::Send(g.url.clone())))
                .collect()
        };
        let near = |i: usize| {
            let row = i / COLUMNS;
            row + MARGIN >= self.first_row && row <= self.first_row + 4 + MARGIN
        };
        self.live.clear();
        let mut rows = Vec::with_capacity(tiles.len());
        for (i, (url, title, pick)) in tiles.iter().enumerate() {
            let (image, anim) = match url.filter(|_| near(i)).and_then(|u| images.get_animated(u, TILE)) {
                Some((image, slot)) => (Some(image), slot),
                None => (None, -1),
            };
            if let Ok(slot) = usize::try_from(anim) {
                self.live.insert(slot);
            }
            // GIFs (not category tiles) get a star.
            let (gif, picture) = match pick {
                Pick::Send(gif) => (gif.to_string(), url.unwrap_or_default().to_owned()),
                _ => (String::new(), String::new()),
            };
            rows.push(GifTileItem {
                gif: gif.into(),
                picture: picture.into(),
                has_image: image.is_some(),
                image: image.unwrap_or_else(Image::default),
                anim,
                title: (*title).into(),
            });
        }
        crate::view::sync(&self.model, rows);
        self.picks = tiles.into_iter().map(|(_, _, pick)| pick).collect();
        if self.favourites {
            if favourites.is_empty() {
                "No favourites yet: star a GIF to keep it here.".to_owned()
            } else {
                String::new()
            }
        } else if let Some(error) = &picker.error {
            format!("Couldn't load GIFs: {error}")
        } else if picker.loading && picker.results.is_empty() {
            "Loading…".to_owned()
        } else if !picker.query.is_empty() && !picker.loading && picker.results.is_empty() {
            "No GIFs found.".to_owned()
        } else {
            String::new()
        }
    }
}

/// The picker for the selected room's account, if it allows GIFs.
pub fn picker_for(state: &AppState) -> Option<&GifPicker> {
    let room = state.selected.as_ref()?;
    state.gifs.get(&room.account)
}

/// Whether the selected room can send GIFs.
pub fn available(state: &AppState) -> bool {
    state
        .selected
        .as_ref()
        .and_then(|r| state.room(r))
        .is_some_and(|r| r.capabilities.gifs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starring_twice_unstars_and_newest_comes_first() {
        let mut favourites = Vec::new();
        assert!(toggle_favourite(&mut favourites, "https://a", "https://a.webm"));
        assert!(toggle_favourite(&mut favourites, "https://b", "https://b.webm"));
        assert_eq!(favourites[0].url, "https://b");
        assert!(!toggle_favourite(&mut favourites, "https://a", ""));
        assert_eq!(favourites.len(), 1);
    }
}
