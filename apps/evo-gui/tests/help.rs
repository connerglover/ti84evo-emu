//! Lane F, Task F2: third-party licenses, the update check and bug reports.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{TempDir, harness_with, open_action, test_options};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use evo_gui::app::{AppOptions, EvoApp};
use evo_gui::backend::fake::FakeBackend;
use evo_gui::help::{
    ISSUES_URL, Release, UPDATE_REPO, latest_release_url, licenses_text, parse_release,
    parse_version,
};
use evo_gui::platform::FakePlatform;
use evo_gui::toolbar::Action;

struct Rig {
    h: Harness<'static, EvoApp>,
    platform: Arc<FakePlatform>,
    _dir: TempDir,
}

/// A paused harness on a fake platform in a fresh data dir.
fn rig(name: &str) -> Rig {
    let dir = TempDir::new(&format!("evo-help-{name}"), true);
    let platform = FakePlatform::shared();
    let opts = AppOptions {
        start_paused: true,
        ..test_options(&dir, platform.clone())
    };
    let h = harness_with(FakeBackend::new(), opts);
    Rig {
        h,
        platform,
        _dir: dir,
    }
}

impl Rig {
    /// Steps until a widget labelled `text` shows, at most 200 × (step + 10 ms).
    fn wait_for(&mut self, text: &str) {
        for _ in 0..200 {
            if self.h.query_by_label(text).is_some() {
                return;
            }
            self.h.step();
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("update check did not finish (no {text:?})");
    }
}

fn release_json(tag: &str) -> String {
    format!(r#"{{"tag_name":"{tag}","html_url":"https://example.invalid/r"}}"#)
}

#[test]
fn constants() {
    assert_eq!(UPDATE_REPO, "connerglover/ti84evo-emu");
    assert_eq!(
        latest_release_url(),
        "https://api.github.com/repos/connerglover/ti84evo-emu/releases/latest"
    );
    assert_eq!(
        ISSUES_URL,
        "https://github.com/connerglover/ti84evo-emu/issues/new"
    );
}

#[test]
fn parse_version_cases() {
    assert_eq!(parse_version("v0.2.0"), Some((0, 2, 0)));
    assert_eq!(parse_version("0.2.0"), Some((0, 2, 0)));
    assert_eq!(parse_version("0.2.0-rc1"), Some((0, 2, 0)));
    assert_eq!(parse_version("v10.20.30"), Some((10, 20, 30)));
    assert_eq!(parse_version("x"), None);
    assert_eq!(parse_version(""), None);
    assert_eq!(parse_version("v"), None);
    assert_eq!(parse_version("1.2"), None);
    assert_eq!(parse_version("1.2.3.4"), None);
    assert_eq!(parse_version("1.2.x"), None);
    assert_eq!(parse_version("1..3"), None);
    assert_eq!(parse_version("99999999999999999999.0.0"), None);
}

#[test]
fn parse_release_cases() {
    let r = parse_release(&release_json("v1.2.3")).unwrap();
    assert_eq!(
        r,
        Release {
            tag: "v1.2.3".to_owned(),
            url: "https://example.invalid/r".to_owned(),
            version: (1, 2, 3),
        }
    );
    assert!(parse_release(r#"{"tag_name":"v1.2.3"}"#).is_err());
    assert!(parse_release(r#"{"html_url":"https://example.invalid/r"}"#).is_err());
    assert!(parse_release(r#"{"tag_name":"nightly","html_url":"u"}"#).is_err());
    assert!(parse_release(r#"{"tag_name":5,"html_url":"u"}"#).is_err());
    // The page goes to the browser: only https.
    assert!(parse_release(r#"{"tag_name":"v1.0.0","html_url":"file:///etc/passwd"}"#).is_err());
    assert!(parse_release("garbage").is_err());
    assert!(parse_release("").is_err());
    assert!(parse_release("[]").is_err());
}

#[test]
fn update_newer_shows_window() {
    let mut r = rig("newer");
    r.platform.set_http(Ok(release_json("v99.0.0")));
    open_action(&mut r.h, Action::CheckUpdates);
    r.wait_for("Version v99.0.0 is available");
    r.h.run_steps(2);
    r.h.get_by_label("Update available");
    r.h.get_by_role_and_label(Role::Button, "Open download page")
        .click();
    r.h.run_steps(2);
    assert_eq!(r.platform.urls(), ["https://example.invalid/r"]);
}

#[test]
fn update_current_says_latest() {
    let mut r = rig("current");
    let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
    r.platform.set_http(Ok(release_json(&tag)));
    open_action(&mut r.h, Action::CheckUpdates);
    r.wait_for(&format!(
        "You have the latest version ({})",
        env!("CARGO_PKG_VERSION")
    ));
    assert!(r.h.query_by_label("Update available").is_none());
}

#[test]
fn update_older_says_latest() {
    let mut r = rig("older");
    r.platform.set_http(Ok(release_json("v0.0.1")));
    open_action(&mut r.h, Action::CheckUpdates);
    r.wait_for(&format!(
        "You have the latest version ({})",
        env!("CARGO_PKG_VERSION")
    ));
    assert!(r.h.query_by_label("Update available").is_none());
}

#[test]
fn update_404_says_no_releases() {
    let mut r = rig("404");
    r.platform.set_http(Err("HTTP 404".to_owned()));
    open_action(&mut r.h, Action::CheckUpdates);
    r.wait_for("No releases published yet");
}

#[test]
fn update_error_toasts() {
    let mut r = rig("offline");
    open_action(&mut r.h, Action::CheckUpdates);
    r.wait_for("Update check failed: offline");
}

#[test]
fn update_garbage_reply_toasts() {
    let mut r = rig("garbage");
    r.platform.set_http(Ok("<html>nope</html>".to_owned()));
    open_action(&mut r.h, Action::CheckUpdates);
    for _ in 0..200 {
        if r.h
            .query_all_by_label_contains("Update check failed: ")
            .next()
            .is_some()
        {
            return;
        }
        r.h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("update check did not finish");
}

#[test]
fn second_check_while_running_says_already_checking() {
    let mut r = rig("twice");
    r.platform.set_http(Err("HTTP 404".to_owned()));
    // Both actions run before any frame polls the worker, so the first is still running.
    r.h.state_mut().run_action(Action::CheckUpdates);
    r.h.state_mut().run_action(Action::CheckUpdates);
    r.h.run_steps(1);
    r.h.get_by_label("Already checking for updates");
    r.wait_for("No releases published yet");
    // Once it finished a new check may start.
    r.h.state_mut().run_action(Action::CheckUpdates);
    r.h.run_steps(1);
    assert_eq!(
        r.h.query_all_by_label("Already checking for updates")
            .count(),
        1
    );
}

#[test]
fn report_bug_opens_issues() {
    let mut r = rig("bug");
    open_action(&mut r.h, Action::ReportBug);
    assert_eq!(r.platform.urls(), [ISSUES_URL]);
}

#[test]
fn licenses_window_opens() {
    let mut r = rig("licenses");
    open_action(&mut r.h, Action::Licenses);
    r.h.get_by_label("Third-party licenses");
    // The top of the text is laid out (only visible lines are).
    r.h.get_by_label("Third-party software in evo-gui");
}

#[test]
fn licenses_mention_key_crates() {
    let t = licenses_text();
    for needle in ["egui", "openh264", "MIT", "Bitstream Vera"] {
        assert!(t.contains(needle), "licenses text lacks {needle:?}");
    }
    // Only third-party crates are listed.
    for own in ["evo-core ", "evo-hw ", "armv8m "] {
        assert!(
            !t.contains(own),
            "licenses text lists workspace crate {own:?}"
        );
    }
}
