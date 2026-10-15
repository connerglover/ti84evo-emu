//! State slots, screenshots and the toolbar's tool table.

mod common;

use common::TempDir;

use egui::{FontId, RawInput};
use evo_gui::backend::fake::FakeBackend;
use evo_gui::backend::{BackendError, EmuBackend, Frame};
use evo_gui::store::{SLOT_MAGIC, SLOT_VERSION, SLOTS, Store, StoreError};
use evo_gui::theme::{has_glyphs, install_fonts};
use evo_gui::toolbar::Tool;

/// A fresh, empty data directory for one test.
fn fresh_dir(test: &str) -> TempDir {
    TempDir::new(&format!("evo-gui-store-{test}"), false)
}

/// Writes `bytes` as slot `n`, creating the states directory.
fn write_slot(store: &Store, n: u8, bytes: &[u8]) {
    let path = store.slot_path(n);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A slot header (magic, version, tag) for `tag` at `version`.
fn header(version: u16, tag: &str) -> Vec<u8> {
    let mut v = SLOT_MAGIC.to_vec();
    v.extend_from_slice(&version.to_le_bytes());
    v.extend_from_slice(&(tag.len() as u16).to_le_bytes());
    v.extend_from_slice(tag.as_bytes());
    v
}

#[test]
fn constants() {
    assert_eq!(SLOTS, 4);
    assert_eq!(SLOT_MAGIC, b"EVOGUIST");
    assert_eq!(SLOT_VERSION, 1);
}

#[test]
fn slot_and_screenshot_paths() {
    let dir = fresh_dir("paths");
    let store = Store::new(dir.to_path_buf());
    assert_eq!(store.slot_path(2), dir.join("states").join("slot2.state"));
    assert!(!store.has_slot(2));
}

#[test]
fn save_then_load_roundtrip() {
    let dir = fresh_dir("roundtrip");
    let store = Store::new(dir.to_path_buf());
    let mut fake = FakeBackend::new();
    fake.run_for(123);
    let path = store.save_slot(1, &fake).unwrap();
    assert_eq!(path, store.slot_path(1));
    assert!(store.has_slot(1));
    assert!(!store.slot_path(1).with_extension("state.tmp").exists());
    fake.reset();
    assert_eq!(fake.cycles(), 0);
    store.load_slot(1, &mut fake).unwrap();
    assert_eq!(fake.cycles(), 123);
}

#[test]
fn save_overwrites_an_existing_slot() {
    let dir = fresh_dir("overwrite");
    let store = Store::new(dir.to_path_buf());
    let mut fake = FakeBackend::new();
    fake.run_for(5);
    store.save_slot(1, &fake).unwrap();
    fake.run_for(5);
    store.save_slot(1, &fake).unwrap();
    fake.reset();
    store.load_slot(1, &mut fake).unwrap();
    assert_eq!(fake.cycles(), 10);
}

#[test]
fn slot_file_layout() {
    let dir = fresh_dir("layout");
    let store = Store::new(dir.to_path_buf());
    let fake = FakeBackend::new();
    store.save_slot(4, &fake).unwrap();
    let bytes = std::fs::read(store.slot_path(4)).unwrap();
    let head = header(1, "demo");
    assert_eq!(&bytes[..head.len()], &head[..]);
    assert_eq!(&bytes[head.len()..], &fake.save_state().unwrap()[..]);
}

#[test]
fn missing_slot_is_empty_error() {
    let dir = fresh_dir("missing");
    let store = Store::new(dir.to_path_buf());
    let mut fake = FakeBackend::new();
    let err = store.load_slot(3, &mut fake).unwrap_err();
    assert!(matches!(err, StoreError::Empty(3)), "{err:?}");
    assert_eq!(err.to_string(), "slot 3 is empty");
}

#[test]
fn garbage_file_is_rejected_unchanged() {
    let dir = fresh_dir("garbage");
    let store = Store::new(dir.to_path_buf());
    write_slot(&store, 2, b"hello");
    let mut fake = FakeBackend::new();
    fake.run_for(77);
    let err = store.load_slot(2, &mut fake).unwrap_err();
    assert!(
        matches!(&err, StoreError::NotAState(p) if *p == store.slot_path(2)),
        "{err:?}"
    );
    assert_eq!(fake.cycles(), 77);
}

#[test]
fn raw_core_state_is_not_a_slot() {
    let dir = fresh_dir("rawcore");
    let store = Store::new(dir.to_path_buf());
    write_slot(&store, 2, b"EVOSTATE\x01\0\0\0");
    let mut fake = FakeBackend::new();
    let err = store.load_slot(2, &mut fake).unwrap_err();
    assert!(matches!(err, StoreError::NotAState(_)), "{err:?}");
}

#[test]
fn truncated_header_is_not_a_state() {
    let dir = fresh_dir("truncated");
    let store = Store::new(dir.to_path_buf());
    let mut fake = FakeBackend::new();
    let full = header(1, "demo");
    for len in [8, 9, 11, full.len() - 1] {
        write_slot(&store, 1, &full[..len]);
        let err = store.load_slot(1, &mut fake).unwrap_err();
        assert!(
            matches!(err, StoreError::NotAState(_)),
            "len {len}: {err:?}"
        );
    }
}

#[test]
fn state_from_other_backend_rejected() {
    let dir = fresh_dir("otherbackend");
    let store = Store::new(dir.to_path_buf());
    let mut a = FakeBackend::new();
    a.run_for(9);
    store.save_slot(1, &a).unwrap();
    let mut b = FakeBackend::new().with_state_tag("demo-b");
    assert_eq!(b.name(), "demo");
    let err = store.load_slot(1, &mut b).unwrap_err();
    match &err {
        StoreError::WrongBackend { expected, found } => {
            assert_eq!(expected, "demo-b");
            assert_eq!(found, "demo");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(err.to_string(), "state is for `demo`, this is `demo-b`");
    assert_eq!(b.cycles(), 0);
}

#[test]
fn future_version_rejected() {
    let dir = fresh_dir("version");
    let store = Store::new(dir.to_path_buf());
    let mut bytes = header(2, "demo");
    bytes.extend_from_slice(&FakeBackend::new().save_state().unwrap());
    write_slot(&store, 1, &bytes);
    let mut fake = FakeBackend::new();
    let err = store.load_slot(1, &mut fake).unwrap_err();
    assert!(matches!(err, StoreError::Version(2)), "{err:?}");
}

#[test]
fn corrupt_payload_rejected_unchanged() {
    let dir = fresh_dir("corrupt");
    let store = Store::new(dir.to_path_buf());
    let mut bytes = header(1, "demo");
    bytes.extend_from_slice(b"{");
    write_slot(&store, 1, &bytes);
    let mut fake = FakeBackend::new();
    fake.run_for(55);
    let err = store.load_slot(1, &mut fake).unwrap_err();
    assert!(
        matches!(err, StoreError::Backend(BackendError::BadState(_))),
        "{err:?}"
    );
    assert_eq!(fake.cycles(), 55);
}

#[test]
fn screenshot_writes_png() {
    let dir = fresh_dir("screenshot");
    let store = Store::new(dir.to_path_buf());
    let mut fake = FakeBackend::new();
    let frame = fake.framebuffer().unwrap().clone();
    let path = store.screenshot(&frame, 1_700_000_000_123).unwrap();
    assert_eq!(path, dir.join("screenshots").join("evo-1700000000123.png"));
    let img = image::load_from_memory(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (320, 240));
    assert_eq!(img.to_rgba8().as_raw(), &frame.rgba);
}

#[test]
fn screenshot_of_invalid_frame_is_no_frame() {
    let dir = fresh_dir("noframe");
    let store = Store::new(dir.to_path_buf());
    let bad = Frame {
        width: 320,
        height: 240,
        rgba: vec![0; 10],
    };
    let err = store.screenshot(&bad, 1).unwrap_err();
    assert!(matches!(err, StoreError::NoFrame), "{err:?}");
}

#[test]
fn tool_table() {
    use Tool::*;
    assert_eq!(
        Tool::ALL,
        [
            Screenshot,
            SaveState,
            LoadState,
            Reset,
            Reprovision,
            Smooth,
            Debug,
            Keymap,
            About
        ]
    );
    let rows: Vec<_> = Tool::ALL
        .iter()
        .map(|t| (t.icon(), t.fallback(), t.tooltip()))
        .collect();
    assert_eq!(
        rows,
        [
            ("\u{1F4F7}", "Shot", "Screenshot"),
            ("\u{1F4BE}", "Save", "Save state"),
            ("\u{1F4C2}", "Load", "Load state"),
            ("\u{1F504}", "Reset", "Reset"),
            ("\u{1F527}", "Prov", "Re-provision"),
            (
                "\u{1F5BC}",
                "Smooth",
                "Smooth scaling (better at non-integer sizes)"
            ),
            ("\u{1F41B}", "Debug", "Debug panels"),
            ("\u{2328}", "Keys", "Keyboard mapping"),
            ("\u{2139}", "About", "About"),
        ]
    );
}

#[test]
fn toolbar_icons_have_a_drawable_label() {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    let mut out = ctx.run_ui(RawInput::default(), |_| {});
    out.textures_delta.clear();
    let font = FontId::proportional(14.0);
    for t in Tool::ALL {
        assert!(has_glyphs(&ctx, &font, t.fallback()), "{t:?} fallback");
        assert!(
            has_glyphs(&ctx, &font, t.icon()) || has_glyphs(&ctx, &font, t.fallback()),
            "{t:?}"
        );
    }
}
