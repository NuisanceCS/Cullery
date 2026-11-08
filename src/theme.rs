//! Themes: colors, corner shapes and fonts. Everything visual lives here.

use std::sync::{Arc, OnceLock};

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
};

/// The themes to choose from in Settings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeId {
    AppleDark,
    AppleLight,
    Win11Dark,
    Win11Light,
    Win10Dark,
    Win10Light,
}

impl ThemeId {
    pub const ALL: [ThemeId; 6] = [
        ThemeId::AppleDark,
        ThemeId::AppleLight,
        ThemeId::Win11Dark,
        ThemeId::Win11Light,
        ThemeId::Win10Dark,
        ThemeId::Win10Light,
    ];

    /// Name stored in the settings file.
    pub fn key(self) -> &'static str {
        match self {
            ThemeId::AppleDark => "apple-dark",
            ThemeId::AppleLight => "apple-light",
            ThemeId::Win11Dark => "win11-dark",
            ThemeId::Win11Light => "win11-light",
            ThemeId::Win10Dark => "win10-dark",
            ThemeId::Win10Light => "win10-light",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.key() == key)
    }

    pub fn name(self) -> &'static str {
        match self {
            ThemeId::AppleDark => "Apple (Dark)",
            ThemeId::AppleLight => "Apple (Light)",
            ThemeId::Win11Dark => "Windows 11 (Dark)",
            ThemeId::Win11Light => "Windows 11 (Light)",
            ThemeId::Win10Dark => "Windows 10 (Dark)",
            ThemeId::Win10Light => "Windows 10 (Light)",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            ThemeId::AppleDark | ThemeId::AppleLight => "Rounded, macOS colors",
            ThemeId::Win11Dark | ThemeId::Win11Light => "Fluent, Mica top bar",
            ThemeId::Win10Dark | ThemeId::Win10Light => "Flat and square",
        }
    }

    pub fn palette(self) -> Palette {
        match self {
            ThemeId::AppleDark => APPLE_DARK,
            ThemeId::AppleLight => APPLE_LIGHT,
            ThemeId::Win11Dark => WIN11_DARK,
            ThemeId::Win11Light => WIN11_LIGHT,
            ThemeId::Win10Dark => WIN10_DARK,
            ThemeId::Win10Light => WIN10_LIGHT,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
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
    /// Accent color for text and icons on the background.
    pub tint: Color32,
    pub tint_bg: Color32,
    /// Accent fill behind text (buttons, pick marks, selection).
    pub accent: Color32,
    /// Text and icons drawn on `accent`.
    pub on_accent: Color32,
    pub danger: Color32,
    /// Selected photo tile background and outline.
    pub select_bg: Color32,
    pub select_border: Color32,
    /// Hover wash for flat (borderless) buttons and rows.
    pub hover: Color32,
    /// Outline of a hovered button (Windows 10 draws one in its accent blue).
    pub hover_border: Option<Color32>,
    pub scroll: Color32,
    pub scroll_hover: Color32,
    /// Top bar shows the Windows 11 Mica backdrop (wallpaper tint) when available.
    pub mica: bool,
    /// What Mica roughly looks like, for the theme preview card.
    pub mica_preview: Color32,
    /// Corner radii: buttons and fields, pill buttons (255 = fully round), cards and
    /// panels, photo thumbnails.
    pub r_ctl: u8,
    pub r_pill: u8,
    pub r_card: u8,
    pub r_thumb: u8,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

const fn rgba(hex: u32, alpha: u8) -> Color32 {
    Color32::from_rgba_premultiplied(
        ((hex >> 16) as u8 as u32 * alpha as u32 / 255) as u8,
        ((hex >> 8) as u8 as u32 * alpha as u32 / 255) as u8,
        (hex as u8 as u32 * alpha as u32 / 255) as u8,
        alpha,
    )
}

/// Apple-style dark appearance (macOS dark mode colors).
pub const APPLE_DARK: Palette = Palette {
    dark: true,
    bg: rgb(0x1E1E1E),
    viewer_bg: rgb(0x141414),
    bar: rgb(0x2A2A2A),
    separator: rgb(0x48484A),
    control: rgb(0x3A3A3C),
    control_border: rgb(0x4A4A4D),
    card: rgb(0x2C2C2E),
    tile: rgb(0x2C2C2E),
    label: rgb(0xF5F5F7),
    label2: rgb(0xA1A1A6),
    tint: rgb(0x2997FF),
    tint_bg: rgba(0x2997FF, 36),
    accent: rgb(0x0071E3),
    on_accent: Color32::WHITE,
    danger: rgb(0xFF6B60),
    select_bg: rgba(0xFFFFFF, 28),
    select_border: rgba(0xFFFFFF, 40),
    hover: rgba(0xFFFFFF, 14),
    hover_border: None,
    scroll: rgb(0x6A6A6A),
    scroll_hover: rgb(0x8C8C8C),
    mica: false,
    mica_preview: rgb(0x2A2A2A),
    r_ctl: 7,
    r_pill: 255,
    r_card: 10,
    r_thumb: 6,
};

/// Apple-style light appearance.
pub const APPLE_LIGHT: Palette = Palette {
    dark: false,
    bg: rgb(0xFFFFFF),
    viewer_bg: rgb(0xECECEF),
    bar: rgb(0xF5F5F7),
    separator: rgb(0xD2D2D7),
    control: rgb(0xFFFFFF),
    control_border: rgb(0xC4C4C9),
    card: rgb(0xF5F5F7),
    tile: rgb(0xE8E8ED),
    label: rgb(0x1D1D1F),
    label2: rgb(0x6E6E73),
    tint: rgb(0x0066CC),
    tint_bg: rgba(0x0071E3, 26),
    accent: rgb(0x0071E3),
    on_accent: Color32::WHITE,
    danger: rgb(0xD70015),
    select_bg: rgba(0x000000, 14),
    select_border: rgba(0x000000, 26),
    hover: rgba(0x000000, 12),
    hover_border: None,
    scroll: rgb(0xB0B0B5),
    scroll_hover: rgb(0x8E8E93),
    mica: false,
    mica_preview: rgb(0xF5F5F7),
    r_ctl: 7,
    r_pill: 255,
    r_card: 10,
    r_thumb: 6,
};

/// Windows 11 dark (Fluent), like File Explorer: Mica top bar, 4 px corners.
pub const WIN11_DARK: Palette = Palette {
    dark: true,
    bg: rgb(0x191919),
    viewer_bg: rgb(0x141414),
    bar: rgb(0x202020),
    separator: rgb(0x4A4A4A),
    control: rgb(0x2D2D2D),
    control_border: rgb(0x575757),
    card: rgb(0x2B2B2B),
    tile: rgb(0x262626),
    label: rgb(0xFFFFFF),
    label2: rgb(0xCFCFCF),
    tint: rgb(0x60CDFF),
    tint_bg: rgba(0x60CDFF, 30),
    accent: rgb(0x60CDFF),
    on_accent: rgb(0x000000),
    danger: rgb(0xFF99A4),
    select_bg: rgba(0xFFFFFF, 22),
    select_border: rgba(0xFFFFFF, 30),
    hover: rgba(0xFFFFFF, 14),
    hover_border: None,
    scroll: rgb(0x9A9A9A),
    scroll_hover: rgb(0xC5C5C5),
    mica: true,
    mica_preview: rgb(0x2A2633),
    r_ctl: 4,
    r_pill: 4,
    r_card: 8,
    r_thumb: 4,
};

/// Windows 11 light (Fluent).
pub const WIN11_LIGHT: Palette = Palette {
    dark: false,
    bg: rgb(0xFFFFFF),
    viewer_bg: rgb(0xF3F3F3),
    bar: rgb(0xF3F3F3),
    separator: rgb(0xD0D0D0),
    control: rgb(0xFBFBFB),
    control_border: rgb(0xC8C8C8),
    card: rgb(0xF9F9F9),
    tile: rgb(0xEBEBEB),
    label: rgb(0x1B1B1B),
    label2: rgb(0x5F5F5F),
    tint: rgb(0x005FB8),
    tint_bg: rgba(0x005FB8, 26),
    accent: rgb(0x005FB8),
    on_accent: Color32::WHITE,
    danger: rgb(0xC42B1C),
    select_bg: rgba(0x000000, 14),
    select_border: rgba(0x000000, 22),
    hover: rgba(0x000000, 10),
    hover_border: None,
    scroll: rgb(0x8A8A8A),
    scroll_hover: rgb(0x5F5F5F),
    mica: true,
    mica_preview: rgb(0xECE8F2),
    r_ctl: 4,
    r_pill: 4,
    r_card: 8,
    r_thumb: 4,
};

/// Windows 10 dark: flat, square corners, classic accent blue.
pub const WIN10_DARK: Palette = Palette {
    dark: true,
    bg: rgb(0x191919),
    viewer_bg: rgb(0x111111),
    bar: rgb(0x2B2B2B),
    separator: rgb(0x474747),
    control: rgb(0x333333),
    control_border: rgb(0x555555),
    card: rgb(0x2B2B2B),
    tile: rgb(0x262626),
    label: rgb(0xFFFFFF),
    label2: rgb(0xC8C8C8),
    tint: rgb(0x4AA3F0),
    tint_bg: rgba(0x0078D7, 60),
    // Windows 10's blue is #0078D7; a hair darker keeps white text on it readable.
    accent: rgb(0x0074D0),
    on_accent: Color32::WHITE,
    danger: rgb(0xFF6060),
    select_bg: rgba(0x777777, 100),
    select_border: rgb(0x777777),
    hover: rgba(0xFFFFFF, 26),
    hover_border: Some(rgb(0x0078D7)),
    scroll: rgb(0x6E6E6E),
    scroll_hover: rgb(0x9E9E9E),
    mica: false,
    mica_preview: rgb(0x2B2B2B),
    r_ctl: 0,
    r_pill: 0,
    r_card: 0,
    r_thumb: 0,
};

/// Windows 10 light: File Explorer's flat look with light-blue selection.
pub const WIN10_LIGHT: Palette = Palette {
    dark: false,
    bg: rgb(0xFFFFFF),
    viewer_bg: rgb(0xF0F0F0),
    bar: rgb(0xF0F0F0),
    separator: rgb(0xC8C9CA),
    control: rgb(0xE1E1E1),
    control_border: rgb(0xADADAD),
    card: rgb(0xF0F0F0),
    tile: rgb(0xE8E8E8),
    label: rgb(0x000000),
    label2: rgb(0x595959),
    tint: rgb(0x0066CC),
    tint_bg: rgb(0xCCE8FF),
    accent: rgb(0x0074D0),
    on_accent: Color32::WHITE,
    danger: rgb(0xE81123),
    select_bg: rgb(0xCCE8FF),
    select_border: rgb(0x99D1FF),
    hover: rgb(0xE5F3FF),
    hover_border: Some(rgb(0x0078D7)),
    scroll: rgb(0xC2C2C2),
    scroll_hover: rgb(0xA6A6A6),
    mica: false,
    mica_preview: rgb(0xF0F0F0),
    r_ctl: 0,
    r_pill: 0,
    r_card: 0,
    r_thumb: 0,
};

/// Fill of the confirm-delete button (white text on it in every theme).
pub const DELETE_RED: Color32 = rgb(0xD70015);

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

    let mut visuals = if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    visuals.panel_fill = p.bg;
    visuals.window_fill = p.card;
    visuals.window_stroke = Stroke::new(1.0_f32, p.separator);
    visuals.window_corner_radius = CornerRadius::same(p.r_card);
    visuals.menu_corner_radius = CornerRadius::same(p.r_card.min(8));
    visuals.extreme_bg_color = p.control;
    visuals.faint_bg_color = p.card;
    visuals.override_text_color = Some(p.label);
    visuals.hyperlink_color = p.tint;
    visuals.selection.bg_fill = p.accent;
    visuals.selection.stroke = Stroke::new(1.0_f32, p.on_accent);
    visuals.slider_trailing_fill = true;
    if !p.dark {
        visuals.window_shadow.color = Color32::from_black_alpha(40);
        visuals.popup_shadow.color = Color32::from_black_alpha(30);
    }

    let radius = CornerRadius::same(p.r_ctl);
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
    let hovered = shade(p, p.control, 12);
    let pressed = shade(p, p.control, 24);
    w.inactive.bg_fill = p.control;
    w.inactive.weak_bg_fill = p.control;
    w.hovered.bg_fill = hovered;
    w.hovered.weak_bg_fill = hovered;
    w.active.bg_fill = pressed;
    w.active.weak_bg_fill = pressed;
    w.open.bg_fill = hovered;
    w.open.weak_bg_fill = hovered;
    if let Some(border) = p.hover_border {
        // Windows 10: hovered buttons turn light blue with a blue outline.
        let fill = if p.dark { shade(p, p.control, 18) } else { p.hover };
        w.hovered.bg_fill = fill;
        w.hovered.weak_bg_fill = fill;
        w.hovered.bg_stroke = Stroke::new(1.0_f32, border);
        w.active.bg_stroke = Stroke::new(1.0_f32, border);
    }
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

pub fn darken(c: Color32, amount: u8) -> Color32 {
    Color32::from_rgb(
        c.r().saturating_sub(amount),
        c.g().saturating_sub(amount),
        c.b().saturating_sub(amount),
    )
}

/// A hover/pressed version of a color: lighter on dark themes, darker on light ones.
pub fn shade(p: &Palette, c: Color32, amount: u8) -> Color32 {
    if p.dark {
        lighten(c, amount)
    } else {
        darken(c, amount / 2)
    }
}

/// Radius a little larger than `r` for something drawn around it (0 stays square).
pub fn around(r: u8, extra: u8) -> u8 {
    if r == 0 {
        0
    } else {
        r.saturating_add(extra)
    }
}

/// Uses Windows' Segoe UI when present, keeping egui's bundled fonts as fallbacks for
/// symbols. The font files are read once.
fn install_fonts(ctx: &egui::Context) {
    static FONTS: OnceLock<FontDefinitions> = OnceLock::new();
    let fonts = FONTS.get_or_init(|| {
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
        fonts
    });
    ctx.set_fonts(fonts.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG contrast ratio between two opaque colors.
    fn contrast(a: Color32, b: Color32) -> f32 {
        let lum = |c: Color32| {
            let ch = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// Dividing lines and control outlines (buttons, radio buttons, fields) stand out
    /// from the surfaces they're drawn on — in Windows 11 dark they were nearly
    /// invisible in Settings.
    #[test]
    fn lines_and_outlines_are_visible() {
        let mut too_faint = Vec::new();
        for id in ThemeId::ALL {
            let p = id.palette();
            for (what, line, surface, min) in [
                ("divider on card", p.separator, p.card, 1.35),
                ("divider on bar", p.separator, p.bar, 1.25),
                ("divider on background", p.separator, p.bg, 1.25),
                ("outline on card", p.control_border, p.card, 1.5),
                ("outline on background", p.control_border, p.bg, 1.5),
            ] {
                let c = contrast(line, surface);
                println!("{:<20} {what:<24} {c:.2}", id.name());
                if c < min {
                    too_faint.push(format!("{}: {what} {c:.2} < {min}", id.name()));
                }
            }
        }
        assert!(too_faint.is_empty(), "too faint:\n{}", too_faint.join("\n"));
    }

    /// Every theme's text is readable: body and secondary text on the background and
    /// bars, and text on the accent color.
    #[test]
    fn theme_text_is_readable() {
        for id in ThemeId::ALL {
            let p = id.palette();
            for (what, fg, bg, min) in [
                ("label on bg", p.label, p.bg, 7.0),
                ("label on bar", p.label, p.bar, 7.0),
                ("secondary on bg", p.label2, p.bg, 4.5),
                ("secondary on bar", p.label2, p.bar, 4.5),
                ("secondary on card", p.label2, p.card, 4.5),
                ("on-accent text", p.on_accent, p.accent, 4.5),
            ] {
                let c = contrast(fg, bg);
                assert!(c >= min, "{}: {what} contrast {c:.2} < {min}", id.name());
            }
            assert_eq!(ThemeId::from_key(id.key()), Some(id));
        }
    }
}
