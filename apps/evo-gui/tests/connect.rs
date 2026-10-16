mod common;

use common::{TempDir, harness_with, open_action, test_options};
use egui_kittest::{Harness, kittest::Queryable};
use evo_gui::app::EvoApp;
use evo_gui::backend::fake::FakeBackend;
use evo_gui::connect::types::{CalculatorFile, ReceivedFile, UsbResult};
use evo_gui::platform::FakePlatform;
use evo_gui::toolbar::Action;

fn directory() -> UsbResult {
    UsbResult::Directory {
        files: vec![
            CalculatorFile {
                name: "HELLO".into(),
                token_name: "48454C4C4F".into(),
                kind: 21,
                size: 328,
                archived: false,
                extension: "8xv2".into(),
            },
            CalculatorFile {
                name: "L1".into(),
                token_name: "5D00".into(),
                kind: 1,
                size: 67,
                archived: false,
                extension: "8xl2".into(),
            },
            CalculatorFile {
                name: "Image1".into(),
                token_name: "E8B0".into(),
                kind: 26,
                size: 33673,
                archived: true,
                extension: "8ca2".into(),
            },
        ],
        info: "TI-84 Evo\nOS 7.1.0.4421".into(),
    }
}

fn finish(h: &mut Harness<'static, EvoApp>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while h.state().features().connect.busy() {
        assert!(
            std::time::Instant::now() < deadline,
            "USB worker did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        h.run_steps(1);
    }
    h.run_steps(3);
}

#[test]
fn usb_uses_and_returns_the_same_backend_without_running_it_on_the_ui_thread() {
    let dir = TempDir::new("evo-usb-ui", true);
    let mut fake = FakeBackend::new()
        .with_state_tag("this-calculator")
        .with_usb_results(vec![Ok(directory())]);
    let gate = fake.usb_gate();
    let probe = fake.probe();
    let mut options = test_options(&dir, FakePlatform::shared());
    options.start_paused = true;
    let mut h = harness_with(fake, options);
    open_action(&mut h, Action::UsbTools);
    assert!(h.state().features().connect.busy());
    h.run_steps(5);
    assert_eq!(probe.run_calls(), 0);
    h.state_mut().run_action(Action::Reset);
    h.state_mut().run_action(Action::ResetEverything);
    assert!(h.query_by_label("Reset everything?").is_none());
    assert_eq!(probe.resets(), 0, "reset must not race a USB worker");
    gate.send(()).unwrap();
    finish(&mut h);
    assert_eq!(h.state().backend().state_tag(), "this-calculator");
    assert_eq!(
        probe.cycles(),
        1234,
        "the USB worker clocked the original machine"
    );
    assert_eq!(probe.usb_calls(), ["Reading calculator files"]);
    assert_eq!(probe.persists(), 1);
    h.get_by_label("HELLO");
    h.get_by_label("Image1");
    assert_eq!(
        h.state().features().connect.status(),
        "3 files on this calculator."
    );
}

#[test]
fn cancelling_a_transfer_restores_the_backend_and_persists_guest_changes() {
    let dir = TempDir::new("evo-usb-cancel", true);
    let mut fake = FakeBackend::new().with_usb_results(vec![Ok(directory())]);
    let _gate = fake.usb_gate();
    let probe = fake.probe();
    let mut h = harness_with(fake, test_options(&dir, FakePlatform::shared()));
    open_action(&mut h, Action::UsbTools);
    h.get_by_label("Cancel transfer").click();
    h.run_steps(2);
    finish(&mut h);
    assert!(h.state().backend().usb_available());
    assert_eq!(probe.persists(), 1);
    assert_eq!(h.state().features().connect.status(), "transfer cancelled");
}

