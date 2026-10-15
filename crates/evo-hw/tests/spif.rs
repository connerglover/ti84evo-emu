//! SPIF QSPI controller at 0x5000_1000 over the NOR engine (`docs/hardware/spif-nor.md` §3,
//! §5, §8).

use evo_hw::dev::spif::{SPIF_FIFO, Spif};
use evo_hw::nor::{NOR_SIZE, SECREGS_LEN};
use evo_hw::soc::FLASH_BUFFER;
use evo_hw::{DevHarness, Device, MemoryMap, ResetKind};

const ADDR: u32 = 0x00;
const CFG: u32 = 0x04;
const LEN: u32 = 0x08;
const OP: u32 = 0x0C;
const CFG0: u32 = 0x10;
const STATUS: u32 = 0x18;
const CTRL: u32 = 0x20;
const INT: u32 = 0x24;
const XIP_CFG: u32 = 0x30;
const XIP_CTRL: u32 = 0x34;
const LEVEL: u32 = 0x54;
const FIFO: u32 = 0x100;

/// CFG for an instruction-only command and for one with 3 address bytes.
const CFG_NOADDR: u32 = 0x0100_0000;
const CFG_ADDR3: u32 = 0x0100_0003;
const WREN: u32 = 0x0600_0002;
const RDSR: u32 = 0x0500_0000;

fn setup() -> (DevHarness, Spif) {
    let h = DevHarness::new().with_buffer(FLASH_BUFFER, vec![0; NOR_SIZE]);
    (h, Spif::new())
}

/// The firmware's command start (`spif-nor.md` §5.1).
fn cmd(h: &mut DevHarness, s: &mut Spif, op: u32, cfg: u32, addr: u32, len: u32) {
    h.write(s, INT, 4, 1);
    h.write(s, ADDR, 4, addr);
    h.write(s, CFG, 4, cfg);
    h.write(s, LEN, 4, len);
    h.write(s, OP, 4, op);
    h.write(s, CTRL, 4, 0x1102);
}

fn pop_bytes(h: &mut DevHarness, s: &mut Spif, n: usize) -> Vec<u8> {
    (0..n).map(|_| h.read(s, FIFO, 1) as u8).collect()
}

fn rdsr(h: &mut DevHarness, s: &mut Spif) -> u8 {
    cmd(h, s, RDSR, CFG_NOADDR, 0, 1);
    h.read(s, FIFO, 1) as u8
}

#[test]
fn jedec_id() {
    let (mut h, mut s) = setup();
    assert_eq!(SPIF_FIFO, 64);
    cmd(&mut h, &mut s, 0x9F00_0000, CFG_NOADDR, 0, 3);
    assert_eq!(h.read(&mut s, INT, 4), 1);
    assert_eq!(h.read(&mut s, STATUS, 4) & 2, 2);
    assert_eq!(h.read(&mut s, LEVEL, 4), 0x300);
    assert_eq!(pop_bytes(&mut h, &mut s, 3), [0xEF, 0x40, 0x18]);
    assert_eq!(h.read(&mut s, STATUS, 4), 1);
    assert_eq!(h.read(&mut s, LEVEL, 4), 0);
    assert!(h.log.is_empty());
}

#[test]
fn rx_survives_done_clear() {
    let (mut h, mut s) = setup();
    cmd(&mut h, &mut s, RDSR, CFG_NOADDR, 0, 1);
    h.write(&mut s, INT, 4, 1);
    assert_eq!(h.read(&mut s, INT, 4), 0);
    h.write(&mut s, CTRL, 4, 0x1100);
    assert_eq!(h.read(&mut s, CTRL, 4), 0x1100);
    assert_eq!(h.read(&mut s, STATUS, 4) & 2, 2);
    assert_eq!(h.read(&mut s, FIFO, 1), 0x00);
    assert_eq!(h.read(&mut s, STATUS, 4), 1);
}

