// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Real VP8/VP9 clips made with ffmpeg 8 (libvpx), checked in so tests
//! don't need it: `rgb-*.webm` are 32x24, three frames red, lime and blue
//! shown at 0, 100 and 300 ms (100 ms frame duration), from
//!
//! ```sh
//! F="color=c=red:s=32x24:r=10:d=0.1[a];color=c=lime:s=32x24:r=10:d=0.1[b];\
//! color=c=blue:s=32x24:r=10:d=0.1[c];[a][b][c]concat=n=3,\
//! setpts='if(eq(N\,0)\,0\,if(eq(N\,1)\,0.1/TB\,0.3/TB))',format=yuv420p"
//! ffmpeg -f lavfi -i "$F" -fps_mode passthrough -c:v libvpx -b:v 200k -auto-alt-ref 0 rgb-vp8.webm
//! ffmpeg -f lavfi -i "$F" -fps_mode passthrough -c:v libvpx-vp9 -b:v 200k rgb-vp9.webm
//! ffmpeg -f lavfi -i "color=c=red@0.5:s=32x24:r=10:d=0.2,format=yuva420p" \
//!   -c:v libvpx-vp9 -pix_fmt yuva420p -b:v 200k alpha-vp9.webm
//! ```

const VP8: &[u8] = include_bytes!("fixtures/rgb-vp8.webm");
const VP9: &[u8] = include_bytes!("fixtures/rgb-vp9.webm");
const ALPHA: &[u8] = include_bytes!("fixtures/alpha-vp9.webm");

const BIG: usize = 64 << 20;

fn close(px: &[u8], want: [u8; 4]) -> bool {
    px.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 12)
}

fn centre(clip: &godwit_media::Clip, frame: usize) -> &[u8] {
    let (w, h) = (clip.width as usize, clip.height as usize);
    let i = ((h / 2) * w + w / 2) * 4;
    &clip.frames[frame].0[i..i + 4]
}

fn check_rgb(bytes: &[u8]) {
    assert!(godwit_media::is_webm(bytes));
    let Some(clip) = godwit_media::decode(bytes, 640, 640, BIG, 1000) else {
        panic!("fixture did not decode");
    };
    assert_eq!((clip.width, clip.height), (32, 24));
    let delays: Vec<u32> = clip.frames.iter().map(|f| f.1).collect();
    assert_eq!(delays, [100, 200, 100]);
    for (pixels, _) in &clip.frames {
        assert_eq!(pixels.len(), 32 * 24 * 4);
    }
    // Bytes in memory are R, G, B, A, as Slint's Rgba8Pixel expects.
    assert!(close(centre(&clip, 0), [255, 0, 0, 255]), "{:?}", centre(&clip, 0));
    assert!(close(centre(&clip, 1), [0, 255, 0, 255]), "{:?}", centre(&clip, 1));
    assert!(close(centre(&clip, 2), [0, 0, 255, 255]), "{:?}", centre(&clip, 2));
}

#[test]
fn vp8_decodes_frames_delays_and_colours() {
    check_rgb(VP8);
}

#[test]
fn vp9_decodes_frames_delays_and_colours() {
    check_rgb(VP9);
}

#[test]
fn vp9_alpha_is_kept() {
    let clip = godwit_media::decode(ALPHA, 640, 640, BIG, 1000).expect("decodes");
    assert_eq!(clip.frames.len(), 2);
    let px = centre(&clip, 0);
    assert!(px[0] > 200 && px[1] < 30 && px[2] < 30, "{px:?}");
    assert!(px[3].abs_diff(128) <= 12, "{px:?}");
}

#[test]
fn frames_shrink_to_fit_but_never_grow() {
    let clip = godwit_media::decode(VP9, 16, 16, BIG, 1000).expect("decodes");
    assert_eq!((clip.width, clip.height), (16, 12));
    assert!(clip.frames.iter().all(|f| f.0.len() == 16 * 12 * 4));
    assert!(close(centre(&clip, 1), [0, 255, 0, 255]));
    let clip = godwit_media::decode(VP8, 4000, 4000, BIG, 1000).expect("decodes");
    assert_eq!((clip.width, clip.height), (32, 24));
}

#[test]
fn byte_budget_lowers_resolution_and_keeps_every_frame() {
    let cap = 3 * 16 * 12 * 4;
    let clip = godwit_media::decode(VP8, 640, 640, cap, 1000).expect("decodes");
    assert_eq!(clip.frames.len(), 3);
    let total: usize = clip.frames.iter().map(|f| f.0.len()).sum();
    assert!(total <= cap, "{total} > {cap}");
    assert!(clip.width <= 16 && clip.height <= 12);
    assert_eq!(clip.frames[2].1, 100);
}

