//! Log of accesses to addresses nobody has modeled yet.
//!
//! The TI-84 Evo SoC is undocumented. Instead of faulting on an unknown address the memory map
//! reads zero / drops the write and records the access here; the resulting table is how the
//! real peripheral map gets discovered.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// Most distinct entries the log keeps. A runaway guest sweeping the address space must not
/// exhaust host memory; accesses beyond this are only counted in [`UnmappedLog::dropped`].
pub const MAX_ENTRIES: usize = 65_536;

/// One access that did not land on RAM, ROM or a device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnmappedAccess {
    pub addr: u32,
    /// Access width in bytes: 1, 2 or 4.
    pub width: u8,
    /// `Some(value)` for a (dropped) write, masked to `width`; `None` for a read.
    pub write: Option<u32>,
    /// `MemoryMap::pc_hint` at the time of the access.
    pub pc: u32,
    pub secure: bool,
}

/// Identity of a log entry: accesses with the same key are one entry with a count. The value
/// written and the security state are not part of it; the first occurrence's are kept.
type Key = (u32, u8, bool, u32);

fn key(a: &UnmappedAccess) -> Key {
    (a.addr, a.width, a.write.is_some(), a.pc)
}

/// Deduplicating, size-bounded log of [`UnmappedAccess`]es, in order of first occurrence.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(from = "LogRepr")]
pub struct UnmappedLog {
    entries: Vec<(UnmappedAccess, u64)>,
    dropped: u64,
    /// `key -> position in entries`. Derived data: rebuilt on deserialize, not stored.
    #[serde(skip)]
    index: HashMap<Key, usize>,
}

/// What actually goes over the wire for [`UnmappedLog`] (no derived index).
#[derive(Deserialize)]
struct LogRepr {
    entries: Vec<(UnmappedAccess, u64)>,
    dropped: u64,
}

impl From<LogRepr> for UnmappedLog {
    fn from(r: LogRepr) -> Self {
        let mut log = UnmappedLog {
            dropped: r.dropped,
            ..UnmappedLog::default()
        };
        // Re-insert through the normal path so a hand-edited or corrupt file still ends up
        // deduplicated and within the cap.
        for (access, count) in r.entries {
            log.record_n(access, count);
        }
        log
    }
}

impl UnmappedLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one access.
    pub fn record(&mut self, access: UnmappedAccess) {
        self.record_n(access, 1);
    }

    fn record_n(&mut self, access: UnmappedAccess, n: u64) {
        match self.index.get(&key(&access)) {
            Some(&i) => {
                let count = &mut self.entries[i].1;
                *count = count.saturating_add(n);
            }
            None if self.entries.len() < MAX_ENTRIES => {
                self.index.insert(key(&access), self.entries.len());
                self.entries.push((access, n));
            }
            None => self.dropped = self.dropped.saturating_add(n),
        }
    }

    /// Distinct entries with their occurrence counts, in order of first occurrence.
    pub fn entries(&self) -> impl Iterator<Item = (&UnmappedAccess, u64)> {
        self.entries.iter().map(|(a, c)| (a, *c))
    }

    /// Number of distinct entries held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Accesses (not distinct entries) that were not recorded because the log was full.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// The log as a markdown table, one row per entry in order of first occurrence.
    pub fn to_markdown(&self) -> String {
        let mut s = String::from(
            "| addr | width | R/W | value | pc | sec | count |\n|---|---|---|---|---|---|---|\n",
        );
        for (a, count) in self.entries() {
            let (rw, value) = match a.write {
                Some(v) => ("W", format!("0x{v:0w$x}", w = usize::from(a.width) * 2)),
                None => ("R", "-".to_owned()),
            };
            let sec = if a.secure { "S" } else { "NS" };
            // Writing to a String cannot fail.
            let _ = writeln!(
                s,
                "| 0x{:08x} | {} | {rw} | {value} | 0x{:08x} | {sec} | {count} |",
                a.addr, a.width, a.pc
            );
        }
        if self.dropped > 0 {
            let _ = write!(
                s,
                "\n{} accesses not recorded (log capped at {MAX_ENTRIES} distinct entries)\n",
                self.dropped
            );
        }
        s
    }
}
