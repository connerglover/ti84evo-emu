//! Exception model: priorities, entry (stacking), return (unstacking), tail-chaining, fault
//! escalation, lockup, and the Security Extension's state transitions (secure stacking with the
//! additional state context, register clearing, `FunctionReturn`). Follows the ARMv8-M
//! pseudocode (`ExceptionEntry`, `PushStack`, `PushCalleeStack`, `ExceptionTaken`,
//! `ExceptionReturn`, `PopStack`, `FunctionReturn`, `ExecutionPriority`).

use crate::bus::{Access, AccessKind, Bus};
use crate::cpu::Cpu;
use crate::regs::{Sec, Xpsr};
use crate::scs::nvic::irqs;
use crate::scs::sau::{
    SFSR_AUVIOL, SFSR_INVEP, SFSR_INVER, SFSR_INVIS, SFSR_INVTRAN, SFSR_SFARVALID,
};
use crate::scs::{PRIORITY_MASK, scb};

/// Exception numbers.
pub const NMI: u16 = 2;
pub const HARDFAULT: u16 = 3;
pub const MEMMANAGE: u16 = 4;
pub const BUSFAULT: u16 = 5;
pub const USAGEFAULT: u16 = 6;
pub const SECUREFAULT: u16 = 7;
pub const SVCALL: u16 = 11;
pub const DEBUGMONITOR: u16 = 12;
pub const PENDSV: u16 = 14;
pub const SYSTICK: u16 = 15;

/// Approximate cost of an exception entry (stacking and vector fetch).
pub(crate) const ENTRY_CYCLES: u32 = 12;

/// Address the PC holds while locked up.
const LOCKUP_PC: u32 = 0xEFFF_FFFE;

// EXC_RETURN fields.
const EXC_RETURN_PREFIX: u32 = 0xFFFF_FF80;
const EXC_RETURN_ES: u32 = 1 << 0;
const EXC_RETURN_SPSEL: u32 = 1 << 2;
const EXC_RETURN_MODE: u32 = 1 << 3;
const EXC_RETURN_FTYPE: u32 = 1 << 4;
const EXC_RETURN_DCRS: u32 = 1 << 5;
const EXC_RETURN_S: u32 = 1 << 6;

/// LR value after BLXNS: a branch to it returns from the Non-secure function (no FP context).
pub(crate) const FNC_RETURN: u32 = 0xFEFF_FFFF;
/// Integrity signature at the bottom of the additional state context (no FP context).
const INTEGRITY_SIGNATURE: u32 = 0xFEFA_125B;

/// Size of the basic (integer-only) stack frame.
const FRAME_SIZE: u32 = 0x20;
/// Size of the additional state context: signature, reserved word, r4-r11.
const CALLEE_FRAME_SIZE: u32 = 0x28;
/// Size of the BLXNS frame: return address and partial xPSR.
pub(crate) const FNC_FRAME_SIZE: u32 = 8;

/// A synchronous fault, with the fault status it records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// UsageFault UNDEFINSTR: undefined or (by choice) UNPREDICTABLE instruction.
    UndefInstr,
    /// UsageFault INVSTATE: execution with EPSR.T = 0.
    InvState,
    /// UsageFault INVPC: invalid EXC_RETURN or exception return integrity check failure.
    InvPc,
    /// UsageFault INVPC raised to Secure state: a function return (FNC_RETURN) whose stacked
    /// partial xPSR does not match the current mode.
    SecureInvPc,
    /// UsageFault NOCP: coprocessor instruction with the coprocessor absent or disabled.
    NoCp,
    /// UsageFault STKOF: stack limit violation.
    StkOf,
    /// UsageFault UNALIGNED.
    Unaligned,
    /// UsageFault DIVBYZERO.
    DivByZero,
    /// BusFault IBUSERR: instruction fetch bus error.
    IBusErr,
    /// BusFault PRECISERR at the given address (BFAR).
    PreciseErr(u32),
    /// BusFault STKERR: bus error while stacking.
    StkErr,
    /// BusFault UNSTKERR: bus error while unstacking.
    UnstkErr,
    /// MemManage IACCVIOL: instruction fetch from an Execute Never region.
    IAccViol,
    /// SecureFault INVEP: Non-secure code entered Secure memory other than at an `SG` in a
    /// Non-secure callable region.
    InvEp,
    /// SecureFault INVIS: the integrity signature of the additional state context is wrong.
    InvIs,
    /// SecureFault INVER: Non-secure software attempted an invalid exception return to Secure.
    InvEr,
    /// SecureFault AUVIOL: Non-secure access to Secure memory; `Some(addr)` is recorded in SFAR
    /// (data accesses), `None` for stacking.
    AuViol(Option<u32>),
    /// SecureFault INVTRAN: Secure code reached Non-secure memory without BXNS/BLXNS.
    InvTran,
}

/// What an exception return did, for `Cpu::step`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReturnOutcome {
    Done,
    /// Returned to Thread mode with `SCR.SLEEPONEXIT` set.
    Sleep,
    Lockup,
}

