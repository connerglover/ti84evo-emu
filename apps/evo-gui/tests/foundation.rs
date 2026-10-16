//! Lane 0 foundation: speed setting, the new preferences, the platform seam, the log buffer
//! and the grouped sidebar; the feature framework, app requests and the debug seam.

mod common;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use common::{TempDir, harness_with, open_action, test_options};
use egui::accesskit::Role;
use egui::{Event, FontId, Modifiers, PointerButton, RawInput, pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp};
use evo_gui::backend::fake::FakeBackend;
use evo_gui::backend::{CLOCK_HZ, EmuBackend, Frame, Goal, RunStatus};
use evo_gui::feature::{AppRequest, Feature, FeatureCtx};
use evo_gui::input::{KeyLatch, MIN_HOLD_CYCLES, Source};
use evo_gui::keypad::KeyId;
use evo_gui::lcd::ScaleFilter;
use evo_gui::logbuf::{LOG_CAP, LogBuf};
use evo_gui::pacer::{Pacer, Speed, capped_budget};
use evo_gui::platform::{FakePlatform, FileFilter, Platform, PlatformRef};
use evo_gui::prefs::Prefs;
use evo_gui::store::Store;
use evo_gui::theme::{has_glyphs, install_fonts};
use evo_gui::toolbar::{Action, Group, not_available};

#[test]
fn speed_factors_and_labels() {
    assert_eq!(
        Speed::ALL,
        [Speed::X1, Speed::X2, Speed::X4, Speed::Unlimited]
    );
    assert_eq!(Speed::default(), Speed::X1);
    let factors: Vec<f64> = Speed::ALL.iter().map(|s| s.factor()).collect();
    assert_eq!(factors, [1.0, 2.0, 4.0, 1000.0]);
    let labels: Vec<&str> = Speed::ALL.iter().map(|s| s.label()).collect();
    assert_eq!(
        labels,
        [
            "Speed 1\u{d7}",
            "Speed 2\u{d7}",
            "Speed 4\u{d7}",
            "Speed unlimited"
        ]
    );
}

#[test]
fn unlimited_budget_is_wall_capped() {
    let p = Pacer {
        clock_hz: CLOCK_HZ,
        speed: Speed::Unlimited.factor(),
    };
    assert_eq!(p.budget(1.0 / 60.0), 2_600_000_000);
}

#[test]
fn old_prefs_load_with_new_defaults() {
    let p: Prefs = serde_json::from_str(r#"{"filter":"Smooth"}"#).expect("old prefs");
    assert_eq!(p.bundle, None);
    assert!(!p.compact);
    assert_eq!(p.speed, Speed::X1);
}

#[test]
fn prefs_roundtrip_new_fields() {
    let p = Prefs {
        bundle: Some(PathBuf::from("roms").join("os.84b2")),
        compact: true,
        speed: Speed::Unlimited,
        ..Prefs::default()
    };
    let json = serde_json::to_string(&p).expect("serialize");
    let back: Prefs = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, p);
    assert_eq!(back.speed, Speed::Unlimited);
    assert!(back.compact);
}

const STATE: FileFilter = FileFilter {
    name: "State",
    extensions: &["state"],
};

#[test]
fn fake_platform_queues_answers_in_order() {
    let p = FakePlatform::shared();
    p.queue_open(Some(PathBuf::from("a.state")));
    p.queue_open(None);
    assert_eq!(
        p.pick_open("First", &[STATE]),
        Some(PathBuf::from("a.state"))
    );
    assert_eq!(p.pick_open("Second", &[STATE]), None);
    assert_eq!(p.pick_open("Third", &[]), None);
    p.queue_save(Some(PathBuf::from("b.state")));
    assert_eq!(
        p.pick_save("Save", "x.state", &[STATE]),
        Some(PathBuf::from("b.state"))
    );
    assert_eq!(p.pick_save("Save again", "x.state", &[STATE]), None);
    assert_eq!(
        p.asked(),
        ["First", "Second", "Third", "Save", "Save again"]
    );
}

#[test]
fn fake_platform_default_backend_is_named_after_file() {
    let p = FakePlatform::shared();
    let bundle = Path::new("roms").join("os.84b2");
    let b = p.open_backend(&bundle, Path::new("nv")).expect("open");
    assert_eq!(b.name(), "os.84b2");
    assert_eq!(p.opened(), [bundle]);
}

