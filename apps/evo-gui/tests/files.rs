//! Lane F, Task F1: import and export of RAM, calculator state, debug state, settings and flash.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{TempDir, harness_with, open_action, test_options};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp};
use evo_gui::backend::fake::{FAKE_MEM, FakeBackend, FakeProbe};
use evo_gui::backend::{EmuBackend, WatchKinds, Watchpoint};
use evo_gui::files::{DebugStateFile, IMPORT_MAX_BYTES, SettingsFile, check_size, read_limited};
use evo_gui::input::PcKey;
use evo_gui::keypad::KeyId;
use evo_gui::platform::FakePlatform;
use evo_gui::prefs::Prefs;
use evo_gui::store::{Store, read_state_file, write_state_file};
use evo_gui::toolbar::Action;

const ADDR: u32 = FAKE_MEM.start + 0x40;

struct Rig {
    h: Harness<'static, EvoApp>,
    platform: Arc<FakePlatform>,
    probe: FakeProbe,
    dir: TempDir,
}

/// A paused harness on `fake` in a fresh data dir.
fn rig_with(name: &str, fake: FakeBackend) -> Rig {
    let dir = TempDir::new(&format!("evo-files-{name}"), true);
    let platform = FakePlatform::shared();
    let probe = fake.probe();
    let opts = AppOptions {
        start_paused: true,
        ..test_options(&dir, platform.clone())
    };
    let h = harness_with(fake, opts);
    Rig {
        h,
        platform,
        probe,
        dir,
    }
}

fn rig(name: &str) -> Rig {
    rig_with(name, FakeBackend::new())
}

impl Rig {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn export(&mut self, a: Action, to: &Path) {
        self.platform.queue_save(Some(to.to_path_buf()));
        self.h.state_mut().run_action(a);
        self.h.run_steps(1);
    }

    fn import(&mut self, a: Action, from: &Path) {
        self.platform.queue_open(Some(from.to_path_buf()));
        self.h.state_mut().run_action(a);
        self.h.run_steps(1);
    }

    fn peek(&self) -> u8 {
        self.h.state().backend().peek(ADDR, 1)[0].unwrap()
    }

    fn poke(&mut self, v: u8) {
        self.h.state_mut().backend_mut().poke(ADDR, &[v]).unwrap();
    }

    fn has_toast(&self, text: &str) {
        self.h.get_by_label(text);
    }

    fn has_toast_starting(&self, prefix: &str) {
        let n = self.h.query_all_by_label_contains(prefix).count();
        assert!(n >= 1, "no toast containing {prefix:?}");
    }
}

fn sparse(path: &Path, len: u64) {
    let f = std::fs::File::create(path).unwrap();
    f.set_len(len).unwrap();
}

// ---- pure helpers --------------------------------------------------------------------------

#[test]
fn read_limited_and_check_size() {
    let dir = TempDir::new("evo-files-limited", true);
    let small = dir.join("small.bin");
    std::fs::write(&small, b"abc").unwrap();
    assert_eq!(read_limited(&small, 3).unwrap(), b"abc");
    assert_eq!(read_limited(&small, 2).unwrap_err(), "file is too large");
    assert_eq!(check_size(&small, 2).unwrap_err(), "file is too large");
    assert!(check_size(&small, 3).is_ok());
    assert_eq!(read_limited(&dir, 10).unwrap_err(), "is a directory");
    assert_eq!(check_size(&dir, 10).unwrap_err(), "is a directory");
    assert!(read_limited(&dir.join("missing"), 10).is_err());
}

#[test]
fn state_file_functions_roundtrip() {
    let dir = TempDir::new("evo-files-statefn", true);
    let path = dir.join("a.evostate");
    let mut b = FakeBackend::new();
    b.run_for(1234);
    write_state_file(&path, &b).unwrap();
    let mut c = FakeBackend::new();
    read_state_file(&path, &mut c).unwrap();
    assert_eq!(c.cycles(), 1234);
    // No temporary file is left next to the target.
    let names: Vec<_> = std::fs::read_dir(&*dir).unwrap().collect();
    assert_eq!(names.len(), 1);
}

// ---- roundtrips ----------------------------------------------------------------------------

