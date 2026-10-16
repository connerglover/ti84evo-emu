//! Test helpers shared by the integration tests.

#![allow(dead_code)]

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp};
use evo_gui::backend::fake::FakeBackend;
use evo_gui::platform::PlatformRef;
use evo_gui::toolbar::Action;

/// A directory under the system temp dir, unique to this process and call, removed with
/// everything in it when the guard drops (also when the test panics).
pub struct TempDir(PathBuf);

impl TempDir {
    /// `{temp}/{prefix}-{pid}-{n}`, emptied; created when `create` is true.
    pub fn new(prefix: &str, create: bool) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        if create {
            std::fs::create_dir_all(&path).unwrap();
        }
        Self(path)
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs `a` from the sidebar: clicks its group, then its entry in the popover, then lets
/// the click play out (3 steps). Dock buttons have no popover: they click the
/// group only.
pub fn open_action(h: &mut Harness<'static, EvoApp>, a: Action) {
    h.get_by_role_and_label(Role::Button, a.group().tooltip())
        .click();
    h.run_steps(3);
    if !matches!(a, Action::ToggleDebug | Action::UsbTools) {
        h.get_by_role_and_label(Role::Button, a.label()).click();
        h.run_steps(3);
    }
}

/// Serializes the tests that render. Several wgpu test renderers alive on different
/// threads at once crash the process now and then on Windows (`STATUS_ACCESS_VIOLATION`),
/// and a crash during `UPDATE_SNAPSHOTS=1` leaves truncated PNGs behind.
pub fn gpu_lock() -> MutexGuard<'static, ()> {
    static GPU: Mutex<()> = Mutex::new(());
    GPU.lock().unwrap_or_else(|e| e.into_inner())
}

/// [`AppOptions::new`] for `dir`, with `platform` (tests never use the desktop platform).
pub fn test_options(dir: &Path, platform: PlatformRef) -> AppOptions {
    AppOptions {
        platform,
        ..AppOptions::new(dir.to_path_buf())
    }
}

/// A 520×960 harness at 1 pixel per point and 60 frames per second, stepped once.
pub fn harness_with(fake: FakeBackend, opts: AppOptions) -> Harness<'static, EvoApp> {
    let mut h = Harness::builder()
        .with_size([520.0, 960.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .build_eframe(|cc| EvoApp::new(&cc.egui_ctx, Box::new(fake), opts));
    h.run_steps(1);
    h
}

/// A harness for golden images: light theme, 1 pixel per point, stepped 10 times.
pub fn snap_harness_with(
    fake: FakeBackend,
    size: [f32; 2],
    opts: AppOptions,
) -> Harness<'static, EvoApp> {
    let mut h = Harness::builder()
        .with_size(size)
        // kittest sets its theme (dark by default) after the app is built, which
        // overrides the light theme `EvoApp::new` asks for; render as the app runs.
        .with_theme(egui::Theme::Light)
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .build_eframe(|cc| EvoApp::new(&cc.egui_ctx, Box::new(fake), opts));
    h.run_steps(10);
    h
}

/// [`snap_harness_with`] on a shared snapshot data dir, started paused.
pub fn snap_harness(
    fake: FakeBackend,
    size: [f32; 2],
    platform: PlatformRef,
) -> Harness<'static, EvoApp> {
    let dir = std::env::temp_dir().join("evo-gui-tests-snapshots");
    let opts = AppOptions {
        start_paused: true,
        ..test_options(&dir, platform)
    };
    snap_harness_with(fake, size, opts)
}
