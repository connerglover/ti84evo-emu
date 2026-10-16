//! Tools: key scripts (E1) and key history (E2).

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
