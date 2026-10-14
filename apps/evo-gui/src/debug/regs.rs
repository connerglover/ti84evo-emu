//! The Registers tab.

use egui::{Grid, RichText};

use crate::backend::CpuView;

/// Display names of `CpuView::core`, in order.
const CORE_NAMES: [&str; 16] = [
    "r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "r12", "sp", "lr",
    "pc",
];

/// The narrowest tab body that shows two registers per grid row, in points.
const TWO_PER_ROW_W: f32 = 280.0;

/// The register value format.
fn hex(v: u32) -> RichText {
    RichText::new(format!("0x{v:08x}")).monospace()
}

/// The Registers tab body.
pub(super) fn show(ui: &mut egui::Ui, cpu: &CpuView) {
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
                ui.label(hex(value));
                if i % per_row == per_row - 1 {
                    ui.end_row();
                }
            }
        });
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
                for (name, value) in &cpu.banked {
                    ui.label(RichText::new(*name).monospace());
                    ui.label(hex(*value));
                    ui.end_row();
                }
            });
    }
}
