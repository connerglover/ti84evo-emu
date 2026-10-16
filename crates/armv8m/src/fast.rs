//! Compact lowering for common instructions outside IT blocks. Every destination is an
//! unbanked r0-r12 register; unusual operands retain the full interpreter path.

use crate::bits::{SRType, add_with_carry, condition_holds, shift_c};
use crate::bus::Bus;
use crate::cpu::Cpu;
use crate::exec::{self, Flow};
use crate::instr::{AluOp, Instr, MemSize, Shift};

#[derive(Clone, Copy, Debug)]
enum Op {
    Mov {
        rd: u8,
        imm: u32,
        flags: bool,
        carry: Option<bool>,
    },
    MoveReg {
        rd: u8,
        rm: u8,
        flags: bool,
    },
    Add {
        rd: u8,
        rn: u8,
        imm: u32,
        flags: bool,
    },
    Sub {
        rd: u8,
        rn: u8,
        imm: u32,
        flags: bool,
    },
    AddReg {
        rd: u8,
        rn: u8,
        rm: u8,
        flags: bool,
    },
    SubReg {
        rd: u8,
        rn: u8,
        rm: u8,
        flags: bool,
    },
    Shift {
        rd: u8,
        rm: u8,
        ty: SRType,
        amount: u32,
        flags: bool,
    },
    Lsl {
        rd: u8,
        rm: u8,
        amount: u32,
        flags: bool,
    },
    And {
        rd: u8,
        rn: u8,
        rm: u8,
        flags: bool,
    },
    Eor {
        rd: u8,
        rn: u8,
        rm: u8,
        flags: bool,
    },
    Orr {
        rd: u8,
        rn: u8,
        rm: u8,
        flags: bool,
    },
    Orn {
        rd: u8,
        rn: u8,
        rm: u8,
        flags: bool,
    },
    Bic {
        rd: u8,
        rn: u8,
        rm: u8,
        flags: bool,
    },
    Adc {
        rd: u8,
        rn: u8,
        rm: u8,
        flags: bool,
    },
    Sbc {
        rd: u8,
        rn: u8,
        rm: u8,
        flags: bool,
    },
    Compare {
        rn: u8,
        imm: u32,
    },
    CompareReg {
        rn: u8,
        rm: u8,
    },
    Mul {
        rd: u8,
        rn: u8,
        rm: u8,
        flags: bool,
    },
    Load {
        rt: u8,
        rn: u8,
        imm: u32,
        size: MemSize,
        signed: bool,
    },
    Store {
        rt: u8,
        rn: u8,
        imm: u32,
        size: MemSize,
    },
    Jump {
        target: u32,
    },
    Branch {
        target: u32,
        cond: u8,
    },
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Entry {
    pub pc: u32,
    pub next: u32,
    op: Op,
}

/// Every register operand in private Op is admitted only by Entry::lower's r0-r12
/// checks. Keep this one unchecked access behind that invariant; malformed/other operands
/// are never lowered and continue through the normal architectural register accessors.
#[inline(always)]
fn reg(r: &mut [u32; 13], index: u8) -> &mut u32 {
    debug_assert!(index < 13);
    // SAFETY: Op cannot be constructed outside this module, and lower checks every operand.
    unsafe { r.get_unchecked_mut(index as usize) }
}

#[inline]
fn add_sub(a: u32, b: u32, subtract: bool) -> (u32, bool, bool) {
    if subtract {
        let (value, overflow) = (a as i32).overflowing_sub(b as i32);
        (value as u32, a >= b, overflow)
    } else {
        let (value, overflow) = (a as i32).overflowing_add(b as i32);
        (value as u32, (value as u32) < a, overflow)
    }
}

impl Entry {
    pub fn lower(instr: Instr, pc: u32, len: u8) -> Option<Self> {
        let op = match instr {
            Instr::MovImm {
                rd,
                imm,
                setflags,
                carry,
            } if rd < 13 => Op::Mov {
                rd,
                imm,
                flags: setflags,
                carry,
            },
            Instr::Movw { rd, imm } if rd < 13 => Op::Mov {
                rd,
                imm: u32::from(imm),
                flags: false,
                carry: None,
            },
            Instr::MovReg { rd, rm, setflags } if rd < 13 && rm < 13 => Op::MoveReg {
                rd,
                rm,
                flags: setflags,
            },
            Instr::AddImm {
                rd,
                rn,
                imm,
                setflags,
                ..
            } if rd < 13 && rn < 13 => Op::Add {
                rd,
                rn,
                imm,
                flags: setflags,
            },
            Instr::SubImm {
                rd,
                rn,
                imm,
                setflags,
                ..
            } if rd < 13 && rn < 13 => Op::Sub {
                rd,
                rn,
                imm,
                flags: setflags,
            },
            Instr::AddReg {
                rd,
                rn,
                rm,
                shift,
                setflags,
            } if rd < 13 && rn < 13 && rm < 13 && shift == Shift::NONE => Op::AddReg {
                rd,
                rn,
                rm,
                flags: setflags,
            },
            Instr::SubReg {
                rd,
                rn,
                rm,
                shift,
                setflags,
            } if rd < 13 && rn < 13 && rm < 13 && shift == Shift::NONE => Op::SubReg {
                rd,
                rn,
                rm,
                flags: setflags,
            },
            Instr::ShiftImm {
                rd,
                rm,
                ty,
                amount,
                setflags,
            } if rd < 13 && rm < 13 && ty == SRType::Lsl && (1..32).contains(&amount) => Op::Lsl {
                rd,
                rm,
                amount,
                flags: setflags,
            },
            Instr::ShiftImm {
                rd,
                rm,
                ty,
                amount,
                setflags,
            } if rd < 13 && rm < 13 => Op::Shift {
                rd,
                rm,
                ty,
                amount,
                flags: setflags,
            },
            Instr::AluReg {
                rd,
                rn,
                rm,
                op,
                shift,
                setflags,
            } if rd < 13 && rn < 13 && rm < 13 && shift == Shift::NONE => match op {
                AluOp::And => Op::And {
                    rd,
                    rn,
                    rm,
                    flags: setflags,
                },
                AluOp::Eor => Op::Eor {
                    rd,
                    rn,
                    rm,
                    flags: setflags,
                },
                AluOp::Orr => Op::Orr {
                    rd,
                    rn,
                    rm,
                    flags: setflags,
                },
                AluOp::Orn => Op::Orn {
                    rd,
                    rn,
                    rm,
                    flags: setflags,
                },
                AluOp::Bic => Op::Bic {
                    rd,
                    rn,
                    rm,
                    flags: setflags,
                },
                AluOp::Adc => Op::Adc {
                    rd,
                    rn,
                    rm,
                    flags: setflags,
                },
                AluOp::Sbc => Op::Sbc {
                    rd,
                    rn,
                    rm,
                    flags: setflags,
                },
            },
            Instr::CmpImm { rn, imm } if rn < 13 => Op::Compare { rn, imm },
            Instr::CmpReg { rn, rm, shift } if rn < 13 && rm < 13 && shift == Shift::NONE => {
                Op::CompareReg { rn, rm }
            }
            Instr::Mul {
                rd,
                rn,
                rm,
                setflags,
            } if rd < 13 && rn < 13 && rm < 13 => Op::Mul {
                rd,
                rn,
                rm,
                flags: setflags,
            },
            Instr::LoadImm {
                rt,
                rn,
                imm,
                size,
                signed,
                index: true,
                add,
                wback: false,
            } if rt < 13 && rn < 13 => Op::Load {
                rt,
                rn,
                imm: if add { imm } else { imm.wrapping_neg() },
                size,
                signed,
            },
            Instr::StoreImm {
                rt,
                rn,
                imm,
                size,
                index: true,
                add,
                wback: false,
            } if rt < 13 && rn < 13 => Op::Store {
                rt,
                rn,
                imm: if add { imm } else { imm.wrapping_neg() },
                size,
            },
            Instr::B { imm } => Op::Jump {
                target: pc.wrapping_add(4).wrapping_add(imm as u32) & !1,
            },
            Instr::BCond { imm, cond } => Op::Branch {
                target: pc.wrapping_add(4).wrapping_add(imm as u32) & !1,
                cond,
            },
            _ => return None,
        };
        Some(Self {
            pc,
            next: pc.wrapping_add(u32::from(len)),
            op,
        })
    }

