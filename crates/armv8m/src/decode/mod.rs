//! Instruction decode.

pub mod t16;

pub use t16::decode16;

use crate::instr::Instr;

/// True when `hw1` is the first halfword of a 32-bit Thumb instruction.
pub fn is_32bit(hw1: u16) -> bool {
    hw1 >> 11 >= 0b11101
}

/// Decode a 32-bit Thumb instruction.
pub fn decode32(hw1: u16, hw2: u16) -> Instr {
    // Tasks 4/5 fill in the 32-bit encodings.
    Instr::Undefined {
        raw: (u32::from(hw1) << 16) | u32::from(hw2),
    }
}
