// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Cropping a picked picture before it's uploaded as an avatar, a server
//! icon or a banner: the user pans and zooms it inside a frame of the
//! network's shape, and the crop is scaled to the network's size and kept
//! under its upload limit.

use std::path::{Path, PathBuf};

use godwit_core::ids::{AccountId, RoomId, ServerId};
use godwit_core::network::{ImageKind, Network};

const MB: u64 = 1024 * 1024;
use godwit_core::own_profile::ProfileImage;
use image::{DynamicImage, GenericImageView, ImageFormat, RgbaImage, imageops::FilterType};

/// Pictures are decoded at most this large for cropping (a 2048² picture is
/// 16 MiB as RGBA; nothing is uploaded larger anyway).
const MAX_SOURCE: u32 = 2048;

/// What a crop is for, and so who gets the result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CropFor {
    Profile { account: AccountId, avatar: bool },
    Server { token: (ServerId, u64), icon: bool },
    Channel { token: (RoomId, u64) },
}

impl CropFor {
    fn network(&self) -> Network {
        match self {
            Self::Profile { account, .. } => account.network,
            Self::Server { token, .. } => token.0.account.network,
            Self::Channel { token } => token.0.account.network,
        }
    }

    /// An avatar or icon (square, shown round), not a banner.
    pub fn round(&self) -> bool {
        match self {
            Self::Profile { avatar, .. } => *avatar,
            Self::Server { icon, .. } => *icon,
            Self::Channel { .. } => true,
        }
    }

    /// The network's shape and size for it, and its upload limit.
    pub fn spec(&self) -> Spec {
        let kind = match self {
            _ if self.round() => ImageKind::Avatar,
            Self::Profile { .. } => ImageKind::ProfileBanner,
            Self::Server { .. } => ImageKind::ServerBanner,
            Self::Channel { .. } => ImageKind::Avatar,
        };
        self.network().image_spec(kind)
    }
}

/// The size a crop is scaled to and the most it may weigh.
pub type Spec = godwit_core::network::ImageSpec;

/// A picture being cropped.
pub struct Crop {
    pub target: CropFor,
    pub source: RgbaImage,
    /// The file as picked, for "use it as it is" (animated GIFs keep moving).
    pub original: PathBuf,
    pub original_size: u64,
}

/// Decodes a picked file for cropping (off the UI thread: big files take a while).
pub fn load(target: CropFor, path: &Path) -> Result<Crop, String> {
    let original_size = std::fs::metadata(path)
        .map_err(|_| "The selected image could not be read.".to_owned())?
        .len();
    let picture = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|_| "The selected image could not be read.".to_owned())?
        .decode()
        .map_err(|_| "That file isn't a picture Godwit can read (PNG, JPEG, GIF or WebP).".to_owned())?;
    let picture = if picture.width() > MAX_SOURCE || picture.height() > MAX_SOURCE {
        picture.resize(MAX_SOURCE, MAX_SOURCE, FilterType::Triangle)
    } else {
        picture
    };
    Ok(Crop {
        target,
        source: picture.to_rgba8(),
        original: path.to_owned(),
        original_size,
    })
}

/// Where the crop frame sits on the picture, from the view's pan and zoom.
/// The view shows the picture covering a `frame_w`×`frame_h` frame at zoom
/// 1, `zoom` times larger, moved by (`pan_x`, `pan_y`) logical pixels.
/// Returns the source rectangle (x, y, width, height), kept inside the picture.
pub fn source_rect(
    (image_w, image_h): (u32, u32),
    (frame_w, frame_h): (f32, f32),
    zoom: f32,
    (pan_x, pan_y): (f32, f32),
) -> (u32, u32, u32, u32) {
    let (iw, ih) = (image_w.max(1) as f32, image_h.max(1) as f32);
    let scale = (frame_w / iw).max(frame_h / ih) * zoom.max(1.0);
    let (shown_w, shown_h) = (iw * scale, ih * scale);
    // The pan can't uncover the frame: the picture always fills it.
    let pan_x = pan_x.clamp(-(shown_w - frame_w) / 2.0, (shown_w - frame_w) / 2.0);
    let pan_y = pan_y.clamp(-(shown_h - frame_h) / 2.0, (shown_h - frame_h) / 2.0);
    let left = ((shown_w - frame_w) / 2.0 - pan_x) / scale;
    let top = ((shown_h - frame_h) / 2.0 - pan_y) / scale;
    let w = (frame_w / scale).min(iw);
    let h = (frame_h / scale).min(ih);
    let x = left.clamp(0.0, iw - w);
    let y = top.clamp(0.0, ih - h);
    (
        x.round() as u32,
        y.round() as u32,
        (w.round() as u32).clamp(1, image_w.max(1)),
        (h.round() as u32).clamp(1, image_h.max(1)),
    )
}

