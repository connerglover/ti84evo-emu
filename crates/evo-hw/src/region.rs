//! Region bookkeeping for the memory map: what an address window points at, and the sorted,
//! non-overlapping table of windows.

use serde::{Deserialize, Serialize};

/// Backing storage that RAM and ROM windows point into. One buffer can sit behind several
/// windows (aliases), so the bytes live here once and windows hold an index.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Buffer {
    pub name: String,
    pub data: Vec<u8>,
}

/// What a window refers to. Indices are into `MemoryMap::buffers` / `MemoryMap::slots`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Target {
    Ram(usize),
    Rom(usize),
    Device(usize),
}

/// One window of the 32-bit address space. `size` is never zero.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Region {
    pub name: String,
    pub base: u32,
    pub size: u32,
    pub target: Target,
}

impl Region {
    /// One past the last address, which can be 2^32 (hence `u64`).
    pub fn end(&self) -> u64 {
        u64::from(self.base) + u64::from(self.size)
    }
}

/// Index of the region containing `addr`. `regions` must be sorted by base and non-overlapping.
pub(crate) fn find(regions: &[Region], addr: u32) -> Option<usize> {
    let i = regions.partition_point(|r| r.base <= addr).checked_sub(1)?;
    (u64::from(addr) < regions[i].end()).then_some(i)
}

/// Insert `new`, keeping `regions` sorted by base. A zero-sized window is ignored.
///
/// # Panics
///
/// If the window runs past the end of the address space or overlaps an existing window. That is
/// a bug in the static machine layout, not something a guest can cause.
pub(crate) fn insert(regions: &mut Vec<Region>, new: Region) {
    if new.size == 0 {
        return;
    }
    assert!(
        new.end() <= 1 << 32,
        "region '{}' at {:#010x} (size {:#x}) runs past the end of the address space",
        new.name,
        new.base,
        new.size
    );
    let at = regions.partition_point(|r| r.base < new.base);
    let before = at.checked_sub(1).and_then(|i| regions.get(i));
    for other in before.into_iter().chain(regions.get(at)) {
        assert!(
            u64::from(other.base) >= new.end() || u64::from(new.base) >= other.end(),
            "region '{}' at {:#010x} (size {:#x}) would overlap region '{}' at {:#010x} (size {:#x})",
            new.name,
            new.base,
            new.size,
            other.name,
            other.base,
            other.size
        );
    }
    regions.insert(at, new);
}