#[test]
fn erase_and_program_hit_flash_buffer() {
    let (mut h, mut s) = setup();
    cmd(&mut h, &mut s, WREN, CFG_NOADDR, 0, 0);
    assert_eq!(h.read(&mut s, INT, 4), 1);
    cmd(&mut h, &mut s, 0x2000_0006, CFG_ADDR3, 0x9_0000, 0);
    assert_eq!(h.read(&mut s, INT, 4), 1);
    let f = h.buffer(FLASH_BUFFER);
    assert!(f[0x9_0000..0x9_1000].iter().all(|&b| b == 0xFF));
    assert_eq!(f[0x8_FFFF], 0);
    assert_eq!(f[0x9_1000], 0);

    // 0x910F8 lies in the next sector; erase it as the firmware does (programming is an AND).
    cmd(&mut h, &mut s, WREN, CFG_NOADDR, 0, 0);
    cmd(&mut h, &mut s, 0x2000_0006, CFG_ADDR3, 0x9_1000, 0);
    cmd(&mut h, &mut s, WREN, CFG_NOADDR, 0, 0);
    cmd(&mut h, &mut s, 0x0200_0002, CFG_ADDR3, 0x910F8, 8);
    assert_eq!(h.read(&mut s, LEVEL, 4), 0x800);
    assert_eq!(h.read(&mut s, INT, 4), 0, "DONE waits for the last byte");
    assert_eq!(h.read(&mut s, STATUS, 4), 1, "a write phase has no RX data");
    h.write(&mut s, FIFO, 4, 0x4433_2211);
    assert_eq!(h.read(&mut s, LEVEL, 4), 0x400);
    assert_eq!(h.read(&mut s, INT, 4), 0);
    h.write(&mut s, FIFO, 4, 0x8877_6655);
    assert_eq!(h.read(&mut s, INT, 4), 1);
    assert_eq!(h.read(&mut s, LEVEL, 4), 0);
    assert_eq!(
        h.buffer(FLASH_BUFFER)[0x910F8..0x91100],
        [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]
    );
    assert_eq!(rdsr(&mut h, &mut s), 0x00, "WEL cleared by the program");
}

#[test]
fn start_is_level_triggered() {
    let (mut h, mut s) = setup();
    cmd(&mut h, &mut s, 0x9F00_0000, CFG_NOADDR, 0, 3);
    assert_eq!(pop_bytes(&mut h, &mut s, 1), [0xEF]);
    // START is still set (a timed-out command): the next write with bit 1 set starts again.
    h.write(&mut s, CTRL, 4, 0x1102);
    assert_eq!(h.read(&mut s, LEVEL, 4), 0x300);
    assert_eq!(pop_bytes(&mut h, &mut s, 3), [0xEF, 0x40, 0x18]);
    // A new descriptor with START left set runs too, aborting the open phase.
    cmd(&mut h, &mut s, 0x9F00_0000, CFG_NOADDR, 0, 3);
    h.write(&mut s, OP, 4, 0x3500_0000);
    h.write(&mut s, LEN, 4, 1);
    h.write(&mut s, CTRL, 4, 0x1102);
    assert_eq!(h.read(&mut s, LEVEL, 4), 0x100);
    assert_eq!(pop_bytes(&mut h, &mut s, 1), [0x02]);
}

#[test]
fn fifo_level_multiple_of_four() {
    let (mut h, mut s) = setup();
    cmd(&mut h, &mut s, 0x0300_0000, CFG_ADDR3, 0, 100);
    assert_eq!(h.read(&mut s, LEVEL, 4), 0x4000);
    assert_eq!(h.read(&mut s, STATUS, 4), 0x3);
    for _ in 0..16 {
        h.read(&mut s, FIFO, 4);
    }
    assert_eq!(h.read(&mut s, LEVEL, 4), 0x2400);
    assert_eq!(h.read(&mut s, STATUS, 4), 0x3);
    for _ in 0..9 {
        h.read(&mut s, FIFO, 4);
    }
    assert_eq!(h.read(&mut s, STATUS, 4), 0x1);
    assert_eq!(h.read(&mut s, LEVEL, 4), 0);
}

