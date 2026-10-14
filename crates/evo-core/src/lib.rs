//! TI-84 Evo machine core: scheduler, bus fabric and peripherals.

pub mod scheduler;

pub use scheduler::{EventKind, Scheduler};
