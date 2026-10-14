//! 32-bit Thumb branches and miscellaneous control: B.W, BL, MSR/MRS, hints, barriers, UDF.

use super::t32::{field, gp};
use crate::instr::{BarrierKind, HintOp, Instr};

/// Sign-extend the low `bits` bits of `v`.
fn sext(v: u32, bits: u32) -> i32 {
    ((v << (32 - bits)) as i32) >> (32 - bits)
}

/// Branches and miscellaneous control: `11110 x ... | 1 x x x ...`.
pub(super) fn branch_misc(h1: u32, h2: u32) -> Option<Instr> {
    let s = field(h1, 10, 1);
    let j1 = field(h2, 13, 1);
    let j2 = field(h2, 11, 1);
    let imm11 = field(h2, 0, 11);
    match (h2 >> 14 & 1, h2 >> 12 & 1) {
        // B T4 / BL: I1 = NOT(J1 EOR S), I2 = NOT(J2 EOR S)
        (0, 1) | (1, 1) => {
            let i1 = (j1 ^ s) ^ 1;
            let i2 = (j2 ^ s) ^ 1;
            let imm = sext(
                (s << 24) | (i1 << 23) | (i2 << 22) | (field(h1, 0, 10) << 12) | (imm11 << 1),
                25,
            );
            Some(if h2 >> 14 & 1 == 0 {
                Instr::B { imm }
            } else {
                Instr::Bl { imm }
            })
        }
        (0, 0) if field(h1, 7, 3) != 0b111 => {
            let imm = sext(
                (s << 20) | (j2 << 19) | (j1 << 18) | (field(h1, 0, 6) << 12) | (imm11 << 1),
                21,
            );
            Some(Instr::BCond {
                cond: field(h1, 6, 4) as u8,
                imm,
            })
        }
        (0, 0) => system(h1, h2),
        // BLX (immediate) does not exist in M-profile; UDF.W is reported as undefined.
        _ => None,
    }
}

/// MSR, MRS, hints and barriers (`1111 0011 1xxx | 1000 ...`).
fn system(h1: u32, h2: u32) -> Option<Instr> {
    let rn = field(h1, 0, 4) as u8;
    match field(h1, 4, 7) {
        // MSR (register); R == 1 selects SPSR, which does not exist.
        0b0111000 if h2 >> 12 == 0b1000 && field(h2, 8, 2) == 0 => gp(rn).then_some(Instr::Msr {
            sysm: field(h2, 0, 8) as u8,
            rn,
            mask: field(h2, 10, 2) as u8,
        }),
        0b0111010 if rn == 15 && h2 & 0xFF00 == 0x8000 => Some(Instr::Hint(match h2 & 0xFF {
            1 => HintOp::Yield,
            2 => HintOp::Wfe,
            3 => HintOp::Wfi,
            4 => HintOp::Sev,
            // NOP, DBG and the unallocated hints.
            _ => HintOp::Nop,
        })),
        0b0111011 if rn == 15 && h2 & 0xFF00 == 0x8F00 => {
            let option = field(h2, 0, 4) as u8;
            let barrier = |kind| Some(Instr::Barrier { kind, option });
            match field(h2, 4, 4) {
                0b0010 if option == 0b1111 => Some(Instr::Clrex),
                0b0100 => barrier(BarrierKind::Dsb),
                0b0101 => barrier(BarrierKind::Dmb),
                0b0110 => barrier(BarrierKind::Isb),
                _ => None,
            }
        }
        0b0111110 if rn == 15 && h2 >> 12 == 0b1000 => {
            let rd = field(h2, 8, 4) as u8;
            gp(rd).then_some(Instr::Mrs {
                rd,
                sysm: field(h2, 0, 8) as u8,
            })
        }
        _ => None,
    }
}
