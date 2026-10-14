//! 32-bit Thumb decode: data processing, multiply and divide. Loads/stores and
//! coprocessor space live in `t32_mem`, branches and system instructions in `t32_branch`.
//!
//! Encodings the ARM ARM marks UNPREDICTABLE for SP/PC operands decode to
//! `Instr::Undefined`. The exceptions are the forms where SP is architecturally valid
//! (SP-based ADD/SUB, and a SP base for CMP/CMN only; TST/TEQ reject SP) and the `Rd == 15`
//! encodings that are the compare/test aliases (CMP/CMN/TST/TEQ).

use super::{t32_branch, t32_mem};
use crate::bits::{SRType, decode_imm_shift, thumb_expand_imm_c};
use crate::instr::{AluOp, Instr, LongMulKind, RevKind, Shift};

/// Operand register that is neither SP nor PC.
pub(super) fn gp(r: u8) -> bool {
    r != 13 && r != 15
}

pub(super) fn field(v: u32, lsb: u32, width: u32) -> u32 {
    (v >> lsb) & ((1 << width) - 1)
}

/// Decode a 32-bit Thumb instruction from its two halfwords.
pub fn decode32(hw1: u16, hw2: u16) -> Instr {
    let (h1, h2) = (u32::from(hw1), u32::from(hw2));
    let decoded = match hw1 >> 11 {
        0b11110 if hw2 & 0x8000 == 0 => {
            if hw1 & 0x0200 == 0 {
                modified_imm(h1, h2)
            } else {
                plain_imm(h1, h2)
            }
        }
        0b11110 => t32_branch::branch_misc(h1, h2),
        0b11101 => match hw1 >> 9 {
            0b1110101 => shifted_reg(h1, h2),
            0b1110100 => t32_mem::multiple_dual_exclusive(h1, h2),
            _ => Some(Instr::Coprocessor {
                raw: (h1 << 16) | h2,
            }),
        },
        0b11111 => match hw1 >> 8 {
            0xFA => register_ops(h1, h2),
            0xFB => multiply(h1, h2),
            0xF8 | 0xF9 => t32_mem::load_store_single(h1, h2),
            _ => Some(Instr::Coprocessor {
                raw: (h1 << 16) | h2,
            }),
        },
        _ => None,
    };
    decoded.unwrap_or(Instr::Undefined {
        raw: (h1 << 16) | h2,
    })
}

/// Logical / carry immediate ALU op with the SP/PC restrictions applied.
fn alu_imm(
    op: AluOp,
    (rd, rn): (u8, u8),
    (imm, carry): (u32, Option<bool>),
    setflags: bool,
) -> Option<Instr> {
    (gp(rd) && gp(rn)).then_some(Instr::AluImm {
        op,
        rd,
        rn,
        imm,
        carry,
        setflags,
    })
}

/// Destination/base rules shared by the 32-bit ADD/SUB forms (SP base permitted).
fn addsub_regs_ok(rd: u8, rn: u8) -> bool {
    match (rd, rn) {
        (15, _) | (_, 15) => false,
        (13, 13) => true,
        (13, _) => false,
        _ => true,
    }
}

/// Data processing (modified immediate): `11110 i 0 op S Rn | 0 imm3 Rd imm8`.
fn modified_imm(h1: u32, h2: u32) -> Option<Instr> {
    let op = field(h1, 5, 4);
    let s = h1 & 0x10 != 0;
    let rn = field(h1, 0, 4) as u8;
    let rd = field(h2, 8, 4) as u8;
    let imm12 = (field(h1, 10, 1) << 11) | (field(h2, 12, 3) << 8) | field(h2, 0, 8);
    let (imm, c) = thumb_expand_imm_c(imm12, false);
    let carry = (field(imm12, 10, 2) != 0).then_some(c);
    let alu = |op| alu_imm(op, (rd, rn), (imm, carry), s);
    let arith = |sub: bool| {
        addsub_regs_ok(rd, rn).then_some(if sub {
            Instr::SubImm {
                rd,
                rn,
                imm,
                setflags: s,
                two_op: false,
                imm12: false,
            }
        } else {
            Instr::AddImm {
                rd,
                rn,
                imm,
                setflags: s,
                two_op: false,
                imm12: false,
            }
        })
    };
    match op {
        0b0000 if rd == 15 => (s && gp(rn)).then_some(Instr::TstImm { rn, imm, carry }),
        0b0000 => alu(AluOp::And),
        0b0001 => alu(AluOp::Bic),
        0b0010 if rn == 15 => gp(rd).then_some(Instr::MovImm {
            rd,
            imm,
            setflags: s,
            carry,
        }),
        0b0010 => alu(AluOp::Orr),
        0b0011 if rn == 15 => gp(rd).then_some(Instr::MvnImm {
            rd,
            imm,
            setflags: s,
            carry,
        }),
        0b0011 => alu(AluOp::Orn),
        0b0100 if rd == 15 => (s && gp(rn)).then_some(Instr::TeqImm { rn, imm, carry }),
        0b0100 => alu(AluOp::Eor),
        0b1000 if rd == 15 => (s && rn != 15).then_some(Instr::CmnImm { rn, imm }),
        0b1000 => arith(false),
        0b1010 => alu(AluOp::Adc),
        0b1011 => alu(AluOp::Sbc),
        0b1101 if rd == 15 => (s && rn != 15).then_some(Instr::CmpImm { rn, imm }),
        0b1101 => arith(true),
        0b1110 => (gp(rd) && gp(rn)).then_some(Instr::RsbImm {
            rd,
            rn,
            imm,
            setflags: s,
        }),
        _ => None,
    }
}

