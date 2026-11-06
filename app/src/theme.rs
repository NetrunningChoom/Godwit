// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! The colour scheme: Material You "tonal spot" from the official client's
//! accent, as for-web builds it.

use material_colors::color::Rgb;
use material_colors::dynamic_color::{DynamicScheme, Variant};
use material_colors::scheme::Scheme;
use slint::{Color, ComponentHandle};

use crate::ui::{AppWindow, Palette, Theme};

/// The official default accent.
pub const DEFAULT_ACCENT: &str = "#5470ec";

/// The accents Appearance offers, in its order (the first is the default);
/// also supplied to Theme so the settings swatches use this same list.
pub const ACCENTS: [&str; 8] = [
    DEFAULT_ACCENT,
    "#2f9e8f",
    "#3f9d4a",
    "#d0a02a",
    "#e0702e",
    "#d64550",
    "#c2448f",
    "#8a55d6",
];

/// Colour styles, in Appearance's order: Calm (Material You's tonal spot,
/// as for-web), Vivid and Muted.
pub const STYLES: [&str; 3] = ["Calm", "Vivid", "Muted"];

pub fn apply(ui: &AppWindow, accent: &str, style: usize, dark: bool) {
    let source: Rgb = accent.parse().unwrap_or(Rgb::new(0x54, 0x70, 0xec));
    let variant = match style {
        1 => Variant::Vibrant,
        2 => Variant::Neutral,
        _ => Variant::TonalSpot,
    };
    let s: Scheme = DynamicScheme::by_variant(source, &variant, dark, Some(0.0)).into();
    let c = |rgb: Rgb| Color::from_rgb_u8(rgb.red, rgb.green, rgb.blue);
    // Slint's own widgets and default text colour follow the scheme too.
    let scheme = if dark {
        slint::language::ColorScheme::Dark
    } else {
        slint::language::ColorScheme::Light
    };
    ui.global::<Palette>().set_color_scheme(scheme);
    let t = ui.global::<Theme>();
    t.set_accent_swatches(slint::ModelRc::new(slint::VecModel::from(
        ACCENTS
            .iter()
            .filter_map(|value| value.parse::<Rgb>().ok())
            .map(c)
            .collect::<Vec<_>>(),
    )));
    // Fixed overlays sit over pictures, video or the brand panel in either
    // theme. Preserve the existing alpha values while centralizing them.
    let black = |alpha| Color::from_argb_u8(alpha, 0, 0, 0);
    let white = |alpha| Color::from_argb_u8(alpha, 255, 255, 255);
    t.set_scrim_light(black(0x40));
    t.set_scrim_half(black(0x80));
    t.set_scrim_play(black(0xa0));
    t.set_scrim_control(black(0xc8));
    t.set_scrim_control_idle(black(0x8c));
    t.set_scrim_viewer(black(0xe6));
    t.set_media_stage(Color::from_rgb_u8(11, 11, 11));
    t.set_on_media_soft(white(0x99));
    t.set_on_media_strong(white(0xcc));
    t.set_on_media_note(white(0xd9));
    t.set_on_media_hover_soft(white(0x33));
    t.set_on_media_idle(white(0x1a));
    t.set_on_media_faint(white(0x14));
    t.set_on_qr(Color::from_rgb_u8(0x55, 0x55, 0x55));
    // Code blocks: GitHub's dark (the official client's, in both of its
    // themes), and GitHub's light in ours, where black blocks glared.
    let (code_bg, code_fg) = if dark {
        (
            Color::from_rgb_u8(0x0d, 0x11, 0x17),
            Color::from_rgb_u8(0xc9, 0xd1, 0xd9),
        )
    } else {
        (
            Color::from_rgb_u8(0xf6, 0xf8, 0xfa),
            Color::from_rgb_u8(0x1f, 0x23, 0x28),
        )
    };
    t.set_code_background(code_bg);
    t.set_on_code(code_fg);
    t.set_primary(c(s.primary));
    t.set_on_primary(c(s.on_primary));
    t.set_primary_container(c(s.primary_container));
    t.set_on_primary_container(c(s.on_primary_container));
    t.set_secondary_container(c(s.secondary_container));
    t.set_on_secondary_container(c(s.on_secondary_container));
    t.set_error(c(s.error));
    t.set_on_error(c(s.on_error));
    t.set_surface_lowest(c(s.surface_container_lowest));
    t.set_surface_low(c(s.surface_container_low));
    t.set_surface(c(s.surface_container));
    t.set_surface_high(c(s.surface_container_high));
    t.set_surface_highest(c(s.surface_container_highest));
    t.set_on_surface(c(s.on_surface));
    t.set_on_surface_variant(c(s.on_surface_variant));
    t.set_outline(c(s.outline));
    t.set_outline_variant(c(s.outline_variant));
    // Amber for warnings: bright on dark, deep enough to read on light.
    let warning = if dark {
        Color::from_rgb_u8(0xf5, 0xa6, 0x23)
    } else {
        Color::from_rgb_u8(0x9a, 0x5b, 0x00)
    };
    t.set_warning(warning);
    t.set_warning_container(Color::from_argb_u8(0x24, 0xf5, 0xa6, 0x23));
}
