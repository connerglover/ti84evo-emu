//! The Visualizer tab: memory drawn as an image.
//!
//! Controls are an address, a pixel format, a width and a height (each size clamped to
//! 1..=[`VIS_MAX`]). While the tab is visible it makes one `peek` of
//! `width * height * bits / 8` bytes (rounded up) per frame and shows the image at the
//! panel width, keeping its aspect, with nearest filtering. Unreadable bytes are magenta.

use egui::{Button, ColorImage, Id, Image, Key, RichText, TextEdit, TextureHandle, TextureOptions};

use super::{button, parse_addr};
use crate::backend::EmuBackend;

/// The largest width and height, in pixels.
pub const VIS_MAX: u32 = 1024;
/// The address the tab starts at.
pub const DEFAULT_VIS_ADDR: u32 = 0x2004_8000;
const DEFAULT_W: u32 = 320;
const DEFAULT_H: u32 = 240;
/// An unreadable pixel.
const MAGENTA: [u8; 4] = [0xFF, 0x00, 0xFF, 0xFF];
const BLACK: [u8; 4] = [0x00, 0x00, 0x00, 0xFF];
const WHITE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];

/// How memory bytes map to pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PixelFormat {
    /// Little-endian `u16`: red in bits 15:11, green 10:5, blue 4:0.
    #[default]
    Rgb565,
    /// Three bytes: red, green, blue.
    Rgb888,
    /// One byte per pixel, 0 black.
    Gray8,
    /// Eight pixels per byte, most significant bit first; 1 is black.
    Mono1,
}

impl PixelFormat {
    /// Every format, in selector order.
    pub const ALL: [PixelFormat; 4] = [
        PixelFormat::Rgb565,
        PixelFormat::Rgb888,
        PixelFormat::Gray8,
        PixelFormat::Mono1,
    ];

    /// The selector label.
    pub fn label(self) -> &'static str {
        match self {
            PixelFormat::Rgb565 => "RGB565",
            PixelFormat::Rgb888 => "RGB888",
            PixelFormat::Gray8 => "Gray 8-bit",
            PixelFormat::Mono1 => "1-bit",
        }
    }

    /// Bits per pixel.
    pub fn bits(self) -> u32 {
        match self {
            PixelFormat::Rgb565 => 16,
            PixelFormat::Rgb888 => 24,
            PixelFormat::Gray8 => 8,
            PixelFormat::Mono1 => 1,
        }
    }

    /// Bytes an image of `width × height` pixels takes (a partial last byte counts).
    pub fn image_len(self, width: u32, height: u32) -> usize {
        let bits = u64::from(width) * u64::from(height) * u64::from(self.bits());
        usize::try_from(bits.div_ceil(8)).unwrap_or(usize::MAX)
    }
}

/// Expands a 5- or 6-bit channel to 8 bits (the top bits repeat in the low bits).
fn expand(v: u16, bits: u32) -> u8 {
    let v = u32::from(v);
    ((v << (8 - bits)) | (v >> (2 * bits - 8))) as u8
}

/// Renders `bytes` as a `width × height` RGBA image (`width * height * 4` bytes). A pixel
/// with any unreadable (`None`) or missing byte is magenta `(0xFF, 0x00, 0xFF, 0xFF)`.
pub fn render(bytes: &[Option<u8>], fmt: PixelFormat, width: u32, height: u32) -> Vec<u8> {
    let n = width as usize * height as usize;
    let byte = |i: usize| bytes.get(i).copied().flatten();
    let mut out = Vec::with_capacity(n * 4);
    for p in 0..n {
        let px = match fmt {
            PixelFormat::Rgb565 => match (byte(2 * p), byte(2 * p + 1)) {
                (Some(lo), Some(hi)) => {
                    let v = u16::from_le_bytes([lo, hi]);
                    [
                        expand(v >> 11, 5),
                        expand((v >> 5) & 0x3F, 6),
                        expand(v & 0x1F, 5),
                        0xFF,
                    ]
                }
                _ => MAGENTA,
            },
            PixelFormat::Rgb888 => match (byte(3 * p), byte(3 * p + 1), byte(3 * p + 2)) {
                (Some(r), Some(g), Some(b)) => [r, g, b, 0xFF],
                _ => MAGENTA,
            },
            PixelFormat::Gray8 => byte(p).map_or(MAGENTA, |v| [v, v, v, 0xFF]),
            PixelFormat::Mono1 => byte(p / 8).map_or(MAGENTA, |v| {
                if v & (0x80 >> (p % 8)) != 0 {
                    BLACK
                } else {
                    WHITE
                }
            }),
        };
        out.extend_from_slice(&px);
    }
    out
}

/// Parses a decimal size and clamps it to 1..=[`VIS_MAX`] (`5000` is 1024, `0` is 1).
/// Surrounding whitespace is trimmed; `None` for anything but decimal digits.
pub fn parse_size(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Digits only, so a parse error is an overflow: as large as it gets.
    let v = s.parse::<u64>().unwrap_or(u64::MAX);
    Some(v.clamp(1, u64::from(VIS_MAX)) as u32)
}

