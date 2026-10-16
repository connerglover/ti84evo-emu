//! Task B1: instance directories, ids, locks and renames.

mod common;

use std::time::Duration;

use common::TempDir;
use evo_gui::instance::{
    InstanceError, LOCK_FILE, instance_dir, next_free_id, prepare, validate_id,
};

fn base(name: &str) -> TempDir {
    TempDir::new(&format!("evo-gui-instance-{name}"), true)
}

#[test]
fn ids_are_validated() {
    let long = "a".repeat(33);
    for bad in ["", long.as_str(), "a b", "../x", "x/y", "é"] {
        match validate_id(bad) {
            Err(InstanceError::Invalid(m)) => {
                assert_eq!(m, "IDs use 1–32 letters, digits, - or _", "{bad:?}")
            }
            other => panic!("{bad:?}: {other:?}"),
        }
    }
    validate_id("lab-1_A").unwrap();
    validate_id(&"z".repeat(32)).unwrap();
    validate_id("7").unwrap();
}

#[test]
fn dirs_per_instance() {
    let b = base("dirs");
    assert_eq!(instance_dir(&b, None), b.to_path_buf());
    assert_eq!(
        instance_dir(&b, Some("lab")),
        b.join("instances").join("lab")
    );

    let default = prepare(&b, None, None).unwrap();
    assert_eq!(default.dir, b.to_path_buf());
    assert!(b.join(LOCK_FILE).is_file());
    let lab = prepare(&b, Some("lab"), None).unwrap();
    assert_eq!(lab.dir, b.join("instances").join("lab"));
    assert!(lab.dir.join(LOCK_FILE).is_file());
}

#[test]
fn prepare_rejects_bad_ids() {
    let b = base("bad");
    assert!(matches!(
        prepare(&b, Some("../x"), None),
        Err(InstanceError::Invalid(_))
    ));
    assert!(!b.join("x").exists());
    assert!(matches!(
        prepare(&b, Some("ok"), Some("a b")),
        Err(InstanceError::Invalid(_))
    ));
}

#[test]
fn next_free_id_skips_existing() {
    let b = base("next");
    assert_eq!(next_free_id(&b), "instance-2");
    std::fs::create_dir_all(b.join("instances").join("instance-2")).unwrap();
    assert_eq!(next_free_id(&b), "instance-3");
    std::fs::create_dir_all(b.join("instances").join("instance-4")).unwrap();
    assert_eq!(next_free_id(&b), "instance-3");
}

#[test]
fn second_lock_is_refused() {
    let b = base("lock");
    let first = prepare(&b, Some("a"), None).unwrap();
    match prepare(&b, Some("a"), None) {
        Err(InstanceError::InUse(id)) => assert_eq!(id, "a"),
        other => panic!("{other:?}"),
    }
    // Another instance is unaffected.
    let _other = prepare(&b, Some("b"), None).unwrap();
    drop(first);
    let _third = prepare(&b, Some("a"), None).unwrap();
}

