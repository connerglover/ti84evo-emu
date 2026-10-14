//! Branch instructions.

use super::Flow;
use crate::bits::condition_holds;
use crate::cpu::Cpu;
use crate::instr::Instr;

/// Branch to `PC + 4 + offset` (`BranchWritePC`, no interworking).
fn branch_rel(cpu: &mut Cpu, offset: u32) -> Flow {
    let target = cpu.reg(15).wrapping_add(offset);
    cpu.branch_write_pc(target);
    Flow::Branched
}

/// Execute a branch instruction; `None` if `instr` is not one.
pub(super) fn execute(cpu: &mut Cpu, instr: &Instr) -> Option<Flow> {
    let flow = match *instr {
        Instr::B { imm } => branch_rel(cpu, imm as u32),
        Instr::Bl { imm } => {
            cpu.regs.lr = cpu.regs.pc.wrapping_add(4) | 1;
            branch_rel(cpu, imm as u32)
        }
        Instr::BCond { cond, imm } => {
            if condition_holds(cond, &cpu.regs.xpsr) {
                branch_rel(cpu, imm as u32)
            } else {
                Flow::Next
            }
        }
        Instr::Cbz { rn, imm, nonzero } => {
            if (cpu.reg(rn) == 0) != nonzero {
                branch_rel(cpu, imm)
            } else {
                Flow::Next
            }
        }
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
        _ => return None,
    };
    Some(flow)
}