/// Data processing (plain binary immediate): `11110 i 1 op5 Rn | 0 imm3 Rd imm8`.
fn plain_imm(h1: u32, h2: u32) -> Option<Instr> {
    let op = field(h1, 4, 5);
    let rn = field(h1, 0, 4) as u8;
    let rd = field(h2, 8, 4) as u8;
    let imm8 = field(h2, 0, 8);
    let imm3 = field(h2, 12, 3);
    let imm5 = (imm3 << 2) | field(h2, 6, 2);
    let imm12 = (field(h1, 10, 1) << 11) | (imm3 << 8) | imm8;
    let sat = field(h2, 0, 5);
    match op {
        0b00000 | 0b01010 => {
            let sub = op == 0b01010;
            if rn == 15 {
                // ADR: Align(PC, 4) +/- imm12
                return gp(rd).then_some(Instr::Adr {
                    rd,
                    imm: if sub { imm12.wrapping_neg() } else { imm12 },
                });
            }
            if !addsub_regs_ok(rd, rn) {
                return None;
            }
            Some(if sub {
                Instr::SubImm {
                    rd,
                    rn,
                    imm: imm12,
                    setflags: false,
                    two_op: false,
                    imm12: true,
                }
            } else {
                Instr::AddImm {
                    rd,
                    rn,
                    imm: imm12,
                    setflags: false,
                    two_op: false,
                    imm12: true,
                }
            })
        }
        0b00100 | 0b01100 => {
            let imm =
                ((field(h1, 0, 4) << 12) | (field(h1, 10, 1) << 11) | (imm3 << 8) | imm8) as u16;
            gp(rd).then_some(if op == 0b00100 {
                Instr::Movw { rd, imm }
            } else {
                Instr::Movt { rd, imm }
            })
        }
        // SSAT (sh = bit 5 selects ASR); SSAT16 (ASR #0) is not decoded.
        0b10000 | 0b10010 | 0b11000 | 0b11010 => {
            let asr = op & 2 != 0;
            if !(gp(rd) && gp(rn)) || (asr && imm5 == 0) {
                return None;
            }
            let (ty, amount) = decode_imm_shift(if asr { 2 } else { 0 }, imm5);
            let shift = Shift { ty, amount };
            Some(if op < 0b11000 {
                Instr::Ssat {
                    rd,
                    rn,
                    bits: sat as u8 + 1,
                    shift,
                }
            } else {
                Instr::Usat {
                    rd,
                    rn,
                    bits: sat as u8,
                    shift,
                }
            })
        }
        0b10100 | 0b11100 => {
            let (lsb, width) = (imm5, sat + 1);
            (gp(rd) && gp(rn) && lsb + width <= 32).then_some(Instr::Bfx {
                rd,
                rn,
                lsb: lsb as u8,
                width: width as u8,
                signed: op == 0b10100,
            })
        }
        0b10110 => {
            let (lsb, msb) = (imm5, sat);
            if !gp(rd) || msb < lsb {
                return None;
            }
            let width = (msb - lsb + 1) as u8;
            if rn == 15 {
                Some(Instr::Bfc {
                    rd,
                    lsb: lsb as u8,
                    width,
                })
            } else {
                gp(rn).then_some(Instr::Bfi {
                    rd,
                    rn,
                    lsb: lsb as u8,
                    width,
                })
            }
        }
        _ => None,
    }
}

