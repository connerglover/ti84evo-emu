//! Golden images of the ROM Setup Wizard, on `FakeBackend` (no TI frame or artwork). They need
//! a wgpu adapter, so they run only with `cargo test -p evo-gui --features snapshots`.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{TempDir, gpu_lock, open_action, snap_harness_with, test_options};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp};
use evo_gui::backend::EmuBackend;
use evo_gui::backend::fake::FakeBackend;
use evo_gui::platform::{FakePlatform, PlatformRef};
use evo_gui::toast::TOAST_SECS;
use evo_gui::toolbar::Action;
use evo_image::testutil::{Img, package, zip_bytes};

const SIZE: [f32; 2] = [520.0, 960.0];

fn options(dir: &Path, platform: &Arc<FakePlatform>) -> AppOptions {
    let platform: PlatformRef = platform.clone();
    AppOptions {
        start_paused: true,
        open_wizard: true,
        ..test_options(&dir.join("data"), platform)
    }
}

fn click(h: &mut Harness<'static, EvoApp>, label: &str) {
    h.get_by_label(label).click();
    h.run_steps(3);
}

/// Steps until no toast is left (they last [`TOAST_SECS`] of `ctx` time).
fn outlive_toasts(h: &mut Harness<'static, EvoApp>) {
    h.run_steps((TOAST_SECS * 60.0) as usize + 10);
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_wizard_welcome() {
    let _gpu = gpu_lock();
    let dir = TempDir::new("evo-gui-snap-wizard-welcome", true);
    let platform = FakePlatform::shared();
    let mut h = snap_harness_with(FakeBackend::new(), SIZE, options(&dir, &platform));
    h.snapshot("wizard_welcome");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_wizard_choose_error() {
    let _gpu = gpu_lock();
    let dir = TempDir::new("evo-gui-snap-wizard-choose", true);
    let garbage = dir.join("garbage.84b2");
    std::fs::write(&garbage, b"this is not an OS bundle").unwrap();
    let platform = FakePlatform::shared();
    let mut h = snap_harness_with(FakeBackend::new(), SIZE, options(&dir, &platform));
    click(&mut h, "Next");
    h.input_mut().dropped_files = vec![dropped(&garbage)];
    h.run_steps(10);
    assert!(h.query_by_label_contains("Not an OS bundle: ").is_some());
    h.snapshot("wizard_choose_error");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_wizard_prepare_failed() {
    let _gpu = gpu_lock();
    let dir = TempDir::new("evo-gui-snap-wizard-prepare", true);
    let path = dir.join("os.84b2");
    std::fs::write(&path, bundle()).unwrap();
    let platform = FakePlatform::shared();
    platform.set_backend_factory(Box::new(|_, _| {
        Ok(Box::new(
            FakeBackend::new()
                .named("os.84b2")
                .needing_provision()
                .failing_provision(),
        ) as Box<dyn EmuBackend>)
    }));
    let mut h = snap_harness_with(FakeBackend::new(), SIZE, options(&dir, &platform));
    click(&mut h, "Next");
    platform.queue_open(Some(path));
    click(&mut h, "Browse\u{2026}");
    click(&mut h, "Next");
    let start = Instant::now();
    while h.query_by_label("Preparing failed.").is_none() {
        assert!(start.elapsed() < Duration::from_secs(10), "never failed");
        std::thread::sleep(Duration::from_millis(5));
        h.run_steps(1);
    }
    outlive_toasts(&mut h);
    h.snapshot("wizard_prepare_failed");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_wizard_instances() {
    let _gpu = gpu_lock();
    let dir = TempDir::new("evo-gui-snap-wizard-instances", true);
    let platform: PlatformRef = FakePlatform::shared();
    let opts = AppOptions {
        start_paused: true,
        instance: Some("lab".to_owned()),
        ..test_options(&dir.join("data/instances/lab"), platform)
    };
    let mut h = snap_harness_with(FakeBackend::new(), SIZE, opts);
    open_action(&mut h, Action::Instances);
    h.run_steps(5);
    assert!(h.query_by_label("Instance: lab").is_some());
    h.snapshot("wizard_instances");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_wizard_reset_everything() {
    let _gpu = gpu_lock();
    let dir = TempDir::new("evo-gui-snap-wizard-reset", true);
    let platform: PlatformRef = FakePlatform::shared();
    let opts = AppOptions {
        start_paused: true,
        ..test_options(&dir.join("data"), platform)
    };
    let mut h = snap_harness_with(FakeBackend::new(), SIZE, opts);
    open_action(&mut h, Action::ResetEverything);
    h.run_steps(5);
    assert!(h.query_by_label("Reset everything?").is_some());
    h.snapshot("wizard_reset_everything");
}

/// A synthetic TI-84 Evo bundle (made-up bytes, genuine framing).
fn bundle() -> Vec<u8> {
    let meta = "bundle_build_version:7.1.0.4421\nbundle_target_device:84US\n\
                bundle_target_prodid:23\nbundle_target_type:RESTORE\n";
    let body: Vec<u8> = (0..0x200u32).map(|i| (i as u8) | 1).collect();
    let pkg = package(&[(0, &Img::new(0x400, body))]);
    zip_bytes(
        &[
            ("METADATA", meta.as_bytes()),
            ("Synthetic_Package.84pk2", &pkg),
            ("Archive/", b""),
        ],
        Default::default(),
    )
}

#[derive(Debug)]
struct Dropped(std::path::PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

fn dropped(path: &Path) -> egui::DroppedFileHandle {
    Arc::new(Dropped(path.to_path_buf()))
}
