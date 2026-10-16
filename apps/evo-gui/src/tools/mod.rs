//! Tools: key scripts, key history, macros / AutoTester and the Hardware panel.
//!
//! [`Tools`] is the feature that owns the Key history, Macros / AutoTester and Hardware
//! windows; until those land it declines every action. It keeps `new(&AppOptions)`.

pub mod script;

use crate::app::AppOptions;
use crate::feature::{Feature, FeatureCtx};
use crate::toolbar::Action;

/// The tools feature (stub: declines every action).
pub struct Tools {
    _priv: (),
}

impl Tools {
    pub fn new(_opts: &AppOptions) -> Self {
        Self { _priv: () }
    }
}

impl Feature for Tools {
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
