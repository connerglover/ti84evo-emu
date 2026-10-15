//! Lane 0 foundation: speed setting and the new preferences.

use std::path::PathBuf;

use evo_gui::backend::CLOCK_HZ;
use evo_gui::pacer::{Pacer, Speed};
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