#[test]
fn shutdown_cancels_and_joins_usb_before_the_last_save() {
    let dir = TempDir::new("evo-usb-shutdown", true);
    let mut fake = FakeBackend::new().with_usb_results(vec![Ok(directory())]);
    let _gate = fake.usb_gate();
    let probe = fake.probe();
    let mut h = harness_with(fake, test_options(&dir, FakePlatform::shared()));
    open_action(&mut h, Action::UsbTools);
    eframe::App::on_exit(h.state_mut());
    assert!(h.state().backend().usb_available());
    assert!(probe.persists() >= 2);
}

#[test]
fn demo_explains_why_usb_is_unavailable() {
    let dir = TempDir::new("evo-usb-demo", true);
    let mut h = harness_with(
        FakeBackend::new(),
        test_options(&dir, FakePlatform::shared()),
    );
    open_action(&mut h, Action::UsbTools);
    assert!(!h.state().features().connect.busy());
    h.get_by_label("Open a calculator OS bundle to use USB transfers. The demonstration calculator has no USB device.");
    assert!(!h.state().backend().usb_available());
}

#[test]
fn file_search_matches_readable_types_and_recovers_from_no_matches() {
    let dir = TempDir::new("evo-usb-search", true);
    let fake = FakeBackend::new().with_usb_results(vec![Ok(directory())]);
    let mut h = harness_with(fake, test_options(&dir, FakePlatform::shared()));
    open_action(&mut h, Action::UsbTools);
    finish(&mut h);
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    h.run_steps(1);
    h.get_by_role(egui::accesskit::Role::TextInput)
        .type_text("List");
    h.run_steps(2);
    h.get_by_label("L1");
    assert!(h.query_by_label("HELLO").is_none());
    h.get_by_role(egui::accesskit::Role::TextInput)
        .type_text("doesnotexist");
    h.run_steps(2);
    h.get_by_label("No files match your search.");
    h.get_by_label("Clear filter").click();
    h.run_steps(2);
    h.get_by_label("HELLO");
}

#[test]
fn partial_receive_saves_completed_files_and_keeps_the_error_report() {
    let dir = TempDir::new("evo-usb-partial", true);
    let platform = FakePlatform::shared();
    platform.queue_open(Some(dir.to_path_buf()));
    let fake = FakeBackend::new().with_usb_results(vec![
        Ok(directory()),
        Ok(UsbResult::Batch {
            files: vec![ReceivedFile {
                name: "HELLO.py".into(),
                bytes: b"print(1)".to_vec(),
            }],
            errors: vec!["L1: calculator rejected this file".into()],
            message: "Received 1 of 3 files.".into(),
        }),
    ]);
    let mut h = harness_with(fake, test_options(&dir, platform));
    open_action(&mut h, Action::UsbTools);
    finish(&mut h);
    h.get_by_label("Select visible").click();
    h.run_steps(2);
    h.get_by_label("Save selected…").click();
    h.run_steps(2);
    finish(&mut h);
    assert_eq!(std::fs::read(dir.join("HELLO.py")).unwrap(), b"print(1)");
    assert!(
        h.state()
            .features()
            .connect
            .status()
            .contains("L1: calculator rejected")
    );
    h.get_by_label("Last transfer");
}

