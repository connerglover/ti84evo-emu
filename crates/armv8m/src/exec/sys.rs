//! System, hint and IT instructions.

use super::Flow;
use crate::cpu::Cpu;
use crate::exception::Fault;
use crate::instr::{HintOp, Instr};
use crate::regs::{Registers, Sec};
use crate::scs::PRIORITY_MASK;
use crate::scs::sau::Attr;

/// `CPACR.CPn` grants access: 0b01 privileged only, 0b11 full.
fn cp_enabled(cpu: &Cpu, cp: u32) -> bool {
    match cpu.scs.scb.cpacr.get(cpu.regs.sec) >> (2 * cp) & 3 {
        0b01 => cpu.is_privileged(),
        0b11 => true,
        _ => false,
    }
}

/// APSR bits visible through `MRS`/`MSR`: N, Z, C, V, Q and GE[3:0].
const APSR_NZCVQ: u32 = 0xF800_0000;
const APSR_GE: u32 = 0x000F_0000;

/// `SP_NS`: the Non-secure stack pointer of the current mode (Secure state only).
const SYSM_SP_NS: u8 = 0x98;

/// Resolve a SYSm value to the bank it addresses and its unbanked number. The `_NS` aliases
/// (0x88..=0x94 and SP_NS, 0x98) reach the Non-Secure bank from Secure state and do not exist
/// otherwise; everything numbered 8 and up is privileged-only except reads of CONTROL.
fn resolve_sysm(cpu: &Cpu, sysm: u8, write: bool) -> Option<(Sec, u8)> {
    let sec = cpu.regs.sec;
    let (bank, base) = if sysm & 0x80 != 0 {
        if sec != Sec::Secure || !((0x88..=0x94).contains(&sysm) || sysm == SYSM_SP_NS) {
            return None;
        }
        (Sec::NonSecure, sysm & 0x7F)
    } else {
        (sec, sysm)
    };
    // CONTROL (20) can be read, but not written, without privilege.
    (base < 8 || cpu.is_privileged() || (!write && base == 20)).then_some((bank, base))
}

/// Whether `SP_NS`, the Non-secure stack pointer of the current mode, is PSP_NS.
fn sp_ns_is_psp(r: &Registers) -> bool {
    !r.handler_mode() && r.control.ns & 0b10 != 0
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
        _ if sysm == SYSM_SP_NS && sp_ns_is_psp(r) => r.psp.ns,
        _ if sysm == SYSM_SP_NS => r.msp.ns,
        _ => 0,
    }
}

/// `MSR`: writes to unknown, inaccessible or read-only registers are ignored.
fn write_sysreg(cpu: &mut Cpu, sysm: u8, mask: u8, v: u32) {
    let Some((bank, base)) = resolve_sysm(cpu, sysm, true) else {
        return;
    };
    let handler = cpu.regs.handler_mode();
    let exec_priority = if base == 19 {
        cpu.execution_priority()
    } else {
        0
    };
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
        // Only the implemented priority bits of BASEPRI exist.
        17 => *r.basepri.get_mut(bank) = v as u8 & PRIORITY_MASK,
        // BASEPRI_MAX only ever raises the priority (lowers the number); zero is ignored.
        18 => {
            let v = v as u8 & PRIORITY_MASK;
            let cur = r.basepri.get_mut(bank);
            if v != 0 && (*cur == 0 || v < *cur) {
                *cur = v;
            }
        }
        // FAULTMASK can only be set below execution priority -1 (not in NMI or HardFault).
        19 => {
            if v & 1 == 0 || exec_priority > -1 {
                *r.faultmask.get_mut(bank) = v & 1 != 0;
            }
        }
        20 => {
            let old = *r.control.get(bank);
            // nPRIV and SPSEL are banked. Without an FPU, FPCA (which is not banked) and SFPA
            // are RES0. SPSEL is read-only in Handler mode.
            let mut new = v & 0b11;
            if handler {
                new = (new & !2) | (old & 2);
            }
            *r.control.get_mut(bank) = new;
        }
        _ if sysm == SYSM_SP_NS && sp_ns_is_psp(r) => r.psp.ns = v & !3,
        _ if sysm == SYSM_SP_NS => r.msp.ns = v & !3,
        _ => {}
    }
}

