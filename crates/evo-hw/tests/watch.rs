//! Data watchpoints and the bus monitor on [`MemoryMap`] (Plan 4, Task A1).

use armv8m::{Access, AccessKind, Bus};
use evo_hw::dev::storage::Storage;
use evo_hw::{
    AccessDir, BUS_RING_CAP, BusRecord, MemoryMap, WatchError, WatchHit, WatchKinds, WatchRange,
};

const RAM: u32 = 0x2000_0000;
const DEV: u32 = 0x4000_0000;
const UNMAPPED: u32 = 0x5000_0000;

fn acc(kind: AccessKind) -> Access {
    Access {
        kind,
        secure: true,
        privileged: true,
    }
}

fn rd() -> Access {
    acc(AccessKind::Read)
}

fn wr() -> Access {
    acc(AccessKind::Write)
}

/// 64 KiB RAM at `RAM` (one whole page: the fast path) and a `Storage` device at `DEV`.
fn map() -> MemoryMap {
    let mut m = MemoryMap::new();
    m.add_ram("sram", 0x1_0000, &[RAM]);
    m.add_device("store", DEV, 0x1000, Box::new(Storage::new(0x1000)));
    m
}

const R: WatchKinds = WatchKinds {
    read: true,
    write: false,
    exec: false,
};
const W: WatchKinds = WatchKinds {
    read: false,
    write: true,
    exec: false,
};
const X: WatchKinds = WatchKinds {
    read: false,
    write: false,
    exec: true,
};

fn range(start: u32, len: u32, kinds: WatchKinds) -> WatchRange {
    WatchRange { start, len, kinds }
}

#[test]
fn write_watch_hits_with_value() {
    let mut m = map();
    m.set_watchpoints(vec![range(RAM + 0x10, 4, W)]).unwrap();
    m.write32(RAM + 0x10, 0xDEAD_BEEF, wr()).unwrap();
    assert_eq!(
        m.take_watch_hit(),
        Some(WatchHit {
            addr: RAM + 0x10,
            width: 4,
            dir: AccessDir::Write,
            value: 0xDEAD_BEEF,
        })
    );
    assert_eq!(m.take_watch_hit(), None);
    // The write itself happened.
    assert_eq!(m.peek(RAM + 0x10, 4), Some(0xDEAD_BEEF));
    // A read does not match a write-only range.
    m.read32(RAM + 0x10, rd()).unwrap();
    assert_eq!(m.take_watch_hit(), None);
}

#[test]
fn read_watch_sees_read_value() {
    let mut m = map();
    assert!(m.poke(RAM + 0x20, 2, 0x1234));
    m.set_watchpoints(vec![range(RAM + 0x20, 2, R)]).unwrap();
    assert_eq!(m.read16(RAM + 0x20, rd()), Ok(0x1234));
    assert_eq!(
        m.take_watch_hit(),
        Some(WatchHit {
            addr: RAM + 0x20,
            width: 2,
            dir: AccessDir::Read,
            value: 0x1234,
        })
    );
    // A write does not match a read-only range.
    m.write16(RAM + 0x20, 7, wr()).unwrap();
    assert_eq!(m.take_watch_hit(), None);
}

#[test]
fn partial_overlap_hits() {
    let mut m = map();
    m.set_watchpoints(vec![range(RAM + 0x12, 1, W)]).unwrap();
    m.write32(RAM + 0x10, 0x0102_0304, wr()).unwrap();
    assert_eq!(
        m.take_watch_hit(),
        Some(WatchHit {
            addr: RAM + 0x10,
            width: 4,
            dir: AccessDir::Write,
            value: 0x0102_0304,
        })
    );
    // Adjacent accesses that do not touch byte 0x12 do not hit.
    m.write16(RAM + 0x10, 1, wr()).unwrap();
    m.write8(RAM + 0x13, 1, wr()).unwrap();
    m.write32(RAM + 0x0C, 1, wr()).unwrap();
    assert_eq!(m.take_watch_hit(), None);
}

#[test]
fn fetch_is_not_a_read() {
    let mut m = map();
    m.set_watchpoints(vec![range(RAM, 0x100, R)]).unwrap();
    m.set_bus_monitor(Some((RAM, 0x100)));
    m.read16(RAM + 4, acc(AccessKind::Fetch)).unwrap();
    m.read32(RAM + 8, acc(AccessKind::Fetch)).unwrap();
    assert_eq!(m.take_watch_hit(), None);
    assert_eq!(m.bus_records(), vec![]);
}

