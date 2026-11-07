// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Pictures and video: the inline players, the viewer, the cropper and GIFs.

use super::*;

impl Shell {
    /// The player of `url`, as one of the timeline's rows shows it.
    pub(super) fn player_spec(&self, url: &str) -> Option<crate::inline_player::Spec> {
        self.presenter
            .rows
            .iter()
            .flat_map(|row| row.player_specs())
            .find(|spec| spec.url == url)
    }

    /// The play button on an embed that didn't load by itself (too many
    /// players, or it failed before): loads it in place of the oldest.
    pub fn play_inline(&mut self, ui: &AppWindow, url: &str, _kind: &str) {
        let Some(spec) = self.player_spec(url) else { return };
        let scale = slint::ComponentHandle::window(ui).scale_factor();
        match self.inline_players.start(&spec, scale, true, self.inline_wake.clone()) {
            Ok(_) => {}
            Err(error) => {
                godwit_core::verbose!("note: inline playback is unavailable ({error})");
                if crate::inline_player::pop_out(&spec).is_err() {
                    open_external(url);
                }
            }
        }
        self.invalidate_player_rows(url);
        self.presenter.images_changed = true;
        self.refresh(ui);
    }

    /// The player's fullscreen button, or its page asked for fullscreen: it
    /// shows the same live helper in another window, preserving its state.
    pub fn player_fullscreen(&mut self, ui: &AppWindow, url: &str) {
        let Some(spec) = self.player_spec(url) else { return };
        if self
            .fullscreen_player
            .as_ref()
            .is_some_and(|player| player.url() == url)
        {
            self.close_fullscreen_player();
            return;
        }
        let scale = slint::ComponentHandle::window(ui).scale_factor();
        if let Err(error) = self.inline_players.start(&spec, scale, true, self.inline_wake.clone()) {
            godwit_core::verbose!("note: inline playback is unavailable ({error})");
            if crate::inline_player::pop_out(&spec).is_err() {
                open_external(url);
            }
            return;
        }
        self.inline_players.keep_playing(url);
        self.close_fullscreen_player();
        match crate::player_window::FullscreenPlayer::new(&spec, &self.inline_players, self.inline_wake.clone()) {
            Ok(player) => {
                self.inline_players.set_fullscreen(url, true);
                self.fullscreen_player = Some(player);
            }
            Err(error) => godwit_core::verbose!("note: the player window is unavailable ({error})"),
        }
        self.invalidate_player_rows(url);
        self.presenter.images_changed = true;
        self.refresh(ui);
    }

    pub(super) fn close_fullscreen_player(&mut self) {
        if let Some(player) = self.fullscreen_player.take() {
            self.inline_players.set_fullscreen(player.url(), false);
        }
    }

    /// Loads the players of the rows on screen, as the official client's
    /// lazy iframes. When all slots (inline_player::MAX_PLAYERS) are taken,
    /// players scrolled off screen make room for ones on it.
    pub(super) fn autoload_players(&mut self, ui: &AppWindow) {
        let Some((top, bottom)) = self.presenter.window else {
            return;
        };
        let on_screen: Vec<_> = self
            .presenter
            .rows
            .iter()
            .enumerate()
            .filter(|(i, _)| *i >= top && *i <= bottom)
            .flat_map(|(_, row)| row.player_specs())
            .collect();
        let missing: Vec<_> = on_screen
            .iter()
            .filter(|spec| !self.inline_players.active(&spec.url) && !self.inline_players.failed(&spec.url))
            .cloned()
            .collect();
        let keep: HashSet<String> = on_screen.into_iter().map(|spec| spec.url).collect();
        // Muted looping GIF videos can warm up just beyond the viewport.
        // Visible players always go first; preloads never evict a player,
        // otherwise several near-view GIFs would repeatedly replace each other.
        let mut upcoming: Vec<_> = self
            .presenter
            .rows
            .iter()
            .enumerate()
            .filter(|(i, _)| (*i < top || *i > bottom) && i.saturating_add(2) >= top && *i <= bottom.saturating_add(2))
            .flat_map(|(i, row)| {
                row.player_specs()
                    .filter(|spec| spec.kind == crate::inline_player::Kind::Gif)
                    .map(move |spec| (if i < top { top - i } else { i - bottom }, spec))
            })
            .filter(|(_, spec)| !self.inline_players.active(&spec.url) && !self.inline_players.failed(&spec.url))
            .collect();
        upcoming.sort_by_key(|(distance, _)| *distance);
        let missing = missing.into_iter().chain(upcoming.into_iter().map(|(_, spec)| spec));
        let scale = slint::ComponentHandle::window(ui).scale_factor();
        for spec in missing {
            let mut started = self.inline_players.start(&spec, scale, false, self.inline_wake.clone());
            if matches!(started, Ok(false)) && keep.contains(&spec.url) && self.inline_players.make_room(&keep) {
                started = self.inline_players.start(&spec, scale, false, self.inline_wake.clone());
            }
            match started {
                Ok(true) => {
                    self.invalidate_player_rows(&spec.url);
                    self.presenter.images_changed = true;
                }
                Ok(false) => break,
                Err(error) => godwit_core::verbose!("note: inline playback is unavailable ({error})"),
            }
        }
    }

