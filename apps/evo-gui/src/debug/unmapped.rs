//! The Unmapped tab: accesses to addresses no device models.

use egui::{Grid, RichText, ScrollArea};

use crate::backend::UnmappedRow;

/// The Unmapped tab body.
pub(super) fn show(ui: &mut egui::Ui, rows: &[UnmappedRow]) {
    let n = rows.len();
    ui.label(format!("{n} address{}", if n == 1 { "" } else { "es" }));
    ui.separator();
    ScrollArea::both()
        .id_salt("evo-unmapped")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Grid::new("evo-unmapped-grid")
                .num_columns(7)
                .spacing([10.0, 2.0])
                .striped(true)
                .show(ui, |ui| {
                    for col in ["addr", "width", "kind", "value", "pc", "sec", "count"] {
                        ui.label(RichText::new(col).strong());
                    }
                    ui.end_row();
                    for r in rows {
                        let mono = |s: String| RichText::new(s).monospace();
                        ui.label(mono(format!("0x{:08x}", r.addr)));
                        ui.label(mono(r.width.to_string()));
                        ui.label(mono(r.kind.to_owned()));
                        ui.label(mono(
                            r.value
                                .map_or_else(|| "-".to_owned(), |v| format!("0x{v:08x}")),
                        ));
                        ui.label(mono(format!("0x{:08x}", r.pc)));
                        ui.label(mono(if r.secure { "S" } else { "NS" }.to_owned()));
                        ui.label(mono(r.count.to_string()));
                        ui.end_row();
                    }
                });
        });
}
