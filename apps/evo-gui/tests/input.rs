use std::time::Duration;

use evo_gui::backend::fake::{FAKE_PC, FakeBackend};
use evo_gui::backend::{CLOCK_HZ, EmuBackend, RunStatus};
use evo_gui::input::{KeyLatch, MIN_HOLD_CYCLES, Source};
use evo_gui::keypad::KeyId;
use evo_gui::pacer::{
    CHUNK_CYCLES, FrameRunner, MIN_CHUNK_CYCLES, Pacer, SpeedMeter, WALL_BUDGET, speed_percent,
};

// ---------------------------------------------------------------- latch

#[test]
fn min_hold_is_50_ms() {
    assert_eq!(MIN_HOLD_CYCLES, CLOCK_HZ * 50 / 1000);
}

#[test]
fn tap_is_held_for_min_hold() {
    let mut latch = KeyLatch::new();
    latch.press(KeyId::Enter, Source::Pointer, 0);
    latch.release(KeyId::Enter, Source::Pointer, 0);
    assert_eq!(latch.take_edges(), vec![(KeyId::Enter, true)]);
    assert!(latch.is_down(KeyId::Enter));
    latch.tick(MIN_HOLD_CYCLES - 1);
    assert_eq!(latch.take_edges(), vec![]);
    assert!(latch.is_down(KeyId::Enter));
    latch.tick(MIN_HOLD_CYCLES);
    assert_eq!(latch.take_edges(), vec![(KeyId::Enter, false)]);
    assert!(!latch.is_down(KeyId::Enter));
}

#[test]
fn long_press_releases_immediately() {
    let mut latch = KeyLatch::new();
    latch.press(KeyId::Enter, Source::Pointer, 0);
    assert_eq!(latch.take_edges(), vec![(KeyId::Enter, true)]);
    latch.release(KeyId::Enter, Source::Pointer, 10_000_000);
    assert_eq!(latch.take_edges(), vec![(KeyId::Enter, false)]);
    assert!(!latch.is_down(KeyId::Enter));
}

#[test]
fn two_sources_keep_key_down() {
    let mut latch = KeyLatch::new();
    latch.press(KeyId::Enter, Source::Pointer, 0);
    latch.press(KeyId::Enter, Source::Keyboard, 10);
    assert_eq!(latch.take_edges(), vec![(KeyId::Enter, true)]);
    latch.release(KeyId::Enter, Source::Pointer, MIN_HOLD_CYCLES + 1);
    latch.tick(MIN_HOLD_CYCLES + 1);
    assert_eq!(latch.take_edges(), vec![]);
    assert!(latch.is_down(KeyId::Enter));
    latch.release(KeyId::Enter, Source::Keyboard, MIN_HOLD_CYCLES + 2);
    assert_eq!(latch.take_edges(), vec![(KeyId::Enter, false)]);
    assert!(!latch.is_down(KeyId::Enter));
}

#[test]
fn repress_during_pending_release_extends() {
    let mut latch = KeyLatch::new();
    latch.press(KeyId::Enter, Source::Pointer, 0);
    latch.release(KeyId::Enter, Source::Pointer, 0);
    assert_eq!(latch.take_edges(), vec![(KeyId::Enter, true)]);
    latch.press(KeyId::Enter, Source::Pointer, 1_000);
    assert_eq!(latch.take_edges(), vec![]);
    latch.tick(MIN_HOLD_CYCLES);
    assert_eq!(latch.take_edges(), vec![]);
    assert!(latch.is_down(KeyId::Enter));
    latch.release(KeyId::Enter, Source::Pointer, MIN_HOLD_CYCLES + 5);
    assert_eq!(latch.take_edges(), vec![(KeyId::Enter, false)]);
}

#[test]
fn release_all_releases_everything() {
    let mut latch = KeyLatch::new();
    latch.press(KeyId::Two, Source::Keyboard, 0);
    latch.press(KeyId::Sin, Source::Pointer, 0);
    latch.take_edges();
    latch.release_all(MIN_HOLD_CYCLES);
    assert_eq!(
        latch.take_edges(),
        vec![(KeyId::Sin, false), (KeyId::Two, false)]
    );
    assert!(!latch.is_down(KeyId::Sin));
    assert!(!latch.is_down(KeyId::Two));
}

#[test]
fn clear_forgets_without_edges() {
    let mut latch = KeyLatch::new();
    latch.press(KeyId::Enter, Source::Pointer, 0);
    latch.release(KeyId::Enter, Source::Pointer, 0);
    latch.take_edges();
    latch.press(KeyId::Sin, Source::Keyboard, 0);
    latch.clear();
    latch.tick(u64::MAX);
    assert_eq!(latch.take_edges(), vec![]);
    assert!(!latch.is_down(KeyId::Enter));
    assert!(!latch.is_down(KeyId::Sin));
}

#[test]
fn release_of_unpressed_key_is_ignored() {
    let mut latch = KeyLatch::new();
    latch.release(KeyId::Enter, Source::Pointer, 0);
    latch.tick(u64::MAX);
    assert_eq!(latch.take_edges(), vec![]);
    // A release from a source that never pressed is also ignored.
    latch.press(KeyId::Enter, Source::Keyboard, 0);
    latch.release(KeyId::Enter, Source::Pointer, MIN_HOLD_CYCLES);
    latch.tick(u64::MAX);
    assert_eq!(latch.take_edges(), vec![(KeyId::Enter, true)]);
    assert!(latch.is_down(KeyId::Enter));
}

