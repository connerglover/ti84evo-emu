use evo_hw::unmapped::MAX_ENTRIES;
use evo_hw::{UnmappedAccess, UnmappedLog};

fn rd(addr: u32, pc: u32) -> UnmappedAccess {
    UnmappedAccess {
        addr,
        width: 4,
        write: None,
        pc,
        secure: true,
    }
}

#[test]
fn log_cap_bounds_memory_and_counts_dropped() {
    let mut log = UnmappedLog::default();
    let cap = MAX_ENTRIES as u32;
    assert_eq!(cap, 65_536);
    // A runaway guest touching a fresh address every time.
    for i in 0..cap + 10 {
        log.record(rd(i * 4, 0));
    }
    assert_eq!(log.len(), MAX_ENTRIES);
    assert_eq!(log.dropped(), 10);
    assert_eq!(log.entries().count(), MAX_ENTRIES);

    // Already-recorded keys keep counting exactly, even when full.
    log.record(rd(0, 0));
    log.record(rd(0, 0));
    assert_eq!(log.entries().next().map(|(_, c)| c), Some(3));
    assert_eq!(log.len(), MAX_ENTRIES);
    assert_eq!(log.dropped(), 10);

    // A new key once full is dropped (and counted as dropped each time).
    log.record(rd(0xF000_0000, 0));
    log.record(rd(0xF000_0000, 0));
    assert_eq!(log.len(), MAX_ENTRIES);
    assert_eq!(log.dropped(), 12);

    let md = log.to_markdown();
    assert!(
        md.contains("12 accesses not recorded"),
        "{}",
        md.lines().last().unwrap()
    );
}

#[test]
fn entries_keep_first_occurrence_order() {
    let mut log = UnmappedLog::default();
    log.record(rd(0x30, 0));
    log.record(rd(0x10, 0));
    log.record(rd(0x30, 0));
    log.record(rd(0x20, 0));
    let order: Vec<_> = log.entries().map(|(a, c)| (a.addr, c)).collect();
    assert_eq!(order, vec![(0x30, 2), (0x10, 1), (0x20, 1)]);
}

#[test]
fn serde_roundtrip_rebuilds_index_and_keeps_dropped() {
    let mut log = UnmappedLog::default();
    log.record(rd(0x30, 1));
    log.record(rd(0x30, 1));
    log.record(rd(0x40, 1));
    let json = serde_json::to_string(&log).unwrap();
    let mut back: UnmappedLog = serde_json::from_str(&json).unwrap();
    assert_eq!(back.len(), 2);
    back.record(rd(0x30, 1));
    let got: Vec<_> = back.entries().map(|(a, c)| (a.addr, c)).collect();
    assert_eq!(got, vec![(0x30, 3), (0x40, 1)]);
}

#[test]
fn serde_roundtrip_preserves_dropped_counter() {
    let mut log = UnmappedLog::default();
    for i in 0..MAX_ENTRIES as u32 + 3 {
        log.record(rd(i * 4, 0));
    }
    assert_eq!(log.dropped(), 3);
    let json = serde_json::to_string(&log).unwrap();
    let back: UnmappedLog = serde_json::from_str(&json).unwrap();
    assert_eq!(back.len(), MAX_ENTRIES);
    assert_eq!(back.dropped(), 3);
}
