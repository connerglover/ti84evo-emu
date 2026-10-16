//! Capture: Copy screen, animated PNG and MP4 recording.

mod sampler;

pub use sampler::{FrameSampler, LCD_FRAME_CYCLES};

use crate::app::AppOptions;
use crate::feature::{Feature, FeatureCtx};
use crate::toast::ToastKind;
use crate::toolbar::Action;

/// Toast after Copy screen succeeds.
pub const COPIED: &str = "Screen copied";
/// Why Copy screen fails when the machine shows no valid frame.
pub const NO_FRAME: &str = "no display frame to capture";

/// What is being recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordKind {
    Apng,
    Mp4,
}

/// The capture feature: Copy screen (recording follows in later tasks).
pub struct Capture {
    _priv: (),
}

impl Capture {
    pub fn new(_opts: &AppOptions) -> Self {
        Self { _priv: () }
    }

    /// The recording in progress, if any.
    pub fn recording(&self) -> Option<RecordKind> {
        None
    }

    /// Puts the current frame on the clipboard and says how it went.
    fn copy_screen(cx: &mut FeatureCtx<'_>) {
        let result = match cx.backend.framebuffer().filter(|f| f.is_valid()) {
            Some(frame) => cx.platform.copy_image(frame),
            None => Err(NO_FRAME.to_owned()),
        };
        match result {
            Ok(()) => cx.toasts.push(COPIED, ToastKind::Info),
            Err(e) => cx
                .toasts
                .push(format!("Copy failed: {e}"), ToastKind::Error),
        }
    }
}

impl Feature for Capture {
    fn on_action(&mut self, action: Action, cx: &mut FeatureCtx<'_>) -> bool {
        match action {
            Action::CopyScreen => {
                Self::copy_screen(cx);
                true
            }
            _ => false,
        }
    }
    fn frame(&mut self, _cx: &mut FeatureCtx<'_>) {}
    fn ui(&mut self, _ctx: &egui::Context, _cx: &mut FeatureCtx<'_>) {}
    fn active(&self, _action: Action) -> bool {
        false
    }
    fn next_deadline(&self) -> Option<u64> {
        None
    }
    fn blocks_input(&self) -> bool {
        false
    }
    fn on_exit(&mut self, _cx: &mut FeatureCtx<'_>) {}
}
