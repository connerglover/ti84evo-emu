//! Tools: key scripts, key history, macros / AutoTester and the Hardware panel.
//!
//! [`Tools`] is the feature that owns the Key history, Macros / AutoTester and Hardware
//! windows; it declines the actions whose windows have not landed yet. It keeps
//! `new(&AppOptions)`.

pub mod history;
pub mod script;

use crate::app::AppOptions;
use crate::feature::{Feature, FeatureCtx};
use crate::toolbar::Action;
use history::KeyHistory;

/// The tools feature.
pub struct Tools {
    history: KeyHistory,
}

impl Tools {
    pub fn new(_opts: &AppOptions) -> Self {
        Self {
            history: KeyHistory::new(),
        }
    }

    /// The key history (fed every frame, also while its window is closed).
    pub fn history(&self) -> &KeyHistory {
        &self.history
    }

    pub fn history_mut(&mut self) -> &mut KeyHistory {
        &mut self.history
    }
}

impl Feature for Tools {
    fn on_action(&mut self, action: Action, _cx: &mut FeatureCtx<'_>) -> bool {
        match action {
            Action::KeyHistory => {
                self.history.toggle();
                true
            }
            _ => false,
        }
    }
    fn frame(&mut self, cx: &mut FeatureCtx<'_>) {
        self.history.push(cx.key_events);
    }
    fn ui(&mut self, ctx: &egui::Context, _cx: &mut FeatureCtx<'_>) {
        self.history.ui(ctx);
    }
    fn active(&self, action: Action) -> bool {
        match action {
            Action::KeyHistory => self.history.is_open(),
            _ => false,
        }
    }
    fn next_deadline(&self) -> Option<u64> {
        None
    }
    fn blocks_input(&self) -> bool {
        false
    }
    fn on_exit(&mut self, _cx: &mut FeatureCtx<'_>) {}
}