    pub fn ends_block(&self) -> bool {
        matches!(self.op, Op::Branch { .. } | Op::Jump { .. })
    }

    /// The caller guarantees Thumb state, no IT block and valid cached fetch attribution.
    /// Memory still passes through the ordinary access checks and bus implementation.
    #[inline(always)]
    pub fn execute(self, cpu: &mut Cpu, bus: &mut impl Bus) -> Flow {
        use Op::*;
        let r = &mut cpu.regs.r;
        let x = &mut cpu.regs.xpsr;
        let nz = |x: &mut crate::regs::Xpsr, value: u32| {
            x.n = value >> 31 != 0;
            x.z = value == 0;
        };
        let arith = |x: &mut crate::regs::Xpsr,
                     (value, carry, overflow): (u32, bool, bool),
                     flags: bool| {
            if flags {
                nz(x, value);
                x.c = carry;
                x.v = overflow;
            }
            value
        };
        match self.op {
            Mov {
                rd,
                imm,
                flags,
                carry,
            } => {
                *reg(r, rd) = imm;
                if flags {
                    nz(x, imm);
                    if let Some(c) = carry {
                        x.c = c;
                    }
                }
            }
            MoveReg { rd, rm, flags } => {
                let value = *reg(r, rm);
                *reg(r, rd) = value;
                if flags {
                    nz(x, value);
                }
            }
            Add { rd, rn, imm, flags } => {
                let value = add_sub(*reg(r, rn), imm, false);
                *reg(r, rd) = arith(x, value, flags);
            }
            Sub { rd, rn, imm, flags } => {
                let value = add_sub(*reg(r, rn), imm, true);
                *reg(r, rd) = arith(x, value, flags);
            }
            AddReg { rd, rn, rm, flags } => {
                let value = add_sub(*reg(r, rn), *reg(r, rm), false);
                *reg(r, rd) = arith(x, value, flags);
            }
            SubReg { rd, rn, rm, flags } => {
                let value = add_sub(*reg(r, rn), *reg(r, rm), true);
                *reg(r, rd) = arith(x, value, flags);
            }
            Shift {
                rd,
                rm,
                ty,
                amount,
                flags,
            } => {
                let (value, carry) = shift_c(*reg(r, rm), ty, amount, x.c);
                *reg(r, rd) = value;
                if flags {
                    nz(x, value);
                    x.c = carry;
                }
            }
            Lsl {
                rd,
                rm,
                amount,
                flags,
            } => {
                let source = *reg(r, rm);
                let value = source << amount;
                *reg(r, rd) = value;
                if flags {
                    nz(x, value);
                    x.c = source >> (32 - amount) & 1 != 0;
                }
            }
            And { rd, rn, rm, flags } => {
                let (a, b) = (*reg(r, rn), *reg(r, rm));
                let value = a & b;
                *reg(r, rd) = value;
                if flags {
                    nz(x, value);
                }
            }
            Eor { rd, rn, rm, flags } => {
                let (a, b) = (*reg(r, rn), *reg(r, rm));
                let value = a ^ b;
                *reg(r, rd) = value;
                if flags {
                    nz(x, value);
                }
            }
            Orr { rd, rn, rm, flags } => {
                let (a, b) = (*reg(r, rn), *reg(r, rm));
                let value = a | b;
                *reg(r, rd) = value;
                if flags {
                    nz(x, value);
                }
            }
            Orn { rd, rn, rm, flags } => {
                let (a, b) = (*reg(r, rn), *reg(r, rm));
                let value = a | !b;
                *reg(r, rd) = value;
                if flags {
                    nz(x, value);
                }
            }
            Bic { rd, rn, rm, flags } => {
                let (a, b) = (*reg(r, rn), *reg(r, rm));
                let value = a & !b;
                *reg(r, rd) = value;
                if flags {
                    nz(x, value);
                }
            }
            Adc { rd, rn, rm, flags } => {
                let (a, b) = (*reg(r, rn), *reg(r, rm));
                let value = arith(x, add_with_carry(a, b, x.c), flags);
                *reg(r, rd) = value;
                if flags {
                    nz(x, value);
                }
            }
            Sbc { rd, rn, rm, flags } => {
                let (a, b) = (*reg(r, rn), *reg(r, rm));
                let value = arith(x, add_with_carry(a, !b, x.c), flags);
                *reg(r, rd) = value;
                if flags {
                    nz(x, value);
                }
            }
            Compare { rn, imm } => {
                arith(x, add_sub(*reg(r, rn), imm, true), true);
            }
            CompareReg { rn, rm } => {
                arith(x, add_sub(*reg(r, rn), *reg(r, rm), true), true);
            }
            Mul { rd, rn, rm, flags } => {
                let value = (*reg(r, rn)).wrapping_mul(*reg(r, rm));
                *reg(r, rd) = value;
                if flags {
                    nz(x, value);
                }
            }
            Load {
                rt,
                rn,
                imm,
                size,
                signed,
            } => {
                let addr = (*reg(r, rn)).wrapping_add(imm);
                match exec::simple_read(cpu, bus, addr, size, signed) {
                    Ok(value) => *reg(&mut cpu.regs.r, rt) = value,
                    Err(flow) => return flow,
                }
            }
            Store { rt, rn, imm, size } => {
                let addr = (*reg(r, rn)).wrapping_add(imm);
                let value = *reg(r, rt);
                if let Err(flow) = exec::simple_write(cpu, bus, addr, value, size) {
                    return flow;
                }
            }
            Jump { target } => {
                cpu.regs.pc = target;
                return Flow::Branched;
            }
            Branch { target, cond } => {
                if condition_holds(cond, x) {
                    cpu.regs.pc = target;
                    return Flow::Branched;
                }
            }
        }
        cpu.regs.pc = self.next;
        Flow::Next
    }
}
