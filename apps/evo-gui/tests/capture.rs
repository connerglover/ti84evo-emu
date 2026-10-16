//! Lane C capture: the LCD frame sampler, Copy screen and animated PNG recording.

mod common;

use std::path::{Path, PathBuf};

use common::{TempDir, harness_with, open_action, test_options};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp};
use evo_gui::backend::fake::FakeBackend;
use evo_gui::backend::{CLOCK_HZ, Frame};
use evo_gui::capture::{
    APNG_MAX_FRAMES, ApngRecorder, CaptureError, FrameSampler, LCD_FRAME_CYCLES, RecordKind,
};
use evo_gui::feature::Feature;
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

// ---- Task C2: animated PNG recording ----

/// A 4×2 frame filled with grey `v` (opaque).
fn solid(v: u8) -> Frame {
    let mut rgba = Vec::new();
    for _ in 0..8 {
        rgba.extend_from_slice(&[v, v, v, 0xFF]);
    }
    Frame {
        width: 4,
        height: 2,
        rgba,
    }
}

/// Decodes an APNG: its frame count from `acTL` and each frame's delay `(num, den)`.
fn decode_apng(path: &Path) -> (u32, Vec<(u16, u16)>) {
    let bytes = std::fs::read(path).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let n = reader.info().animation_control.unwrap().num_frames;
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let mut delays = Vec::new();
    for _ in 0..n {
        reader.next_frame(&mut buf).unwrap();
        let fc = reader.info().frame_control.unwrap();
        delays.push((fc.delay_num, fc.delay_den));
    }
    (n, delays)
}

/// The files under `{dir}/recordings/`.
fn recordings(dir: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(dir.join("recordings")) {
        Ok(rd) => rd.map(|e| e.unwrap().path()).collect(),
        Err(_) => Vec::new(),
    }
}

/// True when some text label starts with `prefix` (toasts that end in an OS error). egui
/// keeps a label's text in the accesskit `value`.
fn has_label_starting(h: &Harness<'static, EvoApp>, prefix: &str) -> bool {
    h.query_all_by(|n| n.value().is_some_and(|v| v.starts_with(prefix)))
        .next()
        .is_some()
}

#[test]
fn apng_cap_is_thirty_seconds() {
    assert_eq!(APNG_MAX_FRAMES, 1800);
}

#[test]
fn apng_has_one_frame_per_distinct_screen() {
    let dir = TempDir::new("evo-cap-apng", true);
    let path = dir.join("a.png");
    let mut r = ApngRecorder::new(path.clone());
    r.push(&solid(1), 3).unwrap();
    r.push(&solid(1), 2).unwrap();
    r.push(&solid(2), 1).unwrap();
    assert_eq!(r.finish().unwrap(), Some(path.clone()));
    let (n, delays) = decode_apng(&path);
    assert_eq!(n, 2);
    assert_eq!(delays, vec![(5, 60), (1, 60)]);
}

#[test]
fn apng_long_frame_splits_its_delay() {
    let dir = TempDir::new("evo-cap-apng-long", true);
    let path = dir.join("a.png");
    let mut r = ApngRecorder::new(path.clone());
    r.push(&solid(1), 70_000).unwrap();
    r.push(&solid(2), 1).unwrap();
    r.finish().unwrap();
    let (n, delays) = decode_apng(&path);
    assert_eq!(n, 3);
    assert_eq!(delays, vec![(65_535, 60), (4_465, 60), (1, 60)]);
}

#[test]
fn apng_rejects_invalid_or_resized_frames() {
    let dir = TempDir::new("evo-cap-apng-bad", true);
    let mut r = ApngRecorder::new(dir.join("a.png"));
    let bad = Frame {
        width: 4,
        height: 2,
        rgba: vec![0; 3],
    };
    assert!(matches!(r.push(&bad, 1), Err(CaptureError::Encode(_))));
    r.push(&solid(1), 1).unwrap();
    let wide = Frame {
        width: 8,
        height: 2,
        rgba: vec![0xFF; 64],
    };
    assert!(matches!(r.push(&wide, 1), Err(CaptureError::Encode(_))));
    // The frames so far still make a valid file.
    let p = r.finish().unwrap().unwrap();
    assert_eq!(decode_apng(&p).0, 1);
}

#[test]
fn no_frames_writes_no_file() {
    let dir = TempDir::new("evo-cap-apng-none", true);
    let path = dir.join("a.png");
    let r = ApngRecorder::new(path.clone());
    assert_eq!(r.finish().unwrap(), None);
    assert!(!path.exists());
}

#[test]
fn apng_full_stops_and_keeps_frames() {
    let dir = TempDir::new("evo-cap-apng-full", true);
    let path = dir.join("a.png");
    let mut r = ApngRecorder::with_cap(path.clone(), 3);
    for v in 0..3 {
        r.push(&solid(v), 1).unwrap();
    }
    // An identical frame still fits: only distinct frames count.
    r.push(&solid(2), 1).unwrap();
    assert!(matches!(r.push(&solid(9), 1), Err(CaptureError::Full)));
    assert_eq!(r.finish().unwrap(), Some(path.clone()));
    let (n, delays) = decode_apng(&path);
    assert_eq!(n, 3);
    assert_eq!(delays[2], (2, 60));
}