// ---------------------------------------------------------------- pacer

#[test]
fn budget_caps_long_frames() {
    let p = Pacer {
        clock_hz: CLOCK_HZ,
        speed: 1.0,
    };
    assert_eq!(p.budget(5.0), 15_600_000);
    assert_eq!(p.budget(1.0 / 60.0), 2_600_000);
    assert_eq!(p.budget(-1.0), 0);
    assert_eq!(p.budget(f64::NAN), 0);
}

#[test]
fn run_frame_uses_whole_budget() {
    let mut fake = FakeBackend::new();
    let mut runner = FrameRunner::new();
    let mut elapsed = || Duration::ZERO;
    let run = runner.run(&mut fake, 2_600_000, WALL_BUDGET, &mut elapsed);
    assert_eq!(run.cycles, 2_600_000);
    assert_eq!(run.status, RunStatus::Running);
    assert_eq!(fake.cycles(), 2_600_000);
}

#[test]
fn run_frame_stops_at_wall_budget() {
    let mut fake = FakeBackend::new();
    let mut runner = FrameRunner::new();
    let mut calls = 0u32;
    let mut elapsed = || {
        calls += 1;
        Duration::from_millis(if calls == 1 { 0 } else { 13 })
    };
    let run = runner.run(&mut fake, 2_600_000, WALL_BUDGET, &mut elapsed);
    assert_eq!(run.cycles, 2 * CHUNK_CYCLES);
    assert_eq!(run.status, RunStatus::Running);
    assert_eq!(calls, 2);
}

#[test]
fn slow_chunk_shrinks_the_next() {
    let mut fake = FakeBackend::new();
    let mut runner = FrameRunner::new();
    assert_eq!(runner.chunk(), CHUNK_CYCLES);
    let mut slow = || Duration::from_millis(20);
    let run = runner.run(&mut fake, 2_600_000, WALL_BUDGET, &mut slow);
    assert_eq!(run.cycles, CHUNK_CYCLES);
    assert_eq!(runner.chunk(), 100_000);

    for _ in 0..10 {
        let mut fast = || Duration::ZERO;
        runner.run(&mut fake, 2_600_000, WALL_BUDGET, &mut fast);
    }
    assert_eq!(runner.chunk(), CHUNK_CYCLES);

    for _ in 0..20 {
        let mut slow = || Duration::from_millis(20);
        runner.run(&mut fake, 2_600_000, WALL_BUDGET, &mut slow);
        assert!(runner.chunk() >= MIN_CHUNK_CYCLES);
    }
    assert_eq!(runner.chunk(), MIN_CHUNK_CYCLES);
}

#[test]
fn elapsed_is_not_called_before_the_first_chunk() {
    let mut fake = FakeBackend::new();
    let probe = fake.probe();
    let mut runner = FrameRunner::new();
    let mut elapsed = || {
        assert_eq!(probe.run_calls(), 1, "elapsed called before a chunk ran");
        Duration::from_millis(13)
    };
    let run = runner.run(&mut fake, 2_600_000, WALL_BUDGET, &mut elapsed);
    assert_eq!(run.cycles, CHUNK_CYCLES);
}

#[test]
fn run_frame_returns_on_idle() {
    let mut fake = FakeBackend::idle();
    let probe = fake.probe();
    let mut runner = FrameRunner::new();
    let mut elapsed = || Duration::ZERO;
    let run = runner.run(&mut fake, 2_600_000, WALL_BUDGET, &mut elapsed);
    assert_eq!(run.status, RunStatus::Idle);
    assert_eq!(run.cycles, 0);
    assert_eq!(probe.run_calls(), 1);
}

#[test]
fn run_frame_stops_on_breakpoint() {
    let mut fake = FakeBackend::new();
    fake.toggle_breakpoint(FAKE_PC);
    let probe = fake.probe();
    let mut runner = FrameRunner::new();
    let mut elapsed = || Duration::ZERO;
    let run = runner.run(&mut fake, 2_600_000, WALL_BUDGET, &mut elapsed);
    assert_eq!(run.status, RunStatus::Breakpoint(FAKE_PC));
    assert_eq!(run.cycles, 0);
    assert_eq!(probe.run_calls(), 1);
}

#[test]
fn speed_percent_cases() {
    assert_eq!(speed_percent(156_000_000, 1.0, CLOCK_HZ), Some(100));
    assert_eq!(speed_percent(78_000_000, 1.0, CLOCK_HZ), Some(50));
    assert_eq!(speed_percent(0, 0.0, CLOCK_HZ), None);
    assert_eq!(speed_percent(1, f64::NAN, CLOCK_HZ), None);
}

#[test]
fn speed_meter_smooths_over_a_second() {
    let mut meter = SpeedMeter::default();
    meter.push(78_000_000, 0.5);
    assert_eq!(meter.percent(), None);
    meter.push(78_000_000, 0.5);
    assert_eq!(meter.percent(), Some(100));
    meter.push(39_000_000, 0.5);
    assert_eq!(meter.percent(), Some(100));
    meter.reset();
    assert_eq!(meter.percent(), None);
}
