//! Lane B: the ROM Setup Wizard, Change ROM and Reload ROM, on `FakeBackend` and
//! `FakePlatform` with synthetic bundles.

mod common;

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{TempDir, harness_with, open_action, test_options};
use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp, PROVISION_NOTICE};
use evo_gui::backend::EmuBackend;
use evo_gui::backend::fake::{FakeBackend, FakeProbe};
use evo_gui::platform::{FakePlatform, PlatformRef};
use evo_gui::toolbar::Action;
use evo_gui::wizard::Page;
use evo_gui::wizard::validate::{BUNDLE_MAX_BYTES, EVO_PRODID, SUPPORTED_OS, check_bundle};
use evo_image::testutil::{Img, package, zip_bytes};

/// A synthetic bundle with this version and product id (nothing in it is TI data).
fn bundle(version: &str, prodid: u32) -> Vec<u8> {
    let meta = format!(
        "bundle_build_version:{version}\nbundle_target_device:84US\n\
         bundle_target_prodid:{prodid}\nbundle_target_type:RESTORE\n"
    );
    let body: Vec<u8> = (0..0x200u32).map(|i| (i as u8) | 1).collect();
    let pkg = package(&[(0, &Img::new(0x400, body))]);
    zip_bytes(
        &[
            ("METADATA", meta.as_bytes()),
            ("Synthetic_Package.84pk2", &pkg),
            ("Archive/", b""),
        ],
        Default::default(),
    )
}

/// Writes an Evo bundle of `version` to `dir/name` and returns its path.
fn write_bundle(dir: &Path, name: &str, version: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bundle(version, EVO_PRODID)).unwrap();
    path
}

fn options(dir: &Path, platform: &Arc<FakePlatform>, open_wizard: bool) -> AppOptions {
    let platform: PlatformRef = platform.clone();
    AppOptions {
        open_wizard,
        ..test_options(&dir.join("data"), platform)
    }
}

fn page(h: &Harness<'static, EvoApp>) -> Option<Page> {
    h.state().features().rom.page()
}

fn click(h: &mut Harness<'static, EvoApp>, label: &str) {
    h.get_by_label(label).click();
    h.run_steps(2);
}

/// Steps (sleeping a little between steps) until `label` is shown; panics after 10 s.
fn wait_for(h: &mut Harness<'static, EvoApp>, label: &str) {
    let start = Instant::now();
    while h.query_by_label(label).is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{label:?} never appeared"
        );
        std::thread::sleep(Duration::from_millis(5));
        h.run_steps(1);
    }
    // The modal re-centres a frame after its content changed size: let it settle before a
    // click aims at a button.
    h.run_steps(2);
}

fn key(key: Key, pressed: bool) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}

/// A file dropped on the window (egui 0.36 hands the app a trait object per file).
#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

fn dropped(path: PathBuf) -> egui::DroppedFileHandle {
    Arc::new(Dropped(path))
}

fn presses(probe: &FakeProbe) -> usize {
    probe.key_log().iter().filter(|(_, down)| *down).count()
}

#[test]
fn check_accepts_evo_bundle() {
    let dir = TempDir::new("evo-gui-wizard-accept", true);
    let path = write_bundle(&dir, "os.84b2", SUPPORTED_OS);
    let c = check_bundle(&path).unwrap();
    assert_eq!(c.info.build_version, "7.1.0.4421");
    assert_eq!(c.info.target_device, "84US");
    assert_eq!(c.info.prodid, 23);
    assert_eq!(c.warning, None);
}

#[test]
fn check_warns_on_other_version() {
    let dir = TempDir::new("evo-gui-wizard-version", true);
    let path = write_bundle(&dir, "os.84b2", "9.8.7.6543");
    let c = check_bundle(&path).unwrap();
    assert_eq!(c.info.build_version, "9.8.7.6543");
    let w = c.warning.expect("a warning");
    assert!(w.contains("7.1.0.4421"), "{w}");
    assert_eq!(
        w,
        "OS 9.8.7.6543: provisioning is tied to OS 7.1.0.4421 and may fail with this version"
    );
}

#[test]
fn check_rejects_other_product() {
    let dir = TempDir::new("evo-gui-wizard-prodid", true);
    let path = dir.join("other.84b2");
    std::fs::write(&path, bundle(SUPPORTED_OS, 42)).unwrap();
    assert_eq!(
        check_bundle(&path).err().unwrap(),
        "This is not a TI-84 Evo OS bundle (product id 42)"
    );
}

