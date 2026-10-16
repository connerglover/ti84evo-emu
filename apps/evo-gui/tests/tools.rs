//! Tools: key scripts (E1), key history (E2), macros / AutoTester (E3) and the Hardware panel (E4).

mod common;

use common::{TempDir, harness_with, open_action, test_options};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp};
use evo_gui::backend::fake::FakeBackend;
use evo_gui::backend::{CLOCK_HZ, KeyEvent};
use evo_gui::keypad::KeyId;
use evo_gui::platform::FakePlatform;
use evo_gui::toolbar::Action;
use evo_gui::tools::history::{KEY_HISTORY_CAP, KeyHistory, history_line};
use evo_gui::tools::script::{
    ScriptError, ScriptEvent, TAP_GAP_MS, TAP_HOLD_MS, key_by_name, ms_between, parse_script,
    to_script,
};

fn ev(at_ms: u64, key: KeyId, pressed: bool) -> ScriptEvent {
    ScriptEvent {
        at_ms,
        key,
        pressed,
    }
}

fn bad(index: usize, token: &str) -> ScriptError {
    ScriptError::BadToken {
        index,
        token: token.to_owned(),
    }
}

#[test]
fn tap_constants_match_the_core() {
    assert_eq!(TAP_HOLD_MS, 50);
    assert_eq!(TAP_GAP_MS, 50);
}

#[test]
fn key_by_name_uses_core_names() {
    for id in KeyId::ALL {
        assert_eq!(key_by_name(id.core_name()), Some(id));
        assert_eq!(key_by_name(&id.core_name().to_ascii_uppercase()), Some(id));
    }
    assert_eq!(key_by_name("2ND"), Some(KeyId::Second));
    assert_eq!(key_by_name("nokey"), None);
    assert_eq!(key_by_name(""), None);
    assert_eq!(key_by_name("r1c2"), None);
}

#[test]
fn parses_core_grammar() {
    use KeyId::*;
    assert_eq!(
        parse_script("2 add 3 enter").unwrap(),
        vec![
            ev(0, Two, true),
            ev(50, Two, false),
            ev(100, Add, true),
            ev(150, Add, false),
            ev(200, Three, true),
            ev(250, Three, false),
            ev(300, Enter, true),
            ev(350, Enter, false),
        ]
    );
    assert_eq!(
        parse_script("+alpha wait=20 -alpha").unwrap(),
        vec![ev(0, Alpha, true), ev(20, Alpha, false)]
    );
    assert_eq!(
        parse_script("2;add ;; 3\n\tenter").unwrap(),
        parse_script("2 add 3 enter").unwrap()
    );
    assert_eq!(
        parse_script("ENTER +Alpha -ALPHA").unwrap(),
        vec![
            ev(0, Enter, true),
            ev(50, Enter, false),
            ev(100, Alpha, true),
            ev(100, Alpha, false),
        ]
    );
    assert_eq!(parse_script("").unwrap(), vec![]);
    assert_eq!(parse_script(" ; ").unwrap(), vec![]);
    assert_eq!(
        parse_script("wait=0 wait=7 on").unwrap(),
        vec![ev(7, On, true), ev(57, On, false)]
    );
}

#[test]
fn rejects_bad_tokens() {
    assert_eq!(parse_script("r1c2"), Err(bad(0, "r1c2")));
    assert_eq!(parse_script("enter wait=-1"), Err(bad(1, "wait=-1")));
    assert_eq!(parse_script("2; add +nokey"), Err(bad(2, "+nokey")));
    assert_eq!(parse_script("wait="), Err(bad(0, "wait=")));
    assert_eq!(parse_script("wait=+5"), Err(bad(0, "wait=+5")));
    assert_eq!(
        parse_script("wait=99999999999999999999999"),
        Err(bad(0, "wait=99999999999999999999999"))
    );
    assert_eq!(parse_script("-r0c0"), Err(bad(0, "-r0c0")));
    assert_eq!(parse_script("+"), Err(bad(0, "+")));
    assert_eq!(
        bad(2, "+nokey").to_string(),
        "key script token 2 `+nokey` is not valid"
    );
}

#[test]
fn to_script_roundtrips_taps() {
    let s = "2 add 3 enter";
    assert_eq!(to_script(&parse_script(s).unwrap()), s);
    let s = "wait=1000 2nd wait=5 mode";
    assert_eq!(to_script(&parse_script(s).unwrap()), s);
    assert_eq!(to_script(&[]), "");
}

#[test]
fn to_script_holds_and_waits() {
    let s = "+alpha wait=20 -alpha";
    assert_eq!(to_script(&parse_script(s).unwrap()), s);
    let s = "+alpha +ln +enter wait=2000 -alpha -ln -enter";
    assert_eq!(to_script(&parse_script(s).unwrap()), s);
    // A tap followed too soon by the next event is written as a hold.
    let events = [
        ev(0, KeyId::Enter, true),
        ev(50, KeyId::Enter, false),
        ev(80, KeyId::Two, true),
        ev(130, KeyId::Two, false),
    ];
    assert_eq!(to_script(&events), "+enter wait=50 -enter wait=30 2");
    // Events out of time order are sorted first.
    let events = [ev(150, KeyId::Add, false), ev(100, KeyId::Add, true)];
    assert_eq!(to_script(&events), "wait=100 add");
}

