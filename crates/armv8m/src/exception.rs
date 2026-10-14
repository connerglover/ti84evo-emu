//! Exception model: priorities, entry (stacking), return (unstacking), tail-chaining, fault
//! escalation and lockup. Follows the ARMv8-M pseudocode (`ExceptionEntry`, `PushStack`,
//! `ExceptionTaken`, `ExceptionReturn`, `PopStack`, `ExecutionPriority`) for a single security
//! state; the places where the Security Extension adds behaviour are marked `Task 8`.

use crate::bus::{Access, AccessKind, Bus};
use crate::cpu::Cpu;
use crate::regs::{Sec, Xpsr};
use crate::scs::nvic::irqs;
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

/// Size of the basic (integer-only) stack frame.
const FRAME_SIZE: u32 = 0x20;

/// A synchronous fault, with the fault status it records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// UsageFault UNDEFINSTR: undefined or (by choice) UNPREDICTABLE instruction.
    UndefInstr,
    /// UsageFault INVSTATE: execution with EPSR.T = 0.
    InvState,
    /// UsageFault INVPC: invalid EXC_RETURN or exception return integrity check failure.
    InvPc,
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
fn is_banked(num: u16) -> bool {
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

    /// Security state an external interrupt targets.
    fn irq_target(&self, _irq: usize) -> Sec {
        // Task 8: ITNS routes the IRQ to Non-secure state (`self.scs.nvic.targets_secure`).
        Sec::Secure
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

    /// `ExceptionPriority()`: the priority of exception `num` for `sec`; `group` selects the
    /// group priority (used for preemption) rather than the full value (used for ordering).
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
        for sec in [Sec::Secure, Sec::NonSecure] {
            let active = *self.scs.scb.active.get(sec);
            for num in 1..16 {
                if active & (1 << num) != 0 {
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

    /// The highest-priority pending and enabled exception, ignoring the execution priority
    /// (lowest priority value first, then lowest exception number).
    pub(crate) fn highest_pending(&self) -> Option<(u16, Sec)> {
        let mut best: Option<(i32, u16, Sec)> = None;
        let mut consider = |p: i32, num: u16, sec: Sec| {
            if best.is_none_or(|(bp, bn, _)| p < bp || (p == bp && num < bn)) {
                best = Some((p, num, sec));
            }
        };
        for sec in [Sec::Secure, Sec::NonSecure] {
            let pending = *self.scs.scb.pending.get(sec);
            for num in 1..16 {
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

    /// Record `fault`'s status bits and return the exception that handles it.
    fn record_fault(&mut self, fault: Fault) -> u16 {
        let sec = self.regs.sec;
        let scb = &mut self.scs.scb;
        let usage = |scb: &mut scb::Scb, bit: u16| {
            *scb.ufsr.get_mut(sec) |= bit;
            USAGEFAULT
        };
        match fault {
            Fault::UndefInstr => usage(scb, scb::UFSR_UNDEFINSTR),
            Fault::InvState => usage(scb, scb::UFSR_INVSTATE),
            Fault::InvPc => usage(scb, scb::UFSR_INVPC),
            Fault::NoCp => usage(scb, scb::UFSR_NOCP),
            Fault::StkOf => usage(scb, scb::UFSR_STKOF),
            Fault::Unaligned => usage(scb, scb::UFSR_UNALIGNED),
            Fault::DivByZero => usage(scb, scb::UFSR_DIVBYZERO),
            Fault::IBusErr => {
                scb.bfsr |= scb::BFSR_IBUSERR;
                BUSFAULT
            }
            Fault::PreciseErr(addr) => {
                scb.bfsr |= scb::BFSR_PRECISERR | scb::BFSR_BFARVALID;
                scb.bfar = addr;
                BUSFAULT
            }
            Fault::StkErr => {
                scb.bfsr |= scb::BFSR_STKERR;
                BUSFAULT
            }
            Fault::UnstkErr => {
                scb.bfsr |= scb::BFSR_UNSTKERR;
                BUSFAULT
            }
            Fault::IAccViol => {
                *scb.mmfsr.get_mut(sec) |= scb::MMFSR_IACCVIOL;
                MEMMANAGE
            }
        }
    }

    /// Raise a synchronous fault: record its status and pend its exception (escalated as
    /// needed). Returns false when the processor must lock up.
    pub(crate) fn raise_fault(&mut self, fault: Fault) -> bool {
        log::debug!("fault {fault:?} at pc {:#010x}", self.regs.pc);
        let num = self.record_fault(fault);
        self.raise_sync(num, self.regs.sec)
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
        // Task 8: HardFault targets Secure state unless AIRCR.BFHFNMINS is set.
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
        self.exception_taken(bus, num, sec);
    }

    /// `PushStack`: write the basic frame below the current SP (8-byte aligned, recording the
    /// realignment in xPSR bit 9) and set LR to the EXC_RETURN describing it. Returns the
    /// derived fault, if stacking failed.
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
            let acc = Access {
                kind: AccessKind::Write,
                secure: sec == Sec::Secure,
                privileged: self.is_privileged(),
            };
            for (i, &w) in words.iter().enumerate() {
                if bus
                    .write32(frame.wrapping_add(4 * i as u32), w, acc)
                    .is_err()
                {
                    derived.get_or_insert(Fault::StkErr);
                }
            }
            frame
        };
        if use_psp {
            *self.regs.psp.get_mut(sec) = new_sp;
        } else {
            *self.regs.msp.get_mut(sec) = new_sp;
        }

        // EXC_RETURN: no FP frame, default callee-register stacking; ES is set when taken.
        let mut exc_return = EXC_RETURN_PREFIX | EXC_RETURN_DCRS | EXC_RETURN_FTYPE;
        if sec == Sec::Secure {
            exc_return |= EXC_RETURN_S;
        }
        if thread {
            exc_return |= EXC_RETURN_MODE;
        }
        if use_psp {
            exc_return |= EXC_RETURN_SPSEL;
        }
        self.regs.lr = exc_return;
        derived
    }

    /// `ExceptionTaken`: fetch the vector and enter the handler of `num` for `sec`. LR already
    /// holds the EXC_RETURN value (from `push_stack`, or kept across a tail-chain).
    fn exception_taken(&mut self, bus: &mut impl Bus, num: u16, sec: Sec) {
        // Task 8: entering a handler in the other security state pushes the additional state
        // context, clears registers and uses that state's banked registers.
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
                // The original exception stays pending; HardFault is taken in its place.
                if num == HARDFAULT
                    || self.exception_priority(HARDFAULT, sec, true) >= self.execution_priority()
                {
                    self.enter_lockup(Some((num, sec)));
                } else {
                    self.exception_taken(bus, HARDFAULT, sec);
                }
                return;
            }
        };
        self.regs.lr = (self.regs.lr & !EXC_RETURN_ES) | u32::from(sec == Sec::Secure);
        let x = &mut self.regs.xpsr;
        x.exception = num;
        x.it = 0;
        // A vector with bit 0 clear faults INVSTATE on the handler's first instruction.
        x.t = vector & 1 != 0;
        self.regs.pc = vector & !1;
        // Handler mode always uses the main stack; SPSEL, FPCA and SFPA are cleared.
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
        let sec = self.regs.sec;
        let num = self.regs.xpsr.exception;
        let target = if exc_return & EXC_RETURN_ES != 0 {
            Sec::Secure
        } else {
            Sec::NonSecure
        };
        let to_thread = exc_return & EXC_RETURN_MODE != 0;
        let spsel = exc_return & EXC_RETURN_SPSEL != 0;
        // Task 8: ES/S different from the current state, DCRS = 0 (additional state context)
        // and FNC_RETURN are security state transitions; until then they are invalid.
        let valid = exc_return & 0x00FF_FF82 == 0x00FF_FF80
            && exc_return & (EXC_RETURN_DCRS | EXC_RETURN_FTYPE)
                == EXC_RETURN_DCRS | EXC_RETURN_FTYPE
            && target == sec
            && (exc_return & EXC_RETURN_S != 0) == (sec == Sec::Secure)
            && self.exc_active(num, target)
            && !(!to_thread && spsel)
            && (to_thread || self.active_count() > 1);
        if !valid {
            log::warn!("invalid exception return {exc_return:#010x} from exception {num}");
            return self.return_fault(bus, exc_return, Fault::InvPc);
        }

        // DeActivate.
        self.set_exc_active(num, target, false);
        if num != NMI {
            *self.regs.faultmask.get_mut(target) = false;
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
            self.exception_taken(bus, n, s);
            return self.entry_outcome();
        }

        // PopStack.
        let use_psp = to_thread && spsel;
        let frame = if use_psp {
            *self.regs.psp.get(sec)
        } else {
            *self.regs.msp.get(sec)
        };
        let acc = Access {
            kind: AccessKind::Read,
            secure: sec == Sec::Secure,
            privileged: !to_thread || self.regs.control.get(sec) & 1 == 0,
        };
        let mut w = [0u32; 8];
        for (i, word) in w.iter_mut().enumerate() {
            match bus.read32(frame.wrapping_add(4 * i as u32), acc) {
                Ok(v) => *word = v,
                Err(_) => return self.return_fault(bus, exc_return, Fault::UnstkErr),
            }
        }
        let psr = w[7];
        // Integrity check: the stacked IPSR must match the mode being returned to.
        if to_thread != (psr & 0x1FF == 0) {
            log::warn!("exception return to {exc_return:#010x} with stacked xPSR {psr:#010x}");
            return self.return_fault(bus, exc_return, Fault::InvPc);
        }
        self.regs.r[..4].copy_from_slice(&w[..4]);
        self.regs.r[12] = w[4];
        self.regs.lr = w[5];
        self.regs.pc = w[6] & !1;
        self.regs.xpsr = Xpsr::from_u32(psr);
        let sp = frame.wrapping_add(FRAME_SIZE) | ((psr >> 9 & 1) << 2);
        if use_psp {
            *self.regs.psp.get_mut(sec) = sp;
        } else {
            *self.regs.msp.get_mut(sec) = sp;
        }
        let control = self.regs.control.get_mut(sec);
        *control = (*control & !0b10) | (u32::from(spsel) << 1);

        if to_thread && self.scs.scb.scr.get(sec) & scb::SCR_SLEEPONEXIT != 0 {
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
            self.exception_taken(bus, n, s);
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
}
