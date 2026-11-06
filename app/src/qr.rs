// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! QR codes for signing in (Steam's mobile app scans one), drawn one pixel
//! per module: the UI scales it up with pixelated rendering, so it stays
//! sharp at any size without a large picture in memory.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

/// Light modules around the code, so scanners find its edge.
const QUIET: usize = 2;

/// `text` as a QR code, dark on light; None if it doesn't fit in one.
pub fn image(text: &str) -> Option<Image> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let width = code.width();
    let size = width + 2 * QUIET;
    let side = u32::try_from(size).ok()?;
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(side, side);
    let pixels = buffer.make_mut_slice();
    for (i, module) in code.to_colors().iter().enumerate() {
        let (x, y) = (i % width + QUIET, i / width + QUIET);
        if *module == qrcode::Color::Dark
            && let Some(p) = pixels.get_mut(y * size + x)
        {
            *p = Rgba8Pixel::new(0, 0, 0, 255);
        }
    }
    // Everything else light (the buffer starts transparent).
    for p in pixels.iter_mut().filter(|p| p.a == 0) {
        *p = Rgba8Pixel::new(255, 255, 255, 255);
    }
    Some(Image::from_rgba8(buffer))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_sign_in_url_becomes_a_square_code() {
        let image = super::image("https://s.team/q/1/1234567890123456789").unwrap();
        let size = image.size();
        assert_eq!(size.width, size.height);
        assert!(size.width >= 21 + 4, "at least a version-1 code and its margin");
    }
}
