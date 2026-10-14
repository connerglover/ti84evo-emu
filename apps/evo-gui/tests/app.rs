use egui::{Event, Key, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp, status_text};
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
fn fresh_dir(test: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("evo-gui-app-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
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
    let mut h = harness_in(fake, true, fresh_dir("reset"));
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
    let store = Store::new(dir.clone());
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = harness_in(fake, true, dir);
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
    let mut h = harness_in(FakeBackend::new(), true, fresh_dir("loadempty"));
    h.run_steps(1);
    click(&mut h, "Load state");
    click(&mut h, "Slot 2");
    h.get_by_label("Load failed: slot 2 is empty");
}

#[test]
fn loading_corrupt_slot_shows_error_toast() {
    let dir = fresh_dir("loadcorrupt");
    let store = Store::new(dir.clone());
    let path = store.slot_path(1);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"hello").unwrap();
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = harness_in(fake, true, dir);
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
    let mut h = harness_in(fake, true, fresh_dir("loadheld"));
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
    let mut h = harness_in(fake, true, fresh_dir("savehook"));
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
    let mut h = harness_in(FakeBackend::new(), true, dir.clone());
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
    let mut h = harness_in(FakeBackend::without_display(), true, fresh_dir("noshot"));
    h.run_steps(1);
    click(&mut h, "Screenshot");
    h.get_by_label("Screenshot failed: no display frame to capture");
}

#[test]
fn toasts_expire() {
    let mut h = harness_in(FakeBackend::new(), true, fresh_dir("expire"));
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