#[test]
fn huge_read_len_is_lazy() {
    let mut flash = vec![0; NOR_SIZE];
    flash[..4].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
    let mut h = DevHarness::new().with_buffer(FLASH_BUFFER, flash);
    let mut s = Spif::new();
    cmd(&mut h, &mut s, 0x0300_0000, CFG_ADDR3, 0, 0xFFFF_FFFF);
    assert_eq!(h.read(&mut s, LEVEL, 4), 0x4000);
    assert_eq!(h.read(&mut s, FIFO, 4), 0x4433_2211);
    assert_eq!(s.nor().remaining(), 0xFFFF_FFFF - 4);
}

#[test]
fn halfword_pop_and_short_word_pop() {
    let mut flash = vec![0; NOR_SIZE];
    flash[0x10..0x13].copy_from_slice(&[0xAA, 0xBB, 0xCC]);
    let mut h = DevHarness::new().with_buffer(FLASH_BUFFER, flash);
    let mut s = Spif::new();
    cmd(&mut h, &mut s, 0x0300_0000, CFG_ADDR3, 0x10, 3);
    assert_eq!(h.read(&mut s, FIFO, 2), 0xBBAA);
    // One byte left: the missing bytes of a word pop read 0.
    assert_eq!(h.read(&mut s, FIFO, 4), 0xCC);
}

#[test]
fn address_is_24_bit_and_needs_address_bytes() {
    let mut flash = vec![0; NOR_SIZE];
    flash[0x12_3456] = 0x5A;
    flash[0] = 0xA5;
    let mut h = DevHarness::new().with_buffer(FLASH_BUFFER, flash);
    let mut s = Spif::new();
    cmd(&mut h, &mut s, 0x0300_0000, CFG_ADDR3, 0xFF12_3456, 1);
    assert_eq!(h.read(&mut s, FIFO, 1), 0x5A);
    cmd(&mut h, &mut s, 0x0300_0000, CFG_NOADDR, 0x12_3456, 1);
    assert_eq!(h.read(&mut s, FIFO, 1), 0xA5);
}

#[test]
fn secreg_program_via_fifo() {
    let (mut h, mut s) = setup();
    cmd(&mut h, &mut s, WREN, CFG_NOADDR, 0, 0);
    cmd(&mut h, &mut s, 0x4200_0002, CFG_ADDR3, 0x101C, 4);
    assert_eq!(h.read(&mut s, INT, 4), 0);
    h.write(&mut s, FIFO, 4, 0xC0DE_0042);
    assert_eq!(h.read(&mut s, INT, 4), 1);
    assert_eq!(s.nor().secregs()[28..32], [0x42, 0x00, 0xDE, 0xC0]);
    // Read it back with 0x48 (8 dummy cycles).
    cmd(&mut h, &mut s, 0x4800_0000, 0x0108_0003, 0x101C, 4);
    assert_eq!(h.read(&mut s, FIFO, 4), 0xC0DE_0042);
}

#[test]
fn byte_pushes_program() {
    let (mut h, mut s) = setup();
    cmd(&mut h, &mut s, WREN, CFG_NOADDR, 0, 0);
    cmd(&mut h, &mut s, 0x2000_0006, CFG_ADDR3, 0, 0);
    cmd(&mut h, &mut s, WREN, CFG_NOADDR, 0, 0);
    cmd(&mut h, &mut s, 0x0200_0002, CFG_ADDR3, 0x20, 2);
    h.write(&mut s, FIFO, 1, 0x12);
    assert_eq!(h.read(&mut s, INT, 4), 0);
    h.write(&mut s, FIFO, 1, 0x34);
    assert_eq!(h.read(&mut s, INT, 4), 1);
    // Bytes past the end of the phase are ignored.
    h.write(&mut s, FIFO, 1, 0x00);
    assert_eq!(h.buffer(FLASH_BUFFER)[0x20..0x23], [0x12, 0x34, 0xFF]);
}

#[test]
fn xip_ctrl_bit10_self_clears() {
    let (mut h, mut s) = setup();
    h.write(&mut s, XIP_CTRL, 4, 0x400);
    assert_eq!(h.read(&mut s, XIP_CTRL, 4), 0);
    h.write(&mut s, XIP_CTRL, 4, 0x401);
    assert_eq!(h.read(&mut s, XIP_CTRL, 4), 1);
    h.write(&mut s, XIP_CFG, 4, 0x100E_B404);
    assert_eq!(h.read(&mut s, XIP_CFG, 4), 0x100E_B404);
    h.write(&mut s, CFG0, 4, 0x13);
    assert_eq!(h.read(&mut s, CFG0, 4), 0x13);
    h.write(&mut s, ADDR, 4, 0x1234);
    assert_eq!(h.read(&mut s, ADDR, 4), 0x1234);
    assert!(h.log.is_empty());
}