#[test]
fn ram_roundtrip() {
    let mut r = rig("ram");
    r.poke(0x5a);
    let file = r.path("evo.evoram");
    r.export(Action::ExportRam, &file);
    r.has_toast(&format!("Exported RAM image to {}", file.display()));
    assert!(file.is_file());
    r.poke(0x11);
    r.import(Action::ImportRam, &file);
    r.has_toast("Imported RAM image");
    assert_eq!(r.peek(), 0x5a);
    // Imports persist flash first, as slot loads do.
    assert_eq!(r.probe.persists(), 1);
    assert_eq!(r.platform.asked(), ["Export RAM image", "Import RAM image"]);
}

#[test]
fn ram_roundtrip_through_the_sidebar() {
    let mut r = rig("ramui");
    r.poke(0x33);
    let file = r.path("ui.evoram");
    r.platform.queue_save(Some(file.clone()));
    open_action(&mut r.h, Action::ExportRam);
    r.has_toast(&format!("Exported RAM image to {}", file.display()));
    r.poke(0x44);
    r.platform.queue_open(Some(file));
    open_action(&mut r.h, Action::ImportRam);
    r.has_toast("Imported RAM image");
    assert_eq!(r.peek(), 0x33);
}

#[test]
fn state_roundtrip_and_slot_compat() {
    let mut r = rig("state");
    r.h.state_mut().backend_mut().run_for(5_000);
    let at = r.probe.cycles();
    let file = r.path("evo.evostate");
    r.export(Action::ExportState, &file);
    r.has_toast(&format!("Exported calculator state to {}", file.display()));
    let store = Store::new(r.dir.to_path_buf());
    let slot = store.save_slot(1, r.h.state().backend()).unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), std::fs::read(&slot).unwrap());

    r.h.state_mut().backend_mut().run_for(777);
    assert_ne!(r.probe.cycles(), at);
    r.import(Action::ImportState, &file);
    r.has_toast("Imported calculator state");
    assert_eq!(r.probe.cycles(), at);
    assert_eq!(r.probe.persists(), 1);
}

#[test]
fn debug_state_roundtrip() {
    let mut r = rig("debug");
    let wp = Watchpoint {
        start: FAKE_MEM.start + 0x100,
        len: 4,
        kinds: WatchKinds {
            read: false,
            write: true,
            exec: false,
        },
    };
    {
        let b = r.h.state_mut().backend_mut();
        b.set_breakpoints(vec![0x1000_0100, 0x1000_0200]);
        b.set_watchpoints(vec![wp]).unwrap();
    }
    let file = r.path("evo.evodebug");
    r.export(Action::ExportDebug, &file);
    r.has_toast(&format!("Exported debug state to {}", file.display()));
    {
        let b = r.h.state_mut().backend_mut();
        b.set_breakpoints(vec![]);
        b.set_watchpoints(vec![]).unwrap();
    }
    r.import(Action::ImportDebug, &file);
    r.has_toast("Imported debug state");
    let b = r.h.state().backend();
    assert_eq!(b.breakpoints(), [0x1000_0100, 0x1000_0200]);
    assert_eq!(b.watchpoints(), [wp]);
    let parsed: DebugStateFile = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(parsed.format, "evo-gui-debug");
    assert_eq!(parsed.version, 1);
}

#[test]
fn settings_roundtrip_keeps_bundle() {
    let mut r = rig("settings");
    let file = r.path("evo-settings.json");
    r.export(Action::ExportSettings, &file);
    r.has_toast(&format!("Exported settings to {}", file.display()));
    let mut parsed: SettingsFile = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(parsed.format, "evo-gui-settings");
    assert_eq!(parsed.version, 1);
    assert_eq!(parsed.prefs, r.h.state().prefs());

    let f20 = PcKey {
        key: egui::Key::F20,
        shift: false,
    };
    assert_eq!(r.h.state().prefs().keymap.lookup(f20), None);
    parsed.prefs.keymap.bind(f20, KeyId::Enter);
    parsed.prefs.bundle = Some(PathBuf::from("x"));
    parsed.prefs.compact = true;
    std::fs::write(&file, serde_json::to_vec(&parsed).unwrap()).unwrap();

    let bundle_before = r.h.state().prefs().bundle;
    r.import(Action::ImportSettings, &file);
    r.has_toast("Imported settings");
    let prefs = r.h.state().prefs();
    assert_eq!(prefs.keymap.lookup(f20), Some(KeyId::Enter));
    assert!(prefs.compact);
    assert_eq!(prefs.bundle, bundle_before);
}

#[test]
fn flash_roundtrip() {
    let mut r = rig("flash");
    let file = r.path("evo.evoflash");
    r.export(Action::ExportFlash, &file);
    r.has_toast(&format!("Exported flash image to {}", file.display()));
    r.import(Action::ImportFlash, &file);
    r.has_toast("Imported flash image");
    assert_eq!(r.probe.flash_imports(), 1);
    assert_eq!(r.probe.persists(), 1);
}