#[test]
fn fake_platform_http_default_is_offline() {
    let p = FakePlatform::shared();
    assert_eq!(
        p.http_get("https://example.invalid"),
        Err("offline".to_owned())
    );
    p.set_http(Ok("{}".to_owned()));
    assert_eq!(p.http_get("https://example.invalid"), Ok("{}".to_owned()));
}

#[test]
fn fake_platform_logs_copies_urls_and_spawns() {
    let p = FakePlatform::shared();
    let frame = Frame {
        width: 1,
        height: 1,
        rgba: vec![0; 4],
    };
    assert_eq!(p.copy_image(&frame), Ok(()));
    p.fail_copy("no clipboard");
    assert_eq!(p.copy_image(&frame), Err("no clipboard".to_owned()));
    assert_eq!(p.copied(), 1);
    p.open_url("https://example.invalid/a").expect("url");
    assert_eq!(p.urls(), ["https://example.invalid/a"]);
    let args = vec!["--instance".to_owned(), "2".to_owned()];
    p.spawn_instance(&args).expect("spawn");
    assert_eq!(p.spawned(), [args]);
}

#[test]
fn platform_ref_is_send_sync() {
    fn assert_send_sync<T: Send + Sync + ?Sized>() {}
    assert_send_sync::<PlatformRef>();
    let fake = FakePlatform::shared();
    let shared: PlatformRef = fake.clone();
    let offline = std::thread::spawn(move || shared.http_get("x").is_err())
        .join()
        .expect("thread");
    assert!(offline);
}

#[test]
fn fake_platform_factory_gets_nv_dir() {
    let p = FakePlatform::shared();
    let seen = Arc::new(Mutex::new(None));
    let seen2 = seen.clone();
    p.set_backend_factory(Box::new(move |bundle, nv| {
        *seen2.lock().unwrap() = Some((bundle.to_path_buf(), nv.to_path_buf()));
        Ok(Box::new(FakeBackend::new().named("custom")) as Box<dyn EmuBackend>)
    }));
    let b = p
        .open_backend(Path::new("os.84b2"), Path::new("nv-dir"))
        .expect("open");
    assert_eq!(b.name(), "custom");
    assert_eq!(
        *seen.lock().unwrap(),
        Some((PathBuf::from("os.84b2"), PathBuf::from("nv-dir")))
    );
}

#[test]
fn logbuf_drains_new_lines_once() {
    let b = LogBuf::new(LOG_CAP);
    b.record("INFO x: a".to_owned());
    assert_eq!(b.drain(), ["INFO x: a"]);
    assert!(b.drain().is_empty());
}

#[test]
fn logbuf_keeps_the_last_cap_lines() {
    let b = LogBuf::new(3);
    for i in 0..5 {
        b.record(format!("line {i}"));
    }
    assert_eq!(b.drain(), ["line 2", "line 3", "line 4"]);
}

// ---------------------------------------------------------------------------
// Task 0.4: grouped sidebar, popovers, compact mode, reset layout.

/// A 520x960 harness on a fresh data dir with a fake platform.
fn sidebar_harness(test: &str, paused: bool) -> (Harness<'static, EvoApp>, TempDir) {
    let dir = TempDir::new(&format!("evo-gui-foundation-{test}"), false);
    let opts = AppOptions {
        start_paused: paused,
        ..test_options(&dir, FakePlatform::shared())
    };
    (harness_with(FakeBackend::new(), opts), dir)
}

/// A primary or secondary click at `pos`, then 3 steps.
fn click_at(h: &mut Harness<'static, EvoApp>, pos: egui::Pos2, button: PointerButton) {
    h.event(Event::PointerMoved(pos));
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    h.run_steps(3);
}

fn has_button(h: &Harness<'static, EvoApp>, label: &str) -> bool {
    h.query_by_role_and_label(Role::Button, label).is_some()
}