/// Exceptions that have a separate instance per security state.
pub(crate) fn is_banked(num: u16) -> bool {
    matches!(
        num,
        HARDFAULT | MEMMANAGE | USAGEFAULT | SVCALL | PENDSV | SYSTICK
    )
}

/// Which copy of the SCB state holds exception `num` for `sec` (non-banked exceptions live in
/// the Secure copy).
fn bank(num: u16, sec: Sec) -> Sec {
    if is_banked(num) { sec } else { Sec::Secure }
}

impl Cpu {
    /// True while the processor is in the lockup state (see `StepResult::Lockup`).
    pub fn locked_up(&self) -> bool {
        self.lockup.is_some()
    }

    /// True if an enabled exception is pending, whatever its priority relative to the current
    /// execution priority (PRIMASK, BASEPRI, active handlers). This is the wake-up condition
    /// for WFI/WFE that the machine uses. It slightly over-approximates the architecture, which
    /// does not wake for an exception that could not preempt the active handlers even with
    /// PRIMASK clear; such a wake only makes the CPU run on instead of sleeping.
    pub fn exception_pending(&self) -> bool {
        self.highest_pending().is_some()
    }

    // ---- Exception state ----

    pub(crate) fn exc_pending(&self, num: u16, sec: Sec) -> bool {
        match num {
            0 => false,
            1..=15 => self.scs.scb.pending.get(bank(num, sec)) & (1 << num) != 0,
            _ => self.scs.nvic.is_pending(usize::from(num - 16)),
        }
    }

    pub(crate) fn set_exc_pending(&mut self, num: u16, sec: Sec, v: bool) {
        match num {
            0 => {}
            1..=15 => {
                let p = self.scs.scb.pending.get_mut(bank(num, sec));
                *p = (*p & !(1 << num)) | (u16::from(v) << num);
            }
            _ => self.scs.nvic.set_pending(usize::from(num - 16), v),
        }
    }

    pub(crate) fn exc_active(&self, num: u16, sec: Sec) -> bool {
        match num {
            0 => false,
            1..=15 => self.scs.scb.active.get(bank(num, sec)) & (1 << num) != 0,
            _ => self.scs.nvic.is_active(usize::from(num - 16)),
        }
    }

    pub(crate) fn set_exc_active(&mut self, num: u16, sec: Sec, v: bool) {
        match num {
            0 => {}
            1..=15 => {
                let a = self.scs.scb.active.get_mut(bank(num, sec));
                *a = (*a & !(1 << num)) | (u16::from(v) << num);
            }
            _ => self.scs.nvic.set_active(usize::from(num - 16), v),
        }
    }

    /// `IsActiveForState()`: exception `num` is active as an exception of security state `sec`.
    fn is_active_for_state(&self, num: u16, sec: Sec) -> bool {
        self.exc_active(num, sec) && self.exc_target(num, sec) == sec
    }

    /// Number of active exceptions (both security states).
    pub(crate) fn active_count(&self) -> u32 {
        let scb = &self.scs.scb;
        scb.active.s.count_ones()
            + scb.active.ns.count_ones()
            + self
                .scs
                .nvic
                .active
                .iter()
                .map(|w| w.count_ones())
                .sum::<u32>()
    }

    /// Security state an external interrupt targets (`NVIC_ITNS`).
    fn irq_target(&self, irq: usize) -> Sec {
        if self.scs.nvic.targets_secure(irq) {
            Sec::Secure
        } else {
            Sec::NonSecure
        }
    }

    /// Security state exception `num` targets. Banked exceptions target the state of the copy
    /// they are pending or active in (`copy`); NMI and BusFault target Non-secure state only
    /// with `AIRCR.BFHFNMINS`; SecureFault and DebugMonitor are Secure; IRQs follow ITNS.
    pub(crate) fn exc_target(&self, num: u16, copy: Sec) -> Sec {
        match num {
            NMI | BUSFAULT if self.scs.scb.bfhfnmins => Sec::NonSecure,
            NMI | BUSFAULT | SECUREFAULT | DEBUGMONITOR => Sec::Secure,
            16.. => self.irq_target(usize::from(num - 16)),
            _ => copy,
        }
    }

    /// The HardFault a fault targeting `sec` escalates to: the Secure HardFault, unless
    /// `AIRCR.BFHFNMINS` gives Non-secure state its own.
    fn hardfault_target(&self, sec: Sec) -> Sec {
        if self.scs.scb.bfhfnmins {
            sec
        } else {
            Sec::Secure
        }
    }

    /// Whether exception `num` (for `sec`) may be taken when pending.
    fn exc_enabled(&self, num: u16, sec: Sec) -> bool {
        let ena = &self.scs.scb.shcsr_ena;
        match num {
            MEMMANAGE => ena.get(sec) & scb::MEMFAULTENA != 0,
            BUSFAULT => ena.s & scb::BUSFAULTENA != 0,
            USAGEFAULT => ena.get(sec) & scb::USGFAULTENA != 0,
            SECUREFAULT => ena.s & scb::SECUREFAULTENA != 0,
            // No debug monitor; reserved numbers never pend.
            DEBUGMONITOR | 8..=10 | 13 | 0 => false,
            1..=15 => true,
            _ => self.scs.nvic.is_enabled(usize::from(num - 16)),
        }
    }

