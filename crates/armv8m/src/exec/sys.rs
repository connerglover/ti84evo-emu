//! System, hint and IT instructions.

use super::Flow;
use crate::cpu::Cpu;
use crate::instr::{HintOp, Instr};

/// Execute a system instruction; `None` if `instr` is not one.
pub(super) fn execute(cpu: &mut Cpu, instr: &Instr) -> Option<Flow> {
    let flow = match *instr {
        Instr::It { firstcond, mask } => {
            cpu.regs.xpsr.it = (firstcond << 4) | mask;
            Flow::Next
        }
        Instr::Svc { imm } => Flow::Svc(imm),
        Instr::Bkpt { imm } => Flow::Bkpt(imm),
        Instr::Cps { disable, i, f } => {
            // CPS has no effect in unprivileged code.
            if cpu.is_privileged() {
                let sec = cpu.regs.sec;
                if i {
                    *cpu.regs.primask.get_mut(sec) = disable;
                }
                if f {
                    *cpu.regs.faultmask.get_mut(sec) = disable;
                }
            }
            Flow::Next
        }
        Instr::Hint(op) => match op {
            HintOp::Nop | HintOp::Yield => Flow::Next,
            HintOp::Sev => {
                cpu.event = true;
                Flow::Next
            }
            HintOp::Wfe if cpu.event => {
                cpu.event = false;
                Flow::Next
            }
            HintOp::Wfe | HintOp::Wfi => Flow::Sleep,
        },
        _ => return None,
    };
    Some(flow)
}
