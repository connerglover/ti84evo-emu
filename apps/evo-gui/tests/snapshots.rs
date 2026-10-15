//! Golden images and pixel probes. They need a wgpu adapter, so they run only
//! with `cargo test -p evo-gui --features snapshots`.

mod common;

use common::{gpu_lock, open_action, snap_harness};
use egui::{Color32, Pos2, pos2, vec2};
use egui_kittest::Harness;
use evo_gui::app::EvoApp;
use evo_gui::backend::fake::FakeBackend;
use evo_gui::keypad::geometry::{KEYPAD_ORIGIN, LCD_RECT, key_rect};
use evo_gui::keypad::{KeyId, KeyShape, key_def};
use evo_gui::platform::FakePlatform;
use evo_gui::theme::{APP_BG, BODY_OUTLINE, KEY_ALPHA, KEY_NUMBER, KEY_SECOND, NO_SIGNAL_BG};
use evo_gui::toolbar::Action;

/// The default window, 520×960, paused, on a fake platform.
fn harness(fake: FakeBackend) -> Harness<'static, EvoApp> {
    snap_harness(fake, [520.0, 960.0], FakePlatform::shared())
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

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_calculator_enter_held() {
    use egui::{Event, PointerButton};
    use egui_kittest::kittest::Queryable;
    let _gpu = gpu_lock();
    let mut h = harness(FakeBackend::new());
    let p = h.get_by_label("enter").rect().center();
    let events = &mut h.input_mut().events;
    events.push(Event::PointerMoved(p));
    events.push(Event::PointerButton {
        pos: p,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    });
    h.run_steps(10);
    h.snapshot("calculator_enter_held");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_about_window() {
    let _gpu = gpu_lock();
    let mut h = harness(FakeBackend::new());
    open_action(&mut h, Action::About);
    // Move the pointer off the sidebar so no hover highlight or tooltip shows.
    h.hover_at(egui::pos2(500.0, 950.0));
    h.run_steps(10);
    h.snapshot("about_window");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_debug_panels() {
    let _gpu = gpu_lock();
    let mut h = snap_harness(FakeBackend::new(), [1060.0, 960.0], FakePlatform::shared());
    open_action(&mut h, Action::ToggleDebug);
    // Move the pointer off the sidebar so no hover highlight or tooltip shows.
    h.hover_at(egui::pos2(300.0, 950.0));
    h.run_steps(10);
    h.snapshot("debug_panels");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_keymap_window() {
    let _gpu = gpu_lock();
    let mut h = harness(FakeBackend::new());
    open_action(&mut h, Action::Keymap);
    // Move the pointer off the sidebar so no hover highlight or tooltip shows.
    h.hover_at(egui::pos2(500.0, 950.0));
    h.run_steps(10);
    h.snapshot("keymap_window");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_sidebar_calculator_popover() {
    use egui_kittest::kittest::Queryable;
    let _gpu = gpu_lock();
    let mut h = harness(FakeBackend::new());
    h.get_by_label("Calculator").click();
    h.run_steps(3);
    // Move the pointer off the sidebar so no hover highlight or tooltip shows.
    h.hover_at(egui::pos2(500.0, 950.0));
    h.run_steps(10);
    h.snapshot("sidebar_calculator_popover");
}