/// A size field and the last size it parsed to.
#[derive(Clone, Debug)]
struct SizeField {
    text: String,
    value: u32,
}

impl SizeField {
    fn new(value: u32) -> Self {
        Self {
            text: value.to_string(),
            value,
        }
    }

    /// Draws the field; returns false when its text is not a size.
    fn show(&mut self, ui: &mut egui::Ui, label: &str, id: &str) -> bool {
        ui.label(label);
        let r = ui.add(
            TextEdit::singleline(&mut self.text)
                .id(Id::new(id))
                .desired_width(50.0)
                .font(egui::TextStyle::Monospace),
        );
        let parsed = parse_size(&self.text);
        if let Some(v) = parsed {
            self.value = v;
            if r.lost_focus() {
                // Show the clamped value once the user leaves the field.
                self.text = v.to_string();
            }
        }
        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            // Keep Enter in the field, away from the calculator.
            r.request_focus();
        }
        parsed.is_some()
    }
}

/// The Visualizer's own state, kept while the tab or the dock is hidden.
#[derive(Clone)]
pub(super) struct VisualizerState {
    field: String,
    addr: u32,
    format: PixelFormat,
    width: SizeField,
    height: SizeField,
    texture: Option<TextureHandle>,
    /// The size and pixels of the last upload, to skip uploading an unchanged image.
    last: Option<([usize; 2], Vec<u8>)>,
}

// `TextureHandle` has no `Debug`.
impl std::fmt::Debug for VisualizerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisualizerState")
            .field("addr", &self.addr)
            .field("format", &self.format)
            .field("width", &self.width.value)
            .field("height", &self.height.value)
            .finish_non_exhaustive()
    }
}

impl Default for VisualizerState {
    fn default() -> Self {
        Self {
            field: format!("0x{DEFAULT_VIS_ADDR:08x}"),
            addr: DEFAULT_VIS_ADDR,
            format: PixelFormat::default(),
            width: SizeField::new(DEFAULT_W),
            height: SizeField::new(DEFAULT_H),
            texture: None,
            last: None,
        }
    }
}

/// Draws the Visualizer tab.
pub(super) fn show(ui: &mut egui::Ui, st: &mut VisualizerState, backend: &dyn EmuBackend) {
    let mut addr_ok = true;
    ui.horizontal_wrapped(|ui| {
        ui.label("Address");
        let r = ui.add(
            TextEdit::singleline(&mut st.field)
                .id(Id::new("evo-vis-addr"))
                .hint_text("0x20048000")
                .desired_width(110.0)
                .font(egui::TextStyle::Monospace),
        );
        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            r.request_focus();
        }
        match parse_addr(&st.field) {
            Some(a) => st.addr = a,
            None => addr_ok = false,
        }
    });
    ui.horizontal_wrapped(|ui| {
        for fmt in PixelFormat::ALL {
            if button(ui, true, Button::selectable(st.format == fmt, fmt.label())) {
                st.format = fmt;
            }
        }
    });
    let mut sizes_ok = true;
    ui.horizontal_wrapped(|ui| {
        sizes_ok &= st.width.show(ui, "Width", "evo-vis-width");
        sizes_ok &= st.height.show(ui, "Height", "evo-vis-height");
    });
    if !addr_ok {
        ui.label(RichText::new("Not a hex address").color(ui.visuals().warn_fg_color));
    }
    if !sizes_ok {
        ui.label(RichText::new("Not a size").color(ui.visuals().warn_fg_color));
    }
    let (w, h) = (st.width.value, st.height.value);
    let len = st.format.image_len(w, h);
    ui.weak(format!("{w} × {h}, {len} bytes"));
    let bytes = backend.peek(st.addr, len);
    let rgba = render(&bytes, st.format, w, h);
    let size = [w as usize, h as usize];
    let unchanged = st
        .last
        .as_ref()
        .is_some_and(|(s, px)| *s == size && *px == rgba);
    if !unchanged || st.texture.is_none() {
        let image = ColorImage::from_rgba_unmultiplied(size, &rgba);
        match &mut st.texture {
            Some(t) => t.set(image, TextureOptions::NEAREST),
            None => {
                st.texture = Some(ui.ctx().load_texture(
                    "evo-visualizer",
                    image,
                    TextureOptions::NEAREST,
                ));
            }
        }
        st.last = Some((size, rgba));
    }
    let Some(texture) = &st.texture else { return };
    egui::ScrollArea::vertical()
        .id_salt("evo-vis-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let avail = ui.available_width().max(1.0);
            let shown = egui::vec2(avail, avail * h as f32 / w as f32);
            ui.add(Image::new(texture).fit_to_exact_size(shown));
        });
}
