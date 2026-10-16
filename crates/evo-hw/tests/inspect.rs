//! Side-effect-free device inspection (`Device::inspect`, `MemoryMap::inspect_devices`).

use evo_hw::dev::gpio::Gpio;
use evo_hw::dev::keypad::{Key, Keypad};
use evo_hw::dev::lcdc::{Lcdc, Pwm};
use evo_hw::dev::mpc::Mpc;
use evo_hw::dev::rtc::Rtc;
use evo_hw::dev::spif::Spif;
use evo_hw::dev::storage::Storage;
use evo_hw::dev::sysctl::{CRU_ID, Cru, Spctrl};
use evo_hw::dev::timer::Timer;
use evo_hw::dev::uart::Uart16550;
use evo_hw::{CPU_HZ, DevHarness, Device, MemoryMap, RegView};

/// Inspect `d` at `h.now` and check the contract: the device state is unchanged, the rows are
/// in strictly increasing offset order, and a guest read of each row's offset (of width
/// `width(offset)`, on a fresh copy of the device so one read's side effects cannot touch
/// another row) returns the row's value.
fn check<D: Device + Clone>(
    what: &str,
    d: &D,
    h: &mut DevHarness,
    width: impl Fn(u32) -> u8,
) -> Vec<RegView> {
    let before = d.save();
    let rows = d.inspect(h.now);
    assert_eq!(d.save(), before, "{what}: inspect changed the state");
    assert!(!rows.is_empty(), "{what}: no rows");
    for pair in rows.windows(2) {
        assert!(
            pair[0].offset < pair[1].offset,
            "{what}: {} at {:#x} before {} at {:#x}",
            pair[0].name,
            pair[0].offset,
            pair[1].name,
            pair[1].offset
        );
    }
    for row in &rows {
        let mut copy = d.clone();
        let got = h.read(&mut copy, row.offset, width(row.offset));
        assert_eq!(
            got, row.value,
            "{what}: row {} at {:#x}",
            row.name, row.offset
        );
    }
    assert!(h.log.is_empty(), "{what}: a row read was unhandled");
    rows
}

fn word(_: u32) -> u8 {
    4
}

fn row<'a>(rows: &'a [RegView], name: &str) -> &'a RegView {
    rows.iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no row {name} in {rows:?}"))
}

