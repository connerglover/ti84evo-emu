//! SoC peripherals, one module per block. Types live at `evo_hw::dev::<module>::<Type>`; they
//! are registered at their base addresses by [`crate::soc::attach`].

pub mod crypto;
pub mod gpio;
pub mod keypad;
pub mod lcdc;
pub mod mpc;
pub mod spi;
pub mod spif;
pub mod storage;
pub mod sysctl;
pub mod timer;
pub mod uart;
pub mod usb;
