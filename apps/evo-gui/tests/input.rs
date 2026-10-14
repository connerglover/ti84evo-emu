use std::time::Duration;

use egui::{Event, Key, Modifiers};
use evo_gui::backend::fake::{FAKE_PC, FakeBackend};
use evo_gui::backend::{CLOCK_HZ, EmuBackend, RunStatus};
use evo_gui::input::{
    KeyAction, KeyLatch, KeyboardTracker, Keymap, MIN_HOLD_CYCLES, PcKey, Source, pc_key_label,
};
use evo_gui::keypad::{KEYS, KeyId};
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

// ---------------------------------------------------------------- keymap

fn pk(key: Key, shift: bool) -> PcKey {
    PcKey { key, shift }
}

fn key_event(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers,
    }
}

fn down(key: Key) -> Event {
    key_event(key, true, Modifiers::NONE)
}

fn up(key: Key) -> Event {
    key_event(key, false, Modifiers::NONE)
}

#[test]
fn default_map_covers_essentials() {
    use KeyId::*;
    let map = Keymap::default_map();
    let essentials = [
        Zero, One, Two, Three, Four, Five, Six, Seven, Eight, Nine, Dot, Neg, Add, Sub, Mul, Div,
        Power, LParen, RParen, Comma, Enter, Del, Clear, Up, Down, Left, Right, Second, Alpha,
        Mode, YEqu, Window, Zoom, Trace, Graph, On, Math, Apps, Vars, Stat, Sto, XTThetaN, Sin,
        Cos, Tan, Log, Ln, Inverse, Prgm, Square,
    ];
    for id in essentials {
        assert!(!map.keys_for(id).is_empty(), "{id:?} has no PC binding");
    }
}

#[test]
fn letters_follow_alpha_legends() {
    let map = Keymap::default_map();
    let mut seen = 0;
    for def in &KEYS {
        let Some(legend) = def.alpha else { continue };
        let mut chars = legend.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            continue;
        };
        if !c.is_ascii_uppercase() {
            continue;
        }
        let key = Key::from_name(&c.to_string()).expect("egui has a key per letter");
        assert_eq!(map.lookup(pk(key, false)), Some(def.id), "letter {c}");
        seen += 1;
    }
    assert_eq!(seen, 26);
}

#[test]
fn no_pc_key_bound_twice() {
    let map = Keymap::default_map();
    let mut keys: Vec<PcKey> = map.bindings.iter().map(|(k, _)| *k).collect();
    let n = keys.len();
    keys.sort_by_key(|k| (k.key, k.shift));
    keys.dedup();
    assert_eq!(keys.len(), n);
}

#[test]
fn press_and_release() {
    let map = Keymap::default_map();
    let mut t = KeyboardTracker::default();
    assert_eq!(
        t.actions(&[down(Key::Enter)], &map, false),
        vec![KeyAction::Press(KeyId::Enter)]
    );
    assert_eq!(
        t.actions(&[up(Key::Enter)], &map, false),
        vec![KeyAction::Release(KeyId::Enter)]
    );
}

#[test]
fn shift_plus_presses_add() {
    let map = Keymap::default_map();
    let mut t = KeyboardTracker::default();
    let ev = key_event(Key::Plus, true, Modifiers::SHIFT);
    assert_eq!(
        t.actions(&[ev], &map, false),
        vec![KeyAction::Press(KeyId::Add)]
    );
}

#[test]
fn shift_falls_back_to_unshifted() {
    let map = Keymap::default_map();
    assert_eq!(map.lookup(pk(Key::Num1, true)), Some(KeyId::One));
    assert_eq!(map.lookup(pk(Key::Num8, true)), Some(KeyId::Mul));
    assert_eq!(map.lookup(pk(Key::Num8, false)), Some(KeyId::Eight));
}

#[test]
fn repeat_is_ignored() {
    let map = Keymap::default_map();
    let mut t = KeyboardTracker::default();
    assert_eq!(
        t.actions(&[down(Key::Enter)], &map, false),
        vec![KeyAction::Press(KeyId::Enter)]
    );
    let repeat = Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: true,
        modifiers: Modifiers::NONE,
    };
    assert_eq!(t.actions(&[repeat.clone(), repeat], &map, false), vec![]);
    assert_eq!(
        t.actions(&[up(Key::Enter)], &map, false),
        vec![KeyAction::Release(KeyId::Enter)]
    );
}

#[test]
fn release_after_shift_lifted_releases_the_pressed_key() {
    let map = Keymap::default_map();
    let mut t = KeyboardTracker::default();
    let ev = key_event(Key::Num8, true, Modifiers::SHIFT);
    assert_eq!(
        t.actions(&[ev], &map, false),
        vec![KeyAction::Press(KeyId::Mul)]
    );
    assert_eq!(
        t.actions(&[up(Key::Num8)], &map, false),
        vec![KeyAction::Release(KeyId::Mul)]
    );
}

fn phys_event(key: Key, physical: Key, pressed: bool, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: Some(physical),
        pressed,
        repeat: false,
        modifiers,
    }
}

