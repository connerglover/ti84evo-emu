//! Lane C capture: the LCD frame sampler, Copy screen, animated PNG and MP4 recording.

mod common;

use std::path::{Path, PathBuf};

use common::{TempDir, harness_with, open_action, test_options};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp};
use evo_gui::backend::fake::FakeBackend;
use evo_gui::backend::{CLOCK_HZ, Frame};
use evo_gui::capture::{
    APNG_MAX_FRAMES, ApngRecorder, CaptureError, FrameSampler, LCD_FRAME_CYCLES, Mp4Recorder,
    RecordKind, scale2x_rgb,
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

// ---- Task C3: MP4 recording ----

/// A 320×240 frame (the LCD size) with a pattern that depends on `v`.
fn lcd(v: u8) -> Frame {
    let (w, h) = (320u32, 240u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let r = (x as u8).wrapping_add(v.wrapping_mul(37));
            let g = (y as u8).wrapping_mul(2).wrapping_add(v);
            let b = if (x / 16 + y / 16 + u32::from(v)) % 2 == 0 {
                0x20
            } else {
                0xE0
            };
            rgba.extend_from_slice(&[r, g, b, 0xFF]);
        }
    }
    Frame {
        width: w,
        height: h,
        rgba,
    }
}

/// An MP4's only track: `(is H.264, width, height, samples as (duration, is_sync))`.
fn read_mp4(path: &Path) -> (bool, u16, u16, Vec<(u32, bool)>) {
    let file = std::fs::File::open(path).unwrap();
    let size = file.metadata().unwrap().len();
    let mut reader = mp4::Mp4Reader::read_header(std::io::BufReader::new(file), size).unwrap();
    assert_eq!(reader.tracks().len(), 1);
    let (&id, track) = reader.tracks().iter().next().unwrap();
    let h264 = track.media_type().unwrap() == mp4::MediaType::H264;
    let (w, h) = (track.width(), track.height());
    let n = reader.sample_count(id).unwrap();
    let mut samples = Vec::new();
    for i in 1..=n {
        let sample = reader.read_sample(id, i).unwrap().unwrap();
        assert!(!sample.bytes.is_empty());
        samples.push((sample.duration, sample.is_sync));
    }
    (h264, w, h, samples)
}

/// The sample durations of an MP4's only track.
fn durations(path: &Path) -> Vec<u32> {
    read_mp4(path).3.into_iter().map(|(d, _)| d).collect()
}

#[test]
fn scale2x_doubles_pixels() {
    let frame = Frame {
        width: 2,
        height: 1,
        rgba: vec![1, 2, 3, 0xFF, 4, 5, 6, 0x80],
    };
    let (w, h, rgb) = scale2x_rgb(&frame);
    assert_eq!((w, h), (4, 2));
    let row = [1, 2, 3, 1, 2, 3, 4, 5, 6, 4, 5, 6];
    assert_eq!(rgb, [row, row].concat());
}

#[test]
fn scale2x_of_the_lcd_is_640_by_480() {
    let (w, h, rgb) = scale2x_rgb(&lcd(0));
    assert_eq!((w, h), (640, 480));
    assert_eq!(rgb.len(), 640 * 480 * 3);
}

#[test]
fn mp4_is_readable() {
    let dir = TempDir::new("evo-cap-mp4", true);
    let path = dir.join("a.mp4");
    let mut r = Mp4Recorder::new(path.clone());
    for v in 0..3 {
        r.push(&lcd(v), 1).unwrap();
    }
    assert_eq!(r.finish().unwrap(), Some(path.clone()));
    let (h264, w, h, samples) = read_mp4(&path);
    assert!(h264);
    assert_eq!((w, h), (640, 480));
    assert_eq!(samples.len(), 3);
    assert!(samples.iter().all(|&(d, _)| d == 1));
    // The first sample is the IDR frame a player starts from.
    assert!(samples[0].1);
}

