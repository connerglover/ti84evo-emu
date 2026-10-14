//! Golden images and pixel probes. They need a wgpu adapter, so they run only
//! with `cargo test -p evo-gui --features snapshots`.

use std::sync::{Mutex, MutexGuard};

use egui::{Color32, Pos2, pos2, vec2};
use egui_kittest::Harness;
use evo_gui::app::{AppOptions, EvoApp};
use evo_gui::backend::fake::FakeBackend;
use evo_gui::keypad::geometry::{KEYPAD_ORIGIN, LCD_RECT, key_rect};
use evo_gui::keypad::{KeyId, KeyShape, key_def};
use evo_gui::theme::{APP_BG, BODY_OUTLINE, KEY_ALPHA, KEY_NUMBER, KEY_SECOND, NO_SIGNAL_BG};

/// Serializes the tests. Several wgpu test renderers alive on different
/// threads at once crash the process now and then on Windows
/// (`STATUS_ACCESS_VIOLATION`), and a crash during `UPDATE_SNAPSHOTS=1`
/// leaves truncated PNGs behind.
static GPU: Mutex<()> = Mutex::new(());

fn gpu_lock() -> MutexGuard<'static, ()> {
    GPU.lock().unwrap_or_else(|e| e.into_inner())
}

fn harness(fake: FakeBackend) -> Harness<'static, EvoApp> {
    let opts = AppOptions {
        start_paused: true,
        ..AppOptions::new(std::env::temp_dir().join("evo-gui-tests-snapshots"))
    };
    let mut h = Harness::builder()
        .with_size([520.0, 960.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .build_eframe(|cc| EvoApp::new(&cc.egui_ctx, Box::new(fake), opts));
    h.run_steps(10);
    h
}

fn body(x: f32, y: f32) -> Pos2 {
    pos2(x, y)
}

/// Body du of the point 4 du right of a rect key's left edge, at its vertical center.
fn key_probe(id: KeyId) -> Pos2 {
    let KeyShape::Rect(r) = key_def(id).shape else {
        panic!("{id:?} is not a rect key");
    };
    KEYPAD_ORIGIN + vec2(r.min.x + 4.0, r.center().y)
}

fn assert_color(img: &image::RgbaImage, screen: Pos2, want: Color32, what: &str) {
    let (x, y) = (screen.x.floor() as u32, screen.y.floor() as u32);
    let p = img.get_pixel(x, y).0;
    let close = |a: u8, b: u8| a.abs_diff(b) <= 2;
    assert!(
        close(p[0], want.r()) && close(p[1], want.g()) && close(p[2], want.b()),
        "{what} at ({x}, {y}): got {p:?}, want {want:?}"
    );
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_calculator_idle() {
    let _gpu = gpu_lock();
    let mut h = harness(FakeBackend::new());
    h.snapshot("calculator_idle");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_no_display() {
    let _gpu = gpu_lock();
    let mut h = harness(FakeBackend::without_display());
    h.snapshot("no_display");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_theme_colors() {
    let _gpu = gpu_lock();
    let mut h = harness(FakeBackend::new());
    let img = h.render().expect("render");
    let t = h.state().calc_transform();
    assert_color(&img, t.to_screen(body(-8.0, 440.0)), APP_BG, "APP_BG");
    assert_color(
        &img,
        t.to_screen(body(1.0, 440.0)),
        BODY_OUTLINE,
        "BODY_OUTLINE",
    );
    assert_color(
        &img,
        t.to_screen(key_probe(KeyId::Second)),
        KEY_SECOND,
        "KEY_SECOND",
    );
    assert_color(
        &img,
        t.to_screen(key_probe(KeyId::Alpha)),
        KEY_ALPHA,
        "KEY_ALPHA",
    );
    assert_color(
        &img,
        t.to_screen(key_probe(KeyId::Seven)),
        KEY_NUMBER,
        "KEY_NUMBER",
    );
    // The layout table and the geometry helper agree on where `7` is.
    assert_eq!(key_def(KeyId::Seven).shape, KeyShape::Rect(key_rect(6, 1)));

    let mut h = harness(FakeBackend::without_display());
    let img = h.render().expect("render");
    let t = h.state().calc_transform();
    assert_color(
        &img,
        t.to_screen(LCD_RECT.min + vec2(4.0, 4.0)),
        NO_SIGNAL_BG,
        "NO_SIGNAL_BG",
    );
}