#[test]
fn check_rejects_garbage_and_directories() {
    let dir = TempDir::new("evo-gui-wizard-garbage", true);
    let garbage = dir.join("garbage.84b2");
    std::fs::write(&garbage, b"this is not a zip file at all").unwrap();
    let e = check_bundle(&garbage).err().unwrap();
    assert!(e.starts_with("Not an OS bundle: "), "{e}");

    let truncated = dir.join("truncated.84b2");
    let full = bundle(SUPPORTED_OS, EVO_PRODID);
    std::fs::write(&truncated, &full[..full.len() / 2]).unwrap();
    let e = check_bundle(&truncated).err().unwrap();
    assert!(e.starts_with("Not an OS bundle: "), "{e}");

    let sub = dir.join("folder.84b2");
    std::fs::create_dir(&sub).unwrap();
    let e = check_bundle(&sub).err().unwrap();
    assert!(e.starts_with("Cannot read folder.84b2: "), "{e}");

    let e = check_bundle(&dir.join("missing.84b2")).err().unwrap();
    assert!(e.starts_with("Cannot read missing.84b2: "), "{e}");
}

#[test]
fn check_rejects_oversized_unread() {
    let dir = TempDir::new("evo-gui-wizard-oversized", true);
    let path = dir.join("huge.84b2");
    let f = std::fs::File::create(&path).unwrap();
    f.set_len(BUNDLE_MAX_BYTES + 1).unwrap();
    drop(f);
    assert_eq!(
        check_bundle(&path).err().unwrap(),
        "File is too large to be an OS bundle"
    );
}

#[test]
fn opens_at_start_without_rom() {
    let dir = TempDir::new("evo-gui-wizard-start", true);
    let platform = FakePlatform::shared();
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let mut h = harness_with(fake, options(&dir, &platform, true));
    h.run_steps(2);
    assert_eq!(page(&h), Some(Page::Welcome));
    h.get_by_label("ROM Setup Wizard");
    h.get_by_label("Welcome");
    h.get_by_label(
        "This emulator needs a TI-84 Evo OS bundle (.84b2). It is not included: use your own copy.",
    );
    h.input_mut().events.push(key(Key::Enter, true));
    h.run_steps(3);
    h.input_mut().events.push(key(Key::Enter, false));
    h.run_steps(30);
    assert_eq!(presses(&probe), 0, "{:?}", probe.key_log());

    // Once the wizard is closed the same key reaches the machine.
    click(&mut h, "Cancel");
    assert_eq!(page(&h), None);
    h.input_mut().events.push(key(Key::Enter, true));
    h.run_steps(3);
    h.input_mut().events.push(key(Key::Enter, false));
    h.run_steps(30);
    assert!(presses(&probe) > 0, "{:?}", probe.key_log());
}

#[test]
fn closed_without_open_wizard() {
    let dir = TempDir::new("evo-gui-wizard-closed", true);
    let platform = FakePlatform::shared();
    let h = harness_with(FakeBackend::new(), options(&dir, &platform, false));
    assert_eq!(page(&h), None);
    assert!(h.query_by_label("ROM Setup Wizard").is_none());
}

#[test]
fn browse_validate_next_replaces_backend() {
    let dir = TempDir::new("evo-gui-wizard-browse", true);
    let path = write_bundle(&dir, "os.84b2", SUPPORTED_OS);
    let platform = FakePlatform::shared();
    let fake = FakeBackend::new();
    let old = fake.probe();
    let mut h = harness_with(fake, options(&dir, &platform, true));
    click(&mut h, "Next");
    assert_eq!(page(&h), Some(Page::Choose));
    h.get_by_label("Choose an OS bundle");

    // Next is disabled until a bundle passed the check.
    click(&mut h, "Next");
    assert_eq!(page(&h), Some(Page::Choose));
    assert!(platform.opened().is_empty());

    platform.queue_open(Some(path.clone()));
    click(&mut h, "Browse\u{2026}");
    assert_eq!(platform.asked(), ["Choose an OS bundle"]);
    h.get_by_label("OS 7.1.0.4421 \u{b7} 84US");
    click(&mut h, "Next");
    h.run_steps(2);
    assert_eq!(platform.opened(), std::slice::from_ref(&path));
    assert_eq!(h.state().backend().name(), "os.84b2");
    assert_eq!(h.state().rom(), Some(path.as_path()));
    assert_eq!(h.state().prefs().bundle, Some(path.clone()));
    assert!(old.persists() >= 1, "the old flash is saved first");
    assert_eq!(page(&h), Some(Page::Done));
    h.get_by_label("Done");
    h.get_by_label("The calculator is ready.");
    click(&mut h, "Finish");
    assert_eq!(page(&h), None);
}

