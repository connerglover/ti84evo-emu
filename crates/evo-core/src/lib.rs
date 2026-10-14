//! TI-84 Evo machine core: scheduler, bus fabric and peripherals.

pub mod gdb;
pub mod host;
pub mod machine;
pub mod scheduler;

pub use host::UartPort;
pub use machine::{CPU_HZ, Machine, StopReason};
pub use scheduler::{EventKind, Scheduler};
