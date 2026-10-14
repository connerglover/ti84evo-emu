//! SPCTRL, CRU (`docs/hardware/security.md` §2, §3), LED pattern generator (§6) and ICACHE
//! (`docs/hardware/spif-nor.md` §7) models.

use armv8m::{Access, AccessKind, Bus};
use evo_hw::dev::storage::Storage;
use evo_hw::dev::sysctl::{
    CRU_CHIPINFO, CRU_CLKSEL_RESET, CRU_ID, CRU_PLL_RESET, CRU_UID_LO, Cru, Spctrl,
};
use evo_hw::{DevHarness, Device, MemoryMap, ResetKind, StateError};

// --- SPCTRL ---------------------------------------------------------------------------------

#[test]
fn spctrl_ppc_bits_store() {
    let mut h = DevHarness::new();
    let mut s = Spctrl::new();
    h.write(&mut s, 0x64, 4, 0x1BFF);
    h.write(&mut s, 0x60, 4, 0x59);
    let v = h.read(&mut s, 0x80, 4);
    h.write(&mut s, 0x80, 4, v | 1);
    assert_eq!(h.read(&mut s, 0x64, 4), 0x1BFF);
    assert_eq!(h.read(&mut s, 0x60, 4), 0x59);
    assert_eq!(h.read(&mut s, 0x80, 4), 1);
    assert_eq!(s.reg(0x64), 0x1BFF);
    assert_eq!(s.reg(0x80), 1);
    // Anywhere else in the window is storage too.
    h.write(&mut s, 0xFFC, 4, 0xDEAD_BEEF);
    assert_eq!(h.read(&mut s, 0xFFC, 4), 0xDEAD_BEEF);
    assert_eq!(h.irq, 0);
    assert!(h.log.is_empty());
}

#[test]
fn spctrl_status_is_read_only() {
    let mut h = DevHarness::new();
    let mut s = Spctrl::new();
    h.write(&mut s, 0x20, 4, 0xFFFF_FFFF);
    assert_eq!(h.read(&mut s, 0x20, 4), 0);
    h.write(&mut s, 0x24, 4, 0x7);
    assert_eq!(h.read(&mut s, 0x24, 4), 0);
    assert_eq!(h.read(&mut s, 0x20, 4), 0);
    h.write(&mut s, 0xC0, 4, 0x50);
    assert_eq!(h.read(&mut s, 0xC0, 4), 0);
    assert_eq!(s.reg(0xC0), 0);
    // The IRQ enables are plain storage, and IRQ 8 stays low.
    h.write(&mut s, 0x28, 4, 7);
    assert_eq!(h.read(&mut s, 0x28, 4), 7);
    assert_eq!(h.irq, 0);
    assert!(h.log.is_empty());
}

#[test]
fn spctrl_narrow_access_logged() {
    let mut h = DevHarness::new();
    let mut s = Spctrl::new();
    h.write(&mut s, 0x64, 4, 0x1BFF);
    assert_eq!(h.read(&mut s, 0x64, 1), 0);
    h.write(&mut s, 0x64, 2, 0);
    assert_eq!(s.reg(0x64), 0x1BFF);
    assert_eq!(h.log.len(), 2);
}

#[test]
fn spctrl_system_reset_keeps_power_on_clears() {
    let mut h = DevHarness::new();
    let mut s = Spctrl::new();
    h.write(&mut s, 0x64, 4, 0x1BFF);
    h.write(&mut s, 0x14, 4, 1);
    h.reset(&mut s, ResetKind::System);
    assert_eq!(h.read(&mut s, 0x64, 4), 0x1BFF);
    assert_eq!(h.read(&mut s, 0x14, 4), 1);
    h.reset(&mut s, ResetKind::PowerOn);
    assert_eq!(h.read(&mut s, 0x64, 4), 0);
    assert_eq!(h.read(&mut s, 0x14, 4), 0);
}

#[test]
fn spctrl_state_roundtrip() {
    let mut h = DevHarness::new();
    let mut s = Spctrl::new();
    h.write(&mut s, 0x70, 4, 0x1234_5678);
    h.write(&mut s, 0xD4, 4, 0x100);
    let st = s.save();
    let mut fresh = Spctrl::new();
    fresh.load(&st).unwrap();
    assert_eq!(fresh.reg(0x70), 0x1234_5678);
    assert_eq!(fresh.reg(0xD4), 0x100);
    // Garbage is refused and leaves the device unchanged.
    assert!(fresh.load(&[0xFF; 3]).is_err());
    assert_eq!(fresh.reg(0x70), 0x1234_5678);
}

// --- CRU ------------------------------------------------------------------------------------

