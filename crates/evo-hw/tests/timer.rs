//! TIMER block (`docs/hardware/timer.md`): 3 down-counters computed from the cycle count.

use armv8m::{Access, AccessKind, Bus};
use evo_hw::dev::timer::Timer;
use evo_hw::{DevHarness, Device, MemoryMap, ResetKind};

const VALUE0: u32 = 0x00;
const LOAD0: u32 = 0x04;
const VALUE1: u32 = 0x10;
const LOAD1: u32 = 0x14;
const VALUE2: u32 = 0x20;
const LOAD2: u32 = 0x24;
const CTRL: u32 = 0x30;
const INT_STAT: u32 = 0x34;
const INT_EN: u32 = 0x38;

/// Channel 0 enabled (bit 0) on the 32 kHz clock (bit 1).
const CH0_EN_32K: u32 = 0b011;

/// A timer whose channel 0 runs at 32 kHz from `value` (LOAD = `load`), started at `h.now`.
fn running_ch0(h: &mut DevHarness, load: u32, value: u32) -> Timer {
    let mut t = Timer::new();
    h.write(&mut t, LOAD0, 4, load);
    h.write(&mut t, VALUE0, 4, value);
    h.write(&mut t, CTRL, 4, CH0_EN_32K);
    t
}

#[test]
fn counts_down_at_32k() {
    let mut h = DevHarness::new();
    let mut t = running_ch0(&mut h, 0xFFFF_FFFF, 0xFFFF_FFFF);
    assert_eq!(h.read(&mut t, VALUE0, 4), 0xFFFF_FFFF);
    // One 32,768 Hz tick is 4,760.74 cycles of 156 MHz.
    h.now = 4_760;
    assert_eq!(h.read(&mut t, VALUE0, 4), 0xFFFF_FFFF);
    h.now = 4_761;
    assert_eq!(h.read(&mut t, VALUE0, 4), 0xFFFF_FFFE);
    // Reads do not change state: reading again at the same time gives the same value.
    assert_eq!(h.read(&mut t, VALUE0, 4), 0xFFFF_FFFE);
    assert_eq!(h.irq, 0);
    assert!(h.log.is_empty());
}

#[test]
fn hundred_ms_is_3276_ticks() {
    let mut h = DevHarness::new();
    let mut t = running_ch0(&mut h, 0xFFFF_FFFF, 0xFFFF_FFFF);
    h.now = 15_600_000;
    assert_eq!(0xFFFF_FFFF - h.read(&mut t, VALUE0, 4), 3276);
}

#[test]
fn clock_39k_when_clock_bit_clear() {
    let mut h = DevHarness::new();
    let mut t = Timer::new();
    h.write(&mut t, LOAD2, 4, 0xFFFF_FFFF);
    h.write(&mut t, VALUE2, 4, 0xFFFF_FFFF);
    // Channel 2: enable = bit 6, clock select = bit 7 (left clear).
    h.write(&mut t, CTRL, 4, 1 << 6);
    h.now = 3_999;
    assert_eq!(h.read(&mut t, VALUE2, 4), 0xFFFF_FFFF);
    h.now = 4_000;
    assert_eq!(h.read(&mut t, VALUE2, 4), 0xFFFF_FFFE);
}

#[test]
fn frozen_while_disabled() {
    let mut h = DevHarness::new();
    let mut t = Timer::new();
    h.write(&mut t, VALUE1, 4, 500);
    // Channel 1 is not enabled: VALUE holds.
    h.now = 1_000_000;
    assert_eq!(h.read(&mut t, VALUE1, 4), 500);
    // Enable channel 1 at 32 kHz (bits 3 and 4): counting starts now, not at the VALUE write.
    h.write(&mut t, CTRL, 4, 0b11 << 3);
    h.now = 1_000_000 + 4_761;
    assert_eq!(h.read(&mut t, VALUE1, 4), 499);
    // Disable: the count freezes at its current value.
    h.write(&mut t, CTRL, 4, 0b10 << 3);
    h.now = 9_000_000;
    assert_eq!(h.read(&mut t, VALUE1, 4), 499);
    // The other channels never moved.
    assert_eq!(h.read(&mut t, VALUE0, 4), 0);
    assert_eq!(h.read(&mut t, VALUE2, 4), 0);
}

#[test]
fn underflow_reloads_from_load() {
    let mut h = DevHarness::new();
    let mut t = running_ch0(&mut h, 0xFFFF_FFFF, 1);
    // 1 -> 0 -> LOAD -> LOAD - 1.
    h.now = 3 * 4_761;
    assert_eq!(h.read(&mut t, VALUE0, 4), 0xFFFF_FFFE);
    // A small LOAD: period LOAD + 1 = 5 ticks after the first underflow.
    let mut h = DevHarness::new();
    let mut t = running_ch0(&mut h, 4, 2);
    let at = |n: u64| (n * 156_000_000).div_ceil(32_768);
    let expect = [2, 1, 0, 4, 3, 2, 1, 0, 4, 3];
    for (n, want) in expect.iter().enumerate() {
        h.now = at(n as u64);
        assert_eq!(h.read(&mut t, VALUE0, 4), *want, "tick {n}");
    }
    // No interrupt is modeled.
    assert_eq!(h.irq, 0);
    assert_eq!(h.read(&mut t, INT_STAT, 4), 0);
}