    /// Rows showing `url`'s player get rebuilt: starting or stopping it
    /// changes them, and rows whose pictures are all loaded aren't otherwise.
    pub(super) fn invalidate_player_rows(&mut self, url: &str) {
        for row in &mut self.presenter.rows {
            if row.player_urls().any(|u| u == url) {
                row.invalidate();
            }
        }
    }

    pub fn stop_inline(&mut self, ui: &AppWindow, url: &str) {
        if self
            .fullscreen_player
            .as_ref()
            .is_some_and(|player| player.url() == url)
        {
            self.close_fullscreen_player();
        }
        self.inline_players.stop(url);
        self.invalidate_player_rows(url);
        self.presenter.images_changed = true;
        self.refresh(ui);
    }

    pub fn player_pointer(&mut self, url: &str, x: i32, y: i32, down: bool) {
        self.inline_players.pointer(url, x, y, down);
    }

    pub fn player_move(&mut self, url: &str, x: i32, y: i32) {
        self.inline_players.move_pointer(url, x, y);
    }

    /// A video fills the window (and the window the screen), or comes back.
    pub fn video_fullscreen(&mut self, ui: &AppWindow, index: i32) {
        use slint::ComponentHandle;
        ui.set_fullscreen_video(index);
        ui.window().set_fullscreen(index >= 0);
        // Our own screen or camera, focused: its preview goes full size.
        godwit_voice::video::focus_preview(self.presenter.video_sid(index));
        self.presenter.fullscreen_sid = self.presenter.video_sid(index).map(str::to_owned);
    }

