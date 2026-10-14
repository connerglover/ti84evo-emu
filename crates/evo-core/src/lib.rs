//! TI-84 Evo machine core: scheduler, bus fabric and peripherals.

pub mod gdb;
pub mod host;
pub mod keys;
pub mod machine;
pub mod scheduler;

// The CLI and GUI depend on evo-core only; they name the flash file and state errors here.
pub use evo_hw::StateError;
pub use evo_hw::dev::lcdc::Frame;
pub use evo_hw::nor::{NvError, NvState};
pub use host::UartPort;
pub use keys::{Key, KeyAction, KeyScriptError, parse_key_script};
pub use machine::{CPU_HZ, Machine, StopReason};
pub use scheduler::{EventKind, Scheduler};
