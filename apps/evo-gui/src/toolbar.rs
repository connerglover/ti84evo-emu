//! The tool table, the toolbar, the slot picker and the About window.

use egui::text::LayoutJob;
use egui::{
    Align2, Button, FontId, Frame, Id, Margin, Order, Pos2, Rect, RichText, Sense, TextFormat,
    Vec2, WidgetInfo, WidgetType, vec2,
};

use crate::app::TITLE;
use crate::store::SLOTS;
use crate::theme::{LABEL_DARK, TOOLBAR_BG, has_glyphs};

/// Width of the toolbar, in points.
pub const TOOLBAR_W: f32 = 48.0;
/// Size of a tool button, in points.
const BUTTON_SIZE: Vec2 = vec2(40.0, 36.0);
/// Size of an icon glyph.
const ICON_SIZE: f32 = 18.0;
/// Size of a fallback word (it must fit a 40-point button).
const FALLBACK_SIZE: f32 = 10.0;
/// Gap between the toolbar and the slot picker, in points.
const PICKER_GAP: f32 = 4.0;

/// A toolbar button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Screenshot,
    SaveState,
    LoadState,
    Reset,
    Smooth,
    Debug,
    Keymap,
    About,
}

impl Tool {
    /// Every tool, top to bottom.
    pub const ALL: [Tool; 8] = [
        Tool::Screenshot,
        Tool::SaveState,
        Tool::LoadState,
        Tool::Reset,
        Tool::Smooth,
        Tool::Debug,
        Tool::Keymap,
        Tool::About,
    ];

    /// The button's glyph.
    pub fn icon(self) -> &'static str {
        match self {
            Tool::Screenshot => "\u{1F4F7}",
            Tool::SaveState => "\u{1F4BE}",
            Tool::LoadState => "\u{1F4C2}",
            Tool::Reset => "\u{1F504}",
            Tool::Smooth => "\u{1F5BC}",
            Tool::Debug => "\u{1F41B}",
            Tool::Keymap => "\u{2328}",
            Tool::About => "\u{2139}",
        }
    }

    /// The button's text when the fonts cannot draw [`Tool::icon`].
    pub fn fallback(self) -> &'static str {
        match self {
            Tool::Screenshot => "Shot",
            Tool::SaveState => "Save",
            Tool::LoadState => "Load",
            Tool::Reset => "Reset",
            Tool::Smooth => "Smooth",
            Tool::Debug => "Debug",
            Tool::Keymap => "Keys",
            Tool::About => "About",
        }
    }

    /// The tooltip, which is also the button's accessibility label.
    pub fn tooltip(self) -> &'static str {
        match self {
            Tool::Screenshot => "Screenshot",
            Tool::SaveState => "Save state",
            Tool::LoadState => "Load state",
            Tool::Reset => "Reset",
            Tool::Smooth => "Smooth scaling (better at non-integer sizes)",
            Tool::Debug => "Debug panels",
            Tool::Keymap => "Keyboard mapping",
            Tool::About => "About",
        }
    }
}

/// For each tool in [`Tool::ALL`] order, whether its icon is drawable.
pub fn drawable_icons(ctx: &egui::Context) -> [bool; 8] {
    let font = FontId::proportional(ICON_SIZE);
    Tool::ALL.map(|t| has_glyphs(ctx, &font, t.icon()))
}

/// What the toolbar did this frame.
pub struct ToolbarOutput {
    /// The tool clicked, if any.
    pub clicked: Option<Tool>,
    /// The screen rect of the clicked button.
    pub clicked_rect: Option<Rect>,
}

/// Draws the tool buttons in `ui` (the toolbar panel).
///
/// The buttons sense clicks but are not focusable, so Tab never focuses them
/// and Space/Enter (calculator keys) never click them.
pub fn toolbar_buttons(
    ui: &mut egui::Ui,
    icons: &[bool; 8],
    active: impl Fn(Tool) -> bool,
) -> ToolbarOutput {
    let mut out = ToolbarOutput {
        clicked: None,
        clicked_rect: None,
    };
    ui.vertical_centered(|ui| {
        ui.add_space(4.0);
        for (tool, &icon) in Tool::ALL.iter().zip(icons) {
            let text = if icon {
                RichText::new(tool.icon()).size(ICON_SIZE)
            } else {
                RichText::new(tool.fallback()).size(FALLBACK_SIZE)
            };
            let resp = ui.add(
                Button::new(text.color(LABEL_DARK))
                    .min_size(BUTTON_SIZE)
                    .selected(active(*tool))
                    .sense(Sense::CLICK),
            );
            let label = tool.tooltip();
            resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
            let resp = resp.on_hover_text(label);
            if resp.clicked() {
                out.clicked = Some(*tool);
                out.clicked_rect = Some(resp.rect);
            }
        }
    });
    out
}

/// What the slot picker is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerMode {
    Save,
    Load,
}

/// What the slot picker did this frame.
pub struct PickerOutput {
    /// The slot clicked, if any.
    pub picked: Option<u8>,
    /// True when the user clicked outside the picker.
    pub clicked_elsewhere: bool,
}

/// The slot picker: an area at `pos` with one row per slot, a `Slot {n}`
/// button then a `saved` or `empty` label.
pub fn slot_picker(
    ctx: &egui::Context,
    pos: Pos2,
    mode: PickerMode,
    has_slot: impl Fn(u8) -> bool,
) -> PickerOutput {
    let mut picked = None;
    let area = egui::Area::new(Id::new("evo-slot-picker"))
        .order(Order::Foreground)
        .fixed_pos(pos + vec2(PICKER_GAP, 0.0))
        .show(ctx, |ui| {
            Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(RichText::new(match mode {
                    PickerMode::Save => "Save to slot",
                    PickerMode::Load => "Load from slot",
                }));
                egui::Grid::new("evo-slot-rows")
                    .num_columns(2)
                    .show(ui, |ui| {
                        for n in 1..=SLOTS {
                            let resp = ui.add(Button::new(format!("Slot {n}")).sense(Sense::CLICK));
                            if resp.clicked() {
                                picked = Some(n);
                            }
                            ui.label(if has_slot(n) { "saved" } else { "empty" });
                            ui.end_row();
                        }
                    });
            });
        });
    PickerOutput {
        picked,
        clicked_elsewhere: area.response.clicked_elsewhere(),
    }
}

/// The About window, centered over the app.
pub fn about_window(ctx: &egui::Context, open: &mut bool) {
    egui::Window::new("About")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(300.0)
        .show(ctx, |ui| {
            let mut job = LayoutJob::default();
            let body = |size: f32, color| TextFormat {
                font_id: FontId::proportional(size),
                color,
                ..Default::default()
            };
            job.append(TITLE, 0.0, body(16.0, ui.visuals().strong_text_color()));
            job.append(
                env!("CARGO_PKG_VERSION"),
                6.0,
                body(14.0, ui.visuals().text_color()),
            );
            ui.label(job);
            ui.add_space(6.0);
            ui.label(
                "An independent, open-source emulator. Not affiliated with Texas \
                 Instruments. TI-84 is a trademark of Texas Instruments.",
            );
            ui.add_space(6.0);
            ui.label("You must supply your own OS bundle.");
        });
}

/// The toolbar panel's frame.
pub fn toolbar_frame() -> Frame {
    Frame::NONE.fill(TOOLBAR_BG).inner_margin(Margin::ZERO)
}
