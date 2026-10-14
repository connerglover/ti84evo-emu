//! Data-processing instructions.

use super::Flow;
use crate::bits::SRType;
use crate::bits::{add_with_carry, shift_c};
use crate::cpu::Cpu;
use crate::exception::Fault;
use crate::instr::{AluOp, Instr, LongMulKind, RevKind, Shift};
use crate::scs::scb::CCR_DIV_0_TRP;

/// Mask of the low `width` bits (`width` is 1..=32).
fn field_mask(width: u8) -> u32 {
    u32::MAX >> (32 - u32::from(width))
}

/// Clamp `v` to `min..=max`, set the sticky Q flag when clamped, and write `rd`.
fn saturate(cpu: &mut Cpu, rd: u8, v: i64, min: i64, max: i64) -> Flow {
    let r = v.clamp(min, max);
    if r != v {
        cpu.regs.xpsr.q = true;
    }
    cpu.write_reg(rd, r as u32);
    Flow::Next
}

fn set_nz(cpu: &mut Cpu, v: u32) {
    cpu.regs.xpsr.n = v >> 31 != 0;
    cpu.regs.xpsr.z = v == 0;
}

fn set_nzcv(cpu: &mut Cpu, (v, c, o): (u32, bool, bool)) {
    set_nz(cpu, v);
    cpu.regs.xpsr.c = c;
    cpu.regs.xpsr.v = o;
}

/// Write an ALU result; a PC destination is an `ALUWritePC` branch.
fn write_rd(cpu: &mut Cpu, rd: u8, v: u32) -> Flow {
    if rd == 15 {
        cpu.branch_write_pc(v);
        Flow::Branched
    } else {
        cpu.write_reg(rd, v);
        Flow::Next
    }
}

/// Finish an add/subtract style operation producing `(result, carry, overflow)`.
/// The register is written first: a write to SP that violates the stack limit faults the
/// instruction, which then leaves the flags alone.
fn arith(cpu: &mut Cpu, rd: u8, res: (u32, bool, bool), setflags: bool) -> Flow {
    let flow = write_rd(cpu, rd, res.0);
    if setflags && !cpu.sp_violation {
        set_nzcv(cpu, res);
    }
    flow
}

/// Finish a logical operation producing `(result, carry)`.
fn logic(cpu: &mut Cpu, rd: u8, (v, c): (u32, bool), setflags: bool) -> Flow {
    let flow = write_rd(cpu, rd, v);
    if setflags && !cpu.sp_violation {
        set_nz(cpu, v);
        cpu.regs.xpsr.c = c;
    }
    flow
}

/// `Shift_C(R[m], shift)` with the current carry flag.
fn shifted(cpu: &Cpu, rm: u8, sh: Shift) -> (u32, bool) {
    shift_c(cpu.reg(rm), sh.ty, sh.amount, cpu.regs.xpsr.c)
}