#[test]
fn unmodeled_registers_read_zero_and_log() {
    let (mut h, mut s) = setup();
    assert_eq!(h.read(&mut s, 0x14, 4), 0);
    h.write(&mut s, 0x58, 4, 7);
    assert_eq!(h.read(&mut s, 0x58, 4), 0);
    h.write(&mut s, STATUS, 4, 0xFF);
    assert_eq!(h.read(&mut s, STATUS, 4), 1);
    assert_eq!(h.read(&mut s, 0x104, 4), 0);
    assert_eq!(h.read(&mut s, 0xFFC, 4), 0);
    assert_eq!(h.read(&mut s, CTRL, 1), 0);
    assert_eq!(h.log.len(), 7);
}

#[test]
fn missing_flash_buffer_reads_erased() {
    let mut h = DevHarness::new();
    let mut s = Spif::new();
    cmd(&mut h, &mut s, WREN, CFG_NOADDR, 0, 0);
    cmd(&mut h, &mut s, 0x0200_0002, CFG_ADDR3, 0, 4);
    h.write(&mut s, FIFO, 4, 0);
    assert_eq!(h.read(&mut s, INT, 4), 1);
    cmd(&mut h, &mut s, 0x0300_0000, CFG_ADDR3, 0, 4);
    assert_eq!(h.read(&mut s, FIFO, 4), 0xFFFF_FFFF);
    cmd(&mut h, &mut s, WREN, CFG_NOADDR, 0, 0);
    cmd(&mut h, &mut s, 0xC700_0006, CFG_NOADDR, 0, 0);
    assert_eq!(h.read(&mut s, INT, 4), 1);
}

#[test]
fn system_reset_keeps_flash_and_secregs() {
    let (mut h, mut s) = setup();
    cmd(&mut h, &mut s, WREN, CFG_NOADDR, 0, 0);
    cmd(&mut h, &mut s, 0x0200_0002, CFG_ADDR3, 0x100, 4);
    h.write(&mut s, FIFO, 4, 0x0000_0001);
    cmd(&mut h, &mut s, WREN, CFG_NOADDR, 0, 0);
    cmd(&mut h, &mut s, 0x4200_0002, CFG_ADDR3, 0x101C, 4);
    h.write(&mut s, FIFO, 4, 0xC0DE_0042);
    s.nor_mut().sr2 = 0x03;
    cmd(&mut h, &mut s, WREN, CFG_NOADDR, 0, 0);
    // Leave a read phase open, too.
    cmd(&mut h, &mut s, 0x0300_0000, CFG_ADDR3, 0x100, 4);
    let flash = h.buffer(FLASH_BUFFER).to_vec();
    let secregs = s.nor().secregs().to_vec();

    h.reset(&mut s, ResetKind::System);
    assert_eq!(h.buffer(FLASH_BUFFER), &flash[..]);
    assert_eq!(s.nor().secregs(), &secregs[..]);
    assert_eq!(s.nor().secregs()[28..32], [0x42, 0x00, 0xDE, 0xC0]);
    assert_eq!(s.nor().sr2, 0x03);
    assert_eq!(s.nor().remaining(), 0);
    assert_eq!(h.read(&mut s, INT, 4), 0);
    assert_eq!(h.read(&mut s, CTRL, 4), 0x1100);
    assert_eq!(h.read(&mut s, STATUS, 4), 1);
    assert_eq!(h.read(&mut s, LEVEL, 4), 0);
    assert_eq!(rdsr(&mut h, &mut s), 0x00, "WEL cleared");
}