#[test]
fn device_and_unmapped_accesses_are_watched() {
    let mut m = map();
    m.set_watchpoints(vec![
        range(
            DEV + 4,
            4,
            WatchKinds {
                read: true,
                write: true,
                exec: false,
            },
        ),
        range(UNMAPPED, 4, R),
    ])
    .unwrap();
    m.write16(DEV + 4, 0xBEEF, wr()).unwrap();
    assert_eq!(
        m.take_watch_hit(),
        Some(WatchHit {
            addr: DEV + 4,
            width: 2,
            dir: AccessDir::Write,
            value: 0xBEEF,
        })
    );
    assert_eq!(m.read32(DEV + 4, rd()), Ok(0xBEEF));
    assert_eq!(
        m.take_watch_hit(),
        Some(WatchHit {
            addr: DEV + 4,
            width: 4,
            dir: AccessDir::Read,
            value: 0xBEEF,
        })
    );
    assert_eq!(m.read8(UNMAPPED + 3, rd()), Ok(0));
    assert_eq!(
        m.take_watch_hit(),
        Some(WatchHit {
            addr: UNMAPPED + 3,
            width: 1,
            dir: AccessDir::Read,
            value: 0,
        })
    );
}

#[test]
fn straddling_access_is_one_hit_with_the_whole_value() {
    let mut m = map();
    // A word at the top of the RAM page runs into unmapped space.
    m.set_watchpoints(vec![range(RAM + 0xFFFE, 1, W)]).unwrap();
    m.write32(RAM + 0xFFFE, 0xAABB_CCDD, wr()).unwrap();
    assert_eq!(
        m.take_watch_hit(),
        Some(WatchHit {
            addr: RAM + 0xFFFE,
            width: 4,
            dir: AccessDir::Write,
            value: 0xAABB_CCDD,
        })
    );
}

#[test]
fn wrapping_access_matches_at_address_zero() {
    let mut m = map();
    m.set_watchpoints(vec![range(0, 1, R)]).unwrap();
    m.read32(0xFFFF_FFFE, rd()).unwrap();
    assert_eq!(
        m.take_watch_hit().map(|h| (h.addr, h.width)),
        Some((0xFFFF_FFFE, 4))
    );
}

#[test]
fn peek_poke_are_not_watched() {
    let mut m = map();
    m.set_watchpoints(vec![range(
        RAM,
        0x100,
        WatchKinds {
            read: true,
            write: true,
            exec: false,
        },
    )])
    .unwrap();
    m.set_bus_monitor(Some((RAM, 0x100)));
    assert!(m.poke(RAM + 8, 4, 5));
    assert_eq!(m.peek(RAM + 8, 4), Some(5));
    assert_eq!(m.take_watch_hit(), None);
    assert_eq!(m.bus_records(), vec![]);
}

#[test]
fn first_hit_is_kept() {
    let mut m = map();
    m.set_watchpoints(vec![range(RAM, 0x100, W)]).unwrap();
    m.write8(RAM + 1, 0x11, wr()).unwrap();
    m.write8(RAM + 2, 0x22, wr()).unwrap();
    assert_eq!(
        m.take_watch_hit(),
        Some(WatchHit {
            addr: RAM + 1,
            width: 1,
            dir: AccessDir::Write,
            value: 0x11,
        })
    );
    assert_eq!(m.take_watch_hit(), None);
    // After a take, the next match is kept again.
    m.write8(RAM + 3, 0x33, wr()).unwrap();
    assert_eq!(m.take_watch_hit().map(|h| h.value), Some(0x33));
}

#[test]
fn invalid_ranges_rejected_unchanged() {
    let mut m = map();
    let good = range(RAM, 4, W);
    m.set_watchpoints(vec![good]).unwrap();
    assert_eq!(range(RAM, 0, W).validate(), Err(WatchError::Empty));
    assert_eq!(
        range(0xFFFF_FFFF, 2, W).validate(),
        Err(WatchError::Overflow)
    );
    assert_eq!(
        range(RAM, 4, WatchKinds::default()).validate(),
        Err(WatchError::NoKind)
    );
    assert_eq!(range(0xFFFF_FFFF, 1, W).validate(), Ok(()));
    assert_eq!(range(0, 0xFFFF_FFFF, X).validate(), Ok(()));

    let other = range(RAM + 0x40, 4, R);
    assert_eq!(
        m.set_watchpoints(vec![other, range(RAM, 0, W)]),
        Err(WatchError::Empty)
    );
    assert_eq!(
        m.set_watchpoints(vec![other, range(0xFFFF_FFFF, 2, W)]),
        Err(WatchError::Overflow)
    );
    assert_eq!(
        m.set_watchpoints(vec![other, range(RAM, 4, WatchKinds::default())]),
        Err(WatchError::NoKind)
    );
    assert_eq!(m.watchpoints(), vec![good]);
    // The old set is still in force.
    m.read32(RAM + 0x40, rd()).unwrap();
    assert_eq!(m.take_watch_hit(), None);
    m.write32(RAM, 1, wr()).unwrap();
    assert_eq!(m.take_watch_hit().map(|h| h.addr), Some(RAM));
}

