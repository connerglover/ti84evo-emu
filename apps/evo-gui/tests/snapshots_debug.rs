//! Golden images of the debug dock tabs, on `FakeBackend`. They need a wgpu adapter, so
//! they run only with `cargo test -p evo-gui --features snapshots`.

mod common;

use common::{gpu_lock, open_action, snap_harness};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::EvoApp;
use evo_gui::backend::fake::FakeBackend;
use evo_gui::platform::FakePlatform;
use evo_gui::toolbar::Action;

/// A paused 1060×960 golden harness with the dock open on tab `tab`, the pointer moved
/// off the sidebar so no hover highlight or tooltip shows.
fn dock_on(tab: &str) -> Harness<'static, EvoApp> {
    let mut h = snap_harness(FakeBackend::new(), [1060.0, 960.0], FakePlatform::shared());
    open_action(&mut h, Action::ToggleDebug);
    h.get_by_label(tab).click();
    h.run_steps(3);
    h.hover_at(egui::pos2(300.0, 950.0));
    h.run_steps(10);
    h
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_debug_control() {
    let _gpu = gpu_lock();
    let mut h = dock_on("Control");
    h.snapshot("debug_control");
}