#[test]
fn power_on_reset_keeps_secregs() {
    let (mut h, mut s) = setup();
    s.nor_mut().set_secregs(&[0x12; SECREGS_LEN]);
    s.nor_mut().sr2 = 0x03;
    h.write(&mut s, XIP_CFG, 4, 0x100E_B404);
    h.reset(&mut s, ResetKind::PowerOn);
    assert_eq!(s.nor().secregs(), &[0x12; SECREGS_LEN][..]);
    assert_eq!(s.nor().sr2, 0x02);
    assert_eq!(h.read(&mut s, XIP_CFG, 4), 0);
    assert_eq!(h.read(&mut s, CTRL, 4), 0);
}

#[test]
fn state_roundtrip() {
    let mut flash = vec![0; NOR_SIZE];
    for (i, b) in flash[..16].iter_mut().enumerate() {
        *b = i as u8;
    }
    let mut h = DevHarness::new().with_buffer(FLASH_BUFFER, flash);
    let mut a = Spif::new();
    h.write(&mut a, XIP_CFG, 4, 0x100E_B404);
    cmd(&mut h, &mut a, 0x0300_0000, CFG_ADDR3, 0, 10);
    assert_eq!(pop_bytes(&mut h, &mut a, 2), [0, 1]);
    let state = a.save();

    // A fresh device resumes the read phase.
    let mut b = Spif::new();
    b.load(&state).unwrap();
    assert_eq!(h.read(&mut b, XIP_CFG, 4), 0x100E_B404);
    assert_eq!(h.read(&mut b, INT, 4), 1);
    assert_eq!(h.read(&mut b, LEVEL, 4), 0x800);
    assert_eq!(h.read(&mut b, STATUS, 4), 3);
    assert_eq!(pop_bytes(&mut h, &mut b, 2), [2, 3]);
    assert_eq!(b.nor().secregs(), &[0xFF; SECREGS_LEN][..]);

    // Security registers are not part of the state: load keeps the current ones.
    a.nor_mut().set_secregs(&[0x12; SECREGS_LEN]);
    a.load(&state).unwrap();
    assert_eq!(a.nor().secregs(), &[0x12; SECREGS_LEN][..]);
    assert_eq!(pop_bytes(&mut h, &mut a, 1), [2]);

    // Garbage is refused and leaves the device unchanged.
    assert!(a.load(&[0xFF; 3]).is_err());
    assert_eq!(a.nor().remaining(), 7);
    assert_eq!(a.nor().secregs(), &[0x12; SECREGS_LEN][..]);
}

#[test]
fn soc_attaches_spif() {
    let mut map = MemoryMap::new();
    evo_hw::soc::attach(&mut map);
    assert!(map.device::<Spif>("spif").is_some());
}

/// Code the CPU cached from flash (`Bus::cache_code`) stays valid across SPIF commands that
/// only read the part: just erases, programs and status-register writes may change bytes, and
/// only those change the code epoch.
#[test]
fn only_commands_that_change_flash_end_cached_code() {
    use armv8m::{Access, AccessKind, Bus};

    const SPIF: u32 = 0x5000_1000;
    let acc = |kind| Access {
        kind,
        secure: true,
        privileged: true,
    };
    let mut map = MemoryMap::new();
    map.add_rom(FLASH_BUFFER, vec![0xFF; NOR_SIZE], &[0x1000_0000]);
    evo_hw::soc::attach(&mut map);
    assert!(map.cache_code(0x1000_0000, 2));
    let run = |map: &mut MemoryMap, op: u32, cfg: u32, addr: u32, len: u32| {
        for (off, v) in [(INT, 1), (ADDR, addr), (CFG, cfg), (LEN, len), (OP, op)] {
            map.write32(SPIF + off, v, acc(AccessKind::Write)).unwrap();
        }
        map.write32(SPIF + CTRL, 0x1102, acc(AccessKind::Write))
            .unwrap();
        map.code_epoch()
    };
    let e0 = map.code_epoch();
    assert_eq!(run(&mut map, 0x0300_0000, CFG_ADDR3, 0, 4), e0, "READ");
    for _ in 0..4 {
        map.read8(SPIF + FIFO, acc(AccessKind::Read)).unwrap();
    }
    assert_eq!(map.code_epoch(), e0, "popping read data");
    assert_eq!(run(&mut map, 0x9F00_0000, CFG_NOADDR, 0, 3), e0, "JEDEC ID");
    assert_eq!(run(&mut map, RDSR, CFG_NOADDR, 0, 1), e0, "RDSR");
    assert_eq!(run(&mut map, WREN, CFG_NOADDR, 0, 0), e0, "WREN");
    let e1 = run(&mut map, 0x2000_0006, CFG_ADDR3, 0, 0);
    assert_ne!(e1, e0, "sector erase with WEL");
    assert_eq!(
        run(&mut map, 0x2000_0006, CFG_ADDR3, 0, 0),
        e1,
        "erase without WEL"
    );
    run(&mut map, WREN, CFG_NOADDR, 0, 0);
    let e2 = run(&mut map, 0x0200_0002, CFG_ADDR3, 0, 4);
    assert_eq!(e2, e1, "a program command before its data");
    map.write32(SPIF + FIFO, 0x1234_5678, acc(AccessKind::Write))
        .unwrap();
    assert_ne!(map.code_epoch(), e2, "program data");
    assert_eq!(
        map.read32(0x1000_0000, acc(AccessKind::Read)),
        Ok(0x1234_5678)
    );
}