#[test]
fn empty_set_turns_watching_off() {
    let mut m = map();
    assert!(!m.watching());
    m.set_watchpoints(vec![range(RAM, 4, W)]).unwrap();
    assert!(m.watching());
    m.set_watchpoints(vec![]).unwrap();
    assert!(!m.watching());
    assert_eq!(m.watchpoints(), vec![]);
    m.write32(RAM, 1, wr()).unwrap();
    assert_eq!(m.take_watch_hit(), None);

    // Exec ranges are stored but are not checked here, so they do not turn watching on.
    let exec = range(RAM, 2, X);
    m.set_watchpoints(vec![exec]).unwrap();
    assert!(!m.watching());
    assert_eq!(m.watchpoints(), vec![exec]);

    // The monitor alone turns watching on; length 0 is off.
    m.set_bus_monitor(Some((RAM, 0x10)));
    assert!(m.watching());
    m.set_bus_monitor(Some((RAM, 0)));
    assert!(!m.watching());
    m.set_bus_monitor(Some((RAM, 0x10)));
    m.set_bus_monitor(None);
    assert!(!m.watching());
}

#[test]
fn monitor_records_and_is_bounded() {
    let mut m = map();
    m.set_bus_monitor(Some((RAM, 0x40)));
    let n = BUS_RING_CAP + 3;
    for i in 0..n {
        let i32 = i as u32;
        m.pc_hint = 0x1000_0000 + 2 * i32;
        m.now = 10 * i as u64;
        m.write32(RAM + 4 * (i32 % 16), i32, wr()).unwrap();
        // Outside the monitored range: not recorded.
        m.write32(RAM + 0x40, i32, wr()).unwrap();
    }
    let recs = m.bus_records();
    assert_eq!(recs.len(), BUS_RING_CAP);
    assert_eq!(
        recs[0],
        BusRecord {
            addr: RAM + 4 * 3,
            width: 4,
            dir: AccessDir::Write,
            value: 3,
            pc: 0x1000_0006,
            cycle: 30,
        }
    );
    let last = (n - 1) as u32;
    assert_eq!(
        recs[BUS_RING_CAP - 1],
        BusRecord {
            addr: RAM + 4 * (last % 16),
            width: 4,
            dir: AccessDir::Write,
            value: last,
            pc: 0x1000_0000 + 2 * last,
            cycle: 10 * u64::from(last),
        }
    );

    // Reads (fast path and device) are recorded with the value read.
    m.clear_bus_records();
    m.set_bus_monitor(Some((DEV, 0x10)));
    m.write8(DEV + 1, 0x5A, wr()).unwrap();
    assert_eq!(m.read8(DEV + 1, rd()), Ok(0x5A));
    let dirs: Vec<_> = m
        .bus_records()
        .iter()
        .map(|r| (r.addr, r.dir, r.value))
        .collect();
    assert_eq!(
        dirs,
        vec![
            (DEV + 1, AccessDir::Write, 0x5A),
            (DEV + 1, AccessDir::Read, 0x5A)
        ]
    );
}

#[test]
fn monitor_off_keeps_records() {
    let mut m = map();
    m.set_bus_monitor(Some((RAM, 0x10)));
    m.write32(RAM, 9, wr()).unwrap();
    m.set_bus_monitor(None);
    m.write32(RAM, 10, wr()).unwrap();
    let recs = m.bus_records();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].value, 9);
    // Changing watchpoints does not touch the records either.
    m.set_watchpoints(vec![range(RAM, 4, W)]).unwrap();
    m.set_watchpoints(vec![]).unwrap();
    assert_eq!(m.bus_records().len(), 1);
}

#[test]
fn clear_bus_records_empties() {
    let mut m = map();
    m.set_bus_monitor(Some((RAM, 0x10)));
    m.write32(RAM, 1, wr()).unwrap();
    m.read32(RAM, rd()).unwrap();
    assert_eq!(m.bus_records().len(), 2);
    m.clear_bus_records();
    assert_eq!(m.bus_records(), vec![]);
    // Still monitoring.
    m.write8(RAM + 2, 1, wr()).unwrap();
    assert_eq!(m.bus_records().len(), 1);
}

#[test]
fn watch_types_round_trip_through_serde() {
    let r = range(
        RAM,
        8,
        WatchKinds {
            read: true,
            write: false,
            exec: true,
        },
    );
    let json = serde_json::to_string(&r).unwrap();
    assert_eq!(serde_json::from_str::<WatchRange>(&json).unwrap(), r);
    let d: AccessDir =
        serde_json::from_str(&serde_json::to_string(&AccessDir::Exec).unwrap()).unwrap();
    assert_eq!(d, AccessDir::Exec);
}
