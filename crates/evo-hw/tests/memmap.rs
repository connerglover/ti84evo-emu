use std::sync::{Arc, Mutex};

use armv8m::{Access, AccessKind, Bus};
use evo_hw::{Device, MemoryMap, UnmappedAccess};

fn acc(secure: bool) -> Access {
    Access {
        kind: AccessKind::Read,
        secure,
        privileged: true,
    }
}

const S: bool = true;
const NS: bool = false;

fn entries(m: &MemoryMap) -> Vec<(UnmappedAccess, u64)> {
    m.log.entries().map(|(a, c)| (a.clone(), c)).collect()
}

fn assert_send<T: Send>() {}

#[test]
fn memory_map_is_send() {
    assert_send::<MemoryMap>();
}

#[test]
fn ram_alias_shares_backing() {
    let mut m = MemoryMap::new();
    m.add_ram("sram", 0x1000, &[0x2000_0000, 0x3000_0000]);
    m.write32(0x3000_0010, 0xDEAD_BEEF, acc(S)).unwrap();
    assert_eq!(m.read32(0x2000_0010, acc(S)), Ok(0xDEAD_BEEF));
    // Little endian, and narrower accesses see the same bytes.
    assert_eq!(m.read8(0x2000_0010, acc(S)), Ok(0xEF));
    assert_eq!(m.read16(0x2000_0012, acc(S)), Ok(0xDEAD));
    m.write8(0x2000_0011, 0x00, acc(S)).unwrap();
    assert_eq!(m.read32(0x3000_0010, acc(S)), Ok(0xDEAD_00EF));
    assert_eq!(
        &m.region_bytes("sram").unwrap()[0x10..0x14],
        &[0xEF, 0x00, 0xAD, 0xDE]
    );
    assert_eq!(m.region_bytes("sram").unwrap().len(), 0x1000);
    assert!(m.region_bytes("nope").is_none());
    assert_eq!(entries(&m), vec![], "mapped accesses are not logged");
}

#[test]
fn unmapped_read_returns_zero_and_logs() {
    let mut m = MemoryMap::new();
    m.pc_hint = 0x0800_1234;
    assert_eq!(m.read32(0x5000_0000, acc(S)), Ok(0));
    assert_eq!(m.read16(0x5000_0010, acc(NS)), Ok(0));
    assert_eq!(m.read8(0x5000_0020, acc(S)), Ok(0));
    assert_eq!(
        entries(&m),
        vec![
            (
                UnmappedAccess {
                    addr: 0x5000_0000,
                    width: 4,
                    write: None,
                    pc: 0x0800_1234,
                    secure: true
                },
                1
            ),
            (
                UnmappedAccess {
                    addr: 0x5000_0010,
                    width: 2,
                    write: None,
                    pc: 0x0800_1234,
                    secure: false
                },
                1
            ),
            (
                UnmappedAccess {
                    addr: 0x5000_0020,
                    width: 1,
                    write: None,
                    pc: 0x0800_1234,
                    secure: true
                },
                1
            ),
        ]
    );
}

#[test]
fn unmapped_write_is_dropped_and_logged_with_masked_value() {
    let mut m = MemoryMap::new();
    m.pc_hint = 0x100;
    assert_eq!(m.write32(0x6000_0000, 0xCAFE_F00D, acc(S)), Ok(()));
    assert_eq!(m.write16(0x6000_0004, 0xABCD, acc(S)), Ok(()));
    assert_eq!(m.write8(0x6000_0008, 0x7F, acc(NS)), Ok(()));
    let got: Vec<_> = entries(&m).into_iter().map(|(a, _)| a).collect();
    let expect = |addr, width, v, secure| UnmappedAccess {
        addr,
        width,
        write: Some(v),
        pc: 0x100,
        secure,
    };
    assert_eq!(
        got,
        vec![
            expect(0x6000_0000, 4, 0xCAFE_F00D, true),
            expect(0x6000_0004, 2, 0xABCD, true),
            expect(0x6000_0008, 1, 0x7F, false),
        ]
    );
    // Dropped: reading it back yields zero (still unmapped).
    assert_eq!(m.read32(0x6000_0000, acc(S)), Ok(0));
}

