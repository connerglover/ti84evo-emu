//! Help: third-party licenses, the update check and bug reports.
//!
//! Stub written by Lane 0; Lane F replaces it and keeps `new(&AppOptions)`.

use crate::app::AppOptions;
use crate::feature::{Feature, FeatureCtx};
use crate::toolbar::Action;

/// The help feature (stub: declines every action).
pub struct Help {
    _priv: (),
}

impl Help {
    pub fn new(_opts: &AppOptions) -> Self {
        Self { _priv: () }
    }
}

impl Feature for Help {
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