#[test]
fn default_lock_is_reported_as_default() {
    let b = base("lockdefault");
    let _first = prepare(&b, None, None).unwrap();
    match prepare(&b, None, None) {
        Err(InstanceError::InUse(id)) => assert_eq!(id, "default"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn rename_moves_the_dir() {
    let b = base("rename");
    let old = b.join("instances").join("old");
    std::fs::create_dir_all(old.join("flash")).unwrap();
    std::fs::write(old.join("flash").join("x"), b"data").unwrap();

    let lock = prepare(&b, Some("new"), Some("old")).unwrap();
    let new = b.join("instances").join("new");
    assert_eq!(lock.dir, new);
    assert_eq!(std::fs::read(new.join("flash").join("x")).unwrap(), b"data");
    assert!(!old.exists());
    // The renamed instance is locked like any other.
    assert!(matches!(
        prepare(&b, Some("new"), None),
        Err(InstanceError::InUse(_))
    ));
}

#[test]
fn rename_waits_for_the_old_lock() {
    let b = base("renamewait");
    let held = prepare(&b, Some("old"), None).unwrap();
    std::fs::write(held.dir.join("x"), b"1").unwrap();
    let base_path = b.to_path_buf();
    let worker =
        std::thread::spawn(move || prepare(&base_path, Some("new"), Some("old")).map(|l| l.dir));
    std::thread::sleep(Duration::from_millis(300));
    // The old process still runs: nothing may have moved yet.
    assert!(!worker.is_finished());
    assert!(b.join("instances").join("old").join("x").is_file());
    drop(held);
    let dir = worker.join().unwrap().unwrap();
    assert_eq!(dir, b.join("instances").join("new"));
    assert!(dir.join("x").is_file());
    assert!(!b.join("instances").join("old").exists());
}

#[test]
fn rename_onto_existing_is_refused() {
    let b = base("renameexists");
    std::fs::create_dir_all(b.join("instances").join("old")).unwrap();
    std::fs::create_dir_all(b.join("instances").join("new")).unwrap();
    std::fs::write(b.join("instances").join("old").join("x"), b"1").unwrap();
    assert!(matches!(
        prepare(&b, Some("new"), Some("old")),
        Err(InstanceError::Invalid(_))
    ));
    assert!(b.join("instances").join("old").join("x").is_file());
}

#[cfg(windows)]
#[test]
fn rename_retries_a_transient_windows_sharing_violation() {
    use std::os::windows::fs::OpenOptionsExt;
    let b = base("sharing");
    let old = prepare(&b, Some("old"), None).unwrap();
    let path = old.dir.join("held");
    std::fs::write(&path, b"saved data").unwrap();
    drop(old);
    // Unlike a normal Rust handle, this explicitly disallows delete/rename sharing.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(120));
        drop(held);
    });
    let renamed = prepare(&b, Some("new"), Some("old"));
    release.join().unwrap();
    let renamed = renamed.unwrap();
    assert_eq!(
        std::fs::read(renamed.dir.join("held")).unwrap(),
        b"saved data"
    );
    assert!(!b.join("instances/old").exists());
}

#[test]
fn rename_refuses_destination_created_while_waiting() {
    let b = base("destination-race");
    let old = prepare(&b, Some("old"), None).unwrap();
    std::fs::write(old.dir.join("source"), b"source").unwrap();
    let base_path = b.to_path_buf();
    let worker = std::thread::spawn(move || prepare(&base_path, Some("new"), Some("old")));
    std::thread::sleep(Duration::from_millis(100));
    let target = b.join("instances/new");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("destination"), b"destination").unwrap();
    drop(old);
    assert!(worker.join().unwrap().is_err());
    assert_eq!(
        std::fs::read(b.join("instances/old/source")).unwrap(),
        b"source"
    );
    assert_eq!(
        std::fs::read(target.join("destination")).unwrap(),
        b"destination"
    );
}

#[test]
fn rename_without_new_id_is_invalid() {
    let b = base("renamenoid");
    std::fs::create_dir_all(b.join("instances").join("old")).unwrap();
    assert!(matches!(
        prepare(&b, None, Some("old")),
        Err(InstanceError::Invalid(_))
    ));
    assert!(b.join("instances").join("old").is_dir());
    assert!(!b.join(LOCK_FILE).exists());
}

#[test]
fn rename_of_missing_instance_is_invalid() {
    let b = base("renamemissing");
    assert!(matches!(
        prepare(&b, Some("new"), Some("ghost")),
        Err(InstanceError::Invalid(_))
    ));
    assert!(!b.join("instances").join("new").exists());
    assert!(!b.join("instances").join("ghost").exists());
}

#[test]
fn rename_onto_itself_is_invalid() {
    let b = base("renameself");
    std::fs::create_dir_all(b.join("instances").join("same")).unwrap();
    assert!(matches!(
        prepare(&b, Some("same"), Some("same")),
        Err(InstanceError::Invalid(_))
    ));
}
