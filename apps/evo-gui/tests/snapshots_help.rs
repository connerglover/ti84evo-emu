//! Golden images of the Help windows (Lane F, Task F2). They need a wgpu adapter, so they run
//! only with `cargo test -p evo-gui --features snapshots`.

mod common;

use std::time::Duration;

use common::{gpu_lock, open_action, snap_harness};
use egui_kittest::kittest::Queryable;
use evo_gui::backend::fake::FakeBackend;
use evo_gui::platform::FakePlatform;
use evo_gui::toolbar::Action;

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_help_licenses() {
    let _gpu = gpu_lock();
    let mut h = snap_harness(FakeBackend::new(), [520.0, 960.0], FakePlatform::shared());
    open_action(&mut h, Action::Licenses);
    // Move the pointer off the sidebar so no hover highlight or tooltip shows.
    h.hover_at(egui::pos2(500.0, 950.0));
    h.run_steps(10);
    h.snapshot("help_licenses");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_help_update_available() {
    let _gpu = gpu_lock();
    let platform = FakePlatform::shared();
    platform.set_http(Ok(
        r#"{"tag_name":"v99.0.0","html_url":"https://example.invalid/r"}"#.to_owned(),
    ));
    let mut h = snap_harness(FakeBackend::new(), [520.0, 960.0], platform.clone());
    open_action(&mut h, Action::CheckUpdates);
    let mut done = false;
    for _ in 0..200 {
        if h.query_by_label("Version v99.0.0 is available").is_some() {
            done = true;
            break;
        }
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(done, "update check did not finish");
    h.hover_at(egui::pos2(500.0, 950.0));
    h.run_steps(10);
    h.snapshot("help_update_available");
}