/// Erases and programs away from cached code keep it: the SPIF declares only the bytes an
/// erase block or a programmed byte covers. A chip erase covers everything.
#[test]
fn flash_changes_away_from_cached_code_keep_it() {
    use armv8m::{Access, AccessKind, Bus};

    const SPIF: u32 = 0x5000_1000;
    let acc = |kind| Access {
        kind,
        secure: true,
        privileged: true,
    };
    let mut map = MemoryMap::new();
    map.add_rom(FLASH_BUFFER, vec![0xFF; NOR_SIZE], &[0x1000_0000]);
    evo_hw::soc::attach(&mut map);
    assert!(map.cache_code(0x1001_0000, 2));
    let run = |map: &mut MemoryMap, op: u32, cfg: u32, addr: u32, len: u32| {
        for (off, v) in [(INT, 1), (ADDR, addr), (CFG, cfg), (LEN, len), (OP, op)] {
            map.write32(SPIF + off, v, acc(AccessKind::Write)).unwrap();
        }
        map.write32(SPIF + CTRL, 0x1102, acc(AccessKind::Write))
            .unwrap();
        map.code_epoch()
    };
    let e0 = map.code_epoch();
    run(&mut map, WREN, CFG_NOADDR, 0, 0);
    assert_eq!(
        run(&mut map, 0x2000_0006, CFG_ADDR3, 0x1_1000, 0),
        e0,
        "4K erase next door"
    );
    run(&mut map, WREN, CFG_NOADDR, 0, 0);
    run(&mut map, 0x0200_0002, CFG_ADDR3, 0x0_FFFC, 4);
    map.write32(SPIF + FIFO, 0x1234_5678, acc(AccessKind::Write))
        .unwrap();
    assert_eq!(map.code_epoch(), e0, "program right below the code page");
    assert_eq!(
        map.read32(0x1000_FFFC, acc(AccessKind::Read)),
        Ok(0x1234_5678)
    );
    run(&mut map, WREN, CFG_NOADDR, 0, 0);
    let e1 = run(&mut map, 0x5200_0006, CFG_ADDR3, 0x0_8000, 0);
    assert_eq!(e1, e0, "32K erase of the block below");
    assert_eq!(map.read32(0x1000_FFFC, acc(AccessKind::Read)), Ok(u32::MAX));
    run(&mut map, WREN, CFG_NOADDR, 0, 0);
    assert_eq!(
        run(&mut map, 0xD800_0006, CFG_ADDR3, 0x0_8000, 0),
        e0,
        "64K erase below"
    );
    run(&mut map, WREN, CFG_NOADDR, 0, 0);
    assert_ne!(
        run(&mut map, 0xD800_0006, CFG_ADDR3, 0x1_8000, 0),
        e0,
        "64K erase of it"
    );
    let e2 = map.code_epoch();
    run(&mut map, WREN, CFG_NOADDR, 0, 0);
    assert_ne!(
        run(&mut map, 0xC700_0002, CFG_NOADDR, 0, 0),
        e2,
        "chip erase"
    );
}
