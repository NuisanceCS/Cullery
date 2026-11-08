//! Colors and fonts. Everything visual lives here so more themes can be added later.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
};

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub viewer_bg: Color32,
    pub bar: Color32,
    pub separator: Color32,
    pub control: Color32,
    pub control_border: Color32,
    pub card: Color32,
    pub tile: Color32,
    pub label: Color32,
    pub label2: Color32,
    /// Blue for text and icons on the background.
    pub tint: Color32,
    pub tint_bg: Color32,
    /// Blue fill behind white text (buttons, pick marks).
    pub accent: Color32,
    pub segment_bg: Color32,
    pub segment_on: Color32,
    pub danger: Color32,
}

/// Apple-style dark appearance.
pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x1E, 0x1E, 0x1E),
    viewer_bg: Color32::from_rgb(0x14, 0x14, 0x14),
    bar: Color32::from_rgb(0x2A, 0x2A, 0x2A),
    separator: Color32::from_rgb(0x3D, 0x3D, 0x3F),
    control: Color32::from_rgb(0x3A, 0x3A, 0x3C),
    control_border: Color32::from_rgb(0x4A, 0x4A, 0x4D),
    card: Color32::from_rgb(0x2C, 0x2C, 0x2E),
    tile: Color32::from_rgb(0x2C, 0x2C, 0x2E),
    label: Color32::from_rgb(0xF5, 0xF5, 0xF7),
    label2: Color32::from_rgb(0xA1, 0xA1, 0xA6),
    tint: Color32::from_rgb(0x29, 0x97, 0xFF),
    tint_bg: Color32::from_rgba_premultiplied(0x06, 0x15, 0x24, 0x24),
    accent: Color32::from_rgb(0x00, 0x71, 0xE3),
    segment_bg: Color32::from_rgb(0x3A, 0x3A, 0x3C),
    segment_on: Color32::from_rgb(0x63, 0x63, 0x66),
    danger: Color32::from_rgb(0xFF, 0x6B, 0x60),
};

pub const RADIUS: u8 = 7;

/// Family used for semibold text (falls back to the regular font).
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn font_semibold(size: f32) -> FontId {
    FontId::new(size, semibold())
}

pub fn apply(ctx: &egui::Context, p: &Palette) {
    install_fonts(ctx);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = p.bg;
    visuals.window_fill = p.card;
    visuals.window_stroke = Stroke::new(1.0_f32, p.separator);
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.extreme_bg_color = p.control;
    visuals.faint_bg_color = p.card;
    visuals.override_text_color = Some(p.label);
    visuals.hyperlink_color = p.tint;
    visuals.selection.bg_fill = p.accent;
    visuals.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
    visuals.slider_trailing_fill = true;

    let radius = CornerRadius::same(RADIUS);
    let w = &mut visuals.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.separator);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.label);
    w.noninteractive.corner_radius = radius;
    for state in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        state.corner_radius = radius;
        state.bg_stroke = Stroke::new(1.0_f32, p.control_border);
        state.fg_stroke = Stroke::new(1.0_f32, p.label);
        state.expansion = 0.0;
    }
    w.inactive.bg_fill = p.control;
    w.inactive.weak_bg_fill = p.control;
    w.hovered.bg_fill = lighten(p.control, 12);
    w.hovered.weak_bg_fill = lighten(p.control, 12);
    w.active.bg_fill = lighten(p.control, 24);
    w.active.weak_bg_fill = lighten(p.control, 24);
    w.open.bg_fill = lighten(p.control, 12);
    w.open.weak_bg_fill = lighten(p.control, 12);
    ctx.set_visuals(visuals);

    ctx.style_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, font(11.0)),
            (TextStyle::Body, font(13.5)),
            (TextStyle::Button, font(13.5)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
            (TextStyle::Heading, font_semibold(22.0)),
        ]
        .into();
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.slider_rail_height = 4.0;
    });
}

pub fn lighten(c: Color32, amount: u8) -> Color32 {
    Color32::from_rgb(
        c.r().saturating_add(amount),
        c.g().saturating_add(amount),
        c.b().saturating_add(amount),
    )
}

/// Uses Windows' Segoe UI (closest built-in match to Apple's system font) when present,
/// keeping egui's bundled fonts as fallbacks for symbols.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let fallbacks = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let fonts_dir = std::path::Path::new(r"C:\Windows\Fonts");

    let mut regular = fallbacks.clone();
    if let Ok(bytes) = std::fs::read(fonts_dir.join("segoeui.ttf")) {
        fonts.font_data.insert("segoe".into(), Arc::new(FontData::from_owned(bytes)));
        regular.insert(0, "segoe".into());
    }
    let mut semibold_list = regular.clone();
    if let Ok(bytes) = std::fs::read(fonts_dir.join("seguisb.ttf")) {
        fonts.font_data.insert("segoe-semibold".into(), Arc::new(FontData::from_owned(bytes)));
        semibold_list.insert(0, "segoe-semibold".into());
    }
    fonts.families.insert(FontFamily::Proportional, regular);
    fonts.families.insert(semibold(), semibold_list);
    ctx.set_fonts(fonts);
}
