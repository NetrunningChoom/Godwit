// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Voice calls over LiveKit, with the official client's microphone processing;
//! the same audio chain for plain WebRTC peers (`peer`) and for transports
//! that carry Opus themselves (`raw`, `opus`).

pub mod call;
pub mod camera;
pub mod dsp;
pub mod mictest;
pub mod opus;
pub mod peer;
pub mod raw;
pub mod screen;
pub mod sounds;
pub mod stream_audio;
mod sync;
mod transport;
pub mod video;
pub mod voice_message;
pub mod worker;
