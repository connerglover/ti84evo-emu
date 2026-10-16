//! The debug dock: tab registry, run row, the Control tab, register editing, the
//! Memory tab (views, editing, search), the Visualizer and the Breakpoints and Watchpoints
//! tabs, on `FakeBackend`.

mod common;

use common::{TempDir, harness_with, test_options};
use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp};
use evo_gui::backend::fake::FakeBackend;
use evo_gui::backend::{AccessDir, EmuBackend, Goal, RunStatus, WatchKinds, Watchpoint};
use evo_gui::debug::{
    DebugTab, MAX_VIEWS, PixelFormat, SEARCH_CHUNK, SEARCH_CHUNKS_PER_FRAME, Search, VIS_MAX,
    cycle_text, find, parse_pattern, parse_size, parse_value, render, watch_text,
};
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

// ---- Memory tab ----

/// How many text fields show: the Find field, each view's address field, and an open
/// edit field, in that drawing order.
fn inputs(h: &Harness<'static, EvoApp>) -> usize {
    h.query_all_by_role(Role::TextInput).count()
}

/// Types `text` into text field `index` (drawing order), replacing what it held.
fn set_field(h: &mut Harness<'static, EvoApp>, index: usize, text: &str) {
    h.get_all_by_role(Role::TextInput)
        .nth(index)
        .expect("text field")
        .click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(1);
    h.get_all_by_role(Role::TextInput)
        .nth(index)
        .expect("text field")
        .type_text(text);
    h.run_steps(3);
}

/// The value of text field `index` (drawing order).
fn field_value(h: &Harness<'static, EvoApp>, index: usize) -> Option<String> {
    h.get_all_by_role(Role::TextInput)
        .nth(index)
        .expect("text field")
        .value()
}

/// Presses and releases the primary button at `pos`.
fn click_at(h: &mut Harness<'static, EvoApp>, pos: egui::Pos2) {
    h.hover_at(pos);
    h.run_steps(1);
    h.drag_at(pos);
    h.drop_at(pos);
    h.run_steps(3);
}

/// Clicks character `col` of the dump row whose text starts with `row_prefix`.
fn click_row_char(h: &mut Harness<'static, EvoApp>, row_prefix: &str, col: usize) {
    let node = h.get_by_label_contains(row_prefix);
    // A label keeps its text in its AccessKit value.
    let text = node.value().expect("row text");
    let rect = node.rect();
    let cw = rect.width() / text.chars().count() as f32;
    let pos = egui::pos2(rect.left() + (col as f32 + 0.5) * cw, rect.center().y);
    click_at(h, pos);
}

/// Column of the first hex digit of byte `j` (0..16) in a Bytes row.
fn byte_col(j: usize) -> usize {
    10 + 3 * j
}

/// Types `text` into the open edit field (the last text field) and presses Enter.
fn type_edit(h: &mut Harness<'static, EvoApp>, text: &str) {
    h.get_all_by_role(Role::TextInput)
        .last()
        .expect("edit field")
        .type_text(text);
    h.run_steps(2);
    press(h, Key::Enter);
}

#[test]
fn parse_pattern_cases() {
    assert_eq!(parse_pattern("de ad"), Some(vec![0xDE, 0xAD]));
    assert_eq!(parse_pattern("dead"), Some(vec![0xDE, 0xAD]));
    assert_eq!(parse_pattern("  DE  ad  "), Some(vec![0xDE, 0xAD]));
    assert_eq!(parse_pattern("\"ab\""), Some(b"ab".to_vec()));
    assert_eq!(parse_pattern("\"a b\""), Some(b"a b".to_vec()));
    for bad in [
        "xyz",
        "",
        "   ",
        "dea",
        "d ead",
        "\"\"",
        "\"ab",
        "0xde",
        "\"\u{e9}\"",
    ] {
        assert_eq!(parse_pattern(bad), None, "{bad:?}");
    }
}