// ---- bad files ---------------------------------------------------------------------------

#[test]
fn import_ram_garbage_keeps_machine() {
    let mut r = rig("ramgarbage");
    r.poke(0x77);
    let file = r.path("bad.evoram");
    std::fs::write(&file, b"garbage, not a RAM image").unwrap();
    r.import(Action::ImportRam, &file);
    r.has_toast_starting("Import failed: ");
    assert_eq!(r.peek(), 0x77);
}

#[test]
fn import_state_garbage_keeps_machine() {
    let mut r = rig("stategarbage");
    r.h.state_mut().backend_mut().run_for(321);
    let at = r.probe.cycles();
    let file = r.path("bad.evostate");
    for bytes in [&b"garbage"[..], b"EVOGUIST\x01", b""] {
        std::fs::write(&file, bytes).unwrap();
        r.import(Action::ImportState, &file);
        assert_eq!(r.probe.cycles(), at);
    }
    r.has_toast_starting("Import failed: ");
}

#[test]
fn import_debug_garbage_keeps_breakpoints() {
    let mut r = rig("debuggarbage");
    r.h.state_mut()
        .backend_mut()
        .set_breakpoints(vec![0x1000_0100]);
    let file = r.path("bad.evodebug");
    std::fs::write(&file, b"{ not json").unwrap();
    r.import(Action::ImportDebug, &file);
    r.has_toast("Import failed: not a debug state file");
    assert_eq!(r.h.state().backend().breakpoints(), [0x1000_0100]);
}

#[test]
fn import_debug_wrong_version_keeps_breakpoints() {
    let mut r = rig("debugversion");
    r.h.state_mut()
        .backend_mut()
        .set_breakpoints(vec![0x1000_0100]);
    let file = r.path("v2.evodebug");
    let f = DebugStateFile {
        format: "evo-gui-debug".to_owned(),
        version: 2,
        breakpoints: vec![0x1000_0300],
        watchpoints: vec![],
    };
    std::fs::write(&file, serde_json::to_vec(&f).unwrap()).unwrap();
    r.import(Action::ImportDebug, &file);
    r.has_toast("Import failed: not a debug state file");
    assert_eq!(r.h.state().backend().breakpoints(), [0x1000_0100]);
}

#[test]
fn import_debug_invalid_watchpoint_applies_nothing() {
    let mut r = rig("debuginvalid");
    r.h.state_mut()
        .backend_mut()
        .set_breakpoints(vec![0x1000_0100]);
    let file = r.path("inv.evodebug");
    let f = DebugStateFile {
        format: "evo-gui-debug".to_owned(),
        version: 1,
        breakpoints: vec![0x1000_0300],
        watchpoints: vec![Watchpoint {
            start: 0,
            len: 0,
            kinds: WatchKinds::default(),
        }],
    };
    std::fs::write(&file, serde_json::to_vec(&f).unwrap()).unwrap();
    r.import(Action::ImportDebug, &file);
    r.has_toast_starting("Import failed: ");
    assert_eq!(r.h.state().backend().breakpoints(), [0x1000_0100]);
    assert!(r.h.state().backend().watchpoints().is_empty());
}

#[test]
fn import_settings_garbage_keeps_prefs() {
    let mut r = rig("settingsgarbage");
    let before = r.h.state().prefs();
    let file = r.path("bad.json");
    std::fs::write(&file, b"[1, 2, 3]").unwrap();
    r.import(Action::ImportSettings, &file);
    r.has_toast("Import failed: not a settings file");
    assert_eq!(r.h.state().prefs(), before);
}

#[test]
fn import_settings_wrong_format_keeps_prefs() {
    for (format, version) in [("evo-gui-debug", 1), ("evo-gui-settings", 2)] {
        let mut r = rig("settingsformat");
        let before = r.h.state().prefs();
        let file = r.path("other.json");
        let f = SettingsFile {
            format: format.to_owned(),
            version,
            prefs: Prefs {
                compact: true,
                ..Prefs::default()
            },
        };
        std::fs::write(&file, serde_json::to_vec(&f).unwrap()).unwrap();
        r.import(Action::ImportSettings, &file);
        r.has_toast("Import failed: not a settings file");
        assert_eq!(r.h.state().prefs(), before, "{format} v{version}");
    }
}

