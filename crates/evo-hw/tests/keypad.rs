//! Keypad scanner at `0x4010_3000` (IRQ 13) and the ON key (IRQ 0) (`docs/hardware/keypad.md`).

use evo_hw::dev::keypad::{KEY_NAMES, KEYPAD_IRQ, Key, Keypad, ON_IRQ, SCAN_CYCLES};
use evo_hw::{DevHarness, Device, MemoryMap, ResetKind, StateError};

const MODE: u32 = 0x00;
const SIZE: u32 = 0x04;
const STATUS: u32 = 0x08;
const MASK: u32 = 0x0C;
const ROWS: u32 = 0x10;

const IRQ_BIT: u64 = 1 << 13;

fn set_key(h: &mut DevHarness, k: &mut Keypad, key: Key, pressed: bool) {
    h.with_ctx(|ctx| k.set_key(key, pressed, ctx));
}

fn m(row: u8, col: u8) -> Key {
    Key::Matrix { row, col }
}

#[test]
fn constants() {
    assert_eq!(SCAN_CYCLES, 156_000);
    assert_eq!(KEYPAD_IRQ, 13);
    assert_eq!(ON_IRQ, 0);
}

#[test]
fn press_sets_row_status_irq() {
    let mut h = DevHarness::new();
    let mut k = Keypad::new();
    h.now = 1_000;
    h.write(&mut k, MASK, 4, 2);
    assert_eq!(h.irq, 0);
    set_key(&mut h, &mut k, m(2, 5), true);
    assert_eq!(h.read(&mut k, ROWS + 4, 2), 1 << 5);
    assert_eq!(h.read(&mut k, STATUS, 4) & 2, 2, "key-down status");
    assert_eq!(h.irq, IRQ_BIT);
    assert_eq!(h.event, Some(Some(1_000 + SCAN_CYCLES)));
    // A word read returns rows r and r + 1.
    set_key(&mut h, &mut k, m(3, 1), true);
    assert_eq!(h.read(&mut k, ROWS + 4, 4), (1 << 5) | (1 << 1) << 16);
    assert!(h.log.is_empty());
}

#[test]
fn scan_sets_data_ready() {
    let mut h = DevHarness::new();
    let mut k = Keypad::new();
    h.write(&mut k, MASK, 4, 2);
    set_key(&mut h, &mut k, m(2, 5), true);
    // The handler clears key-down and enables the scan-done interrupt.
    h.write(&mut k, STATUS, 4, 2);
    h.write(&mut k, MASK, 4, 3);
    assert_eq!(h.irq, 0);
    assert!(h.fire(&mut k));
    assert_eq!(h.now, SCAN_CYCLES);
    assert_eq!(h.read(&mut k, STATUS, 4), 1);
    assert_eq!(h.irq, IRQ_BIT);
    // Scanning continues while a key is held.
    assert_eq!(h.event, Some(Some(2 * SCAN_CYCLES)));
}

#[test]
fn scans_repeat_while_held_and_stop_after_release() {
    let mut h = DevHarness::new();
    let mut k = Keypad::new();
    set_key(&mut h, &mut k, m(6, 0), true);
    let mut scans = 0;
    for _ in 0..6 {
        h.write(&mut k, STATUS, 4, 3);
        assert!(h.fire(&mut k));
        assert_eq!(h.read(&mut k, STATUS, 4), 1);
        assert_eq!(h.read(&mut k, ROWS + 12, 2), 1, "rows stay stable");
        scans += 1;
    }
    assert_eq!(scans, 6);
    set_key(&mut h, &mut k, m(6, 0), false);
    assert_eq!(h.event, Some(Some(h.now + SCAN_CYCLES)), "release scans");
    assert!(h.fire(&mut k));
    assert_eq!(
        h.event, None,
        "no further scans with no key held and scan-done masked"
    );
    assert!(!h.fire(&mut k));
}

/// The BSP decodes the rows only after a key change (STATUS bit 1) and then six scan-dones; a
/// release must be a key change too, or the BSP never sees the rows go empty and takes the
/// next press of the same key for the key still being held.
#[test]
fn release_is_a_key_change() {
    let mut h = DevHarness::new();
    let mut k = Keypad::new();
    h.write(&mut k, MASK, 4, 2);
    set_key(&mut h, &mut k, m(4, 1), true);
    h.write(&mut k, STATUS, 4, 3);
    assert_eq!(h.irq, 0);
    set_key(&mut h, &mut k, m(4, 1), false);
    assert_eq!(h.read(&mut k, STATUS, 4) & 2, 2, "key change on release");
    assert_eq!(h.irq, IRQ_BIT);
    assert_eq!(h.read(&mut k, ROWS + 8, 2), 0);
}

