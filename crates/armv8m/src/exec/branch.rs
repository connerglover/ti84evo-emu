//! Branch instructions.

use super::Flow;
use crate::bits::condition_holds;
use crate::bus::Bus;
use crate::cpu::Cpu;
use crate::exception::{FNC_FRAME_SIZE, FNC_RETURN, Fault};
use crate::instr::Instr;
use crate::regs::Sec;

/// Branch to `PC + 4 + offset` (`BranchWritePC`, no interworking).
fn branch_rel(cpu: &mut Cpu, offset: u32) -> Flow {
    let target = cpu.reg(15).wrapping_add(offset);
    cpu.branch_write_pc(target);
    Flow::Branched
}

/// `BLXNS` (Secure state only). A target with bit 0 set is an ordinary BLX. Otherwise it calls
/// Non-secure code: the return address and a partial xPSR (the exception number; there is no
/// FPU, so SFPA is 0) are pushed to the current Secure stack, LR becomes FNC_RETURN, and in
/// Handler mode IPSR reads 1 so the Secure exception number is not revealed.
fn blxns(cpu: &mut Cpu, bus: &mut impl Bus, target: u32) -> Flow {
    let next = cpu.regs.pc.wrapping_add(2) | 1;
    if target & 1 != 0 {
        cpu.regs.lr = next;
        cpu.blx_write_pc(target);
        return Flow::Branched;
    }
    let handler = cpu.regs.handler_mode();
    let limit = if handler || cpu.regs.control.s & 0b10 == 0 {
        cpu.regs.msplim.s
    } else {
        cpu.regs.psplim.s
    };
    let frame = cpu.regs.sp().wrapping_sub(FNC_FRAME_SIZE);
    if frame < limit {
        return Flow::Fault(Fault::StkOf);
    }
    let psr = u32::from(cpu.regs.xpsr.exception);
    let privileged = cpu.is_privileged();
    for (i, w) in [next, psr].into_iter().enumerate() {
        let addr = frame.wrapping_add(4 * i as u32);
        if let Err(f) = cpu.stack_write(bus, addr, w, Sec::Secure, privileged) {
            return Flow::Fault(f);
        }
    }
    cpu.regs.set_sp(frame);
    cpu.regs.lr = FNC_RETURN;
    if handler {
        cpu.regs.xpsr.exception = 1;
    }
    cpu.regs.sec = Sec::NonSecure;
    cpu.regs.xpsr.t = true;
    cpu.regs.pc = target;
    Flow::Branched
}

/// Execute a branch instruction; `None` if `instr` is not one.
pub(super) fn execute(cpu: &mut Cpu, bus: &mut impl Bus, instr: &Instr) -> Option<Flow> {
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
            cpu.bx_write_pc(target, false)
        }
        // BXNS and BLXNS are UNDEFINED in Non-secure state.
        Instr::Bxns { .. } | Instr::Blxns { .. } if cpu.regs.sec != Sec::Secure => {
            Flow::Fault(Fault::UndefInstr)
        }
        Instr::Bxns { rm } => {
            let target = cpu.reg(rm);
            cpu.bx_write_pc(target, true)
        }
        Instr::Blxns { rm } => {
            let target = cpu.reg(rm);
            blxns(cpu, bus, target)
        }
        Instr::Blx { rm } => {
            let target = cpu.reg(rm);
            cpu.regs.lr = cpu.regs.pc.wrapping_add(2) | 1;
            cpu.blx_write_pc(target);
            Flow::Branched
        }
        _ => return None,
    };
    Some(flow)
}
