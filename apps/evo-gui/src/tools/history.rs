//! Key history: the last [`KEY_HISTORY_CAP`] key changes that reached the machine, shown in
//! the "Key history" window and copyable as a key script.
//!
//! The history is fed from [`crate::feature::FeatureCtx::key_events`] every frame, also while
//! the window is closed, so opening it shows what was pressed before.

use std::collections::VecDeque;

use egui::{Align2, RichText, ScrollArea, Window};

use crate::backend::{CLOCK_HZ, KeyEvent};
use crate::tools::script::{ScriptEvent, ms_between, to_script};

/// The most key changes the history keeps; older ones are dropped first.
pub const KEY_HISTORY_CAP: usize = 1000;

/// The window title.
pub const KEY_HISTORY_TITLE: &str = "Key history";

const WINDOW_W: f32 = 300.0;
const LIST_H: f32 = 260.0;

/// One history line: seconds of emulated time, the key's core name and `down` / `up`.
pub fn history_line(e: &KeyEvent) -> String {
    format!(
        "{:>10.3} s  {:<8} {}",
        e.cycle as f64 / CLOCK_HZ as f64,
        e.key.core_name(),
        if e.pressed { "down" } else { "up" }
    )
}

/// The key history and its window.
#[derive(Debug, Default)]
pub struct KeyHistory {
    events: VecDeque<KeyEvent>,
    open: bool,
}

impl KeyHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `events` (oldest first), dropping the oldest beyond [`KEY_HISTORY_CAP`].
    pub fn push(&mut self, events: &[KeyEvent]) {
        let skip = events.len().saturating_sub(KEY_HISTORY_CAP);
        self.events.extend(&events[skip..]);
        let excess = self.events.len().saturating_sub(KEY_HISTORY_CAP);
        self.events.drain(..excess);
    }

    /// The kept events, oldest first.
    pub fn events(&self) -> impl Iterator<Item = &KeyEvent> {
        self.events.iter()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Every kept event as a key script, timed from the first one.
    pub fn script(&self) -> String {
        let Some(first) = self.events.front() else {
            return String::new();
        };
        let events: Vec<ScriptEvent> = self
            .events
            .iter()
            .map(|e| ScriptEvent {
                at_ms: ms_between(first.cycle, e.cycle),
                key: e.key,
                pressed: e.pressed,
            })
            .collect();
        to_script(&events)
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the window when closed, closes it when open.
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    /// Draws the window while open.
    pub fn ui(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let mut open = true;
        Window::new(KEY_HISTORY_TITLE)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .vscroll(false)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .default_width(WINDOW_W)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Clear").clicked() {
                        self.clear();
                    }
                    let copy =
                        ui.add_enabled(!self.is_empty(), egui::Button::new("Copy as key script"));
                    if copy.clicked() {
                        ctx.copy_text(self.script());
                    }
                });
                ui.separator();
                let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
                ScrollArea::vertical()
                    .max_height(LIST_H)
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show_rows(ui, row_h, self.events.len(), |ui, rows| {
                        for e in self.events.range(rows) {
                            ui.label(RichText::new(history_line(e)).monospace());
                        }
                    });
            });
        if !open {
            self.open = false;
        }
    }
}
