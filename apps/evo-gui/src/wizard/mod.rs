//! The ROM Setup Wizard, Reload ROM, Change ROM, Instances and Reset everything.
//!
//! Stub written by Lane 0; Lane B replaces it and keeps `new(&AppOptions)` and `page()`.

use crate::app::AppOptions;
use crate::feature::{Feature, FeatureCtx};
use crate::toolbar::Action;

/// A page of the ROM Setup Wizard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Welcome,
    Choose,
    Prepare,
    Done,
}

/// The ROM setup feature (stub: declines every action, never opens).
pub struct RomSetup {
    _priv: (),
}

impl RomSetup {
    pub fn new(_opts: &AppOptions) -> Self {
        Self { _priv: () }
    }

    /// The open wizard page; `None` when the wizard is closed.
    pub fn page(&self) -> Option<Page> {
        None
    }
}

impl Feature for RomSetup {
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
