//! SIE-200 MPC model (`docs/hardware/security.md` §1).

use armv8m::{Access, AccessKind, Bus};
use evo_hw::dev::mpc::{MPC_BLK_CFG, Mpc};
use evo_hw::{DevHarness, Device, MemoryMap, ResetKind, StateError};

const CTRL: u32 = 0x00;
const BLK_MAX: u32 = 0x10;
const BLK_CFG: u32 = 0x14;
const BLK_IDX: u32 = 0x18;
const BLK_LUT: u32 = 0x1C;
const INT_STAT: u32 = 0x20;
const INT_CLEAR: u32 = 0x24;
const INT_EN: u32 = 0x28;
const INT_INFO1: u32 = 0x2C;
const INT_INFO2: u32 = 0x30;

#[test]
fn geometry() {
    let mut h = DevHarness::new();
    let mut sram = Mpc::new(3);
    assert_eq!(h.read(&mut sram, BLK_MAX, 4), 3);
    assert_eq!(h.read(&mut sram, BLK_CFG, 4), 7);
    assert_eq!(MPC_BLK_CFG, 7);
    assert_eq!(sram.lut().len(), 4);
    let mut flash = Mpc::new(127);
    assert_eq!(h.read(&mut flash, BLK_MAX, 4), 127);
    assert_eq!(h.read(&mut flash, BLK_CFG, 4), 7);
    assert_eq!(flash.lut().len(), 128);
    assert!(h.log.is_empty());
}

#[test]
fn soc_attaches_four_mpcs() {
    let mut map = MemoryMap::new();
    evo_hw::soc::attach(&mut map);
    for (name, blk_max) in [
        ("mpc_sram0", 3),
        ("mpc_sram1", 3),
        ("mpc_sram2", 3),
        ("mpc_flash", 127),
    ] {
        let mpc = map.device::<Mpc>(name).expect(name);
        assert_eq!(mpc.lut().len(), blk_max + 1, "{name}");
    }
    for (base, blk_max) in [
        (0x5020_4000u32, 3u32),
        (0x5020_5000, 3),
        (0x5020_6000, 3),
        (0x5020_8000, 127),
    ] {
        let a = Access {
            kind: AccessKind::Read,
            secure: true,
            privileged: true,
        };
        assert_eq!(
            map.read32(base + BLK_MAX, a).ok(),
            Some(blk_max),
            "{base:#x}"
        );
        assert_eq!(
            map.read32(base + 0xFFC, a).ok(),
            Some(0),
            "{base:#x} window end"
        );
    }
}

#[test]
fn lut_explicit_index() {
    let mut h = DevHarness::new();
    let mut m = Mpc::new(127);
    h.write(&mut m, BLK_IDX, 4, 8);
    h.write(&mut m, BLK_LUT, 4, 0xFFFF_FFFF);
    h.write(&mut m, BLK_IDX, 4, 8);
    assert_eq!(h.read(&mut m, BLK_LUT, 4), 0xFFFF_FFFF);
    assert_eq!(m.lut()[8], 0xFFFF_FFFF);
    assert_eq!(m.lut()[7], 0);
    assert_eq!(m.lut()[9], 0);
    // Without AUTOINC the index stays put.
    assert_eq!(h.read(&mut m, BLK_IDX, 4), 8);
}

#[test]
fn idx_clamped_to_blk_max() {
    let mut h = DevHarness::new();
    let mut m = Mpc::new(3);
    h.write(&mut m, BLK_IDX, 4, 1000);
    assert_eq!(h.read(&mut m, BLK_IDX, 4), 3);
    h.write(&mut m, BLK_LUT, 4, 0x1234);
    assert_eq!(m.lut()[3], 0x1234);
}

#[test]
fn autoinc_when_ctrl_bit8() {
    let mut h = DevHarness::new();
    let mut m = Mpc::new(3);
    h.write(&mut m, CTRL, 4, 0x100);
    h.write(&mut m, BLK_IDX, 4, 0);
    h.write(&mut m, BLK_LUT, 4, 0xAAAA_AAAA);
    h.write(&mut m, BLK_LUT, 4, 0x5555_5555);
    assert_eq!(&m.lut()[0..2], &[0xAAAA_AAAA, 0x5555_5555]);
    assert_eq!(h.read(&mut m, BLK_IDX, 4), 2);
    // Reads advance too, and the index saturates at BLK_MAX.
    assert_eq!(h.read(&mut m, BLK_LUT, 4), 0);
    assert_eq!(h.read(&mut m, BLK_IDX, 4), 3);
    assert_eq!(h.read(&mut m, BLK_LUT, 4), 0);
    assert_eq!(h.read(&mut m, BLK_IDX, 4), 3);
}

