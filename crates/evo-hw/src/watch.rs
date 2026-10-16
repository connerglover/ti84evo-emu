//! Data watchpoints and the bus monitor of [`MemoryMap`](crate::MemoryMap).
//!
//! Every guest data access (every `Bus` read or write that is not an instruction fetch) is
//! observed while anything is watching: the page-table fast path, devices and unmapped space
//! alike, with the value read or written. Host `peek`/`poke` and fetches are not observed.
//! Exec ranges are stored with the others but are checked by the `Machine` before it steps,
//! not here (the CPU's instruction cache bypasses the bus).

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// The direction of an observed access.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccessDir {
    Read,
    Write,
    Exec,
}

/// Which accesses a [`WatchRange`] stops on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchKinds {
    pub read: bool,
    pub write: bool,
    pub exec: bool,
}

impl WatchKinds {
    fn any(self) -> bool {
        self.read || self.write || self.exec
    }

    /// Whether a data access in direction `dir` matches (exec never does on the bus).
    fn data_match(self, dir: AccessDir) -> bool {
        match dir {
            AccessDir::Read => self.read,
            AccessDir::Write => self.write,
            AccessDir::Exec => false,
        }
    }
}

/// A watched address range: `len` bytes from `start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchRange {
    pub start: u32,
    pub len: u32,
    pub kinds: WatchKinds,
}

/// Why a [`WatchRange`] was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WatchError {
    #[error("watchpoint length is 0")]
    Empty,
    #[error("watchpoint runs past 0xffffffff")]
    Overflow,
    #[error("watchpoint has no access kind")]
    NoKind,
}

/// One past the last address.
const TOP: u64 = 1 << 32;

/// Whether `[start, start + len)` (clamped at 2^32) and the `width`-byte access at `addr` share
/// a byte. Accesses wrap at 2^32 like the CPU's addresses.
#[inline]
fn spans_overlap(start: u32, len: u32, addr: u32, width: u8) -> bool {
    let (s, e) = (
        u64::from(start),
        (u64::from(start) + u64::from(len)).min(TOP),
    );
    let (a, b) = (u64::from(addr), u64::from(addr) + u64::from(width));
    let hit = |a: u64, b: u64| a < e && s < b;
    hit(a, b.min(TOP)) || (b > TOP && hit(0, b - TOP))
}

impl WatchRange {
    /// Non-empty, inside the address space, and watching at least one kind of access.
    pub fn validate(&self) -> Result<(), WatchError> {
        if self.len == 0 {
            return Err(WatchError::Empty);
        }
        if u64::from(self.start) + u64::from(self.len) > TOP {
            return Err(WatchError::Overflow);
        }
        if !self.kinds.any() {
            return Err(WatchError::NoKind);
        }
        Ok(())
    }

    /// Whether the `width`-byte access at `addr` touches any byte of the range.
    pub fn overlaps(&self, addr: u32, width: u8) -> bool {
        spans_overlap(self.start, self.len, addr, width)
    }
}

/// The first data access that matched a watchpoint since the last
/// [`take_watch_hit`](crate::MemoryMap::take_watch_hit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchHit {
    pub addr: u32,
    pub width: u8,
    pub dir: AccessDir,
    /// The value read or written (the access's width, zero-extended).
    pub value: u32,
}

/// One access recorded by the bus monitor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusRecord {
    pub addr: u32,
    pub width: u8,
    pub dir: AccessDir,
    pub value: u32,
    /// `MemoryMap::pc_hint` at the access: the instruction that made it.
    pub pc: u32,
    /// `MemoryMap::now` at the access.
    pub cycle: u64,
}

/// The bus monitor keeps at most this many records, dropping the oldest.
pub const BUS_RING_CAP: usize = 4096;

/// What the per-access hook checks; present only while something is watching.
#[derive(Debug)]
struct Active {
    /// The ranges with read or write set.
    data: Vec<WatchRange>,
    /// The monitored `(start, len)`, len > 0.
    monitor: Option<(u32, u32)>,
}

/// Watchpoints, bus monitor and their results. Not saved state.
#[derive(Debug, Default)]
pub(crate) struct Watch {
    /// Every range as set, exec ranges included.
    ranges: Vec<WatchRange>,
    monitor: Option<(u32, u32)>,
    records: VecDeque<BusRecord>,
    hit: Option<WatchHit>,
    /// `None` whenever no read/write range and no monitor is set: the hot path then costs one
    /// `is_some` per access.
    active: Option<Box<Active>>,
}

impl Watch {
    /// Whether any data access has to be observed.
    #[inline(always)]
    pub(crate) fn watching(&self) -> bool {
        self.active.is_some()
    }

    pub(crate) fn set_ranges(&mut self, ranges: Vec<WatchRange>) -> Result<(), WatchError> {
        ranges.iter().try_for_each(WatchRange::validate)?;
        self.ranges = ranges;
        self.refresh();
        Ok(())
    }

    pub(crate) fn ranges(&self) -> Vec<WatchRange> {
        self.ranges.clone()
    }

    pub(crate) fn set_monitor(&mut self, range: Option<(u32, u32)>) {
        self.monitor = range.filter(|&(_, len)| len > 0);
        self.refresh();
    }

    pub(crate) fn records(&self) -> Vec<BusRecord> {
        self.records.iter().copied().collect()
    }

    pub(crate) fn clear_records(&mut self) {
        self.records.clear();
    }

    pub(crate) fn take_hit(&mut self) -> Option<WatchHit> {
        self.hit.take()
    }

    fn refresh(&mut self) {
        let data: Vec<WatchRange> = self
            .ranges
            .iter()
            .copied()
            .filter(|r| r.kinds.read || r.kinds.write)
            .collect();
        self.active = (!data.is_empty() || self.monitor.is_some()).then(|| {
            Box::new(Active {
                data,
                monitor: self.monitor,
            })
        });
    }

    /// A guest data access happened. Call only while [`watching`](Self::watching).
    #[cold]
    #[inline(never)]
    pub(crate) fn observe(
        &mut self,
        addr: u32,
        width: u8,
        dir: AccessDir,
        value: u32,
        pc: u32,
        cycle: u64,
    ) {
        let Some(active) = self.active.as_deref() else {
            return;
        };
        if self.hit.is_none()
            && active
                .data
                .iter()
                .any(|r| r.kinds.data_match(dir) && r.overlaps(addr, width))
        {
            self.hit = Some(WatchHit {
                addr,
                width,
                dir,
                value,
            });
        }
        if let Some((start, len)) = active.monitor
            && spans_overlap(start, len, addr, width)
        {
            if self.records.len() >= BUS_RING_CAP {
                self.records.pop_front();
            }
            self.records.push_back(BusRecord {
                addr,
                width,
                dir,
                value,
                pc,
                cycle,
            });
        }
    }
}