#[test]
fn find_in_fake_memory() {
    let fake = FakeBackend::new();
    let regions = fake.memory_regions();
    assert_eq!(
        find(&fake, &regions, 0x2000_0000, &[5, 6, 7]),
        Some(0x2000_0005)
    );
    // The fake's bytes repeat every 251: the next match is at 251 + 5 = 0x100.
    assert_eq!(
        find(&fake, &regions, 0x2000_0006, &[5, 6, 7]),
        Some(0x2000_0100)
    );
    // No match from 0x2000_fff0 to the region's end: the search wraps to its start.
    assert_eq!(
        find(&fake, &regions, 0x2000_FFF0, &[5, 6, 7]),
        Some(0x2000_0005)
    );
    assert_eq!(find(&fake, &regions, 0x2000_0000, &[7, 6, 5]), None);
    assert_eq!(find(&fake, &regions, 0x2000_0000, &[]), None);
    // A start past every region: only the wrap finds it.
    assert_eq!(
        find(&fake, &regions, 0x3000_0000, &[5, 6, 7]),
        Some(0x2000_0005)
    );
}

#[test]
fn find_across_chunk_boundary() {
    let mut fake = FakeBackend::new();
    fake.poke(0x2000_3FFF, &[0xAB, 0xCD, 0xEF]).unwrap();
    let regions = fake.memory_regions();
    assert_eq!(SEARCH_CHUNK, 0x4000);
    assert_eq!(
        find(&fake, &regions, 0x2000_0000, &[0xAB, 0xCD, 0xEF]),
        Some(0x2000_3FFF)
    );
}

#[test]
fn search_steps_are_bounded() {
    assert_eq!(SEARCH_CHUNKS_PER_FRAME * SEARCH_CHUNK, 4 << 20);
    let fake = FakeBackend::new();
    let mut s = Search::new(fake.memory_regions(), 0x2000_0000, vec![7, 6, 5]);
    // One 0x4000-byte chunk of the 0x1_0000-byte region per call.
    assert_eq!(s.step(&fake, 1), None);
    let mut done = None;
    for _ in 0..4 {
        if let Some(r) = s.step(&fake, 1) {
            done = Some(r);
            break;
        }
    }
    assert_eq!(done, Some(None));
}

#[test]
fn memory_tab_has_one_view_and_find_row() {
    let (mut h, _dir) = dock(FakeBackend::new());
    click(&mut h, "Memory");
    assert_eq!(MAX_VIEWS, 4);
    for label in ["Find", "Next", "Add view", "Bytes", "Words"] {
        h.get_by_label(label);
    }
    // A single view cannot be closed.
    assert!(h.query_by_label("Close").is_none());
    assert_eq!(inputs(&h), 2);
    h.get_by_label_contains("20000000  00 01 02 03");
}

#[test]
fn edit_byte_pokes() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = dock(fake);
    click(&mut h, "Memory");
    click_row_char(&mut h, "20000000  ", byte_col(3));
    assert_eq!(inputs(&h), 3);
    assert_eq!(field_value(&h, 2).as_deref(), Some("03"));
    type_edit(&mut h, "aa");
    assert_eq!(probe.pokes(), [(0x2000_0003, vec![0xAA])]);
    assert_eq!(inputs(&h), 2);
    h.get_by_label_contains("20000000  00 01 02 aa 04");
    // The field had the keyboard: no calculator key was pressed.
    assert!(probe.key_log().is_empty(), "{:?}", probe.key_log());
}

#[test]
fn edit_word_pokes_little_endian() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = dock(fake);
    click(&mut h, "Memory");
    click(&mut h, "Words");
    h.get_by_label_contains("20000000  03020100 07060504");
    // The second word's first digit.
    click_row_char(&mut h, "20000000  ", 10 + 9);
    assert_eq!(field_value(&h, 2).as_deref(), Some("07060504"));
    type_edit(&mut h, "deadbeef");
    assert_eq!(probe.pokes(), [(0x2000_0004, vec![0xEF, 0xBE, 0xAD, 0xDE])]);
    h.get_by_label_contains("20000000  03020100 deadbeef");
}

#[test]
fn unparsable_byte_keeps_field() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = dock(fake);
    click(&mut h, "Memory");
    click_row_char(&mut h, "20000000  ", byte_col(3));
    type_edit(&mut h, "zz");
    h.get_by_label("Not a value");
    assert_eq!(inputs(&h), 3);
    press(&mut h, Key::Escape);
    click_row_char(&mut h, "20000000  ", byte_col(3));
    // Too big for a byte.
    type_edit(&mut h, "#300");
    h.get_by_label("Not a value");
    press(&mut h, Key::Escape);
    assert_eq!(inputs(&h), 2);
    assert!(probe.pokes().is_empty());
    // Escape is the calculator's CLEAR; the field kept it.
    assert!(probe.key_log().is_empty(), "{:?}", probe.key_log());
}