#[test]
fn log_dedups_and_counts() {
    let mut m = MemoryMap::new();
    m.pc_hint = 0x10;
    for _ in 0..3 {
        m.read32(0x5000_0000, acc(S)).unwrap();
    }
    // Different pc, width, or direction are distinct entries.
    m.pc_hint = 0x20;
    m.read32(0x5000_0000, acc(S)).unwrap();
    m.read16(0x5000_0000, acc(S)).unwrap();
    m.write32(0x5000_0000, 1, acc(S)).unwrap();
    // Same key, different written value and security: still one entry; first occurrence wins.
    m.write32(0x5000_0000, 2, acc(NS)).unwrap();

    let e = entries(&m);
    assert_eq!(e.len(), 4);
    assert_eq!(
        (e[0].0.pc, e[0].0.width, e[0].0.write, e[0].1),
        (0x10, 4, None, 3)
    );
    assert_eq!(
        (e[1].0.pc, e[1].0.width, e[1].0.write, e[1].1),
        (0x20, 4, None, 1)
    );
    assert_eq!(
        (e[2].0.pc, e[2].0.width, e[2].0.write, e[2].1),
        (0x20, 2, None, 1)
    );
    assert_eq!(
        (e[3].0.pc, e[3].0.width, e[3].0.write, e[3].0.secure, e[3].1),
        (0x20, 4, Some(1), true, 2)
    );
}

#[test]
fn rom_write_is_logged_not_applied() {
    let mut m = MemoryMap::new();
    m.add_rom(
        "boot",
        vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
        &[0x0000_0000],
    );
    m.pc_hint = 0x44;
    assert_eq!(m.read32(0, acc(S)), Ok(0x0403_0201));
    assert_eq!(m.log.len(), 0, "ROM reads are not logged");
    assert_eq!(m.write32(0, 0xFFFF_FFFF, acc(S)), Ok(()));
    assert_eq!(m.read32(0, acc(S)), Ok(0x0403_0201), "write not applied");
    assert_eq!(m.region_bytes("boot").unwrap()[0..4], [1, 2, 3, 4]);
    assert_eq!(
        entries(&m),
        vec![(
            UnmappedAccess {
                addr: 0,
                width: 4,
                write: Some(0xFFFF_FFFF),
                pc: 0x44,
                secure: true
            },
            1
        )]
    );
    // Past the end of the ROM is unmapped.
    assert_eq!(m.read32(8, acc(S)), Ok(0));
    assert_eq!(m.log.len(), 2);
}

#[test]
fn straddling_word_read() {
    let mut m = MemoryMap::new();
    m.add_ram("ram", 0x100, &[0x2000_0000]);
    m.write8(0x2000_00FE, 0x11, acc(S)).unwrap();
    m.write8(0x2000_00FF, 0x22, acc(S)).unwrap();
    m.pc_hint = 0x77;
    // Two bytes from RAM, two beyond its end: no panic, no garbage, per-byte log.
    assert_eq!(m.read32(0x2000_00FE, acc(S)), Ok(0x0000_2211));
    let got: Vec<_> = entries(&m)
        .into_iter()
        .map(|(a, c)| (a.addr, a.width, a.write, a.pc, c))
        .collect();
    assert_eq!(
        got,
        vec![
            (0x2000_0100, 1, None, 0x77, 1),
            (0x2000_0101, 1, None, 0x77, 1)
        ]
    );
}

#[test]
fn straddling_word_read_into_region_start() {
    let mut m = MemoryMap::new();
    m.add_ram("ram", 0x100, &[0x2000_0000]);
    m.write8(0x2000_0000, 0xAA, acc(S)).unwrap();
    m.write8(0x2000_0001, 0xBB, acc(S)).unwrap();
    assert_eq!(m.read32(0x1FFF_FFFE, acc(S)), Ok(0xBBAA_0000));
    let addrs: Vec<_> = entries(&m).iter().map(|(a, _)| (a.addr, a.width)).collect();
    assert_eq!(addrs, vec![(0x1FFF_FFFE, 1), (0x1FFF_FFFF, 1)]);
}

