//! TI-84 Evo machine core: scheduler, bus fabric and peripherals.

pub mod gdb;
pub mod machine;
pub mod scheduler;

pub use machine::{Machine, StopReason};
pub use scheduler::{EventKind, Scheduler};