#[test]
fn load_reads_back() {
    let mut h = DevHarness::new();
    let mut t = running_ch0(&mut h, 0xFFFF_FFFF, 100);
    h.write(&mut t, LOAD1, 4, 0x1234_5678);
    assert_eq!(h.read(&mut t, LOAD0, 4), 0xFFFF_FFFF);
    assert_eq!(h.read(&mut t, LOAD1, 4), 0x1234_5678);
    assert_eq!(h.read(&mut t, LOAD2, 4), 0);
    // A LOAD write does not touch the running count.
    h.now = 4_761;
    h.write(&mut t, LOAD0, 4, 7);
    assert_eq!(h.read(&mut t, LOAD0, 4), 7);
    assert_eq!(h.read(&mut t, VALUE0, 4), 99);
    h.now = 2 * 4_761;
    assert_eq!(h.read(&mut t, VALUE0, 4), 98);
    // CTRL reads back too, all bits.
    h.write(&mut t, CTRL, 4, 0xFFFF_FFFF);
    assert_eq!(h.read(&mut t, CTRL, 4), 0xFFFF_FFFF);
}

#[test]
fn ctrl_write_latches_carrying_remainder() {
    let mut h = DevHarness::new();
    let mut t = Timer::new();
    h.write(&mut t, CTRL, 4, 0b011);
    h.write(&mut t, VALUE0, 4, 1000);
    // 50,000 cycles = 10.5 ticks at 32 kHz; 10 ticks consume 47,608 cycles, 2,392 carry.
    h.now = 50_000;
    h.write(&mut t, CTRL, 4, 0b001);
    assert_eq!(h.read(&mut t, VALUE0, 4), 990);
    // Now at 39 kHz (4,000 cycles per tick), counted from cycle 47,608.
    h.now = 51_607;
    assert_eq!(h.read(&mut t, VALUE0, 4), 990);
    h.now = 51_608;
    assert_eq!(h.read(&mut t, VALUE0, 4), 989);
}

#[test]
fn value_write_restarts_from_written_value() {
    let mut h = DevHarness::new();
    let mut t = running_ch0(&mut h, 0xFFFF_FFFF, 1000);
    h.now = 10_000;
    h.write(&mut t, VALUE0, 4, 50);
    assert_eq!(h.read(&mut t, VALUE0, 4), 50);
    h.now = 10_000 + 4_760;
    assert_eq!(h.read(&mut t, VALUE0, 4), 50);
    h.now = 10_000 + 4_761;
    assert_eq!(h.read(&mut t, VALUE0, 4), 49);
}

#[test]
fn int_stat_w1c() {
    let mut h = DevHarness::new();
    let mut t = running_ch0(&mut h, 0xFFFF_FFFF, 0);
    // Underflow many times: no status bit is ever set, no IRQ line is raised.
    h.now = 1_000_000_000;
    let _ = h.read(&mut t, VALUE0, 4);
    assert_eq!(h.read(&mut t, INT_STAT, 4), 0);
    h.write(&mut t, INT_STAT, 4, 1 << 2 | 1 << 5 | 1 << 8);
    assert_eq!(h.read(&mut t, INT_STAT, 4), 0);
    // INT_EN stores (a warning is logged once) but IRQs 1-3 stay low.
    h.write(&mut t, INT_EN, 4, 1 << 2 | 1 << 5 | 1 << 8);
    h.write(&mut t, INT_EN, 4, 1 << 2);
    assert_eq!(h.read(&mut t, INT_EN, 4), 1 << 2);
    h.now = 2_000_000_000;
    let _ = h.read(&mut t, VALUE0, 4);
    assert_eq!(h.irq, 0);
    assert!(h.pulses.is_empty());
    assert!(h.log.is_empty());
}

#[test]
fn unmodeled_offsets_and_widths_logged() {
    let mut h = DevHarness::new();
    let mut t = Timer::new();
    h.write(&mut t, LOAD0, 4, 0xFFFF_FFFF);
    assert_eq!(h.read(&mut t, 0x08, 4), 0);
    h.write(&mut t, 0x3C, 4, 1);
    assert_eq!(h.read(&mut t, 0xFFC, 4), 0);
    assert_eq!(h.read(&mut t, LOAD0, 2), 0);
    h.write(&mut t, LOAD0, 1, 0);
    assert_eq!(h.read(&mut t, 0x05, 4), 0);
    assert_eq!(h.read(&mut t, LOAD0, 4), 0xFFFF_FFFF);
    assert_eq!(h.log.len(), 6);
}