/// Data processing (shifted register): `1110101 op S Rn | 0 imm3 Rd imm2 type Rm`.
fn shifted_reg(h1: u32, h2: u32) -> Option<Instr> {
    let op = field(h1, 5, 4);
    let s = h1 & 0x10 != 0;
    let rn = field(h1, 0, 4) as u8;
    let rd = field(h2, 8, 4) as u8;
    let rm = field(h2, 0, 4) as u8;
    let ty = field(h2, 4, 2);
    let imm5 = (field(h2, 12, 3) << 2) | field(h2, 6, 2);
    let (sty, amount) = decode_imm_shift(ty, imm5);
    let shift = Shift { ty: sty, amount };
    let alu = |op| {
        (gp(rd) && gp(rn) && gp(rm)).then_some(Instr::AluReg {
            op,
            rd,
            rn,
            rm,
            shift,
            setflags: s,
        })
    };
    // CMP/CMN/TST/TEQ aliases (Rd == 15, S == 1).
    let compare = rd == 15 && s;
    let cmp_ok = rn != 15 && gp(rm);
    match op {
        0b0000 if compare => (gp(rn) && gp(rm)).then_some(Instr::TstReg { rn, rm, shift }),
        0b0000 => alu(AluOp::And),
        0b0001 => alu(AluOp::Bic),
        // ORR with Rn == 15 is MOV / shift-by-immediate / RRX.
        0b0010 if rn == 15 => mov_or_shift(rd, rm, (ty, imm5), shift, s),
        0b0010 => alu(AluOp::Orr),
        0b0011 if rn == 15 => (gp(rd) && gp(rm)).then_some(Instr::Mvn {
            rd,
            rm,
            shift,
            setflags: s,
        }),
        0b0011 => alu(AluOp::Orn),
        0b0100 if compare => (gp(rn) && gp(rm)).then_some(Instr::TeqReg { rn, rm, shift }),
        0b0100 => alu(AluOp::Eor),
        0b0110 => {
            // PKHBT / PKHTB: S == 0, bit 4 == 0, type is 00 (BT) or 10 (TB).
            let tb = ty == 2;
            if s || h2 & 0x10 != 0 || (ty & 1) != 0 || !(gp(rd) && gp(rn) && gp(rm)) {
                return None;
            }
            Some(Instr::Pkh {
                rd,
                rn,
                rm,
                tb,
                amount: if tb { amount } else { imm5 },
            })
        }
        0b1000 if compare => cmp_ok.then_some(Instr::CmnReg { rn, rm, shift }),
        0b1000 => {
            (addsub_regs_ok(rd, rn) && gp(rm) && sp_shift_ok(rd, shift)).then_some(Instr::AddReg {
                rd,
                rn,
                rm,
                shift,
                setflags: s,
            })
        }
        0b1010 => alu(AluOp::Adc),
        0b1011 => alu(AluOp::Sbc),
        0b1101 if compare => cmp_ok.then_some(Instr::CmpReg { rn, rm, shift }),
        0b1101 => {
            (addsub_regs_ok(rd, rn) && gp(rm) && sp_shift_ok(rd, shift)).then_some(Instr::SubReg {
                rd,
                rn,
                rm,
                shift,
                setflags: s,
            })
        }
        0b1110 => (gp(rd) && gp(rn) && gp(rm)).then_some(Instr::RsbReg {
            rd,
            rn,
            rm,
            shift,
            setflags: s,
        }),
        _ => None,
    }
}

/// `ORR` with `Rn == 15`: `MOV` (register), `LSL/LSR/ASR/ROR` (immediate) or `RRX`.
fn mov_or_shift(rd: u8, rm: u8, (ty, imm5): (u32, u32), shift: Shift, s: bool) -> Option<Instr> {
    if ty == 0 && imm5 == 0 {
        // MOV.W: SP is allowed as a non-flag-setting source/destination (not both).
        let ok = if s {
            gp(rd) && gp(rm)
        } else {
            rd != 15 && rm != 15 && !(rd == 13 && rm == 13)
        };
        return ok.then_some(Instr::MovReg {
            rd,
            rm,
            setflags: s,
        });
    }
    (gp(rd) && gp(rm)).then_some(Instr::ShiftImm {
        rd,
        rm,
        ty: shift.ty,
        amount: shift.amount,
        setflags: s,
    })
}