#[test]
fn straddling_halfword_and_word_write() {
    let mut m = MemoryMap::new();
    m.add_ram("ram", 0x100, &[0x2000_0000]);
    m.pc_hint = 5;
    m.write32(0x2000_00FE, 0xAABB_CCDD, acc(S)).unwrap();
    assert_eq!(
        m.read16(0x2000_00FE, acc(S)),
        Ok(0xCCDD),
        "in-region half is applied"
    );
    let got: Vec<_> = entries(&m)
        .iter()
        .map(|(a, _)| (a.addr, a.width, a.write))
        .collect();
    assert_eq!(
        got,
        vec![(0x2000_0100, 1, Some(0xBB)), (0x2000_0101, 1, Some(0xAA))]
    );
    // A halfword whose second byte falls off the end: first byte lands, second is logged
    // (same key as the 0x2000_0100 byte write above, so it bumps that entry's count).
    m.write16(0x2000_00FF, 0x1234, acc(S)).unwrap();
    assert_eq!(m.read8(0x2000_00FF, acc(S)), Ok(0x34));
    assert_eq!(
        m.read8(0x2000_00FE, acc(S)),
        Ok(0xDD),
        "byte before is untouched"
    );
    assert_eq!(m.log.len(), 2);
    assert_eq!(entries(&m)[0].1, 2);
}

#[test]
fn access_across_adjacent_regions_is_not_logged() {
    let mut m = MemoryMap::new();
    m.add_ram("a", 0x100, &[0x2000_0000]);
    m.add_ram("b", 0x100, &[0x2000_0100]);
    m.write32(0x2000_00FE, 0x4433_2211, acc(S)).unwrap();
    assert_eq!(m.region_bytes("a").unwrap()[0xFE..], [0x11, 0x22]);
    assert_eq!(m.region_bytes("b").unwrap()[..2], [0x33, 0x44]);
    assert_eq!(m.read32(0x2000_00FE, acc(S)), Ok(0x4433_2211));
    assert_eq!(m.log.len(), 0);
}

#[test]
fn access_wrapping_past_top_of_address_space() {
    let mut m = MemoryMap::new();
    m.add_ram("low", 0x10, &[0x0000_0000]);
    m.write8(0, 0x55, acc(S)).unwrap();
    m.write8(1, 0x66, acc(S)).unwrap();
    assert_eq!(m.read32(0xFFFF_FFFE, acc(S)), Ok(0x6655_0000));
    let addrs: Vec<_> = entries(&m).iter().map(|(a, _)| a.addr).collect();
    assert_eq!(addrs, vec![0xFFFF_FFFE, 0xFFFF_FFFF]);
}

type Seen = Arc<Mutex<Vec<(&'static str, u32, u8, u32)>>>;

/// Records every access; reads return the offset so dispatch is observable.
struct Probe {
    seen: Seen,
}

impl Device for Probe {
    fn read(&mut self, off: u32, width: u8) -> u32 {
        self.seen.lock().unwrap().push(("r", off, width, 0));
        // Deliberately returns junk above the access width: the map must mask.
        0xA5A5_0000 | off
    }
    fn write(&mut self, off: u32, width: u8, v: u32) {
        self.seen.lock().unwrap().push(("w", off, width, v));
    }
}

#[test]
fn device_dispatch_offsets() {
    let seen = Seen::default();
    let mut m = MemoryMap::new();
    m.add_device(
        "probe",
        0x4000_0000,
        0x100,
        Box::new(Probe { seen: seen.clone() }),
    );

    assert_eq!(m.read32(0x4000_0010, acc(S)), Ok(0xA5A5_0010));
    assert_eq!(m.read16(0x4000_0022, acc(S)), Ok(0x0022), "masked to width");
    assert_eq!(m.read8(0x4000_0031, acc(S)), Ok(0x31), "masked to width");
    m.write32(0x4000_0040, 0x1234_5678, acc(S)).unwrap();
    m.write16(0x4000_0042, 0xBEEF, acc(S)).unwrap();
    m.write8(0x4000_0043, 0x9A, acc(S)).unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            ("r", 0x10, 4, 0),
            ("r", 0x22, 2, 0),
            ("r", 0x31, 1, 0),
            ("w", 0x40, 4, 0x1234_5678),
            ("w", 0x42, 2, 0xBEEF),
            ("w", 0x43, 1, 0x9A),
        ]
    );
    assert_eq!(m.log.len(), 0, "device accesses are not logged");

    // One past the end of the device is unmapped.
    assert_eq!(m.read32(0x4000_0100, acc(S)), Ok(0));
    assert_eq!(m.log.len(), 1);
}