#[test]
fn group_and_action_tables() {
    assert_eq!(Action::ALL.len(), 39);
    let labels: HashSet<&str> = Action::ALL.iter().map(|a| a.label()).collect();
    assert_eq!(labels.len(), Action::ALL.len(), "labels are unique");
    for g in Group::ALL {
        let n = Action::ALL.iter().filter(|a| a.group() == g).count();
        if g == Group::Debug {
            assert_eq!(n, 1, "Debug holds only ToggleDebug");
        } else {
            assert!(n >= 1, "{g:?} has no action");
        }
    }
    assert_eq!(Action::ToggleDebug.group(), Group::Debug);
    let speeds: Vec<Action> = Action::ALL
        .into_iter()
        .filter(|a| matches!(a, Action::Speed(_)))
        .collect();
    assert_eq!(speeds, Speed::ALL.map(Action::Speed));
    assert_eq!(Action::Speed(Speed::X2).label(), "Speed 2\u{d7}");
}

#[test]
fn every_action_label_is_drawable() {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let mut out = ctx.run_ui(RawInput::default(), |_| {});
    out.textures_delta.clear();
    let font = FontId::proportional(14.0);
    for g in Group::ALL {
        assert!(has_glyphs(&ctx, &font, g.fallback()), "{g:?} fallback");
        assert!(has_glyphs(&ctx, &font, g.tooltip()), "{g:?} tooltip");
    }
    for a in Action::ALL {
        assert!(has_glyphs(&ctx, &font, a.label()), "{a:?} label");
    }
}

#[test]
fn popover_lists_its_group() {
    let (mut h, _dir) = sidebar_harness("popover", true);
    h.get_by_role_and_label(Role::Button, "Capture").click();
    h.run_steps(3);
    for label in [
        "Screenshot",
        "Copy screen",
        "Record animated PNG",
        "Record MP4",
    ] {
        assert!(has_button(&h, label), "{label}");
    }
    assert!(!has_button(&h, "Save state"));
    // Another group's button replaces it.
    h.get_by_role_and_label(Role::Button, "Calculator").click();
    h.run_steps(3);
    assert!(has_button(&h, "Save state"));
    assert!(has_button(&h, "Speed unlimited"));
    assert!(!has_button(&h, "Copy screen"));
    // Its own button closes it.
    h.get_by_role_and_label(Role::Button, "Calculator").click();
    h.run_steps(3);
    assert!(!has_button(&h, "Save state"));
}

#[test]
fn popover_closes_after_pick_and_on_outside_click() {
    let (mut h, _dir) = sidebar_harness("popclose", true);
    open_action(&mut h, Action::Screenshot);
    assert!(!has_button(&h, "Record MP4"), "closed after the pick");
    h.get_by_label_contains("Screenshot saved to");

    h.get_by_role_and_label(Role::Button, "Capture").click();
    h.run_steps(3);
    assert!(has_button(&h, "Record MP4"));
    // A click on the header, away from the sidebar and the popover.
    click_at(&mut h, pos2(300.0, 20.0), PointerButton::Primary);
    assert!(!has_button(&h, "Record MP4"), "closed by the outside click");
}

#[test]
fn save_state_from_popover_opens_slot_picker() {
    let (mut h, dir) = sidebar_harness("popsave", true);
    open_action(&mut h, Action::SaveState);
    assert!(!has_button(&h, "Load state"), "popover closed");
    h.get_by_label("Save to slot");
    h.get_by_label("Slot 1").click();
    h.run_steps(3);
    h.get_by_label("Saved slot 1");
    assert!(Store::new(dir.to_path_buf()).has_slot(1));
}

#[test]
fn speed_action_sets_pacer() {
    let (mut h, _dir) = sidebar_harness("speed", true);
    assert_eq!(h.state().speed(), Speed::X1);
    open_action(&mut h, Action::Speed(Speed::X4));
    assert_eq!(h.state().speed(), Speed::X4);
    assert_eq!(h.state().prefs().speed, Speed::X4);
    open_action(&mut h, Action::Speed(Speed::Unlimited));
    assert_eq!(h.state().speed(), Speed::Unlimited);
}

#[test]
fn pause_action_toggles() {
    let (mut h, _dir) = sidebar_harness("pause", false);
    assert!(!h.state().paused());
    open_action(&mut h, Action::TogglePause);
    assert!(h.state().paused());
    h.get_by_label("Paused");
    open_action(&mut h, Action::TogglePause);
    assert!(!h.state().paused());
}

#[test]
fn not_available_text() {
    assert_eq!(
        not_available(Action::ReportBug),
        "Report a bug is not available yet"
    );
}