    // ---- Priorities ----

    /// Group priority of a raw priority value for `sec`: subpriority bits cleared by
    /// `AIRCR.PRIGROUP`, then `AIRCR.PRIS` de-prioritisation of Non-secure values.
    fn group_priority(&self, raw: u8, sec: Sec, group: bool) -> i32 {
        let scb = &self.scs.scb;
        let mut p = u32::from(raw & PRIORITY_MASK);
        if group {
            p &= !((2u32 << scb.prigroup.get(sec)) - 1);
        }
        if sec == Sec::NonSecure && scb.pris {
            p = (p >> 1) + 0x80;
        }
        p as i32
    }

    /// `ExceptionPriority()`: the priority of exception `num` targeting `sec`; `group` selects
    /// the group priority (used for preemption) rather than the full value (used for ordering).
    pub(crate) fn exception_priority(&self, num: u16, sec: Sec, group: bool) -> i32 {
        let raw = match num {
            1 => return -4,
            NMI => return -2,
            HARDFAULT => {
                return if sec == Sec::Secure && self.scs.scb.bfhfnmins {
                    -3
                } else {
                    -1
                };
            }
            4..=15 => self.scs.scb.shpr.get(bank(num, sec))[usize::from(num - 4)],
            _ => {
                let irq = usize::from(num.saturating_sub(16));
                self.scs.nvic.priority.get(irq).copied().unwrap_or(0)
            }
        };
        self.group_priority(raw, sec, group)
    }

    /// `ExecutionPriority()`: the highest active group priority, boosted by BASEPRI, PRIMASK and
    /// FAULTMASK of both security states. 256 when nothing applies.
    pub(crate) fn execution_priority(&self) -> i32 {
        let mut highest = 256;
        for copy in [Sec::Secure, Sec::NonSecure] {
            let active = *self.scs.scb.active.get(copy);
            for num in 1..16 {
                if active & (1 << num) != 0 {
                    let sec = self.exc_target(num, copy);
                    highest = highest.min(self.exception_priority(num, sec, true));
                }
            }
        }
        for irq in irqs(&self.scs.nvic.active) {
            let p = self.exception_priority(16 + irq as u16, self.irq_target(irq), true);
            highest = highest.min(p);
        }

        let r = &self.regs;
        let scb = &self.scs.scb;
        let mut boosted = 256;
        // Non-secure boosting.
        if r.basepri.ns & PRIORITY_MASK != 0 {
            boosted = boosted.min(self.group_priority(r.basepri.ns, Sec::NonSecure, true));
        }
        if r.primask.ns {
            boosted = boosted.min(if scb.pris { 0x80 } else { 0 });
        }
        if r.faultmask.ns {
            boosted = boosted.min(match (scb.bfhfnmins, scb.pris) {
                (true, _) => -1,
                (false, true) => 0x80,
                (false, false) => 0,
            });
        }
        // Secure boosting.
        if r.basepri.s & PRIORITY_MASK != 0 {
            boosted = boosted.min(self.group_priority(r.basepri.s, Sec::Secure, true));
        }
        if r.primask.s {
            boosted = boosted.min(0);
        }
        if r.faultmask.s {
            boosted = boosted.min(if scb.bfhfnmins { -3 } else { -1 });
        }
        highest.min(boosted)
    }

    /// The highest-priority pending and enabled exception and the state it targets, ignoring
    /// the execution priority (lowest priority value first, then lowest exception number).
    pub(crate) fn highest_pending(&self) -> Option<(u16, Sec)> {
        let mut best: Option<(i32, u16, Sec)> = None;
        let mut consider = |p: i32, num: u16, sec: Sec| {
            if best.is_none_or(|(bp, bn, _)| p < bp || (p == bp && num < bn)) {
                best = Some((p, num, sec));
            }
        };
        for copy in [Sec::Secure, Sec::NonSecure] {
            let pending = *self.scs.scb.pending.get(copy);
            for num in 1..16 {
                let sec = self.exc_target(num, copy);
                if pending & (1 << num) != 0 && self.exc_enabled(num, sec) {
                    consider(self.exception_priority(num, sec, false), num, sec);
                }
            }
        }
        for irq in irqs(&self.scs.nvic.pending) {
            if self.scs.nvic.is_enabled(irq) {
                let (num, sec) = (16 + irq as u16, self.irq_target(irq));
                consider(self.exception_priority(num, sec, false), num, sec);
            }
        }
        best.map(|(_, num, sec)| (num, sec))
    }

    /// The pending exception that preempts the current execution priority, if any.
    pub(crate) fn preemptable_pending(&self) -> Option<(u16, Sec)> {
        let scb = &self.scs.scb;
        if scb.pending.s == 0
            && scb.pending.ns == 0
            && self.scs.nvic.pending.iter().all(|&w| w == 0)
        {
            return None;
        }
        let (num, sec) = self.highest_pending()?;
        (self.exception_priority(num, sec, true) < self.execution_priority()).then_some((num, sec))
    }

    // ---- Faults ----

