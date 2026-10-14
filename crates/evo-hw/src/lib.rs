//! TI-84 Evo hardware models: the memory map and (later) peripherals.

pub mod memmap;
mod region;
pub mod unmapped;

pub use memmap::MemoryMap;
pub use region::Device;
pub use unmapped::{UnmappedAccess, UnmappedKind, UnmappedLog};