#[test]
fn compact_hides_sidebar_and_context_menu_restores() {
    let (mut h, _dir) = sidebar_harness("compact", true);
    open_action(&mut h, Action::Compact);
    assert!(h.state().compact());
    assert!(h.state().prefs().compact);
    assert!(h.query_by_label("Calculator").is_none());
    assert!(h.query_by_label("TI-84 Evo Emulator").is_none());
    let p = h.state().calc_transform().to_screen(pos2(20.0, 20.0));
    click_at(&mut h, p, PointerButton::Secondary);
    h.get_by_label("Exit compact mode").click();
    h.run_steps(3);
    assert!(!h.state().compact());
    assert!(has_button(&h, "Calculator"));
    h.get_by_label("TI-84 Evo Emulator");
}

#[test]
fn compact_and_speed_restore_from_prefs() {
    let dir = TempDir::new("evo-gui-foundation-compactprefs", false);
    let opts = AppOptions {
        start_paused: true,
        prefs: Prefs {
            compact: true,
            speed: Speed::X2,
            ..Prefs::default()
        },
        ..test_options(&dir, FakePlatform::shared())
    };
    let h = harness_with(FakeBackend::new(), opts);
    assert!(h.state().compact());
    assert_eq!(h.state().speed(), Speed::X2);
    assert!(h.query_by_label("Calculator").is_none());
}

#[test]
fn reset_layout_closes_dock_and_compact() {
    let (mut h, _dir) = sidebar_harness("resetlayout", true);
    open_action(&mut h, Action::ToggleDebug);
    assert!(h.state().debug_open());
    open_action(&mut h, Action::ResetLayout);
    assert!(!h.state().debug_open());
    h.get_by_label("Layout reset");
    h.state_mut().run_action(Action::Compact);
    h.run_steps(2);
    assert!(h.state().compact());
    h.state_mut().run_action(Action::ResetLayout);
    h.run_steps(2);
    assert!(!h.state().compact());
    assert!(has_button(&h, "Calculator"));
}

// ---------------------------------------------------------------------------
// Task 0.5a: feature framework, app requests, debug seam.

/// A feature for the tests: owns one action, pushes scripted requests and reports a deadline.
#[derive(Default)]
struct TestFeature {
    owns: Option<Action>,
    on_action_push: Vec<AppRequest>,
    ui_push_compact_once: bool,
    on_exit_push: Vec<AppRequest>,
    deadline: Option<u64>,
    blocks: bool,
}

impl Feature for TestFeature {
    fn on_action(&mut self, action: Action, cx: &mut FeatureCtx<'_>) -> bool {
        if self.owns != Some(action) {
            return false;
        }
        cx.requests.append(&mut self.on_action_push);
        true
    }
    fn frame(&mut self, _cx: &mut FeatureCtx<'_>) {}
    fn ui(&mut self, _ctx: &egui::Context, cx: &mut FeatureCtx<'_>) {
        if std::mem::take(&mut self.ui_push_compact_once) {
            cx.requests.push(AppRequest::ApplyPrefs(Prefs {
                compact: true,
                ..cx.prefs.clone()
            }));
        }
    }
    fn active(&self, _action: Action) -> bool {
        false
    }
    fn next_deadline(&self) -> Option<u64> {
        self.deadline
    }
    fn blocks_input(&self) -> bool {
        self.blocks
    }
    fn on_exit(&mut self, cx: &mut FeatureCtx<'_>) {
        cx.requests.append(&mut self.on_exit_push);
    }
}

fn request_harness(
    test: &str,
    fake: FakeBackend,
    paused: bool,
) -> (Harness<'static, EvoApp>, TempDir) {
    let dir = TempDir::new(&format!("evo-gui-foundation-{test}"), false);
    let opts = AppOptions {
        start_paused: paused,
        ..test_options(&dir, FakePlatform::shared())
    };
    (harness_with(fake, opts), dir)
}

#[test]
fn replace_backend_persists_swaps_and_resyncs() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = request_harness("replace", fake, true);
    let b2 = FakeBackend::new().named("b2");
    let probe2 = b2.probe();
    h.state_mut().apply_request(AppRequest::ReplaceBackend {
        backend: Box::new(b2),
        rom: Some(PathBuf::from("b2.84b2")),
        persist_old: true,
    });
    assert_eq!(probe.persists(), 1);
    assert_eq!(h.state().backend().name(), "b2");
    assert_eq!(h.state().rom(), Some(Path::new("b2.84b2")));
    assert_eq!(h.state().status(), RunStatus::Running);
    // Resynced: every key released on the new backend.
    assert_eq!(probe2.key_log().len(), KeyId::ALL.len());
    assert!(!h.state().provisioning());
}