    fn usage_fault(&mut self, sec: Sec, bit: u16) -> (u16, Sec) {
        *self.scs.scb.ufsr.get_mut(sec) |= bit;
        (USAGEFAULT, sec)
    }

    fn bus_fault(&mut self, bits: u8) -> (u16, Sec) {
        self.scs.scb.bfsr |= bits;
        (BUSFAULT, self.exc_target(BUSFAULT, Sec::Secure))
    }

    fn secure_fault(&mut self, bits: u32) -> (u16, Sec) {
        self.scs.sau.sfsr |= bits;
        (SECUREFAULT, Sec::Secure)
    }

    /// Record `fault`'s status bits and return the exception that handles it and the security
    /// state that exception targets.
    fn record_fault(&mut self, fault: Fault) -> (u16, Sec) {
        let cur = self.regs.sec;
        match fault {
            Fault::UndefInstr => self.usage_fault(cur, scb::UFSR_UNDEFINSTR),
            Fault::InvState => self.usage_fault(cur, scb::UFSR_INVSTATE),
            Fault::InvPc => self.usage_fault(cur, scb::UFSR_INVPC),
            Fault::SecureInvPc => self.usage_fault(Sec::Secure, scb::UFSR_INVPC),
            Fault::NoCp => self.usage_fault(cur, scb::UFSR_NOCP),
            Fault::StkOf => self.usage_fault(cur, scb::UFSR_STKOF),
            Fault::Unaligned => self.usage_fault(cur, scb::UFSR_UNALIGNED),
            Fault::DivByZero => self.usage_fault(cur, scb::UFSR_DIVBYZERO),
            Fault::IBusErr => self.bus_fault(scb::BFSR_IBUSERR),
            Fault::PreciseErr(addr) => {
                self.scs.scb.bfar = addr;
                self.bus_fault(scb::BFSR_PRECISERR | scb::BFSR_BFARVALID)
            }
            Fault::StkErr => self.bus_fault(scb::BFSR_STKERR),
            Fault::UnstkErr => self.bus_fault(scb::BFSR_UNSTKERR),
            Fault::IAccViol => {
                *self.scs.scb.mmfsr.get_mut(cur) |= scb::MMFSR_IACCVIOL;
                (MEMMANAGE, cur)
            }
            Fault::InvEp => self.secure_fault(SFSR_INVEP),
            Fault::InvIs => self.secure_fault(SFSR_INVIS),
            Fault::InvEr => self.secure_fault(SFSR_INVER),
            Fault::InvTran => self.secure_fault(SFSR_INVTRAN),
            Fault::AuViol(None) => self.secure_fault(SFSR_AUVIOL),
            Fault::AuViol(Some(addr)) => {
                self.scs.sau.sfar = addr;
                self.secure_fault(SFSR_AUVIOL | SFSR_SFARVALID)
            }
        }
    }

    /// Raise a synchronous fault: record its status and pend its exception (escalated as
    /// needed). Returns false when the processor must lock up.
    pub(crate) fn raise_fault(&mut self, fault: Fault) -> bool {
        log::debug!(
            "fault {fault:?} at pc {:#010x} ({:?})",
            self.regs.pc,
            self.regs.sec
        );
        let (num, sec) = self.record_fault(fault);
        self.raise_sync(num, sec)
    }

    /// Pend synchronous exception `num` for `sec`. A configurable exception that is disabled or
    /// cannot preempt the current execution priority escalates to HardFault (HFSR.FORCED).
    /// Returns false when HardFault cannot preempt either: the processor must lock up.
    pub(crate) fn raise_sync(&mut self, num: u16, sec: Sec) -> bool {
        let exec = self.execution_priority();
        let mut target = num;
        if num != HARDFAULT
            && !(self.exc_enabled(num, sec) && self.exception_priority(num, sec, true) < exec)
        {
            self.scs.scb.hfsr |= scb::HFSR_FORCED;
            target = HARDFAULT;
        }
        let sec = if target == HARDFAULT {
            self.hardfault_target(sec)
        } else {
            sec
        };
        if target == HARDFAULT && self.exception_priority(HARDFAULT, sec, true) >= exec {
            return false;
        }
        self.set_exc_pending(target, sec, true);
        true
    }

    /// Enter the lockup state. It is held at the current execution priority (at most -1), or
    /// at the priority of `entering` if that is the exception whose entry failed (an NMI whose
    /// vector or stacking failed locks up at -2). Execution stops, and nothing is stacked, until
    /// reset or a pending exception of strictly higher priority than that (NMI from -1).
    pub(crate) fn enter_lockup(&mut self, entering: Option<(u16, Sec)>) {
        let mut priority = self.execution_priority().min(-1);
        if let Some((num, sec)) = entering {
            priority = priority.min(self.exception_priority(num, sec, true));
        }
        log::warn!("lockup at priority {priority} (pc {:#010x})", self.regs.pc);
        self.lockup = Some(priority);
        self.regs.pc = LOCKUP_PC;
    }

    /// While locked up: leave lockup if a pending exception outranks the lockup priority.
    /// Returns true if still locked up.
    pub(crate) fn stay_locked_up(&mut self) -> bool {
        let Some(priority) = self.lockup else {
            return false;
        };
        match self.highest_pending() {
            Some((num, sec)) if self.exception_priority(num, sec, true) < priority => {
                self.lockup = None;
                false
            }
            _ => true,
        }
    }