/// While the guest unmasks scan-done (the BSP does from a key change until it has decoded the
/// rows), the scanner keeps scanning even with no key held, as it does in continuous mode.
#[test]
fn scans_continue_while_scan_done_is_unmasked() {
    let mut h = DevHarness::new();
    let mut k = Keypad::new();
    h.write(&mut k, MASK, 4, 2);
    set_key(&mut h, &mut k, m(4, 1), true);
    set_key(&mut h, &mut k, m(4, 1), false);
    // The BSP's handler: acknowledge, unmask scan-done; then six scans.
    h.write(&mut k, STATUS, 4, 2);
    h.write(&mut k, MASK, 4, 3);
    for n in 1..=6 {
        assert!(h.fire(&mut k), "scan {n}");
        assert_eq!(h.read(&mut k, STATUS, 4), 1);
        assert_eq!(h.irq, IRQ_BIT);
        h.write(&mut k, STATUS, 4, 1);
        assert_eq!(
            h.event,
            Some(Some(h.now + SCAN_CYCLES)),
            "scan {n} schedules"
        );
    }
    // Decoded: the BSP masks scan-done again; the pending scan is the last one.
    h.write(&mut k, MASK, 4, 2);
    assert!(h.fire(&mut k));
    assert_eq!(h.event, None);
    assert_eq!(h.irq, 0, "scan-done is masked");
}

/// Unmasking scan-done with nothing pending starts the scanner.
#[test]
fn unmasking_scan_done_starts_scanning() {
    let mut h = DevHarness::new();
    let mut k = Keypad::new();
    h.now = 5_000;
    h.write(&mut k, MASK, 4, 1);
    assert_eq!(h.event, Some(Some(5_000 + SCAN_CYCLES)));
    assert!(h.fire(&mut k));
    assert_eq!(h.irq, IRQ_BIT);
}

#[test]
fn status_w1c() {
    let mut h = DevHarness::new();
    let mut k = Keypad::new();
    h.write(&mut k, MASK, 4, 3);
    set_key(&mut h, &mut k, m(0, 0), true);
    h.fire(&mut k);
    assert_eq!(h.read(&mut k, STATUS, 4), 3);
    assert_eq!(h.irq, IRQ_BIT);
    // The BSP's `+0x08 &= ~2` keeps bit 1.
    h.write(&mut k, STATUS, 4, 1);
    assert_eq!(h.read(&mut k, STATUS, 4), 2);
    h.write(&mut k, STATUS, 4, 3);
    assert_eq!(h.read(&mut k, STATUS, 4), 0);
    assert_eq!(h.irq, 0);
}

#[test]
fn registers_store() {
    let mut h = DevHarness::new();
    let mut k = Keypad::new();
    h.write(&mut k, SIZE, 4, 0x0808);
    h.write(&mut k, MODE, 4, 0x5B68_0C33);
    h.write(&mut k, MASK, 4, 0xFFFF_FFFF);
    assert_eq!(h.read(&mut k, SIZE, 4), 0x0808);
    assert_eq!(h.read(&mut k, MODE, 4), 0x5B68_0C33);
    assert_eq!(h.read(&mut k, MASK, 4), 3, "two interrupt bits");
    assert!(h.log.is_empty());
    // Row registers are read-only; other offsets are unhandled.
    h.write(&mut k, ROWS, 2, 0xFF);
    assert_eq!(h.read(&mut k, ROWS, 2), 0);
    assert_eq!(h.read(&mut k, 0x20, 4), 0);
    assert_eq!(h.read(&mut k, MODE, 2), 0);
    assert_eq!(h.log.len(), 3);
}

#[test]
fn release_clears_row() {
    let mut h = DevHarness::new();
    let mut k = Keypad::new();
    set_key(&mut h, &mut k, m(7, 3), true);
    set_key(&mut h, &mut k, m(7, 0), true);
    assert_eq!(h.read(&mut k, ROWS + 14, 2), 0b1001);
    set_key(&mut h, &mut k, m(7, 3), false);
    assert_eq!(h.read(&mut k, ROWS + 14, 2), 0b0001);
    assert!(k.pressed(m(7, 0)));
    assert!(!k.pressed(m(7, 3)));
}

#[test]
fn on_key_pulses_irq0() {
    let mut h = DevHarness::new();
    let mut k = Keypad::new();
    set_key(&mut h, &mut k, Key::On, true);
    assert_eq!(h.pulses, vec![0]);
    assert_eq!(h.irq, 0);
    assert_eq!(h.event, None);
    set_key(&mut h, &mut k, Key::On, false);
    assert_eq!(h.pulses, vec![0], "nothing on release");
    for r in 0..8 {
        assert_eq!(h.read(&mut k, ROWS + 2 * r, 2), 0);
    }
}