    /// The image viewer, as Yapfast's: the picture shrunk to fit, its name,
    /// "2 / 3" and arrows when the message has several, a note while it loads.
    pub(super) fn show_viewer(&mut self, ui: &AppWindow) {
        let state = self.app.state();
        let shown = self.viewer.as_ref().and_then(|(id, index)| {
            let pictures = pictures(state.selected_message(id)?);
            let p = pictures.get(*index)?.clone();
            Some((p, (*index + 1, pictures.len())))
        });
        let Some(((url, name, dimensions, video), position)) = shown else {
            self.viewer = None;
            ui.set_viewer_open(false);
            if self.viewer_anim.take().is_some() {
                self.schedule_animation();
            }
            if self.viewer_shown {
                self.viewer_shown = false;
                ui.set_viewer_image(slint::Image::default());
                ui.set_viewer_anim(-1);
            }
            ui.set_viewer_player(-1);
            return;
        };
        // A GIF that is a looping video moves here as it does in the
        // timeline: the same player's frames, kept playing while shown
        // (its still thumbnail stays underneath until the player draws).
        let player = video
            .as_deref()
            .and_then(|v| {
                self.inline_players.keep_playing(v);
                self.inline_players.slot(v)
            })
            .and_then(|slot| i32::try_from(slot).ok())
            .unwrap_or(-1);
        ui.set_viewer_player(player);
        // Animated pictures (GIFs) move here too, through the shared animator.
        let picture = self.images.get_animated(&url, crate::images::VIEWER);
        // The size it opens at: the picture's own (shrunk as the viewer
        // shrinks it), else the decoded one's once loaded.
        let size = dimensions
            .map(|(w, h)| crate::images::target(crate::images::VIEWER, w, h))
            .or_else(|| picture.as_ref().map(|(p, _)| (p.size().width, p.size().height)))
            .unwrap_or((1, 1));
        ui.set_viewer_width(i32::try_from(size.0).unwrap_or(1));
        ui.set_viewer_height(i32::try_from(size.1).unwrap_or(1));
        ui.set_viewer_note(
            if picture.is_some() {
                ""
            } else {
                "Loading the full image…"
            }
            .into(),
        );
        self.viewer_shown = picture.is_some();
        let (image, anim) = picture.unwrap_or((slint::Image::default(), -1));
        ui.set_viewer_image(image);
        ui.set_viewer_anim(anim);
        let slot = usize::try_from(anim).ok();
        if slot.is_some() || slot != self.viewer_anim {
            self.viewer_anim = slot;
            self.schedule_animation();
        }
        ui.set_viewer_name(name.to_string().into());
        ui.set_viewer_position(match position {
            (at, of) if of > 1 => format!("{at} / {of}").into(),
            _ => "".into(),
        });
        ui.set_viewer_web(url.starts_with("https://") || url.starts_with("http://"));
        ui.set_viewer_open(true);
    }

    /// Opens the viewer on a message's `index`th picture (attachments, then embeds).
    pub(super) fn open_viewer(&mut self, ui: &AppWindow, message: MessageId, index: usize) {
        self.viewer = Some((message, index));
        // Each picture opens unzoomed.
        ui.set_viewer_zoom(1.0);
        ui.set_viewer_pan_x(0.0);
        ui.set_viewer_pan_y(0.0);
        self.show_viewer(ui);
    }

    pub fn close_viewer(&mut self, ui: &AppWindow) {
        self.viewer = None;
        self.show_viewer(ui);
    }

    /// The previous (-1) or next (1) picture of the message, wrapping around.
    pub fn step_image(&mut self, ui: &AppWindow, step: i32) {
        let Some((id, index)) = self.viewer.clone() else { return };
        let count = self.app.state().selected_message(&id).map_or(0, |m| pictures(m).len());
        if count == 0 {
            return;
        }
        let next = (index as i64 + i64::from(step)).rem_euclid(count as i64);
        if let Ok(next) = usize::try_from(next) {
            self.open_viewer(ui, id, next);
        }
    }

    /// Saves the viewer's picture to Downloads (web pictures; others are
    /// saved and opened the way attachments always were).
    pub fn save_viewer_image(&self, external: bool) {
        let Some((id, index)) = self.viewer.clone() else { return };
        let Some((url, name, kind)) = self.app.state().selected_message(&id).and_then(|m| {
            let (url, name, _, _) = pictures(m).get(index)?.clone();
            Some((url.to_string(), name.to_string(), AttachmentKind::Image))
        }) else {
            return;
        };
        let web = url.starts_with("https://") || url.starts_with("http://");
        if external && web {
            open_external(&url);
        } else if let Some((_, fetch)) = self.media.iter().find(|(scheme, _)| url.starts_with(scheme.as_str())) {
            let whole = url.split('#').next().unwrap_or(&url).to_owned();
            save_and_open(fetch.clone(), whole, name, kind);
        } else if web {
            save_download(url, name);
        }
    }

