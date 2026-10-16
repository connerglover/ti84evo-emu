//! The GUI on the user's own OS bundle (`EVO_OS_IMAGE`; no TI code is part of the repository).
//! Each test runs billions of cycles, so they run in release only:
//!
//! ```text
//! cargo test --release -p evo-gui --features snapshots --test real_os -- --nocapture
//! ```
//!
//! Without `EVO_OS_IMAGE`, or in a debug build, each test prints a `skipping:` line and passes.
//! `real_os_home_screen_and_keypad` renders with wgpu, so it also needs `--features snapshots`;
//! it writes its screenshots to the temp dir only (they show TI's OS and are never committed).

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{TempDir, open_action, test_options};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp};
use evo_gui::backend::machine::MachineBackend;
use evo_gui::backend::placeholder::Placeholder;
use evo_gui::backend::{AccessDir, CLOCK_HZ, EmuBackend, RunStatus, WatchKinds, Watchpoint};
use evo_gui::platform::FakePlatform;
use evo_gui::toolbar::Action;
use evo_gui::tools::macros::{MACROS_TITLE, MacroResult, screen_sha256};
use evo_gui::wizard::Page;
use evo_gui::wizard::validate::SUPPORTED_OS;

/// The OS's LCD framebuffer (LCDUPBASE, `docs/hardware/lcdc.md`): 320×240 RGB565.
const FRAMEBUFFER: u32 = 0x2004_8000;

/// The selected tile's colour on the OS 7.1.0.4421 app launcher (RGB565 0x035E,
/// `docs/hardware/lcdc.md` "Colour order"); with red and blue swapped it would be orange.
const SELECTION_BLUE: [u8; 3] = [0, 105, 247];

/// The bundle, or `None` (printing why) when the test should skip.
fn bundle() -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        println!("skipping: real-OS GUI tests need a release build");
        return None;
    }
    let Some(path) = std::env::var_os("EVO_OS_IMAGE").filter(|p| !p.is_empty()) else {
        println!("skipping: EVO_OS_IMAGE is not set");
        return None;
    };
    Some(PathBuf::from(path))
}

/// A first run (no flash file) needs provisioning; afterwards the flash file is written and
/// the next open does not need it again.
#[test]
fn real_os_first_run_provisions_once() {
    let Some(path) = bundle() else {
        return;
    };
    let dir = TempDir::new("evo-gui-tests-real-os", true);
    let mut b = MachineBackend::open(&path, &dir).unwrap();
    assert!(b.needs_provision(), "a fresh flash needs provisioning");
    b.toggle_breakpoint(0x0000_1234);
    b.provision().expect("provisioning succeeds");
    assert!(!b.needs_provision());
    assert_eq!(
        b.breakpoints(),
        vec![0x0000_1234],
        "user breakpoints are kept"
    );
    b.persist().unwrap();
    assert!(b.nv_path().exists());
    let b2 = MachineBackend::open(&path, &dir).unwrap();
    assert!(!b2.needs_provision(), "the saved flash is provisioned");
}

/// The selection-blue pixels of the current LCD frame (`None` while the display is off).
fn blue_pixels(h: &mut Harness<'static, EvoApp>) -> Option<usize> {
    let frame = h.state_mut().backend_mut().framebuffer()?;
    Some(
        frame
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[..3] == SELECTION_BLUE)
            .count(),
    )
}

fn lcd_rgba(h: &mut Harness<'static, EvoApp>) -> Vec<u8> {
    h.state_mut()
        .backend_mut()
        .framebuffer()
        .expect("the LCD is on")
        .rgba
        .clone()
}

/// Steps the app until `cycles` more guest cycles have run.
fn run_guest(h: &mut Harness<'static, EvoApp>, cycles: u64) {
    let end = h.state().backend().cycles() + cycles;
    while h.state().backend().cycles() < end {
        assert!(
            !h.state().paused(),
            "emulation stopped: {:?}",
            h.state().status()
        );
        h.run_steps(1);
    }
}

/// Runs the app until the OS app launcher shows its blue selection, then lets it settle.
fn boot_to_launcher(h: &mut Harness<'static, EvoApp>) {
    let mut blue = 0;
    while blue < 1000 {
        assert!(
            h.state().backend().cycles() < 5_000_000_000,
            "no launcher: {blue} blue pixels at cycle {}",
            h.state().backend().cycles()
        );
        run_guest(h, guest_ms(100));
        blue = blue_pixels(h).unwrap_or(0);
    }
    run_guest(h, guest_ms(500));
    let blue = blue_pixels(h).unwrap_or(0);
    println!(
        "launcher at cycle {}: {blue} selection-blue pixels",
        h.state().backend().cycles()
    );
    assert!(blue >= 1000, "{blue}");
}

fn guest_ms(ms: u64) -> u64 {
    CLOCK_HZ / 1000 * ms
}

/// Clicks the keypad key labelled `label` and lets the OS handle it.
fn tap(h: &mut Harness<'static, EvoApp>, label: &str) {
    h.get_by_label(label).click();
    h.run_steps(1);
    run_guest(h, guest_ms(300));
}

