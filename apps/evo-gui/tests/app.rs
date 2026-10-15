mod common;

use common::TempDir;
use egui::{Event, Key, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp, debug_max_width, status_text};
use evo_gui::backend::fake::{FAKE_PC, FakeBackend};
use evo_gui::backend::{EmuBackend, Frame, RunStatus};
use evo_gui::keypad::KeyId;
use evo_gui::keypad::geometry::{DPAD_CENTER, KEYPAD_ORIGIN};
use evo_gui::lcd::{ScaleFilter, lcd_draw_rect};
use evo_gui::store::Store;
use evo_gui::toolbar::Tool;

fn data_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("evo-gui-tests-app")
}

fn harness(fake: FakeBackend, paused: bool) -> Harness<'static, EvoApp> {
    harness_in(fake, paused, data_dir())
}

#[test]
fn status_text_strings() {
    use RunStatus::*;
    assert_eq!(status_text(Running, true, None), "Paused");
    assert_eq!(status_text(Running, true, Some(87)), "Paused");
    assert_eq!(status_text(Idle, true, None), "Paused");
    assert_eq!(status_text(Running, false, None), "Running");
    assert_eq!(status_text(Running, false, Some(87)), "Running \u{b7} 87%");
    assert_eq!(status_text(Idle, false, None), "Idle (CPU asleep)");
    assert_eq!(
        status_text(Breakpoint(0x1000_0200), false, None),
        "Breakpoint at 0x10000200"
    );
    assert_eq!(
        status_text(Lockup { pc: 0xEFFF_FFFE }, false, None),
        "Lockup at 0xeffffffe"
    );
    assert_eq!(
        status_text(
            Bkpt {
                pc: 0x0000_0123,
                imm: 0xAB
            },
            false,
            None
        ),
        "BKPT #171 at 0x00000123"
    );
    // A stop status is shown even while paused (the stop is why it paused).
    assert_eq!(
        status_text(Breakpoint(0x1000_0200), true, Some(87)),
        "Breakpoint at 0x10000200"
    );
}

#[test]
fn header_shows_title_and_backend() {
    let mut h = harness(FakeBackend::new(), true);
    h.run_steps(2);
    h.get_by_label("TI-84 Evo Emulator");
    h.get_by_label("demo");
    h.get_by_label("Paused");
}

#[test]
fn no_display_shows_placeholder() {
    let mut h = harness(FakeBackend::without_display(), true);
    h.run_steps(2);
    h.get_by_label("No display signal");
}

#[test]
fn invalid_frame_shows_placeholder() {
    let fake = FakeBackend::with_frame(Frame {
        width: 320,
        height: 240,
        rgba: vec![0; 10],
    });
    let mut h = harness(fake, true);
    h.run_steps(2);
    h.get_by_label("No display signal");
}

#[test]
fn valid_frame_shows_no_placeholder() {
    let mut h = harness(FakeBackend::new(), true);
    h.run_steps(2);
    assert!(h.query_by_label("No display signal").is_none());
}

#[test]
fn app_accessors() {
    let mut h = harness(FakeBackend::new(), true);
    h.run_steps(1);
    let app = h.state_mut();
    assert!(app.paused());
    assert!(!app.debug_open());
    assert_eq!(app.status(), RunStatus::Running);
    assert_eq!(app.backend().name(), "demo");
    assert!(!app.key_down(evo_gui::keypad::KeyId::Enter));
    app.set_paused(false);
    assert!(!app.paused());
    let t = app.calc_transform();
    assert!(t.scale > 0.9 && t.scale < 1.2, "scale {}", t.scale);
    assert!(!AppOptions::new(data_dir()).start_paused);
}

fn approx(a: Rect, b: Rect) -> bool {
    (a.min - b.min).length() < 1e-3 && (a.max - b.max).length() < 1e-3
}