#[test]
fn to_script_tap_slack() {
    let tap = |release| [ev(0, KeyId::Enter, true), ev(release, KeyId::Enter, false)];
    assert_eq!(to_script(&tap(60)), "enter");
    assert_eq!(to_script(&tap(50)), "enter");
    assert_eq!(to_script(&tap(67)), "enter");
    assert_eq!(to_script(&tap(68)), "+enter wait=68 -enter");
    assert_eq!(to_script(&tap(49)), "+enter wait=49 -enter");
    // A slack tap re-parses to an exact TAP_HOLD_MS hold.
    assert_eq!(
        parse_script(&to_script(&tap(60))).unwrap(),
        vec![ev(0, KeyId::Enter, true), ev(50, KeyId::Enter, false)]
    );
    // The next event must come TAP_GAP_MS after the slack release.
    let events = [
        ev(0, KeyId::Enter, true),
        ev(60, KeyId::Enter, false),
        ev(110, KeyId::Two, true),
        ev(160, KeyId::Two, false),
    ];
    assert_eq!(to_script(&events), "enter wait=10 2");
    let events = [
        ev(0, KeyId::Enter, true),
        ev(60, KeyId::Enter, false),
        ev(109, KeyId::Two, true),
        ev(159, KeyId::Two, false),
    ];
    assert_eq!(to_script(&events), "+enter wait=60 -enter wait=49 2");
    // Another event between press and release makes both holds.
    let events = [
        ev(0, KeyId::Alpha, true),
        ev(10, KeyId::Two, true),
        ev(50, KeyId::Alpha, false),
        ev(60, KeyId::Two, false),
    ];
    assert_eq!(
        to_script(&events),
        "+alpha wait=10 +2 wait=40 -alpha wait=10 -2"
    );
}

#[test]
fn ms_between_cases() {
    assert_eq!(ms_between(0, 0), 0);
    assert_eq!(ms_between(0, CLOCK_HZ), 1000);
    assert_eq!(ms_between(100, 100 + 7_800_000), 50);
    assert_eq!(ms_between(0, CLOCK_HZ / 1000 - 1), 0);
    assert_eq!(ms_between(5, 4), 0);
    // No overflow on the multiply: the exact quotient.
    let exact = (u128::from(u64::MAX) * 1000 / u128::from(CLOCK_HZ)) as u64;
    assert_eq!(ms_between(0, u64::MAX), exact);
    assert!(exact > u64::MAX / CLOCK_HZ * 1000);
}

// ---- E2: key history ----

/// A running app on `fake` with its own data dir (kept alive by the returned guard).
fn tools_harness(fake: FakeBackend) -> (Harness<'static, EvoApp>, TempDir) {
    let dir = TempDir::new("evo-gui-tools", true);
    let opts = AppOptions {
        start_paused: false,
        ..test_options(&dir, FakePlatform::shared())
    };
    (harness_with(fake, opts), dir)
}

fn kev(key: KeyId, pressed: bool, cycle: u64) -> KeyEvent {
    KeyEvent {
        key,
        pressed,
        cycle,
    }
}

fn history_open(h: &Harness<'static, EvoApp>) -> bool {
    h.state().features().active(Action::KeyHistory)
}

#[test]
fn history_line_format() {
    assert_eq!(KEY_HISTORY_CAP, 1000);
    assert_eq!(
        history_line(&kev(KeyId::Enter, true, 156_000_000)),
        "     1.000 s  enter    down"
    );
    assert_eq!(
        history_line(&kev(KeyId::Second, false, 7_800_000)),
        "     0.050 s  2nd      up"
    );
}

#[test]
fn history_window_lists_presses() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = tools_harness(fake);
    open_action(&mut h, Action::KeyHistory);
    assert!(history_open(&h));
    h.get_by_label("enter").click();
    h.run_steps(8);
    let events = probe.key_events();
    assert_eq!(
        events
            .iter()
            .map(|e| (e.key, e.pressed))
            .collect::<Vec<_>>(),
        [(KeyId::Enter, true), (KeyId::Enter, false)]
    );
    let down = history_line(&events[0]);
    let up = history_line(&events[1]);
    assert!(down.ends_with("enter    down"), "{down}");
    assert!(up.ends_with("enter    up"), "{up}");
    h.get_by_label(&down);
    h.get_by_label(&up);

    h.get_by_label("Clear").click();
    h.run_steps(2);
    assert!(h.query_by_label(&down).is_none());
    assert!(h.query_by_label(&up).is_none());
}