#[test]
fn inspect_has_no_side_effects() {
    // Timer: channel 0 loaded and running for a while.
    let mut h = DevHarness::new();
    let mut t = Timer::new();
    h.write(&mut t, 0x04, 4, 100);
    h.write(&mut t, 0x00, 4, 50);
    h.write(&mut t, 0x30, 4, 1);
    h.now = CPU_HZ / 1000;
    let rows = check("timer", &t, &mut h, word);
    assert_eq!(row(&rows, "ch0_load").value, 100);
    assert_eq!(row(&rows, "ch2_value").offset, 0x20);
    assert_eq!(row(&rows, "ctrl").offset, 0x30);

    // RTC: an alarm and a time set.
    let mut h = DevHarness::new();
    let mut r = Rtc::new();
    h.write(&mut r, 0x10, 4, 7);
    h.write(&mut r, 0x44, 4, 1 << 12);
    h.now = 2 * CPU_HZ;
    let rows = check("rtc", &r, &mut h, word);
    assert_eq!(row(&rows, "alarm0").value, 7);
    assert_eq!(row(&rows, "time").value, 1 << 12 | 2);

    // UART: line settings and two bytes waiting in the RX FIFO.
    let mut h = DevHarness::new();
    let mut u = Uart16550::new(17);
    h.write(&mut u, 0x0C, 4, 0x03);
    h.write(&mut u, 0x1C, 4, 0x5A);
    h.write(&mut u, 0x04, 4, 0x01);
    h.with_ctx(|ctx| u.inject_rx(b"AB", ctx));
    let rows = check("uart", &u, &mut h, word);
    let rbr = &rows[0];
    assert_eq!((rbr.offset, rbr.value), (0x00, u32::from(b'A')));
    assert_eq!(row(&rows, "lsr").value, 0x61);
    // The front byte was not popped by inspect or the reads on copies.
    assert_eq!(h.read(&mut u, 0x00, 4), u32::from(b'A'));

    // LCDC: timing and control set.
    let mut h = DevHarness::new();
    let mut l = Lcdc::new();
    h.write(&mut l, 0x00, 4, 0x3C);
    h.write(&mut l, 0x1C, 4, 0x08);
    h.write(&mut l, 0x18, 4, 0x80D);
    let rows = check("lcdc", &l, &mut h, word);
    let control = row(&rows, "control");
    assert_eq!((control.offset, control.value), (0x18, 0x80D));
    assert_eq!(control.note.as_deref(), Some("enabled, power on, rgb565"));

    // PWM.
    let mut h = DevHarness::new();
    let mut p = Pwm::new();
    h.write(&mut p, 0x00, 4, 5);
    h.write(&mut p, 0x04, 4, 10);
    let rows = check("pwm", &p, &mut h, word);
    let names: Vec<_> = rows.iter().map(|r| (r.name.as_str(), r.offset)).collect();
    assert_eq!(names, [("duty", 0x00), ("period", 0x04)]);
    assert_eq!(rows[0].value, 5);

    // GPIO: pin 1 an output driven high, pin 3 pulled up, pin 9 driven by the host.
    let mut h = DevHarness::new();
    let mut g = Gpio::new();
    h.write(&mut g, 0x08, 4, 0b10);
    h.write(&mut g, 0x00, 4, 0b10);
    h.write(&mut g, 0x18, 4, 0b1000);
    h.write(&mut g, 0x1C, 4, 0b1000);
    h.with_ctx(|ctx| g.set_input(9, Some(true), ctx));
    let rows = check("gpio", &g, &mut h, word);
    let data_in = row(&rows, "data_in");
    assert_eq!((data_in.offset, data_in.value), (0x04, 1 << 9 | 0b1010));

    // Keypad: ENTER held, scan-done unmasked. Row registers are halfwords.
    let mut h = DevHarness::new();
    let mut k = Keypad::new();
    h.write(&mut k, 0x0C, 4, 0x3);
    h.with_ctx(|ctx| k.set_key(Key::Matrix { row: 6, col: 0 }, true, ctx));
    let rows = check("keypad", &k, &mut h, |off| if off >= 0x10 { 2 } else { 4 });
    let row6 = row(&rows, "row6");
    assert_eq!((row6.offset, row6.value), (0x1C, 1));
    assert_eq!(row6.note.as_deref(), Some("enter"));
    assert_eq!(row(&rows, "status").value, 0b10);

    // SPIF: a JEDEC ID read left open (no flash buffer needed).
    let mut h = DevHarness::new();
    let mut s = Spif::new();
    h.write(&mut s, 0x0C, 4, 0x9F << 24);
    h.write(&mut s, 0x08, 4, 3);
    h.write(&mut s, 0x20, 4, 0x2);
    let rows = check("spif", &s, &mut h, word);
    assert_eq!(row(&rows, "op").value, 0x9F << 24);
    assert_eq!(row(&rows, "fifo_level").value, 3 << 8);

    // MPC: auto-increment on, LUT word 1 written, index back at 1.
    let mut h = DevHarness::new();
    let mut m = Mpc::new(3);
    h.write(&mut m, 0x00, 4, 1 << 8);
    h.write(&mut m, 0x18, 4, 1);
    h.write(&mut m, 0x1C, 4, 0xF0);
    h.write(&mut m, 0x18, 4, 1);
    let rows = check("mpc", &m, &mut h, word);
    assert_eq!(row(&rows, "blk_lut").value, 0xF0);
    assert_eq!(row(&rows, "blk_idx").value, 1);
    assert_eq!(row(&rows, "blk_max").value, 3);

    // CRU: a clock gate written.
    let mut h = DevHarness::new();
    let mut c = Cru::new();
    h.write(&mut c, 0x14, 4, 0xABC);
    let rows = check("cru", &c, &mut h, word);
    let id = rows.iter().find(|r| r.offset == 0xFC).expect("id row");
    assert_eq!(id.value, CRU_ID);
    assert_eq!(
        rows.iter().find(|r| r.offset == 0x14).map(|r| r.value),
        Some(0xABC)
    );

    // SPCTRL: deep sleep armed and a PPC word.
    let mut h = DevHarness::new();
    let mut sp = Spctrl::new();
    h.write(&mut sp, 0xD4, 4, 0x10);
    h.write(&mut sp, 0x40, 4, 7);
    let rows = check("spctrl", &sp, &mut h, word);
    assert_eq!(row(&rows, "deep_sleep").value, 0x10);
    assert_eq!(
        rows.iter().find(|r| r.offset == 0x40).map(|r| r.value),
        Some(7)
    );
}

#[test]
fn uart_lsr_at_reset() {
    let mut h = DevHarness::new();
    let u = Uart16550::new(17);
    let rows = check("uart", &u, &mut h, word);
    let lsr = row(&rows, "lsr");
    assert_eq!((lsr.offset, lsr.value), (0x14, 0x60));
}

#[test]
fn rtc_time_counts_from_now() {
    let mut h = DevHarness::new();
    let r = Rtc::new();
    h.now = 3 * CPU_HZ;
    let rows = check("rtc", &r, &mut h, word);
    let time = row(&rows, "time");
    assert_eq!((time.offset, time.value), (0x44, 3));
    assert_eq!(time.note.as_deref(), Some("00:00:03"));
}

#[test]
fn inspect_devices_lists_non_empty_in_add_order() {
    let mut map = MemoryMap::new();
    map.add_device("pwm", 0x4010_4000, 0x1000, Box::new(Pwm::new()));
    map.add_device("plain", 0x5020_2B00, 0x100, Box::new(Storage::new(0x100)));
    map.add_device("rtc", 0x4010_2000, 0x1000, Box::new(Rtc::new()));
    map.now = CPU_HZ;
    let devs = map.inspect_devices(map.now);
    let names: Vec<_> = devs.iter().map(|d| (d.name, d.base)).collect();
    assert_eq!(names, [("pwm", 0x4010_4000), ("rtc", 0x4010_2000)]);
    assert_eq!(row(&devs[1].regs, "time").value, 1);
}
