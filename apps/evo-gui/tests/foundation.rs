//! Lane 0 foundation: speed setting, the new preferences, the platform seam and the log buffer.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use evo_gui::backend::fake::FakeBackend;
use evo_gui::backend::{CLOCK_HZ, EmuBackend, Frame};
use evo_gui::logbuf::{LOG_CAP, LogBuf};
use evo_gui::pacer::{Pacer, Speed};
use evo_gui::platform::{FakePlatform, FileFilter, Platform, PlatformRef};
use evo_gui::prefs::Prefs;

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
