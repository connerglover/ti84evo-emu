//! The debug dock: tab registry, run row, the Control tab and register editing, on
//! `FakeBackend`.

mod common;

use common::{TempDir, harness_with, test_options};
use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp};
use evo_gui::backend::fake::FakeBackend;
use evo_gui::backend::{Goal, RunStatus};
use evo_gui::debug::{DebugTab, cycle_text, parse_value};
use evo_gui::platform::FakePlatform;

/// Every tab label, in selector order.
const LABELS: [&str; 12] = [
    "Control",
    "Registers",
    "Disassembly",
    "Memory",
    "Visualizer",
    "Breakpoints",
    "Watchpoints",
    "Peripherals",
    "Bus monitor",
    "Stacks",
    "Console",
    "Unmapped",
];

/// A paused 1060×960 harness with the debug dock open. The data dir lives as long as the
/// returned guard.
fn dock(fake: FakeBackend) -> (Harness<'static, EvoApp>, TempDir) {
    let dir = TempDir::new("evo-gui-debug-dock", true);
    let opts = AppOptions {
        start_paused: true,
        ..test_options(&dir, FakePlatform::shared())
    };
    let mut h = harness_with(fake, opts);
    h.set_size(egui::vec2(1060.0, 960.0));
    h.run_steps(1);
    click(&mut h, "Debug panels");
    (h, dir)
}

fn click(h: &mut Harness<'static, EvoApp>, label: &str) {
    h.get_by_label(label).click();
    h.run_steps(3);
}

/// Opens the Control tab and types `text` into its "Run to" field.
fn type_run_to(h: &mut Harness<'static, EvoApp>, text: &str) {
    click(h, "Control");
    h.get_by_role(Role::TextInput).click();
    h.run_steps(2);
    h.get_by_role(Role::TextInput).type_text(text);
    h.run_steps(2);
}

#[test]
fn tabs_in_order() {
    let labels: Vec<&str> = DebugTab::ALL.iter().map(|t| t.label()).collect();
    assert_eq!(labels, LABELS);
    assert_eq!(DebugTab::default(), DebugTab::Registers);
    let (h, _dir) = dock(FakeBackend::new());
    let mut xs = Vec::new();
    for label in LABELS {
        xs.push(h.get_by_label(label).rect().min);
    }
    // Selector order: each tab is right of, or on a row below, the one before.
    for w in xs.windows(2) {
        assert!(
            w[1].y > w[0].y + 1.0 || (w[1].x > w[0].x && (w[1].y - w[0].y).abs() < 1.0),
            "{xs:?}"
        );
    }
}

#[test]
fn run_row_buttons() {
    let (mut h, _dir) = dock(FakeBackend::new());
    for label in ["Continue", "Step", "Step over", "Step out"] {
        h.get_by_label(label);
    }
    click(&mut h, "Continue");
    h.get_by_label("Pause");
    assert!(h.query_by_label("Continue").is_none());
}

#[test]
fn step_over_is_a_goal() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = dock(fake);
    click(&mut h, "Step over");
    assert_eq!(probe.goals(), [Goal::StepOver]);
    assert!(h.state().paused());
}

#[test]
fn step_out_runs_until_goal() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = dock(fake);
    h.get_by_label("Step out").click();
    h.run_steps(1);
    assert_eq!(probe.goals(), [Goal::StepOut]);
    assert!(!h.state().paused());
    h.run_steps(3);
    assert!(h.state().paused());
    assert_eq!(h.state().status(), RunStatus::Goal { pc: 0x1000_0240 });
    h.get_by_label("Stopped at 0x10000240");
    assert!(h.state().debug_open());
}

#[test]
fn run_to_field() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = dock(fake);
    type_run_to(&mut h, "0x10000300");
    click(&mut h, "Run");
    assert_eq!(probe.goals(), [Goal::RunTo(0x1000_0300)]);
    h.run_steps(3);
    h.get_by_label("Stopped at 0x10000300");
}

#[test]
fn run_to_rejects_garbage() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = dock(fake);
    type_run_to(&mut h, "zz");
    click(&mut h, "Run");
    assert!(probe.goals().is_empty());
    assert!(h.state().paused());
    h.get_by_label("Not a hex address");
}