#[test]
fn mp4_decodes_back_to_the_screen() {
    let dir = TempDir::new("evo-cap-mp4-decode", true);
    let path = dir.join("a.mp4");
    let mut r = Mp4Recorder::new(path.clone());
    for v in 0..3 {
        r.push(&lcd(v), 1).unwrap();
    }
    r.finish().unwrap();

    let file = std::fs::File::open(&path).unwrap();
    let size = file.metadata().unwrap().len();
    let mut reader = mp4::Mp4Reader::read_header(std::io::BufReader::new(file), size).unwrap();
    let (&id, track) = reader.tracks().iter().next().unwrap();
    // AVCC (length-prefixed) back to Annex B (start codes) for the decoder.
    let start = [0u8, 0, 0, 1];
    let mut stream = Vec::new();
    for ps in [
        track.sequence_parameter_set().unwrap(),
        track.picture_parameter_set().unwrap(),
    ] {
        stream.extend_from_slice(&start);
        stream.extend_from_slice(ps);
    }
    let mut decoder = openh264::decoder::Decoder::new().unwrap();
    let mut decoded = 0;
    let mut last = Vec::new();
    for i in 1..=reader.sample_count(id).unwrap() {
        let sample = reader.read_sample(id, i).unwrap().unwrap();
        let mut bytes = &sample.bytes[..];
        while bytes.len() >= 4 {
            let len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
            stream.extend_from_slice(&start);
            stream.extend_from_slice(&bytes[4..4 + len]);
            bytes = &bytes[4 + len..];
        }
        assert!(bytes.is_empty(), "sample {i} is not AVCC");
        if let Some(yuv) = decoder.decode(&stream).unwrap() {
            use openh264::formats::YUVSource;
            assert_eq!(yuv.dimensions(), (640, 480));
            last = vec![0; 640 * 480 * 3];
            yuv.write_rgb8(&mut last);
            decoded += 1;
        }
        stream.clear();
    }
    assert_eq!(decoded, 3);
    // Lossy, but close to the last screen scaled ×2.
    let (_, _, want) = scale2x_rgb(&lcd(2));
    let diff: u64 = want
        .iter()
        .zip(&last)
        .map(|(&a, &b)| u64::from(a.abs_diff(b)))
        .sum();
    let mean = diff as f64 / want.len() as f64;
    assert!(mean < 16.0, "mean error {mean}");
}

#[test]
fn mp4_identical_frames_merge() {
    let dir = TempDir::new("evo-cap-mp4-merge", true);
    let path = dir.join("a.mp4");
    let mut r = Mp4Recorder::new(path.clone());
    r.push(&lcd(7), 2).unwrap();
    r.push(&lcd(7), 3).unwrap();
    r.finish().unwrap();
    assert_eq!(durations(&path), vec![5]);

    let path = dir.join("b.mp4");
    let mut r = Mp4Recorder::new(path.clone());
    r.push(&lcd(7), 2).unwrap();
    r.push(&lcd(7), 3).unwrap();
    r.push(&lcd(8), 1).unwrap();
    r.finish().unwrap();
    assert_eq!(durations(&path), vec![5, 1]);
}

#[test]
fn mp4_no_frames_writes_no_file() {
    let dir = TempDir::new("evo-cap-mp4-none", true);
    let path = dir.join("a.mp4");
    let r = Mp4Recorder::new(path.clone());
    assert_eq!(r.finish().unwrap(), None);
    assert!(!path.exists());
}

#[test]
fn mp4_rejects_invalid_or_resized_frames() {
    let dir = TempDir::new("evo-cap-mp4-bad", true);
    let mut r = Mp4Recorder::new(dir.join("a.mp4"));
    let bad = Frame {
        width: 4,
        height: 2,
        rgba: vec![0; 3],
    };
    assert!(matches!(r.push(&bad, 1), Err(CaptureError::Encode(_))));
    r.push(&lcd(1), 1).unwrap();
    assert!(matches!(r.push(&solid(1), 1), Err(CaptureError::Encode(_))));
    // The frames so far still make a valid file.
    let p = r.finish().unwrap().unwrap();
    assert_eq!(durations(&p), vec![1]);
}

#[test]
fn mp4_write_error_stops_cleanly() {
    let dir = TempDir::new("evo-cap-mp4-err", true);
    std::fs::write(dir.join("a"), b"not a dir").unwrap();
    let path = dir.join("a").join("x.mp4");
    let mut r = Mp4Recorder::new(path.clone());
    assert!(matches!(r.push(&lcd(1), 1), Err(CaptureError::Io(_))));
    assert!(matches!(r.push(&lcd(2), 1), Err(CaptureError::Io(_))));
    assert!(matches!(r.finish(), Err(CaptureError::Io(_))));
    assert!(!path.exists());
}

#[test]
fn record_video_via_action() {
    let dir = TempDir::new("evo-cap-mp4-act", true);
    let mut h = harness_with(
        FakeBackend::new(),
        test_options(&dir, FakePlatform::shared()),
    );
    open_action(&mut h, Action::RecordVideo);
    assert!(h.query_by_label("Recording MP4…").is_some());
    let capture = &h.state().features().capture;
    assert_eq!(capture.recording(), Some(RecordKind::Mp4));
    assert!(capture.active(Action::RecordVideo));
    assert!(!capture.active(Action::RecordApng));
    h.run_steps(30);
    open_action(&mut h, Action::RecordVideo);
    assert_eq!(h.state().features().capture.recording(), None);
    let files = recordings(&dir);
    assert_eq!(files.len(), 1);
    let name = files[0].file_name().unwrap().to_str().unwrap().to_owned();
    assert!(name.starts_with("evo-") && name.ends_with(".mp4"), "{name}");
    let (h264, w, hh, samples) = read_mp4(&files[0]);
    assert!(h264);
    assert_eq!((w, hh), (640, 480));
    assert!(samples.len() >= 2, "{} samples", samples.len());
    let toast = format!("Recording saved to {}", files[0].display());
    assert!(h.query_by_label(&toast).is_some());
}