    // ---- Stack accesses ----

    /// Write one word of a stack frame that belongs to security state `sec`. The access has
    /// the frame's security; a Non-secure frame reaching Secure memory is a SecureFault AUVIOL.
    pub(crate) fn stack_write(
        &self,
        bus: &mut impl Bus,
        addr: u32,
        v: u32,
        sec: Sec,
        privileged: bool,
    ) -> Result<(), Fault> {
        let acc = Access {
            kind: AccessKind::Write,
            secure: sec == Sec::Secure,
            privileged,
        };
        let acc = self.attribute(addr, acc, sec).ok_or(Fault::AuViol(None))?;
        bus.write32(addr, v, acc).map_err(|_| Fault::StkErr)
    }

    /// Read one word of a stack frame that belongs to security state `sec` (see `stack_write`).
    fn stack_read(
        &self,
        bus: &mut impl Bus,
        addr: u32,
        sec: Sec,
        privileged: bool,
    ) -> Result<u32, Fault> {
        let acc = Access {
            kind: AccessKind::Read,
            secure: sec == Sec::Secure,
            privileged,
        };
        let acc = self.attribute(addr, acc, sec).ok_or(Fault::AuViol(None))?;
        bus.read32(addr, acc).map_err(|_| Fault::UnstkErr)
    }

    // ---- Entry ----

    /// Take the highest-priority pending exception if it preempts the current execution
    /// priority. Returns true if an exception entry was performed (or it locked up trying).
    pub(crate) fn take_pending(&mut self, bus: &mut impl Bus) -> bool {
        let Some((num, sec)) = self.preemptable_pending() else {
            return false;
        };
        self.exception_entry(bus, num, sec);
        true
    }

    /// `ExceptionEntry`: stack the current context and enter the handler of `num`.
    fn exception_entry(&mut self, bus: &mut impl Bus, num: u16, sec: Sec) {
        let (num, sec) = match self.push_stack(bus) {
            None => (num, sec),
            // A fault while stacking is a derived exception. It is raised against the
            // interrupted context; if it (or its escalation) outranks the original exception it
            // is taken instead and the original stays pending.
            Some(fault) => {
                if !self.raise_fault(fault) {
                    self.enter_lockup(Some((num, sec)));
                    return;
                }
                self.preemptable_pending().unwrap_or((num, sec))
            }
        };
        self.exception_taken(bus, num, sec, false);
    }

    /// `PushStack`: write the basic frame below the current SP of the current security state
    /// (8-byte aligned, recording the realignment in xPSR bit 9) and set LR to the EXC_RETURN
    /// describing it. Returns the derived fault, if stacking failed.
    fn push_stack(&mut self, bus: &mut impl Bus) -> Option<Fault> {
        let sec = self.regs.sec;
        let thread = !self.regs.handler_mode();
        let use_psp = thread && self.regs.control.get(sec) & 0b10 != 0;
        let (sp, limit) = if use_psp {
            (*self.regs.psp.get(sec), *self.regs.psplim.get(sec))
        } else {
            (*self.regs.msp.get(sec), *self.regs.msplim.get(sec))
        };
        let realign = sp & 4 != 0;
        let frame = sp.wrapping_sub(FRAME_SIZE) & !7;
        let xpsr = (self.regs.xpsr.to_u32() & !(1 << 9)) | (u32::from(realign) << 9);
        let r = &self.regs.r;
        let words = [
            r[0],
            r[1],
            r[2],
            r[3],
            r[12],
            self.regs.lr,
            self.regs.pc,
            xpsr,
        ];

        let mut derived = None;
        let new_sp = if frame < limit {
            // Stack limit violation: nothing is written below the limit and SP is set to it.
            derived = Some(Fault::StkOf);
            limit
        } else {
            let privileged = self.is_privileged();
            for (i, &w) in words.iter().enumerate() {
                let addr = frame.wrapping_add(4 * i as u32);
                if let Err(f) = self.stack_write(bus, addr, w, sec, privileged) {
                    derived.get_or_insert(f);
                }
            }
            frame
        };
        if use_psp {
            *self.regs.psp.get_mut(sec) = new_sp;
        } else {
            *self.regs.msp.get_mut(sec) = new_sp;
        }

        // EXC_RETURN: no FP frame, default callee-register stacking. ES and SPSEL (the
        // transitory CONTROL.SPSEL of the state the exception targets) are set when taken.
        let mut exc_return = EXC_RETURN_PREFIX | EXC_RETURN_DCRS | EXC_RETURN_FTYPE;
        if sec == Sec::Secure {
            exc_return |= EXC_RETURN_S;
        }
        if thread {
            exc_return |= EXC_RETURN_MODE;
        }
        self.regs.lr = exc_return;
        derived
    }