#[test]
fn run_to_field_takes_keys_from_calculator() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = dock(fake);
    type_run_to(&mut h, "5");
    h.key_down(Key::Num5);
    h.key_up(Key::Num5);
    h.run_steps(2);
    assert!(probe.key_log().is_empty(), "{:?}", probe.key_log());
}

#[test]
fn endless_step_out_pause_then_continue() {
    let fake = FakeBackend::new().goal_never_reached();
    let probe = fake.probe();
    let (mut h, _dir) = dock(fake);
    click(&mut h, "Step out");
    assert_eq!(probe.goals(), [Goal::StepOut]);
    let mut last = probe.cycles();
    for _ in 0..20 {
        h.run_steps(1);
        assert!(!h.state().paused());
        assert!(!h.state().status().is_stop(), "{:?}", h.state().status());
        let now = probe.cycles();
        assert!(now > last, "cycles stuck at {now}");
        last = now;
    }
    assert!(probe.goal_armed());
    // Running a goal is a normal run: the run row offers Pause.
    click(&mut h, "Pause");
    assert!(h.state().paused());
    assert_eq!(probe.goal_cancels(), 1);
    assert!(!probe.goal_armed());
    click(&mut h, "Continue");
    h.run_steps(5);
    assert!(!h.state().paused());
    assert!(h.query_by_label_contains("Stopped at").is_none());
}

#[test]
fn step_buttons_disabled_while_running() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = dock(fake);
    click(&mut h, "Continue");
    for label in ["Step", "Step over", "Step out"] {
        h.get_by_label(label).click();
        h.run_steps(2);
    }
    assert!(probe.goals().is_empty());
    assert!(!h.state().paused());
}

#[test]
fn mark_resets_since_mark() {
    assert_eq!(
        cycle_text(500, 200),
        ("Cycles 500".to_owned(), "Since mark 300".to_owned())
    );
    // A mark past the counter (after a reset or state load) saturates at 0.
    assert_eq!(
        cycle_text(5, 200),
        ("Cycles 5".to_owned(), "Since mark 0".to_owned())
    );
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = dock(fake);
    click(&mut h, "Control");
    h.get_by_label("Cycles 0");
    h.get_by_label("Since mark 0");
    click(&mut h, "Step");
    click(&mut h, "Step");
    assert_eq!(probe.cycles(), 2);
    h.get_by_label("Cycles 2");
    h.get_by_label("Since mark 2");
    click(&mut h, "Mark");
    h.get_by_label("Since mark 0");
    click(&mut h, "Step");
    h.get_by_label("Cycles 3");
    h.get_by_label("Since mark 1");
}

#[test]
fn parse_value_cases() {
    // Hex follows `parse_addr`.
    assert_eq!(parse_value("0x20001000"), Some(0x2000_1000));
    assert_eq!(parse_value("  ff "), Some(0xff));
    assert_eq!(parse_value("0XDEAD_BEEF"), Some(0xdead_beef));
    assert_eq!(parse_value("ffffffff"), Some(u32::MAX));
    assert_eq!(parse_value("100000000"), None);
    // A leading `#` is decimal.
    assert_eq!(parse_value("#10"), Some(10));
    assert_eq!(parse_value(" #4294967295 "), Some(u32::MAX));
    assert_eq!(parse_value("#1_000"), Some(1000));
    assert_eq!(parse_value("#0"), Some(0));
    for bad in [
        "",
        "   ",
        "0x",
        "zz",
        "-1",
        "+1",
        "1 2",
        "#",
        "#4294967296",
        "#ff",
        "#-1",
        "#+1",
        "##1",
        "#0x10",
        "# 1",
    ] {
        assert_eq!(parse_value(bad), None, "{bad:?}");
    }
}

/// Opens the Registers tab and clicks the value `value`, which opens its edit field.
fn open_edit(h: &mut Harness<'static, EvoApp>, value: &str) {
    click(h, "Registers");
    click(h, value);
}

/// Replaces the open edit field's text with `text`.
fn retype(h: &mut Harness<'static, EvoApp>, text: &str) {
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(2);
    h.get_by_role(Role::TextInput).type_text(text);
    h.run_steps(2);
}

fn press(h: &mut Harness<'static, EvoApp>, key: Key) {
    h.key_press(key);
    h.run_steps(3);
}

