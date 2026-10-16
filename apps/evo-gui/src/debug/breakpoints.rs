//! The Breakpoints tab: the breakpoint list with "Remove" per row, an "Address" field
//! with "Add", and "Remove all". Changes are enabled only while paused.

use egui::{Button, Id, Key, RichText, ScrollArea, TextEdit};

use super::{button, parse_addr};
use crate::backend::EmuBackend;

/// The Breakpoints tab's state, kept while the tab or the dock is hidden.
#[derive(Clone, Debug, Default)]
pub(super) struct BreakpointsState {
    /// The "Address" field's text.
    field: String,
    /// True after "Add" with text that is not an address, until the next edit.
    bad_addr: bool,
}

/// The Breakpoints tab body.
pub(super) fn show(
    ui: &mut egui::Ui,
    st: &mut BreakpointsState,
    backend: &mut dyn EmuBackend,
    paused: bool,
) {
    let bps = backend.breakpoints();
    ui.horizontal_wrapped(|ui| {
        ui.label("Address");
        let field = ui.add(
            TextEdit::singleline(&mut st.field)
                .id(Id::new("evo-bp-addr"))
                .hint_text("0x10000000")
                .desired_width(110.0)
                .font(egui::TextStyle::Monospace),
        );
        if field.changed() {
            st.bad_addr = false;
        }
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if enter {
            // Keep Enter in the field, away from the calculator.
            field.request_focus();
        }
        let add = button(ui, paused, Button::new("Add"));
        if paused && (add || enter) {
            match parse_addr(&st.field) {
                Some(a) => {
                    // Breakpoints are on instruction addresses; a Thumb bit is dropped.
                    let a = a & !1;
                    if !bps.contains(&a) {
                        backend.toggle_breakpoint(a);
                    }
                    st.field.clear();
                    st.bad_addr = false;
                }
                None => st.bad_addr = true,
            }
        }
        if button(ui, paused && !bps.is_empty(), Button::new("Remove all")) {
            backend.set_breakpoints(Vec::new());
        }
    });
    if st.bad_addr {
        ui.colored_label(ui.visuals().error_fg_color, "Not a hex address");
    }
    ui.separator();
    // The list as it was at the top of the frame; a change shows on the next one.
    if bps.is_empty() {
        ui.weak("No breakpoints");
        return;
    }
    let mut remove = None;
    ScrollArea::vertical()
        .id_salt("evo-bp-list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for &a in &bps {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("0x{a:08x}")).monospace());
                    if button(ui, paused, Button::new("Remove")) {
                        remove = Some(a);
                    }
                });
            }
        });
    if let Some(a) = remove {
        backend.set_breakpoints(bps.into_iter().filter(|&b| b != a).collect());
    }
}
