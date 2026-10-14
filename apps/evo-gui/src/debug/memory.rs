//! The Memory tab: a 16x16 hex dump.

use egui::{RichText, ScrollArea, TextEdit};

use super::parse_addr;
use crate::backend::EmuBackend;

/// Bytes per row and rows per dump.
const ROW: usize = 16;
/// The dump's first address until the user types another.
pub const DEFAULT_MEM_ADDR: u32 = 0x2000_0000;

/// One dump row: `{addr:08x}  bb bb … bb  |ascii|`; an unreadable byte shows
/// as `??` and `.`.
fn format_row(addr: u32, bytes: &[Option<u8>]) -> String {
    let hex: Vec<String> = bytes
        .iter()
        .map(|b| b.map_or_else(|| "??".to_owned(), |b| format!("{b:02x}")))
        .collect();
    let ascii: String = bytes
        .iter()
        .map(|b| match b {
            Some(c @ 0x20..=0x7E) => char::from(*c),
            _ => '.',
        })
        .collect();
    format!("{addr:08x}  {}  |{ascii}|", hex.join(" "))
}

/// The Memory tab's own state.
#[derive(Clone, Debug)]
pub(super) struct MemoryState {
    field: String,
    /// The last address `field` parsed to.
    addr: u32,
}

impl Default for MemoryState {
    fn default() -> Self {
        Self {
            field: format!("0x{DEFAULT_MEM_ADDR:08x}"),
            addr: DEFAULT_MEM_ADDR,
        }
    }
}

/// The Memory tab body.
pub(super) fn show(ui: &mut egui::Ui, st: &mut MemoryState, backend: &dyn EmuBackend) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Address");
        ui.add(
            TextEdit::singleline(&mut st.field)
                .desired_width(110.0)
                .font(egui::TextStyle::Monospace),
        );
    });
    let valid = parse_addr(&st.field);
    if let Some(a) = valid {
        st.addr = a;
    } else {
        ui.label(RichText::new("Not a hex address").color(ui.visuals().warn_fg_color));
    }
    let bytes = backend.peek(st.addr, ROW * ROW);
    ui.separator();
    ScrollArea::both()
        .id_salt("evo-memory")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (r, row) in (0u32..).zip(bytes.chunks(ROW)) {
                let addr = st.addr.wrapping_add(r * ROW as u32);
                ui.label(RichText::new(format_row(addr, row)).monospace());
            }
        });
}