#[test]
fn edit_outside_notes_error() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = dock(fake);
    click(&mut h, "Memory");
    set_field(&mut h, 1, "0x1fff_fff8");
    click_row_char(&mut h, "1ffffff8  ", byte_col(0));
    type_edit(&mut h, "aa");
    assert!(probe.pokes().is_empty());
    h.get_by_label_contains("Cannot write 0x1ffffff8");
    h.get_by_label_contains("1ffffff8  ?? ?? ??");
}

#[test]
fn no_memory_edit_while_running() {
    let (mut h, _dir) = dock(FakeBackend::new());
    click(&mut h, "Memory");
    click(&mut h, "Continue");
    click_row_char(&mut h, "20000000  ", byte_col(3));
    assert_eq!(inputs(&h), 2);
    click(&mut h, "Pause");
    click_row_char(&mut h, "20000000  ", byte_col(3));
    assert_eq!(inputs(&h), 3);
}

#[test]
fn add_and_close_views() {
    let (mut h, _dir) = dock(FakeBackend::new());
    click(&mut h, "Memory");
    for n in 2..=MAX_VIEWS {
        click(&mut h, "Add view");
        assert_eq!(h.get_all_by_label("Close").count(), n);
        assert_eq!(inputs(&h), n + 1);
    }
    // At the cap "Add view" is disabled.
    click(&mut h, "Add view");
    assert_eq!(h.get_all_by_label("Close").count(), MAX_VIEWS);
    set_field(&mut h, 2, "0x20000100");
    h.get_by_label_contains("20000100  05 06 07");
    // Closing the first view keeps the others.
    h.get_all_by_label("Close").next().unwrap().click();
    h.run_steps(3);
    assert_eq!(inputs(&h), MAX_VIEWS);
    assert_eq!(field_value(&h, 1).as_deref(), Some("0x20000100"));
    while h.query_all_by_label("Close").count() > 0 {
        h.get_all_by_label("Close").next().unwrap().click();
        h.run_steps(3);
    }
    assert_eq!(inputs(&h), 2);
}

#[test]
fn search_next_jumps_and_continues() {
    let (mut h, _dir) = dock(FakeBackend::new());
    click(&mut h, "Memory");
    set_field(&mut h, 0, "05 06 07");
    click(&mut h, "Next");
    // The hit 0x20000005 is in the row at 0x20000000.
    assert_eq!(field_value(&h, 1).as_deref(), Some("0x20000000"));
    assert!(h.query_by_label("Not found").is_none());
    click(&mut h, "Next");
    assert_eq!(field_value(&h, 1).as_deref(), Some("0x20000100"));
    h.get_by_label_contains("20000100  05 06 07");
}

#[test]
fn search_not_found_and_bad_pattern() {
    let (mut h, _dir) = dock(FakeBackend::new());
    click(&mut h, "Memory");
    set_field(&mut h, 0, "\"zz\"");
    click(&mut h, "Next");
    h.get_by_label("Not found");
    assert_eq!(field_value(&h, 1).as_deref(), Some("0x20000000"));
    // Editing the pattern clears the result.
    set_field(&mut h, 0, "xyz");
    assert!(h.query_by_label("Not found").is_none());
    click(&mut h, "Next");
    h.get_by_label("Not a pattern");
}

#[test]
fn enter_in_find_searches_without_pressing_keys() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = dock(fake);
    click(&mut h, "Memory");
    set_field(&mut h, 0, "05 06 07");
    press(&mut h, Key::Enter);
    press(&mut h, Key::Enter);
    // Two searches: the hits at 0x20000005 and 0x20000100.
    assert_eq!(field_value(&h, 1).as_deref(), Some("0x20000100"));
    assert!(probe.key_log().is_empty(), "{:?}", probe.key_log());
}

// ---- Visualizer ----

