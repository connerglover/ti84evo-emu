//! The Peripherals tab: one collapsing header per device the backend inspects, each a grid
//! of its registers (name, offset from the base, value, note). Read-only.

use egui::{CollapsingHeader, Grid, RichText, ScrollArea};

use crate::backend::{DeviceView, EmuBackend};

/// A device's header: `"{name} @ 0x{base:08x}"`.
fn header(d: &DeviceView) -> String {
    format!("{} @ 0x{:08x}", d.name, d.base)
}

/// The Peripherals tab body. Reads the devices once per frame, only while it is drawn.
pub(super) fn show(ui: &mut egui::Ui, backend: &dyn EmuBackend) {
    let devices = backend.peripherals();
    if devices.is_empty() {
        ui.weak("No devices");
        return;
    }
    ScrollArea::vertical()
        .id_salt("evo-periph-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, d) in devices.iter().enumerate() {
                CollapsingHeader::new(header(d))
                    .id_salt(("evo-periph", i))
                    .default_open(true)
                    .show(ui, |ui| {
                        Grid::new(("evo-periph-grid", i))
                            .num_columns(4)
                            .striped(true)
                            .show(ui, |ui| {
                                for r in &d.regs {
                                    ui.label(RichText::new(&r.name).monospace());
                                    ui.label(
                                        RichText::new(format!("+0x{:03x}", r.offset)).monospace(),
                                    );
                                    ui.label(
                                        RichText::new(format!("0x{:08x}", r.value)).monospace(),
                                    );
                                    match &r.note {
                                        Some(n) => ui.weak(n),
                                        None => ui.label(""),
                                    };
                                    ui.end_row();
                                }
                            });
                    });
            }
        });
}