#[test]
fn cru_reset_values() {
    assert_eq!(CRU_ID, 0x0011_0000);
    assert_eq!(CRU_CLKSEL_RESET, 0x0002_0001);
    assert_eq!(CRU_PLL_RESET, 0x0000_3433);
    assert_eq!(CRU_UID_LO, 0x4556_4F31);
    assert_eq!(CRU_CHIPINFO, 0);
    let mut h = DevHarness::new();
    let mut c = Cru::new();
    assert_eq!(h.read(&mut c, 0x04, 4), 0x0002_0001);
    assert_eq!(h.read(&mut c, 0x1C, 4), 0x3433);
    assert_eq!(h.read(&mut c, 0xFC, 4), 0x0011_0000);
    assert_eq!(h.read(&mut c, 0x0C, 4), 0x4556_4F31);
    assert_eq!(h.read(&mut c, 0x10, 4), 0);
    assert_eq!(c.reg(0x04), 0x0002_0001);
    assert_eq!(c.reg(0xFC), 0x0011_0000);
    for off in [0x00, 0x08, 0x14, 0x18, 0x20, 0x4C, 0x50, 0x60, 0x70, 0x94] {
        assert_eq!(h.read(&mut c, off, 4), 0, "{off:#x}");
    }
    assert!(h.log.is_empty());
}

#[test]
fn cru_id_read_only() {
    let mut h = DevHarness::new();
    let mut c = Cru::new();
    for off in [0xFC, 0x0C, 0x10] {
        h.write(&mut c, off, 4, 0xFFFF_FFFF);
    }
    assert_eq!(h.read(&mut c, 0xFC, 4), 0x0011_0000);
    assert_eq!(h.read(&mut c, 0x0C, 4), 0x4556_4F31);
    assert_eq!(h.read(&mut c, 0x10, 4), 0);
    assert!(h.log.is_empty());
}

#[test]
fn cru_pll_lock_follows_enable() {
    let mut h = DevHarness::new();
    let mut c = Cru::new();
    h.write(&mut c, 0x1C, 4, 0x3430);
    assert_eq!(h.read(&mut c, 0x1C, 4), 0x3430);
    h.write(&mut c, 0x1C, 4, 0x3431);
    assert_eq!(h.read(&mut c, 0x1C, 4), 0x3433);
    // A written LOCK bit without ENABLE does not read back.
    h.write(&mut c, 0x1C, 4, 0x3432);
    assert_eq!(h.read(&mut c, 0x1C, 4), 0x3430);
    assert_eq!(c.reg(0x1C), 0x3430);
}

#[test]
fn cru_system_reset_keeps_clock_gates() {
    let mut h = DevHarness::new();
    let mut c = Cru::new();
    h.write(&mut c, 0x14, 4, 0x197D0);
    h.write(&mut c, 0x04, 4, 0x0002_0000);
    h.reset(&mut c, ResetKind::System);
    assert_eq!(h.read(&mut c, 0x14, 4), 0x197D0);
    assert_eq!(h.read(&mut c, 0x04, 4), 0x0002_0000);
    h.reset(&mut c, ResetKind::PowerOn);
    assert_eq!(h.read(&mut c, 0x14, 4), 0);
    assert_eq!(h.read(&mut c, 0x04, 4), 0x0002_0001);
    assert_eq!(h.read(&mut c, 0x1C, 4), 0x3433);
    assert_eq!(h.read(&mut c, 0xFC, 4), 0x0011_0000);
}

#[test]
fn cru_narrow_access_logged() {
    let mut h = DevHarness::new();
    let mut c = Cru::new();
    assert_eq!(h.read(&mut c, 0xFE, 2), 0);
    h.write(&mut c, 0x14, 1, 0xFF);
    assert_eq!(c.reg(0x14), 0);
    assert_eq!(h.log.len(), 2);
}

#[test]
fn cru_state_roundtrip() {
    let mut h = DevHarness::new();
    let mut c = Cru::new();
    h.write(&mut c, 0x14, 4, 0x197D0);
    h.write(&mut c, 0x18, 4, 0x160);
    h.write(&mut c, 0x1C, 4, 0x3430);
    let st = c.save();
    let mut fresh = Cru::new();
    fresh.load(&st).unwrap();
    assert_eq!(h.read(&mut fresh, 0x14, 4), 0x197D0);
    assert_eq!(h.read(&mut fresh, 0x18, 4), 0x160);
    assert_eq!(h.read(&mut fresh, 0x1C, 4), 0x3430);
    assert_eq!(h.read(&mut fresh, 0xFC, 4), 0x0011_0000);
    assert!(fresh.load(&[0xFF; 3]).is_err());
    assert_eq!(fresh.reg(0x14), 0x197D0);
}