/// The flash dir the wizard passed to `open_backend` (recorded by the factory).
fn platform_nv(h: &Harness<'static, EvoApp>) -> Option<PathBuf> {
    NV_SEEN
        .lock()
        .unwrap()
        .get(&h.state().data_dir().to_path_buf())
        .cloned()
}

static NV_SEEN: Mutex<std::collections::BTreeMap<PathBuf, PathBuf>> =
    Mutex::new(std::collections::BTreeMap::new());

#[test]
fn next_opens_the_bundle_with_the_instance_flash_dir() {
    let dir = TempDir::new("evo-gui-wizard-nvdir", true);
    let path = write_bundle(&dir, "os.84b2", SUPPORTED_OS);
    let platform = FakePlatform::shared();
    let data = dir.join("data");
    platform.set_backend_factory(Box::new(move |bundle: &Path, nv: &Path| {
        let data = nv.parent().unwrap().to_path_buf();
        NV_SEEN.lock().unwrap().insert(data, nv.to_path_buf());
        let name = bundle.file_name().unwrap().to_string_lossy().into_owned();
        Ok(Box::new(FakeBackend::new().named(&name)) as Box<dyn EmuBackend>)
    }));
    let mut h = harness_with(FakeBackend::new(), options(&dir, &platform, true));
    click(&mut h, "Next");
    platform.queue_open(Some(path.clone()));
    click(&mut h, "Browse\u{2026}");
    click(&mut h, "Next");
    assert_eq!(platform_nv(&h), Some(data.join("flash")));
    assert_eq!(h.state().backend().name(), "os.84b2");
}

#[test]
fn version_warning_keeps_next_enabled() {
    let dir = TempDir::new("evo-gui-wizard-warn", true);
    let path = write_bundle(&dir, "other.84b2", "9.8.7.6543");
    let platform = FakePlatform::shared();
    let mut h = harness_with(FakeBackend::new(), options(&dir, &platform, true));
    click(&mut h, "Next");
    platform.queue_open(Some(path.clone()));
    click(&mut h, "Browse\u{2026}");
    h.get_by_label(
        "OS 9.8.7.6543: provisioning is tied to OS 7.1.0.4421 and may fail with this version",
    );
    click(&mut h, "Next");
    h.run_steps(2);
    assert_eq!(h.state().backend().name(), "other.84b2");
    assert_eq!(page(&h), Some(Page::Done));
}

#[test]
fn choose_errors_disable_next() {
    let dir = TempDir::new("evo-gui-wizard-errors", true);
    let garbage = dir.join("garbage.84b2");
    std::fs::write(&garbage, b"garbage").unwrap();
    let other = dir.join("other.84b2");
    std::fs::write(&other, bundle(SUPPORTED_OS, 42)).unwrap();
    let platform = FakePlatform::shared();
    let mut h = harness_with(FakeBackend::new(), options(&dir, &platform, true));
    click(&mut h, "Next");
    platform.queue_open(Some(garbage));
    click(&mut h, "Browse\u{2026}");
    assert!(
        h.query_by_label_contains("Not an OS bundle: ").is_some(),
        "no inline error"
    );
    click(&mut h, "Next");
    assert_eq!(page(&h), Some(Page::Choose));
    platform.queue_open(Some(other));
    click(&mut h, "Browse\u{2026}");
    h.get_by_label("This is not a TI-84 Evo OS bundle (product id 42)");
    assert!(h.query_by_label_contains("Not an OS bundle").is_none());
    click(&mut h, "Next");
    assert_eq!(page(&h), Some(Page::Choose));
    assert!(platform.opened().is_empty());
    assert_eq!(h.state().backend().name(), "demo");

    // A cancelled dialog keeps what was there.
    platform.queue_open(None);
    click(&mut h, "Browse\u{2026}");
    h.get_by_label("This is not a TI-84 Evo OS bundle (product id 42)");
}

#[test]
fn open_error_is_inline_and_keeps_backend() {
    let dir = TempDir::new("evo-gui-wizard-openerr", true);
    let path = write_bundle(&dir, "os.84b2", SUPPORTED_OS);
    let platform = FakePlatform::shared();
    platform.set_backend_factory(Box::new(|_, _| Err("flash is unreadable".to_owned())));
    let mut h = harness_with(FakeBackend::new(), options(&dir, &platform, true));
    click(&mut h, "Next");
    platform.queue_open(Some(path));
    click(&mut h, "Browse\u{2026}");
    click(&mut h, "Next");
    h.get_by_label("Cannot open os.84b2: flash is unreadable");
    assert_eq!(page(&h), Some(Page::Choose));
    assert_eq!(h.state().backend().name(), "demo");
    assert_eq!(h.state().prefs().bundle, None);
}

