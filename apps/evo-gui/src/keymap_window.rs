//! The keyboard-mapping window: lists every calculator key with its PC keys
//! and rebinds them by capturing the next key press.

use egui::{Align2, Event, Grid, ScrollArea, Window};

use crate::input::{Keymap, PcKey, pc_key_label};
use crate::keypad::KeyId;

/// The window title.
pub const KEYMAP_TITLE: &str = "Keyboard mapping";
/// The note above the key list.
pub const KEYMAP_NOTE: &str = "Letters type the key with that alpha legend. F10 may be taken \
                               by Windows; rebind it if it does nothing.";
/// Width of the window, in points.
const WINDOW_W: f32 = 380.0;
/// Height of the scrolling key list, in points.
const LIST_H: f32 = 520.0;

/// The PC key a capture takes from `events`: the first non-repeat key press
/// without Ctrl, Alt or Command (those never press calculator keys), with its
/// index in `events`.
pub fn captured_key(events: &[Event]) -> Option<(usize, PcKey)> {
    events.iter().enumerate().find_map(|(i, ev)| match *ev {
        Event::Key {
            key,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } if !(modifiers.ctrl || modifiers.alt || modifiers.command || modifiers.mac_cmd) => {
            Some((
                i,
                PcKey {
                    key,
                    shift: modifiers.shift,
                },
            ))
        }
        _ => None,
    })
}

/// The PC keys bound to `id`, joined by `", "`, or `—` when there are none.
pub fn bindings_text(keymap: &Keymap, id: KeyId) -> String {
    let keys = keymap.keys_for(id);
    if keys.is_empty() {
        "\u{2014}".to_owned()
    } else {
        keys.into_iter()
            .map(pc_key_label)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// State of the keyboard-mapping window.
#[derive(Debug, Default)]
pub struct KeymapWindow {
    open: bool,
    /// The calculator key waiting for its next PC key.
    capturing: Option<KeyId>,
}

impl KeymapWindow {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens or closes the window; closing it cancels a capture.
    pub fn toggle(&mut self) {
        self.open = !self.open;
        if !self.open {
            self.capturing = None;
        }
    }

    /// True while a rebinding waits for a key press. The app treats the
    /// keyboard as typing meanwhile, so the press reaches no calculator key.
    pub fn capturing(&self) -> bool {
        self.capturing.is_some()
    }

    /// Draws the window (when open) and applies its edits to `keymap`.
    pub fn show(&mut self, ctx: &egui::Context, keymap: &mut Keymap) {
        if !self.open {
            return;
        }
        // Take the captured press out of this frame's events, so that a
        // focused button does not also react to it (Enter, Space).
        if let Some(id) = self.capturing {
            let captured = ctx.input_mut(|i| {
                let found = captured_key(&i.events);
                if let Some((index, _)) = found {
                    i.events.remove(index);
                }
                found
            });
            if let Some((_, pc)) = captured {
                keymap.bind(pc, id);
                self.capturing = None;
            }
        }

        let mut open = true;
        Window::new(KEYMAP_TITLE)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .vscroll(false)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .default_width(WINDOW_W)
            .show(ctx, |ui| {
                ui.label(KEYMAP_NOTE);
                ui.separator();
                ScrollArea::vertical()
                    .max_height(LIST_H)
                    .auto_shrink([false, true])
                    .show(ui, |ui| self.grid(ui, keymap));
                ui.separator();
                if ui.button("Reset to defaults").clicked() {
                    *keymap = Keymap::default_map();
                    self.capturing = None;
                }
            });
        if !open {
            self.open = false;
            self.capturing = None;
        }
    }

    /// One row per calculator key: name, PC keys, `Rebind` (or `Cancel`
    /// while that row captures) and `Clear`.
    fn grid(&mut self, ui: &mut egui::Ui, keymap: &mut Keymap) {
        Grid::new("evo-keymap-grid")
            .num_columns(4)
            .striped(true)
            .show(ui, |ui| {
                for id in KeyId::ALL {
                    ui.label(id.name());
                    if self.capturing == Some(id) {
                        ui.label("Press a key\u{2026}");
                        if ui.button("Cancel").clicked() {
                            self.capturing = None;
                        }
                    } else {
                        ui.label(bindings_text(keymap, id));
                        let rebind = ui.button("Rebind");
                        if rebind.clicked() {
                            // Enter or Space on a focused button would click it
                            // again; the capture wants those keys.
                            rebind.surrender_focus();
                            self.capturing = Some(id);
                        }
                    }
                    if ui.button("Clear").clicked() {
                        keymap.unbind_key(id);
                    }
                    ui.end_row();
                }
            });
    }
}