#[test]
fn lcd_draw_rect_snaps() {
    let lcd = Rect::from_min_size(pos2(0.0, 0.0), vec2(332.8, 249.6));
    assert_eq!(lcd_draw_rect(lcd, 1.0, ScaleFilter::Smooth), lcd);

    let r1 = lcd_draw_rect(lcd, 1.0, ScaleFilter::Nearest);
    assert!(approx(
        r1,
        Rect::from_center_size(lcd.center(), vec2(320.0, 240.0))
    ));

    let r2 = lcd_draw_rect(lcd, 2.0, ScaleFilter::Nearest);
    assert!(approx(
        r2,
        Rect::from_center_size(lcd.center(), vec2(320.0, 240.0))
    ));

    let small = Rect::from_min_size(pos2(0.0, 0.0), vec2(320.0, 240.0));
    assert_eq!(lcd_draw_rect(small, 1.5, ScaleFilter::Nearest), small);
}

fn pointer_button(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    }
}

#[test]
fn clicking_a_key_taps_it() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = harness(fake, false);
    h.run_steps(1);
    h.get_by_label("enter").click();
    // Move, press and release run as three frames.
    h.run_steps(1);
    assert_eq!(probe.key_log(), [(KeyId::Enter, true)]);
    h.run_steps(4);
    assert_eq!(
        probe.key_log(),
        [(KeyId::Enter, true), (KeyId::Enter, false)]
    );
}

#[test]
fn same_frame_tap_is_held() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = harness(fake, false);
    h.run_steps(1);
    let p = h.get_by_label("enter").rect().center();
    let events = &mut h.input_mut().events;
    events.push(Event::PointerMoved(p));
    events.push(pointer_button(p, true));
    events.push(pointer_button(p, false));
    h.step();
    assert_eq!(probe.key_log(), [(KeyId::Enter, true)]);
    h.run_steps(4);
    assert_eq!(
        probe.key_log(),
        [(KeyId::Enter, true), (KeyId::Enter, false)]
    );
}

/// Presses the primary button at `p` in one frame (move and press).
fn press_at(h: &mut Harness<'static, EvoApp>, p: Pos2) {
    let events = &mut h.input_mut().events;
    events.push(Event::PointerMoved(p));
    events.push(pointer_button(p, true));
    h.step();
}

#[test]
fn dpad_overlap_presses_the_right_wedge() {
    for (r, deg) in [(25.0f32, 220.0f32), (29.0, 145.0)] {
        let fake = FakeBackend::new();
        let probe = fake.probe();
        let mut h = harness(fake, true);
        h.run_steps(1);
        let a = deg.to_radians();
        let t = h.state().calc_transform();
        let p = t.to_screen(KEYPAD_ORIGIN + DPAD_CENTER.to_vec2() + vec2(a.cos(), a.sin()) * r);
        press_at(&mut h, p);
        assert_eq!(probe.key_log(), [(KeyId::Left, true)], "r {r}, {deg} deg");
    }
}

#[test]
fn dpad_center_gap_presses_nothing() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = harness(fake, false);
    h.run_steps(1);
    let t = h.state().calc_transform();
    let p = t.to_screen(KEYPAD_ORIGIN + DPAD_CENTER.to_vec2());
    press_at(&mut h, p);
    h.input_mut().events.push(pointer_button(p, false));
    h.run_steps(2);
    assert!(probe.key_log().is_empty(), "{:?}", probe.key_log());
}

#[test]
fn dpad_release_after_drag_releases_the_pressed_wedge() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = harness(fake, false);
    h.run_steps(1);
    let t = h.state().calc_transform();
    let c = KEYPAD_ORIGIN + DPAD_CENTER.to_vec2();
    let left = t.to_screen(c + vec2(-30.0, 0.0));
    let right = t.to_screen(c + vec2(30.0, 0.0));
    press_at(&mut h, left);
    h.input_mut().events.push(Event::PointerMoved(right));
    h.step();
    h.input_mut().events.push(pointer_button(right, false));
    h.run_steps(5);
    assert_eq!(probe.key_log(), [(KeyId::Left, true), (KeyId::Left, false)]);
}

#[test]
fn key_tooltip_lists_bindings() {
    let mut h = harness(FakeBackend::new(), true);
    h.run_steps(1);
    let p = h.get_by_label("enter").rect().center();
    h.hover_at(p);
    // Tooltips appear after a short delay.
    h.run_steps(60);
    h.get_by_label("enter \u{2014} Enter");
}

#[test]
fn keypad_keys_take_no_focus() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = harness(fake, true);
    h.run_steps(1);
    h.key_press(Key::Tab);
    h.key_press(Key::Space);
    h.run_steps(2);
    assert!(probe.key_log().is_empty(), "{:?}", probe.key_log());
}

