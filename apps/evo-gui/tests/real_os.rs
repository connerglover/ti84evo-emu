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

use std::path::PathBuf;
use std::time::Duration;

use common::{TempDir, test_options};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::EvoApp;
use evo_gui::backend::machine::MachineBackend;
use evo_gui::backend::{CLOCK_HZ, EmuBackend};
use evo_gui::platform::FakePlatform;

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
    let mut blue = 0;
    while blue < 1000 {
        assert!(
            h.state().backend().cycles() < 5_000_000_000,
            "no launcher: {blue} blue pixels at cycle {}",
            h.state().backend().cycles()
        );
        run_guest(&mut h, guest_ms(100));
        blue = blue_pixels(&mut h).unwrap_or(0);
    }
    run_guest(&mut h, guest_ms(500));
    let blue = blue_pixels(&mut h).unwrap_or(0);
    println!(
        "launcher at cycle {}: {blue} selection-blue pixels",
        h.state().backend().cycles()
    );
    assert!(blue >= 1000, "{blue}");
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