#[test]
fn frame_limit_is_honoured() {
    let clip = godwit_media::decode(VP9, 640, 640, BIG, 2).expect("decodes");
    assert_eq!(clip.frames.len(), 2);
    assert_eq!(clip.frames[0].1, 100);
}

#[test]
fn garbage_is_rejected() {
    assert!(godwit_media::decode(&[], 640, 640, BIG, 1000).is_none());
    assert!(godwit_media::decode(b"GIF89a not a video", 640, 640, BIG, 1000).is_none());
    assert!(!godwit_media::is_webm(b"\x89PNG\r\n\x1a\n"));
    assert!(godwit_media::decode(VP8, 0, 640, BIG, 1000).is_none());
    // Deterministic pseudo-random bytes behind a valid magic.
    let mut state = 0x2545_F491_u32;
    let mut noise = vec![0x1A, 0x45, 0xDF, 0xA3];
    for _ in 0..4096 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        noise.push(state as u8);
    }
    assert!(godwit_media::decode(&noise, 640, 640, BIG, 1000).is_none());
}

#[test]
fn truncated_and_corrupted_clips_never_panic() {
    for bytes in [VP8, VP9, ALPHA] {
        for len in 0..bytes.len() {
            if let Some(clip) = godwit_media::decode(&bytes[..len], 64, 64, BIG, 1000) {
                assert!(!clip.frames.is_empty() && clip.frames.len() <= 3);
            }
        }
        // Flip bytes throughout, including inside the compressed frames.
        for i in (0..bytes.len()).step_by(3) {
            let mut damaged = bytes.to_vec();
            damaged[i] ^= 0xA5;
            let _ = godwit_media::decode(&damaged, 64, 64, BIG, 1000);
        }
    }
}

fn stream(bytes: &[u8], max: u32) -> godwit_media::Stream {
    let Some(stream) = godwit_media::Stream::new(std::sync::Arc::from(bytes), max, max) else {
        panic!("fixture did not open");
    };
    stream
}

#[test]
fn streams_loop_with_the_same_frames_and_delays() {
    for bytes in [VP8, VP9] {
        let mut s = stream(bytes, 640);
        assert_eq!(s.size(), (32, 24));
        assert_eq!(s.frame_count(), 3);
        let frames: Vec<(Vec<u8>, u32)> = (0..7).map(|_| s.next_frame().unwrap()).collect();
        let delays: Vec<u32> = frames.iter().map(|f| f.1).collect();
        assert_eq!(delays, [100, 200, 100, 100, 200, 100, 100]);
        // A fresh decoder from the keyframe gives the same pictures again.
        assert_eq!(frames[3].0, frames[0].0);
        assert_eq!(frames[4].0, frames[1].0);
        assert_eq!(frames[6].0, frames[0].0);
        let px = |f: &[u8]| f[((12 * 32) + 16) * 4..][..4].to_vec();
        assert!(close(&px(&frames[0].0), [255, 0, 0, 255]));
        assert!(close(&px(&frames[1].0), [0, 255, 0, 255]));
        assert!(close(&px(&frames[2].0), [0, 0, 255, 255]));
    }
}

#[test]
fn streams_fit_without_a_byte_cap_and_reuse_buffers() {
    let mut s = stream(VP9, 16);
    assert_eq!(s.size(), (16, 12));
    let mut buf = Vec::new();
    for _ in 0..5 {
        assert!(s.next_frame_into(&mut buf).is_some());
        assert_eq!(buf.len(), 16 * 12 * 4);
    }
    let mut s = stream(ALPHA, 640);
    let (px, _) = s.next_frame().unwrap();
    assert!(px[((12 * 32) + 16) * 4 + 3].abs_diff(128) <= 12);
}

#[test]
fn streams_reject_garbage_and_survive_damage() {
    let open = |b: &[u8]| godwit_media::Stream::new(std::sync::Arc::from(b), 64, 64);
    assert!(open(&[]).is_none());
    assert!(open(b"GIF89a").is_none());
    assert!(godwit_media::Stream::new(std::sync::Arc::from(VP8), 0, 64).is_none());
    for bytes in [VP8, VP9, ALPHA] {
        for len in 0..bytes.len() {
            if let Some(mut s) = open(&bytes[..len]) {
                for _ in 0..8 {
                    if s.next_frame().is_none() {
                        break;
                    }
                }
            }
        }
        for i in (0..bytes.len()).step_by(3) {
            let mut damaged = bytes.to_vec();
            damaged[i] ^= 0xA5;
            if let Some(mut s) = open(&damaged) {
                for _ in 0..8 {
                    if let Some((px, _)) = s.next_frame() {
                        assert_eq!(px.len() as u32, s.size().0 * s.size().1 * 4);
                    }
                }
            }
        }
    }
}