    /// The Secure stack pointer (as a mutable reference) and its limit for the frame that the
    /// EXC_RETURN value `exc_return` describes: `LookUpSP_with_security_mode(TRUE, mode)`, with
    /// the mode from EXC_RETURN.Mode and the stack from CONTROL_S.SPSEL (EXC_RETURN.SPSEL
    /// belongs to the state the exception targets, not to the Secure background).
    fn secure_frame_sp(&mut self, exc_return: u32) -> (&mut u32, u32) {
        let r = &mut self.regs;
        if exc_return & EXC_RETURN_MODE != 0 && r.control.s & 0b10 != 0 {
            (&mut r.psp.s, r.psplim.s)
        } else {
            (&mut r.msp.s, r.msplim.s)
        }
    }

    /// `PushCalleeStack`: push the additional state context (integrity signature, a reserved
    /// word, r4-r11) below the basic frame on the Secure stack that LR (EXC_RETURN) describes.
    /// Like `push_stack`, a stack limit violation writes nothing and sets SP to the limit, and
    /// a bus error does not stop the other writes; SP moves either way.
    fn push_callee_stack(&mut self, bus: &mut impl Bus) -> Option<Fault> {
        let lr = self.regs.lr;
        let privileged = lr & EXC_RETURN_MODE == 0 || self.regs.control.s & 1 == 0;
        let (sp, limit) = {
            let (sp, limit) = self.secure_frame_sp(lr);
            (*sp, limit)
        };
        let frame = sp.wrapping_sub(CALLEE_FRAME_SIZE);
        if frame < limit {
            *self.secure_frame_sp(lr).0 = limit;
            return Some(Fault::StkOf);
        }
        let r = &self.regs.r;
        let mut words = [INTEGRITY_SIGNATURE, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        words[2..].copy_from_slice(&r[4..12]);
        let mut derived = None;
        for (i, &w) in words.iter().enumerate() {
            let addr = frame.wrapping_add(4 * i as u32);
            if let Err(f) = self.stack_write(bus, addr, w, Sec::Secure, privileged) {
                derived.get_or_insert(f);
            }
        }
        *self.secure_frame_sp(lr).0 = frame;
        derived
    }

    /// `ExceptionTaken`: fetch the vector and enter the handler of `num` targeting `sec`. LR
    /// already holds the EXC_RETURN value (from `push_stack`, or kept across a tail-chain:
    /// `tail_chain`, in which case its ES bit is the security of the exception just left).
    ///
    /// EXC_RETURN.DCRS ("default callee register stacking") is 1 when the callee-saved
    /// registers are stacked by the default rule: on the Secure stack exactly when a
    /// Non-secure exception interrupted a Secure background. It is 0 when they are on the
    /// stack although the exception is Secure, which happens when a Non-secure exception over
    /// a Secure background tail-chains into a Secure one.
    ///
    /// EXC_RETURN.SPSEL records the transitory CONTROL.SPSEL of the target state (the state
    /// ES names), which Handler mode then clears; `exception_return` restores it. The other
    /// state's CONTROL.SPSEL is not touched and keeps selecting its stack.
    fn exception_taken(&mut self, bus: &mut impl Bus, num: u16, sec: Sec, tail_chain: bool) {
        let spsel = self.regs.control.get(sec) & 0b10 != 0;
        self.regs.lr = (self.regs.lr & !EXC_RETURN_SPSEL) | (u32::from(spsel) << 2);
        let lr = self.regs.lr;
        let mut derived = None;
        if lr & EXC_RETURN_S != 0 {
            // Secure background. Leaving a Non-secure exception, its callee context is already
            // on the Secure stack.
            let from_ns_exception = tail_chain && lr & EXC_RETURN_ES == 0;
            if sec == Sec::NonSecure {
                if lr & EXC_RETURN_DCRS != 0 && !from_ns_exception {
                    derived = self.push_callee_stack(bus);
                }
                self.regs.lr |= EXC_RETURN_DCRS;
            } else if from_ns_exception {
                self.regs.lr &= !EXC_RETURN_DCRS;
            }
        }
        self.regs.lr = (self.regs.lr & !EXC_RETURN_ES) | u32::from(sec == Sec::Secure);
        if let Some(fault) = derived {
            // A derived exception on callee stacking: start again as a tail-chain from this
            // exception, taking the derived exception instead if it preempts (the original
            // stays pending). The context is not pushed a second time.
            if !self.raise_fault(fault) {
                self.enter_lockup(Some((num, sec)));
                return;
            }
            let (n, s) = self.preemptable_pending().unwrap_or((num, sec));
            self.exception_taken(bus, n, s, true);
            return;
        }

        let vtor = *self.scs.scb.vtor.get(sec);
        let acc = Access {
            kind: AccessKind::Read,
            secure: sec == Sec::Secure,
            privileged: true,
        };
        let addr = vtor.wrapping_add(4 * u32::from(num));
        let vector = match bus.read32(addr, acc) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("vector read for exception {num} at {addr:#010x} failed: {e:?}");
                self.scs.scb.hfsr |= scb::HFSR_VECTTBL;
                // The original exception stays pending; HardFault is taken in its place, as a
                // tail-chain from it (LR already describes the frame for it).
                let hf = self.hardfault_target(sec);
                if num == HARDFAULT
                    || self.exception_priority(HARDFAULT, hf, true) >= self.execution_priority()
                {
                    self.enter_lockup(Some((num, sec)));
                } else {
                    self.exception_taken(bus, HARDFAULT, hf, true);
                }
                return;
            }
        };

