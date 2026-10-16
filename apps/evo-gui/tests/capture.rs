//! Lane C capture: the LCD frame sampler and Copy screen.

mod common;

use common::{TempDir, harness_with, open_action, test_options};
use egui_kittest::kittest::Queryable;
use evo_gui::backend::fake::FakeBackend;
use evo_gui::backend::{CLOCK_HZ, Frame};
use evo_gui::capture::{FrameSampler, LCD_FRAME_CYCLES};
use evo_gui::platform::FakePlatform;
use evo_gui::toolbar::Action;

#[test]
fn lcd_frame_is_a_sixtieth_of_a_second() {
    assert_eq!(LCD_FRAME_CYCLES, CLOCK_HZ / 60);
    assert_eq!(LCD_FRAME_CYCLES, 2_600_000);
}

#[test]
fn sampler_counts_whole_ticks() {
    let mut s = FrameSampler::new(0);
    assert_eq!(s.ticks(2_599_999), 0);
    assert_eq!(s.ticks(2_600_000), 1);
    // The rest is kept: 13_000_000 is 5 frames from 0, one of which was already counted.
    assert_eq!(s.ticks(13_000_000), 4);
    assert_eq!(s.ticks(13_000_000), 0);
}

#[test]
fn sampler_keeps_the_rest_across_calls() {
    let mut s = FrameSampler::new(1_000);
    assert_eq!(s.ticks(1_000 + LCD_FRAME_CYCLES / 2), 0);
    assert_eq!(s.ticks(1_000 + LCD_FRAME_CYCLES), 1);
    assert_eq!(s.ticks(1_000 + 3 * LCD_FRAME_CYCLES - 1), 1);
    assert_eq!(s.ticks(1_000 + 3 * LCD_FRAME_CYCLES), 1);
}

#[test]
fn sampler_tolerates_time_going_back() {
    let mut s = FrameSampler::new(0);
    assert_eq!(s.ticks(10 * LCD_FRAME_CYCLES), 10);
    // A machine reset sets the cycle count back to 0.
    assert_eq!(s.ticks(0), 0);
    assert_eq!(s.ticks(LCD_FRAME_CYCLES - 1), 0);
    assert_eq!(s.ticks(LCD_FRAME_CYCLES), 1);
}

#[test]
fn sampler_saturates_on_huge_gaps() {
    let mut s = FrameSampler::new(0);
    assert_eq!(s.ticks(u64::MAX), u32::MAX);
}

#[test]
fn copy_screen_copies() {
    let dir = TempDir::new("evo-cap-copy", true);
    let platform = FakePlatform::shared();
    let mut h = harness_with(FakeBackend::new(), test_options(&dir, platform.clone()));
    open_action(&mut h, Action::CopyScreen);
    assert_eq!(platform.copied(), 1);
    assert!(h.query_by_label("Screen copied").is_some());
}

#[test]
fn copy_screen_failure_toasts() {
    let dir = TempDir::new("evo-cap-copyfail", true);
    let platform = FakePlatform::shared();
    platform.fail_copy("busy");
    let mut h = harness_with(FakeBackend::new(), test_options(&dir, platform.clone()));
    open_action(&mut h, Action::CopyScreen);
    assert_eq!(platform.copied(), 0);
    assert!(h.query_by_label("Copy failed: busy").is_some());
    assert!(h.query_by_label("Screen copied").is_none());
}

#[test]
fn copy_without_display_toasts() {
    let dir = TempDir::new("evo-cap-nodisp", true);
    let platform = FakePlatform::shared();
    let mut h = harness_with(
        FakeBackend::without_display(),
        test_options(&dir, platform.clone()),
    );
    open_action(&mut h, Action::CopyScreen);
    assert_eq!(platform.copied(), 0);
    assert!(
        h.query_by_label("Copy failed: no display frame to capture")
            .is_some()
    );
}

#[test]
fn copy_invalid_frame_toasts() {
    let dir = TempDir::new("evo-cap-invalid", true);
    let platform = FakePlatform::shared();
    let bad = Frame {
        width: 4,
        height: 4,
        rgba: vec![0; 3],
    };
    let mut h = harness_with(
        FakeBackend::with_frame(bad),
        test_options(&dir, platform.clone()),
    );
    open_action(&mut h, Action::CopyScreen);
    assert_eq!(platform.copied(), 0);
    assert!(
        h.query_by_label("Copy failed: no display frame to capture")
            .is_some()
    );
}

#[test]
fn capture_is_not_recording_at_start() {
    let dir = TempDir::new("evo-cap-idle", true);
    let h = harness_with(
        FakeBackend::new(),
        test_options(&dir, FakePlatform::shared()),
    );
    assert_eq!(h.state().features().capture.recording(), None);
}