#[test]
fn apng_write_error_stops_cleanly() {
    let dir = TempDir::new("evo-cap-apng-err", true);
    std::fs::write(dir.join("a"), b"not a dir").unwrap();
    let path = dir.join("a").join("x.png");
    let mut r = ApngRecorder::with_cap(path.clone(), 10);
    r.push(&solid(1), 1).unwrap();
    r.push(&solid(2), 1).unwrap();
    assert!(matches!(r.finish(), Err(CaptureError::Io(_))));
    assert!(!path.exists());
}

#[test]
fn apng_write_error_via_action_stops_with_toast() {
    let dir = TempDir::new("evo-cap-apng-err-act", true);
    let mut h = harness_with(
        FakeBackend::new(),
        test_options(&dir, FakePlatform::shared()),
    );
    open_action(&mut h, Action::RecordApng);
    h.run_steps(10);
    std::fs::remove_dir_all(dir.join("recordings")).unwrap();
    std::fs::write(dir.join("recordings"), b"in the way").unwrap();
    open_action(&mut h, Action::RecordApng);
    assert!(has_label_starting(&h, "Recording stopped: "));
    assert_eq!(h.state().features().capture.recording(), None);
    assert!(dir.join("recordings").is_file());
}

#[test]
fn record_apng_via_action() {
    let dir = TempDir::new("evo-cap-apng-act", true);
    let mut h = harness_with(
        FakeBackend::new(),
        test_options(&dir, FakePlatform::shared()),
    );
    open_action(&mut h, Action::RecordApng);
    assert!(h.query_by_label("Recording animated PNG…").is_some());
    let capture = &h.state().features().capture;
    assert_eq!(capture.recording(), Some(RecordKind::Apng));
    assert!(capture.active(Action::RecordApng));
    assert!(!capture.active(Action::RecordVideo));
    h.run_steps(30);
    open_action(&mut h, Action::RecordApng);
    assert_eq!(h.state().features().capture.recording(), None);
    let files = recordings(&dir);
    assert_eq!(files.len(), 1);
    let name = files[0].file_name().unwrap().to_str().unwrap().to_owned();
    assert!(name.starts_with("evo-") && name.ends_with(".png"), "{name}");
    let (n, _) = decode_apng(&files[0]);
    assert!(n >= 2, "{n} frames");
    let toast = format!("Recording saved to {}", files[0].display());
    assert!(h.query_by_label(&toast).is_some());
}

#[test]
fn paused_or_no_display_pushes_nothing() {
    let dir = TempDir::new("evo-cap-apng-paused", true);
    let opts = AppOptions {
        start_paused: true,
        ..test_options(&dir, FakePlatform::shared())
    };
    let mut h = harness_with(FakeBackend::new(), opts);
    open_action(&mut h, Action::RecordApng);
    h.run_steps(30);
    open_action(&mut h, Action::RecordApng);
    assert!(recordings(&dir).is_empty());
    assert!(h.query_by_label("Nothing was recorded").is_some());
    assert_eq!(h.state().features().capture.recording(), None);

    let dir = TempDir::new("evo-cap-apng-nodisp", true);
    let mut h = harness_with(
        FakeBackend::without_display(),
        test_options(&dir, FakePlatform::shared()),
    );
    open_action(&mut h, Action::RecordApng);
    h.run_steps(30);
    open_action(&mut h, Action::RecordApng);
    assert!(recordings(&dir).is_empty());
    assert!(h.query_by_label("Nothing was recorded").is_some());
}

#[test]
fn recording_into_unwritable_dir_stops_with_toast() {
    let dir = TempDir::new("evo-cap-apng-unwritable", true);
    std::fs::write(dir.join("recordings"), b"in the way").unwrap();
    let mut h = harness_with(
        FakeBackend::new(),
        test_options(&dir, FakePlatform::shared()),
    );
    open_action(&mut h, Action::RecordApng);
    assert!(has_label_starting(&h, "Recording failed: "));
    assert!(!has_label_starting(&h, "Recording animated PNG"));
    assert_eq!(h.state().features().capture.recording(), None);
    assert!(!h.state().features().capture.active(Action::RecordApng));
}

#[test]
fn on_exit_finishes_recording() {
    let dir = TempDir::new("evo-cap-apng-exit", true);
    let mut h = harness_with(
        FakeBackend::new(),
        test_options(&dir, FakePlatform::shared()),
    );
    open_action(&mut h, Action::RecordApng);
    h.run_steps(30);
    eframe::App::on_exit(h.state_mut());
    assert_eq!(h.state().features().capture.recording(), None);
    let files = recordings(&dir);
    assert_eq!(files.len(), 1);
    assert!(decode_apng(&files[0]).0 >= 1);
}
