//! Short messages shown bottom-center for a few seconds.

use std::time::Duration;

use egui::{Align2, Color32, Frame, Id, Margin, Order, RichText, vec2};

use crate::theme::{LABEL_LIGHT, TOAST_BG, TOAST_ERROR_BG};

/// How long a toast stays, in seconds of `ctx` time.
pub const TOAST_SECS: f64 = 4.0;
/// Gap between the toasts and the bottom of the window, in points.
const BOTTOM_GAP: f32 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Error,
}

struct Toast {
    text: String,
    kind: ToastKind,
    /// `ctx` time of the first frame that showed it.
    shown_at: Option<f64>,
}

/// A queue of toasts, plus at most one progress message that stays until it is cleared.
#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
    progress: Option<String>,
}

impl Toasts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues a toast; its 4 s start on the next frame that shows it.
    pub fn push(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.items.push(Toast {
            text: text.into(),
            kind,
            shown_at: None,
        });
    }

    /// Shows `text` above the toasts until the next call; `None` removes it.
    pub fn set_progress(&mut self, text: Option<String>) {
        self.progress = text;
    }

    /// The progress message, then the texts of the live toasts, oldest first.
    pub fn texts(&self) -> Vec<&str> {
        self.progress
            .iter()
            .map(String::as_str)
            .chain(self.items.iter().map(|t| t.text.as_str()))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.progress.is_none()
    }

    /// Drops expired toasts and draws the rest; each is an `egui::Label`.
    pub fn show(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        for t in &mut self.items {
            t.shown_at.get_or_insert(now);
        }
        self.items
            .retain(|t| now - t.shown_at.unwrap_or(now) < TOAST_SECS);
        let first_expiry = self
            .items
            .iter()
            .map(|t| t.shown_at.unwrap_or(now) + TOAST_SECS)
            .reduce(f64::min);
        if first_expiry.is_none() && self.progress.is_none() {
            return;
        }
        let progress = self.progress.iter().map(|text| (text, ToastKind::Info));
        let toasts = self.items.iter().map(|t| (&t.text, t.kind));
        egui::Area::new(Id::new("evo-toasts"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -BOTTOM_GAP))
            .interactable(false)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    for (text, kind) in progress.chain(toasts) {
                        let fill: Color32 = match kind {
                            ToastKind::Info => TOAST_BG,
                            ToastKind::Error => TOAST_ERROR_BG,
                        };
                        Frame::NONE
                            .fill(fill)
                            .corner_radius(6)
                            .inner_margin(Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(RichText::new(text).color(LABEL_LIGHT)).wrap(),
                                );
                            });
                    }
                });
            });
        // Wake up to remove the toast even while nothing else repaints.
        if let Some(expiry) = first_expiry {
            let wait = (expiry - now).max(0.0);
            ctx.request_repaint_after(Duration::from_secs_f64(wait));
        }
    }
}