/// The `TT` response word for `addr`. Secure state sees the security attribution (S, SAU and
/// IDAU regions). The MPU fields (R/RW, MREGION/MRVALID, and so NSR/NSRW) are reported only to
/// privileged software or for `alt` (TTA/TTAT), and are zero otherwise. They come from the
/// current security state's MPU, or the Non-secure one for `alt`, at the privilege of the
/// target state's current mode, or unprivileged for `unpriv` (TTT/TTAT).
fn tt_response(cpu: &Cpu, addr: u32, alt: bool, unpriv: bool) -> u32 {
    const MRVALID: u32 = 1 << 16;
    const SRVALID: u32 = 1 << 17;
    const R: u32 = 1 << 18;
    const RW: u32 = 1 << 19;
    const NSR: u32 = 1 << 20;
    const NSRW: u32 = 1 << 21;
    const S: u32 = 1 << 22;
    const IRVALID: u32 = 1 << 23;

    let secure = cpu.regs.sec == Sec::Secure;
    let mpu_sec = match (alt, cpu.regs.sec) {
        (false, sec) => sec,
        (true, Sec::Secure) => Sec::NonSecure,
        (true, Sec::NonSecure) => Sec::Secure,
    };
    let mut resp = 0;
    let mut addr_secure = false;
    if secure {
        let a = cpu.attribution(addr);
        if let Some(r) = a.sregion {
            resp |= (u32::from(r) << 8) | SRVALID;
        }
        if let Some(r) = a.iregion {
            resp |= (u32::from(r) << 24) | IRVALID;
        }
        // Exempt memory is accessed in the security state being queried.
        addr_secure = match a.attr {
            Attr::NonSecure => false,
            Attr::Exempt => mpu_sec == Sec::Secure,
            Attr::Secure | Attr::NonSecureCallable => true,
        };
        if addr_secure {
            resp |= S;
        }
    }
    if !(cpu.is_privileged() || alt) {
        return resp;
    }
    let privileged = !unpriv && (cpu.regs.handler_mode() || cpu.regs.control.get(mpu_sec) & 1 == 0);
    let negative = cpu.execution_priority() < 0;
    let (read, write, region) = cpu
        .scs
        .mpu
        .get(mpu_sec)
        .permissions(addr, privileged, negative);
    if let Some(r) = region {
        resp |= u32::from(r) | MRVALID;
    }
    if read {
        resp |= R;
        if secure && !addr_secure {
            resp |= NSR;
        }
    }
    if write {
        resp |= RW;
        if secure && !addr_secure {
            resp |= NSRW;
        }
    }
    resp
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
                // CPSID f has no effect at execution priority -1 or higher.
                if f && (!disable || cpu.execution_priority() > -1) {
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
        // SG from Non-secure state (only reachable in a Non-secure callable region: anywhere
        // else in Secure memory the fetch faults INVEP) enters Secure state and clears LR[0],
        // so that the Secure function returns to Non-secure state with BXNS LR. Elsewhere,
        // and in Secure state, it is a NOP.
        Instr::Sg => {
            if cpu.regs.sec == Sec::NonSecure
                && cpu.security_of(cpu.regs.pc) == Attr::NonSecureCallable
            {
                cpu.regs.lr &= !1;
                cpu.regs.sec = Sec::Secure;
            }
            Flow::Next
        }
        // TTA/TTAT are UNDEFINED in Non-secure state.
        Instr::Tt { alt: true, .. } if cpu.regs.sec != Sec::Secure => {
            Flow::Fault(Fault::UndefInstr)
        }
        Instr::Tt {
            rd,
            rn,
            alt,
            unpriv,
        } => {
            let v = tt_response(cpu, cpu.reg(rn), alt, unpriv);
            cpu.set_reg(rd, v);
            Flow::Next
        }
        // Single in-order core: barriers and preloads have no effect.
        Instr::Barrier { .. } | Instr::Preload { .. } => Flow::Next,
        // No coprocessor is implemented. CP10/CP11 (the FPU) enabled in CPACR would mean the
        // firmware expects an FPU: report that as UNDEFINSTR; otherwise NOCP.
        Instr::Coprocessor { raw } => {
            let cp = (raw >> 8) & 0xF;
            if matches!(cp, 10 | 11) && cp_enabled(cpu, cp) {
                log::warn!(
                    "FPU instruction {raw:#010x} at {:#010x}: FPU not implemented",
                    cpu.regs.pc
                );
                Flow::Fault(Fault::UndefInstr)
            } else {
                log::warn!(
                    "coprocessor instruction {raw:#010x} at {:#010x}: NOCP",
                    cpu.regs.pc
                );
                Flow::Fault(Fault::NoCp)
            }
        }
        _ => return None,
    };
    Some(flow)
}