#[test]
fn pc_key_drives_calculator_key() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = harness(fake, true);
    h.run_steps(1);
    h.key_down(Key::Enter);
    h.run_steps(1);
    assert_eq!(probe.held(), [KeyId::Enter]);
    assert!(h.state().key_down(KeyId::Enter));
    h.key_up(Key::Enter);
    h.state_mut().set_paused(false);
    h.run_steps(4);
    assert!(probe.held().is_empty());
    assert!(!h.state().key_down(KeyId::Enter));
}

#[test]
fn breakpoint_pauses_and_opens_debug() {
    let mut fake = FakeBackend::new();
    fake.toggle_breakpoint(FAKE_PC);
    let mut h = harness(fake, false);
    h.run_steps(2);
    let app = h.state();
    assert!(app.paused());
    assert!(app.debug_open());
    assert_eq!(app.status(), RunStatus::Breakpoint(FAKE_PC));
    h.get_by_label("Breakpoint at 0x10000200");
}

#[test]
fn idle_backend_shows_idle() {
    let mut h = harness(FakeBackend::idle(), false);
    h.run_steps(2);
    h.get_by_label("Idle (CPU asleep)");
    assert!(!h.state().paused());
}

#[test]
fn running_advances_cycles_and_reports_speed() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = harness(fake, false);
    // Building the harness already runs a few frames.
    let before = probe.cycles();
    h.run_steps(3);
    assert_eq!(probe.cycles() - before, 3 * 2_600_000);
    h.run_steps(60);
    h.get_by_label("Running \u{b7} 100%");
}

// ---------------------------------------------------------------------------
// Toolbar, state slots, screenshots, toasts and persistence hooks (Task 7).

/// A fresh, empty data directory for one test.
fn fresh_dir(test: &str) -> TempDir {
    TempDir::new(&format!("evo-gui-app-{test}"), false)
}

fn harness_in(
    fake: FakeBackend,
    paused: bool,
    dir: std::path::PathBuf,
) -> Harness<'static, EvoApp> {
    let opts = AppOptions {
        start_paused: paused,
        ..AppOptions::new(dir)
    };
    Harness::builder()
        .with_size([520.0, 960.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .build_eframe(|cc| EvoApp::new(&cc.egui_ctx, Box::new(fake), opts))
}

/// Clicks the widget labeled `label` and lets the click play out.
fn click(h: &mut Harness<'static, EvoApp>, label: &str) {
    h.get_by_label(label).click();
    h.run_steps(3);
}

/// A test-local `eframe::Storage`.
#[derive(Default)]
struct MemStorage(std::collections::HashMap<String, String>);

impl eframe::Storage for MemStorage {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.to_owned(), value);
    }
    fn remove_string(&mut self, key: &str) {
        self.0.remove(key);
    }
    fn flush(&mut self) {}
}

#[test]
fn toolbar_has_every_tool() {
    let mut h = harness(FakeBackend::new(), true);
    h.run_steps(1);
    for t in Tool::ALL {
        h.get_by_label(t.tooltip());
    }
}

#[test]
fn reset_button_resets_backend() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let dir = fresh_dir("reset");
    let mut h = harness_in(fake, true, dir.to_path_buf());
    h.run_steps(1);
    click(&mut h, "Reset");
    assert_eq!(probe.resets(), 1);
    assert_eq!(probe.persists(), 1);
    // The toolbar button and the toast.
    assert_eq!(h.query_all_by_label("Reset").count(), 2);
}

#[test]
fn save_and_load_via_toolbar() {
    let dir = fresh_dir("saveload");
    let store = Store::new(dir.to_path_buf());
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = harness_in(fake, true, dir.to_path_buf());
    h.run_steps(1);
    click(&mut h, "Save state");
    assert_eq!(h.query_all_by_label("empty").count(), 4);
    click(&mut h, "Slot 1");
    assert!(store.slot_path(1).exists());
    h.get_by_label("saved");
    h.get_by_label("Saved slot 1");

    click(&mut h, "Load state");
    click(&mut h, "Slot 1");
    h.get_by_label("Loaded slot 1");
    // Loading persists flash first.
    assert_eq!(probe.persists(), 1);
}

