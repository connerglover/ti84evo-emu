//! TI-84 Evo machine core: scheduler, bus fabric and peripherals.

pub mod debug;
pub mod gdb;
pub mod host;
pub mod keys;
pub mod machine;
pub mod provision;
pub mod scheduler;
pub mod state;

// The CLI and GUI depend on evo-core only; they name the flash file and state errors here.
pub use debug::RunGoal;
pub use evo_hw::dev::lcdc::Frame;
pub use evo_hw::nor::{NvError, NvState, write_atomic};
pub use evo_hw::{AccessDir, BUS_RING_CAP, BusRecord, WatchError, WatchKinds, WatchRange};
pub use evo_hw::{DeviceInspect, RegView, StateError};
pub use host::UartPort;
pub use keys::{Key, KeyAction, KeyScriptError, parse_key_script};
pub use machine::{CPU_HZ, Machine, StopReason};
pub use provision::{ProvisionError, ProvisionReport, provision};
pub use scheduler::{EventKind, Scheduler};
pub use state::{STATE_MAGIC, STATE_VERSION};