#[test]
fn out_of_range_ignored() {
    let mut h = DevHarness::new();
    let mut k = Keypad::new();
    h.write(&mut k, MASK, 4, 2);
    set_key(&mut h, &mut k, m(8, 0), true);
    set_key(&mut h, &mut k, m(0, 8), true);
    set_key(&mut h, &mut k, m(255, 255), true);
    assert_eq!(h.read(&mut k, STATUS, 4), 0);
    assert_eq!(h.irq, 0);
    assert_eq!(h.event, None);
    for r in 0..8 {
        assert_eq!(h.read(&mut k, ROWS + 2 * r, 2), 0);
    }
}

#[test]
fn system_reset_keeps_keys_and_relatches_key_down() {
    let mut h = DevHarness::new();
    let mut k = Keypad::new();
    h.write(&mut k, MASK, 4, 3);
    h.write(&mut k, MODE, 4, 0x33);
    set_key(&mut h, &mut k, m(2, 7), true);
    h.now = 10_000;
    h.reset(&mut k, ResetKind::System);
    assert_eq!(h.read(&mut k, MODE, 4), 0);
    assert_eq!(h.read(&mut k, MASK, 4), 0);
    assert_eq!(h.irq, 0);
    assert_eq!(h.read(&mut k, ROWS + 4, 2), 1 << 7, "held keys stay");
    // A key held through the reset is a key-down once the guest unmasks it.
    assert_eq!(h.read(&mut k, STATUS, 4), 2);
    h.write(&mut k, MASK, 4, 2);
    assert_eq!(h.irq, IRQ_BIT);
    assert_eq!(h.event, Some(Some(10_000 + SCAN_CYCLES)));
    // PowerOn releases everything.
    h.reset(&mut k, ResetKind::PowerOn);
    assert_eq!(h.read(&mut k, ROWS + 4, 2), 0);
    assert_eq!(h.read(&mut k, STATUS, 4), 0);
    assert_eq!(h.event, Some(None));
}

#[test]
fn key_names_follow_the_ce_layout() {
    // code = (7 - row) * 8 + col + 1 (CE GetCSC numbering).
    let code = |k: Key| match k {
        Key::Matrix { row, col } => (7 - row) * 8 + col + 1,
        Key::On => 0,
    };
    for (name, code_want) in [
        ("down", 0x01),
        ("up", 0x04),
        ("enter", 0x09),
        ("add", 0x0A),
        ("clear", 0x0F),
        ("3", 0x12),
        ("2", 0x1A),
        ("0", 0x21),
        ("sin", 0x26),
        ("ln", 0x2B),
        ("alpha", 0x30),
        ("graph", 0x31),
        ("2nd", 0x36),
        ("del", 0x38),
    ] {
        let k = Key::from_name(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(code(k), code_want, "{name}");
        assert_eq!(k.name(), Some(name));
    }
    assert_eq!(Key::from_name("on"), Some(Key::On));
    assert_eq!(Key::from_name("ENTER"), Some(m(6, 0)), "case-insensitive");
    assert_eq!(Key::from_name("bogus"), None);
    assert_eq!(KEY_NAMES.len(), 50, "49 matrix keys and ON");
    let mut keys: Vec<Key> = KEY_NAMES.iter().map(|&(_, k)| k).collect();
    keys.sort_by_key(|k| format!("{k:?}"));
    keys.dedup();
    assert_eq!(keys.len(), 50, "every name is a different key");
}

#[test]
fn state_roundtrip() {
    let mut h = DevHarness::new();
    let mut k = Keypad::new();
    h.write(&mut k, MODE, 4, 0x33);
    h.write(&mut k, MASK, 4, 3);
    set_key(&mut h, &mut k, m(4, 6), true);
    let st = k.save();
    let mut k2 = Keypad::new();
    k2.load(&st).unwrap();
    assert_eq!(h.read(&mut k2, MODE, 4), 0x33);
    assert_eq!(h.read(&mut k2, MASK, 4), 3);
    assert_eq!(h.read(&mut k2, STATUS, 4), 2);
    assert_eq!(h.read(&mut k2, ROWS + 8, 2), 1 << 6);
    assert!(k2.load(&[0xFF; 3]).is_err());
    assert!(matches!(
        k2.load(&[st.clone(), vec![0]].concat()),
        Err(StateError::Mismatch(_))
    ));
    assert_eq!(
        h.read(&mut k2, ROWS + 8, 2),
        1 << 6,
        "failed load leaves state"
    );
}

#[test]
fn soc_attaches_keypad() {
    let mut map = MemoryMap::new();
    evo_hw::soc::attach(&mut map);
    assert!(map.device::<Keypad>("keypad").is_some());
}
