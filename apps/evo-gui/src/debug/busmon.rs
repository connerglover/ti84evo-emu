//! The Bus monitor tab: "Start" and "Length" fields, "Monitor" (records the accesses to that
//! range), "Stop" (keeps the records), "Clear", and the newest [`BUS_SHOWN`] records as
//! [`bus_line`] lines. The monitor only observes, so its controls work while running too.

use egui::{Button, Id, Key, RichText, ScrollArea, TextEdit, TextStyle};

use super::{button, parse_addr, parse_value};
use crate::backend::{AccessDir, BusRecord, EmuBackend};

/// How many of the newest records the tab shows.
pub const BUS_SHOWN: usize = 1000;

/// One record: cycle (right-aligned, 12 wide), pc, `R`/`W`/`X`, address, width in bytes and
/// the value as `2 * width` hex digits (cut to the width).
pub fn bus_line(r: &BusRecord) -> String {
    let dir = match r.dir {
        AccessDir::Read => 'R',
        AccessDir::Write => 'W',
        AccessDir::Exec => 'X',
    };
    let width = usize::from(r.width.clamp(1, 4));
    let value = if width >= 4 {
        r.value
    } else {
        r.value & ((1u32 << (8 * width)) - 1)
    };
    format!(
        "{:>12} {:08x} {} {:08x} {} {:0w$x}",
        r.cycle,
        r.pc,
        dir,
        r.addr,
        r.width,
        value,
        w = 2 * width
    )
}

/// The Bus monitor tab's state, kept while the tab or the dock is hidden.
#[derive(Clone, Debug, Default)]
pub(super) struct BusState {
    start: String,
    len: String,
    /// The range last handed to the backend, `(start, len)`; `None` when stopped.
    active: Option<(u32, u32)>,
    /// True after "Monitor" refused the fields, until the next edit.
    invalid: bool,
}

impl BusState {
    /// The fields as `(start, len)`: a non-empty range ending at or below `u32::MAX`.
    fn parsed(&self) -> Option<(u32, u32)> {
        let start = parse_addr(&self.start)?;
        let len = parse_value(&self.len)?;
        (len >= 1 && start.checked_add(len - 1).is_some()).then_some((start, len))
    }
}

/// A monospace field; returns (changed, Enter pressed in it).
fn field(ui: &mut egui::Ui, text: &mut String, id: &str, hint: &str, width: f32) -> (bool, bool) {
    let r = ui.add(
        TextEdit::singleline(text)
            .id(Id::new(id))
            .hint_text(hint)
            .desired_width(width)
            .font(TextStyle::Monospace),
    );
    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
    if enter {
        // Keep Enter in the field, away from the calculator.
        r.request_focus();
    }
    (r.changed(), enter)
}

/// The records summary line.
fn summary(n: usize) -> String {
    match n {
        0 => "No records".to_owned(),
        1 => "1 record".to_owned(),
        n if n <= BUS_SHOWN => format!("{n} records"),
        n => format!("Newest {BUS_SHOWN} of {n} records"),
    }
}

/// The Bus monitor tab body. Reads the records once per frame, only while it is drawn.
pub(super) fn show(ui: &mut egui::Ui, st: &mut BusState, backend: &mut dyn EmuBackend) {
    let mut changed = false;
    let mut enter = false;
    ui.horizontal_wrapped(|ui| {
        ui.label("Start");
        let (c, e) = field(ui, &mut st.start, "evo-bus-start", "0x20000000", 110.0);
        changed |= c;
        enter |= e;
        ui.label("Length");
        let (c, e) = field(ui, &mut st.len, "evo-bus-len", "0x10000", 80.0);
        changed |= c;
        enter |= e;
    });
    if changed {
        st.invalid = false;
    }
    ui.horizontal_wrapped(|ui| {
        if button(ui, true, Button::new("Monitor")) || enter {
            match st.parsed() {
                Some(range) => {
                    backend.set_bus_monitor(Some(range));
                    st.active = Some(range);
                    st.invalid = false;
                }
                // A running monitor keeps going.
                None => st.invalid = true,
            }
        }
        if button(ui, st.active.is_some(), Button::new("Stop")) {
            backend.set_bus_monitor(None);
            st.active = None;
        }
        if button(ui, true, Button::new("Clear")) {
            backend.clear_bus_records();
        }
    });
    if st.invalid {
        ui.colored_label(ui.visuals().error_fg_color, "Invalid range");
    }
    match st.active {
        Some((start, len)) => ui.label(format!(
            "Monitoring 0x{start:08x}..0x{:08x}",
            start.saturating_add(len - 1)
        )),
        None => ui.weak("Not monitoring"),
    };
    ui.separator();
    let recs = backend.bus_records();
    ui.label(summary(recs.len()));
    if recs.is_empty() {
        return;
    }
    let shown = &recs[recs.len().saturating_sub(BUS_SHOWN)..];
    let head = format!(
        "{:>12} {:8} {} {:8} {} {}",
        "cycle", "pc", "d", "addr", "w", "value"
    );
    ui.label(RichText::new(head).monospace().weak());
    let row_h = ui.text_style_height(&TextStyle::Monospace);
    ScrollArea::vertical()
        .id_salt("evo-bus-list")
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show_rows(ui, row_h, shown.len(), |ui, rows| {
            for r in &shown[rows] {
                ui.label(RichText::new(bus_line(r)).monospace());
            }
        });
}