#[test]
fn replace_backend_without_persist_skips_it() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = request_harness("replacenopersist", fake, true);
    h.state_mut().apply_request(AppRequest::ReplaceBackend {
        backend: Box::new(FakeBackend::new().named("b2")),
        rom: None,
        persist_old: false,
    });
    assert_eq!(probe.persists(), 0);
    assert_eq!(h.state().backend().name(), "b2");
    assert_eq!(h.state().rom(), None);
}

#[test]
fn replace_with_unprovisioned_starts_provisioning() {
    let (mut h, _dir) = request_harness("replaceprov", FakeBackend::new(), true);
    h.state_mut().apply_request(AppRequest::ReplaceBackend {
        backend: Box::new(FakeBackend::new().needing_provision()),
        rom: None,
        persist_old: false,
    });
    assert!(h.state().provisioning());
}

#[test]
fn apply_prefs_updates_app() {
    let (mut h, _dir) = request_harness("applyprefs", FakeBackend::new(), true);
    let prefs = Prefs {
        speed: Speed::X4,
        compact: true,
        filter: ScaleFilter::Smooth,
        bundle: Some(PathBuf::from("x.84b2")),
        ..Prefs::default()
    };
    h.state_mut()
        .apply_request(AppRequest::ApplyPrefs(prefs.clone()));
    assert_eq!(h.state().speed(), Speed::X4);
    assert!(h.state().compact());
    assert_eq!(h.state().filter(), ScaleFilter::Smooth);
    assert_eq!(h.state().prefs(), prefs);
}

#[test]
fn set_paused_true_cancels_goal() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = request_harness("pausecancel", fake, false);
    h.state_mut().apply_request(AppRequest::SetPaused(true));
    assert!(h.state().paused());
    assert_eq!(probe.goal_cancels(), 1);
    h.state_mut().apply_request(AppRequest::SetPaused(false));
    assert!(!h.state().paused());
    assert_eq!(probe.goal_cancels(), 1);
}

#[test]
fn close_request_is_recorded() {
    let (mut h, _dir) = request_harness("close", FakeBackend::new(), true);
    assert!(!h.state().close_requested());
    h.state_mut().apply_request(AppRequest::Close);
    assert!(h.state().close_requested());
}

#[test]
fn deadline_caps_run_budget() {
    assert_eq!(capped_budget(5_000, 100, Some(1_100)), 1_000);
    assert_eq!(capped_budget(5_000, 100, Some(50)), 0);
    assert_eq!(capped_budget(5_000, 100, None), 5_000);
    assert_eq!(capped_budget(5_000, 100, Some(100)), 0);
    assert_eq!(capped_budget(500, 100, Some(1_100)), 500);
}

#[test]
fn latch_next_release_is_the_earliest_pending_release() {
    let mut latch = KeyLatch::new();
    assert_eq!(latch.next_release(), None);
    latch.press(KeyId::Enter, Source::Pointer, 100);
    // Held, no release pending yet.
    assert_eq!(latch.next_release(), None);
    latch.press(KeyId::Clear, Source::Pointer, 50);
    latch.release(KeyId::Enter, Source::Pointer, 200);
    assert_eq!(latch.next_release(), Some(100 + MIN_HOLD_CYCLES));
    latch.release(KeyId::Clear, Source::Pointer, 300);
    assert_eq!(latch.next_release(), Some(50 + MIN_HOLD_CYCLES));
    latch.tick(100 + MIN_HOLD_CYCLES);
    assert_eq!(latch.next_release(), None);
}

#[test]
fn feature_deadline_stops_the_run() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = request_harness("deadline", fake, false);
    let start = probe.cycles();
    h.state_mut().push_feature(Box::new(TestFeature {
        deadline: Some(start + 1_000),
        ..TestFeature::default()
    }));
    h.step();
    assert_eq!(probe.cycles(), start + 1_000);
    // At the deadline the budget is 0: nothing more runs.
    h.step();
    assert_eq!(probe.cycles(), start + 1_000);
}