#[test]
fn history_is_fed_while_closed() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = tools_harness(fake);
    h.get_by_label("enter").click();
    h.run_steps(8);
    assert!(!history_open(&h));
    open_action(&mut h, Action::KeyHistory);
    let events = probe.key_events();
    assert_eq!(events.len(), 2);
    h.get_by_label(&history_line(&events[0]));
    h.get_by_label(&history_line(&events[1]));
    // The action toggles: running it again closes the window.
    open_action(&mut h, Action::KeyHistory);
    assert!(!history_open(&h));
    assert!(h.query_by_label(&history_line(&events[0])).is_none());
}

#[test]
fn history_is_capped() {
    let mut hist = KeyHistory::new();
    let events: Vec<KeyEvent> = (0..KEY_HISTORY_CAP as u64 + 5)
        .map(|i| kev(KeyId::Enter, i % 2 == 0, i * 1000))
        .collect();
    hist.push(&events[..10]);
    hist.push(&events[10..]);
    assert_eq!(hist.len(), KEY_HISTORY_CAP);
    let kept: Vec<KeyEvent> = hist.events().copied().collect();
    assert_eq!(kept, events[5..]);
    hist.clear();
    assert_eq!(hist.len(), 0);
}

#[test]
fn history_script_times_from_the_first_event() {
    let mut hist = KeyHistory::new();
    assert_eq!(hist.script(), "");
    let t0 = 1_000_000_000;
    let ms = CLOCK_HZ / 1000;
    hist.push(&[
        kev(KeyId::Two, true, t0),
        kev(KeyId::Two, false, t0 + 50 * ms),
        kev(KeyId::Alpha, true, t0 + 100 * ms),
        kev(KeyId::Alpha, false, t0 + 400 * ms),
    ]);
    assert_eq!(hist.script(), "2 +alpha wait=300 -alpha");
}

#[test]
fn history_restarts_when_cycles_go_back() {
    let ms = CLOCK_HZ / 1000;
    let s = CLOCK_HZ;
    // Across two batches: tap enter at 1 s, then (after a reset) tap 2 at 0.5 s.
    let mut hist = KeyHistory::new();
    hist.push(&[
        kev(KeyId::Enter, true, s),
        kev(KeyId::Enter, false, s + 50 * ms),
    ]);
    hist.push(&[
        kev(KeyId::Two, true, s / 2),
        kev(KeyId::Two, false, s / 2 + 50 * ms),
    ]);
    assert_eq!(hist.len(), 2);
    assert_eq!(hist.script(), "2");

    // Within one batch: only the events after the last drop are kept.
    let mut hist = KeyHistory::new();
    hist.push(&[
        kev(KeyId::Enter, true, s),
        kev(KeyId::Enter, false, s + 50 * ms),
        kev(KeyId::Alpha, true, 10 * ms),
        kev(KeyId::Alpha, false, 60 * ms),
        kev(KeyId::Two, true, 5 * ms),
        kev(KeyId::Two, false, 55 * ms),
    ]);
    assert_eq!(hist.len(), 2);
    assert_eq!(hist.script(), "2");

    // Equal cycles are not a restart.
    let mut hist = KeyHistory::new();
    hist.push(&[kev(KeyId::Two, true, s)]);
    hist.push(&[kev(KeyId::Two, false, s)]);
    assert_eq!(hist.len(), 2);
}

#[test]
fn copy_as_script() {
    let (mut h, _dir) = tools_harness(FakeBackend::new());
    open_action(&mut h, Action::KeyHistory);
    h.get_by_label("enter").click();
    h.run_steps(8);
    h.get_by_label("Copy as key script").click();
    let want = egui::OutputCommand::CopyText("enter".into());
    let mut seen = Vec::new();
    for _ in 0..3 {
        h.step();
        seen.extend(h.output().platform_output.commands.iter().cloned());
    }
    assert!(seen.contains(&want), "{seen:?}");
}

// ---- E3: macros / AutoTester ----

mod macros {
    use super::{TempDir, kev, test_options, tools_harness};
    use crate::common::{harness_with, open_action};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use evo_gui::app::{AppOptions, EvoApp};
    use evo_gui::backend::fake::{FakeBackend, FakeProbe};
    use evo_gui::backend::{CLOCK_HZ, Frame, KeyEvent};
    use evo_gui::keypad::KeyId;
    use evo_gui::pacer::Speed;
    use evo_gui::platform::FakePlatform;
    use evo_gui::prefs::Prefs;
    use evo_gui::toolbar::Action;
    use evo_gui::tools::macros::{
        DEFAULT_SETTLE_MS, MACROS_TITLE, MacroFile, MacroResult, macro_path, screen_sha256,
    };
    use sha2::{Digest, Sha256};

    const MS: u64 = CLOCK_HZ / 1000;

    fn frame() -> Frame {
        Frame {
            width: 4,
            height: 2,
            rgba: (0u8..32).collect(),
        }
    }

    fn open(h: &mut Harness<'static, EvoApp>) {
        open_action(h, Action::Macros);
        assert!(h.state().features().active(Action::Macros));
        h.get_by_label(MACROS_TITLE);
    }

