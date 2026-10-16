//! RTC at `0x4010_2000` (IRQ 15) (`docs/hardware/rtc.md` "Model (first cut)").

use evo_hw::dev::rtc::{RTC_BUSY, RTC_IRQ, Rtc};
use evo_hw::{CPU_HZ, DevHarness, Device, MemoryMap, ResetKind, StateError};

const ALARM0: u32 = 0x10;
const ALARM1: u32 = 0x14;
const ALARM2: u32 = 0x18;
const CTRL: u32 = 0x20;
const STATUS: u32 = 0x34;
const BUSY: u32 = 0x40;
const TIME: u32 = 0x44;

const IRQ_BIT: u64 = 1 << 15;

/// `sec | min << 6 | hour << 12`.
fn hms(h: u32, m: u32, s: u32) -> u32 {
    s | m << 6 | h << 12
}

fn raise(h: &mut DevHarness, r: &mut Rtc, bits: u32) {
    h.with_ctx(|ctx| r.raise_status(bits, ctx));
}

#[test]
fn constants() {
    assert_eq!(RTC_IRQ, 15);
    assert_eq!(RTC_BUSY, 0);
}

#[test]
fn time_counts_from_midnight() {
    let mut h = DevHarness::new();
    let mut r = Rtc::new();
    assert_eq!(h.read(&mut r, TIME, 4), 0, "00:00:00 at power-on");
    assert_eq!(r.seconds_of_day(0), 0);
    h.now = 3_725 * CPU_HZ;
    assert_eq!(h.read(&mut r, TIME, 4), 0x1085);
    assert_eq!(hms(1, 2, 5), 0x1085);
    assert_eq!(r.seconds_of_day(h.now), 3_725);
    // Partial seconds do not count.
    h.now = 3_726 * CPU_HZ - 1;
    assert_eq!(r.seconds_of_day(h.now), 3_725);
    assert!(h.log.is_empty());
}

#[test]
fn write_sets_time() {
    let mut h = DevHarness::new();
    let mut r = Rtc::new();
    let n = 12_345_678;
    h.now = n;
    h.write(&mut r, TIME, 4, 0x1085);
    assert_eq!(r.seconds_of_day(n), 3_725);
    assert_eq!(h.read(&mut r, TIME, 4), 0x1085);
    h.now = n + CPU_HZ;
    assert_eq!(r.seconds_of_day(h.now), 3_726);
    assert_eq!(h.read(&mut r, TIME, 4), hms(1, 2, 6));
}

#[test]
fn wraps_at_midnight() {
    let mut h = DevHarness::new();
    let mut r = Rtc::new();
    let n = 999;
    h.now = n;
    assert_eq!(hms(23, 59, 59), 0x1_7EFB);
    h.write(&mut r, TIME, 4, 0x1_7EFB);
    assert_eq!(h.read(&mut r, TIME, 4), 0x1_7EFB);
    h.now = n + CPU_HZ;
    assert_eq!(h.read(&mut r, TIME, 4), 0);
    assert_eq!(r.seconds_of_day(h.now), 0);
    // Out-of-range fields wrap too: hour 31 = 07:00 next day.
    h.write(&mut r, TIME, 4, hms(31, 0, 0));
    assert_eq!(h.read(&mut r, TIME, 4), hms(7, 0, 0));
}

#[test]
fn busy_reads_zero() {
    let mut h = DevHarness::new();
    let mut r = Rtc::new();
    assert_eq!(h.read(&mut r, BUSY, 4), RTC_BUSY);
    h.now = 7 * CPU_HZ;
    assert_eq!(h.read(&mut r, BUSY, 4), 0);
    assert!(h.log.is_empty());
}

#[test]
fn alarm_and_ctrl_are_storage() {
    let mut h = DevHarness::new();
    let mut r = Rtc::new();
    for (off, v) in [
        (ALARM0, 0x11),
        (ALARM1, 0x2222),
        (ALARM2, 0x3_3333),
        (CTRL, 0xA1),
    ] {
        h.write(&mut r, off, 4, v);
        assert_eq!(h.read(&mut r, off, 4), v, "{off:#x}");
    }
    // The BSP's RMW of CTRL.
    let c = h.read(&mut r, CTRL, 4);
    h.write(&mut r, CTRL, 4, c | 0x20);
    assert_eq!(h.read(&mut r, CTRL, 4), 0xA1 | 0x20);
    assert!(h.log.is_empty());
}

#[test]
fn status_w1c_and_irq() {
    let mut h = DevHarness::new();
    let mut r = Rtc::new();
    assert_eq!(h.read(&mut r, STATUS, 4), 0);
    raise(&mut h, &mut r, 1);
    assert_eq!(h.read(&mut r, STATUS, 4), 1);
    assert_eq!(h.irq, IRQ_BIT);
    h.write(&mut r, STATUS, 4, 1);
    assert_eq!(h.read(&mut r, STATUS, 4), 0);
    assert_eq!(h.irq, 0);
    // Only bits 0-5 exist; W1C clears only the bits written.
    raise(&mut h, &mut r, 0xFFFF_FFFF);
    assert_eq!(h.read(&mut r, STATUS, 4), 0x3F);
    h.write(&mut r, STATUS, 4, 0x01);
    assert_eq!(h.read(&mut r, STATUS, 4), 0x3E);
    assert_eq!(h.irq, IRQ_BIT);
    h.write(&mut r, STATUS, 4, 0);
    assert_eq!(h.read(&mut r, STATUS, 4), 0x3E, "writing 0 clears nothing");
    h.write(&mut r, STATUS, 4, 0x3E);
    assert_eq!(h.irq, 0);
    raise(&mut h, &mut r, 0x40);
    assert_eq!(h.irq, 0, "bit 6 does not exist");
}