#[test]
fn click_release_lands_at_min_hold() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = request_harness("clickhold", fake, false);
    // At 2x a frame is 5.2 Mcycles, which does not divide MIN_HOLD_CYCLES (7.8 M): without
    // the cap the release would land late.
    h.state_mut().run_action(Action::Speed(Speed::X2));
    h.get_by_label("enter").click();
    for _ in 0..20 {
        h.step();
        if probe.key_events().iter().any(|e| !e.pressed) {
            break;
        }
    }
    let events = probe.key_events();
    let press = events
        .iter()
        .find(|e| e.key == KeyId::Enter && e.pressed)
        .expect("press");
    let release = events
        .iter()
        .find(|e| e.key == KeyId::Enter && !e.pressed)
        .expect("release");
    assert_eq!(release.cycle, press.cycle + MIN_HOLD_CYCLES);
}

#[test]
fn requests_from_action_and_ui_apply_in_the_same_frame() {
    // Every action has a built-in owner, so the test feature goes ahead of them and
    // takes ReportBug before Help sees it.
    let (mut h, _dir) = request_harness("sameframe", FakeBackend::new(), false);
    h.state_mut().push_feature_first(Box::new(TestFeature {
        owns: Some(Action::ReportBug),
        on_action_push: vec![AppRequest::SetPaused(true)],
        ui_push_compact_once: true,
        ..TestFeature::default()
    }));
    h.state_mut().run_action(Action::ReportBug);
    assert!(h.state().paused());
    h.step();
    assert!(h.state().compact());
    // The feature took the action: no "not available" toast.
    assert!(
        h.query_by_label(&not_available(Action::ReportBug))
            .is_none()
    );
}

#[test]
fn on_exit_applies_feature_requests_then_persists() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = request_harness("onexit", fake, true);
    h.state_mut().push_feature(Box::new(TestFeature {
        on_exit_push: vec![AppRequest::Persist],
        ..TestFeature::default()
    }));
    assert_eq!(probe.persists(), 0);
    eframe::App::on_exit(h.state_mut());
    assert_eq!(probe.persists(), 2);
}

#[test]
fn blocking_feature_keeps_keys_from_the_calculator() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = request_harness("blocks", fake, false);
    h.state_mut().push_feature(Box::new(TestFeature {
        blocks: true,
        ..TestFeature::default()
    }));
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert!(probe.key_log().is_empty(), "{:?}", probe.key_log());
}

#[test]
fn pause_cancels_goal() {
    let fake = FakeBackend::new();
    let probe = fake.probe();
    let (mut h, _dir) = request_harness("pausegoal", fake, true);
    let armed = h
        .state_mut()
        .backend_mut()
        .begin_goal(Goal::RunTo(0x1000_0300));
    assert!(armed.unwrap());
    h.state_mut().apply_request(AppRequest::SetPaused(false));
    h.state_mut().apply_request(AppRequest::SetPaused(true));
    assert_eq!(probe.goal_cancels(), 1);
    assert_eq!(h.state_mut().backend_mut().run_for(10), RunStatus::Running);
}

#[test]
fn endless_goal_keeps_running_until_pause() {
    let fake = FakeBackend::new().goal_never_reached();
    let probe = fake.probe();
    let (mut h, _dir) = request_harness("endlessgoal", fake, true);
    let armed = h
        .state_mut()
        .backend_mut()
        .begin_goal(Goal::RunTo(0x1000_0300));
    assert!(armed.unwrap());
    h.state_mut().apply_request(AppRequest::SetPaused(false));
    let mut last = probe.cycles();
    for _ in 0..20 {
        h.step();
        assert!(!h.state().paused());
        assert_eq!(h.state().status(), RunStatus::Running);
        let now = probe.cycles();
        assert!(now > last, "cycles stalled at {now}");
        last = now;
    }
    h.state_mut().apply_request(AppRequest::SetPaused(true));
    assert_eq!(probe.goal_cancels(), 1);
    assert!(!probe.goal_armed());
    h.state_mut().apply_request(AppRequest::SetPaused(false));
    for _ in 0..5 {
        h.step();
        assert!(!h.state().paused());
        assert!(!matches!(h.state().status(), RunStatus::Goal { .. }));
    }
}

