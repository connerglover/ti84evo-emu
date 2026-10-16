//! Capture: Copy screen, animated PNG and MP4 recording.
//!
//! Stub written by Lane 0; Lane C replaces it and keeps `new(&AppOptions)` and `recording()`.

use crate::app::AppOptions;
use crate::feature::{Feature, FeatureCtx};
use crate::toolbar::Action;

/// What is being recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordKind {
    Apng,
    Mp4,
}

/// The capture feature (stub: declines every action, never records).
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
}

impl Feature for Capture {
    fn on_action(&mut self, _action: Action, _cx: &mut FeatureCtx<'_>) -> bool {
        false
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