/// Data processing (register): shifts, extends, parallel add/sub (not decoded), misc.
fn register_ops(h1: u32, h2: u32) -> Option<Instr> {
    let rn = field(h1, 0, 4) as u8;
    let rd = field(h2, 8, 4) as u8;
    let rm = field(h2, 0, 4) as u8;
    let op1 = field(h1, 4, 4);
    let op2 = field(h2, 4, 4);
    if field(h2, 12, 4) != 0xF {
        return None;
    }
    if op1 & 8 == 0 {
        if op2 == 0 {
            // LSL/LSR/ASR/ROR (register)
            let ty = [SRType::Lsl, SRType::Lsr, SRType::Asr, SRType::Ror][(op1 >> 1) as usize];
            return (gp(rd) && gp(rn) && gp(rm)).then_some(Instr::ShiftReg {
                rd,
                rn,
                rm,
                ty,
                setflags: op1 & 1 != 0,
            });
        }
        if op2 & 0xC != 0x8 {
            return None;
        }
        // SXTAH/UXTAH/SXTAB/UXTAB (and the unsupported *16 forms)
        let (signed, half) = match op1 {
            0 => (true, true),
            1 => (false, true),
            4 => (true, false),
            5 => (false, false),
            _ => return None,
        };
        let rotate = (field(h2, 4, 2) * 8) as u8;
        if !gp(rd) || !gp(rm) || rn == 13 {
            return None;
        }
        return Some(if rn == 15 {
            Instr::Extend {
                rd,
                rm,
                rotate,
                signed,
                half,
            }
        } else {
            Instr::ExtendAdd {
                rd,
                rn,
                rm,
                rotate,
                signed,
                half,
            }
        });
    }
    // Miscellaneous operations: op1 = 10xx, op2 = 10xx (Rm repeats Rn).
    if op1 & 0xC != 0x8 || op2 & 0xC != 0x8 {
        return None;
    }
    let sel = field(h2, 4, 2);
    match (field(h1, 4, 2), sel) {
        (0b01, s) if rm == rn && gp(rd) && gp(rm) => Some(match s {
            0 => Instr::Rev {
                rd,
                rm,
                kind: RevKind::Rev,
            },
            1 => Instr::Rev {
                rd,
                rm,
                kind: RevKind::Rev16,
            },
            2 => Instr::Rbit { rd, rm },
            _ => Instr::Rev {
                rd,
                rm,
                kind: RevKind::Revsh,
            },
        }),
        (0b10, 0) => (gp(rd) && gp(rn) && gp(rm)).then_some(Instr::Sel { rd, rn, rm }),
        (0b11, 0) if rm == rn && gp(rd) && gp(rm) => Some(Instr::Clz { rd, rm }),
        _ => None,
    }
}

/// Multiply, multiply accumulate, long multiply and divide: `111110111 op1 Rn | ...`.
fn multiply(h1: u32, h2: u32) -> Option<Instr> {
    let rn = field(h1, 0, 4) as u8;
    let op1 = field(h1, 4, 3);
    let hi_field = field(h2, 12, 4) as u8;
    let rd = field(h2, 8, 4) as u8;
    let op2 = field(h2, 4, 4);
    let rm = field(h2, 0, 4) as u8;
    if h1 & 0x80 == 0 {
        // MUL / MLA / MLS (the DSP multiplies are not decoded).
        if op1 != 0 || !(gp(rd) && gp(rn) && gp(rm)) {
            return None;
        }
        return match op2 {
            0 if hi_field == 15 => Some(Instr::Mul {
                rd,
                rn,
                rm,
                setflags: false,
            }),
            0 | 1 if gp(hi_field) => Some(Instr::MulAcc {
                rd,
                rn,
                rm,
                ra: hi_field,
                sub: op2 == 1,
            }),
            _ => None,
        };
    }
    let long = |kind| {
        let (rdlo, rdhi) = (hi_field, rd);
        (gp(rdlo) && gp(rdhi) && gp(rn) && gp(rm) && rdlo != rdhi).then_some(Instr::LongMul {
            kind,
            rdlo,
            rdhi,
            rn,
            rm,
        })
    };
    let div = |signed| {
        (hi_field == 15 && gp(rd) && gp(rn) && gp(rm)).then_some(Instr::Div { rd, rn, rm, signed })
    };
    match (op1, op2) {
        (0b000, 0b0000) => long(LongMulKind::Smull),
        (0b001, 0b1111) => div(true),
        (0b010, 0b0000) => long(LongMulKind::Umull),
        (0b011, 0b1111) => div(false),
        (0b100, 0b0000) => long(LongMulKind::Smlal),
        (0b110, 0b0000) => long(LongMulKind::Umlal),
        (0b110, 0b0110) => long(LongMulKind::Umaal),
        _ => None,
    }
}

/// ADD/SUB (register) with `Rd == SP` require `LSL #0..=3`.
fn sp_shift_ok(rd: u8, shift: Shift) -> bool {
    rd != 13 || (shift.ty == SRType::Lsl && shift.amount <= 3)
}
