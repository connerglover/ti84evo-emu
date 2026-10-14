//! The LCD view: uploads backend frames to a texture and paints them into the
//! calculator's screen area.

use egui::{Color32, ColorImage, Label, Rect, RichText, TextureHandle, TextureOptions, pos2};
use serde::{Deserialize, Serialize};

use crate::backend::Frame;
use crate::theme::{LABEL_LIGHT, NO_SIGNAL_BG};

/// Native LCD width in pixels.
const LCD_W: f32 = 320.0;
/// Native LCD height in pixels.
const LCD_H: f32 = 240.0;
/// `Nearest` snaps only if the snapped screen keeps at least this share of the width.
const MIN_SNAP_SHARE: f32 = 0.8;

/// How the LCD image is scaled to the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScaleFilter {
    /// Sharp pixels, snapped to whole physical pixels where that costs little size.
    Nearest,
    /// Bilinear filtering over the whole LCD area.
    Smooth,
}

impl ScaleFilter {
    fn texture_options(self) -> TextureOptions {
        match self {
            ScaleFilter::Nearest => TextureOptions::NEAREST,
            ScaleFilter::Smooth => TextureOptions::LINEAR,
        }
    }
}

/// Where the LCD image is drawn inside the screen area `lcd` (points).
///
/// `Smooth` uses all of `lcd`. `Nearest` uses an integer scale `s` (physical
/// pixels per LCD pixel) when `320·s` physical pixels still cover at least 80 %
/// of `lcd`'s width; the snapped rect is centered in `lcd`. Otherwise it uses
/// all of `lcd`.
pub fn lcd_draw_rect(lcd: Rect, pixels_per_point: f32, filter: ScaleFilter) -> Rect {
    if filter == ScaleFilter::Smooth || !valid_ppp(pixels_per_point) {
        return lcd;
    }
    let s = (lcd.width() * pixels_per_point / LCD_W).floor();
    if s < 1.0 || !s.is_finite() {
        return lcd;
    }
    let w = LCD_W * s / pixels_per_point;
    if w < MIN_SNAP_SHARE * lcd.width() {
        return lcd;
    }
    let h = LCD_H * s / pixels_per_point;
    Rect::from_center_size(lcd.center(), egui::vec2(w, h))
}

/// The largest rect with the frame's aspect ratio, centered in `area`.
fn letterbox(area: Rect, width: u32, height: u32) -> Rect {
    let (fw, fh) = (width as f32, height as f32);
    let scale = (area.width() / fw).min(area.height() / fh);
    Rect::from_center_size(area.center(), egui::vec2(fw * scale, fh * scale))
}

/// Rounds `r.min` to the physical pixel grid, keeping the size.
fn align_to_pixels(r: Rect, pixels_per_point: f32) -> Rect {
    if !valid_ppp(pixels_per_point) {
        return r;
    }
    let snap = |v: f32| (v * pixels_per_point).round() / pixels_per_point;
    let min = pos2(snap(r.min.x), snap(r.min.y));
    Rect::from_min_size(min, r.size())
}

/// The LCD texture and the frame it was made from.
pub struct LcdView {
    texture: Option<TextureHandle>,
    /// A copy of the last uploaded frame, to skip uploading an unchanged frame.
    last: Option<Frame>,
    /// The texture options of the last upload.
    uploaded_filter: Option<ScaleFilter>,
    filter: ScaleFilter,
    /// True when the last `update` had a valid frame.
    signal: bool,
    tint: Color32,
}

impl Default for LcdView {
    fn default() -> Self {
        Self::new()
    }
}

impl LcdView {
    pub fn new() -> Self {
        Self {
            texture: None,
            last: None,
            uploaded_filter: None,
            filter: ScaleFilter::Nearest,
            signal: false,
            tint: Color32::WHITE,
        }
    }

    /// True when the last `update` had a valid frame to show.
    pub fn has_signal(&self) -> bool {
        self.signal
    }

    /// Takes the current frame. The texture is uploaded only when the frame
    /// bytes or the filter differ from the last upload. An absent or invalid
    /// frame shows the "No display signal" placeholder.
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        frame: Option<&Frame>,
        backlight: f32,
        filter: ScaleFilter,
    ) {
        let level = if backlight.is_nan() {
            0.0
        } else {
            backlight.clamp(0.0, 1.0)
        };
        self.tint = Color32::from_gray((255.0 * level) as u8);
        self.filter = filter;
        let Some(frame) = frame.filter(|f| f.is_valid()) else {
            self.signal = false;
            return;
        };
        self.signal = true;
        let unchanged = self.last.as_ref() == Some(frame) && self.uploaded_filter == Some(filter);
        if unchanged && self.texture.is_some() {
            return;
        }
        let image = ColorImage::from_rgba_unmultiplied(
            [frame.width as usize, frame.height as usize],
            &frame.rgba,
        );
        let options = filter.texture_options();
        match &mut self.texture {
            Some(tex) => tex.set(image, options),
            None => self.texture = Some(ctx.load_texture("evo-lcd", image, options)),
        }
        match &mut self.last {
            Some(last) => last.clone_from(frame),
            None => self.last = Some(frame.clone()),
        }
        self.uploaded_filter = Some(filter);
    }

    /// Paints the LCD into `screen_rect` (the LCD area in screen points).
    ///
    /// With a frame, the image is letterboxed into [`lcd_draw_rect`] and the
    /// rest of `screen_rect` is left to whatever was painted underneath (the
    /// bezel). Without one, `screen_rect` is filled with [`NO_SIGNAL_BG`] and a
    /// "No display signal" label is added.
    pub fn paint(&self, ui: &mut egui::Ui, screen_rect: Rect) {
        let texture = self.texture.as_ref().filter(|_| self.signal);
        let frame = self.last.as_ref();
        match (texture, frame) {
            (Some(tex), Some(frame)) => {
                let ppp = ui.ctx().pixels_per_point();
                let area = lcd_draw_rect(screen_rect, ppp, self.filter);
                let mut rect = letterbox(area, frame.width, frame.height);
                if self.filter == ScaleFilter::Nearest {
                    rect = align_to_pixels(rect, ppp);
                }
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                ui.painter().image(tex.id(), rect, uv, self.tint);
            }
            _ => {
                ui.painter().rect_filled(screen_rect, 0.0, NO_SIGNAL_BG);
                ui.put(
                    screen_rect,
                    Label::new(RichText::new("No display signal").color(LABEL_LIGHT)),
                );
            }
        }
    }
}

/// True for a usable pixels-per-point value (finite and positive).
fn valid_ppp(pixels_per_point: f32) -> bool {
    pixels_per_point.is_finite() && pixels_per_point > 0.0
}