#[test]
fn device_access_straddling_its_end_splits_into_bytes() {
    let seen = Seen::default();
    let mut m = MemoryMap::new();
    m.add_device(
        "probe",
        0x4000_0000,
        0x100,
        Box::new(Probe { seen: seen.clone() }),
    );
    // Bytes 0xFE and 0xFF come from the device (junk masked off), the rest is unmapped.
    assert_eq!(m.read32(0x4000_00FE, acc(S)), Ok(0x0000_FFFE));
    assert_eq!(
        *seen.lock().unwrap(),
        vec![("r", 0xFE, 1, 0), ("r", 0xFF, 1, 0)],
        "the device sees byte reads, never a word that runs off its end"
    );
    let addrs: Vec<_> = entries(&m).iter().map(|(a, _)| (a.addr, a.width)).collect();
    assert_eq!(addrs, vec![(0x4000_0100, 1), (0x4000_0101, 1)]);
}

#[test]
fn multiple_devices_and_regions_dispatch_by_address() {
    let s1 = Seen::default();
    let s2 = Seen::default();
    let mut m = MemoryMap::new();
    // Added out of address order on purpose.
    m.add_device(
        "hi",
        0x5000_0000,
        0x10,
        Box::new(Probe { seen: s2.clone() }),
    );
    m.add_ram("ram", 0x100, &[0x2000_0000]);
    m.add_device(
        "lo",
        0x4000_0000,
        0x10,
        Box::new(Probe { seen: s1.clone() }),
    );
    m.read8(0x5000_0004, acc(S)).unwrap();
    m.read8(0x4000_0008, acc(S)).unwrap();
    assert_eq!(*s1.lock().unwrap(), vec![("r", 8, 1, 0)]);
    assert_eq!(*s2.lock().unwrap(), vec![("r", 4, 1, 0)]);
}

#[test]
fn to_markdown_table_format() {
    let mut m = MemoryMap::new();
    m.pc_hint = 0x0800_1234;
    for _ in 0..3 {
        m.read32(0x5000_0000, acc(S)).unwrap();
    }
    m.pc_hint = 0x0800_1238;
    m.write16(0x5000_0004, 0xAB, acc(NS)).unwrap();
    let expected = "\
| addr | width | R/W | value | pc | sec | count |
|---|---|---|---|---|---|---|
| 0x50000000 | 4 | R | - | 0x08001234 | S | 3 |
| 0x50000004 | 2 | W | 0x00ab | 0x08001238 | NS | 1 |
";
    assert_eq!(m.log.to_markdown(), expected);
}

#[test]
fn to_markdown_empty_log_is_header_only() {
    let m = MemoryMap::new();
    assert_eq!(
        m.log.to_markdown(),
        "| addr | width | R/W | value | pc | sec | count |\n|---|---|---|---|---|---|---|\n"
    );
}

// ---- debugger API (peek/poke): no logging, no device side effects ----

#[test]
fn peek_poke_ram_and_rom_do_not_log() {
    let mut m = MemoryMap::new();
    m.add_ram("ram", 0x100, &[0x2000_0000, 0x3000_0000]);
    m.add_rom("rom", vec![9, 8, 7, 6], &[0x0800_0000]);
    assert!(m.poke(0x3000_0010, 4, 0x1122_3344));
    assert_eq!(m.peek(0x2000_0010, 4), Some(0x1122_3344));
    assert_eq!(m.peek(0x2000_0011, 2), Some(0x2233));
    assert_eq!(m.peek(0x2000_0013, 1), Some(0x11));
    // The debugger may patch ROM.
    assert!(m.poke(0x0800_0000, 2, 0xBEEF));
    assert_eq!(m.peek(0x0800_0000, 4), Some(0x0607_BEEF));
    assert_eq!(m.region_bytes("rom").unwrap(), [0xEF, 0xBE, 7, 6]);
    assert_eq!(m.log.len(), 0);
}

#[test]
fn peek_poke_fail_without_side_effects_on_unmapped_or_device() {
    let seen = Seen::default();
    let mut m = MemoryMap::new();
    m.add_ram("ram", 0x100, &[0x2000_0000]);
    m.add_device(
        "probe",
        0x4000_0000,
        0x100,
        Box::new(Probe { seen: seen.clone() }),
    );
    assert_eq!(m.peek(0x5000_0000, 4), None);
    assert!(!m.poke(0x5000_0000, 4, 1));
    assert_eq!(
        m.peek(0x4000_0000, 4),
        None,
        "device reads can have side effects"
    );
    assert!(!m.poke(0x4000_0000, 4, 1));
    assert!(seen.lock().unwrap().is_empty(), "device never touched");
    assert_eq!(m.log.len(), 0, "no unmapped logging from the debugger");
    // Bad widths are refused, not panics.
    assert_eq!(m.peek(0x2000_0000, 3), None);
    assert_eq!(m.peek(0x2000_0000, 0), None);
    assert!(!m.poke(0x2000_0000, 8, 0));
}