    /// Opens the cropper on a picked picture: a frame of the network's shape
    /// (round for avatars and icons) to pan and zoom it in.
    pub fn open_crop(&mut self, ui: &AppWindow, crop: crate::cropper::Crop) {
        let spec = crop.target.spec();
        let round = crop.target.round();
        let (w, h) = (crop.source.width(), crop.source.height());
        let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(crop.source.as_raw(), w, h);
        ui.set_crop_image(slint::Image::from_rgba8(buffer));
        ui.set_crop_image_width(i32::try_from(w).unwrap_or(1));
        ui.set_crop_image_height(i32::try_from(h).unwrap_or(1));
        // The frame on screen: 280 px for round ones, banners 440 px wide.
        let frame_w = if round { 280.0 } else { 440.0 };
        let frame_h = frame_w * spec.height as f32 / spec.width.max(1) as f32;
        ui.set_crop_frame_width(frame_w);
        ui.set_crop_frame_height(frame_h);
        ui.set_crop_round(round);
        ui.set_crop_title(
            match &crop.target {
                crate::cropper::CropFor::Profile { avatar: true, .. } => "Crop your avatar",
                crate::cropper::CropFor::Profile { avatar: false, .. } => "Crop your banner",
                crate::cropper::CropFor::Server { icon: true, .. } => "Crop the server icon",
                crate::cropper::CropFor::Server { icon: false, .. } => "Crop the server banner",
                crate::cropper::CropFor::Channel { .. } => "Crop the channel icon",
            }
            .into(),
        );
        ui.set_crop_note(
            format!(
                "Saved as {}×{}, at most {:.1} MB. Drag to move, scroll or slide to zoom.",
                spec.width,
                spec.height,
                spec.max_bytes as f64 / (1024.0 * 1024.0)
            )
            .into(),
        );
        ui.set_crop_error("".into());
        ui.set_crop_zoom(1.0);
        ui.set_crop_pan_x(0.0);
        ui.set_crop_pan_y(0.0);
        ui.set_crop_open(true);
        self.crop = Some(crop);
    }

    /// The crop confirmed (or the original chosen): handed to the editor
    /// that asked, as if that file had been picked.
    pub fn crop_done(&mut self, ui: &AppWindow, use_original: bool) {
        let Some(crop) = self.crop.as_ref() else { return };
        let result = if use_original {
            crate::cropper::original(crop)
        } else {
            let rect = crate::cropper::source_rect(
                (crop.source.width(), crop.source.height()),
                (ui.get_crop_frame_width(), ui.get_crop_frame_height()),
                ui.get_crop_zoom(),
                (ui.get_crop_pan_x(), ui.get_crop_pan_y()),
            );
            crate::cropper::finish(crop, rect)
        };
        let image = match result {
            Ok(image) => image,
            // Stays open, saying why (too large as it is, say).
            Err(e) => return ui.set_crop_error(e.into()),
        };
        let Some(crop) = self.crop.take() else { return };
        self.close_crop(ui);
        match crop.target {
            crate::cropper::CropFor::Profile { account, avatar } => {
                self.profile_image_picked(ui, &account, avatar, Ok(image))
            }
            crate::cropper::CropFor::Server { token, icon } => self.server_image_picked(ui, &token, icon, Ok(image)),
            crate::cropper::CropFor::Channel { token } => self.channel_image_picked(ui, &token, Ok(image)),
        }
    }

    pub fn crop_cancel(&mut self, ui: &AppWindow) {
        self.crop = None;
        self.close_crop(ui);
    }

    pub(super) fn close_crop(&self, ui: &AppWindow) {
        ui.set_crop_open(false);
        // The decoded picture can be large: don't keep it.
        ui.set_crop_image(slint::Image::default());
    }

    /// The GIF picker opened: categories load once, the view starts on them.
    pub fn open_gifs(&mut self, ui: &AppWindow) {
        self.gif_tiles.first_row = 0;
        self.act(ui, Action::OpenGifs);
        self.gif_search(ui, "");
    }

    /// Shows GIFs for `query` (typing has paused; "" shows the categories).
    pub fn gif_search(&mut self, ui: &AppWindow, query: &str) {
        use slint::ComponentHandle;
        self.gif_tiles.first_row = 0;
        self.gif_tiles.favourites = false;
        ui.global::<crate::ui::Gifs>()
            .set_query(query.trim().to_lowercase().into());
        self.act(
            ui,
            Action::SearchGifs {
                query: query.to_owned(),
                more: false,
            },
        );
    }

