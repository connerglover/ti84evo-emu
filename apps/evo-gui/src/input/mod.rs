//! Calculator key input from the pointer and the PC keyboard.

pub mod keymap;
pub mod latch;

pub use keymap::{KeyAction, KeyboardTracker, Keymap, PcKey, pc_key_label};
pub use latch::{KeyLatch, MIN_HOLD_CYCLES, Source};
