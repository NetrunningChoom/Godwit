// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The picker's Stickers tab: fills the `Gifs` global's sticker tiles from
//! the core's per-room sticker list. Only rows near the view get their
//! pictures (still ones: an animated sticker shows its first frame).

use std::sync::Arc;

use godwit_core::application::AppState;
use godwit_core::stickers::Sticker;
use slint::{Image, ModelRc, VecModel};

use crate::images::{Fit, Images};
use crate::ui::StickerTileItem;

/// Whether the selected room can send stickers.
pub fn available(state: &AppState) -> bool {
    state
        .selected
        .as_ref()
        .and_then(|r| state.room(r))
        .is_some_and(|r| r.capabilities.stickers)
}

/// A sticker's picture: 80 px tiles, sharp on a 2× screen.
pub const STICKER: Fit = Fit::Within(160, 160);
/// Tiles to a row.
const COLUMNS: usize = 4;
/// Rows either side of the view that get their pictures.
const MARGIN: usize = 1;
/// Rows the list shows at once.
const VISIBLE: usize = 4;

#[derive(Default)]
pub struct StickerTiles {
    model: std::rc::Rc<VecModel<StickerTileItem>>,
    /// The top row on screen.
    pub first_row: usize,
    /// The search shown, lowercased.
    pub query: String,
    /// What each tile sends (None while the network refuses it).
    picks: Vec<Option<Arc<str>>>,
}

impl StickerTiles {
    pub fn model(&self) -> ModelRc<StickerTileItem> {
        ModelRc::from(self.model.clone())
    }

    pub fn pick(&self, index: usize) -> Option<Arc<str>> {
        self.picks.get(index).cloned().flatten()
    }

    /// The stickers of the selected room matching the search.
    pub fn listed<'a>(state: &'a AppState, query: &str) -> Vec<&'a Sticker> {
        let Some(room) = state.selected.as_ref() else {
            return Vec::new();
        };
        state
            .stickers_for(room)
            .into_iter()
            .flat_map(|g| &g.stickers)
            .filter(|s| s.matches(query))
            .collect()
    }

    /// Brings the tiles up to date; returns the status line. `open` false
    /// empties them (nothing is decoded while the picker is shut).
    pub fn show(&mut self, state: &AppState, open: bool, images: &mut Images) -> String {
        let list = if open {
            Self::listed(state, &self.query)
        } else {
            Vec::new()
        };
        let near = |i: usize| {
            let row = i / COLUMNS;
            row + MARGIN >= self.first_row && row <= self.first_row + VISIBLE + MARGIN
        };
        let rows: Vec<StickerTileItem> = list
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let image = near(i).then(|| images.get(&s.image_url, STICKER)).flatten();
                StickerTileItem {
                    name: (*s.name).into(),
                    has_image: image.is_some(),
                    image: image.unwrap_or_else(Image::default),
                    enabled: s.available,
                }
            })
            .collect();
        crate::view::sync(&self.model, rows);
        self.picks = list.iter().map(|s| s.available.then(|| s.id.clone())).collect();
        if !open {
            String::new()
        } else if list.is_empty() {
            if self.query.is_empty() {
                "This server has no stickers.".to_owned()
            } else {
                "No stickers found.".to_owned()
            }
        } else {
            String::new()
        }
    }
}