#[test]
fn loading_empty_slot_shows_error_toast() {
    let dir = fresh_dir("loadempty");
    let mut h = harness_in(FakeBackend::new(), true, dir.to_path_buf());
    h.run_steps(1);
    click(&mut h, "Load state");
    click(&mut h, "Slot 2");
    h.get_by_label("Load failed: slot 2 is empty");
}

#[test]
fn loading_corrupt_slot_shows_error_toast() {
    let dir = fresh_dir("loadcorrupt");
    let store = Store::new(dir.to_path_buf());
    let path = store.slot_path(1);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"hello").unwrap();
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = harness_in(fake, true, dir.to_path_buf());
    h.run_steps(1);
    let before = probe.cycles();
    click(&mut h, "Load state");
    h.get_by_label("saved");
    click(&mut h, "Slot 1");
    h.get_by_label_contains("Load failed");
    assert_eq!(probe.cycles(), before);
    assert_eq!(probe.resets(), 0);
}

#[test]
fn load_while_key_held_releases_keys() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let dir = fresh_dir("loadheld");
    let mut h = harness_in(fake, true, dir.to_path_buf());
    h.run_steps(1);
    h.key_down(Key::Enter);
    h.run_steps(1);
    assert_eq!(probe.held(), [KeyId::Enter]);
    click(&mut h, "Save state");
    click(&mut h, "Slot 1");
    click(&mut h, "Load state");
    click(&mut h, "Slot 1");
    h.get_by_label("Loaded slot 1");
    assert!(probe.held().is_empty(), "{:?}", probe.held());
    assert!(!h.state().key_down(KeyId::Enter));
}

#[test]
fn save_hook_persists_flash() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let dir = fresh_dir("savehook");
    let mut h = harness_in(fake, true, dir.to_path_buf());
    h.run_steps(1);
    eframe::App::save(h.state_mut(), &mut MemStorage::default());
    assert_eq!(probe.persists(), 1);
}

#[test]
fn smooth_toggle_flips_filter() {
    let mut h = harness(FakeBackend::new(), true);
    h.run_steps(1);
    assert_eq!(h.state().filter(), ScaleFilter::Nearest);
    click(&mut h, "Smooth scaling (better at non-integer sizes)");
    assert_eq!(h.state().filter(), ScaleFilter::Smooth);
    click(&mut h, "Smooth scaling (better at non-integer sizes)");
    assert_eq!(h.state().filter(), ScaleFilter::Nearest);
}

#[test]
fn debug_toggle_flips_debug_open() {
    let mut h = harness(FakeBackend::new(), true);
    h.run_steps(1);
    click(&mut h, "Debug panels");
    assert!(h.state().debug_open());
    click(&mut h, "Debug panels");
    assert!(!h.state().debug_open());
}

#[test]
fn screenshot_button_writes_png() {
    let dir = fresh_dir("screenshot");
    let mut h = harness_in(FakeBackend::new(), true, dir.to_path_buf());
    h.run_steps(1);
    click(&mut h, "Screenshot");
    let shots: Vec<_> = std::fs::read_dir(dir.join("screenshots"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(shots.len(), 1, "{shots:?}");
    h.get_by_label_contains("Screenshot saved to");
}

#[test]
fn screenshot_without_display_shows_error_toast() {
    let dir = fresh_dir("noshot");
    let mut h = harness_in(FakeBackend::without_display(), true, dir.to_path_buf());
    h.run_steps(1);
    click(&mut h, "Screenshot");
    h.get_by_label("Screenshot failed: no display frame to capture");
}

#[test]
fn toasts_expire() {
    let dir = fresh_dir("expire");
    let mut h = harness_in(FakeBackend::new(), true, dir.to_path_buf());
    h.run_steps(1);
    click(&mut h, "Reset");
    assert_eq!(h.query_all_by_label("Reset").count(), 2);
    // 4 s at 60 frames per second, plus slack.
    h.run_steps(260);
    assert_eq!(h.query_all_by_label("Reset").count(), 1);
}

#[test]
fn about_window_shows_disclaimer() {
    let mut h = harness(FakeBackend::new(), true);
    h.run_steps(1);
    click(&mut h, "About");
    h.get_by_label_contains("Not affiliated with Texas Instruments");
    h.get_by_label("You must supply your own OS bundle.");
    h.get_by_label_contains(env!("CARGO_PKG_VERSION"));
}

// ---------------------------------------------------------------------------
// Debug panels (Task 8).

/// A harness at an explicit window size.
fn harness_sized(fake: FakeBackend, paused: bool, size: [f32; 2]) -> Harness<'static, EvoApp> {
    let opts = AppOptions {
        start_paused: paused,
        ..AppOptions::new(data_dir())
    };
    Harness::builder()
        .with_size(size)
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .build_eframe(|cc| EvoApp::new(&cc.egui_ctx, Box::new(fake), opts))
}

const WIDE: [f32; 2] = [1060.0, 960.0];

/// A paused wide harness with the debug panels open.
fn debug_harness(fake: FakeBackend) -> Harness<'static, EvoApp> {
    let mut h = harness_sized(fake, true, WIDE);
    h.run_steps(1);
    click(&mut h, "Debug panels");
    h
}