// --- Storage (LED pattern generator, ICACHE) --------------------------------------------------

#[test]
fn storage_lanes() {
    let mut h = DevHarness::new();
    let mut s = Storage::new(0x100);
    h.write(&mut s, 0x05, 1, 0xAB);
    assert_eq!(s.word(4), 0x0000_AB00);
    assert_eq!(h.read(&mut s, 0x04, 2), 0xAB00);
    assert_eq!(h.read(&mut s, 0x06, 2), 0);
    assert_eq!(h.read(&mut s, 0x05, 1), 0xAB);
    h.write(&mut s, 0x06, 2, 0x1234);
    assert_eq!(s.word(4), 0x1234_AB00);
    assert_eq!(h.read(&mut s, 0x04, 4), 0x1234_AB00);
    h.write(&mut s, 0x08, 4, 0xFFFF_F800);
    assert_eq!(h.read(&mut s, 0x0A, 2), 0xFFFF);
    assert_eq!(h.read(&mut s, 0x08, 1), 0x00);
    // The last word of the window.
    h.write(&mut s, 0xFC, 4, 0x8000_1643);
    assert_eq!(s.word(0xFC), 0x8000_1643);
    assert!(h.log.is_empty());
}

#[test]
fn storage_out_of_range_logged() {
    let mut h = DevHarness::new();
    let mut s = Storage::new(0x10);
    assert_eq!(h.read(&mut s, 0x10, 4), 0);
    h.write(&mut s, 0x0E, 4, 0xFFFF_FFFF);
    assert_eq!(s.word(0x0C), 0);
    assert_eq!(s.word(0x10), 0);
    assert_eq!(h.read(&mut s, 0, 3), 0);
    assert_eq!(h.log.len(), 3);
}

#[test]
fn storage_resets_to_zero() {
    let mut h = DevHarness::new();
    let mut s = Storage::new(0x1000);
    h.write(&mut s, 0x04, 4, 0x1D);
    h.reset(&mut s, ResetKind::System);
    assert_eq!(s.word(4), 0);
    h.write(&mut s, 0x04, 4, 0x1D);
    h.reset(&mut s, ResetKind::PowerOn);
    assert_eq!(s.word(4), 0);
}

#[test]
fn storage_state_roundtrip() {
    let mut h = DevHarness::new();
    let mut s = Storage::new(0x100);
    h.write(&mut s, 0x00, 4, 0x8000_0603);
    h.write(&mut s, 0x0C, 4, 0x00FF_FF00);
    let st = s.save();
    let mut fresh = Storage::new(0x100);
    fresh.load(&st).unwrap();
    assert_eq!(fresh.word(0), 0x8000_0603);
    assert_eq!(fresh.word(0x0C), 0x00FF_FF00);
    // A state of another size is refused, leaving the device unchanged.
    let mut other = Storage::new(0x1000);
    assert!(matches!(other.load(&st), Err(StateError::Mismatch(_))));
    assert_eq!(other.word(0), 0);
    assert!(fresh.load(&[0xFF; 3]).is_err());
    assert_eq!(fresh.word(0), 0x8000_0603);
}

// --- SoC wiring -----------------------------------------------------------------------------

#[test]
fn soc_attaches_sysctl_blocks() {
    let mut map = MemoryMap::new();
    evo_hw::soc::attach(&mut map);
    assert!(map.device::<Cru>("cru").is_some());
    assert!(map.device::<Spctrl>("spctrl").is_some());
    assert!(map.device::<Storage>("ledpat").is_some());
    assert!(map.device::<Storage>("icache").is_some());
    let a = Access {
        kind: AccessKind::Read,
        secure: true,
        privileged: true,
    };
    assert_eq!(map.read32(0x5020_00FC, a).ok(), Some(0x0011_0000));
    assert_eq!(map.read32(0x5020_0004, a).ok(), Some(0x0002_0001));
    assert_eq!(map.read32(0x5020_1FFC, a).ok(), Some(0));
    assert_eq!(map.read32(0x5020_2BFC, a).ok(), Some(0));
    assert_eq!(map.read32(0x5000_5004, a).ok(), Some(0));
    assert_eq!(map.read32(0x5000_5FFC, a).ok(), Some(0));
    let w = Access {
        kind: AccessKind::Write,
        ..a
    };
    map.write32(0x5000_5004, 0x1D, w).unwrap();
    assert_eq!(map.read32(0x5000_5004, a).ok(), Some(0x1D));
    map.write32(0x5020_2B00, 0x8000_0603, w).unwrap();
    assert_eq!(
        map.device::<Storage>("ledpat").map(|s| s.word(0)),
        Some(0x8000_0603)
    );
    assert!(map.log.is_empty());
}
