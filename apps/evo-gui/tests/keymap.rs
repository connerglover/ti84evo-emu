//! The keyboard-mapping window and persisted preferences.

use egui::Key;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp};
use evo_gui::backend::fake::FakeBackend;
use evo_gui::input::{Keymap, PcKey};
use evo_gui::keypad::KeyId;
use evo_gui::lcd::ScaleFilter;
use evo_gui::prefs::{PREFS_KEY, Prefs};

fn harness_with(fake: FakeBackend, prefs: Prefs) -> Harness<'static, EvoApp> {
    let opts = AppOptions {
        start_paused: true,
        prefs,
        ..AppOptions::new(std::env::temp_dir().join("evo-gui-tests-keymap"))
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

/// Opens the keyboard-mapping window from the toolbar.
fn open_window(h: &mut Harness<'static, EvoApp>) {
    h.get_by_label("Keyboard mapping").click();
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
    };
    let h = harness_with(FakeBackend::new(), prefs.clone());
    assert_eq!(h.state().prefs(), prefs);
    assert!(h.state().debug_open());
    assert_eq!(h.state().filter(), ScaleFilter::Smooth);
}

#[test]
fn save_writes_prefs() {
    let mut h = harness(FakeBackend::new());
    h.get_by_label("Smooth scaling (better at non-integer sizes)")
        .click();
    h.run_steps(2);
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
