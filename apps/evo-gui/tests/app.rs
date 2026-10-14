use egui::{Rect, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp, status_text};
use evo_gui::backend::fake::FakeBackend;
use evo_gui::backend::{Frame, RunStatus};
use evo_gui::lcd::{ScaleFilter, lcd_draw_rect};

fn data_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("evo-gui-tests-app")
}

fn harness(fake: FakeBackend, paused: bool) -> Harness<'static, EvoApp> {
    let opts = AppOptions {
        start_paused: paused,
        ..AppOptions::new(data_dir())
    };
    Harness::builder()
        .with_size([520.0, 960.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .build_eframe(|cc| EvoApp::new(&cc.egui_ctx, Box::new(fake), opts))
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