#[test]
fn multiple_files_are_reviewed_before_sending_and_directory_refreshes_afterward() {
    let dir = TempDir::new("evo-usb-send-many", true);
    let paths: Vec<_> = ["FIRST.py", "SECOND.py"]
        .into_iter()
        .map(|name| {
            let path = dir.join(name);
            std::fs::write(&path, b"print(1)").unwrap();
            path
        })
        .collect();
    let platform = FakePlatform::shared();
    platform.queue_open_many(paths);
    let fake = FakeBackend::new().with_usb_results(vec![
        Ok(directory()),
        Ok(UsbResult::Batch {
            files: vec![],
            errors: vec![],
            message: "Sent 2 files.".into(),
        }),
        Ok(directory()),
    ]);
    let probe = fake.probe();
    let mut h = harness_with(fake, test_options(&dir, platform));
    open_action(&mut h, Action::UsbTools);
    finish(&mut h);
    h.get_by_label("Send files…").click();
    h.run_steps(2);
    h.get_by_label("Send 2 files");
    assert_eq!(
        probe.usb_calls().len(),
        1,
        "opening the chooser must not send files"
    );
    h.get_by_label("Send to calculator").click();
    h.run_steps(2);
    finish(&mut h);
    assert_eq!(
        probe.usb_calls(),
        [
            "Reading calculator files",
            "Sending files",
            "Reading calculator files"
        ]
    );
    h.get_by_label("Last transfer");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images need --features snapshots"
)]
fn snapshot_usb_files() {
    let _gpu = common::gpu_lock();
    let dir = TempDir::new("evo-usb-snapshot", true);
    let fake = FakeBackend::new().with_usb_results(vec![Ok(directory())]);
    let mut opts = test_options(&dir, FakePlatform::shared());
    opts.start_paused = true;
    let mut h = common::snap_harness_with(fake, [760.0, 960.0], opts);
    open_action(&mut h, Action::UsbTools);
    finish(&mut h);
    h.snapshot("usb_files");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images need --features snapshots"
)]
fn snapshot_usb_developer_narrow() {
    let _gpu = common::gpu_lock();
    let dir = TempDir::new("evo-usb-snapshot-narrow", true);
    let fake = FakeBackend::new().with_usb_results(vec![Ok(directory())]);
    let mut opts = test_options(&dir, FakePlatform::shared());
    opts.start_paused = true;
    let mut h = common::snap_harness_with(fake, [520.0, 700.0], opts);
    open_action(&mut h, Action::UsbTools);
    finish(&mut h);
    h.get_by_label("Developer").click();
    h.run_steps(3);
    h.snapshot("usb_developer_narrow");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images need --features snapshots"
)]
fn snapshot_usb_calculator_and_screen() {
    use evo_gui::backend::EmuBackend;
    let _gpu = common::gpu_lock();
    let dir = TempDir::new("evo-usb-views", true);
    let mut fake = FakeBackend::new();
    let frame = fake.framebuffer().unwrap().clone();
    let fake = fake.with_usb_results(vec![Ok(directory()), Ok(UsbResult::Screen(frame))]);
    let mut opts = test_options(&dir, FakePlatform::shared());
    opts.start_paused = true;
    let mut h = common::snap_harness_with(fake, [760.0, 960.0], opts);
    open_action(&mut h, Action::UsbTools);
    finish(&mut h);
    h.query_all_by_label("Calculator").last().unwrap().click();
    h.run_steps(3);
    h.snapshot("usb_calculator");
    h.get_by_label("Screen").click();
    h.run_steps(3);
    h.get_by_label("Capture screen").click();
    h.run_steps(2);
    finish(&mut h);
    assert_eq!(
        h.state().features().connect.status(),
        "Screen captured over USB."
    );
    h.get_by_label("Screen captured over USB.");
    h.snapshot("usb_screen");
}

#[test]
#[cfg_attr(
    not(feature = "snapshots"),
    ignore = "golden images need --features snapshots"
)]
fn snapshot_usb_send_review() {
    let _gpu = common::gpu_lock();
    let dir = TempDir::new("evo-usb-review", true);
    let paths = ["LESSON.py", "EXAMPLE.py"]
        .into_iter()
        .map(|name| {
            let path = dir.join(name);
            std::fs::write(&path, b"print(1)").unwrap();
            path
        })
        .collect();
    let platform = FakePlatform::shared();
    platform.queue_open_many(paths);
    let fake = FakeBackend::new().with_usb_results(vec![Ok(directory())]);
    let mut opts = test_options(&dir, platform);
    opts.start_paused = true;
    let mut h = common::snap_harness_with(fake, [520.0, 700.0], opts);
    open_action(&mut h, Action::UsbTools);
    finish(&mut h);
    h.get_by_label("Send files…").click();
    h.run_steps(3);
    h.snapshot("usb_send_review");
}
