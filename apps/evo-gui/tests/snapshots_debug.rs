//! Golden images of the debug dock tabs, on `FakeBackend`. They need a wgpu adapter, so
//! they run only with `cargo test -p evo-gui --features snapshots`.

mod common;

use common::{gpu_lock, open_action, snap_harness};
use egui::accesskit::Role;
use egui::{Key, Modifiers};
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

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_debug_registers_editing() {
    let _gpu = gpu_lock();
    let mut h = dock_on("Registers");
    // sp's value opens its edit field.
    h.get_by_label("0x20008000").click();
    h.run_steps(3);
    h.hover_at(egui::pos2(300.0, 950.0));
    h.run_steps(10);
    h.snapshot("debug_registers_editing");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_debug_memory_two_views() {
    let _gpu = gpu_lock();
    let mut h = dock_on("Memory");
    h.get_by_label("Add view").click();
    h.run_steps(3);
    // The second view shows words.
    h.get_all_by_label("Words").nth(1).unwrap().click();
    h.run_steps(3);
    h.hover_at(egui::pos2(300.0, 950.0));
    h.run_steps(10);
    h.snapshot("debug_memory_two_views");
}

/// Types `text` into text field `index` (drawing order), replacing what it held, and
/// presses Enter.
fn set_field(h: &mut Harness<'static, EvoApp>, index: usize, text: &str) {
    h.get_all_by_role(Role::TextInput)
        .nth(index)
        .expect("text field")
        .click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(1);
    h.get_all_by_role(Role::TextInput)
        .nth(index)
        .expect("text field")
        .type_text(text);
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(3);
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images: cargo test -p evo-gui --features snapshots"
)]
fn snapshot_debug_visualizer() {
    let _gpu = gpu_lock();
    let mut h = dock_on("Visualizer");
    // FAKE_MEM as a 64×64 grayscale image.
    set_field(&mut h, 0, "0x20000000");
    h.get_by_label("Gray 8-bit").click();
    h.run_steps(3);
    set_field(&mut h, 1, "64");
    set_field(&mut h, 2, "64");
    // Click off the field so no caret shows.
    h.get_by_label("64 × 64, 4096 bytes").click();
    h.run_steps(3);
    h.hover_at(egui::pos2(300.0, 950.0));
    h.run_steps(10);
    h.snapshot("debug_visualizer");
}
