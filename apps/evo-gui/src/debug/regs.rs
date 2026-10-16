//! The Registers tab. While paused, clicking a value opens a text field: Enter applies a
//! [`parse_value`] value, Escape (or clicking elsewhere) cancels, and an unparsable value
//! shows "Not a value" and keeps the field. While running, values are plain labels.

use egui::text::{CCursor, CCursorRange};
use egui::{CursorIcon, EventFilter, Grid, Id, Key, Label, RichText, Sense, TextEdit};

use super::parse_value;
use crate::backend::EmuBackend;

/// Display names of `CpuView::core`, in order; `set_core_reg` index 16 is xPSR.
const CORE_NAMES: [&str; 16] = [
    "r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "r12", "sp", "lr",
    "pc",
];

/// The narrowest tab body that shows two registers per grid row, in points.
const TWO_PER_ROW_W: f32 = 280.0;

/// The edit field's width, in points: room for `0x` and eight digits.
const FIELD_W: f32 = 92.0;

/// The register value format.
fn hex_text(v: u32) -> String {
    format!("0x{v:08x}")
}

/// A register the tab can write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Reg {
    /// `set_core_reg` index: r0..pc, then xPSR (16).
    Core(usize),
    /// `set_banked_reg` name, as in `CpuView::banked`.
    Banked(&'static str),
}

impl Reg {
    fn name(self) -> &'static str {
        match self {
            Reg::Core(i) => CORE_NAMES.get(i).copied().unwrap_or("xpsr"),
            Reg::Banked(name) => name,
        }
    }
}

/// An open edit field.
#[derive(Clone, Debug)]
struct Edit {
    reg: Reg,
    text: String,
    /// The field takes the keyboard focus this frame (just opened, or Enter on an
    /// unparsable value).
    focus: bool,
    /// The text was submitted and did not parse; cleared by the next edit.
    bad: bool,
}

/// The Registers tab's state, kept while the tab or the dock is hidden.
#[derive(Clone, Debug, Default)]
pub(super) struct RegsState {
    edit: Option<Edit>,
}

/// What a value cell asked for this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CellAction {
    Open(Reg, u32),
    Apply(Reg, u32),
    Close,
}

/// Keeps the frame's action: clicking another value closes the open field (its focus is
/// lost) and opens the new one, so `Open` wins over `Close` in either drawing order.
fn merge(action: &mut Option<CellAction>, a: CellAction) {
    if action.is_none() || a != CellAction::Close {
        *action = Some(a);
    }
}

/// Draws one value cell: the field when `reg` is being edited, else a label that opens
/// the field when clicked while paused.
fn cell(
    ui: &mut egui::Ui,
    edit: &mut Option<Edit>,
    reg: Reg,
    value: u32,
    paused: bool,
) -> Option<CellAction> {
    let Some(e) = edit.as_mut().filter(|e| e.reg == reg) else {
        // The plain text colour while clickable too, so values look the same paused or
        // running.
        let text = RichText::new(hex_text(value)).monospace();
        let label = Label::new(text.color(ui.visuals().text_color()));
        if !paused {
            ui.add(label);
            return None;
        }
        // CLICK without FOCUSABLE: Space and Enter (calculator keys) never open it.
        let r = ui
            .add(label.sense(Sense::CLICK))
            .on_hover_cursor(CursorIcon::Text);
        return r.clicked().then_some(CellAction::Open(reg, value));
    };
    let id = Id::new(("evo-reg-edit", reg));
    let mut out = TextEdit::singleline(&mut e.text)
        .id(id)
        .font(egui::TextStyle::Monospace)
        .desired_width(FIELD_W)
        // The field keeps Escape: egui would otherwise drop the focus before the app
        // reads the keyboard, and Escape would reach the calculator as CLEAR.
        .event_filter(EventFilter {
            horizontal_arrows: true,
            vertical_arrows: true,
            escape: true,
            ..Default::default()
        })
        .show(ui);
    let r = out.response;
    if r.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
        r.surrender_focus();
        return Some(CellAction::Close);
    }
    if e.focus {
        e.focus = false;
        r.request_focus();
        // Select the whole value, so typing replaces it.
        let end = e.text.chars().count();
        out.state
            .cursor
            .set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(end))));
        out.state.store(ui.ctx(), id);
    }
    if r.changed() {
        e.bad = false;
    }
    if !r.lost_focus() {
        return None;
    }
    if !ui.input(|i| i.key_pressed(Key::Enter)) {
        // Escape, or a click elsewhere.
        return Some(CellAction::Close);
    }
    match parse_value(&e.text) {
        Some(v) => Some(CellAction::Apply(reg, v)),
        None => {
            e.bad = true;
            e.focus = true;
            ui.ctx().request_repaint();
            None
        }
    }
}

/// The Registers tab body. Backend errors go to `notes`.
pub(super) fn show(
    ui: &mut egui::Ui,
    st: &mut RegsState,
    backend: &mut dyn EmuBackend,
    paused: bool,
    notes: &mut Vec<String>,
) {
    if !paused {
        st.edit = None;
    }
    let cpu = backend.cpu();
    let mut action = None;
    let regs = CORE_NAMES
        .iter()
        .copied()
        .zip(cpu.core)
        .chain(std::iter::once(("xpsr", cpu.xpsr)));
    // Two registers per row when they fit, else one.
    let per_row = if ui.available_width() >= TWO_PER_ROW_W {
        2
    } else {
        1
    };
    Grid::new("evo-regs")
        .num_columns(2 * per_row)
        .spacing([12.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for (i, (name, value)) in regs.enumerate() {
                ui.label(RichText::new(name).monospace());
                if let Some(a) = cell(ui, &mut st.edit, Reg::Core(i), value, paused) {
                    merge(&mut action, a);
                }
                if i % per_row == per_row - 1 {
                    ui.end_row();
                }
            }
        });
    if st.edit.as_ref().is_some_and(|e| e.bad) {
        ui.colored_label(ui.visuals().error_fg_color, "Not a value");
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(if cpu.secure { "Secure" } else { "Non-secure" });
        ui.separator();
        ui.label(if cpu.handler { "Handler" } else { "Thread" });
    });
    if !cpu.banked.is_empty() {
        ui.add_space(6.0);
        Grid::new("evo-banked")
            .num_columns(2)
            .spacing([12.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                for &(name, value) in &cpu.banked {
                    ui.label(RichText::new(name).monospace());
                    if let Some(a) = cell(ui, &mut st.edit, Reg::Banked(name), value, paused) {
                        merge(&mut action, a);
                    }
                    ui.end_row();
                }
            });
    }
    match action {
        None => {}
        Some(CellAction::Open(reg, value)) => {
            st.edit = Some(Edit {
                reg,
                text: hex_text(value),
                focus: true,
                bad: false,
            });
            ui.ctx().request_repaint();
        }
        Some(CellAction::Close) => st.edit = None,
        Some(CellAction::Apply(reg, value)) => {
            st.edit = None;
            let result = match reg {
                Reg::Core(i) => backend.set_core_reg(i, value),
                Reg::Banked(name) => backend.set_banked_reg(name, value),
            };
            if let Err(e) = result {
                notes.push(format!("Cannot set {}: {e}", reg.name()));
            }
            ui.ctx().request_repaint();
        }
    }
}