/// Execute a data-processing instruction; `None` if `instr` is not one.
pub(super) fn execute(cpu: &mut Cpu, instr: &Instr, suppress_flags: bool) -> Option<Flow> {
    let sf = |setflags: bool| setflags && !suppress_flags;
    let carry = cpu.regs.xpsr.c;
    let flow = match *instr {
        Instr::MovImm {
            rd,
            imm,
            setflags,
            carry: c,
        } => logic(cpu, rd, (imm, c.unwrap_or(carry)), sf(setflags)),
        Instr::Movw { rd, imm } => write_rd(cpu, rd, u32::from(imm)),
        Instr::Movt { rd, imm } => {
            let v = (cpu.reg(rd) & 0xFFFF) | (u32::from(imm) << 16);
            write_rd(cpu, rd, v)
        }
        Instr::MvnImm {
            rd,
            imm,
            setflags,
            carry: c,
        } => logic(cpu, rd, (!imm, c.unwrap_or(carry)), sf(setflags)),
        Instr::MovReg { rd, rm, setflags } => {
            let v = cpu.reg(rm);
            logic(cpu, rd, (v, carry), sf(setflags))
        }
        Instr::Mvn {
            rd,
            rm,
            shift,
            setflags,
        } => {
            let (v, c) = shifted(cpu, rm, shift);
            logic(cpu, rd, (!v, c), sf(setflags))
        }
        Instr::ShiftImm {
            rd,
            rm,
            ty,
            amount,
            setflags,
        } => {
            let res = shift_c(cpu.reg(rm), ty, amount, carry);
            logic(cpu, rd, res, sf(setflags))
        }
        Instr::ShiftReg {
            rd,
            rn,
            rm,
            ty,
            setflags,
        } => {
            let amount = cpu.reg(rm) & 0xFF;
            let res = shift_c(cpu.reg(rn), ty, amount, carry);
            logic(cpu, rd, res, sf(setflags))
        }
        Instr::AddImm {
            rd,
            rn,
            imm,
            setflags,
            ..
        } => {
            let res = add_with_carry(cpu.reg(rn), imm, false);
            arith(cpu, rd, res, sf(setflags))
        }
        Instr::SubImm {
            rd,
            rn,
            imm,
            setflags,
            ..
        } => {
            let res = add_with_carry(cpu.reg(rn), !imm, true);
            arith(cpu, rd, res, sf(setflags))
        }
        Instr::RsbImm {
            rd,
            rn,
            imm,
            setflags,
        } => {
            let res = add_with_carry(!cpu.reg(rn), imm, true);
            arith(cpu, rd, res, sf(setflags))
        }
        Instr::AddReg {
            rd,
            rn,
            rm,
            shift,
            setflags,
        } => {
            let (op2, _) = shifted(cpu, rm, shift);
            let res = add_with_carry(cpu.reg(rn), op2, false);
            arith(cpu, rd, res, sf(setflags))
        }
        Instr::SubReg {
            rd,
            rn,
            rm,
            shift,
            setflags,
        } => {
            let (op2, _) = shifted(cpu, rm, shift);
            let res = add_with_carry(cpu.reg(rn), !op2, true);
            arith(cpu, rd, res, sf(setflags))
        }
        Instr::AluReg {
            op,
            rd,
            rn,
            rm,
            shift,
            setflags,
        } => {
            let (op2, c) = shifted(cpu, rm, shift);
            let n = cpu.reg(rn);
            match op {
                AluOp::And => logic(cpu, rd, (n & op2, c), sf(setflags)),
                AluOp::Eor => logic(cpu, rd, (n ^ op2, c), sf(setflags)),
                AluOp::Orr => logic(cpu, rd, (n | op2, c), sf(setflags)),
                AluOp::Orn => logic(cpu, rd, (n | !op2, c), sf(setflags)),
                AluOp::Bic => logic(cpu, rd, (n & !op2, c), sf(setflags)),
                AluOp::Adc => {
                    let res = add_with_carry(n, op2, carry);
                    arith(cpu, rd, res, sf(setflags))
                }
                AluOp::Sbc => {
                    let res = add_with_carry(n, !op2, carry);
                    arith(cpu, rd, res, sf(setflags))
                }
            }
        }
        Instr::Mul {
            rd,
            rn,
            rm,
            setflags,
        } => {
            let v = cpu.reg(rn).wrapping_mul(cpu.reg(rm));
            if sf(setflags) {
                set_nz(cpu, v);
            }
            write_rd(cpu, rd, v)
        }
        Instr::CmpImm { rn, imm } => {
            let res = add_with_carry(cpu.reg(rn), !imm, true);
            set_nzcv(cpu, res);
            Flow::Next
        }
        Instr::CmpReg { rn, rm, shift } => {
            let (op2, _) = shifted(cpu, rm, shift);
            let res = add_with_carry(cpu.reg(rn), !op2, true);
            set_nzcv(cpu, res);
            Flow::Next
        }
        Instr::CmnReg { rn, rm, shift } => {
            let (op2, _) = shifted(cpu, rm, shift);
            let res = add_with_carry(cpu.reg(rn), op2, false);
            set_nzcv(cpu, res);
            Flow::Next
        }
        Instr::TstReg { rn, rm, shift } => {
            let (op2, c) = shifted(cpu, rm, shift);
            set_nz(cpu, cpu.reg(rn) & op2);
            cpu.regs.xpsr.c = c;
            Flow::Next
        }
        Instr::Adr { rd, imm } => {
            let base = cpu.reg(15) & !3;
            write_rd(cpu, rd, base.wrapping_add(imm))
        }
        Instr::Extend {
            rd,
            rm,
            rotate,
            signed,
            half,
        } => {
            let v = cpu.reg(rm).rotate_right(u32::from(rotate));
            let r = match (half, signed) {
                (true, true) => v as u16 as i16 as i32 as u32,
                (true, false) => v & 0xFFFF,
                (false, true) => v as u8 as i8 as i32 as u32,
                (false, false) => v & 0xFF,
            };
            cpu.write_reg(rd, r);
            Flow::Next
        }
        Instr::Rev { rd, rm, kind } => {
            let v = cpu.reg(rm);
            let r = match kind {
                RevKind::Rev => v.swap_bytes(),
                RevKind::Rev16 => ((v & 0x00FF_00FF) << 8) | ((v >> 8) & 0x00FF_00FF),
                RevKind::Revsh => (v as u16).swap_bytes() as i16 as i32 as u32,
            };
            cpu.write_reg(rd, r);
            Flow::Next
        }
        Instr::RsbReg {
            rd,
            rn,
            rm,
            shift,
            setflags,
        } => {
            let (op2, _) = shifted(cpu, rm, shift);
            let res = add_with_carry(!cpu.reg(rn), op2, true);
            arith(cpu, rd, res, sf(setflags))
        }
        Instr::AluImm {
            op,
            rd,
            rn,
            imm,
            carry: c,
            setflags,
        } => {
            let n = cpu.reg(rn);
            let c = c.unwrap_or(carry);
            match op {
                AluOp::And => logic(cpu, rd, (n & imm, c), sf(setflags)),
                AluOp::Eor => logic(cpu, rd, (n ^ imm, c), sf(setflags)),
                AluOp::Orr => logic(cpu, rd, (n | imm, c), sf(setflags)),
                AluOp::Orn => logic(cpu, rd, (n | !imm, c), sf(setflags)),
                AluOp::Bic => logic(cpu, rd, (n & !imm, c), sf(setflags)),
                AluOp::Adc => {
                    let res = add_with_carry(n, imm, carry);
                    arith(cpu, rd, res, sf(setflags))
                }
                AluOp::Sbc => {
                    let res = add_with_carry(n, !imm, carry);
                    arith(cpu, rd, res, sf(setflags))
                }
            }
        }
        Instr::CmnImm { rn, imm } => {
            let res = add_with_carry(cpu.reg(rn), imm, false);
            set_nzcv(cpu, res);
            Flow::Next
        }
        Instr::TstImm { rn, imm, carry: c } => {
            set_nz(cpu, cpu.reg(rn) & imm);
            cpu.regs.xpsr.c = c.unwrap_or(carry);
            Flow::Next
        }
        Instr::TeqImm { rn, imm, carry: c } => {
            set_nz(cpu, cpu.reg(rn) ^ imm);
            cpu.regs.xpsr.c = c.unwrap_or(carry);
            Flow::Next
        }
        Instr::TeqReg { rn, rm, shift } => {
            let (op2, c) = shifted(cpu, rm, shift);
            set_nz(cpu, cpu.reg(rn) ^ op2);
            cpu.regs.xpsr.c = c;
            Flow::Next
        }
        Instr::ExtendAdd {
            rd,
            rn,
            rm,
            rotate,
            signed,
            half,
        } => {
            let v = cpu.reg(rm).rotate_right(u32::from(rotate));
            let ext = match (half, signed) {
                (true, true) => v as u16 as i16 as i32 as u32,
                (true, false) => v & 0xFFFF,
                (false, true) => v as u8 as i8 as i32 as u32,
                (false, false) => v & 0xFF,
            };
            cpu.write_reg(rd, cpu.reg(rn).wrapping_add(ext));
            Flow::Next
        }
        Instr::Clz { rd, rm } => {
            cpu.write_reg(rd, cpu.reg(rm).leading_zeros());
            Flow::Next
        }
        Instr::Rbit { rd, rm } => {
            cpu.write_reg(rd, cpu.reg(rm).reverse_bits());
            Flow::Next
        }
        Instr::Sel { rd, rn, rm } => {
            let (n, m, ge) = (cpu.reg(rn), cpu.reg(rm), cpu.regs.xpsr.ge);
            let mut r = 0;
            for i in 0..4 {
                let mask = 0xFFu32 << (8 * i);
                r |= if ge & (1 << i) != 0 {
                    n & mask
                } else {
                    m & mask
                };
            }
            cpu.write_reg(rd, r);
            Flow::Next
        }
        Instr::Pkh {
            rd,
            rn,
            rm,
            tb,
            amount,
        } => {
            let (n, m) = (cpu.reg(rn), cpu.reg(rm));
            let r = if tb {
                let (op2, _) = shift_c(m, SRType::Asr, amount, false);
                (n & 0xFFFF_0000) | (op2 & 0xFFFF)
            } else {
                let (op2, _) = shift_c(m, SRType::Lsl, amount, false);
                (op2 & 0xFFFF_0000) | (n & 0xFFFF)
            };
            cpu.write_reg(rd, r);
            Flow::Next
        }
        Instr::Ssat {
            rd,
            rn,
            bits,
            shift,
        } => {
            let (v, _) = shifted(cpu, rn, shift);
            let (max, min) = ((1i64 << (bits - 1)) - 1, -(1i64 << (bits - 1)));
            saturate(cpu, rd, i64::from(v as i32), min, max)
        }
        Instr::Usat {
            rd,
            rn,
            bits,
            shift,
        } => {
            let (v, _) = shifted(cpu, rn, shift);
            saturate(cpu, rd, i64::from(v as i32), 0, (1i64 << bits) - 1)
        }
        Instr::Bfx {
            rd,
            rn,
            lsb,
            width,
            signed,
        } => {
            // Shift the field to the top, then back down (arithmetic for SBFX).
            let up = cpu.reg(rn) << (32 - u32::from(lsb) - u32::from(width));
            let down = 32 - u32::from(width);
            let r = if signed {
                ((up as i32) >> down) as u32
            } else {
                up >> down
            };
            cpu.write_reg(rd, r);
            Flow::Next
        }
        Instr::Bfi { rd, rn, lsb, width } => {
            let mask = field_mask(width) << lsb;
            let r = (cpu.reg(rd) & !mask) | ((cpu.reg(rn) << lsb) & mask);
            cpu.write_reg(rd, r);
            Flow::Next
        }
        Instr::Bfc { rd, lsb, width } => {
            let mask = field_mask(width) << lsb;
            cpu.write_reg(rd, cpu.reg(rd) & !mask);
            Flow::Next
        }
        Instr::MulAcc {
            rd,
            rn,
            rm,
            ra,
            sub,
        } => {
            let p = cpu.reg(rn).wrapping_mul(cpu.reg(rm));
            let a = cpu.reg(ra);
            cpu.write_reg(
                rd,
                if sub {
                    a.wrapping_sub(p)
                } else {
                    a.wrapping_add(p)
                },
            );
            Flow::Next
        }
        Instr::LongMul {
            kind,
            rdlo,
            rdhi,
            rn,
            rm,
        } => {
            let (n, m) = (cpu.reg(rn), cpu.reg(rm));
            let (lo, hi) = (u64::from(cpu.reg(rdlo)), u64::from(cpu.reg(rdhi)));
            let acc = (hi << 32) | lo;
            let signed = (i64::from(n as i32) * i64::from(m as i32)) as u64;
            let unsigned = u64::from(n) * u64::from(m);
            let r = match kind {
                LongMulKind::Smull => signed,
                LongMulKind::Umull => unsigned,
                LongMulKind::Smlal => signed.wrapping_add(acc),
                LongMulKind::Umlal => unsigned.wrapping_add(acc),
                LongMulKind::Umaal => unsigned + lo + hi,
            };
            cpu.write_reg(rdlo, r as u32);
            cpu.write_reg(rdhi, (r >> 32) as u32);
            Flow::Next
        }
        Instr::Div { rd, rn, rm, signed } => {
            let (n, m) = (cpu.reg(rn), cpu.reg(rm));
            if m == 0 && cpu.scs.scb.ccr.get(cpu.regs.sec) & CCR_DIV_0_TRP != 0 {
                return Some(Flow::Fault(Fault::DivByZero));
            }
            let r = match (m, signed) {
                (0, _) => 0,
                (_, true) => (n as i32).wrapping_div(m as i32) as u32,
                (_, false) => n / m,
            };
            cpu.write_reg(rd, r);
            Flow::Next
        }
        _ => return None,
    };
    Some(flow)
}