#[test]
fn lockdown_freezes() {
    let mut h = DevHarness::new();
    let mut m = Mpc::new(3);
    h.write(&mut m, BLK_IDX, 4, 1);
    h.write(&mut m, BLK_LUT, 4, 0x1111);
    assert!(!m.locked());
    h.write(&mut m, CTRL, 4, 0x8000_0100);
    assert!(m.locked());
    h.write(&mut m, CTRL, 4, 0);
    assert_eq!(h.read(&mut m, CTRL, 4), 0x8000_0100);
    h.write(&mut m, BLK_IDX, 4, 1);
    h.write(&mut m, BLK_LUT, 4, 0xFFFF_FFFF);
    assert_eq!(m.lut(), &[0, 0x1111, 0, 0]);
    assert!(m.locked());
}

#[test]
fn int_regs() {
    let mut h = DevHarness::new();
    let mut m = Mpc::new(3);
    h.write(&mut m, INT_EN, 4, 1);
    assert_eq!(h.read(&mut m, INT_EN, 4), 1);
    h.write(&mut m, INT_EN, 4, 0xFFFF_FFFF);
    assert_eq!(h.read(&mut m, INT_EN, 4), 1);
    assert_eq!(h.read(&mut m, INT_STAT, 4), 0);
    h.write(&mut m, INT_CLEAR, 4, 1);
    assert_eq!(h.read(&mut m, INT_STAT, 4), 0);
    assert_eq!(h.read(&mut m, INT_INFO1, 4), 0);
    assert_eq!(h.read(&mut m, INT_INFO2, 4), 0);
    assert_eq!(h.irq, 0);
    assert!(h.pulses.is_empty());
    assert!(h.log.is_empty());
}

fn configured(h: &mut DevHarness) -> Mpc {
    let mut m = Mpc::new(3);
    h.write(&mut m, BLK_IDX, 4, 2);
    h.write(&mut m, BLK_LUT, 4, 0xFFFF_0000);
    h.write(&mut m, INT_EN, 4, 1);
    h.write(&mut m, CTRL, 4, 0x8000_0100);
    m
}

#[test]
fn system_reset_keeps_power_on_clears() {
    let mut h = DevHarness::new();
    let mut m = configured(&mut h);
    let idx = h.read(&mut m, BLK_IDX, 4);

    h.reset(&mut m, ResetKind::System);
    assert!(m.locked());
    assert_eq!(h.read(&mut m, CTRL, 4), 0x8000_0100);
    assert_eq!(m.lut()[2], 0xFFFF_0000);
    assert_eq!(h.read(&mut m, BLK_IDX, 4), idx);
    assert_eq!(h.read(&mut m, INT_EN, 4), 1);

    h.reset(&mut m, ResetKind::PowerOn);
    assert!(!m.locked());
    assert_eq!(h.read(&mut m, CTRL, 4), 0);
    assert_eq!(m.lut(), &[0, 0, 0, 0]);
    assert_eq!(h.read(&mut m, BLK_IDX, 4), 0);
    assert_eq!(h.read(&mut m, INT_EN, 4), 0);
    assert_eq!(h.read(&mut m, BLK_MAX, 4), 3);
    assert_eq!(h.irq, 0);
}

#[test]
fn state_roundtrip() {
    let mut h = DevHarness::new();
    let m = configured(&mut h);
    let s = m.save();

    let mut fresh = Mpc::new(3);
    fresh.load(&s).unwrap();
    assert!(fresh.locked());
    assert_eq!(fresh.lut(), m.lut());
    assert_eq!(h.read(&mut fresh, INT_EN, 4), 1);
    assert_eq!(h.read(&mut fresh, BLK_IDX, 4), 2);

    // A state from an MPC of another size is refused, leaving the device unchanged.
    let mut flash = Mpc::new(127);
    assert!(matches!(flash.load(&s), Err(StateError::Mismatch(_))));
    assert!(!flash.locked());
    assert_eq!(flash.lut().len(), 128);
    // Garbage is refused too.
    assert!(fresh.load(&[0xFF; 3]).is_err());
    assert!(fresh.locked());
}

#[test]
fn unknown_offset_logged() {
    let mut h = DevHarness::new();
    let mut m = Mpc::new(3);
    assert_eq!(h.read(&mut m, 0x40, 4), 0);
    assert_eq!(h.log.len(), 1);
    let (a, _) = h.log.entries().next().unwrap();
    assert_eq!(a.addr, 0x40);
    assert_eq!(a.write, None);

    h.write(&mut m, 0xFFC, 4, 5);
    assert_eq!(h.log.len(), 2);
    // Byte access to a register is not modeled.
    assert_eq!(h.read(&mut m, BLK_MAX, 1), 0);
    assert_eq!(h.log.len(), 3);
}
