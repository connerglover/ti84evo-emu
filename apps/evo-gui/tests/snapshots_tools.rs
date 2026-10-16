//! Golden images of the tools windows (FakeBackend only). They need a wgpu adapter, so they
//! run only with `cargo test -p evo-gui --features snapshots`.

mod common;

use common::{gpu_lock, open_action, snap_harness};
use evo_gui::backend::fake::FakeBackend;
use evo_gui::backend::{CLOCK_HZ, KeyEvent};
use evo_gui::keypad::KeyId;
use evo_gui::platform::FakePlatform;
use evo_gui::toolbar::Action;

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_tools_key_history() {
    let _gpu = gpu_lock();
    let mut h = snap_harness(FakeBackend::new(), [520.0, 960.0], FakePlatform::shared());
    let ms = CLOCK_HZ / 1000;
    let ev = |key, pressed, at_ms: u64| KeyEvent {
        key,
        pressed,
        cycle: CLOCK_HZ + at_ms * ms,
    };
    h.state_mut().features_mut().tools.history_mut().push(&[
        ev(KeyId::Two, true, 0),
        ev(KeyId::Two, false, 50),
        ev(KeyId::Add, true, 100),
        ev(KeyId::Add, false, 150),
        ev(KeyId::Alpha, true, 200),
        ev(KeyId::Alpha, false, 520),
        ev(KeyId::Enter, true, 600),
        ev(KeyId::Enter, false, 650),
    ]);
    open_action(&mut h, Action::KeyHistory);
    h.run_steps(5);
    h.snapshot("tools_key_history");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_tools_macros() {
    let _gpu = gpu_lock();
    let mut h = snap_harness(FakeBackend::new(), [520.0, 960.0], FakePlatform::shared());
    let macros = h.state_mut().features_mut().tools.macros_mut();
    macros.set_name("sum");
    macros.set_script("2 add 3 enter");
    macros.set_check(true, &"0123456789abcdef".repeat(4));
    open_action(&mut h, Action::Macros);
    h.run_steps(5);
    h.snapshot("tools_macros");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_tools_hardware() {
    let _gpu = gpu_lock();
    let mut h = snap_harness(FakeBackend::new(), [520.0, 960.0], FakePlatform::shared());
    open_action(&mut h, Action::Hardware);
    h.run_steps(5);
    h.snapshot("tools_hardware");
}