#[test]
fn shift_equals_released_after_shift_lifted_releases_add() {
    // Shift+= arrives as logical Plus; with Shift lifted first, the release
    // arrives as logical Equals. The physical key is Equals both times.
    let map = Keymap::default_map();
    let mut t = KeyboardTracker::default();
    let press = phys_event(Key::Plus, Key::Equals, true, Modifiers::SHIFT);
    assert_eq!(
        t.actions(&[press], &map, false),
        vec![KeyAction::Press(KeyId::Add)]
    );
    let release = phys_event(Key::Equals, Key::Equals, false, Modifiers::NONE);
    assert_eq!(
        t.actions(&[release], &map, false),
        vec![KeyAction::Release(KeyId::Add)]
    );
}

#[test]
fn one_released_after_shift_pressed_releases_one() {
    // '1' pressed plain, Shift goes down, release arrives as logical '!'.
    let map = Keymap::default_map();
    let mut t = KeyboardTracker::default();
    let press = phys_event(Key::Num1, Key::Num1, true, Modifiers::NONE);
    assert_eq!(
        t.actions(&[press], &map, false),
        vec![KeyAction::Press(KeyId::One)]
    );
    let release = phys_event(Key::Exclamationmark, Key::Num1, false, Modifiers::SHIFT);
    assert_eq!(
        t.actions(&[release], &map, false),
        vec![KeyAction::Release(KeyId::One)]
    );
}

#[test]
fn semicolon_released_as_colon_releases_xtthetan() {
    let map = Keymap::default_map();
    let mut t = KeyboardTracker::default();
    let press = phys_event(Key::Semicolon, Key::Semicolon, true, Modifiers::NONE);
    assert_eq!(
        t.actions(&[press], &map, false),
        vec![KeyAction::Press(KeyId::XTThetaN)]
    );
    let release = phys_event(Key::Colon, Key::Semicolon, false, Modifiers::SHIFT);
    assert_eq!(
        t.actions(&[release], &map, false),
        vec![KeyAction::Release(KeyId::XTThetaN)]
    );
}

#[test]
fn focus_loss_releases_all() {
    let map = Keymap::default_map();
    let mut t = KeyboardTracker::default();
    assert_eq!(
        t.actions(&[down(Key::Enter)], &map, false),
        vec![KeyAction::Press(KeyId::Enter)]
    );
    assert_eq!(
        t.actions(&[Event::WindowFocused(false)], &map, false),
        vec![KeyAction::ReleaseAll]
    );
    assert_eq!(t.actions(&[up(Key::Enter)], &map, false), vec![]);
}

#[test]
fn typing_suppresses_presses_not_releases() {
    let map = Keymap::default_map();
    let mut t = KeyboardTracker::default();
    assert_eq!(
        t.actions(&[down(Key::Enter)], &map, false),
        vec![KeyAction::Press(KeyId::Enter)]
    );
    assert_eq!(t.actions(&[down(Key::Num5)], &map, true), vec![]);
    assert_eq!(
        t.actions(&[up(Key::Enter)], &map, true),
        vec![KeyAction::Release(KeyId::Enter)]
    );
    // The key typed into the text field never pressed anything, so its release is silent.
    assert_eq!(t.actions(&[up(Key::Num5)], &map, false), vec![]);
}

#[test]
fn ctrl_combinations_are_ignored() {
    let map = Keymap::default_map();
    let mut t = KeyboardTracker::default();
    for mods in [Modifiers::CTRL, Modifiers::ALT, Modifiers::COMMAND] {
        assert_eq!(
            t.actions(&[key_event(Key::S, true, mods)], &map, false),
            vec![]
        );
        assert_eq!(
            t.actions(&[key_event(Key::S, false, mods)], &map, false),
            vec![]
        );
    }
}

#[test]
fn bind_replaces_existing() {
    let mut map = Keymap::default_map();
    map.bind(pk(Key::Space, false), KeyId::Enter);
    map.bind(pk(Key::Space, false), KeyId::Sin);
    assert_eq!(map.lookup(pk(Key::Space, false)), Some(KeyId::Sin));
    let n = map
        .bindings
        .iter()
        .filter(|(k, _)| *k == pk(Key::Space, false))
        .count();
    assert_eq!(n, 1);
}

#[test]
fn unbind_key_removes_all_bindings() {
    let mut map = Keymap::default_map();
    assert!(map.keys_for(KeyId::Del).len() >= 2);
    map.unbind_key(KeyId::Del);
    assert!(map.keys_for(KeyId::Del).is_empty());
    assert_eq!(map.lookup(pk(Key::Backspace, false)), None);
}

#[test]
fn pc_key_label_formats() {
    assert_eq!(pc_key_label(pk(Key::Num8, true)), "Shift+8");
    assert_eq!(pc_key_label(pk(Key::Enter, false)), "Enter");
}

#[test]
fn keymap_serde_roundtrip() {
    let mut map = Keymap::default_map();
    map.bind(pk(Key::Space, true), KeyId::Neg);
    let json = serde_json::to_string(&map).expect("serialize");
    let back: Keymap = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, map);
}

#[test]
fn two_pc_keys_for_one_calc_key_release_when_both_up() {
    let map = Keymap::default_map();
    let mut t = KeyboardTracker::default();
    assert_eq!(
        t.actions(&[down(Key::Backspace), down(Key::Delete)], &map, false),
        vec![KeyAction::Press(KeyId::Del)]
    );
    assert_eq!(t.actions(&[up(Key::Backspace)], &map, false), vec![]);
    assert_eq!(
        t.actions(&[up(Key::Delete)], &map, false),
        vec![KeyAction::Release(KeyId::Del)]
    );
}
