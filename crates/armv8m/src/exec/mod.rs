//! Instruction execution.

mod alu;

use crate::bus::Bus;
use crate::cpu::Cpu;
use crate::instr::Instr;

/// Whether an instruction left PC for the caller to advance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    /// Fall through to the next instruction.
    Next,
    /// PC was written by the instruction.
    Branched,
}

/// Execute `instr`. `suppress_flags` is true for 16-bit encodings inside an IT block.
pub(crate) fn execute(
    cpu: &mut Cpu,
    _bus: &mut impl Bus,
    instr: &Instr,
    suppress_flags: bool,
) -> Flow {
    match *instr {
        Instr::MovImm { .. }
        | Instr::MovReg { .. }
        | Instr::Mvn { .. }
        | Instr::ShiftImm { .. }
        | Instr::ShiftReg { .. }
        | Instr::AddImm { .. }
        | Instr::SubImm { .. }
        | Instr::RsbImm { .. }
        | Instr::AddReg { .. }
        | Instr::SubReg { .. }
        | Instr::AluReg { .. }
        | Instr::Mul { .. }
        | Instr::CmpImm { .. }
        | Instr::CmpReg { .. }
        | Instr::CmnReg { .. }
        | Instr::TstReg { .. }
        | Instr::Adr { .. } => alu::execute(cpu, instr, suppress_flags).unwrap_or(Flow::Next),
        Instr::Bx { rm } => {
            let target = cpu.reg(rm);
            cpu.bx_write_pc(target);
            Flow::Branched
        }
        Instr::Blx { rm } => {
            let target = cpu.reg(rm);
            cpu.regs.lr = cpu.regs.pc.wrapping_add(2) | 1;
            cpu.bx_write_pc(target);
            Flow::Branched
        }
        Instr::It { firstcond, mask } => {
            cpu.regs.xpsr.it = (firstcond << 4) | mask;
            Flow::Next
        }
        // Intercepted in `Cpu::step` before execution.
        Instr::Undefined { .. } => Flow::Next,
    }
}
