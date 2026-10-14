//! The Disassembly tab.

use egui::{Button, RichText, ScrollArea, Sense, TextEdit};

use super::parse_addr;
use crate::backend::{DisasmLine, EmuBackend};

/// How many lines the tab shows.
pub const DISASM_LINES: usize = 32;

/// One disassembly line: `"{bp}{pc} {addr:08x}  {raw:<9}  {text}"`, where
/// `bp` is `●` or a space, `pc` is `▶` or a space, and `raw` is `hhhh` for a
/// 2-byte instruction or `hhhh hhhh` for a 4-byte one.
pub fn format_disasm(l: &DisasmLine, is_bp: bool, is_pc: bool) -> String {
    let bp = if is_bp { '\u{25CF}' } else { ' ' };
    let pc = if is_pc { '\u{25B6}' } else { ' ' };
    let raw = if l.len == 4 {
        format!("{:04x} {:04x}", l.raw >> 16, l.raw & 0xFFFF)
    } else {
        format!("{:04x}", l.raw & 0xFFFF)
    };
    format!("{bp}{pc} {:08x}  {raw:<9}  {}", l.addr, l.text)
}

/// The Disassembly tab's own state.
#[derive(Clone, Debug)]
pub(super) struct DisasmState {
    follow_pc: bool,
    goto: String,
    /// The last address `goto` parsed to.
    start: u32,
}

impl Default for DisasmState {
    fn default() -> Self {
        Self {
            follow_pc: true,
            goto: String::new(),
            start: 0,
        }
    }
}

/// The Disassembly tab body. Clicking a line toggles a breakpoint there.
pub(super) fn show(ui: &mut egui::Ui, st: &mut DisasmState, backend: &mut dyn EmuBackend) {
    let pc = backend.cpu().core[15];
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut st.follow_pc, "Follow PC");
        ui.add_enabled_ui(!st.follow_pc, |ui| {
            ui.label("Go to");
            ui.add(
                TextEdit::singleline(&mut st.goto)
                    .desired_width(110.0)
                    .hint_text("0x10000000")
                    .font(egui::TextStyle::Monospace),
            );
        });
    });
    if let Some(a) = parse_addr(&st.goto) {
        st.start = a;
    }
    let start = if st.follow_pc { pc } else { st.start };
    let lines = backend.disasm(start, DISASM_LINES);
    let bps = backend.breakpoints();
    let mut toggle = None;
    ui.separator();
    ScrollArea::both()
        .id_salt("evo-disasm")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for l in &lines {
                let is_bp = bps.contains(&l.addr);
                let text = format_disasm(l, is_bp, l.addr == pc);
                let resp = ui.add(
                    Button::selectable(is_bp, RichText::new(text).monospace()).sense(Sense::CLICK),
                );
                if resp.clicked() {
                    toggle = Some(l.addr);
                }
            }
        });
    if let Some(a) = toggle {
        backend.toggle_breakpoint(a);
    }
}