#[test]
fn back_returns_to_welcome() {
    let dir = TempDir::new("evo-gui-wizard-back", true);
    let platform = FakePlatform::shared();
    let mut h = harness_with(FakeBackend::new(), options(&dir, &platform, true));
    click(&mut h, "Next");
    assert_eq!(page(&h), Some(Page::Choose));
    click(&mut h, "Back");
    assert_eq!(page(&h), Some(Page::Welcome));
}

#[test]
fn prepare_waits_for_provisioning_and_retries() {
    let dir = TempDir::new("evo-gui-wizard-prepare", true);
    let path = write_bundle(&dir, "os.84b2", SUPPORTED_OS);
    let platform = FakePlatform::shared();
    let probes: Arc<Mutex<Vec<FakeProbe>>> = Arc::default();
    let gates: Arc<Mutex<Vec<Sender<()>>>> = Arc::default();
    {
        let probes = probes.clone();
        let gates = gates.clone();
        platform.set_backend_factory(Box::new(move |_, _| {
            let mut b = FakeBackend::new()
                .named("fresh")
                .needing_provision()
                .failing_provision();
            gates.lock().unwrap().push(b.provision_gate());
            probes.lock().unwrap().push(b.probe());
            Ok(Box::new(b) as Box<dyn EmuBackend>)
        }));
    }
    let mut h = harness_with(FakeBackend::new(), options(&dir, &platform, true));
    click(&mut h, "Next");
    platform.queue_open(Some(path));
    click(&mut h, "Browse\u{2026}");
    click(&mut h, "Next");
    assert_eq!(page(&h), Some(Page::Prepare));
    assert!(h.state().provisioning());
    h.get_by_label("Prepare calculator");
    // The notice is in the wizard, in the header and in the progress toast.
    assert!(h.query_all_by_label(PROVISION_NOTICE).count() >= 2);
    h.get_by_label("0 s");
    h.run_steps(150);
    h.get_by_label("2 s");
    assert!(h.query_by_label("Preparing failed.").is_none());

    // Provisioning ends but fails: the wizard offers Retry.
    gates.lock().unwrap().clear();
    wait_for(&mut h, "Preparing failed.");
    assert_eq!(page(&h), Some(Page::Prepare));
    let probe = probes.lock().unwrap()[0].clone();
    assert_eq!(probe.provisions(), 1);
    click(&mut h, "Retry");
    // The second run fails at once (the gate is gone); wait until it was made and ended.
    let start = Instant::now();
    while probe.provisions() < 2 || h.state().provisioning() {
        assert!(start.elapsed() < Duration::from_secs(10), "no second run");
        std::thread::sleep(Duration::from_millis(5));
        h.run_steps(1);
    }
    h.run_steps(2);
    wait_for(&mut h, "Preparing failed.");
    assert_eq!(probe.provisions(), 2);
    assert_eq!(page(&h), Some(Page::Prepare));
    click(&mut h, "Cancel");
    assert_eq!(page(&h), None);
}

#[test]
fn prepare_succeeds_into_done() {
    let dir = TempDir::new("evo-gui-wizard-prepared", true);
    let path = write_bundle(&dir, "os.84b2", SUPPORTED_OS);
    let platform = FakePlatform::shared();
    platform.set_backend_factory(Box::new(|_, _| {
        Ok(Box::new(FakeBackend::new().named("fresh").needing_provision()) as Box<dyn EmuBackend>)
    }));
    let mut h = harness_with(FakeBackend::new(), options(&dir, &platform, true));
    click(&mut h, "Next");
    platform.queue_open(Some(path));
    click(&mut h, "Browse\u{2026}");
    click(&mut h, "Next");
    wait_for(&mut h, "The calculator is ready.");
    assert_eq!(page(&h), Some(Page::Done));
    assert!(!h.state().backend().needs_provision());
    assert_eq!(h.state().backend().name(), "fresh");
}