#[test]
fn only_one_recording_at_a_time() {
    let dir = TempDir::new("evo-cap-one", true);
    let mut h = harness_with(
        FakeBackend::new(),
        test_options(&dir, FakePlatform::shared()),
    );
    open_action(&mut h, Action::RecordApng);
    open_action(&mut h, Action::RecordVideo);
    assert!(
        h.query_by_label("Stop the current recording first")
            .is_some()
    );
    assert!(!has_label_starting(&h, "Recording MP4"));
    assert_eq!(
        h.state().features().capture.recording(),
        Some(RecordKind::Apng)
    );
    open_action(&mut h, Action::RecordApng);
    assert_eq!(h.state().features().capture.recording(), None);

    let dir = TempDir::new("evo-cap-one-b", true);
    let mut h = harness_with(
        FakeBackend::new(),
        test_options(&dir, FakePlatform::shared()),
    );
    open_action(&mut h, Action::RecordVideo);
    open_action(&mut h, Action::RecordApng);
    assert!(
        h.query_by_label("Stop the current recording first")
            .is_some()
    );
    assert!(!has_label_starting(&h, "Recording animated PNG"));
    assert_eq!(
        h.state().features().capture.recording(),
        Some(RecordKind::Mp4)
    );
}

#[test]
fn mp4_paused_or_no_display_pushes_nothing() {
    let dir = TempDir::new("evo-cap-mp4-paused", true);
    let opts = AppOptions {
        start_paused: true,
        ..test_options(&dir, FakePlatform::shared())
    };
    let mut h = harness_with(FakeBackend::new(), opts);
    open_action(&mut h, Action::RecordVideo);
    h.run_steps(30);
    open_action(&mut h, Action::RecordVideo);
    assert!(recordings(&dir).is_empty());
    assert!(h.query_by_label("Nothing was recorded").is_some());

    let dir = TempDir::new("evo-cap-mp4-nodisp", true);
    let mut h = harness_with(
        FakeBackend::without_display(),
        test_options(&dir, FakePlatform::shared()),
    );
    open_action(&mut h, Action::RecordVideo);
    h.run_steps(30);
    open_action(&mut h, Action::RecordVideo);
    assert!(recordings(&dir).is_empty());
    assert!(h.query_by_label("Nothing was recorded").is_some());
}

#[test]
fn mp4_into_unwritable_dir_stops_with_toast() {
    let dir = TempDir::new("evo-cap-mp4-unwritable", true);
    std::fs::write(dir.join("recordings"), b"in the way").unwrap();
    let mut h = harness_with(
        FakeBackend::new(),
        test_options(&dir, FakePlatform::shared()),
    );
    open_action(&mut h, Action::RecordVideo);
    assert!(has_label_starting(&h, "Recording failed: "));
    assert!(!has_label_starting(&h, "Recording MP4"));
    assert_eq!(h.state().features().capture.recording(), None);
    assert!(!h.state().features().capture.active(Action::RecordVideo));
}

#[test]
fn mp4_write_error_via_action_stops_with_toast() {
    let dir = TempDir::new("evo-cap-mp4-err-act", true);
    let opts = AppOptions {
        start_paused: true,
        ..test_options(&dir, FakePlatform::shared())
    };
    let mut h = harness_with(FakeBackend::new(), opts);
    open_action(&mut h, Action::RecordVideo);
    // The file is created at the first frame: make that fail, then let frames come.
    std::fs::remove_dir_all(dir.join("recordings")).unwrap();
    std::fs::write(dir.join("recordings"), b"in the way").unwrap();
    open_action(&mut h, Action::TogglePause);
    h.run_steps(10);
    assert!(has_label_starting(&h, "Recording stopped: "));
    assert!(h.query_by_label("Nothing was recorded").is_none());
    assert_eq!(h.state().features().capture.recording(), None);
    assert!(dir.join("recordings").is_file());
}

#[test]
fn mp4_on_exit_finishes_recording() {
    let dir = TempDir::new("evo-cap-mp4-exit", true);
    let mut h = harness_with(
        FakeBackend::new(),
        test_options(&dir, FakePlatform::shared()),
    );
    open_action(&mut h, Action::RecordVideo);
    h.run_steps(30);
    eframe::App::on_exit(h.state_mut());
    assert_eq!(h.state().features().capture.recording(), None);
    let files = recordings(&dir);
    assert_eq!(files.len(), 1);
    assert!(!read_mp4(&files[0]).3.is_empty());
}