const MAGENTA: [u8; 4] = [0xFF, 0x00, 0xFF, 0xFF];

fn bytes(v: &[u8]) -> Vec<Option<u8>> {
    v.iter().copied().map(Some).collect()
}

#[test]
fn pixel_format_labels_and_bits() {
    let labels: Vec<&str> = PixelFormat::ALL.iter().map(|f| f.label()).collect();
    assert_eq!(labels, ["RGB565", "RGB888", "Gray 8-bit", "1-bit"]);
    let bits: Vec<u32> = PixelFormat::ALL.iter().map(|f| f.bits()).collect();
    assert_eq!(bits, [16, 24, 8, 1]);
    assert_eq!(PixelFormat::default(), PixelFormat::Rgb565);
    assert_eq!(VIS_MAX, 1024);
}

#[test]
fn rgb565_white_and_red() {
    let px = render(&bytes(&[0xFF, 0xFF, 0x00, 0xF8]), PixelFormat::Rgb565, 2, 1);
    assert_eq!(px, [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0xFF]);
    // Green is bits 10:5, blue bits 4:0.
    let px = render(&bytes(&[0xE0, 0x07, 0x1F, 0x00]), PixelFormat::Rgb565, 2, 1);
    assert_eq!(px, [0x00, 0xFF, 0x00, 0xFF, 0x00, 0x00, 0xFF, 0xFF]);
}

#[test]
fn rgb888_and_gray() {
    let px = render(&bytes(&[0x12, 0x34, 0x56]), PixelFormat::Rgb888, 1, 1);
    assert_eq!(px, [0x12, 0x34, 0x56, 0xFF]);
    let px = render(&bytes(&[0x00, 0x80, 0xFF]), PixelFormat::Gray8, 3, 1);
    assert_eq!(
        px,
        [
            0, 0, 0, 0xFF, 0x80, 0x80, 0x80, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF
        ]
    );
}

#[test]
fn mono1_bits() {
    // MSB first; 1 is black, 0 white.
    let black = [0x00, 0x00, 0x00, 0xFF];
    let white = [0xFF, 0xFF, 0xFF, 0xFF];
    let px = render(&bytes(&[0b1010_0000]), PixelFormat::Mono1, 4, 1);
    assert_eq!(px, [black, white, black, white].concat());
    // The ninth pixel is the next byte's MSB.
    let px = render(&bytes(&[0x00, 0x80]), PixelFormat::Mono1, 9, 1);
    assert_eq!(&px[28..32], white);
    assert_eq!(&px[32..36], black);
}

#[test]
fn unreadable_is_magenta() {
    for fmt in PixelFormat::ALL {
        let px = render(&[None; 3], fmt, 1, 1);
        assert_eq!(px, MAGENTA, "{fmt:?}");
    }
    // One unreadable byte spoils only its own pixel.
    let px = render(
        &[Some(0xFF), Some(0xFF), None, Some(0)],
        PixelFormat::Rgb565,
        2,
        1,
    );
    assert_eq!(px, [[0xFF; 4], MAGENTA].concat());
}

#[test]
fn short_input_pads_magenta() {
    let px = render(&bytes(&[0x40]), PixelFormat::Gray8, 2, 2);
    assert_eq!(px.len(), 16);
    assert_eq!(&px[..4], [0x40, 0x40, 0x40, 0xFF]);
    for p in px[4..].chunks(4) {
        assert_eq!(p, MAGENTA);
    }
    // A half pixel is magenta too.
    let px = render(&bytes(&[0xFF]), PixelFormat::Rgb565, 1, 1);
    assert_eq!(px, MAGENTA);
    assert_eq!(render(&[], PixelFormat::Mono1, 3, 1), [MAGENTA; 3].concat());
}

#[test]
fn sizes_are_clamped() {
    assert_eq!(parse_size("5000"), Some(VIS_MAX));
    assert_eq!(parse_size(" 64 "), Some(64));
    assert_eq!(parse_size("0"), Some(1));
    assert_eq!(parse_size("99999999999999"), Some(VIS_MAX));
    for bad in ["", "abc", "-3", "0x10", "1.5"] {
        assert_eq!(parse_size(bad), None, "{bad:?}");
    }
    // Through the UI: the width field takes 5000 as 1024.
    let (mut h, _dir) = visualizer(FakeBackend::new());
    set_field(&mut h, 1, "5000");
    h.get_by_label("1024 × 240, 491520 bytes");
    // Leaving the field shows the clamped value.
    press(&mut h, Key::Enter);
    assert_eq!(field_value(&h, 1).as_deref(), Some("1024"));
    set_field(&mut h, 2, "0");
    h.get_by_label("1024 × 1, 2048 bytes");
}

