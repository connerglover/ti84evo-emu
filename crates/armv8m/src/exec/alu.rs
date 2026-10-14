//! Data-processing instructions.

use super::Flow;
use crate::bits::{add_with_carry, shift_c};
use crate::cpu::Cpu;
use crate::instr::{AluOp, Instr, Shift};

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
        cpu.set_reg(rd, v);
        Flow::Next
    }
}

/// Finish an add/subtract style operation producing `(result, carry, overflow)`.
fn arith(cpu: &mut Cpu, rd: u8, res: (u32, bool, bool), setflags: bool) -> Flow {
    if setflags {
        set_nzcv(cpu, res);
    }
    write_rd(cpu, rd, res.0)
}

/// Finish a logical operation producing `(result, carry)`.
fn logic(cpu: &mut Cpu, rd: u8, (v, c): (u32, bool), setflags: bool) -> Flow {
    if setflags {
        set_nz(cpu, v);
        cpu.regs.xpsr.c = c;
    }
    write_rd(cpu, rd, v)
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
        Instr::MovImm { rd, imm, setflags } => {
            let c = carry;
            logic(cpu, rd, (imm, c), sf(setflags))
        }
        Instr::MovReg { rd, rm, setflags } => {
            let v = cpu.reg(rm);
            if rd == 15 {
                cpu.bx_write_pc(v);
                Flow::Branched
            } else {
                logic(cpu, rd, (v, carry), sf(setflags))
            }
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
        } => {
            let res = add_with_carry(cpu.reg(rn), imm, false);
            arith(cpu, rd, res, sf(setflags))
        }
        Instr::SubImm {
            rd,
            rn,
            imm,
            setflags,
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
        _ => return None,
    };
    Some(flow)
}