#[test]
fn import_flash_garbage_keeps_machine() {
    let mut r = rig("flashgarbage");
    let file = r.path("bad.evoflash");
    std::fs::write(&file, b"garbage").unwrap();
    r.import(Action::ImportFlash, &file);
    r.has_toast_starting("Import failed: ");
    assert_eq!(r.probe.flash_imports(), 0);
}

#[test]
fn import_oversized_file_is_refused_unread() {
    let cases = [
        (Action::ImportRam, "big.evoram"),
        (Action::ImportState, "big.evostate"),
        (Action::ImportDebug, "big.evodebug"),
        (Action::ImportSettings, "big.json"),
    ];
    for (action, name) in cases {
        let mut r = rig("oversized");
        r.poke(0x66);
        let before = r.h.state().prefs();
        let at = r.probe.cycles();
        let file = r.path(name);
        sparse(&file, IMPORT_MAX_BYTES + 1);
        r.import(action, &file);
        r.has_toast("Import failed: file is too large");
        assert_eq!(r.peek(), 0x66, "{name}");
        assert_eq!(r.probe.cycles(), at, "{name}");
        assert_eq!(r.h.state().prefs(), before, "{name}");
        // The machine was not touched, so it was not persisted either.
        assert_eq!(r.probe.persists(), 0, "{name}");
    }
}

#[test]
fn import_directory_fails() {
    for action in [
        Action::ImportRam,
        Action::ImportState,
        Action::ImportDebug,
        Action::ImportSettings,
    ] {
        let mut r = rig("dir");
        let sub = r.path("sub");
        std::fs::create_dir_all(&sub).unwrap();
        r.import(action, &sub);
        r.has_toast("Import failed: is a directory");
    }
}

#[test]
fn import_flash_oversized_is_refused_unread() {
    let mut r = rig("flashbig");
    let file = r.path("big.evoflash");
    sparse(&file, IMPORT_MAX_BYTES + 1);
    r.import(Action::ImportFlash, &file);
    r.has_toast("Import failed: file is too large");
    assert_eq!(r.probe.flash_imports(), 0);
}

#[test]
fn import_flash_directory_fails() {
    let mut r = rig("flashdir");
    let sub = r.path("sub.evoflash");
    std::fs::create_dir_all(&sub).unwrap();
    r.import(Action::ImportFlash, &sub);
    r.has_toast("Import failed: is a directory");
    assert_eq!(r.probe.flash_imports(), 0);
}

#[test]
fn import_state_from_other_backend_keeps_machine() {
    let mut r = rig("otherbackend");
    r.h.state_mut().backend_mut().run_for(999);
    let at = r.probe.cycles();
    let file = r.path("other.evostate");
    let mut other = FakeBackend::new().with_state_tag("other");
    other.run_for(5);
    write_state_file(&file, &other).unwrap();
    r.import(Action::ImportState, &file);
    r.has_toast_starting("Import failed: ");
    assert_eq!(r.probe.cycles(), at);
}

#[test]
fn export_into_missing_dir_toasts() {
    let mut r = rig("exportfail");
    let file = r.path("no/such/dir/evo.evoram");
    r.export(Action::ExportRam, &file);
    r.has_toast_starting("Export failed: ");
    assert!(!file.exists());
}

#[test]
fn cancel_does_nothing() {
    let mut r = rig("cancel");
    r.poke(0x22);
    let before = r.h.state().prefs();
    let at = r.probe.cycles();
    let exports = [
        Action::ExportRam,
        Action::ExportState,
        Action::ExportDebug,
        Action::ExportSettings,
        Action::ExportFlash,
    ];
    let imports = [
        Action::ImportRam,
        Action::ImportState,
        Action::ImportDebug,
        Action::ImportSettings,
        Action::ImportFlash,
    ];
    for a in exports.into_iter().chain(imports) {
        // An empty answer queue cancels.
        r.h.state_mut().run_action(a);
        r.h.run_steps(1);
    }
    assert_eq!(r.platform.asked().len(), 10);
    for text in ["Exported", "Imported", "failed"] {
        assert_eq!(r.h.query_all_by_label_contains(text).count(), 0, "{text}");
    }
    assert_eq!(r.peek(), 0x22);
    assert_eq!(r.probe.cycles(), at);
    assert_eq!(r.probe.persists(), 0);
    assert_eq!(r.probe.flash_imports(), 0);
    assert_eq!(r.h.state().prefs(), before);
    // Nothing was written into the data dir.
    assert_eq!(std::fs::read_dir(&*r.dir).unwrap().count(), 0);
}
