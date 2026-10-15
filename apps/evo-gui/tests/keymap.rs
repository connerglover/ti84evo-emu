//! The keyboard-mapping window and persisted preferences.

mod common;

use common::{open_action, test_options};
use egui::Key;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp};
use evo_gui::backend::fake::FakeBackend;
use evo_gui::input::{Keymap, PcKey};
use evo_gui::keypad::KeyId;
use evo_gui::lcd::ScaleFilter;
use evo_gui::pacer::Speed;
use evo_gui::platform::FakePlatform;
use evo_gui::prefs::{PREFS_KEY, Prefs};
use evo_gui::toolbar::Action;

fn harness_with(fake: FakeBackend, prefs: Prefs) -> Harness<'static, EvoApp> {
    let opts = AppOptions {
        start_paused: true,
        prefs,
        ..test_options(
            &std::env::temp_dir().join("evo-gui-tests-keymap"),
            FakePlatform::shared(),
        )
    };
    let mut h = Harness::builder()
        .with_size([520.0, 960.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .build_eframe(|cc| EvoApp::new(&cc.egui_ctx, Box::new(fake), opts));
    h.run_steps(1);
    h
}

fn harness(fake: FakeBackend) -> Harness<'static, EvoApp> {
    harness_with(fake, Prefs::default())
}

/// Opens the keyboard-mapping window from the sidebar.
fn open_window(h: &mut Harness<'static, EvoApp>) {
    open_action(h, Action::Keymap);
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
fn prefs_defaults() {
    assert_eq!(PREFS_KEY, "evo-gui-prefs");
    let p = Prefs::default();
    assert_eq!(p.keymap, Keymap::default_map());
    assert_eq!(p.filter, ScaleFilter::Nearest);
    assert!(!p.debug_open);
}

#[test]
fn prefs_json_roundtrip() {
    let mut keymap = Keymap::default_map();
    keymap.bind(
        PcKey {
            key: Key::Q,
            shift: true,
        },
        KeyId::Enter,
    );
    keymap.unbind_key(KeyId::On);
    let p = Prefs {
        keymap,
        filter: ScaleFilter::Smooth,
        debug_open: true,
        ..Prefs::default()
    };
    let json = serde_json::to_string(&p).expect("serialize");
    let back: Prefs = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, p);
}

#[test]
fn prefs_missing_fields_default() {
    assert_eq!(
        serde_json::from_str::<Prefs>("{}").expect("empty object"),
        Prefs::default()
    );
    let p: Prefs = serde_json::from_str(r#"{"filter":"Smooth"}"#).expect("partial");
    assert_eq!(p.filter, ScaleFilter::Smooth);
    assert_eq!(p.keymap, Keymap::default_map());
}

#[test]
fn app_starts_from_prefs_and_reports_them() {
    let mut keymap = Keymap::default_map();
    keymap.unbind_key(KeyId::Five);
    let prefs = Prefs {
        keymap,
        filter: ScaleFilter::Smooth,
        debug_open: true,
        bundle: Some(std::path::PathBuf::from("os.84b2")),
        compact: true,
        speed: Speed::X2,
    };
    let h = harness_with(FakeBackend::new(), prefs.clone());
    assert_eq!(h.state().prefs(), prefs);
    assert!(h.state().debug_open());
    assert_eq!(h.state().filter(), ScaleFilter::Smooth);
}

#[test]
fn save_writes_prefs() {
    let mut h = harness(FakeBackend::new());
    open_action(&mut h, Action::Smooth);
    let mut storage = MemStorage::default();
    eframe::App::save(h.state_mut(), &mut storage);
    let saved: Prefs = eframe::get_value(&storage, PREFS_KEY).expect("prefs saved");
    assert_eq!(saved.filter, ScaleFilter::Smooth);
    assert_eq!(saved, h.state().prefs());
}

#[test]
fn window_lists_every_key() {
    let mut h = harness(FakeBackend::new());
    open_window(&mut h);
    assert_eq!(h.query_all_by_label("Rebind").count(), KeyId::ALL.len());
    assert_eq!(h.query_all_by_label("Clear").count(), KeyId::ALL.len());
    h.get_by_label(
        "Letters type the key with that alpha legend. F10 may be taken by Windows; \
         rebind it if it does nothing.",
    );
    h.get_by_label("Reset to defaults");
    // `on` is bound to Home only.
    h.get_by_label("Home");
}

#[test]
fn rebinding_via_window() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = harness(fake);
    open_window(&mut h);
    assert_eq!(KeyId::ALL[49], KeyId::Enter);
    h.get_all_by_label("Rebind")
        .nth(49)
        .expect("50 Rebind buttons")
        .click_accesskit();
    h.run_steps(2);
    assert!(h.state().keymap_capturing());
    h.get_by_label("Cancel");
    h.key_press(Key::Num5);
    h.run_steps(2);
    assert!(!h.state().keymap_capturing());
    assert_eq!(
        h.state().prefs().keymap.lookup(PcKey {
            key: Key::Num5,
            shift: false
        }),
        Some(KeyId::Enter)
    );
    assert!(probe.key_log().is_empty(), "{:?}", probe.key_log());

    // Afterwards the new binding presses `enter`.
    h.key_down(Key::Num5);
    h.run_steps(1);
    assert_eq!(probe.key_log(), [(KeyId::Enter, true)]);
}