    /// A tile was picked: a category opens its search, a GIF is sent.
    pub fn gif_picked(&mut self, ui: &AppWindow, index: i32, now_ms: i64) {
        use slint::ComponentHandle;
        let Some(pick) = usize::try_from(index)
            .ok()
            .and_then(|i| self.gif_tiles.pick(i))
            .cloned()
        else {
            return;
        };
        match pick {
            crate::gifs::Pick::Search(query) => self.gif_search(ui, &query),
            crate::gifs::Pick::Favourites => {
                self.gif_tiles.first_row = 0;
                self.gif_tiles.favourites = true;
                self.show_gifs(ui);
                self.schedule_animation();
            }
            crate::gifs::Pick::Send(url) => {
                ui.global::<crate::ui::Gifs>().set_open(false);
                self.act(ui, Action::SendGif { url, now_ms });
            }
        }
    }

    /// A GIF's star clicked (picker or chat): into Favourites, or out.
    pub fn toggle_gif_favourite(&mut self, ui: &AppWindow, url: &str, picture: &str) {
        use slint::ComponentHandle;
        if url.is_empty() || picture.is_empty() {
            return;
        }
        crate::gifs::toggle_favourite(&mut self.settings.gif_favourites, url, picture);
        crate::gifs::set_starred(&self.settings.gif_favourites);
        self.save_settings();
        let gifs = ui.global::<crate::ui::Gifs>();
        gifs.set_favourites_version(gifs.get_favourites_version().wrapping_add(1));
        self.show_gifs(ui);
        self.schedule_animation();
    }

    /// The picker scrolled: previews follow the view, and near the end the
    /// next page loads.
    pub fn gif_scrolled(&mut self, ui: &AppWindow, first: i32, near_end: bool) {
        use slint::ComponentHandle;
        self.gif_tiles.first_row = usize::try_from(first).unwrap_or(0);
        let query = ui.global::<crate::ui::Gifs>().get_query().to_string();
        if near_end && !query.is_empty() {
            self.act(ui, Action::SearchGifs { query, more: true });
        } else {
            self.show_gifs(ui);
            self.schedule_animation();
        }
    }

    pub(super) fn show_gifs(&mut self, ui: &AppWindow) {
        use slint::ComponentHandle;
        let gifs = ui.global::<crate::ui::Gifs>();
        let state = self.app.state();
        let available = crate::gifs::available(state);
        gifs.set_available(available);
        if !available {
            gifs.set_open(false);
        }
        if !gifs.get_open() {
            self.gif_tiles.favourites = false;
            if slint::Model::row_count(&*self.gif_tiles.model) > 0 {
                self.gif_tiles.show(None, &[], &mut self.images);
                self.gif_tiles.live.clear();
            }
            return;
        }
        let status = self.gif_tiles.show(
            crate::gifs::picker_for(state),
            &self.settings.gif_favourites,
            &mut self.images,
        );
        gifs.set_status(status.into());
    }

    /// Plays the animations in rows near the view, and only those.
    pub(super) fn schedule_animation(&mut self) {
        let mut live = view::live_slots(&self.presenter.rows, self.presenter.window);
        live.extend(self.gif_tiles.live.iter().copied());
        live.extend(self.viewer_anim);
        if let Ok(mut a) = self.animator.try_borrow_mut() {
            a.set_live(live);
        }
    }

    /// The timeline's visible rows changed: pictures follow the view.
    pub fn visible_rows(&mut self, ui: &AppWindow, top: i32, bottom: i32) {
        let (Ok(top), Ok(bottom)) = (usize::try_from(top), usize::try_from(bottom)) else {
            return;
        };
        let window = Some((top.min(bottom), top.max(bottom)));
        if self.presenter.window != window {
            self.presenter.window = window;
            let near: HashSet<String> = window.map_or_else(HashSet::new, |(top, bottom)| {
                self.presenter
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i + PLAYER_KEEP_ROWS >= top && *i <= bottom.saturating_add(PLAYER_KEEP_ROWS))
                    .flat_map(|(_, row)| row.player_urls().map(str::to_owned))
                    .collect()
            });
            self.inline_players.stop_unless_near(&near);
            self.refresh(ui);
            self.autoload_players(ui);
            self.load_invites(ui);
            self.refresh(ui);
        }
    }
}