fn save_png(h: &mut Harness<'static, EvoApp>, name: &str) -> PathBuf {
    let img = h.render().expect("render");
    let path = std::env::temp_dir().join(format!("evo-gui-real-os-{name}.png"));
    img.save(&path).unwrap();
    println!("screenshot: {}", path.display());
    path
}

/// First run in the GUI: the app provisions the fresh unit by itself and the OS launcher
/// comes up with its blue selection; clicking the keypad opens the calculator and computes
/// 2 + 3 (the screenshots show the 5; the test checks that each step changes the screen).
#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "renders with wgpu: cargo test --release -p evo-gui --features snapshots"
)]
fn real_os_home_screen_and_keypad() {
    let Some(path) = bundle() else {
        return;
    };
    let nv = TempDir::new("evo-gui-tests-real-os", true);
    let data = TempDir::new("evo-gui-tests-real-os-data", true);
    let backend = MachineBackend::open(&path, &nv).unwrap();
    assert!(backend.needs_provision());
    let opts = test_options(&data, FakePlatform::shared());
    let mut h = Harness::builder()
        .with_size([520.0, 960.0])
        .with_theme(egui::Theme::Light)
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .build_eframe(|cc| EvoApp::new(&cc.egui_ctx, Box::new(backend), opts));

    // Provisioning runs on its own thread while the UI keeps drawing.
    h.run_steps(2);
    assert!(h.state().provisioning());
    let start = std::time::Instant::now();
    while h.state().provisioning() {
        assert!(
            start.elapsed() < Duration::from_secs(600),
            "provisioning hangs"
        );
        std::thread::sleep(Duration::from_millis(50));
        h.run_steps(1);
    }
    h.get_by_label("Calculator ready");
    assert!(!h.state().backend().needs_provision());

    // The OS boots to the app launcher: the selected tile is blue.
    boot_to_launcher(&mut h);
    save_png(&mut h, "home");
    let home = lcd_rgba(&mut h);

    // ENTER opens the first app, the calculator.
    tap(&mut h, "enter");
    run_guest(&mut h, guest_ms(700));
    let calc = lcd_rgba(&mut h);
    assert_ne!(calc, home, "ENTER did not open the calculator");
    save_png(&mut h, "calculator");

    // 2 + 3 ENTER.
    for key in ["2", "+", "3"] {
        tap(&mut h, key);
    }
    let typed = lcd_rgba(&mut h);
    assert_ne!(typed, calc, "typing changed nothing");
    save_png(&mut h, "typed");
    tap(&mut h, "enter");
    run_guest(&mut h, guest_ms(500));
    let result = lcd_rgba(&mut h);
    assert_ne!(result, typed, "ENTER changed nothing");
    save_png(&mut h, "result");
}

/// A real-OS backend in `dir`, provisioned (as the app's first run does on its worker).
fn provisioned(path: &Path, dir: &Path) -> MachineBackend {
    let mut b = MachineBackend::open(path, dir).unwrap();
    b.provision().expect("provisioning succeeds");
    b
}

/// The app on `backend` with `opts`: 520×960, 1 pixel per point, 60 frames per second.
fn app(backend: Box<dyn EmuBackend>, opts: AppOptions) -> Harness<'static, EvoApp> {
    Harness::builder()
        .with_size([520.0, 960.0])
        .with_theme(egui::Theme::Light)
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .build_eframe(|cc| EvoApp::new(&cc.egui_ctx, backend, opts))
}

fn click(h: &mut Harness<'static, EvoApp>, label: &str) {
    h.get_by_label(label).click();
    h.run_steps(2);
}

/// First launch without a ROM: the ROM Setup Wizard takes the bundle through Browse, opens
/// it with the instance's flash dir, provisions it on Prepare and ends on Done; after Finish
/// the OS boots to its launcher.
#[test]
fn real_os_wizard_provisions_to_home_screen() {
    let Some(path) = bundle() else {
        return;
    };
    let data = TempDir::new("evo-gui-tests-real-os-wizard", true);
    let platform = FakePlatform::shared();
    platform.set_backend_factory(Box::new(|bundle: &Path, nv_dir: &Path| {
        MachineBackend::open(bundle, nv_dir)
            .map(|b| Box::new(b) as Box<dyn EmuBackend>)
            .map_err(|e| e.to_string())
    }));
    let opts = AppOptions {
        open_wizard: true,
        ..test_options(&data, platform.clone())
    };
    let mut h = app(Box::new(Placeholder::no_rom()), opts);
    h.run_steps(2);
    assert_eq!(h.state().features().rom.page(), Some(Page::Welcome));
    click(&mut h, "Next");
    assert_eq!(h.state().features().rom.page(), Some(Page::Choose));
    platform.queue_open(Some(path.clone()));
    click(&mut h, "Browse\u{2026}");
    // The bundle passed the check: its version line is shown and no warning or error.
    h.get_by_label(&format!("OS {SUPPORTED_OS} \u{b7} 84US"));
    click(&mut h, "Next");
    assert_eq!(platform.opened(), std::slice::from_ref(&path));
    assert_eq!(h.state().rom(), Some(path.as_path()));

    let start = Instant::now();
    while h.state().features().rom.page() != Some(Page::Done) {
        assert_eq!(
            h.state().features().rom.page(),
            Some(Page::Prepare),
            "the wizard left Prepare"
        );
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "provisioning did not finish in 120 s"
        );
        std::thread::sleep(Duration::from_millis(50));
        h.run_steps(1);
    }
    println!("provisioned in {:.1} s", start.elapsed().as_secs_f32());
    h.run_steps(2);
    h.get_by_label("The calculator is ready.");
    click(&mut h, "Finish");
    assert_eq!(h.state().features().rom.page(), None);
    assert!(!h.state().backend().needs_provision());
    boot_to_launcher(&mut h);
}

