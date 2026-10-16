//! The Watchpoints tab: the watchpoint list with "Remove" per row, an add row ("Start",
//! "Length", "R", "W", "X", "Add") and "Remove all". Changes are enabled only while paused.

use egui::{Button, Id, Key, RichText, ScrollArea, TextEdit};

use super::{button, parse_addr, parse_value};
use crate::backend::{EmuBackend, WatchKinds, Watchpoint};

/// One watchpoint row: `"0x{start:08x}..0x{last:08x} {r}{w}{x}"`, each kind its letter
/// (`R`, `W`, `X`) when watched and `-` when not.
pub fn watch_text(w: &Watchpoint) -> String {
    let k = w.kinds;
    let flag = |on: bool, c: char| if on { c } else { '-' };
    format!(
        "0x{:08x}..0x{:08x} {}{}{}",
        w.start,
        w.last(),
        flag(k.read, 'R'),
        flag(k.write, 'W'),
        flag(k.exec, 'X'),
    )
}

/// The Watchpoints tab's state, kept while the tab or the dock is hidden.
#[derive(Clone, Debug)]
pub(super) struct WatchpointsState {
    start: String,
    len: String,
    kinds: WatchKinds,
    /// True after "Add" refused the row, until the next edit.
    invalid: bool,
}

impl Default for WatchpointsState {
    fn default() -> Self {
        Self {
            start: String::new(),
            len: String::new(),
            // A write watchpoint is the usual one.
            kinds: WatchKinds {
                write: true,
                ..WatchKinds::default()
            },
            invalid: false,
        }
    }
}

impl WatchpointsState {
    /// The add row as a watchpoint, or `None` when it is not a valid one.
    fn parsed(&self) -> Option<Watchpoint> {
        let w = Watchpoint {
            start: parse_addr(&self.start)?,
            len: parse_value(&self.len)?,
            kinds: self.kinds,
        };
        w.is_valid().then_some(w)
    }
}

/// A monospace add-row field; returns (changed, Enter pressed in it).
fn field(ui: &mut egui::Ui, text: &mut String, id: &str, hint: &str, width: f32) -> (bool, bool) {
    let r = ui.add(
        TextEdit::singleline(text)
            .id(Id::new(id))
            .hint_text(hint)
            .desired_width(width)
            .font(egui::TextStyle::Monospace),
    );
    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
    if enter {
        // Keep Enter in the field, away from the calculator.
        r.request_focus();
    }
    (r.changed(), enter)
}

/// The Watchpoints tab body.
pub(super) fn show(
    ui: &mut egui::Ui,
    st: &mut WatchpointsState,
    backend: &mut dyn EmuBackend,
    paused: bool,
    notes: &mut Vec<String>,
) {
    let list = backend.watchpoints();
    let mut changed = false;
    let mut enter = false;
    let mut add = false;
    ui.horizontal_wrapped(|ui| {
        ui.label("Start");
        let (c, e) = field(ui, &mut st.start, "evo-watch-start", "0x20000000", 110.0);
        changed |= c;
        enter |= e;
        ui.label("Length");
        let (c, e) = field(ui, &mut st.len, "evo-watch-len", "4", 60.0);
        changed |= c;
        enter |= e;
    });
    ui.horizontal_wrapped(|ui| {
        changed |= ui.checkbox(&mut st.kinds.read, "R").changed();
        changed |= ui.checkbox(&mut st.kinds.write, "W").changed();
        changed |= ui.checkbox(&mut st.kinds.exec, "X").changed();
        add = button(ui, paused, Button::new("Add"));
        if button(ui, paused && !list.is_empty(), Button::new("Remove all")) {
            set(backend, Vec::new(), notes);
        }
    });
    if changed {
        st.invalid = false;
    }
    if paused && (add || enter) {
        match st.parsed() {
            Some(w) => {
                let mut all = list.clone();
                all.push(w);
                st.invalid = !set(backend, all, notes);
            }
            None => st.invalid = true,
        }
    }
    if st.invalid {
        ui.colored_label(ui.visuals().error_fg_color, "Invalid watchpoint");
    }
    ui.separator();
    // The list as it was at the top of the frame; a change shows on the next one.
    if list.is_empty() {
        ui.weak("No watchpoints");
        return;
    }
    let mut remove = None;
    ScrollArea::vertical()
        .id_salt("evo-watch-list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, w) in list.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(watch_text(w)).monospace());
                    if button(ui, paused, Button::new("Remove")) {
                        remove = Some(i);
                    }
                });
            }
        });
    if let Some(i) = remove {
        let mut rest = list;
        rest.remove(i);
        set(backend, rest, notes);
    }
}

/// Replaces the watchpoints; an error becomes a note. True on success.
fn set(backend: &mut dyn EmuBackend, w: Vec<Watchpoint>, notes: &mut Vec<String>) -> bool {
    match backend.set_watchpoints(w) {
        Ok(()) => true,
        Err(e) => {
            notes.push(format!("Cannot set watchpoints: {e}"));
            false
        }
    }
}
