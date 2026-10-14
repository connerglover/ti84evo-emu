//! System, hint and IT instructions.

use super::Flow;
use crate::cpu::Cpu;
use crate::instr::{HintOp, Instr};
use crate::regs::Sec;

/// APSR bits visible through `MRS`/`MSR`: N, Z, C, V, Q and GE[3:0].
const APSR_NZCVQ: u32 = 0xF800_0000;
const APSR_GE: u32 = 0x000F_0000;

/// Resolve a SYSm value to the bank it addresses and its unbanked number. The `_NS` aliases
/// (0x88..=0x94) reach the Non-Secure bank from Secure state and do not exist otherwise;
/// everything numbered 8 and up is privileged-only except reads of CONTROL.
fn resolve_sysm(cpu: &Cpu, sysm: u8, write: bool) -> Option<(Sec, u8)> {
    let sec = cpu.regs.sec;
    let (bank, base) = if sysm & 0x80 != 0 {
        if sec != Sec::Secure || !(0x88..=0x94).contains(&sysm) {
            return None;
        }
        (Sec::NonSecure, sysm & 0x7F)
    } else {
        (sec, sysm)
    };
    // CONTROL (20) can be read, but not written, without privilege.
    (base < 8 || cpu.is_privileged() || (!write && base == 20)).then_some((bank, base))
}

/// `MRS`: unknown, inaccessible or reserved registers read as zero.
fn read_sysreg(cpu: &Cpu, sysm: u8) -> u32 {
    let Some((bank, base)) = resolve_sysm(cpu, sysm, false) else {
        return 0;
    };
    let r = &cpu.regs;
    match base {
        // APSR / IAPSR / EAPSR / XPSR / IPSR / EPSR / IEPSR. EPSR bits read as zero.
        0..=3 | 5..=7 => {
            let apsr = if base & 4 == 0 {
                r.xpsr.to_u32() & (APSR_NZCVQ | APSR_GE)
            } else {
                0
            };
            let ipsr = if base & 1 != 0 {
                u32::from(r.xpsr.exception) & 0x1FF
            } else {
                0
            };
            apsr | ipsr
        }
        8 => *r.msp.get(bank),
        9 => *r.psp.get(bank),
        10 => *r.msplim.get(bank),
        11 => *r.psplim.get(bank),
        16 => u32::from(*r.primask.get(bank)),
        17 | 18 => u32::from(*r.basepri.get(bank)),
        19 => u32::from(*r.faultmask.get(bank)),
        20 => *r.control.get(bank),
        _ => 0,
    }
}

/// `MSR`: writes to unknown, inaccessible or read-only registers are ignored.
fn write_sysreg(cpu: &mut Cpu, sysm: u8, mask: u8, v: u32) {
    let Some((bank, base)) = resolve_sysm(cpu, sysm, true) else {
        return;
    };
    let handler = cpu.regs.handler_mode();
    let r = &mut cpu.regs;
    match base {
        // The IPSR and EPSR parts of IAPSR/EAPSR/XPSR are not writable.
        0..=3 => {
            if mask & 2 != 0 {
                r.xpsr.n = v >> 31 & 1 != 0;
                r.xpsr.z = v >> 30 & 1 != 0;
                r.xpsr.c = v >> 29 & 1 != 0;
                r.xpsr.v = v >> 28 & 1 != 0;
                r.xpsr.q = v >> 27 & 1 != 0;
            }
            if mask & 1 != 0 {
                r.xpsr.ge = (v >> 16 & 0xF) as u8;
            }
        }
        8 => *r.msp.get_mut(bank) = v & !3,
        9 => *r.psp.get_mut(bank) = v & !3,
        10 => *r.msplim.get_mut(bank) = v & !7,
        11 => *r.psplim.get_mut(bank) = v & !7,
        16 => *r.primask.get_mut(bank) = v & 1 != 0,
        17 => *r.basepri.get_mut(bank) = v as u8,
        // BASEPRI_MAX only ever raises the priority (lowers the number); zero is ignored.
        18 => {
            let cur = r.basepri.get_mut(bank);
            if v as u8 != 0 && (*cur == 0 || (v as u8) < *cur) {
                *cur = v as u8;
            }
        }
        // Task 7: FAULTMASK writes are ignored at execution priority -1 or lower.
        19 => *r.faultmask.get_mut(bank) = v & 1 != 0,
        20 => {
            let old = *r.control.get(bank);
            // nPRIV, SPSEL and FPCA; SFPA (bit 3) exists only in the Secure bank. SPSEL is
            // read-only in Handler mode.
            let keep = if bank == Sec::Secure { 0xF } else { 0x7 };
            let mut new = v & keep;
            if handler {
                new = (new & !2) | (old & 2);
            }
            *r.control.get_mut(bank) = new;
        }
        _ => {}
    }
}

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
        Instr::Msr { sysm, rn, mask } => {
            let v = cpu.reg(rn);
            write_sysreg(cpu, sysm, mask, v);
            Flow::Next
        }
        Instr::Mrs { rd, sysm } => {
            let v = read_sysreg(cpu, sysm);
            cpu.set_reg(rd, v);
            Flow::Next
        }
        // Single in-order core: barriers and preloads have no effect.
        Instr::Barrier { .. } | Instr::Preload { .. } => Flow::Next,
        // Task 7: raise UsageFault NOCP instead of reporting a generic fault.
        Instr::Coprocessor { .. } => Flow::Fault,
        _ => return None,
    };
    Some(flow)
}