/// The OS draws into the LCD framebuffer, so a write watchpoint over it stops the run with
/// the written address inside the range.
#[test]
fn real_os_framebuffer_watchpoint_stops() {
    let Some(path) = bundle() else {
        return;
    };
    let dir = TempDir::new("evo-gui-tests-real-os", true);
    let mut b = provisioned(&path, &dir);
    let w = Watchpoint {
        start: FRAMEBUFFER,
        len: 320 * 240 * 2,
        kinds: WatchKinds {
            read: false,
            write: true,
            exec: false,
        },
    };
    b.set_watchpoints(vec![w]).unwrap();
    let start = b.cycles();
    loop {
        assert!(
            b.cycles() - start < 3_000_000_000,
            "no framebuffer write in 3 G cycles"
        );
        match b.run_for(10_000_000) {
            RunStatus::Watchpoint { addr, dir, pc, .. } => {
                println!(
                    "framebuffer write at {addr:#010x} by pc {pc:#010x}, cycle {}",
                    b.cycles()
                );
                assert_eq!(dir, AccessDir::Write);
                assert!(
                    (w.start..w.start + w.len).contains(&addr),
                    "{addr:#010x} is outside the framebuffer"
                );
                break;
            }
            RunStatus::Running | RunStatus::Idle => {}
            other => panic!("unexpected stop: {other:?}"),
        }
    }
}

/// Plays the Macros window's script from the paused machine and waits for its result.
fn play_macro(h: &mut Harness<'static, EvoApp>) -> MacroResult {
    assert!(h.state().paused());
    click(h, "Play");
    let start = Instant::now();
    while h.state().features().tools.macros().is_playing() {
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "the macro did not finish"
        );
        h.run_steps(1);
    }
    h.run_steps(2);
    let result = h
        .state()
        .features()
        .tools
        .macros()
        .result()
        .cloned()
        .expect("a result");
    h.get_by_label(&result.label());
    result
}

/// AutoTester on the real OS: "2 add 3 enter" in the calculator, played from a saved state,
/// yields a screen hash; played again from the same state with that hash expected, it passes
/// (playback lands its keys at the same cycles, so the screen is the same).
#[test]
fn real_os_macro_screen_check_passes() {
    let Some(path) = bundle() else {
        return;
    };
    let nv = TempDir::new("evo-gui-tests-real-os", true);
    let data = TempDir::new("evo-gui-tests-real-os-data", true);
    let backend = provisioned(&path, &nv);
    let mut h = app(
        Box::new(backend),
        test_options(&data, FakePlatform::shared()),
    );
    h.run_steps(1);
    boot_to_launcher(&mut h);
    // ENTER opens the first app, the calculator, where the macro types.
    tap(&mut h, "enter");
    run_guest(&mut h, guest_ms(700));

    h.state_mut().set_paused(true);
    h.run_steps(1);
    let state = h.state().backend().save_state().unwrap();
    let saved_at = h.state().backend().cycles();
    let before = screen_sha256(
        h.state_mut()
            .backend_mut()
            .framebuffer()
            .expect("the LCD is on"),
    );
    open_action(&mut h, Action::Macros);
    h.get_by_label(MACROS_TITLE);
    h.state_mut()
        .features_mut()
        .tools
        .macros_mut()
        .set_script("2 add 3 enter");
    // "Check screen at end" with no hash yet: the FAIL result shows the screen's hash.
    h.state_mut()
        .features_mut()
        .tools
        .macros_mut()
        .set_check(true, "");
    h.run_steps(1);
    let MacroResult::Fail { got } = play_macro(&mut h) else {
        panic!("an empty expected hash matches no screen");
    };
    println!("screen sha256 after the macro: {got}");
    assert_ne!(got, before, "the macro's keys changed nothing on screen");

    h.state_mut().set_paused(true);
    h.run_steps(1);
    h.state_mut().backend_mut().load_state(&state).unwrap();
    assert_eq!(h.state().backend().cycles(), saved_at);
    h.state_mut()
        .features_mut()
        .tools
        .macros_mut()
        .set_check(true, &got);
    h.run_steps(1);
    assert_eq!(play_macro(&mut h), MacroResult::Pass);
    h.get_by_label("Result: PASS");
}