/// The crop, scaled to the network's size, saved as a PNG (or JPEG when a
/// PNG would be over the limit), ready to upload.
pub fn finish(crop: &Crop, rect: (u32, u32, u32, u32)) -> Result<ProfileImage, String> {
    let spec = crop.target.spec();
    let (x, y, w, h) = rect;
    let cut = DynamicImage::ImageRgba8(crop.source.view(x, y, w, h).to_image());
    let out = cut.resize_exact(spec.width, spec.height, FilterType::Lanczos3);
    let bytes = encode(&out, spec.max_bytes)?;
    save(&bytes)
}

/// The picked file itself, if the network takes it as it is.
pub fn original(crop: &Crop) -> Result<ProfileImage, String> {
    let spec = crop.target.spec();
    if crop.original_size > spec.max_bytes {
        return Err(format!(
            "That file is {}; {} takes at most {} here. Crop it to make it smaller.",
            human(crop.original_size),
            crop.target.network().name(),
            human(spec.max_bytes)
        ));
    }
    Ok(ProfileImage::File {
        path: crop.original.clone(),
        size: crop.original_size,
    })
}

fn encode(picture: &DynamicImage, max_bytes: u64) -> Result<Vec<u8>, String> {
    let mut png = Vec::new();
    picture
        .write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|e| format!("Couldn't prepare the picture: {e}"))?;
    if png.len() as u64 <= max_bytes {
        return Ok(png);
    }
    // Photos compress far better as JPEG; lower the quality until it fits.
    let rgb = DynamicImage::ImageRgb8(picture.to_rgb8());
    for quality in [92u8, 85, 75, 60] {
        let mut jpeg = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, quality);
        rgb.write_with_encoder(encoder)
            .map_err(|e| format!("Couldn't prepare the picture: {e}"))?;
        if jpeg.len() as u64 <= max_bytes {
            return Ok(jpeg);
        }
    }
    Err("The picture is too large even after compressing it.".into())
}

/// Saves the prepared picture where the upload reads it from (the system's
/// temporary folder; each crop gets its own name).
fn save(bytes: &[u8]) -> Result<ProfileImage, String> {
    let dir = std::env::temp_dir().join("godwit-crops");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't save the picture: {e}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let jpeg = bytes.starts_with(&[0xff, 0xd8]);
    let path = dir.join(format!("crop-{stamp}.{}", if jpeg { "jpg" } else { "png" }));
    std::fs::write(&path, bytes).map_err(|e| format!("Couldn't save the picture: {e}"))?;
    Ok(ProfileImage::File {
        path,
        size: bytes.len() as u64,
    })
}

fn human(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / MB as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_covers_the_middle_at_first_and_stays_inside() {
        // A 400×200 picture in a 100×100 frame: covering it shows 200×200 of it.
        assert_eq!(
            source_rect((400, 200), (100.0, 100.0), 1.0, (0.0, 0.0)),
            (100, 0, 200, 200)
        );
        // Zoomed 2×: half of that, still centred.
        assert_eq!(
            source_rect((400, 200), (100.0, 100.0), 2.0, (0.0, 0.0)),
            (150, 50, 100, 100)
        );
        // Panned far right: the frame stops at the picture's left edge.
        assert_eq!(
            source_rect((400, 200), (100.0, 100.0), 1.0, (10_000.0, 0.0)),
            (0, 0, 200, 200)
        );
    }

    #[test]
    fn crops_are_scaled_to_the_networks_size_and_limit() {
        let account = AccountId::new(Network::Stoat, "me");
        let crop = Crop {
            target: CropFor::Profile {
                account: account.clone(),
                avatar: true,
            },
            source: RgbaImage::from_pixel(300, 200, image::Rgba([200, 30, 30, 255])),
            original: PathBuf::from("/nonexistent"),
            original_size: 10 * MB,
        };
        let ProfileImage::File { path, size } = finish(&crop, (50, 0, 200, 200)).expect("cropped") else {
            panic!("expected a file")
        };
        assert!(size <= crop.target.spec().max_bytes);
        let saved = image::open(&path).expect("readable");
        assert_eq!((saved.width(), saved.height()), (512, 512));
        let _ = std::fs::remove_file(path);
        assert!(original(&crop).is_err(), "10 MB is over Stoat's avatar limit");
    }

    #[test]
    fn banners_take_each_networks_shape() {
        let stoat = AccountId::new(Network::Stoat, "me");
        let discord = AccountId::new(Network::Discord, "me");
        let spec = |account: &AccountId, avatar| {
            CropFor::Profile {
                account: account.clone(),
                avatar,
            }
            .spec()
        };
        assert_eq!((spec(&stoat, false).width, spec(&stoat, false).height), (1500, 500));
        assert_eq!((spec(&discord, false).width, spec(&discord, false).height), (1360, 480));
        assert_eq!(spec(&discord, true).width, 512);
    }
}
