//! DSP-extension decode: parallel add/subtract, saturating add/subtract, DSP multiplies,
//! `USAD8`, `SSAT16`/`USAT16` and `SXTB16`/`UXTB16`. Called from `t32`'s group decoders.
//!
//! As elsewhere, encodings the Arm ARM marks UNPREDICTABLE for SP/PC operands (and
//! `RdLo == RdHi` for the long multiplies) decode to `Instr::Undefined` (`None` here).

use super::t32::{field, gp};
use crate::instr::{Instr, ParKind, ParOp, Reg};

/// Parallel add/subtract: `111110101 op1 Rn | 1111 Rd 0 prefix Rm`.
pub(super) fn parallel(h1: u32, h2: u32) -> Option<Instr> {
    let (rn, rd, rm) = (
        field(h1, 0, 4) as u8,
        field(h2, 8, 4) as u8,
        field(h2, 0, 4) as u8,
    );
    if field(h2, 12, 4) != 0xF || h2 & 0x80 != 0 || !(gp(rd) && gp(rn) && gp(rm)) {
        return None;
    }
    let op = match field(h1, 4, 3) {
        0b000 => ParOp::Add8,
        0b001 => ParOp::Add16,
        0b010 => ParOp::Asx,
        0b100 => ParOp::Sub8,
        0b101 => ParOp::Sub16,
        0b110 => ParOp::Sax,
        _ => return None,
    };
    let kind = match field(h2, 4, 3) {
        0b000 => ParKind::S,
        0b001 => ParKind::Q,
        0b010 => ParKind::Sh,
        0b100 => ParKind::U,
        0b101 => ParKind::Uq,
        0b110 => ParKind::Uh,
        _ => return None,
    };
    Some(Instr::Parallel {
        kind,
        op,
        rd,
        rn,
        rm,
    })
}

/// `QADD`/`QDADD`/`QSUB`/`QDSUB rd, rm, rn` (`sel` is `op2[1:0]`).
pub(super) fn sat_arith(rd: Reg, rm: Reg, rn: Reg, sel: u32) -> Option<Instr> {
    (gp(rd) && gp(rm) && gp(rn)).then_some(Instr::SatArith {
        rd,
        rm,
        rn,
        sub: sel & 2 != 0,
        double: sel & 1 != 0,
    })
}

/// `SXTB16`/`UXTB16`/`SXTAB16`/`UXTAB16` (`op1` is 2 or 3; `Rn == PC` selects the non-add form).
pub(super) fn extend16(h1: u32, h2: u32) -> Option<Instr> {
    let (rn, rd, rm) = (
        field(h1, 0, 4) as u8,
        field(h2, 8, 4) as u8,
        field(h2, 0, 4) as u8,
    );
    if !gp(rd) || !gp(rm) || rn == 13 {
        return None;
    }
    Some(Instr::Extend16 {
        rd,
        rn: (rn != 15).then_some(rn),
        rm,
        rotate: (field(h2, 4, 2) * 8) as u8,
        signed: field(h1, 4, 4) == 2,
    })
}

/// `SSAT16`/`USAT16` (the `ASR #0` forms of `SSAT`/`USAT`); `imm4` is `h2[3:0]`.
pub(super) fn sat16(h1: u32, h2: u32, signed: bool) -> Option<Instr> {
    let (rn, rd) = (field(h1, 0, 4) as u8, field(h2, 8, 4) as u8);
    if !(gp(rd) && gp(rn)) || field(h2, 4, 2) != 0 {
        return None;
    }
    let imm4 = field(h2, 0, 4) as u8;
    Some(if signed {
        Instr::Ssat16 {
            rd,
            rn,
            bits: imm4 + 1,
        }
    } else {
        Instr::Usat16 { rd, rn, bits: imm4 }
    })
}

/// Optional accumulate register: `PC` (15) means "none", SP is UNPREDICTABLE.
fn accumulator(ra: u8) -> Option<Option<Reg>> {
    match ra {
        15 => Some(None),
        r if gp(r) => Some(Some(r)),
        _ => None,
    }
}

/// DSP 32-bit multiplies: `111110110 op1 Rn | Ra Rd op2 Rm` with `op1 != 0` (`op1` is 3 bits).
pub(super) fn multiply(h1: u32, h2: u32) -> Option<Instr> {
    let rn = field(h1, 0, 4) as u8;
    let (rd, rm) = (field(h2, 8, 4) as u8, field(h2, 0, 4) as u8);
    if !(gp(rd) && gp(rn) && gp(rm)) {
        return None;
    }
    let ra = accumulator(field(h2, 12, 4) as u8)?;
    let op2 = field(h2, 4, 4);
    let bit4 = op2 & 1 != 0;
    // Everything except SMULxy/SMLAxy has `op2[3:1] == 0`.
    let plain = op2 >> 1 == 0;
    match field(h1, 4, 3) {
        0b001 if op2 >> 2 == 0 => Some(Instr::MulHalf {
            rd,
            rn,
            rm,
            ra,
            n_top: op2 & 2 != 0,
            m_top: bit4,
        }),
        0b010 | 0b100 if plain => Some(Instr::DualMul {
            rd,
            rn,
            rm,
            ra,
            sub: field(h1, 4, 3) == 0b100,
            x: bit4,
        }),
        0b011 if plain => Some(Instr::MulWordHalf {
            rd,
            rn,
            rm,
            ra,
            top: bit4,
        }),
        0b101 | 0b110 if plain => Some(Instr::MulMsw {
            rd,
            rn,
            rm,
            ra,
            sub: field(h1, 4, 3) == 0b110,
            round: bit4,
        }),
        0b111 if op2 == 0 => Some(Instr::Usad8 { rd, rn, rm, ra }),
        _ => None,
    }
}

/// DSP long multiplies (`SMLALxy`, `SMLALD`, `SMLSLD`): `111110111 op1 Rn | RdLo RdHi op2 Rm`.
pub(super) fn long_multiply(h1: u32, h2: u32) -> Option<Instr> {
    let rn = field(h1, 0, 4) as u8;
    let (rdlo, rdhi, rm) = (
        field(h2, 12, 4) as u8,
        field(h2, 8, 4) as u8,
        field(h2, 0, 4) as u8,
    );
    if !(gp(rdlo) && gp(rdhi) && gp(rn) && gp(rm)) || rdlo == rdhi {
        return None;
    }
    let op2 = field(h2, 4, 4);
    let bit4 = op2 & 1 != 0;
    match (field(h1, 4, 3), op2 >> 1) {
        (0b100, 0b100 | 0b101) => Some(Instr::MulLongHalf {
            rdlo,
            rdhi,
            rn,
            rm,
            n_top: op2 & 2 != 0,
            m_top: bit4,
        }),
        (op1 @ (0b100 | 0b101), 0b110) => Some(Instr::DualMulLong {
            rdlo,
            rdhi,
            rn,
            rm,
            sub: op1 == 0b101,
            x: bit4,
        }),
        _ => None,
    }
}