/// Opens the Visualizer; its text fields in drawing order are address, width, height.
fn visualizer(fake: FakeBackend) -> (Harness<'static, EvoApp>, TempDir) {
    let (mut h, dir) = dock(fake);
    click(&mut h, "Visualizer");
    (h, dir)
}

#[test]
fn visualizer_defaults() {
    let (h, _dir) = visualizer(FakeBackend::new());
    assert_eq!(inputs(&h), 3);
    assert_eq!(field_value(&h, 0).as_deref(), Some("0x20048000"));
    assert_eq!(field_value(&h, 1).as_deref(), Some("320"));
    assert_eq!(field_value(&h, 2).as_deref(), Some("240"));
    for label in ["RGB565", "RGB888", "Gray 8-bit", "1-bit"] {
        h.get_by_label(label);
    }
    h.get_by_label("320 × 240, 153600 bytes");
}

#[test]
fn visualizer_format_and_bad_fields() {
    let (mut h, _dir) = visualizer(FakeBackend::new());
    click(&mut h, "1-bit");
    h.get_by_label("320 × 240, 9600 bytes");
    click(&mut h, "Gray 8-bit");
    h.get_by_label("320 × 240, 76800 bytes");
    set_field(&mut h, 0, "zz");
    h.get_by_label("Not a hex address");
    set_field(&mut h, 1, "wide");
    h.get_by_label("Not a size");
    // The last good size stays in use.
    h.get_by_label("320 × 240, 76800 bytes");
}

#[test]
fn visualizer_fields_take_keys_from_calculator() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = visualizer(fake);
    set_field(&mut h, 0, "0x20000000");
    press(&mut h, Key::Enter);
    set_field(&mut h, 1, "64");
    press(&mut h, Key::Enter);
    assert!(probe.key_log().is_empty(), "{:?}", probe.key_log());
}

// --- Breakpoints and Watchpoints ---

#[test]
fn watch_text_format() {
    let w = Watchpoint {
        start: 0x2000_0100,
        len: 4,
        kinds: WatchKinds {
            write: true,
            ..WatchKinds::default()
        },
    };
    assert_eq!(watch_text(&w), "0x20000100..0x20000103 -W-");
    let all = Watchpoint {
        start: 0xFFFF_FFFF,
        len: 1,
        kinds: WatchKinds {
            read: true,
            write: true,
            exec: true,
        },
    };
    assert_eq!(watch_text(&all), "0xffffffff..0xffffffff RWX");
}

/// The dock open on tab `tab`.
fn dock_on(fake: FakeBackend, tab: &str) -> (Harness<'static, EvoApp>, TempDir) {
    let (mut h, dir) = dock(fake);
    click(&mut h, tab);
    (h, dir)
}

/// Clicks the `n`th widget labelled `label` (drawing order).
fn click_nth(h: &mut Harness<'static, EvoApp>, label: &str, n: usize) {
    h.get_all_by_label(label).nth(n).expect(label).click();
    h.run_steps(3);
}

#[test]
fn add_and_remove_breakpoints() {
    let (mut h, _dir) = dock_on(FakeBackend::new(), "Breakpoints");
    h.get_by_label("No breakpoints");
    set_field(&mut h, 0, "0x10000300");
    click(&mut h, "Add");
    assert_eq!(h.state().backend().breakpoints(), [0x1000_0300]);
    h.get_by_label("0x10000300");
    // Adding one already set keeps it (no toggle off).
    set_field(&mut h, 0, "0x10000300");
    click(&mut h, "Add");
    assert_eq!(h.state().backend().breakpoints(), [0x1000_0300]);
    // Enter in the field adds too.
    set_field(&mut h, 0, "1000_0400");
    press(&mut h, Key::Enter);
    assert_eq!(
        h.state().backend().breakpoints(),
        [0x1000_0300, 0x1000_0400]
    );
    assert_eq!(h.get_all_by_label("Remove").count(), 2);
    click_nth(&mut h, "Remove", 0);
    assert_eq!(h.state().backend().breakpoints(), [0x1000_0400]);
    assert!(h.query_by_label("0x10000300").is_none());
    h.get_by_label("0x10000400");
}