#[test]
fn unmodelled_accesses_are_logged() {
    let mut h = DevHarness::new();
    let mut r = Rtc::new();
    assert_eq!(h.read(&mut r, 0x00, 4), 0);
    assert_eq!(h.read(&mut r, 0x48, 4), 0);
    assert_eq!(h.read(&mut r, TIME, 1), 0);
    h.write(&mut r, CTRL, 2, 0xFF);
    h.write(&mut r, 0x12, 4, 1);
    h.write(&mut r, BUSY, 4, 1);
    h.write(&mut r, 0xFFC, 4, 1);
    assert_eq!(h.read(&mut r, CTRL, 4), 0);
    assert_eq!(h.read(&mut r, BUSY, 4), RTC_BUSY);
    assert_eq!(h.log.len(), 7);
}

#[test]
fn system_reset_keeps_time() {
    let mut h = DevHarness::new();
    let mut r = Rtc::new();
    h.now = 100;
    h.write(&mut r, TIME, 4, hms(12, 30, 0));
    h.write(&mut r, CTRL, 4, 0x81);
    h.write(&mut r, ALARM0, 4, 5);
    raise(&mut h, &mut r, 0x4);
    h.now = 100 + 10 * CPU_HZ;
    h.irq = 0;
    h.reset(&mut r, ResetKind::System);
    assert_eq!(h.read(&mut r, TIME, 4), hms(12, 30, 10));
    assert_eq!(h.read(&mut r, CTRL, 4), 0x81);
    assert_eq!(h.read(&mut r, ALARM0, 4), 5);
    assert_eq!(h.read(&mut r, STATUS, 4), 0x4);
    assert_eq!(h.irq, IRQ_BIT, "the held status drives the line again");
    h.reset(&mut r, ResetKind::PowerOn);
    assert_eq!(h.irq, 0);
    assert_eq!(h.read(&mut r, CTRL, 4), 0);
    assert_eq!(h.read(&mut r, STATUS, 4), 0);
    // Power-on restarts the count at 00:00:00 from now.
    assert_eq!(h.read(&mut r, TIME, 4), 0);
    h.now += 61 * CPU_HZ;
    assert_eq!(h.read(&mut r, TIME, 4), hms(0, 1, 1));
}

#[test]
fn state_roundtrip() {
    let mut h = DevHarness::new();
    let mut r = Rtc::new();
    h.now = 5;
    h.write(&mut r, TIME, 4, hms(8, 0, 0));
    h.write(&mut r, ALARM1, 4, 0x77);
    h.write(&mut r, CTRL, 4, 0x21);
    raise(&mut h, &mut r, 0x2);
    let st = r.save();
    let mut r2 = Rtc::new();
    r2.load(&st).unwrap();
    h.now = 5 + 3 * CPU_HZ;
    assert_eq!(h.read(&mut r2, TIME, 4), hms(8, 0, 3));
    assert_eq!(h.read(&mut r2, ALARM1, 4), 0x77);
    assert_eq!(h.read(&mut r2, CTRL, 4), 0x21);
    assert_eq!(h.read(&mut r2, STATUS, 4), 0x2);
    assert!(r2.load(&[0xFF; 3]).is_err());
    assert!(matches!(
        r2.load(&[st.clone(), vec![0]].concat()),
        Err(StateError::Mismatch(_))
    ));
    assert_eq!(
        h.read(&mut r2, TIME, 4),
        hms(8, 0, 3),
        "failed load leaves state"
    );
}

#[test]
fn soc_attaches_rtc() {
    let mut map = MemoryMap::new();
    evo_hw::soc::attach(&mut map);
    assert!(map.device::<Rtc>("rtc").is_some());
}

#[test]
fn set_seconds_of_day_restarts_count() {
    let mut h = DevHarness::new();
    let mut r = Rtc::new();
    h.now = 10 * CPU_HZ + 7;
    r.set_seconds_of_day(100, h.now);
    assert_eq!(r.seconds_of_day(h.now), 100);
    assert_eq!(h.read(&mut r, TIME, 4), hms(0, 1, 40));
    // The count restarts from the set time: a partial second since then does not count.
    assert_eq!(r.seconds_of_day(h.now + CPU_HZ - 1), 100);
    assert_eq!(r.seconds_of_day(h.now + 5 * CPU_HZ), 105);
    // Wrapped into one day.
    r.set_seconds_of_day(86_400 + 5, h.now);
    assert_eq!(r.seconds_of_day(h.now), 5);
    r.set_seconds_of_day(u32::MAX, h.now);
    assert_eq!(r.seconds_of_day(h.now), u32::MAX % 86_400);
    assert!(h.log.is_empty());
}
