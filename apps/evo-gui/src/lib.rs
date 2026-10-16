//! Desktop GUI of the TI-84 Evo emulator.
//!
//! The GUI talks to an emulated machine only through [`backend::EmuBackend`].

pub mod app;
pub mod backend;
pub mod capture;
pub mod connect;
pub mod debug;
pub mod feature;
pub mod features;
pub mod files;
pub mod help;
pub mod input;
pub mod instance;
pub mod keymap_window;
pub mod keypad;
pub mod lcd;
pub mod logbuf;
pub mod pacer;
pub mod platform;
pub mod prefs;
pub mod store;
pub mod theme;
pub mod toast;
pub mod toolbar;
pub mod tools;
pub mod wizard;
