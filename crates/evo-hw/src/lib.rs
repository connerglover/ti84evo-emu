//! TI-84 Evo hardware models: the memory map, the device runtime and the SoC peripherals.

pub mod dev;
pub mod device;
pub mod memmap;
pub mod nor;
mod region;
pub mod soc;
pub mod unmapped;
pub mod watch;

pub use device::{DevCtx, DevHarness, Device, ResetKind, StateError, load_serde, save_serde};
pub use memmap::{DeviceState, MemoryMap};
pub use unmapped::{UnmappedAccess, UnmappedKind, UnmappedLog};
pub use watch::{AccessDir, BUS_RING_CAP, BusRecord, WatchError, WatchHit, WatchKinds, WatchRange};

/// CPU clock in Hz. One `Scheduler::now` unit is one CPU cycle; devices derive every rate from
/// this.
pub const CPU_HZ: u64 = 156_000_000;
