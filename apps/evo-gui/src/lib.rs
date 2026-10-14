//! Desktop GUI of the TI-84 Evo emulator.
//!
//! The GUI talks to an emulated machine only through [`backend::EmuBackend`].

pub mod app;
pub mod backend;
pub mod input;
pub mod keypad;
pub mod lcd;
pub mod pacer;
pub mod theme;