#[test]
fn breakpoint_garbage_is_refused() {
    let (mut h, _dir) = dock_on(FakeBackend::new(), "Breakpoints");
    set_field(&mut h, 0, "zz");
    click(&mut h, "Add");
    h.get_by_label("Not a hex address");
    assert!(h.state().backend().breakpoints().is_empty());
    // The next edit clears the message.
    set_field(&mut h, 0, "0x10000300");
    assert!(h.query_by_label("Not a hex address").is_none());
}

#[test]
fn breakpoints_tab_lists_disassembly_breakpoints() {
    let mut fake = FakeBackend::new();
    fake.toggle_breakpoint(0x1000_0204);
    let (h, _dir) = dock_on(fake, "Breakpoints");
    h.get_by_label("0x10000204");
    assert!(h.query_by_label("No breakpoints").is_none());
}

#[test]
fn remove_all_breakpoints() {
    let mut fake = FakeBackend::new();
    fake.set_breakpoints(vec![0x1000_0300, 0x1000_0400, 0x1000_0500]);
    let (mut h, _dir) = dock_on(fake, "Breakpoints");
    assert_eq!(h.get_all_by_label("Remove").count(), 3);
    click(&mut h, "Remove all");
    assert!(h.state().backend().breakpoints().is_empty());
    assert_eq!(h.query_all_by_label("Remove").count(), 0);
    h.get_by_label("No breakpoints");
}

#[test]
fn breakpoint_edits_only_while_paused() {
    let (mut h, _dir) = dock_on(FakeBackend::new(), "Breakpoints");
    click(&mut h, "Continue");
    set_field(&mut h, 0, "0x10000300");
    click(&mut h, "Add");
    assert!(h.state().backend().breakpoints().is_empty());
    click(&mut h, "Pause");
    click(&mut h, "Add");
    assert_eq!(h.state().backend().breakpoints(), [0x1000_0300]);
}

/// Fills the Watchpoints add row: start, length.
fn fill_watch(h: &mut Harness<'static, EvoApp>, start: &str, len: &str) {
    set_field(h, 0, start);
    set_field(h, 1, len);
}

#[test]
fn watchpoints_tab_add_row() {
    let (h, _dir) = dock_on(FakeBackend::new(), "Watchpoints");
    for label in ["Start", "Length", "R", "W", "X", "Add", "Remove all"] {
        h.get_by_label(label);
    }
    assert_eq!(inputs(&h), 2);
    h.get_by_label("No watchpoints");
}

#[test]
fn add_watchpoint_then_run_stops() {
    let (mut h, _dir) = dock_on(FakeBackend::new(), "Watchpoints");
    fill_watch(&mut h, "0x20000100", "4");
    click(&mut h, "Add");
    let w = Watchpoint {
        start: 0x2000_0100,
        len: 4,
        kinds: WatchKinds {
            write: true,
            ..WatchKinds::default()
        },
    };
    assert_eq!(h.state().backend().watchpoints(), [w]);
    h.get_by_label("0x20000100..0x20000103 -W-");
    click(&mut h, "Continue");
    h.run_steps(3);
    assert!(h.state().paused());
    assert!(h.state().debug_open());
    assert_eq!(
        h.state().status(),
        RunStatus::Watchpoint {
            addr: 0x2000_0100,
            dir: AccessDir::Write,
            value: 0x2A,
            pc: 0x1000_0200,
        }
    );
    h.get_by_label("Watchpoint (write) at 0x20000100, pc 0x10000200");
}