    fn set_script(h: &mut Harness<'static, EvoApp>, s: &str) {
        h.state_mut()
            .features_mut()
            .tools
            .macros_mut()
            .set_script(s);
        h.run_steps(1);
    }

    fn playing(h: &Harness<'static, EvoApp>) -> bool {
        h.state().features().tools.macros().is_playing()
    }

    /// Steps (at least once, so a queued click lands) until `done` holds, at most `max` steps.
    fn step_until(
        h: &mut Harness<'static, EvoApp>,
        max: usize,
        mut done: impl FnMut(&Harness<'static, EvoApp>) -> bool,
    ) {
        for _ in 0..max {
            h.step();
            if done(h) {
                return;
            }
        }
        assert!(done(h), "condition not reached in {max} steps");
    }

    /// The four events of `"2 add"` from `start`.
    fn two_add(start: u64) -> Vec<KeyEvent> {
        vec![
            kev(KeyId::Two, true, start),
            kev(KeyId::Two, false, start + 7_800_000),
            kev(KeyId::Add, true, start + 15_600_000),
            kev(KeyId::Add, false, start + 23_400_000),
        ]
    }

    /// Plays `"2 add"` to the end; returns the probe's key events.
    fn play_two_add(mut h: Harness<'static, EvoApp>, probe: &FakeProbe) -> Vec<KeyEvent> {
        open(&mut h);
        set_script(&mut h, "2 add");
        h.get_by_label("Play").click();
        step_until(&mut h, 400, |h| {
            !playing(h) && probe.key_events().len() == 4
        });
        h.get_by_label("Result: done");
        probe.key_events()
    }

    #[test]
    fn screen_sha256_matches_sha2() {
        let f = frame();
        let want: String = Sha256::digest(&f.rgba)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(screen_sha256(&f), want);
        assert_eq!(want.len(), 64);
        // Only the pixels count, not the size.
        let other = Frame {
            width: 8,
            height: 1,
            ..frame()
        };
        assert_eq!(screen_sha256(&other), want);
        assert_eq!(
            screen_sha256(&Frame {
                width: 0,
                height: 0,
                rgba: vec![]
            }),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn macro_file_roundtrip() {
        assert_eq!(DEFAULT_SETTLE_MS, 500);
        let hash = "ab".repeat(32);
        let m = MacroFile {
            name: "graph test".into(),
            script: "2 add 3\nenter".into(),
            expect_sha256: Some(hash.clone()),
            settle_ms: 750,
        };
        let text = m.to_text();
        assert!(text.contains(&format!("# expect-sha256: {hash}")), "{text}");
        assert!(text.contains("# settle-ms: 750"), "{text}");
        assert_eq!(MacroFile::parse("graph test", &text).unwrap(), m);

        let m = MacroFile {
            expect_sha256: None,
            ..m
        };
        assert_eq!(MacroFile::parse("graph test", &m.to_text()).unwrap(), m);

        // Unknown '#' lines are ignored; no settle header means the default; CRLF is fine;
        // an upper-case hash is kept lower-case.
        let text = format!(
            "# a note\r\n# EXPECT: no\r\n2 add\r\n# expect-sha256: {}\r\n",
            "AB".repeat(32)
        );
        assert_eq!(
            MacroFile::parse("x", &text).unwrap(),
            MacroFile {
                name: "x".into(),
                script: "2 add".into(),
                expect_sha256: Some(hash.clone()),
                settle_ms: DEFAULT_SETTLE_MS,
            }
        );

        assert!(MacroFile::parse("x", "# settle-ms: soon\n2").is_err());
        assert!(MacroFile::parse("x", "# settle-ms: -1\n2").is_err());
        assert!(MacroFile::parse("x", "# expect-sha256: 1234\n2").is_err());
        let text = format!("# expect-sha256: {}\n2", "g".repeat(64));
        assert!(MacroFile::parse("x", &text).is_err());
    }

    #[test]
    fn macro_names_validated() {
        let dir = std::path::Path::new("data");
        assert_eq!(
            macro_path(dir, "graph test_1-a").unwrap(),
            dir.join("macros").join("graph test_1-a.keys")
        );
        assert!(macro_path(dir, &"a".repeat(64)).is_ok());
        for bad in [
            "",
            "../x",
            "a/b",
            "a\\b",
            "x.keys",
            "caf\u{e9}",
            " lead",
            "trail ",
            "con",
            "NUL",
            "com1",
        ] {
            assert!(macro_path(dir, bad).is_err(), "{bad:?} accepted");
        }
        assert!(macro_path(dir, &"a".repeat(65)).is_err());
    }

    #[test]
    fn record_then_stop_fills_script() {
        let (mut h, _dir) = tools_harness(FakeBackend::new());
        open(&mut h);
        h.get_by_label("Record").click();
        h.run_steps(3);
        assert!(h.state().features().tools.macros().is_recording());
        h.get_by_label("enter").click();
        h.run_steps(8);
        h.get_by_label("Stop recording").click();
        h.run_steps(2);
        let m = h.state().features().tools.macros();
        assert!(!m.is_recording());
        assert_eq!(m.script(), "enter");
    }

    #[test]
    fn play_presses_at_scripted_cycles() {
        let fake = FakeBackend::new();
        let probe = fake.probe();
        let (h, _dir) = tools_harness(fake);
        let events = play_two_add(h, &probe);
        assert_eq!(events, two_add(events[0].cycle));
        assert!(probe.held().is_empty());
    }

    #[test]
    fn playback_at_unlimited_speed_hits_exact_cycles() {
        let fake = FakeBackend::new();
        let probe = fake.probe();
        let dir = TempDir::new("evo-gui-tools", true);
        let opts = AppOptions {
            start_paused: false,
            prefs: Prefs {
                speed: Speed::Unlimited,
                ..Prefs::default()
            },
            ..test_options(&dir, FakePlatform::shared())
        };
        let h = harness_with(fake, opts);
        assert_eq!(h.state().speed(), Speed::Unlimited);
        let events = play_two_add(h, &probe);
        assert_eq!(events, two_add(events[0].cycle));
    }

    #[test]
    fn play_unpauses() {
        let fake = FakeBackend::new();
        let probe = fake.probe();
        let (mut h, _dir) = tools_harness(fake);
        h.state_mut().set_paused(true);
        open(&mut h);
        set_script(&mut h, "2");
        h.get_by_label("Play").click();
        h.run_steps(2);
        assert!(!h.state().paused());
        step_until(&mut h, 200, |h| !playing(h));
        assert_eq!(probe.key_events().len(), 2);
    }

    #[test]
    fn playback_across_pause_hits_exact_cycles() {
        let fake = FakeBackend::new();
        let probe = fake.probe();
        let (mut h, _dir) = tools_harness(fake);
        open(&mut h);
        set_script(&mut h, "2 add");
        h.get_by_label("Play").click();
        step_until(&mut h, 200, |_| probe.key_events().len() == 1);
        h.state_mut().run_action(Action::TogglePause);
        assert!(h.state().paused());
        let cycles = probe.cycles();
        h.run_steps(10);
        assert_eq!(probe.cycles(), cycles);
        assert_eq!(probe.held(), vec![KeyId::Two]);
        assert!(playing(&h));
        h.state_mut().run_action(Action::TogglePause);
        assert!(!h.state().paused());
        step_until(&mut h, 400, |h| !playing(h));
        let events = probe.key_events();
        assert_eq!(events, two_add(events[0].cycle));
        h.get_by_label("Result: done");
    }

    #[test]
    fn stop_releases_held_keys() {
        let fake = FakeBackend::new();
        let probe = fake.probe();
        let (mut h, _dir) = tools_harness(fake);
        open(&mut h);
        set_script(&mut h, "+alpha wait=60000 -alpha");
        h.get_by_label("Play").click();
        step_until(&mut h, 200, |_| probe.key_events().len() == 1);
        h.get_by_label("Stop").click();
        h.run_steps(2);
        assert!(!playing(&h));
        assert!(probe.held().is_empty());
        assert_eq!(
            probe
                .key_events()
                .iter()
                .map(|e| (e.key, e.pressed))
                .collect::<Vec<_>>(),
            [(KeyId::Alpha, true), (KeyId::Alpha, false)]
        );
        assert!(h.query_by_label_contains("Result:").is_none());
    }

    #[test]
    fn reset_ends_playback() {
        let fake = FakeBackend::new();
        let probe = fake.probe();
        let (mut h, _dir) = tools_harness(fake);
        open(&mut h);
        set_script(&mut h, "+alpha wait=60000 -alpha 2");
        h.get_by_label("Play").click();
        step_until(&mut h, 200, |_| probe.key_events().len() == 1);
        h.state_mut().run_action(Action::Reset);
        h.run_steps(4);
        assert!(!playing(&h));
        assert!(
            h.state()
                .features()
                .tools
                .macros()
                .next_deadline()
                .is_none()
        );
        // Nothing of the script is played after the reset.
        let presses: Vec<_> = probe
            .key_events()
            .into_iter()
            .filter(|e| e.pressed)
            .collect();
        assert_eq!(presses.len(), 1, "{presses:?}");
    }

    #[test]
    fn bad_script_is_reported_inline() {
        let fake = FakeBackend::new();
        let probe = fake.probe();
        let (mut h, _dir) = tools_harness(fake);
        open(&mut h);
        set_script(&mut h, "2 +nokey");
        h.get_by_label("Play").click();
        h.run_steps(2);
        assert!(!playing(&h));
        h.get_by_label("key script token 1 `+nokey` is not valid");
        assert!(probe.key_events().is_empty());
    }

    fn check_harness() -> (Harness<'static, EvoApp>, TempDir) {
        let (mut h, dir) = tools_harness(FakeBackend::with_frame(frame()));
        open(&mut h);
        set_script(&mut h, "2");
        (h, dir)
    }

    #[test]
    fn check_passes_with_current_screen_hash() {
        let (mut h, _dir) = check_harness();
        h.get_by_label("Use current screen").click();
        h.run_steps(2);
        let m = h.state().features().tools.macros();
        assert_eq!(m.expected(), screen_sha256(&frame()));
        assert!(m.check());
        h.get_by_label("Play").click();
        step_until(&mut h, 300, |h| !playing(h));
        assert_eq!(
            h.state().features().tools.macros().result(),
            Some(&MacroResult::Pass)
        );
        h.get_by_label("Result: PASS");
    }

    #[test]
    fn check_waits_for_settle() {
        let fake = FakeBackend::with_frame(frame());
        let probe = fake.probe();
        let (mut h, _dir) = tools_harness(fake);
        open(&mut h);
        set_script(&mut h, "2");
        h.get_by_label("Use current screen").click();
        h.run_steps(2);
        h.get_by_label("Play").click();
        step_until(&mut h, 300, |_| probe.key_events().len() == 2);
        assert!(playing(&h));
        let release = probe.key_events()[1].cycle;
        assert_eq!(
            h.state().features().tools.macros().next_deadline(),
            Some(release + DEFAULT_SETTLE_MS * MS)
        );
        step_until(&mut h, 300, |h| !playing(h));
        // The run stopped exactly at the check cycle.
        assert_eq!(probe.cycles(), release + DEFAULT_SETTLE_MS * MS);
    }

    #[test]
    fn check_fails_with_other_hash() {
        let (mut h, _dir) = check_harness();
        let other = "0".repeat(64);
        h.state_mut()
            .features_mut()
            .tools
            .macros_mut()
            .set_check(true, &other);
        h.get_by_label("Play").click();
        step_until(&mut h, 300, |h| !playing(h));
        let got = screen_sha256(&frame());
        assert_eq!(
            h.state().features().tools.macros().result(),
            Some(&MacroResult::Fail { got: got.clone() })
        );
        h.get_by_label(&format!("Result: FAIL (screen sha256 {got})"));
    }

    #[test]
    fn check_without_display_reports_no_frame() {
        let (mut h, _dir) = tools_harness(FakeBackend::without_display());
        open(&mut h);
        set_script(&mut h, "2");
        h.state_mut()
            .features_mut()
            .tools
            .macros_mut()
            .set_check(true, &"0".repeat(64));
        h.get_by_label("Play").click();
        step_until(&mut h, 300, |h| !playing(h));
        assert_eq!(
            h.state().features().tools.macros().result(),
            Some(&MacroResult::NoFrame)
        );
        h.get_by_label("Result: no display frame");
        // "Use current screen" without a display explains itself and changes nothing.
        h.get_by_label("Use current screen").click();
        h.run_steps(2);
        h.get_by_label("No display frame");
        assert_eq!(
            h.state().features().tools.macros().expected(),
            "0".repeat(64)
        );
    }

    #[test]
    fn check_with_bad_expected_hash_is_refused() {
        let (mut h, _dir) = check_harness();
        h.state_mut()
            .features_mut()
            .tools
            .macros_mut()
            .set_check(true, "abc");
        h.get_by_label("Play").click();
        h.run_steps(2);
        assert!(!playing(&h));
        h.get_by_label("Expected SHA-256 must be 64 hex digits");
    }

    #[test]
    fn check_with_empty_hash_reports_screen_hash() {
        let fake = FakeBackend::with_frame(frame());
        let probe = fake.probe();
        let (mut h, _dir) = tools_harness(fake);
        open(&mut h);
        set_script(&mut h, "2");
        h.state_mut()
            .features_mut()
            .tools
            .macros_mut()
            .set_check(true, "  ");
        h.get_by_label("Play").click();
        step_until(&mut h, 300, |_| probe.key_events().len() == 2);
        assert!(playing(&h));
        let release = probe.key_events()[1].cycle;
        step_until(&mut h, 300, |h| !playing(h));
        // The run waited for settle, then checked the screen and showed its hash.
        assert_eq!(probe.cycles(), release + DEFAULT_SETTLE_MS * MS);
        let got = screen_sha256(&frame());
        assert_eq!(
            h.state().features().tools.macros().result(),
            Some(&MacroResult::Fail { got: got.clone() })
        );
        assert_eq!(h.state().features().tools.macros().error(), None);
        h.get_by_label(&format!("Result: FAIL (screen sha256 {got})"));
    }

    #[test]
    fn save_with_check_and_empty_hash_is_refused() {
        let (mut h, dir) = tools_harness(FakeBackend::new());
        open(&mut h);
        let macros = h.state_mut().features_mut().tools.macros_mut();
        macros.set_name("sum");
        macros.set_check(true, "");
        h.run_steps(1);
        h.get_by_label("Save").click();
        h.run_steps(2);
        assert!(!dir.join("macros").join("sum.keys").exists());
        h.get_by_label("Saving with the screen check needs an Expected SHA-256");
    }

    #[test]
    fn save_and_load_macro() {
        let (mut h, dir) = tools_harness(FakeBackend::new());
        open(&mut h);
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Name")
            .click();
        h.run_steps(1);
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Name")
            .type_text("sum");
        h.run_steps(1);
        set_script(&mut h, "2 add 3 enter");
        let hash = "cd".repeat(32);
        h.state_mut()
            .features_mut()
            .tools
            .macros_mut()
            .set_check(true, &hash);
        h.run_steps(1);
        h.get_by_label("Save").click();
        h.run_steps(2);
        let path = dir.join("macros").join("sum.keys");
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            MacroFile::parse("sum", &text).unwrap(),
            MacroFile {
                name: "sum".into(),
                script: "2 add 3 enter".into(),
                expect_sha256: Some(hash.clone()),
                settle_ms: DEFAULT_SETTLE_MS,
            }
        );

        // A garbage file next to it is listed but fails to load without changing anything.
        std::fs::write(dir.join("macros").join("junk.keys"), "# settle-ms: x\n").unwrap();
        set_script(&mut h, "mode");
        h.state_mut()
            .features_mut()
            .tools
            .macros_mut()
            .set_check(false, "");
        h.get_by_label("Load").click();
        h.run_steps(2);
        h.get_by_label("junk").click();
        h.run_steps(2);
        assert_eq!(h.state().features().tools.macros().script(), "mode");
        h.get_by_label("sum").click();
        h.run_steps(2);
        let m = h.state().features().tools.macros();
        assert_eq!(m.script(), "2 add 3 enter");
        assert_eq!(m.name(), "sum");
        assert!(m.check());
        assert_eq!(m.expected(), hash);
    }

    #[test]
    fn save_with_bad_name_is_refused() {
        let (mut h, dir) = tools_harness(FakeBackend::new());
        open(&mut h);
        h.state_mut()
            .features_mut()
            .tools
            .macros_mut()
            .set_name("../x");
        h.run_steps(1);
        h.get_by_label("Save").click();
        h.run_steps(2);
        assert!(!dir.join("macros").exists());
        assert!(!dir.join("x.keys").exists());
        assert!(
            h.state()
                .features()
                .tools
                .macros()
                .error()
                .is_some_and(|e| e.starts_with("Macro name"))
        );
    }

    #[test]
    fn oversized_macro_file_is_refused_unread() {
        let (mut h, dir) = tools_harness(FakeBackend::new());
        open(&mut h);
        std::fs::create_dir_all(dir.join("macros")).unwrap();
        let f = std::fs::File::create(dir.join("macros").join("big.keys")).unwrap();
        f.set_len(64 * 1024 * 1024).unwrap();
        drop(f);
        set_script(&mut h, "mode");
        h.get_by_label("Load").click();
        h.run_steps(2);
        h.get_by_label("big").click();
        h.run_steps(2);
        assert_eq!(h.state().features().tools.macros().script(), "mode");
        assert!(
            h.state()
                .features()
                .tools
                .macros()
                .error()
                .is_some_and(|e| e.contains("too large"))
        );
    }
}

// ---- E4: hardware panel ----

mod hardware {
    use super::{kev, tools_harness};
    use crate::common::open_action;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use evo_gui::app::EvoApp;
    use evo_gui::backend::HwChange;
    use evo_gui::backend::fake::FakeBackend;
    use evo_gui::input::MIN_HOLD_CYCLES;
    use evo_gui::keypad::KeyId;
    use evo_gui::toolbar::Action;
    use evo_gui::tools::hardware::{HARDWARE_TITLE, hms, utc_seconds_of_day};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn open(h: &mut Harness<'static, EvoApp>) {
        open_action(h, Action::Hardware);
        assert!(h.state().features().active(Action::Hardware));
        h.get_by_label(HARDWARE_TITLE);
    }

    #[test]
    fn hms_cases() {
        assert_eq!(hms(0), "00:00:00");
        assert_eq!(hms(3_723), "01:02:03");
        assert_eq!(hms(86_399), "23:59:59");
        assert_eq!(hms(59), "00:00:59");
        assert_eq!(hms(36_000), "10:00:00");
    }

    #[test]
    fn utc_seconds_of_day_cases() {
        let at = |s: u64| UNIX_EPOCH + Duration::from_secs(s);
        assert_eq!(utc_seconds_of_day(at(86_400 * 3 + 3_723)), 3_723);
        assert_eq!(utc_seconds_of_day(UNIX_EPOCH), 0);
        assert_eq!(utc_seconds_of_day(at(86_399)), 86_399);
        assert_eq!(utc_seconds_of_day(at(86_400)), 0);
        // Sub-second parts round down.
        assert_eq!(utc_seconds_of_day(at(10) + Duration::from_millis(999)), 10);
        // Before the epoch: one second before midnight, 1970-01-01.
        assert_eq!(
            utc_seconds_of_day(UNIX_EPOCH - Duration::from_secs(1)),
            86_399
        );
        assert_eq!(
            utc_seconds_of_day(UNIX_EPOCH - Duration::from_millis(500)),
            86_399
        );
        assert_eq!(
            utc_seconds_of_day(UNIX_EPOCH - Duration::from_secs(86_400)),
            0
        );
        assert!(utc_seconds_of_day(SystemTime::now()) < 86_400);
    }

    #[test]
    fn window_shows_board_state() {
        let (mut h, _dir) = tools_harness(FakeBackend::new());
        open(&mut h);
        h.get_by_label("Backlight 100%");
        h.get_by_label("Power hold: on");
        h.get_by_label("Deep sleep: not armed");
        h.get_by_label("RTC 00:00:00");
        // Toggling the action again closes the window.
        open_action(&mut h, Action::Hardware);
        assert!(!h.state().features().active(Action::Hardware));
        assert!(h.query_by_label("Press ON").is_none());
    }

    #[test]
    fn toggles_send_changes() {
        let fake = FakeBackend::new();
        let probe = fake.probe();
        let (mut h, _dir) = tools_harness(fake);
        open(&mut h);
        h.get_by_label("Charger line high (GPIO 9)").click();
        h.run_steps(2);
        assert_eq!(probe.hw_changes(), [HwChange::ChargerLine(false)]);
        h.get_by_label("Battery OK (GPIO 11)").click();
        h.run_steps(2);
        h.get_by_label("USB-C attached").click();
        h.run_steps(2);
        assert_eq!(
            probe.hw_changes(),
            [
                HwChange::ChargerLine(false),
                HwChange::BatteryOk(false),
                HwChange::UsbAttached(true),
            ]
        );
        // The checkbox follows the backend: ticking the charger line again sends `true`.
        h.get_by_label("Charger line high (GPIO 9)").click();
        h.run_steps(2);
        assert_eq!(probe.hw_changes()[3], HwChange::ChargerLine(true));
    }

    #[test]
    fn press_on_holds_then_releases() {
        let fake = FakeBackend::new();
        let probe = fake.probe();
        let (mut h, _dir) = tools_harness(fake);
        open(&mut h);
        h.get_by_label("Press ON").click();
        for _ in 0..200 {
            h.step();
            if probe.key_events().len() == 2 {
                break;
            }
        }
        let events = probe.key_events();
        assert_eq!(events.len(), 2, "{events:?}");
        let p = events[0].cycle;
        assert_eq!(
            events,
            [
                kev(KeyId::On, true, p),
                kev(KeyId::On, false, p + MIN_HOLD_CYCLES)
            ]
        );
        assert!(probe.held().is_empty());
        assert!(
            h.state()
                .features()
                .tools
                .hardware()
                .next_deadline()
                .is_none()
        );
    }

    #[test]
    fn press_on_reports_its_release_deadline() {
        let fake = FakeBackend::new();
        let probe = fake.probe();
        let (mut h, _dir) = tools_harness(fake);
        h.state_mut().set_paused(true);
        open(&mut h);
        h.get_by_label("Press ON").click();
        h.run_steps(3);
        // Paused: the press is in, the release waits for its cycle.
        let events = probe.key_events();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(probe.held(), [KeyId::On]);
        assert_eq!(
            h.state().features().tools.hardware().next_deadline(),
            Some(events[0].cycle + MIN_HOLD_CYCLES)
        );
        h.state_mut().set_paused(false);
        for _ in 0..200 {
            h.step();
            if probe.key_events().len() == 2 {
                break;
            }
        }
        assert_eq!(
            probe.key_events()[1],
            kev(KeyId::On, false, events[0].cycle + MIN_HOLD_CYCLES)
        );
    }

    #[test]
    fn reset_drops_a_pending_on_release() {
        let fake = FakeBackend::new();
        let probe = fake.probe();
        let (mut h, _dir) = tools_harness(fake);
        h.state_mut().set_paused(true);
        open(&mut h);
        h.get_by_label("Press ON").click();
        h.run_steps(3);
        assert_eq!(probe.key_events().len(), 1);
        h.state_mut().run_action(Action::Reset);
        h.state_mut().set_paused(false);
        h.run_steps(4);
        assert!(
            h.state()
                .features()
                .tools
                .hardware()
                .next_deadline()
                .is_none()
        );
        assert!(probe.held().is_empty());
    }

    #[test]
    fn set_rtc_sends_seconds() {
        let fake = FakeBackend::new();
        let probe = fake.probe();
        let (mut h, _dir) = tools_harness(fake);
        open(&mut h);
        let before = utc_seconds_of_day(SystemTime::now());
        h.get_by_label("Set RTC to host clock (UTC)").click();
        h.run_steps(2);
        let after = utc_seconds_of_day(SystemTime::now());
        let changes = probe.hw_changes();
        assert_eq!(changes.len(), 1, "{changes:?}");
        let HwChange::RtcSeconds(s) = changes[0] else {
            panic!("{changes:?}");
        };
        // `after < before` only across UTC midnight.
        assert!(
            (before <= s && s <= after) || after < before,
            "{before} {s} {after}"
        );
        h.get_by_label(&format!("RTC {}", hms(s)));
    }
}