/// The count is exact on both sides of the point where `elapsed * 32768` leaves 64 bits.
#[test]
fn count_is_exact_past_64_bit_products() {
    let expect = |now: u64| {
        let ticks = u128::from(now) * 32_768 / 156_000_000;
        // From 0xFFFF_FFFF with LOAD 0xFFFF_FFFF: one period is 2^32 ticks.
        (0xFFFF_FFFFu128 - ticks % (1 << 32)) as u32
    };
    let boundary = u64::MAX / 32_768;
    for now in [
        boundary - 1,
        boundary,
        boundary + 1,
        boundary * 3,
        u64::MAX / 2,
    ] {
        let mut h = DevHarness::new();
        let mut t = running_ch0(&mut h, 0xFFFF_FFFF, 0xFFFF_FFFF);
        h.now = now;
        assert_eq!(h.read(&mut t, VALUE0, 4), expect(now), "at {now}");
        // Latching (a CTRL write) keeps the count and carries the part tick.
        h.write(&mut t, CTRL, 4, CH0_EN_32K);
        assert_eq!(h.read(&mut t, VALUE0, 4), expect(now), "latched at {now}");
        h.now = now + 4_761;
        let next = expect(now + 4_761);
        assert_eq!(h.read(&mut t, VALUE0, 4), next, "a tick after {now}");
    }
}

#[test]
fn no_overflow_at_u64_scale() {
    let mut h = DevHarness::new();
    let mut t = Timer::new();
    h.write(&mut t, LOAD0, 4, 0xFFFF_FFFF);
    h.write(&mut t, LOAD1, 4, 3);
    h.write(&mut t, LOAD2, 4, 0);
    // Every channel enabled; ch0 and ch1 at 32 kHz, ch2 at 39 kHz.
    h.write(&mut t, CTRL, 4, 0b001_011_011);
    for now in [1u64 << 62, u64::MAX - 1, u64::MAX] {
        h.now = now;
        for off in [VALUE0, VALUE1, VALUE2] {
            let _ = h.read(&mut t, off, 4);
        }
        assert!(h.read(&mut t, VALUE1, 4) <= 3);
        assert_eq!(h.read(&mut t, VALUE2, 4), 0);
        h.write(&mut t, CTRL, 4, 0b001_011_011);
    }
    h.write(&mut t, VALUE0, 4, 5);
    assert_eq!(h.read(&mut t, VALUE0, 4), 5);
    // Time going backwards (a host bug) does not panic either.
    h.now = 0;
    let _ = h.read(&mut t, VALUE0, 4);
    h.write(&mut t, CTRL, 4, 0);
}

#[test]
fn system_reset_keeps_state_power_on_clears() {
    let mut h = DevHarness::new();
    let mut t = running_ch0(&mut h, 0xFFFF_FFFF, 1000);
    h.now = 4_761;
    h.reset(&mut t, ResetKind::System);
    assert_eq!(h.read(&mut t, VALUE0, 4), 999);
    assert_eq!(h.read(&mut t, CTRL, 4), CH0_EN_32K);
    assert_eq!(h.read(&mut t, LOAD0, 4), 0xFFFF_FFFF);
    h.reset(&mut t, ResetKind::PowerOn);
    assert_eq!(h.read(&mut t, VALUE0, 4), 0);
    assert_eq!(h.read(&mut t, CTRL, 4), 0);
    assert_eq!(h.read(&mut t, LOAD0, 4), 0);
}

#[test]
fn state_roundtrip() {
    let mut h = DevHarness::new();
    let mut t = running_ch0(&mut h, 0xFFFF_FFFF, 1000);
    h.write(&mut t, LOAD2, 4, 77);
    h.write(&mut t, INT_EN, 4, 1 << 8);
    h.now = 30_000;
    let st = t.save();
    let mut fresh = Timer::new();
    fresh.load(&st).unwrap();
    for now in [30_000, 100_000, 10_000_000] {
        h.now = now;
        assert_eq!(
            h.read(&mut fresh, VALUE0, 4),
            h.read(&mut t, VALUE0, 4),
            "at {now}"
        );
    }
    assert_eq!(h.read(&mut fresh, LOAD2, 4), 77);
    assert_eq!(h.read(&mut fresh, CTRL, 4), CH0_EN_32K);
    assert_eq!(h.read(&mut fresh, INT_EN, 4), 1 << 8);
    // Garbage is refused and leaves the device unchanged.
    assert!(fresh.load(&[0xFF; 3]).is_err());
    assert_eq!(h.read(&mut fresh, LOAD2, 4), 77);
}

#[test]
fn soc_attaches_timer() {
    let mut map = MemoryMap::new();
    evo_hw::soc::attach(&mut map);
    assert!(map.device::<Timer>("timer").is_some());
    let r = Access {
        kind: AccessKind::Read,
        secure: false,
        privileged: true,
    };
    let w = Access {
        kind: AccessKind::Write,
        ..r
    };
    map.write32(0x4010_1004, 0xFFFF_FFFF, w).unwrap();
    map.write32(0x4010_1000, 0xFFFF_FFFF, w).unwrap();
    map.write32(0x4010_1030, CH0_EN_32K, w).unwrap();
    map.now = 15_600_000;
    assert_eq!(
        map.read32(0x4010_1000, r).ok().map(|v| 0xFFFF_FFFF - v),
        Some(3276)
    );
    assert_eq!(map.read32(0x4010_1004, r).ok(), Some(0xFFFF_FFFF));
    assert!(map.log.is_empty());
}