#[test]
fn debug_toggle_shows_registers() {
    let mut h = debug_harness(FakeBackend::new());
    h.get_by_label("r0");
    h.get_by_label("0x10000200");
    h.get_by_label("sp");
    h.get_by_label("xpsr");
    h.get_by_label("Secure");
    h.get_by_label("Thread");
    h.get_by_label("msp_ns");
    h.get_by_label("0x20093000");
    for tab in ["Registers", "Disassembly", "Memory", "UART", "Unmapped"] {
        h.get_by_label(tab);
    }
    click(&mut h, "Debug panels");
    assert!(h.query_by_label("r0").is_none());
}

#[test]
fn debug_keeps_calculator_usable() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = harness_sized(fake, true, [520.0, 960.0]);
    h.run_steps(1);
    h.get_by_label("Debug panels").click();
    h.run_steps(2);
    assert!(h.state().debug_open());
    let scale = h.state().calc_transform().scale;
    assert!(scale >= 0.6 - 1e-3, "scale {scale}");
    // The panel fits the window instead of growing past its right edge.
    for label in ["Step", "Registers", "Unmapped", "r0", "0x10000200"] {
        let r = h.get_by_label(label).rect();
        assert!(r.right() <= 520.0 + 1e-3, "{label} at {r:?}");
    }
    h.get_by_label("enter").click();
    h.run_steps(1);
    assert_eq!(probe.key_log(), [(KeyId::Enter, true)]);
}

#[test]
fn typing_in_memory_address_does_not_press_keys() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = debug_harness(fake);
    click(&mut h, "Memory");
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    h.run_steps(2);
    h.key_down(Key::Num5);
    h.key_up(Key::Num5);
    h.run_steps(2);
    assert!(probe.key_log().is_empty(), "{:?}", probe.key_log());
}

#[test]
fn memory_tab_shows_hex_dump() {
    let mut h = debug_harness(FakeBackend::new());
    click(&mut h, "Memory");
    h.get_by_label("20000000  00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f  |................|");
    // Bytes 0x20..=0x2f are printable ASCII.
    h.get_by_label(
        "20000020  20 21 22 23 24 25 26 27 28 29 2a 2b 2c 2d 2e 2f  | !\"#$%&'()*+,-./|",
    );
}

#[test]
fn memory_tab_shows_unreadable_bytes() {
    let mut h = debug_harness(FakeBackend::new());
    click(&mut h, "Memory");
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, Key::A);
    h.run_steps(1);
    h.get_by_role(egui::accesskit::Role::TextInput)
        .type_text("0x1fff_fff8");
    h.run_steps(3);
    h.get_by_label("1ffffff8  ?? ?? ?? ?? ?? ?? ?? ?? 00 01 02 03 04 05 06 07  |................|");
}

#[test]
fn uart_tab_shows_output() {
    let mut fake = FakeBackend::new();
    fake.run_for(100_000_000);
    let mut h = debug_harness(fake);
    click(&mut h, "UART");
    h.get_by_label_contains("fake: 100 Mcycles");
    click(&mut h, "Clear");
    assert!(h.query_by_label_contains("fake: 100 Mcycles").is_none());
}

