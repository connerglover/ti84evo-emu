//! The feature seam: each tool (ROM setup, capture, tools, files, help) is a [`Feature`] the
//! app drives through [`Feature::on_action`], [`Feature::frame`] and [`Feature::ui`].
//!
//! Features never change the app directly. They change the backend through
//! [`FeatureCtx::backend`] and ask the app for everything else with an [`AppRequest`].
//!
//! **Request rule.** Each pass that hands features a [`FeatureCtx`] (an action, the `logic`
//! pass, the `ui` pass, exit) collects the requests into one list. The app applies that list,
//! in push order, right after the pass ends and before the next pass starts. A request pushed
//! from `on_action` or `ui` therefore takes effect in the same frame. Requests are never kept
//! for a later frame.

use std::path::{Path, PathBuf};

use crate::backend::{EmuBackend, KeyEvent};
use crate::platform::PlatformRef;
use crate::prefs::Prefs;
use crate::toast::Toasts;
use crate::toolbar::Action;

/// What a feature may use during one pass.
pub struct FeatureCtx<'a> {
    /// The machine. While provisioning runs this is a stand-in with nothing behind it.
    pub backend: &'a mut dyn EmuBackend,
    pub toasts: &'a mut Toasts,
    pub platform: &'a PlatformRef,
    /// Where this instance's flash, slots, screenshots and settings live.
    pub data_dir: &'a Path,
    /// The preferences as the app would save them now (a snapshot taken once per pass).
    pub prefs: &'a Prefs,
    /// The OS bundle the running backend was opened from; `None` without a ROM or for the demo.
    pub rom: Option<&'a Path>,
    /// The instance id; `None` for the default instance.
    pub instance: Option<&'a str>,
    pub paused: bool,
    pub provisioning: bool,
    /// The key changes that reached the machine this frame (`logic` pass only; empty otherwise).
    pub key_events: &'a [KeyEvent],
    /// Requests to the app, applied after this pass in push order.
    pub requests: &'a mut Vec<AppRequest>,
}

/// A change a feature asks the app to make.
pub enum AppRequest {
    /// Lends the running calculator to an exclusive USB worker.
    Usb(crate::connect::types::UsbTask),
    /// Releases every key and forgets the last stop status, as after a state load.
    Resync,
    /// Writes flash to disk.
    Persist,
    /// Resets the machine (flash is kept).
    Reset,
    /// Starts provisioning the backend on a worker thread.
    Provision,
    /// Swaps in `backend` (opened from `rom`), first persisting the old one when `persist_old`.
    /// The new backend is provisioned when it needs it.
    ReplaceBackend {
        backend: Box<dyn EmuBackend>,
        rom: Option<PathBuf>,
        persist_old: bool,
    },
    /// Applies keymap, filter, debug-open, compact, speed and bundle.
    ApplyPrefs(Prefs),
    /// Pauses (which cancels any debugger goal) or resumes.
    SetPaused(bool),
    /// As Settings › Reset layout.
    ResetLayout,
    /// Closes the window.
    Close,
}

impl std::fmt::Debug for AppRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppRequest::Usb(task) => f.debug_tuple("Usb").field(&task.operation.label()).finish(),
            AppRequest::Resync => f.write_str("Resync"),
            AppRequest::Persist => f.write_str("Persist"),
            AppRequest::Reset => f.write_str("Reset"),
            AppRequest::Provision => f.write_str("Provision"),
            AppRequest::ReplaceBackend {
                backend,
                rom,
                persist_old,
            } => f
                .debug_struct("ReplaceBackend")
                .field("backend", &backend.name())
                .field("rom", rom)
                .field("persist_old", persist_old)
                .finish(),
            AppRequest::ApplyPrefs(p) => f.debug_tuple("ApplyPrefs").field(p).finish(),
            AppRequest::SetPaused(b) => f.debug_tuple("SetPaused").field(b).finish(),
            AppRequest::ResetLayout => f.write_str("ResetLayout"),
            AppRequest::Close => f.write_str("Close"),
        }
    }
}

/// One tool of the app. There are no default methods: every feature states each one.
pub trait Feature {
    /// Runs `action` if this feature owns it; `false` means "not mine".
    fn on_action(&mut self, action: Action, cx: &mut FeatureCtx<'_>) -> bool;
    /// Called every frame in `logic`, after emulation.
    fn frame(&mut self, cx: &mut FeatureCtx<'_>);
    /// Draws this feature's windows and modals, after the app's overlays.
    fn ui(&mut self, ctx: &egui::Context, cx: &mut FeatureCtx<'_>);
    /// Whether `action` is drawn selected in its popover.
    fn active(&self, action: Action) -> bool;
    /// The absolute cycle count the next run must stop at, if any.
    fn next_deadline(&self) -> Option<u64>;
    /// True while calculator keys must be ignored (a modal is open).
    fn blocks_input(&self) -> bool;
    /// The window is closing. Preferences are already saved: this must not change them.
    fn on_exit(&mut self, cx: &mut FeatureCtx<'_>);
}