#[test]
fn edit_sp_applies() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = dock(fake);
    open_edit(&mut h, "0x20008000");
    let value = h.get_by_role(Role::TextInput).value();
    assert_eq!(value.as_deref(), Some("0x20008000"));
    retype(&mut h, "0x20001000");
    press(&mut h, Key::Enter);
    assert!(h.query_by_role(Role::TextInput).is_none());
    h.get_by_label("0x20001000");
    assert!(h.query_by_label("0x20008000").is_none());
    assert!(h.state().paused());
    // The field had the keyboard: no calculator key was pressed.
    assert!(probe.key_log().is_empty(), "{:?}", probe.key_log());
}

#[test]
fn edit_xpsr_takes_decimal() {
    let (mut h, _dir) = dock(FakeBackend::new());
    open_edit(&mut h, "0x01000000");
    retype(&mut h, "#16");
    press(&mut h, Key::Enter);
    h.get_by_label("0x00000010");
    assert!(h.query_by_label("0x01000000").is_none());
}

#[test]
fn edit_banked_applies() {
    let (mut h, _dir) = dock(FakeBackend::new());
    // msp_ns resets to 0x20093000 in the fake.
    open_edit(&mut h, "0x20093000");
    retype(&mut h, "0x20090000");
    press(&mut h, Key::Enter);
    assert!(h.query_by_role(Role::TextInput).is_none());
    h.get_by_label("0x20090000");
    assert!(h.query_by_label("0x20093000").is_none());
    // msp_s is untouched.
    h.get_by_label("0x30000800");
}

#[test]
fn unparsable_value_keeps_field() {
    let (mut h, _dir) = dock(FakeBackend::new());
    open_edit(&mut h, "0x20008000");
    retype(&mut h, "zz");
    press(&mut h, Key::Enter);
    h.get_by_label("Not a value");
    let value = h.get_by_role(Role::TextInput).value();
    assert_eq!(value.as_deref(), Some("zz"));
    // The field keeps the keyboard, so a fix can be typed at once.
    retype(&mut h, "0x20002000");
    assert!(h.query_by_label("Not a value").is_none());
    press(&mut h, Key::Enter);
    assert!(h.query_by_role(Role::TextInput).is_none());
    h.get_by_label("0x20002000");
}

#[test]
fn unparsable_then_escape_leaves_register() {
    let (mut h, _dir) = dock(FakeBackend::new());
    open_edit(&mut h, "0x20008000");
    retype(&mut h, "zz");
    press(&mut h, Key::Enter);
    h.get_by_label("Not a value");
    press(&mut h, Key::Escape);
    assert!(h.query_by_role(Role::TextInput).is_none());
    assert!(h.query_by_label("Not a value").is_none());
    h.get_by_label("0x20008000");
}

#[test]
fn escape_cancels_edit() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = dock(fake);
    open_edit(&mut h, "0x20008000");
    retype(&mut h, "0x20001000");
    press(&mut h, Key::Escape);
    assert!(h.query_by_role(Role::TextInput).is_none());
    h.get_by_label("0x20008000");
    assert!(h.query_by_label("0x20001000").is_none());
    // Escape is the calculator's CLEAR; the field kept it.
    assert!(probe.key_log().is_empty(), "{:?}", probe.key_log());
}

#[test]
fn no_edit_while_running() {
    let (mut h, _dir) = dock(FakeBackend::new());
    click(&mut h, "Registers");
    click(&mut h, "Continue");
    click(&mut h, "0x20008000");
    assert!(h.query_by_role(Role::TextInput).is_none());
    click(&mut h, "Pause");
    click(&mut h, "0x20008000");
    assert!(h.query_by_role(Role::TextInput).is_some());
}

#[test]
fn edit_closes_when_run_starts() {
    let (mut h, _dir) = dock(FakeBackend::new());
    open_edit(&mut h, "0x20008000");
    retype(&mut h, "0x20001000");
    click(&mut h, "Continue");
    assert!(h.query_by_role(Role::TextInput).is_none());
    h.get_by_label("0x20008000");
    click(&mut h, "Pause");
    // The cancelled edit does not come back.
    assert!(h.query_by_role(Role::TextInput).is_none());
}