        // A Non-secure handler must not see Secure register contents: the caller-saved
        // registers and APSR when leaving Secure state (they are in the basic frame), and r4-r11
        // over a Secure background (they are in the callee context).
        if sec == Sec::NonSecure {
            let r = &mut self.regs.r;
            if self.regs.sec == Sec::Secure {
                r[..4].fill(0);
                r[12] = 0;
                let x = &mut self.regs.xpsr;
                (x.n, x.z, x.c, x.v, x.q, x.ge) = (false, false, false, false, false, 0);
            }
            if self.regs.lr & EXC_RETURN_S != 0 {
                r[4..12].fill(0);
            }
        }

        self.regs.sec = sec;
        let x = &mut self.regs.xpsr;
        x.exception = num;
        x.it = 0;
        // A vector with bit 0 clear faults INVSTATE on the handler's first instruction.
        x.t = vector & 1 != 0;
        self.regs.pc = vector & !1;
        // Handler mode always uses the main stack: SPSEL of the handler's state is cleared.
        *self.regs.control.get_mut(sec) &= 1;
        self.set_exc_pending(num, sec, false);
        self.set_exc_active(num, sec, true);
        self.exclusive = None;
        // SetEventRegister().
        self.event = true;
    }

    // ---- Return ----

    /// `ExceptionReturn`: a BX/POP/LDR of an EXC_RETURN value in Handler mode.
    pub(crate) fn exception_return(
        &mut self,
        bus: &mut impl Bus,
        exc_return: u32,
    ) -> ReturnOutcome {
        let cur = self.regs.sec;
        let num = self.regs.xpsr.exception;
        // ES: the security state of the exception being returned from. S: that of the
        // background (and of the stack holding its frame).
        let es = exc_return & EXC_RETURN_ES != 0;
        let exc_sec = if es { Sec::Secure } else { Sec::NonSecure };
        let to_secure = exc_return & EXC_RETURN_S != 0;
        let frame_sec = if to_secure {
            Sec::Secure
        } else {
            Sec::NonSecure
        };
        let to_thread = exc_return & EXC_RETURN_MODE != 0;
        let spsel = exc_return & EXC_RETURN_SPSEL != 0;
        let dcrs = exc_return & EXC_RETURN_DCRS != 0;

        // Non-secure software can neither return from a Secure exception (ES = 1) nor claim a
        // non-default callee context (DCRS = 0). The exception is then treated as Non-secure.
        let inver = cur == Sec::NonSecure && (es || !dcrs);
        let (exc_return, exc_sec) = if inver {
            (exc_return & !EXC_RETURN_ES, Sec::NonSecure)
        } else {
            (exc_return, exc_sec)
        };
        // Restore the transitory CONTROL.SPSEL of the state the exception was taken to. This
        // comes before any tail-chain or derived exception, which record it in their own
        // EXC_RETURN. (Still in Handler mode, so it does not switch the current stack.)
        let control = self.regs.control.get_mut(exc_sec);
        *control = (*control & !0b10) | (u32::from(spsel) << 1);
        if inver {
            log::warn!("invalid exception return {exc_return:#010x} from Non-secure state");
            return self.return_fault(bus, exc_return, Fault::InvEr);
        }
        // EXC_RETURN.SPSEL describes the state named by ES, which may be the other state than
        // the background's, so Mode = 0 with SPSEL = 1 is valid (unlike v7-M's 0xFFFF_FFF5).
        let valid = exc_return & 0x00FF_FF82 == 0x00FF_FF80
            && exc_return & EXC_RETURN_FTYPE != 0
            && self.is_active_for_state(num, exc_sec)
            && (to_thread || self.active_count() > 1);
        if !valid {
            log::warn!("invalid exception return {exc_return:#010x} from exception {num}");
            return self.return_fault(bus, exc_return, Fault::InvPc);
        }

        // DeActivate.
        self.set_exc_active(num, exc_sec, false);
        if num != NMI {
            *self.regs.faultmask.get_mut(exc_sec) = false;
        }
        if num >= 16 && self.scs.nvic.level(usize::from(num - 16)) {
            self.pend_irq(usize::from(num - 16));
        }
        self.exclusive = None;
        // SetEventRegister().
        self.event = true;

        // Tail-chain: a pending exception that preempts the context being returned to is
        // taken without unstacking and restacking; LR keeps describing the frame.
        if let Some((n, s)) = self.preemptable_pending() {
            self.regs.lr = exc_return;
            self.exception_taken(bus, n, s, true);
            return self.entry_outcome();
        }

        // PopStack: the frame is on `LookUpSP_with_security_mode(S, Mode)`, the stack the
        // background state's own CONTROL.SPSEL selects.
        let use_psp = to_thread && self.regs.control.get(frame_sec) & 0b10 != 0;
        let mut frame = if use_psp {
            *self.regs.psp.get(frame_sec)
        } else {
            *self.regs.msp.get(frame_sec)
        };
        let privileged = !to_thread || self.regs.control.get(frame_sec) & 1 == 0;
        // The callee context is on the Secure stack for a Non-secure exception over a Secure
        // background (ES = 0), and for a Secure one with DCRS = 0.
        let mut callee = None;
        if to_secure && (!es || !dcrs) {
            let sig = match self.stack_read(bus, frame, frame_sec, privileged) {
                Ok(v) => v,
                Err(f) => return self.return_fault(bus, exc_return, f),
            };
            if sig != INTEGRITY_SIGNATURE {
                log::warn!("exception return integrity signature {sig:#010x} at {frame:#010x}");
                return self.return_fault(bus, exc_return, Fault::InvIs);
            }
            let mut regs = [0u32; 8];
            for (i, word) in regs.iter_mut().enumerate() {
                let addr = frame.wrapping_add(8 + 4 * i as u32);
                match self.stack_read(bus, addr, frame_sec, privileged) {
                    Ok(v) => *word = v,
                    Err(f) => return self.return_fault(bus, exc_return, f),
                }
            }
            callee = Some(regs);
            frame = frame.wrapping_add(CALLEE_FRAME_SIZE);
        }
        let mut w = [0u32; 8];
        for (i, word) in w.iter_mut().enumerate() {
            let addr = frame.wrapping_add(4 * i as u32);
            match self.stack_read(bus, addr, frame_sec, privileged) {
                Ok(v) => *word = v,
                Err(f) => return self.return_fault(bus, exc_return, f),
            }
        }
        let psr = w[7];
        // Integrity check: the stacked IPSR must match the mode being returned to.
        if to_thread != (psr & 0x1FF == 0) {
            log::warn!("exception return to {exc_return:#010x} with stacked xPSR {psr:#010x}");
            return self.return_fault(bus, exc_return, Fault::InvPc);
        }
        if let Some(regs) = callee {
            self.regs.r[4..12].copy_from_slice(&regs);
        }
        self.regs.r[..4].copy_from_slice(&w[..4]);
        self.regs.r[12] = w[4];
        self.regs.lr = w[5];
        self.regs.pc = w[6] & !1;
        self.regs.xpsr = Xpsr::from_u32(psr);
        let sp = frame.wrapping_add(FRAME_SIZE) | ((psr >> 9 & 1) << 2);
        if use_psp {
            *self.regs.psp.get_mut(frame_sec) = sp;
        } else {
            *self.regs.msp.get_mut(frame_sec) = sp;
        }
        self.regs.sec = frame_sec;

        if to_thread && self.scs.scb.scr.get(cur) & scb::SCR_SLEEPONEXIT != 0 {
            return ReturnOutcome::Sleep;
        }
        ReturnOutcome::Done
    }

    /// A fault detected during an exception return (invalid EXC_RETURN, failed integrity check
    /// or unstacking error). The frame stays on the stack and the fault is taken like a
    /// tail-chain, with LR keeping the EXC_RETURN value.
    fn return_fault(&mut self, bus: &mut impl Bus, exc_return: u32, fault: Fault) -> ReturnOutcome {
        if !self.raise_fault(fault) {
            self.enter_lockup(None);
            return ReturnOutcome::Lockup;
        }
        if let Some((n, s)) = self.preemptable_pending() {
            self.regs.lr = exc_return;
            self.exception_taken(bus, n, s, true);
        }
        self.entry_outcome()
    }

    fn entry_outcome(&self) -> ReturnOutcome {
        if self.lockup.is_some() {
            ReturnOutcome::Lockup
        } else {
            ReturnOutcome::Done
        }
    }

    /// `FunctionReturn`: a branch to FNC_RETURN ends a Non-secure function called with BLXNS.
    /// The return address and partial xPSR are popped from the Secure stack of the current mode
    /// and execution continues there in Secure state. On a fault nothing changes.
    pub(crate) fn function_return(&mut self, bus: &mut impl Bus) -> Result<(), Fault> {
        let handler = self.regs.handler_mode();
        let use_psp = !handler && self.regs.control.s & 0b10 != 0;
        let frame = if use_psp {
            self.regs.psp.s
        } else {
            self.regs.msp.s
        };
        let privileged = handler || self.regs.control.s & 1 == 0;
        let ret = self.stack_read(bus, frame, Sec::Secure, privileged)?;
        let psr = self.stack_read(bus, frame.wrapping_add(4), Sec::Secure, privileged)?;
        // BLXNS from Handler mode hides the exception number as IPSR = 1; from Thread mode it
        // stacks IPSR = 0. Anything else is not the frame of this call.
        let saved = (psr & 0x1FF) as u16;
        let valid = if handler {
            self.regs.xpsr.exception == 1 && saved != 0
        } else {
            saved == 0
        };
        if !valid {
            log::warn!("function return with stacked partial xPSR {psr:#010x}");
            return Err(Fault::SecureInvPc);
        }
        let sp = frame.wrapping_add(FNC_FRAME_SIZE);
        if use_psp {
            self.regs.psp.s = sp;
        } else {
            self.regs.msp.s = sp;
        }
        self.regs.xpsr.exception = saved;
        self.regs.sec = Sec::Secure;
        self.blx_write_pc(ret);
        Ok(())
    }
}