#[test]
fn clicking_disasm_line_sets_breakpoint() {
    let mut h = debug_harness(FakeBackend::new());
    click(&mut h, "Disassembly");
    click(&mut h, " \u{25B6} 10000200  bf00       nop");
    assert_eq!(h.state().backend().breakpoints(), [FAKE_PC]);
    h.get_by_label("\u{25CF}\u{25B6} 10000200  bf00       nop");
    click(&mut h, "\u{25CF}\u{25B6} 10000200  bf00       nop");
    assert!(h.state().backend().breakpoints().is_empty());
}

#[test]
fn step_button_steps_when_paused() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = debug_harness(fake);
    click(&mut h, "Step");
    assert_eq!(probe.cycles(), 1);
}

#[test]
fn continue_and_pause_buttons() {
    let mut h = debug_harness(FakeBackend::new());
    click(&mut h, "Continue");
    assert!(!h.state().paused());
    click(&mut h, "Pause");
    assert!(h.state().paused());
    h.get_by_label("Continue");
}

#[test]
fn unmapped_tab_lists_rows() {
    let mut h = debug_harness(FakeBackend::new());
    click(&mut h, "Unmapped");
    for col in ["addr", "width", "kind", "value", "pc", "sec", "count"] {
        h.get_by_label(col);
    }
    h.get_by_label("1 address");
    h.get_by_label("0x50208014");
    h.get_by_label("read");
}

#[test]
fn breakpoint_stop_opens_registers_tab() {
    let mut fake = FakeBackend::new();
    fake.toggle_breakpoint(FAKE_PC);
    let mut h = harness_sized(fake, false, WIDE);
    h.run_steps(3);
    assert!(h.state().debug_open());
    h.get_by_label("r0");
}

#[test]
fn debug_width_cap_keeps_calculator_at_0_6() {
    // 520 - toolbar 48 - margins 16 - 0.6 * 372.
    assert!((debug_max_width(520.0) - 232.8).abs() < 1e-3);
    assert!((debug_max_width(1060.0) - 772.8).abs() < 1e-3);
    assert_eq!(debug_max_width(200.0), 0.0);
}

#[test]
fn grown_window_gives_the_panel_its_default_width() {
    let mut h = harness_sized(FakeBackend::new(), true, [520.0, 960.0]);
    h.run_steps(1);
    // kittest leaves the viewport size unset and ignores resize commands:
    // report the size, then grow the window as the OS would.
    let inner = Rect::from_min_size(Pos2::ZERO, vec2(520.0, 960.0));
    h.input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .inner_rect = Some(inner);
    click(&mut h, "Debug panels");
    h.set_size(vec2(940.0, 960.0));
    h.run_steps(3);
    // The panel spans about DEBUG_W (420) from the right edge, not the
    // 232.8-point cap of the 520-point window it opened in.
    let left = h.get_by_label("Continue").rect().left();
    assert!(left < 940.0 - 400.0, "Continue at x {left}");
}

#[test]
fn choose_backend_rules() {
    use evo_gui::app::{Choice, choose_backend};
    use std::ffi::OsString;
    use std::path::PathBuf;
    let arg = || Some(PathBuf::from("arg.84b2"));
    let env = || Some(OsString::from("env.84b2"));
    // --demo wins over everything.
    assert_eq!(
        choose_backend(arg(), true, env()),
        Choice::Demo { notice: false }
    );
    // Then the argument, over the environment.
    assert_eq!(
        choose_backend(arg(), false, env()),
        Choice::Bundle(PathBuf::from("arg.84b2"))
    );
    // Then EVO_OS_IMAGE.
    assert_eq!(
        choose_backend(None, false, env()),
        Choice::Bundle(PathBuf::from("env.84b2"))
    );
    // Else the demo, with the notice.
    assert_eq!(
        choose_backend(None, false, None),
        Choice::Demo { notice: true }
    );
    // An empty EVO_OS_IMAGE counts as unset.
    assert_eq!(
        choose_backend(None, false, Some(OsString::new())),
        Choice::Demo { notice: true }
    );
}

#[test]
fn notice_is_shown_as_a_toast() {
    use evo_gui::app::DEMO_NOTICE;
    assert_eq!(
        DEMO_NOTICE,
        "No OS bundle given: running the demo backend. Usage: evo-gui <bundle.84b2>"
    );
    let opts = AppOptions {
        start_paused: true,
        notice: Some(DEMO_NOTICE.to_owned()),
        ..AppOptions::new(data_dir())
    };
    let mut h = Harness::builder()
        .with_size([520.0, 960.0])
        .with_pixels_per_point(1.0)
        .build_eframe(|cc| EvoApp::new(&cc.egui_ctx, Box::new(FakeBackend::new()), opts));
    h.run_steps(1);
    h.get_by_label(DEMO_NOTICE);
}

