//! Tools: key scripts, key history, macros / AutoTester and the Hardware panel.
//!
//! [`Tools`] is the feature that owns the Key history, Macros / AutoTester and Hardware
//! windows; it declines the actions whose windows have not landed yet. It keeps
//! `new(&AppOptions)`.

pub mod history;
pub mod macros;
pub mod script;

use crate::app::AppOptions;
use crate::feature::{Feature, FeatureCtx};
use crate::toolbar::Action;
use history::KeyHistory;
use macros::Macros;

/// The tools feature.
pub struct Tools {
    history: KeyHistory,
    macros: Macros,
}

impl Tools {
    pub fn new(_opts: &AppOptions) -> Self {
        Self {
            history: KeyHistory::new(),
            macros: Macros::new(),
        }
    }

    /// The key history (fed every frame, also while its window is closed).
    pub fn history(&self) -> &KeyHistory {
        &self.history
    }

    pub fn history_mut(&mut self) -> &mut KeyHistory {
        &mut self.history
    }

    /// The Macros / AutoTester window, recorder and player.
    pub fn macros(&self) -> &Macros {
        &self.macros
    }

    pub fn macros_mut(&mut self) -> &mut Macros {
        &mut self.macros
    }
}

impl Feature for Tools {
    fn on_action(&mut self, action: Action, _cx: &mut FeatureCtx<'_>) -> bool {
        match action {
            Action::KeyHistory => {
                self.history.toggle();
                true
            }
            Action::Macros => {
                self.macros.toggle();
                true
            }
            _ => false,
        }
    }
    fn frame(&mut self, cx: &mut FeatureCtx<'_>) {
        self.history.push(cx.key_events);
        self.macros.frame(cx);
    }
    fn ui(&mut self, ctx: &egui::Context, cx: &mut FeatureCtx<'_>) {
        self.history.ui(ctx);
        self.macros.ui(ctx, cx);
    }
    fn active(&self, action: Action) -> bool {
        match action {
            Action::KeyHistory => self.history.is_open(),
            Action::Macros => self.macros.is_open(),
            _ => false,
        }
    }
    fn next_deadline(&self) -> Option<u64> {
        self.macros.next_deadline()
    }
    fn blocks_input(&self) -> bool {
        false
    }
    fn on_exit(&mut self, cx: &mut FeatureCtx<'_>) {
        self.macros.on_exit(cx);
    }
}
