//! Color and size tokens, and font setup.
//!
//! Sizes are in design units (du); see [`crate::keypad::geometry`].

use std::sync::Arc;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, TextStyle};

use crate::keypad::KeyStyle;

/// Window background around the calculator.
pub const APP_BG: Color32 = Color32::from_rgb(0xD6, 0xE9, 0xF8);
/// Calculator body fill.
pub const BODY_FILL: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// Calculator body outline (stroke centered on the body edge).
pub const BODY_OUTLINE: Color32 = Color32::from_rgb(0x1F, 0x6F, 0xB5);
/// Width of [`BODY_OUTLINE`].
pub const BODY_OUTLINE_W: f32 = 6.0;
/// Corner radius of the body.
pub const BODY_RADIUS: f32 = 34.0;
/// LCD bezel.
pub const BEZEL: Color32 = Color32::from_rgb(0x2E, 0x36, 0x40);
/// Fill of `Function` and `Gray` keys.
pub const KEY_GRAY: Color32 = Color32::from_rgb(0xE6, 0xE9, 0xEE);
/// Outline of `Function` and `Gray` keys.
pub const KEY_GRAY_STROKE: Color32 = Color32::from_rgb(0xB8, 0xC0, 0xCC);
/// Label on light keys.
pub const LABEL_DARK: Color32 = Color32::from_rgb(0x1C, 0x24, 0x30);
/// Fill of `Number` and `Dpad` keys.
pub const KEY_NUMBER: Color32 = Color32::from_rgb(0x3C, 0x4A, 0x5C);
/// Label on dark keys.
pub const LABEL_LIGHT: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// Fill of the `2nd` key.
pub const KEY_SECOND: Color32 = Color32::from_rgb(0x1A, 0x9B, 0xA8);
/// Fill of the `alpha` key.
pub const KEY_ALPHA: Color32 = Color32::from_rgb(0x3E, 0x9B, 0x4F);
/// Second (blue) legend.
pub const LEGEND_SECOND: Color32 = Color32::from_rgb(0x1F, 0x6F, 0xB5);
/// Alpha (green) legend.
pub const LEGEND_ALPHA: Color32 = Color32::from_rgb(0x3E, 0x9B, 0x4F);
/// Outline of a hovered key.
pub const HOVER_OUTLINE: Color32 = Color32::from_rgb(0x1F, 0x6F, 0xB5);
/// Width of [`HOVER_OUTLINE`].
pub const HOVER_OUTLINE_W: f32 = 1.5;
/// Key drop shadow (`#00000026`).
pub const KEY_SHADOW: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 0x26);
/// Offset of [`KEY_SHADOW`].
pub const KEY_SHADOW_OFFSET: egui::Vec2 = egui::vec2(0.0, 2.0);
/// Header bar background.
pub const HEADER_BG: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// Toolbar background.
pub const TOOLBAR_BG: Color32 = Color32::from_rgb(0xF5, 0xF9, 0xFD);
/// Background of an info toast.
pub const TOAST_BG: Color32 = Color32::from_rgb(0x2E, 0x36, 0x40);
/// Background of an error toast.
pub const TOAST_ERROR_BG: Color32 = Color32::from_rgb(0xB3, 0x26, 0x1E);
/// LCD fill when there is no frame.
pub const NO_SIGNAL_BG: Color32 = Color32::from_rgb(0x1E, 0x23, 0x29);
/// Per-channel factor applied to a key's fill while it is pressed.
pub const PRESSED_FACTOR: f32 = 0.82;

/// Main label size (rows 1–9).
pub const LABEL_SIZE: f32 = 14.0;
/// Main label size on row 0.
pub const LABEL_SIZE_ROW0: f32 = 12.0;
/// Legend size.
pub const LEGEND_SIZE: f32 = 10.0;

/// `base` with each color channel scaled by [`PRESSED_FACTOR`]; alpha is kept.
pub fn pressed(base: Color32) -> Color32 {
    let f = |c: u8| (f32::from(c) * PRESSED_FACTOR).round() as u8;
    Color32::from_rgba_premultiplied(f(base.r()), f(base.g()), f(base.b()), base.a())
}

/// Fill and label color of a key style.
pub fn key_colors(style: KeyStyle) -> (Color32, Color32) {
    match style {
        KeyStyle::Function | KeyStyle::Gray => (KEY_GRAY, LABEL_DARK),
        KeyStyle::Number | KeyStyle::Dpad => (KEY_NUMBER, LABEL_LIGHT),
        KeyStyle::Second => (KEY_SECOND, LABEL_LIGHT),
        KeyStyle::Alpha => (KEY_ALPHA, LABEL_LIGHT),
    }
}

const DEJAVU_SANS: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");
const DEJAVU_SANS_MONO: &[u8] = include_bytes!("../assets/DejaVuSansMono.ttf");

/// Adds DejaVu Sans (proportional) and DejaVu Sans Mono (monospace) as
/// fallbacks behind egui's bundled fonts and sets the text style sizes.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "DejaVuSans".to_owned(),
        Arc::new(FontData::from_static(DEJAVU_SANS)),
    );
    fonts.font_data.insert(
        "DejaVuSansMono".to_owned(),
        Arc::new(FontData::from_static(DEJAVU_SANS_MONO)),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .push("DejaVuSans".to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .push("DejaVuSansMono".to_owned());
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(10.0)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Monospace, FontId::monospace(12.0)),
            (TextStyle::Heading, FontId::proportional(18.0)),
        ]
        .into();
    });
}

/// Whether every char of `s` is drawn from a real glyph of `font`, not the
/// replacement glyph. Call it once the fonts are loaded, i.e. during or after
/// the first pass after [`install_fonts`].
///
/// egui 0.36.2's `has_glyphs` reports `false` for every char of the face that
/// also holds the replacement glyph `◻` (its own TODO in `Font::has_glyph`):
/// with the default fonts that is Hack for `Monospace`, so even `?` "is
/// missing". A char `has_glyphs` rejects therefore counts as present when it
/// lays out to a different atlas glyph than `◻` does.
pub fn has_glyphs(ctx: &egui::Context, font: &FontId, s: &str) -> bool {
    ctx.fonts_mut(|f| {
        if f.has_glyphs(font, s) {
            return true;
        }
        let mut uv = |c: char| {
            let g = f.layout_no_wrap(c.to_string(), font.clone(), Color32::WHITE);
            g.rows
                .first()
                .and_then(|r| r.row.glyphs.first())
                .map(|g| g.uv_rect)
        };
        let replacement = uv('\u{25FB}');
        s.chars().all(|c| uv(c) != replacement)
    })
}