#[test]
fn add_appends_to_existing_watchpoints() {
    let (mut h, _dir) = dock_on(FakeBackend::new(), "Watchpoints");
    fill_watch(&mut h, "0x20000000", "#16");
    click(&mut h, "R");
    click(&mut h, "W");
    click(&mut h, "Add");
    fill_watch(&mut h, "0x10000200", "2");
    click(&mut h, "R");
    click(&mut h, "X");
    click(&mut h, "Add");
    let texts: Vec<String> = h
        .state()
        .backend()
        .watchpoints()
        .iter()
        .map(watch_text)
        .collect();
    assert_eq!(
        texts,
        ["0x20000000..0x2000000f R--", "0x10000200..0x10000201 --X"]
    );
    h.get_by_label("0x20000000..0x2000000f R--");
    h.get_by_label("0x10000200..0x10000201 --X");
}

#[test]
fn invalid_watchpoint_is_refused() {
    let (mut h, _dir) = dock_on(FakeBackend::new(), "Watchpoints");
    // Length 0.
    fill_watch(&mut h, "0x20000100", "0");
    click(&mut h, "Add");
    h.get_by_label("Invalid watchpoint");
    assert!(h.state().backend().watchpoints().is_empty());
    // Past the end of the address space; an edit first clears the message.
    fill_watch(&mut h, "0xffffffff", "2");
    assert!(h.query_by_label("Invalid watchpoint").is_none());
    click(&mut h, "Add");
    h.get_by_label("Invalid watchpoint");
    // Not an address, not a length.
    fill_watch(&mut h, "zz", "4");
    click(&mut h, "Add");
    h.get_by_label("Invalid watchpoint");
    fill_watch(&mut h, "0x20000100", "four");
    click(&mut h, "Add");
    h.get_by_label("Invalid watchpoint");
    // No kind ticked.
    fill_watch(&mut h, "0x20000100", "4");
    click(&mut h, "W");
    click(&mut h, "Add");
    h.get_by_label("Invalid watchpoint");
    assert!(h.state().backend().watchpoints().is_empty());
    // Valid again.
    click(&mut h, "W");
    click(&mut h, "Add");
    assert!(h.query_by_label("Invalid watchpoint").is_none());
    assert_eq!(h.state().backend().watchpoints().len(), 1);
}

#[test]
fn remove_and_remove_all_watchpoints() {
    let mut fake = FakeBackend::new();
    let kinds = WatchKinds {
        read: true,
        ..WatchKinds::default()
    };
    let ws: Vec<Watchpoint> = [0x2000_0000, 0x2000_0010, 0x2000_0020]
        .into_iter()
        .map(|start| Watchpoint {
            start,
            len: 4,
            kinds,
        })
        .collect();
    fake.set_watchpoints(ws.clone()).unwrap();
    let (mut h, _dir) = dock_on(fake, "Watchpoints");
    assert_eq!(h.get_all_by_label("Remove").count(), 3);
    click_nth(&mut h, "Remove", 1);
    assert_eq!(h.state().backend().watchpoints(), [ws[0], ws[2]]);
    assert!(h.query_by_label("0x20000010..0x20000013 R--").is_none());
    click(&mut h, "Remove all");
    assert!(h.state().backend().watchpoints().is_empty());
    h.get_by_label("No watchpoints");
}

#[test]
fn watchpoint_edits_only_while_paused() {
    let (mut h, _dir) = dock_on(FakeBackend::new(), "Watchpoints");
    click(&mut h, "Continue");
    fill_watch(&mut h, "0x20000000", "4");
    click(&mut h, "Add");
    assert!(h.state().backend().watchpoints().is_empty());
    assert!(!h.state().paused());
}

#[test]
fn watch_fields_take_keys_from_calculator() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = dock_on(fake, "Watchpoints");
    set_field(&mut h, 0, "0x20000000");
    set_field(&mut h, 1, "4");
    press(&mut h, Key::Enter);
    // Enter in a field adds the watchpoint and stays away from the calculator.
    assert_eq!(h.state().backend().watchpoints().len(), 1);
    assert!(probe.key_log().is_empty(), "{:?}", probe.key_log());
}

#[test]
fn breakpoint_thumb_bit_is_dropped() {
    let (mut h, _dir) = dock_on(FakeBackend::new(), "Breakpoints");
    set_field(&mut h, 0, "0x10000301");
    click(&mut h, "Add");
    assert_eq!(h.state().backend().breakpoints(), [0x1000_0300]);
}