#[test]
fn cancel_stops_capturing() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = harness(fake);
    open_window(&mut h);
    h.get_all_by_label("Rebind")
        .next()
        .expect("Rebind buttons")
        .click_accesskit();
    h.run_steps(2);
    h.get_by_label("Cancel").click_accesskit();
    h.run_steps(2);
    assert!(!h.state().keymap_capturing());
    assert_eq!(h.state().prefs().keymap, Keymap::default_map());
    // Keys press calculator keys again.
    h.key_down(Key::Num5);
    h.run_steps(1);
    assert_eq!(probe.key_log(), [(KeyId::Five, true)]);
}

#[test]
fn clear_unbinds_key() {
    let mut h = harness(FakeBackend::new());
    open_window(&mut h);
    // Row 49 is `enter`.
    h.get_all_by_label("Clear")
        .nth(49)
        .expect("50 Clear buttons")
        .click_accesskit();
    h.run_steps(2);
    assert!(h.state().prefs().keymap.keys_for(KeyId::Enter).is_empty());
}

#[test]
fn reset_to_defaults_restores_map() {
    let mut keymap = Keymap::default_map();
    keymap.unbind_key(KeyId::Enter);
    keymap.bind(
        PcKey {
            key: Key::Num5,
            shift: false,
        },
        KeyId::Graph,
    );
    let mut h = harness_with(
        FakeBackend::new(),
        Prefs {
            keymap,
            ..Prefs::default()
        },
    );
    open_window(&mut h);
    h.get_by_label("Reset to defaults").click_accesskit();
    h.run_steps(2);
    assert_eq!(h.state().prefs().keymap, Keymap::default_map());
}

// ---------------------------------------------------------------------------
// A persisted open debug panel and eframe's window persistence.

/// The `InnerSize` commands the last frame sent to the root viewport.
fn inner_sizes(out: &egui::FullOutput) -> Vec<egui::Vec2> {
    out.viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| {
            v.commands
                .iter()
                .filter_map(|c| match c {
                    egui::ViewportCommand::InnerSize(s) => Some(*s),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Reports `size` as the window's inner size for the next frames.
fn report_inner(input: &mut egui::RawInput, size: egui::Vec2) {
    input
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .inner_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size));
}

fn debug_prefs() -> Prefs {
    Prefs {
        debug_open: true,
        ..Prefs::default()
    }
}

/// Drives the app as eframe does, where the window size is known on the very
/// first frame: eframe restored the saved window, which already includes the
/// panel, so the app must not grow it again.
#[test]
fn restored_debug_panel_does_not_grow_on_first_frame() {
    let ctx = egui::Context::default();
    let opts = AppOptions {
        start_paused: true,
        prefs: debug_prefs(),
        ..test_options(
            &std::env::temp_dir().join("evo-gui-tests-keymap"),
            FakePlatform::shared(),
        )
    };
    let mut app = EvoApp::new(&ctx, Box::new(FakeBackend::new()), opts);
    let mut frame = eframe::Frame::_new_kittest();
    let size = egui::vec2(940.0, 960.0);
    for n in 0..3 {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        report_inner(&mut input, size);
        let mut out = ctx.run_ui(input, |ui| {
            eframe::App::logic(&mut app, ui.ctx(), &mut frame);
            eframe::App::ui(&mut app, ui, &mut frame);
        });
        out.textures_delta.clear();
        assert_eq!(inner_sizes(&out), [], "frame {n} resized the window");
    }
    assert!(app.debug_open());
}

/// Clicks `label` and returns every `InnerSize` the following frames sent.
fn click_collect(h: &mut Harness<'static, EvoApp>, label: &str) -> Vec<egui::Vec2> {
    h.get_by_label(label).click();
    let mut sizes = Vec::new();
    for _ in 0..3 {
        h.step();
        sizes.extend(inner_sizes(h.output()));
    }
    sizes
}

#[test]
fn restored_debug_panel_still_shrinks_and_grows_by_debug_w() {
    use evo_gui::debug::DEBUG_W;
    let mut h = harness_with(FakeBackend::new(), debug_prefs());
    let wide = egui::vec2(520.0 + DEBUG_W, 960.0);
    h.set_size(wide);
    report_inner(h.input_mut(), wide);
    h.step();
    assert_eq!(inner_sizes(h.output()), [], "no grow at start");
    // Closing gives the panel's width back.
    let shrunk = click_collect(&mut h, "Debug panels");
    assert_eq!(shrunk, [egui::vec2(520.0, 960.0)]);
    assert!(!h.state().debug_open());
    // Reopening grows by the panel's width again.
    let narrow = egui::vec2(520.0, 960.0);
    h.set_size(narrow);
    report_inner(h.input_mut(), narrow);
    h.step();
    let grown = click_collect(&mut h, "Debug panels");
    assert_eq!(grown, [wide]);
    assert!(h.state().debug_open());
}