// ---------------------------------------------------------------------------
// First-run provisioning and Re-provision.

const PREPARING: &str = "Preparing calculator\u{2026}";

/// Steps until provisioning has ended, at most about 10 s of wall time.
fn finish_provisioning(h: &mut Harness<'static, EvoApp>) {
    for _ in 0..1000 {
        if !h.state().provisioning() {
            h.run_steps(2);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        h.run_steps(1);
    }
    panic!("provisioning did not end");
}

#[test]
fn fresh_unit_is_provisioned_off_the_ui_thread() {
    let mut fake = FakeBackend::new().needing_provision();
    let gate = fake.provision_gate();
    let probe = fake.probe();
    let dir = fresh_dir("provision");
    let mut h = harness_in(fake, false, dir.to_path_buf());
    h.run_steps(3);
    // The UI keeps running while provisioning waits on the gate.
    assert!(h.state().provisioning());
    assert_eq!(probe.provisions(), 1);
    assert_eq!(
        h.query_all_by_label(PREPARING).count(),
        2,
        "toast and header"
    );
    // Emulation and flash writes wait for it.
    let runs = probe.run_calls();
    h.run_steps(5);
    assert_eq!(probe.run_calls(), runs);
    assert_eq!(probe.persists(), 0);
    // Keys pressed meanwhile do not reach the machine.
    h.event(Event::Key {
        key: Key::Num5,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run_steps(2);

    gate.send(()).unwrap();
    finish_provisioning(&mut h);
    assert!(!h.state().provisioning());
    assert!(!h.state().backend().needs_provision());
    assert_eq!(probe.provisions(), 1);
    // The provisioned flash is written at once, then the machine runs.
    assert_eq!(probe.persists(), 1);
    h.get_by_label("Calculator ready");
    assert!(h.query_by_label(PREPARING).is_none());
    assert!(probe.run_calls() > runs);
    assert!(!probe.key_log().contains(&(KeyId::Five, true)));
}

#[test]
fn provisioned_unit_starts_without_provisioning() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = harness(fake, false);
    h.run_steps(3);
    assert!(!h.state().provisioning());
    assert_eq!(probe.provisions(), 0);
    assert!(h.query_by_label(PREPARING).is_none());
}

#[test]
fn reprovision_tool_provisions_again() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let dir = fresh_dir("reprovision");
    let mut h = harness_in(fake, true, dir.to_path_buf());
    h.run_steps(1);
    click(&mut h, "Re-provision");
    finish_provisioning(&mut h);
    assert_eq!(probe.provisions(), 1);
    assert_eq!(probe.persists(), 1);
    h.get_by_label("Calculator ready");
}

#[test]
fn machine_tools_wait_for_provisioning() {
    let mut fake = FakeBackend::new().needing_provision();
    let gate = fake.provision_gate();
    let probe = fake.probe();
    let dir = fresh_dir("provisionbusy");
    let mut h = harness_in(fake, true, dir.to_path_buf());
    h.run_steps(2);
    click(&mut h, "Reset");
    click(&mut h, "Re-provision");
    assert_eq!(
        h.query_all_by_label("Wait until the calculator is ready")
            .count(),
        2
    );
    assert_eq!(probe.resets(), 0);
    drop(gate);
    finish_provisioning(&mut h);
    assert_eq!(probe.provisions(), 1);
}

#[test]
fn failed_provisioning_shows_error_and_keeps_machine() {
    let fake = FakeBackend::new().needing_provision().failing_provision();
    let probe = fake.probe();
    let dir = fresh_dir("provisionfail");
    let mut h = harness_in(fake, true, dir.to_path_buf());
    h.run_steps(1);
    finish_provisioning(&mut h);
    h.get_by_label("provisioning failed: the fake always fails");
    assert_eq!(h.state().backend().name(), "demo");
    assert!(h.state().backend().needs_provision());
    assert_eq!(probe.persists(), 0);
}