#[test]
fn drop_two_files_is_refused() {
    let dir = TempDir::new("evo-gui-wizard-drop2", true);
    let a = write_bundle(&dir, "a.84b2", SUPPORTED_OS);
    let b = write_bundle(&dir, "b.84b2", SUPPORTED_OS);
    let platform = FakePlatform::shared();
    let mut h = harness_with(FakeBackend::new(), options(&dir, &platform, true));
    click(&mut h, "Next");
    h.input_mut().dropped_files = vec![dropped(a), dropped(b)];
    h.run_steps(1);
    h.run_steps(1);
    h.get_by_label("Drop one file at a time");
    click(&mut h, "Next");
    assert_eq!(page(&h), Some(Page::Choose));
    assert!(platform.opened().is_empty());
}

#[test]
fn drop_one_file_checks_it() {
    let dir = TempDir::new("evo-gui-wizard-drop1", true);
    let a = write_bundle(&dir, "a.84b2", SUPPORTED_OS);
    let platform = FakePlatform::shared();
    let mut h = harness_with(FakeBackend::new(), options(&dir, &platform, true));
    click(&mut h, "Next");
    h.input_mut().dropped_files = vec![dropped(a.clone())];
    h.run_steps(2);
    h.get_by_label("OS 7.1.0.4421 \u{b7} 84US");
    click(&mut h, "Next");
    h.run_steps(2);
    assert_eq!(platform.opened(), [a]);
    assert_eq!(h.state().backend().name(), "a.84b2");
}

#[test]
fn cancel_keeps_backend() {
    let dir = TempDir::new("evo-gui-wizard-cancel", true);
    let path = write_bundle(&dir, "os.84b2", SUPPORTED_OS);
    let platform = FakePlatform::shared();
    let mut h = harness_with(FakeBackend::new(), options(&dir, &platform, true));
    click(&mut h, "Next");
    platform.queue_open(Some(path));
    click(&mut h, "Browse\u{2026}");
    click(&mut h, "Cancel");
    assert_eq!(page(&h), None);
    assert!(h.query_by_label("ROM Setup Wizard").is_none());
    assert!(platform.opened().is_empty());
    assert_eq!(h.state().backend().name(), "demo");
    assert_eq!(h.state().prefs().bundle, None);
}

#[test]
fn change_rom_opens_choose() {
    let dir = TempDir::new("evo-gui-wizard-change", true);
    let platform = FakePlatform::shared();
    let mut h = harness_with(FakeBackend::new(), options(&dir, &platform, false));
    assert_eq!(page(&h), None);
    open_action(&mut h, Action::ChangeRom);
    assert_eq!(page(&h), Some(Page::Choose));
    h.get_by_label("Choose an OS bundle");
    assert!(h.state().features().active(Action::ChangeRom));
}

#[test]
fn reload_rom_reopens_same_path() {
    let dir = TempDir::new("evo-gui-wizard-reload", true);
    let path = write_bundle(&dir, "os.84b2", SUPPORTED_OS);
    let platform = FakePlatform::shared();
    let fake = FakeBackend::new().named("os.84b2");
    let old = fake.probe();
    let opts = AppOptions {
        rom: Some(path.clone()),
        ..options(&dir, &platform, false)
    };
    let mut h = harness_with(fake, opts);
    open_action(&mut h, Action::ReloadRom);
    assert_eq!(platform.opened(), std::slice::from_ref(&path));
    assert_eq!(h.state().rom(), Some(path.as_path()));
    assert_eq!(h.state().backend().name(), "os.84b2");
    assert!(old.persists() >= 1, "the old flash is saved first");
    assert_eq!(page(&h), None);
    h.get_by_label("ROM reloaded");
}

#[test]
fn reload_failure_toasts_and_keeps_backend() {
    let dir = TempDir::new("evo-gui-wizard-reloadfail", true);
    let path = write_bundle(&dir, "os.84b2", SUPPORTED_OS);
    let platform = FakePlatform::shared();
    platform.set_backend_factory(Box::new(|_, _| Err("gone".to_owned())));
    let opts = AppOptions {
        rom: Some(path.clone()),
        ..options(&dir, &platform, false)
    };
    let mut h = harness_with(FakeBackend::new(), opts);
    open_action(&mut h, Action::ReloadRom);
    h.get_by_label("Reload failed: gone");
    assert_eq!(h.state().backend().name(), "demo");
    assert_eq!(h.state().rom(), Some(path.as_path()));
}

#[test]
fn reload_without_rom_toasts() {
    let dir = TempDir::new("evo-gui-wizard-reloadnone", true);
    let platform = FakePlatform::shared();
    let mut h = harness_with(FakeBackend::new(), options(&dir, &platform, false));
    open_action(&mut h, Action::ReloadRom);
    h.get_by_label("No ROM to reload");
    assert!(platform.opened().is_empty());
    assert_eq!(h.state().backend().name(), "demo");
}
