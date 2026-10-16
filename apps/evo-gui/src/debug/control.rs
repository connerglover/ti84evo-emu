//! The Control tab: run to an address, and the cycle counter with a mark.

use egui::{Button, RichText, TextEdit};

use super::{button, parse_addr};
use crate::backend::{EmuBackend, Goal};

/// The "Run to" field's hint.
const RUN_TO_HINT: &str = "0x10000000";

/// The cycle counter's two labels: `("Cycles {cycles}", "Since mark {cycles - mark}")`.
/// The difference saturates at 0 when the mark is past the counter (after a reset or a
/// state load).
pub fn cycle_text(cycles: u64, mark: u64) -> (String, String) {
    (
        format!("Cycles {cycles}"),
        format!("Since mark {}", cycles.saturating_sub(mark)),
    )
}

/// The Control tab's state, kept while the tab or the dock is hidden.
#[derive(Clone, Debug, Default)]
pub(super) struct ControlState {
    /// The "Run to" field's text.
    run_to: String,
    /// True after "Run" with text that is not an address, until the next edit.
    bad_addr: bool,
    /// The cycle count when "Mark" was last clicked.
    mark: u64,
}

/// The Control tab body. Returns the goal "Run" asked for (only while paused).
pub(super) fn show(
    ui: &mut egui::Ui,
    state: &mut ControlState,
    backend: &dyn EmuBackend,
    paused: bool,
) -> Option<Goal> {
    let mut goal = None;
    ui.horizontal(|ui| {
        ui.label("Run to");
        let field = ui.add(
            TextEdit::singleline(&mut state.run_to)
                .hint_text(RUN_TO_HINT)
                .font(egui::TextStyle::Monospace)
                .desired_width(110.0),
        );
        if field.changed() {
            state.bad_addr = false;
        }
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let run = button(ui, paused, Button::new("Run"));
        if paused && (run || enter) {
            match parse_addr(&state.run_to) {
                Some(a) => {
                    state.bad_addr = false;
                    goal = Some(Goal::RunTo(a));
                }
                None => state.bad_addr = true,
            }
        }
    });
    if state.bad_addr {
        ui.colored_label(ui.visuals().error_fg_color, "Not a hex address");
    }
    ui.separator();
    let cycles = backend.cycles();
    let (total, since) = cycle_text(cycles, state.mark);
    ui.label(RichText::new(total).monospace());
    ui.horizontal(|ui| {
        ui.label(RichText::new(since).monospace());
        if button(ui, true, Button::new("Mark")) {
            state.mark = cycles;
        }
    });
    goal
}
