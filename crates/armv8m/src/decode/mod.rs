//! Instruction decode.

pub mod t16;
pub mod t32;

pub use t16::decode16;
pub use t32::decode32;

/// True when `hw1` is the first halfword of a 32-bit Thumb instruction.
pub fn is_32bit(hw1: u16) -> bool {
    hw1 >> 11 >= 0b11101
}
