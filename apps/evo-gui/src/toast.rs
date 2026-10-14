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

/// A queue of toasts.
#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
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

    /// The texts of the live toasts, oldest first.
    pub fn texts(&self) -> Vec<&str> {
        self.items.iter().map(|t| t.text.as_str()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Drops expired toasts and draws the rest; each is an `egui::Label`.
    pub fn show(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        for t in &mut self.items {
            t.shown_at.get_or_insert(now);
        }
        self.items
            .retain(|t| now - t.shown_at.unwrap_or(now) < TOAST_SECS);
        let Some(first_expiry) = self
            .items
            .iter()
            .map(|t| t.shown_at.unwrap_or(now) + TOAST_SECS)
            .reduce(f64::min)
        else {
            return;
        };
        egui::Area::new(Id::new("evo-toasts"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -BOTTOM_GAP))
            .interactable(false)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    for t in &self.items {
                        let fill: Color32 = match t.kind {
                            ToastKind::Info => TOAST_BG,
                            ToastKind::Error => TOAST_ERROR_BG,
                        };
                        Frame::NONE
                            .fill(fill)
                            .corner_radius(6)
                            .inner_margin(Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(RichText::new(&t.text).color(LABEL_LIGHT))
                                        .wrap(),
                                );
                            });
                    }
                });
            });
        // Wake up to remove the toast even while nothing else repaints.
        let wait = (first_expiry - now).max(0.0);
        ctx.request_repaint_after(Duration::from_secs_f64(wait));
    }
}
