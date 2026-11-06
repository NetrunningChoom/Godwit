// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Fullscreen is another view of the same player, preserving its page and
//! playback state. Input comes back on the normal UI wake path, without a
//! second WebKit process or provider-specific seeking APIs.

use std::sync::mpsc::{self, Receiver};

use godwit_core::backend::Waker;
use slint::ComponentHandle;

use crate::inline_player::{InlinePlayers, Spec};
use crate::ui::PlayerWindow;

#[derive(Debug)]
enum Input {
    Pointer(i32, i32, bool),
    Move(i32, i32),
    Close,
}

pub struct FullscreenPlayer {
    url: String,
    ui: PlayerWindow,
    input: Receiver<Input>,
    pressed: Option<(i32, i32)>,
}

impl FullscreenPlayer {
    pub fn new(spec: &Spec, players: &InlinePlayers, wake: Waker) -> Result<Self, slint::PlatformError> {
        let ui = PlayerWindow::new()?;
        ui.set_media_width(spec.width as i32);
        ui.set_media_height(spec.height as i32);
        if let Some(image) = players.image(&spec.url) {
            ui.set_frame(image);
        }
        let (send, input) = mpsc::channel();
        {
            let (send, wake) = (send.clone(), wake.clone());
            ui.on_pointer(move |x, y, down| {
                let _ = send.send(Input::Pointer(x, y, down));
                wake();
            });
        }
        {
            let (send, wake) = (send.clone(), wake.clone());
            ui.on_moved(move |x, y| {
                let _ = send.send(Input::Move(x, y));
                wake();
            });
        }
        {
            let (send, wake) = (send.clone(), wake.clone());
            ui.on_return_inline(move || {
                let _ = send.send(Input::Close);
                wake();
            });
        }
        ui.window().on_close_requested(move || {
            let _ = send.send(Input::Close);
            wake();
            slint::CloseRequestResponse::HideWindow
        });
        ui.window().set_fullscreen(true);
        ui.show()?;
        Ok(Self {
            url: spec.url.clone(),
            ui,
            input,
            pressed: None,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// False means Escape, Close, or the helper ended. The caller then drops
    /// this view; it leaves the helper playing in the message.
    pub fn poll(&mut self, players: &mut InlinePlayers) -> bool {
        for input in self.input.try_iter() {
            match input {
                Input::Pointer(x, y, down) => {
                    self.pressed = down.then_some((x, y));
                    players.pointer(&self.url, x, y, down);
                }
                Input::Move(x, y) => {
                    if self.pressed.is_some() {
                        self.pressed = Some((x, y));
                    }
                    players.move_pointer(&self.url, x, y);
                }
                Input::Close => {
                    if let Some((x, y)) = self.pressed.take() {
                        players.pointer(&self.url, x, y, false);
                    }
                    return false;
                }
            }
        }
        if let Some(image) = players.image(&self.url) {
            self.ui.set_frame(image);
        }
        players.active(&self.url)
    }
}

impl Drop for FullscreenPlayer {
    fn drop(&mut self) {
        let _ = self.ui.hide();
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::Arc;

    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::platform::{Platform, WindowAdapter, WindowEvent};

    use super::*;
    use crate::inline_player::Kind;

    struct Offscreen(Rc<MinimalSoftwareWindow>);

    impl Platform for Offscreen {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn fullscreen_maps_letterboxed_input_and_escape_returns_inline() -> Result<(), Box<dyn std::error::Error>> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Offscreen(window.clone())))?;
        let players = InlinePlayers::default();
        let spec = Spec {
            url: "https://example.com/video.mp4".into(),
            kind: Kind::Video,
            width: 400,
            height: 225,
        };
        let player = FullscreenPlayer::new(&spec, &players, Arc::new(|| {}))?;
        window.set_size(slint::PhysicalSize::new(800, 600));
        let mut pixels = vec![slint::Rgb8Pixel::default(); 800 * 600];
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, 800);
        });
        let position = slint::LogicalPosition::new(400.0, 300.0);
        window.dispatch_event(WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
        let input = player.input.try_recv();
        assert!(matches!(input, Ok(Input::Pointer(200, 112..=113, true))), "{input:?}");
        window.dispatch_event(WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
        assert!(matches!(
            player.input.try_recv(),
            Ok(Input::Pointer(200, 112..=113, false))
        ));
        // Slint may synthesize a hover move after releasing the pointer.
        for input in player.input.try_iter() {
            assert!(matches!(input, Input::Move(200, 112..=113)), "{input:?}");
        }
        let escape: slint::SharedString = slint::platform::Key::Escape.into();
        window.dispatch_event(WindowEvent::KeyPressed { text: escape.clone() });
        window.dispatch_event(WindowEvent::KeyReleased { text: escape });
        let input = player.input.try_recv();
        assert!(matches!(input, Ok(Input::Close)), "{input:?}");
        window.dispatch_event(WindowEvent::CloseRequested);
        assert!(matches!(player.input.try_recv(), Ok(Input::Close)));
        assert!(players.is_empty(), "a fullscreen view must not create a helper");
        Ok(())
    }
}