#[test]
fn peek_poke_straddling() {
    let mut m = MemoryMap::new();
    m.add_ram("a", 0x100, &[0x2000_0000]);
    // Straddling into unmapped space: refused, and a failed poke writes nothing.
    assert_eq!(m.peek(0x2000_00FE, 4), None);
    assert!(!m.poke(0x2000_00FE, 4, 0xFFFF_FFFF));
    assert_eq!(m.peek(0x2000_00FE, 2), Some(0), "poke was all-or-nothing");
    // Straddling two adjacent RAM regions works.
    m.add_ram("b", 0x100, &[0x2000_0100]);
    assert!(m.poke(0x2000_00FE, 4, 0x4433_2211));
    assert_eq!(m.peek(0x2000_00FE, 4), Some(0x4433_2211));
    assert_eq!(m.log.len(), 0);
}

// ---- configuration ----

#[test]
#[should_panic(expected = "overlap")]
fn overlapping_regions_are_rejected() {
    let mut m = MemoryMap::new();
    m.add_ram("a", 0x1000, &[0x2000_0000]);
    m.add_ram("b", 0x1000, &[0x2000_0800]);
}

#[test]
#[should_panic(expected = "overlap")]
fn aliases_of_one_region_must_not_overlap_each_other() {
    let mut m = MemoryMap::new();
    m.add_ram("a", 0x1000, &[0x2000_0000, 0x2000_0FFF]);
}

#[test]
fn adjacent_regions_are_fine_and_region_may_end_at_top_of_memory() {
    let mut m = MemoryMap::new();
    m.add_ram("a", 0x1000, &[0x2000_0000]);
    m.add_ram("b", 0x1000, &[0x2000_1000]);
    m.add_ram("top", 0x100, &[0xFFFF_FF00]);
    m.write32(0xFFFF_FFFC, 0x0102_0304, acc(S)).unwrap();
    assert_eq!(m.read32(0xFFFF_FFFC, acc(S)), Ok(0x0102_0304));
    assert_eq!(m.log.len(), 0);
}

// ---- save-state serialization ----

#[test]
fn serde_roundtrip_keeps_memory_log_and_dedup() {
    let mut m = MemoryMap::new();
    m.add_ram("ram", 0x40, &[0x2000_0000, 0x3000_0000]);
    m.add_rom("rom", vec![1, 2, 3, 4], &[0x0800_0000]);
    m.add_device(
        "probe",
        0x4000_0000,
        0x10,
        Box::new(Probe {
            seen: Seen::default(),
        }),
    );
    m.pc_hint = 0x99;
    m.write32(0x2000_0000, 0xA1B2_C3D4, acc(S)).unwrap();
    m.read32(0x5000_0000, acc(S)).unwrap();

    let json = serde_json::to_string(&m).unwrap();
    let mut back: MemoryMap = serde_json::from_str(&json).unwrap();

    assert_eq!(back.pc_hint, 0x99);
    assert_eq!(
        back.read32(0x3000_0000, acc(S)),
        Ok(0xA1B2_C3D4),
        "alias still shares backing"
    );
    assert_eq!(back.read32(0x0800_0000, acc(S)), Ok(0x0403_0201));
    assert_eq!(back.region_bytes("ram"), m.region_bytes("ram"));
    assert_eq!(entries(&back), entries(&m));
    // The dedup index is rebuilt: repeating the logged access bumps the count.
    back.read32(0x5000_0000, acc(S)).unwrap();
    assert_eq!(entries(&back)[0].1, 2);
    assert_eq!(back.log.len(), 1);
}

#[test]
fn deserialized_map_without_devices_logs_instead_of_panicking() {
    // Devices carry their own state in Plan 2 and are re-attached by the Machine; until
    // then a restored device window behaves as unmapped.
    let mut m = MemoryMap::new();
    m.add_device(
        "probe",
        0x4000_0000,
        0x10,
        Box::new(Probe {
            seen: Seen::default(),
        }),
    );
    let json = serde_json::to_string(&m).unwrap();
    let mut back: MemoryMap = serde_json::from_str(&json).unwrap();
    assert_eq!(back.read32(0x4000_0000, acc(S)), Ok(0));
    back.write32(0x4000_0004, 7, acc(S)).unwrap();
    assert_eq!(back.log.len(), 2);
    assert_eq!(back.peek(0x4000_0000, 4), None);
}
