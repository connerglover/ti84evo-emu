//! TI-84 Evo hardware models: the memory map, the device runtime and (later) peripherals.

pub mod device;
pub mod memmap;
mod region;
pub mod unmapped;

pub use device::{DevCtx, DevHarness, Device, ResetKind, StateError, load_serde, save_serde};
pub use memmap::{DeviceState, MemoryMap};
pub use unmapped::{UnmappedAccess, UnmappedKind, UnmappedLog};

/// CPU clock in Hz. One `Scheduler::now` unit is one CPU cycle; devices derive every rate from
/// this.
pub const CPU_HZ: u64 = 156_000_000;