#[test]
fn features_are_reachable() {
    let (mut h, _dir) = request_harness("reachable", FakeBackend::new(), true);
    assert_eq!(h.state().features().rom.page(), None);
    assert_eq!(h.state().features().capture.recording(), None);
    assert_eq!(h.state_mut().features_mut().each().len(), 5);
    assert_eq!(h.state().features().next_deadline(), None);
    assert!(!h.state().features().blocks_input());
}

#[test]
fn instance_stub_dirs() {
    let base = TempDir::new("evo-gui-foundation-instance", true);
    let lock = evo_gui::instance::prepare(&base, None, None).unwrap();
    assert_eq!(lock.dir, base.to_path_buf());
    let lock = evo_gui::instance::prepare(&base, Some("a"), None).unwrap();
    assert_eq!(lock.dir, base.join("instances").join("a"));
    assert!(lock.dir.is_dir());
}

// ---------------------------------------------------------------------------
// Task 0.5b: start-up without a ROM.

#[test]
fn start_backend_cases() {
    use evo_gui::app::start_backend;
    let dir = Path::new("data-dir");

    // No bundle linked: no ROM, no notice, the wizard opens.
    let p = FakePlatform::shared();
    let platform: PlatformRef = p.clone();
    let s = start_backend(&Prefs::default(), &platform, dir);
    assert_eq!(s.backend.name(), "No ROM");
    assert_eq!(s.notice, None);
    assert!(s.open_wizard);
    assert!(p.opened().is_empty());

    // A linked bundle that does not open: no ROM, a notice, the wizard opens.
    let prefs = Prefs {
        bundle: Some(PathBuf::from("os.84b2")),
        ..Prefs::default()
    };
    let p = FakePlatform::shared();
    p.set_backend_factory(Box::new(|_, _| Err("bad".to_owned())));
    let platform: PlatformRef = p.clone();
    let s = start_backend(&prefs, &platform, dir);
    assert_eq!(s.backend.name(), "No ROM");
    assert_eq!(s.notice.as_deref(), Some("Cannot open os.84b2: bad"));
    assert!(s.open_wizard);

    // A linked bundle that opens: that backend, no wizard; flash lives in `dir/flash`.
    let p = FakePlatform::shared();
    let seen = Arc::new(Mutex::new(None));
    let seen2 = seen.clone();
    p.set_backend_factory(Box::new(move |bundle, nv| {
        *seen2.lock().unwrap() = Some((bundle.to_path_buf(), nv.to_path_buf()));
        Ok(Box::new(FakeBackend::new().named("opened")) as Box<dyn EmuBackend>)
    }));
    let platform: PlatformRef = p.clone();
    let s = start_backend(&prefs, &platform, dir);
    assert_eq!(s.backend.name(), "opened");
    assert_eq!(s.notice, None);
    assert!(!s.open_wizard);
    assert_eq!(
        *seen.lock().unwrap(),
        Some((PathBuf::from("os.84b2"), dir.join("flash")))
    );
}

#[test]
fn start_up_notice_is_shown_as_a_toast() {
    use evo_gui::app::start_backend;
    let prefs = Prefs {
        bundle: Some(PathBuf::from("os.84b2")),
        ..Prefs::default()
    };
    let p = FakePlatform::shared();
    p.set_backend_factory(Box::new(|_, _| Err("bad".to_owned())));
    let platform: PlatformRef = p.clone();
    let dir = TempDir::new("evo-gui-foundation-startnotice", false);
    let s = start_backend(&prefs, &platform, &dir);
    let opts = AppOptions {
        start_paused: true,
        notice: s.notice.clone(),
        open_wizard: s.open_wizard,
        ..test_options(&dir, platform)
    };
    let mut h = Harness::builder()
        .with_size([520.0, 960.0])
        .with_pixels_per_point(1.0)
        .build_eframe(|cc| EvoApp::new(&cc.egui_ctx, s.backend, opts));
    h.run_steps(1);
    h.get_by_label("Cannot open os.84b2: bad");
    assert_eq!(h.state().backend().name(), "No ROM");
}
